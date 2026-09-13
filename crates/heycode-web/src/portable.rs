//! Portable HTTP fetch plus DuckDuckGo/Brave search provider.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt as _;
use tokio_util::sync::CancellationToken;

use crate::{
    SERVICE_WEB, WebError, WebFetchRequest, WebFetchResult, WebProcessorHandle, WebProvider,
    WebProviderDescriptor, WebRawDocument, WebRegistry, WebSearchRequest, WebSearchResult,
};

const REQUEST_TIMEOUT_SECS: u64 = 20;
const SEARCH_RESPONSE_MAX_BYTES: usize = 1024 * 1024;

/// Trusted provider endpoint configuration; secrets resolve at operation time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortableWebConfig {
    brave_base_url: String,
    duckduckgo_base_url: String,
}

impl PortableWebConfig {
    /// Construct validated provider endpoints.
    ///
    /// # Errors
    /// Both endpoints must be absolute HTTP(S) URLs without userinfo.
    pub fn new(
        brave_base_url: impl Into<String>,
        duckduckgo_base_url: impl Into<String>,
    ) -> Result<Self, WebError> {
        let config = Self {
            brave_base_url: brave_base_url.into(),
            duckduckgo_base_url: duckduckgo_base_url.into(),
        };
        validate_endpoint(&config.brave_base_url)?;
        validate_endpoint(&config.duckduckgo_base_url)?;
        Ok(config)
    }

    /// Official provider endpoints.
    #[must_use]
    pub fn official() -> Self {
        Self {
            brave_base_url: "https://api.search.brave.com".to_owned(),
            duckduckgo_base_url: "https://lite.duckduckgo.com/lite/".to_owned(),
        }
    }
}

#[async_trait]
trait DnsResolver: Send + Sync {
    async fn resolve(
        &self,
        host: String,
        port: u16,
        cancellation: CancellationToken,
    ) -> Result<Vec<std::net::SocketAddr>, WebError>;
}

struct SystemDnsResolver;

#[async_trait]
impl DnsResolver for SystemDnsResolver {
    async fn resolve(
        &self,
        host: String,
        port: u16,
        cancellation: CancellationToken,
    ) -> Result<Vec<std::net::SocketAddr>, WebError> {
        let lookup = tokio::net::lookup_host((host.as_str(), port));
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(WebError::cancelled()),
            result = lookup => match result {
                Ok(addresses) => {
                    let addresses = addresses.collect::<Vec<_>>();
                    if addresses.is_empty() {
                        Err(WebError::network())
                    } else {
                        Ok(addresses)
                    }
                }
                Err(_) => Err(WebError::network()),
            }
        }
    }
}

#[derive(Debug)]
struct PinnedTarget {
    url: url::Url,
    domain: Option<String>,
    addresses: Vec<std::net::SocketAddr>,
}

struct RedirectChain {
    current: url::Url,
    seen: BTreeSet<String>,
    hops: u8,
}

impl RedirectChain {
    fn new(current: url::Url) -> Result<Self, WebError> {
        validate_endpoint(current.as_str())?;
        let mut seen = BTreeSet::new();
        seen.insert(current.as_str().to_owned());
        Ok(Self {
            current,
            seen,
            hops: 0,
        })
    }

    fn current(&self) -> &url::Url {
        &self.current
    }

    fn advance(&mut self, location: &str) -> Result<(), WebError> {
        if self.hops >= 5 {
            return Err(WebError::invalid_response());
        }
        let next = self
            .current
            .join(location)
            .map_err(|_| WebError::invalid_response())?;
        validate_endpoint(next.as_str()).map_err(|_| WebError::invalid_response())?;
        // QSEC03 finding W2: a redirect must not downgrade the transport. A
        // request that began encrypted continuing in plaintext is a silent loss
        // of the property the caller asked for, and the redirect is chosen by
        // the remote host rather than by us. Upgrade (http→https) stays legal.
        if self.current.scheme() == "https" && next.scheme() != "https" {
            return Err(WebError::invalid_response());
        }
        if !self.seen.insert(next.as_str().to_owned()) {
            return Err(WebError::invalid_response());
        }
        self.current = next;
        self.hops = self.hops.saturating_add(1);
        Ok(())
    }
}

