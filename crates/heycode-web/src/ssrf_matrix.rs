//! QSEC03 — adversarial SSRF matrix against the WEB02 redirect-aware guard.
//!
//! These cases attack `admit_target`, `RedirectChain` and `ip_is_private`
//! rather than confirming them. Every case is deterministic and local: the
//! resolver is a scripted double and the only sockets are loopback listeners
//! bound by the test itself. Nothing here reaches a real host, and no case
//! depends on the ambient network resolving anything.
//!
//! Matrix axes are **technique × address family × spelling**. The guard is
//! pure URL/address logic, so the OS axis is uniform; platform-specific cells
//! live in the sandbox and path-policy suites instead.
//!
//! **QSEC03 findings W1 through W5 are fixed**, and each tripwire that
//! recorded a gap has become the requirement it was recording:
//!
//! * W1 — `ip_is_private` used `to_ipv4_mapped`, so the IPv4-compatible
//!   (`::7f00:1`), NAT64 (`64:ff9b::7f00:1`) and 6to4 (`2002:7f00:1::`)
//!   spellings of 127.0.0.1 reached loopback, and eight special-purpose IPv4
//!   ranges were reachable outright. The predicate now resolves the embedded
//!   IPv4 destination and recurses, so every IPv4 rule applies through every
//!   encoding.
//! * W2 — a redirect could downgrade https to http, and off-protocol ports
//!   (SSH, SMTP, Redis, memcached, MongoDB, Elasticsearch, the Docker daemon)
//!   were admitted. Both are refused; upgrades and ordinary high HTTP ports
//!   stay legal.
//! * W3 — the internal-name arm knew only the bare `localhost` label and
//!   `.local`/`.internal`, so `localhost.`, the rest of the RFC 6761
//!   `localhost` TLD, `.localdomain`, `.lan` and RFC 8375 `home.arpa` reached
//!   the resolver, making its answer the only thing between the caller and
//!   loopback.
//! * W4 — shared values described as public admitted obvious private literals
//!   and reserved local names, so a non-portable provider could publish or
//!   dispatch them before WEB02. Those spellings now fail at construction.
//! * W5 — reqwest's default ambient proxy support could route a pinned fetch
//!   through a proxy that resolved the hostname again. Pinned clients now
//!   disable proxies, and a child-process regression proves the admitted
//!   address—not the proxy—receives the request.
//!
//! The final host-policy finding is fixed: a configured block list now refuses
//! literals and requires positive allow-list authority for IDNs, so address
//! translation and punycoded homographs cannot step around an ASCII rule.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

/// Deadline for every socket-backed case. A guard bug must surface as a
/// failure, never as a hung gate.
const CASE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// A resolver whose answers are scripted per call, so a second lookup of the
/// same name can differ from the first — the shape of a DNS rebind.
struct ScriptedResolver {
    answers: Mutex<std::collections::VecDeque<Vec<std::net::SocketAddr>>>,
    fallback: Vec<std::net::SocketAddr>,
    calls: AtomicUsize,
    hosts: Mutex<Vec<String>>,
}

impl ScriptedResolver {
    fn always(addresses: &[&str]) -> Self {
        Self {
            answers: Mutex::new(std::collections::VecDeque::new()),
            fallback: addresses.iter().filter_map(|a| a.parse().ok()).collect(),
            calls: AtomicUsize::new(0),
            hosts: Mutex::new(Vec::new()),
        }
    }

