//! Raw HTTP transport for an isolated browser, with the same public socket policy as fetch.
use crate::{WebError, WebRegistry};
use tokio_util::sync::CancellationToken;

/// Explicit local application authority. Only one numeric loopback HTTP origin.
#[derive(Clone)]
pub struct BrowserLocalOrigin(url::Url);
impl BrowserLocalOrigin {
    /// Admit an explicit local application origin, never a hostname or arbitrary LAN address.
    ///
    /// # Errors
    /// Requires HTTP, numeric loopback, an unprivileged application port and no path/query/userinfo.
    pub fn new(value: &str) -> Result<Self, WebError> {
        let url = url::Url::parse(value).map_err(|_| WebError::invalid_request())?;
        let loopback = match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        let port = url.port().ok_or_else(WebError::invalid_request)?;
        if url.scheme() != "http"
            || !loopback
            || port < 1024
            || super::portable::port_is_off_protocol(port)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(WebError::invalid_request());
        }
        Ok(Self(url))
    }
    /// Canonical origin, safe to use as the exact local grant comparison.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
    fn matches(&self, url: &url::Url) -> bool {
        self.0.origin() == url.origin()
    }
}
impl std::fmt::Debug for BrowserLocalOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BrowserLocalOrigin([redacted])")
    }
}

/// Browser-authored request. No ambient host credentials or proxy configuration are used.
/// Fields are revalidated at every dispatch; bodies are never included in Debug.
pub struct BrowserHttpRequest {
    /// HTTP(S) destination, re-admitted independently on every redirect/subresource.
    pub url: String,
    /// GET/HEAD/POST/PUT/PATCH/DELETE/OPTIONS only.
    pub method: String,
    /// Browser context headers, bounded to 32 KiB; hop-by-hop/host headers are removed.
    pub headers: Vec<(String, String)>,
    /// At most 1 MiB; file upload tooling is intentionally absent.
    pub body: Vec<u8>,
}
/// One bounded raw response. Redirects are returned to the browser, never followed here.
pub struct BrowserHttpResponse {
    /// HTTP status.
    pub status: u16,
    /// Response headers; hop-by-hop and length headers are removed.
    pub headers: Vec<(String, String)>,
    /// Complete response body, at most 4 MiB. Oversized responses fail instead of truncating code.
    pub body: Vec<u8>,
}
impl std::fmt::Debug for BrowserHttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BrowserHttpRequest([redacted])")
    }
}
impl std::fmt::Debug for BrowserHttpResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserHttpResponse")
            .field("status", &self.status)
            .field("bytes", &self.body.len())
            .finish()
    }
}

impl WebRegistry {
    /// Fulfill one browser request through live fetch-domain policy and DNS-pinned direct HTTP.
    ///
    /// An explicit local grant only permits its exact numeric loopback origin. Domain rules still
    /// apply. No arbitrary private address, ambient proxy, redirect follow or credential lookup.
    /// The calling approved tool owns effects, aggregate request budgets and cancellation.
    ///
    /// # Errors
    /// Policy denial, invalid/bounded request, DNS admission, HTTP/body limit, cancellation or shutdown.
    pub async fn browser_request(
        &self,
        request: BrowserHttpRequest,
        local: Option<&BrowserLocalOrigin>,
        cancellation: CancellationToken,
    ) -> Result<BrowserHttpResponse, WebError> {
        let policy = self.policy_snapshot()?;
        let url = request.url.clone();
        let response =
            super::portable::browser_request(request, local, policy.fetch_domains(), cancellation)
                .await?;
        // A retired/invalidated policy cannot publish an in-flight response.
        if !self.policy_snapshot()?.fetch_domains().allows(&url) {
            return Err(WebError::policy_denied());
        }
        Ok(response)
    }
}

pub(super) fn local_matches(local: Option<&BrowserLocalOrigin>, url: &url::Url) -> bool {
    local.is_some_and(|local| local.matches(url))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::{WebDomainPolicy, WebPolicy};
    fn request(url: &str) -> BrowserHttpRequest {
        BrowserHttpRequest {
            url: url.into(),
            method: "GET".into(),
            headers: vec![],
            body: vec![],
        }
    }
    #[test]
    fn local_authority_is_exact_numeric_loopback_application_origin() {
        for url in ["http://127.0.0.1:3000/", "http://[::1]:8000/"] {
            assert!(BrowserLocalOrigin::new(url).is_ok());
        }
        for url in [
            "http://localhost:3000/",
            "http://192.168.1.1:3000/",
            "http://127.0.0.1/",
            "http://127.0.0.1:6379/",
            "http://127.0.0.1:3000/path",
            "http://user@127.0.0.1:3000/",
            "http://127.0.0.1:3000/?secret=x",
        ] {
            assert!(BrowserLocalOrigin::new(url).is_err(), "{url}");
        }
        let local = BrowserLocalOrigin::new("http://127.0.0.1:3000/").unwrap();
        assert!(local.matches(&url::Url::parse("http://127.0.0.1:3000/path").unwrap()));
        assert!(!local.matches(&url::Url::parse("http://127.0.0.1:3001/path").unwrap()));
    }
    #[tokio::test]
    async fn raw_browser_policy_denies_private_hosts_methods_and_domain_blocks() {
        let web = WebRegistry::new();
        for url in [
            "http://127.0.0.1:3000/",
            "http://169.254.169.254/",
            "http://[::ffff:127.0.0.1]:3000/",
            "file:///etc/passwd",
            "http://example.com:6379/",
        ] {
            assert!(
                web.browser_request(request(url), None, CancellationToken::new())
                    .await
                    .is_err()
            );
        }
        web.replace_policy(
            WebPolicy::new(
                None,
                None,
                WebDomainPolicy::default(),
                WebDomainPolicy::new(vec![], vec!["example.com".into()]).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(
            web.browser_request(
                request("https://www.example.com/"),
                None,
                CancellationToken::new()
            )
            .await
            .is_err()
        );
        let mut invalid = request("https://example.org/");
        invalid.method = "CONNECT".into();
        assert!(
            web.browser_request(invalid, None, CancellationToken::new())
                .await
                .is_err()
        );
        let local = BrowserLocalOrigin::new("http://127.0.0.1:3000/").unwrap();
        assert!(
            web.browser_request(
                request(local.as_str()),
                Some(&local),
                CancellationToken::new()
            )
            .await
            .is_err(),
            "domain policy also applies to explicit local grants"
        );
    }
    #[tokio::test]
    async fn redirects_are_not_followed_and_bodies_are_complete() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let local = BrowserLocalOrigin::new(&origin).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 8192];
            assert!(stream.read(&mut bytes).await.unwrap() > 0);
            stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnext").await.unwrap();
        });
        let web = WebRegistry::new();
        let response = web
            .browser_request(request(&origin), Some(&local), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(response.status, 302);
        assert_eq!(response.body, b"next");
        assert!(
            response
                .headers
                .iter()
                .any(|(key, value)| key == "location" && value == "http://169.254.169.254/")
        );
        server.await.unwrap();
        assert!(
            web.browser_request(
                request("http://169.254.169.254/"),
                Some(&local),
                CancellationToken::new()
            )
            .await
            .is_err()
        );
    }
}