struct PortableWebProvider {
    config: PortableWebConfig,
    client: reqwest::Client,
    resolver: Arc<dyn DnsResolver>,
    processors: WebProcessorHandle,
}

impl PortableWebProvider {
    fn new(config: PortableWebConfig, processors: WebProcessorHandle) -> Result<Self, WebError> {
        Self::new_with_resolver(config, Arc::new(SystemDnsResolver), processors)
    }

    fn new_with_resolver(
        config: PortableWebConfig,
        resolver: Arc<dyn DnsResolver>,
        processors: WebProcessorHandle,
    ) -> Result<Self, WebError> {
        validate_endpoint(&config.brave_base_url)?;
        validate_endpoint(&config.duckduckgo_base_url)?;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .user_agent("heycode-web/0.3")
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| WebError::unavailable())?;
        Ok(Self {
            config,
            client,
            resolver,
            processors,
        })
    }

    async fn brave(
        &self,
        request: &WebSearchRequest,
        key: &str,
        cancellation: CancellationToken,
    ) -> Result<Vec<WebSearchResult>, WebError> {
        let url = format!(
            "{}/res/v1/web/search",
            self.config.brave_base_url.trim_end_matches('/')
        );
        let count = request.max_results().to_string();
        let send = self
            .client
            .get(url)
            .query(&[("q", request.query()), ("count", count.as_str())])
            .header("X-Subscription-Token", key)
            .header("Accept", "application/json")
            .send();
        let response = select_cancel(send, cancellation.clone()).await?;
        if !response.status().is_success() {
            return Err(WebError::http());
        }
        let body = bounded_body(response, SEARCH_RESPONSE_MAX_BYTES, cancellation).await?;
        let body: serde_json::Value =
            serde_json::from_slice(&body).map_err(|_| WebError::invalid_response())?;
        let results = body
            .pointer("/web/results")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(WebError::invalid_response)?;
        results
            .iter()
            .take(usize::from(request.max_results()))
            .map(|result| {
                let result = result.as_object().ok_or_else(WebError::invalid_response)?;
                WebSearchResult::new(
                    result
                        .get("title")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("(untitled)")
                        .trim(),
                    result
                        .get("url")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(WebError::invalid_response)?,
                    result
                        .get("description")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(""),
                )
            })
            .collect()
    }

    async fn duckduckgo(
        &self,
        request: &WebSearchRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<WebSearchResult>, WebError> {
        let send = self
            .client
            .get(&self.config.duckduckgo_base_url)
            .query(&[("q", request.query())])
            .header("Accept", "text/html")
            .send();
        let response = select_cancel(send, cancellation.clone()).await?;
        if !response.status().is_success() {
            return Err(WebError::http());
        }
        let body = bounded_body(response, SEARCH_RESPONSE_MAX_BYTES, cancellation).await?;
        let html = String::from_utf8_lossy(&body);
        parse_ddg_lite(&html)
            .into_iter()
            .take(usize::from(request.max_results()))
            .map(|(title, url, snippet)| WebSearchResult::new(title, url, snippet))
            .collect()
    }
}

#[async_trait]
impl WebProvider for PortableWebProvider {
    fn descriptor(&self) -> WebProviderDescriptor {
        WebProviderDescriptor {
            id: "portable".to_owned(),
            search: true,
            fetch: true,
        }
    }