    fn scripted(script: &[&[&str]]) -> Self {
        Self {
            answers: script
                .iter()
                .map(|answer| answer.iter().filter_map(|a| a.parse().ok()).collect())
                .collect::<std::collections::VecDeque<_>>()
                .into(),
            fallback: Vec::new(),
            calls: AtomicUsize::new(0),
            hosts: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn hosts(&self) -> Vec<String> {
        self.hosts
            .lock()
            .map(|hosts| hosts.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl DnsResolver for ScriptedResolver {
    async fn resolve(
        &self,
        host: String,
        port: u16,
        _cancellation: CancellationToken,
    ) -> Result<Vec<std::net::SocketAddr>, WebError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut hosts) = self.hosts.lock() {
            hosts.push(host);
        }
        let answer = match self.answers.lock() {
            Ok(mut answers) => answers.pop_front().unwrap_or_else(|| self.fallback.clone()),
            Err(_) => self.fallback.clone(),
        };
        if answer.is_empty() {
            return Err(WebError::network());
        }
        Ok(answer
            .into_iter()
            .map(|address| std::net::SocketAddr::new(address.ip(), port))
            .collect())
    }
}

/// One public answer the guard should always accept, so that a denial in any
/// case below is attributable to the attack and not to the resolver.
const PUBLIC_ANSWER: &str = "93.184.216.34:443";

fn public_resolver() -> ScriptedResolver {
    ScriptedResolver::always(&[PUBLIC_ANSWER])
}

async fn admit(url: &str, resolver: &ScriptedResolver) -> Result<PinnedTarget, WebError> {
    let parsed = url::Url::parse(url).map_err(|_| WebError::invalid_request())?;
    admit_target(
        &parsed,
        resolver,
        &crate::WebDomainPolicy::default(),
        CancellationToken::new(),
    )
    .await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const PROXY_CHILD: &str = "HEYCODE_SSRF_PROXY_CHILD";
    const PROXY_TARGET_URL: &str = "HEYCODE_SSRF_PROXY_TARGET_URL";
    const PROXY_TARGET_ADDRESS: &str = "HEYCODE_SSRF_PROXY_TARGET_ADDRESS";

    fn serve_one_with_deadline(
        listener: std::net::TcpListener,
        response: &'static [u8],
        stop: &std::sync::atomic::AtomicBool,
    ) -> bool {
        use std::io::{Read as _, Write as _};

        listener
            .set_nonblocking(true)
            .expect("set probe listener nonblocking");
        let deadline = std::time::Instant::now() + CASE_TIMEOUT;
        loop {
            if stop.load(Ordering::Acquire) {
                return false;
            }
            match listener.accept() {
                Ok((mut stream, _peer)) => {
                    stream
                        .set_read_timeout(Some(CASE_TIMEOUT))
                        .expect("set probe read timeout");
                    let mut request = [0_u8; 4096];
                    let _read = stream.read(&mut request).expect("read probe request");
                    stream.write_all(response).expect("write probe response");
                    return true;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        return false;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("accept probe connection: {error}"),
            }
        }
    }

    #[tokio::test]
    async fn pinned_proxy_child() {
        if std::env::var_os(PROXY_CHILD).is_none() {
            return;
        }
        let url = std::env::var(PROXY_TARGET_URL).expect("proxy child target URL");
        let address = std::env::var(PROXY_TARGET_ADDRESS)
            .expect("proxy child target address")
            .parse()
            .expect("parse proxy child target address");
        let target = PinnedTarget {
            url: url::Url::parse(&url).expect("parse proxy child target URL"),
            domain: Some("pinned.invalid".to_owned()),
            addresses: vec![address],
        };
        let response = pinned_fetch_client(&target)
            .expect("build pinned client")
            .get(target.url.as_str())
            .send()
            .await
            .expect("pinned request reaches the admitted address");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }

    /// DNS pinning is a direct-connect guarantee. Letting reqwest honor an
    /// ambient proxy delegates resolution and connection to that proxy, so the
    /// address admitted above is no longer the address reached on the wire.
    #[tokio::test]
    async fn a_pinned_fetch_ignores_ambient_proxy_configuration() {
        let target_listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind target listener");
        let target_address = target_listener
            .local_addr()
            .expect("target listener address");
        let proxy_listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind proxy listener");
        let proxy_address = proxy_listener.local_addr().expect("proxy listener address");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let target_stop = std::sync::Arc::clone(&stop);
        let target = std::thread::spawn(move || {
            serve_one_with_deadline(
                target_listener,
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                &target_stop,
            )
        });
        let proxy_stop = std::sync::Arc::clone(&stop);
        let proxy = std::thread::spawn(move || {
            serve_one_with_deadline(
                proxy_listener,
                b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                &proxy_stop,
            )
        });

        let proxy_url = format!("http://{proxy_address}");
        let target_url = format!("http://pinned.invalid:{}/probe", target_address.port());
        let output = tokio::process::Command::new(
            std::env::current_exe().expect("resolve current test executable"),
        )
        .args(["pinned_proxy_child", "--nocapture"])
        .env(PROXY_CHILD, "1")
        .env(PROXY_TARGET_URL, target_url)
        .env(PROXY_TARGET_ADDRESS, target_address.to_string())
        .env("HTTP_PROXY", &proxy_url)
        .env("http_proxy", &proxy_url)
        .env("HTTPS_PROXY", &proxy_url)
        .env("https_proxy", &proxy_url)
        .env("ALL_PROXY", &proxy_url)
        .env("all_proxy", &proxy_url)
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .output()
        .await
        .expect("run proxy-isolated child test");

        stop.store(true, Ordering::Release);
        let target_received = target.join().expect("join target listener");
        let proxy_received = proxy.join().expect("join proxy listener");
        assert!(
            output.status.success(),
            "the pinned request did not reach its admitted address: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(target_received, "the admitted target received no request");
        assert!(
            !proxy_received,
            "an ambient proxy intercepted a request whose DNS answer was pinned"
        );
    }

    /// Values advertised as public must reject hosts that are already known
    /// to be local or special before a provider can publish or fetch them.
    #[test]
    fn public_web_values_reject_obvious_private_and_metadata_hosts() {
        for denied in [
            "http://169.254.169.254/latest/meta-data/",
            "http://metadata.google.internal/computeMetadata/v1/",
            "http://127.0.0.1/",
            "http://[::1]/",
            "http://printer.local/",
        ] {
            assert!(
                crate::WebSearchResult::new("result", denied, "snippet").is_err(),
                "a private URL must not become a public search result: {denied}"
            );
            assert!(
                crate::WebFetchRequest::new(denied, 4_096).is_err(),
                "a private URL must not become a public fetch request: {denied}"
            );
        }
        for allowed in [
            "https://example.test/",
            "https://93.184.216.34/",
            "https://[2606:2800:220:1:248:1893:25c8:1946]/",
        ] {
            crate::WebSearchResult::new("result", allowed, "snippet")
                .unwrap_or_else(|_| panic!("public result must remain valid: {allowed}"));
            crate::WebFetchRequest::new(allowed, 4_096)
                .unwrap_or_else(|_| panic!("public fetch must remain valid: {allowed}"));
        }
    }

    // ── Technique: literal address in every spelling ────────────────────────

    /// Cloud metadata endpoints, loopback, private, link-local and CGNAT space
    /// must be denied whichever notation the URL uses. A resolver that would
    /// happily answer "public" is supplied so that any admission here is a
    /// guard failure and not a lookup failure.
    #[tokio::test]
    async fn every_spelling_of_a_private_or_metadata_address_is_denied_before_connect() {
        let denied = [
            // AWS/GCP/Azure IMDS, plain and re-encoded.
            "http://169.254.169.254/latest/meta-data/",
            "http://[fd00:ec2::254]/latest/meta-data/",
            "http://[::ffff:169.254.169.254]/latest/meta-data/",
            "http://2852039166/latest/meta-data/",
            "http://0251.0376.0251.0376/latest/meta-data/",
            "http://0xa9fea9fe/latest/meta-data/",
            "http://169.254.169.254:80/",
            "https://169.254.169.254/",
            // Loopback in its many spellings.
            "http://127.0.0.1/",
            "http://127.1/",
            "http://127.0.0.1:8080/",
            "http://2130706433/",
            "http://0177.0.0.1/",
            "http://0x7f.0.0.1/",
            "http://0x7f000001/",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[0:0:0:0:0:ffff:7f00:1]/",
            "http://localhost/",
            "http://LOCALHOST/",
            // Unspecified — connects to loopback on Linux and macOS.
            "http://0.0.0.0/",
            "http://[::]/",
            // RFC1918 and CGNAT.
            "http://10.0.0.1/",
            "http://10.255.255.255/",
            "http://172.16.0.1/",
            "http://172.31.255.255/",
            "http://192.168.0.1/",
            "http://100.64.0.1/",
            "http://100.127.255.255/",
            // IPv6 unique-local and link-local.
            "http://[fc00::1]/",
            "http://[fd12:3456::1]/",
            "http://[fe80::1]/",
            "http://[fe80::1%25en0]/",
            // Suffixes the guard treats as always-internal.
            "http://metadata.google.internal/computeMetadata/v1/",
            "http://printer.local/",
            "http://anything.internal/",
        ];
        for case in denied {
            let resolver = public_resolver();
            let outcome = admit(case, &resolver).await;
            assert!(
                outcome.is_err(),
                "{case} was admitted; the resolver answered {PUBLIC_ANSWER} so this is a guard \
                 failure, not a lookup failure"
            );
        }
    }

    /// The complement: ordinary public literals and names stay reachable, so
    /// the denials above are not a blanket refusal that would hide a bug.
    #[tokio::test]
    async fn ordinary_public_addresses_and_names_remain_admissible() {
        let allowed = [
            "https://example.test/page",
            "http://93.184.216.34/page",
            "https://[2606:2800:220:1:248:1893:25c8:1946]/page",
            // Boundaries just outside each blocked block.
            "http://172.15.255.255/",
            "http://172.32.0.1/",
            "http://11.0.0.1/",
            "http://192.169.0.1/",
            "http://100.63.255.255/",
            "http://100.128.0.1/",
            "http://169.253.255.255/",
            "http://169.255.0.1/",
        ];
        for case in allowed {
            let resolver = public_resolver();
            assert!(
                admit(case, &resolver).await.is_ok(),
                "{case} must remain reachable; over-blocking hides real denials"
            );
        }
    }

    /// QSEC03 finding W1, now the requirement rather than the gap.
    ///
    /// The three IPv6 entries matter most: each is a standard way of writing
    /// 127.0.0.1 inside an IPv6 literal, and each reached loopback before the
    /// predicate resolved embedded IPv4.
    #[test]
    fn every_special_purpose_range_is_denied_by_the_private_predicate() {
        for literal in [
            "255.255.255.255",
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
            "192.0.0.1",
            "198.18.0.1",
            "0.1.2.3",
            "::7f00:1",
            "64:ff9b::7f00:1",
            "2002:7f00:1::",
            "ff02::1",
        ] {
            let address: std::net::IpAddr = literal.parse().unwrap();
            assert!(ip_is_private(&address), "{literal} must be denied");
        }
    }

    /// QSEC03 finding W3, now the requirement: these spellings are denied by
    /// the string arm itself, so a *lying* resolver cannot admit them.
    #[tokio::test]
    async fn internal_name_spellings_are_denied_by_the_string_arm_not_by_the_resolver() {
        let smuggled = [
            "http://localhost./",
            "http://sub.localhost/",
            "http://host.localdomain/",
            "http://gateway.home.arpa/",
            "http://printer.lan/",
        ];
        for case in smuggled {
            let honest = ScriptedResolver::always(&["127.0.0.1:80"]);
            assert!(
                admit(case, &honest).await.is_err(),
                "{case} must be denied when the resolver answers loopback"
            );

            let lying = public_resolver();
            assert!(
                admit(case, &lying).await.is_err(),
                "{case} must be denied even when the resolver claims it is public"
            );
        }
    }

    // ── Technique: DNS rebinding ────────────────────────────────────────────

    /// The guard resolves once per hop and pins the answer. A name that would
    /// rebind to loopback on a second lookup never gets a second lookup.
    #[tokio::test]
    async fn a_name_that_rebinds_to_loopback_on_the_second_lookup_is_pinned_to_the_first_answer() {
        let rebinder = ScriptedResolver::scripted(&[&[PUBLIC_ANSWER], &["127.0.0.1:443"]]);
        let target = admit("https://rebind.test/path", &rebinder)
            .await
            .expect("first answer is public");
        assert_eq!(rebinder.calls(), 1, "one hop must cost exactly one lookup");
        assert_eq!(
            target.addresses,
            vec!["93.184.216.34:443".parse::<std::net::SocketAddr>().unwrap()],
            "the pin must carry the validated answer, not a re-lookup"
        );
        assert_eq!(target.domain.as_deref(), Some("rebind.test"));

        // Building the client must not consult DNS either.
        pinned_fetch_client(&target).expect("pinned client builds");
        assert_eq!(
            rebinder.calls(),
            1,
            "client construction re-resolved the name"
        );
    }

    /// Pinning is per hop, not per fetch: a redirect back to the same name
    /// re-resolves, and the rebound private answer is refused at hop two.
    #[tokio::test]
    async fn a_rebind_that_lands_on_the_redirect_hop_is_denied_at_that_hop() {
        let rebinder = ScriptedResolver::scripted(&[&[PUBLIC_ANSWER], &["169.254.169.254:80"]]);
        let mut chain =
            RedirectChain::new(url::Url::parse("https://rebind.test/first").unwrap()).unwrap();
        admit_target(
            chain.current(),
            &rebinder,
            &crate::WebDomainPolicy::default(),
            CancellationToken::new(),
        )
        .await
        .expect("hop one is public");

        chain.advance("https://rebind.test/second").unwrap();
        let denied = admit_target(
            chain.current(),
            &rebinder,
            &crate::WebDomainPolicy::default(),
            CancellationToken::new(),
        )
        .await;
        assert!(denied.is_err(), "the rebound second hop must be refused");
        assert_eq!(rebinder.calls(), 2, "each hop must resolve independently");
        assert_eq!(rebinder.hosts(), vec!["rebind.test", "rebind.test"]);
    }

    /// A split answer — one public address alongside one private one — must be
    /// refused whole. Admitting the public member and letting the connect pick
    /// either is the classic multi-A-record rebind.
    #[tokio::test]
    async fn a_mixed_public_and_private_answer_is_refused_rather_than_filtered() {
        for answer in [
            &[PUBLIC_ANSWER, "127.0.0.1:443"][..],
            &["127.0.0.1:443", PUBLIC_ANSWER][..],
            &[PUBLIC_ANSWER, "169.254.169.254:443"][..],
            &[PUBLIC_ANSWER, "[::1]:443"][..],
            &[PUBLIC_ANSWER, "10.1.2.3:443"][..],
        ] {
            let resolver = ScriptedResolver::always(answer);
            assert!(
                admit("https://split.test/path", &resolver).await.is_err(),
                "answer {answer:?} must be refused whole, not filtered down to its public member"
            );
        }
    }

    /// An empty answer is a denial, never an admission with no pin — a pinless
    /// client would fall back to the system resolver at connect time.
    #[tokio::test]
    async fn an_empty_resolver_answer_never_yields_an_unpinned_target() {
        let empty = ScriptedResolver::always(&[]);
        assert!(admit("https://empty.test/path", &empty).await.is_err());
    }

    /// The load-bearing claim of a redirect-aware guard: the address actually
    /// connected to is the address that was validated. `pinned.invalid` cannot
    /// resolve — `.invalid` is reserved and never delegated — so if the pin is
    /// ignored the request fails instead of reaching the listener.
    #[tokio::test]
    async fn the_pinned_client_connects_to_the_validated_address_and_not_to_dns() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe listener");
        let address = listener.local_addr().expect("probe listener address");
        let served = std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};
            let (mut stream, _peer) = listener.accept().ok()?;
            stream
                .set_read_timeout(Some(CASE_TIMEOUT))
                .expect("probe read timeout");
            let mut buffer = [0_u8; 4096];
            let read = stream.read(&mut buffer).ok()?;
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .ok()?;
            Some(request)
        });

        let target = PinnedTarget {
            url: url::Url::parse(&format!("http://pinned.invalid:{}/probe", address.port()))
                .unwrap(),
            domain: Some("pinned.invalid".to_owned()),
            addresses: vec![address],
        };
        let client = pinned_fetch_client(&target).expect("pinned client builds");
        let response = tokio::time::timeout(CASE_TIMEOUT, client.get(target.url.as_str()).send())
            .await
            .expect("pinned request completed within the case deadline")
            .expect("pinned request reached the pinned address");
        assert!(response.status().is_success());

        let request = served
            .join()
            .expect("probe listener thread")
            .expect("probe listener served one request");
        assert!(request.starts_with("GET /probe "), "{request:?}");
        assert!(
            request
                .to_ascii_lowercase()
                .contains("host: pinned.invalid"),
            "the pin must redirect the connection only, leaving Host intact: {request:?}"
        );
    }

    // ── Technique: redirect chains ──────────────────────────────────────────

    /// Every hop is re-admitted, so a chain that starts public and ends inside
    /// the fence is refused at the hop that crosses it — including the case
    /// where the crossing is disguised as a protocol-relative reference.
    #[tokio::test]
    async fn a_redirect_chain_ending_anywhere_private_is_refused_at_the_crossing_hop() {
        let landings = [
            "http://169.254.169.254/latest/meta-data/",
            "http://[fd00:ec2::254]/latest/meta-data/",
            "//169.254.169.254/latest/meta-data/",
            "http://127.0.0.1:6379/",
            "http://localhost/admin",
            "http://10.0.0.1/",
            "http://2130706433/",
            "http://[::ffff:127.0.0.1]/",
        ];
        for landing in landings {
            let resolver = public_resolver();
            let mut chain =
                RedirectChain::new(url::Url::parse("https://start.test/one").unwrap()).unwrap();
            admit_target(
                chain.current(),
                &resolver,
                &crate::WebDomainPolicy::default(),
                CancellationToken::new(),
            )
            .await
            .expect("the first hop is public");
            chain.advance("https://start.test/two").unwrap();
            let Ok(()) = chain.advance(landing) else {
                continue; // Refused by the chain itself, which is also a denial.
            };
            assert!(
                admit_target(
                    chain.current(),
                    &resolver,
                    &crate::WebDomainPolicy::default(),
                    CancellationToken::new(),
                )
                .await
                .is_err(),
                "a chain landing on {landing} must be refused"
            );
        }
    }

    /// Non-HTTP schemes, credentials and hostless forms are refused by the
    /// chain before any admission decision is reached.
    #[test]
    fn a_redirect_that_changes_to_a_non_http_scheme_or_adds_credentials_is_refused() {
        for location in [
            "file:///etc/passwd",
            "file://localhost/etc/shadow",
            "gopher://169.254.169.254:70/_GET%20/",
            "ftp://169.254.169.254/",
            "data:text/html,<script>1</script>",
            "javascript:fetch('/')",
            "ws://169.254.169.254/",
            "https://user:secret@example.test/",
            "https://user@example.test/",
            "http://:8080/",
        ] {
            let mut chain =
                RedirectChain::new(url::Url::parse("https://start.test/one").unwrap()).unwrap();
            assert!(
                chain.advance(location).is_err(),
                "redirect to {location} must be refused"
            );
            assert_eq!(
                chain.current().as_str(),
                "https://start.test/one",
                "a refused hop must not advance the chain"
            );
        }
    }

    /// Bounds: loops are refused by identity, and the hop budget is finite.
    #[test]
    fn redirect_loops_and_unbounded_chains_are_refused_by_the_chain_itself() {
        let mut two_step =
            RedirectChain::new(url::Url::parse("https://start.test/a").unwrap()).unwrap();
        two_step.advance("/b").unwrap();
        two_step.advance("/c").unwrap();
        assert!(two_step.advance("/a").is_err(), "A→B→C→A must be refused");

        let mut self_loop =
            RedirectChain::new(url::Url::parse("https://start.test/a").unwrap()).unwrap();
        assert!(
            self_loop.advance("https://start.test/a").is_err(),
            "a redirect to the current URL must be refused"
        );

        let mut budget =
            RedirectChain::new(url::Url::parse("https://start.test/0").unwrap()).unwrap();
        for hop in 1..=5 {
            budget
                .advance(&format!("/{hop}"))
                .unwrap_or_else(|_| panic!("hop {hop} is within budget"));
        }
        assert!(
            budget.advance("/6").is_err(),
            "the sixth hop must be refused"
        );
    }

    /// QSEC03 finding W2, now the requirement: a redirect may not downgrade the
    /// transport, and off-protocol ports are refused before connect.
    ///
    /// Upgrading stays legal, and ordinary high HTTP ports stay reachable — a
    /// port policy that blocked 8080 would break real fetches to buy nothing.
    #[tokio::test]
    async fn a_redirect_cannot_downgrade_the_transport_and_off_protocol_ports_are_refused() {
        let mut downgrade =
            RedirectChain::new(url::Url::parse("https://start.test/secure").unwrap()).unwrap();
        assert!(
            downgrade.advance("http://start.test/plaintext").is_err(),
            "an https chain must refuse a plaintext hop"
        );
        assert_eq!(
            downgrade.current().as_str(),
            "https://start.test/secure",
            "a refused hop must not advance the chain"
        );

        let mut upgrade =
            RedirectChain::new(url::Url::parse("http://start.test/plain").unwrap()).unwrap();
        upgrade
            .advance("https://start.test/secure")
            .expect("an upgrade to https stays legal");

        let resolver = public_resolver();
        for refused in [
            "https://start.test:22/",
            "https://start.test:25/",
            "https://start.test:3306/",
            "https://start.test:6379/",
            "https://start.test:11211/",
            "https://start.test:9200/",
            "https://start.test:2375/",
            "https://start.test:27017/",
        ] {
            assert!(
                admit(refused, &resolver).await.is_err(),
                "{refused} must be refused before connect"
            );
        }
        for allowed in [
            "https://start.test/",
            "http://start.test/",
            "https://start.test:8443/",
            "http://start.test:8080/",
            "http://start.test:3000/",
            "http://start.test:5000/",
            "http://start.test:9000/",
        ] {
            assert!(
                admit(allowed, &resolver).await.is_ok(),
                "{allowed} must stay reachable; over-blocking hides real denials"
            );
        }
    }

    /// A redirect with no usable target must not silently continue on the
    /// previous URL.
    #[test]
    fn a_redirect_with_no_usable_target_does_not_advance_the_chain() {
        for location in ["", "http://", "https://", "http://:8080/"] {
            let mut chain =
                RedirectChain::new(url::Url::parse("https://start.test/one").unwrap()).unwrap();
            assert!(
                chain.advance(location).is_err(),
                "redirect to {location:?} must be refused"
            );
            assert_eq!(chain.current().as_str(), "https://start.test/one");
        }
    }

    // ── Technique: domain policy interaction ────────────────────────────────

    /// An allow list confines the guard to named domains, and a literal
    /// address cannot be used to step outside it.
    #[tokio::test]
    async fn an_allow_list_cannot_be_stepped_around_with_a_literal_address() {
        let policy =
            crate::WebDomainPolicy::new(vec!["allowed.test".to_owned()], Vec::new()).unwrap();
        let resolver = public_resolver();
        for case in [
            "https://93.184.216.34/",
            "https://[2606:2800:220:1:248:1893:25c8:1946]/",
            "https://elsewhere.test/",
            "https://allowed.test.evil.test/",
            "https://notallowed.test/",
        ] {
            let parsed = url::Url::parse(case).unwrap();
            let outcome = admit_target(&parsed, &resolver, &policy, CancellationToken::new()).await;
            assert!(
                outcome.is_err(),
                "{case} must not pass an allow list of allowed.test"
            );
        }
        for case in ["https://allowed.test/", "https://sub.allowed.test/"] {
            let parsed = url::Url::parse(case).unwrap();
            assert!(
                admit_target(&parsed, &resolver, &policy, CancellationToken::new())
                    .await
                    .is_ok(),
                "{case} must satisfy an allow list of allowed.test"
            );
        }
    }

    /// A block list must fail closed for host spellings that cannot be
    /// compared safely with its ASCII rules. A literal address could be the
    /// resolved form of a blocked name, and an unapproved IDN could be its
    /// Unicode homograph.
    #[tokio::test]
    async fn qsec03_requirement_a_block_list_survives_address_and_idn_translation() {
        let policy =
            crate::WebDomainPolicy::new(Vec::new(), vec!["blocked.test".to_owned()]).unwrap();
        let resolver = public_resolver();

        let blocked = url::Url::parse("https://blocked.test/").unwrap();
        assert!(
            admit_target(&blocked, &resolver, &policy, CancellationToken::new())
                .await
                .is_err(),
            "the named form must be blocked"
        );

        // Public literals in both families and a Cyrillic homograph that the
        // URL parser punycodes into a label no ASCII rule can match.
        for evasion in [
            "https://93.184.216.34/",
            "https://[2606:2800:220:1:248:1893:25c8:1946]/",
            "https://bl\u{43e}cked.test/",
        ] {
            let parsed = url::Url::parse(evasion).unwrap();
            assert!(
                admit_target(&parsed, &resolver, &policy, CancellationToken::new())
                    .await
                    .is_err(),
                "{evasion} must not bypass blocked.test"
            );
        }
        assert_eq!(
            url::Url::parse("https://bl\u{43e}cked.test/")
                .unwrap()
                .host_str(),
            Some("xn--blcked-xqf.test"),
            "the homograph must reach the policy already punycoded"
        );
        assert_eq!(
            resolver.calls(),
            0,
            "policy-rejected spellings must not reach DNS"
        );
    }

    /// The fail-closed ambiguity rule must not turn an ASCII block list into
    /// a blanket network denial. Ordinary ASCII neighbours stay reachable,
    /// and an operator can name one intentional IDN explicitly in the allow
    /// list. With no block rules, public literals and IDNs retain their prior
    /// behavior.
    #[tokio::test]
    async fn qsec03_requirement_block_rules_preserve_explicit_safe_neighbours() {
        let blocked =
            crate::WebDomainPolicy::new(Vec::new(), vec!["blocked.test".to_owned()]).unwrap();
        let resolver = public_resolver();
        let unrelated = url::Url::parse("https://unrelated.test/").unwrap();
        assert!(
            admit_target(&unrelated, &resolver, &blocked, CancellationToken::new())
                .await
                .is_ok(),
            "an unrelated ASCII domain must stay reachable"
        );

        let explicit_idn = crate::WebDomainPolicy::new(
            vec!["xn--bcher-kva.example".to_owned()],
            vec!["blocked.test".to_owned()],
        )
        .unwrap();
        let intentional = url::Url::parse("https://b\u{fc}cher.example/").unwrap();
        assert!(
            admit_target(
                &intentional,
                &resolver,
                &explicit_idn,
                CancellationToken::new()
            )
            .await
            .is_ok(),
            "an explicitly allow-listed IDN must stay reachable"
        );

        let broad_ascii_allow =
            crate::WebDomainPolicy::new(vec!["test".to_owned()], vec!["blocked.test".to_owned()])
                .unwrap();
        let homograph = url::Url::parse("https://bl\u{43e}cked.test/").unwrap();
        assert!(
            admit_target(
                &homograph,
                &resolver,
                &broad_ascii_allow,
                CancellationToken::new()
            )
            .await
            .is_err(),
            "a broad ASCII allow rule must not authorize an unlisted IDN homograph"
        );

        let open = crate::WebDomainPolicy::default();
        for permitted in [
            "https://93.184.216.34/",
            "https://[2606:2800:220:1:248:1893:25c8:1946]/",
            "https://b\u{fc}cher.example/",
        ] {
            let parsed = url::Url::parse(permitted).unwrap();
            assert!(
                admit_target(&parsed, &resolver, &open, CancellationToken::new())
                    .await
                    .is_ok(),
                "{permitted} must retain default-policy reachability"
            );
        }
    }

    /// Suffix rules must match on label boundaries, in both directions.
    #[test]
    fn domain_rules_match_on_label_boundaries_and_not_on_raw_suffixes() {
        let policy =
            crate::WebDomainPolicy::new(Vec::new(), vec!["example.test".to_owned()]).unwrap();
        for blocked in [
            "https://example.test/",
            "https://EXAMPLE.TEST/",
            "https://sub.example.test/",
            "https://a.b.example.test/",
            "https://example.test./",
        ] {
            assert!(!policy.allows(blocked), "{blocked} must be blocked");
        }
        for permitted in [
            "https://notexample.test/",
            "https://myexample.test/",
            "https://example.testing/",
            "https://example.test.evil.test/",
        ] {
            assert!(policy.allows(permitted), "{permitted} must not be blocked");
        }
    }

    /// Credentials, non-HTTP schemes and hostless URLs never satisfy a policy,
    /// whatever the rules say.
    #[test]
    fn credentials_and_non_http_schemes_never_satisfy_a_domain_policy() {
        let open = crate::WebDomainPolicy::default();
        for rejected in [
            "https://user:secret@example.test/",
            "https://user@example.test/",
            "file:///etc/passwd",
            "gopher://example.test/",
            "data:text/plain,hi",
            "mailto:someone@example.test",
            "not a url",
        ] {
            assert!(
                !open.allows(rejected),
                "{rejected} must never satisfy a policy"
            );
        }
    }

    /// Malformed and oversized rule inputs are refused at construction, so a
    /// policy can never be half-applied at request time.
    #[test]
    fn malformed_domain_rules_are_refused_at_construction() {
        assert!(crate::WebDomainPolicy::new(vec![".example.test".to_owned()], Vec::new()).is_err());
        assert!(crate::WebDomainPolicy::new(vec!["example.test.".to_owned()], Vec::new()).is_err());
        assert!(crate::WebDomainPolicy::new(vec!["exa mple.test".to_owned()], Vec::new()).is_err());
        assert!(crate::WebDomainPolicy::new(vec!["-example.test".to_owned()], Vec::new()).is_err());
        assert!(crate::WebDomainPolicy::new(vec!["example..test".to_owned()], Vec::new()).is_err());
        assert!(crate::WebDomainPolicy::new(vec!["exámple.test".to_owned()], Vec::new()).is_err());
        assert!(
            crate::WebDomainPolicy::new(
                vec!["example.test".to_owned(), "EXAMPLE.TEST".to_owned()],
                Vec::new()
            )
            .is_err(),
            "case-folded duplicates are ambiguous and must be refused"
        );
        assert!(
            crate::WebDomainPolicy::new(
                vec!["example.test".to_owned()],
                vec!["example.test".to_owned()]
            )
            .is_err(),
            "the same exact domain cannot be both allowed and blocked"
        );
        let too_many: Vec<String> = (0..129).map(|index| format!("h{index}.test")).collect();
        assert!(crate::WebDomainPolicy::new(too_many, Vec::new()).is_err());
    }

    // ── Technique: cancellation and bounds ──────────────────────────────────

    /// Admission delegates cancellation to the resolver and performs no
    /// lookup at all for a literal address. Both halves matter: a literal must
    /// never leak a hostname to DNS, and a cancelled lookup must fail closed
    /// rather than return a stale answer.
    #[tokio::test]
    async fn a_literal_address_never_reaches_dns_and_a_cancelled_lookup_fails_closed() {
        let resolver = public_resolver();
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        for literal in [
            "http://169.254.169.254/",
            "http://127.0.0.1/",
            "https://93.184.216.34/",
            "https://[2606:2800:220:1:248:1893:25c8:1946]/",
        ] {
            let parsed = url::Url::parse(literal).unwrap();
            let _ = admit_target(
                &parsed,
                &resolver,
                &crate::WebDomainPolicy::default(),
                cancelled.clone(),
            )
            .await;
        }
        assert_eq!(
            resolver.calls(),
            0,
            "a literal address must never be handed to a resolver"
        );

        // `admit_target` holds no cancellation check of its own; the contract
        // is that the resolver honours the token. A resolver that does is
        // enough to fail the admission closed.
        struct CancelHonouring;
        #[async_trait]
        impl DnsResolver for CancelHonouring {
            async fn resolve(
                &self,
                _host: String,
                _port: u16,
                cancellation: CancellationToken,
            ) -> Result<Vec<std::net::SocketAddr>, WebError> {
                if cancellation.is_cancelled() {
                    return Err(WebError::cancelled());
                }
                Ok(vec![PUBLIC_ANSWER.parse().unwrap()])
            }
        }
        let named = url::Url::parse("https://example.test/").unwrap();
        let outcome = tokio::time::timeout(
            CASE_TIMEOUT,
            admit_target(
                &named,
                &CancelHonouring,
                &crate::WebDomainPolicy::default(),
                cancelled,
            ),
        )
        .await
        .expect("cancelled admission returns within the case deadline");
        assert!(outcome.is_err(), "a cancelled lookup must not admit");
    }
}