    async fn search(
        &self,
        request: WebSearchRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<WebSearchResult>, WebError> {
        match std::env::var("BRAVE_API_KEY")
            .ok()
            .filter(|key| !key.trim().is_empty())
        {
            Some(key) => self.brave(&request, &key, cancellation).await,
            None => self.duckduckgo(&request, cancellation).await,
        }
    }

    async fn fetch(
        &self,
        request: WebFetchRequest,
        cancellation: CancellationToken,
    ) -> Result<WebFetchResult, WebError> {
        let initial = url::Url::parse(request.url()).map_err(|_| WebError::invalid_request())?;
        let mut redirects = RedirectChain::new(initial)?;
        let response = loop {
            let target = admit_target(
                redirects.current(),
                self.resolver.as_ref(),
                request.domain_policy(),
                cancellation.clone(),
            )
            .await?;
            let client = pinned_fetch_client(&target)?;
            let response =
                select_cancel(client.get(target.url.as_str()).send(), cancellation.clone()).await?;
            if is_redirect(response.status()) {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(WebError::invalid_response)?;
                redirects.advance(location)?;
                continue;
            }
            break response;
        };
        if !response.status().is_success() {
            return Err(WebError::http());
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let source_limit =
            usize::try_from(request.max_source_bytes()).map_err(|_| WebError::invalid_request())?;
        let output_limit =
            usize::try_from(request.max_bytes()).map_err(|_| WebError::invalid_request())?;
        let body = bounded_body(response, source_limit, cancellation.clone()).await?;
        let raw_truncated = body.len() > source_limit;
        let bounded = if raw_truncated {
            &body[..source_limit]
        } else {
            &body
        };
        if !bounded.is_empty() {
            let document = WebRawDocument::new(
                redirects.current().as_str(),
                content_type.as_deref(),
                bounded.to_vec(),
                raw_truncated,
                output_limit,
            )?;
            if let Some(result) = self
                .processors
                .extract(&document, cancellation.clone())
                .await?
            {
                return Ok(result);
            }
        }
        let mut content = if content_type
            .as_deref()
            .is_some_and(|value| value.to_ascii_lowercase().contains("html"))
        {
            strip_html(&String::from_utf8_lossy(bounded))
        } else if content_type
            .as_deref()
            .is_some_and(|value| !content_type_is_textual(value))
        {
            return Err(WebError::unsupported());
        } else {
            std::str::from_utf8(bounded)
                .map(str::to_owned)
                .map_err(|_| WebError::unsupported())?
        };
        let rendered_truncated = truncate_utf8_bytes(&mut content, output_limit);
        WebFetchResult::new(
            redirects.current().as_str(),
            content,
            content_type.as_deref(),
            raw_truncated || rendered_truncated,
        )
    }
}

fn content_type_is_textual(value: &str) -> bool {
    let value = value
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    value.starts_with("text/")
        || matches!(
            value.as_str(),
            "application/json"
                | "application/ld+json"
                | "application/xml"
                | "application/xhtml+xml"
                | "application/javascript"
        )
        || value.ends_with("+json")
        || value.ends_with("+xml")
}

/// Register the portable provider into the shared registry.
#[must_use]
pub fn portable_web_plugin(config: PortableWebConfig) -> Box<dyn heycode_core::Plugin> {
    struct PortableWebPlugin(PortableWebConfig);

    impl heycode_core::Plugin for PortableWebPlugin {
        fn name(&self) -> &'static str {
            "web-portable"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Provider],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::WebProvider,
                "portable",
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_WEB]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let registry = context
                .get::<WebRegistry>(SERVICE_WEB)
                .ok_or_else(|| heycode_core::CoreError::other("web service type mismatch"))?;
            let provider = PortableWebProvider::new(self.0.clone(), registry.processor_handle())
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            registry
                .register(context, Arc::new(provider))
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))
        }
    }

    Box::new(PortableWebPlugin(config))
}

async fn select_cancel(
    future: impl std::future::Future<Output = Result<reqwest::Response, reqwest::Error>>,
    cancellation: CancellationToken,
) -> Result<reqwest::Response, WebError> {
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(WebError::cancelled()),
        result = future => result.map_err(|error| {
            if error.is_timeout() { WebError::timeout() } else { WebError::network() }
        }),
    }
}

async fn bounded_body(
    response: reqwest::Response,
    maximum: usize,
    cancellation: CancellationToken,
) -> Result<Vec<u8>, WebError> {
    let mut stream = response.bytes_stream();
    let mut output = Vec::new();
    while output.len() <= maximum {
        let next = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(WebError::cancelled()),
            next = stream.next() => next,
        };
        let Some(chunk) = next else {
            break;
        };
        let chunk = chunk.map_err(|_| WebError::network())?;
        let remaining = maximum.saturating_add(1).saturating_sub(output.len());
        output.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if output.len() > maximum {
            break;
        }
    }
    Ok(output)
}

async fn admit_target(
    url: &url::Url,
    resolver: &dyn DnsResolver,
    domain_policy: &crate::WebDomainPolicy,
    cancellation: CancellationToken,
) -> Result<PinnedTarget, WebError> {
    validate_endpoint(url.as_str())?;
    if !domain_policy.allows_url(url) {
        return Err(WebError::policy_denied());
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(WebError::invalid_request)?;
    if port_is_off_protocol(port) {
        return Err(WebError::invalid_request());
    }
    match url.host().ok_or_else(WebError::invalid_request)? {
        url::Host::Ipv4(address) => {
            let ip = std::net::IpAddr::V4(address);
            if ip_is_private(&ip) {
                return Err(WebError::invalid_request());
            }
            Ok(PinnedTarget {
                url: url.clone(),
                domain: None,
                addresses: vec![std::net::SocketAddr::new(ip, port)],
            })
        }
        url::Host::Ipv6(address) => {
            let ip = std::net::IpAddr::V6(address);
            if ip_is_private(&ip) {
                return Err(WebError::invalid_request());
            }
            Ok(PinnedTarget {
                url: url.clone(),
                domain: None,
                addresses: vec![std::net::SocketAddr::new(ip, port)],
            })
        }
        url::Host::Domain(host) => {
            if private_host(host) {
                return Err(WebError::invalid_request());
            }
            let addresses = resolver
                .resolve(host.to_owned(), port, cancellation)
                .await?
                .into_iter()
                .map(|address| std::net::SocketAddr::new(address.ip(), port))
                .collect::<Vec<_>>();
            if addresses.is_empty() || addresses.iter().any(|address| ip_is_private(&address.ip()))
            {
                return Err(WebError::invalid_request());
            }
            Ok(PinnedTarget {
                url: url.clone(),
                domain: Some(host.to_owned()),
                addresses,
            })
        }
    }
}

fn pinned_fetch_client(target: &PinnedTarget) -> Result<reqwest::Client, WebError> {
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .user_agent("heycode-web/0.3")
        // A DNS pin is a direct-connect guarantee. Reqwest enables ambient
        // system/environment proxies by default; a proxy would resolve and
        // connect the original hostname itself, bypassing the admitted address
        // set entirely.
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none());
    if let Some(domain) = &target.domain {
        builder = builder.resolve_to_addrs(domain, &target.addresses);
    }
    builder.build().map_err(|_| WebError::unavailable())
}

fn is_redirect(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

fn validate_endpoint(value: &str) -> Result<(), WebError> {
    let parsed = url::Url::parse(value).map_err(|_| WebError::invalid_request())?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(WebError::invalid_request());
    }
    Ok(())
}

fn private_host(host: &str) -> bool {
    // A fully-qualified name may carry a trailing root dot; `localhost.` and
    // `localhost` name the same host, so the dot must not be a way past the arm.
    let host = host.trim_end_matches('.');
    // RFC 6761 reserves the whole `localhost` TLD for loopback, not just the
    // bare label, so `sub.localhost` belongs here too. RFC 8375 does the same
    // for `home.arpa`. QSEC03 finding W3: these previously reached the resolver,
    // which made a lying answer the only thing between the caller and loopback.
    if [
        "localhost",
        "local",
        "internal",
        "localdomain",
        "lan",
        "home.arpa",
    ]
    .iter()
    .any(|suffix| has_suffix_label(host, suffix))
    {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|address| ip_is_private(&address))
}

/// Whether a parsed URL already names a host that cannot be public, without
/// consulting DNS. The shared value boundary uses this before any provider can
/// publish or dispatch an obvious local/metadata target; the portable provider
/// adds the stronger per-hop DNS admission and connection pin.
pub(crate) fn url_host_is_obviously_private(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(address)) => ip_is_private(&std::net::IpAddr::V4(address)),
        Some(url::Host::Ipv6(address)) => ip_is_private(&std::net::IpAddr::V6(address)),
        Some(url::Host::Domain(host)) => private_host(host),
        None => true,
    }
}

/// IPv4 ranges that must never be reachable through the portable fetcher.
///
/// Special-purpose rather than merely "private": broadcast, multicast and
/// reserved space are not RFC1918 but reaching them from a model-directed
/// request is still an SSRF, and `240.0.0.0/4` in particular contains the
/// limited-broadcast address.
///
/// Deliberately *not* widened past this list. `ranges_that_look_private_but_are
/// _not_stay_reachable` pins the neighbours of every boundary here — 172.15 and
/// 172.32, 100.63 and 100.128, 169.253 and 169.255 — because over-blocking
/// hides real denials just as badly as under-blocking admits them.
/// Ports a model-directed fetch has no business reaching. QSEC03 finding W2.
///
/// The rule is deliberately stated as a rule rather than a list, because a bare
/// list is the shape that rots: **below 1024 only the web's own two ports are
/// admissible**, since everything else down there is a system service, and
/// above 1024 only named datastore and admin ports are refused, because that
/// range is where ordinary HTTP services legitimately live (3000, 5000, 8000,
/// 8080, 8443, 9000 all stay reachable).
///
/// The high-port list targets services that answer on TCP and can be driven by
/// text a fetcher would send — Redis and memcached are the classic protocol-
/// confusion pivots, and an unauthenticated Elasticsearch or Docker daemon is
/// an SSRF's whole objective. Blocking them costs the ability to fetch content
/// *from* them, which is not what a web fetcher is for.
pub(super) fn port_is_off_protocol(port: u16) -> bool {
    if port < 1024 {
        return !matches!(port, 80 | 443);
    }
    matches!(
        port,
        1433 | 1521      // MSSQL, Oracle
            | 2375 | 2376 // Docker daemon, plain and TLS
            | 2379 | 2380 // etcd client and peer
            | 3306        // MySQL
            | 3389        // RDP
            | 5432        // PostgreSQL
            | 5900        // VNC
            | 5984        // CouchDB
            | 6379        // Redis
            | 9042        // Cassandra
            | 9200 | 9300 // Elasticsearch HTTP and transport
            | 11211       // memcached
            | 27017 | 27018 | 27019 // MongoDB
    )
}

/// Single-hop raw browser transport reuses the fetcher's actual socket admission.
pub(super) async fn browser_request(
    request: crate::BrowserHttpRequest,
    local: Option<&crate::BrowserLocalOrigin>,
    policy: &crate::WebDomainPolicy,
    cancellation: CancellationToken,
) -> Result<crate::BrowserHttpResponse, WebError> {
    if request.url.len() > 8192
        || request.body.len() > 1024 * 1024
        || request.headers.len() > 128
        || request
            .headers
            .iter()
            .map(|(k, v)| k.len() + v.len())
            .sum::<usize>()
            > 32768
        || !matches!(
            request.method.as_str(),
            "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS"
        )
    {
        return Err(WebError::invalid_request());
    }
    let url = url::Url::parse(&request.url).map_err(|_| WebError::invalid_request())?;
    validate_endpoint(url.as_str())?;
    if !policy.allows_url(&url) {
        return Err(WebError::policy_denied());
    }
    let target = if crate::browser::local_matches(local, &url) {
        PinnedTarget {
            url: url.clone(),
            domain: None,
            addresses: Vec::new(),
        }
    } else {
        admit_target(&url, &SystemDnsResolver, policy, cancellation.clone()).await?
    };
    let client = pinned_fetch_client(&target)?;
    let method = reqwest::Method::from_bytes(request.method.as_bytes())
        .map_err(|_| WebError::invalid_request())?;
    let mut builder = client.request(method, url);
    for (name, value) in request.headers {
        if matches!(
            name.to_ascii_lowercase().as_str(),
            "host"
                | "connection"
                | "proxy-authorization"
                | "proxy-connection"
                | "transfer-encoding"
                | "content-length"
                | "accept-encoding"
                | "upgrade"
                | "te"
                | "trailer"
                | "keep-alive"
        ) {
            continue;
        }
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| WebError::invalid_request())?;
        let value = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|_| WebError::invalid_request())?;
        builder = builder.header(name, value);
    }
    let response = select_cancel(
        builder
            .header("accept-encoding", "identity")
            .body(request.body)
            .send(),
        cancellation.clone(),
    )
    .await?;
    let status = response.status().as_u16();
    let mut headers = Vec::new();
    let mut header_bytes = 0usize;
    for (name, value) in response.headers() {
        if matches!(
            name.as_str(),
            "connection" | "transfer-encoding" | "content-length" | "upgrade" | "keep-alive"
        ) {
            continue;
        }
        let value = value.to_str().map_err(|_| WebError::invalid_response())?;
        header_bytes += name.as_str().len() + value.len();
        if header_bytes > 32768 || headers.len() >= 128 {
            return Err(WebError::invalid_response());
        }
        headers.push((name.as_str().to_owned(), value.to_owned()));
    }
    let body = bounded_body(response, 4 * 1024 * 1024, cancellation).await?;
    if body.len() > 4 * 1024 * 1024 {
        return Err(WebError::invalid_response());
    }
    Ok(crate::BrowserHttpResponse {
        status,
        headers,
        body,
    })
}

/// Whether `host` ends in `suffix` on a label boundary.
///
/// `ends_with(".lan")` would already be label-safe, but stating the boundary
/// once means a bare suffix can never be added by accident — `hostname.lan`
/// matches, `notalan` does not, and `lan` itself does.
fn has_suffix_label(host: &str, suffix: &str) -> bool {
    if host.eq_ignore_ascii_case(suffix) {
        return true;
    }
    host.len() > suffix.len()
        && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
        && host[host.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

fn ipv4_is_special(address: &std::net::Ipv4Addr) -> bool {
    let [first, second, third, _] = address.octets();
    address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || address.is_unspecified()
        || address.is_multicast()
        || address.is_documentation()
        // 0.0.0.0/8, "this network".
        || first == 0
        // 240.0.0.0/4 reserved, which subsumes 255.255.255.255 limited broadcast.
        || first >= 240
        // 100.64.0.0/10 carrier-grade NAT.
        || (first == 100 && (64..=127).contains(&second))
        // 192.0.0.0/24 IETF protocol assignments.
        || (first == 192 && second == 0 && third == 0)
        // 198.18.0.0/15 benchmarking.
        || (first == 198 && (18..=19).contains(&second))
}

/// The IPv4 address an IPv6 address actually delivers packets to, if any.
///
/// This is the half that made the guard bypassable. The predicate previously
/// used `to_ipv4_mapped`, which recognises only `::ffff:a.b.c.d`, so three
/// other standard ways of writing an IPv4 destination inside an IPv6 literal
/// walked straight past every IPv4 rule — including loopback:
///
/// * `::7f00:1` — IPv4-compatible (deprecated, still parses and still routes)
/// * `64:ff9b::7f00:1` — the NAT64 well-known prefix
/// * `2002:7f00:1::` — 6to4
///
/// Returning the embedded address and recursing means every IPv4 rule applies
/// through every encoding automatically, so a future IPv4 range added to
/// [`ipv4_is_special`] cannot be reachable through a spelling nobody thought
/// to re-check. QSEC03 finding W1.
fn embedded_ipv4(address: &std::net::Ipv6Addr) -> Option<std::net::Ipv4Addr> {
    let segments = address.segments();
    // Covers both `::a.b.c.d` (IPv4-compatible) and `::ffff:a.b.c.d` (mapped).
    if let Some(embedded) = address.to_ipv4() {
        return Some(embedded);
    }
    // 2002::/16 — 6to4 carries the IPv4 in the two segments after the prefix.
    if segments[0] == 0x2002 {
        return Some(std::net::Ipv4Addr::from(
            (u32::from(segments[1]) << 16) | u32::from(segments[2]),
        ));
    }
    // 64:ff9b::/96 and 64:ff9b:1::/48 — NAT64 carries it in the low 32 bits.
    if segments[0] == 0x0064 && segments[1] == 0xff9b {
        return Some(std::net::Ipv4Addr::from(
            (u32::from(segments[6]) << 16) | u32::from(segments[7]),
        ));
    }
    None
}

fn ip_is_private(address: &std::net::IpAddr) -> bool {
    match address {
        std::net::IpAddr::V4(address) => ipv4_is_special(address),
        std::net::IpAddr::V6(address) => {
            // An IPv6 address that carries an IPv4 destination is judged as
            // that destination; the IPv6-only arms below cannot apply to it.
            if let Some(embedded) = embedded_ipv4(address) {
                return ipv4_is_special(&embedded);
            }
            address.is_loopback()
                || address.is_unspecified()
                || address.is_multicast()
                || (address.segments()[0] & 0xfe00) == 0xfc00
                || (address.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Test/public security predicate shared with portable-provider fixtures.
#[doc(hidden)]
#[must_use]
pub fn ip_is_private_for_tests(address: &std::net::IpAddr) -> bool {
    ip_is_private(address)
}

fn strip_html(html: &str) -> String {
    let mut output = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    for character in html.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            character if !in_tag => output.push(character),
            _ => {}
        }
    }
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Test/public HTML normalization helper.
#[doc(hidden)]
#[must_use]
pub fn strip_html_for_tests(html: &str) -> String {
    strip_html(html)
}

fn decode_ddg_redirect(url: &str) -> String {
    let decoded_amp = url.replace("&amp;", "&");
    let Some(index) = decoded_amp.find("uddg=") else {
        return decoded_amp;
    };
    let encoded = &decoded_amp[index + 5..];
    let encoded = &encoded[..encoded.find('&').unwrap_or(encoded.len())];
    percent_decode(encoded)
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                if let Some(decoded) = value
                    .get(index + 1..index + 3)
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                {
                    output.push(decoded);
                    index += 3;
                    continue;
                }
                output.push(bytes[index]);
                index += 1;
            }
            b'+' => {
                output.push(b' ');
                index += 1;
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&output).into_owned()
}

/// Parse DuckDuckGo Lite result anchors/snippets in document order.
#[must_use]
pub fn parse_ddg_lite(html: &str) -> Vec<(String, String, String)> {
    let mut output = Vec::new();
    let mut cursor = 0_usize;
    while let Some(relative) = html[cursor..]
        .find("class='result-link'")
        .or_else(|| html[cursor..].find("class=\"result-link\""))
    {
        let class_position = cursor + relative;
        let Some(anchor_open) = html[..class_position].rfind("<a") else {
            break;
        };
        let segment = &html[anchor_open..];
        let Some(href_start) = segment
            .find("href=\"")
            .map(|index| index + 6)
            .or_else(|| segment.find("href='").map(|index| index + 6))
        else {
            break;
        };
        let after_href = &segment[href_start..];
        let Some(href_length) = after_href.find(['"', '\'']) else {
            break;
        };
        let url = decode_ddg_redirect(&after_href[..href_length]);
        let after_tag = &after_href[href_length..];
        let Some(tag_end) = after_tag.find('>') else {
            break;
        };
        let resume = anchor_open + href_start + href_length + tag_end + 1;
        let title_segment = &after_tag[tag_end + 1..];
        let Some(title_end) = title_segment.find('<') else {
            break;
        };
        let title = strip_html(title_segment[..title_end].trim());
        let snippet_region = &html[class_position..];
        let next_anchor = snippet_region.find("<a ").unwrap_or(snippet_region.len());
        let snippet = snippet_region[..next_anchor]
            .find("result-snippet")
            .map(|position| {
                let following = &snippet_region[position..];
                let content_start = following.find('>').map_or(0, |index| index + 1);
                let content = &following[content_start..];
                let content_end = content.find("</td>").unwrap_or(content.len());
                strip_html(&content[..content_end]).trim().to_owned()
            })
            .unwrap_or_default();
        if !url.is_empty()
            && !title.is_empty()
            && !output
                .iter()
                .any(|(_title, existing, _snippet)| existing == &url)
        {
            output.push((title, url, snippet));
        }
        cursor = resume;
        if output.len() >= 32 {
            break;
        }
    }
    output
}

fn truncate_utf8_bytes(value: &mut String, maximum: usize) -> bool {
    if value.len() <= maximum {
        return false;
    }
    let mut boundary = maximum;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    true
}

/// QSEC03 adversarial SSRF matrix; see `ssrf_matrix.rs`.
#[cfg(test)]
#[path = "ssrf_matrix.rs"]
mod ssrf_matrix;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct MockResolver {
        addresses: Vec<std::net::SocketAddr>,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl DnsResolver for MockResolver {
        async fn resolve(
            &self,
            _host: String,
            _port: u16,
            _cancellation: CancellationToken,
        ) -> Result<Vec<std::net::SocketAddr>, WebError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(self.addresses.clone())
        }
    }

    #[test]
    fn redirect_chain_rejects_loops_unsafe_schemes_userinfo_and_exhaustion() {
        let mut chain =
            RedirectChain::new(url::Url::parse("https://example.test/a").unwrap()).unwrap();
        chain.advance("/b").unwrap();
        assert!(chain.advance("/a").is_err());

        let mut unsafe_scheme =
            RedirectChain::new(url::Url::parse("https://example.test/a").unwrap()).unwrap();
        assert!(unsafe_scheme.advance("file:///etc/passwd").is_err());
        assert!(
            unsafe_scheme
                .advance("https://user:pass@example.test/private")
                .is_err()
        );

        let mut bounded =
            RedirectChain::new(url::Url::parse("https://example.test/0").unwrap()).unwrap();
        for index in 1..=5 {
            bounded.advance(&format!("/{index}")).unwrap();
        }
        assert!(bounded.advance("/6").is_err());
    }

    #[tokio::test]
    async fn search_redirect_is_not_followed() {
        use std::io::{Read as _, Write as _};

        let sink = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let sink_address = sink.local_addr().unwrap();
        sink.set_nonblocking(true).unwrap();
        let sink_task = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(750);
            loop {
                match sink.accept() {
                    Ok((_stream, _peer)) => return true,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() >= deadline {
                            return false;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(error) => panic!("sink accept failed: {error}"),
                }
            }
        });

        let source = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let source_address = source.local_addr().unwrap();
        let source_task = std::thread::spawn(move || {
            let (mut stream, _peer) = source.accept().unwrap();
            let mut request = [0_u8; 16 * 1024];
            let _read = stream.read(&mut request).unwrap();
            write!(
                stream,
                "HTTP/1.1 302 Found\r\nLocation: http://{sink_address}/credential-leak\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        });
        let endpoint = format!("http://{source_address}");
        let registry = WebRegistry::new();
        let provider = PortableWebProvider::new(
            PortableWebConfig::new(endpoint.clone(), endpoint).unwrap(),
            registry.processor_handle(),
        )
        .unwrap();
        let error = provider
            .search(
                WebSearchRequest::new("redirect", 1).unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.class(), crate::WebErrorClass::Http);
        source_task.join().unwrap();
        assert!(!sink_task.join().unwrap());
    }

    #[test]
    fn dns_resolution_has_no_detachable_join_handle() {
        let source = include_str!("portable.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        assert!(!production.contains("spawn_blocking"));
    }

    #[tokio::test]
    async fn admission_denies_any_private_answer_and_pins_one_public_resolution() {
        let mixed = MockResolver {
            addresses: vec![
                "93.184.216.34:443".parse().unwrap(),
                "127.0.0.1:443".parse().unwrap(),
            ],
            calls: AtomicUsize::new(0),
        };
        let url = url::Url::parse("https://example.test/path").unwrap();
        assert!(
            admit_target(
                &url,
                &mixed,
                &crate::WebDomainPolicy::default(),
                CancellationToken::new(),
            )
            .await
            .is_err()
        );

        let public = MockResolver {
            addresses: vec!["93.184.216.34:443".parse().unwrap()],
            calls: AtomicUsize::new(0),
        };
        let target = admit_target(
            &url,
            &public,
            &crate::WebDomainPolicy::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(public.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(target.addresses, public.addresses);
        assert!(pinned_fetch_client(&target).is_ok());
        assert_eq!(public.calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        let domains = crate::WebDomainPolicy::new(
            vec!["example.test".to_owned()],
            vec!["private.example.test".to_owned()],
        )
        .unwrap();
        let mut policy_redirect =
            RedirectChain::new(url::Url::parse("https://example.test/start").unwrap()).unwrap();
        policy_redirect
            .advance("https://private.example.test/blocked")
            .unwrap();
        let error = admit_target(
            policy_redirect.current(),
            &public,
            &domains,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.class(), crate::WebErrorClass::PolicyDenied);
        assert_eq!(public.calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        let metadata = url::Url::parse("http://169.254.169.254/latest/meta-data").unwrap();
        assert!(
            admit_target(
                &metadata,
                &public,
                &crate::WebDomainPolicy::default(),
                CancellationToken::new(),
            )
            .await
            .is_err()
        );
        assert_eq!(public.calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        let mut redirected =
            RedirectChain::new(url::Url::parse("https://example.test/start").unwrap()).unwrap();
        // Same scheme deliberately: this case is about the *admission* layer
        // refusing cloud metadata, and an https-to-http hop is now refused a
        // step earlier by the downgrade rule, which would prove nothing here.
        redirected
            .advance("https://169.254.169.254/latest/meta-data")
            .unwrap();
        assert!(
            admit_target(
                redirected.current(),
                &public,
                &crate::WebDomainPolicy::default(),
                CancellationToken::new(),
            )
            .await
            .is_err()
        );
        assert_eq!(public.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
