//! Where an export goes, and how it authenticates — described, never carried.
//!
//! Two things about an OTLP destination are routinely secrets, and both are
//! routinely printed.
//!
//! The first is the address. `OTEL_EXPORTER_OTLP_ENDPOINT` is a URL, and a URL
//! has two places to put a credential that look like configuration rather than
//! like a secret: userinfo (`https://user:token@collector/`) and the query
//! string (`https://collector/v1/metrics?api-key=…`). Vendors document both.
//! [`OtlpEndpoint::parse`] refuses an address with either, so an endpoint value
//! never holds one, and no `Debug`, log line or support bundle can print one.
//!
//! Refusing rather than stripping is deliberate. Stripping produces an endpoint
//! that looks configured and silently cannot authenticate; the operator learns
//! about it from a dashboard that stays empty. Refusing says so at the moment
//! the value is read, and the fault is a closed [`TelemetryFault`] that carries
//! none of the offending text.
//!
//! Percent-encoding is refused for a related reason: a screen that runs before
//! decoding proves nothing about what is actually sent. `/v1/%6d%65trics` and
//! `/v1/metrics` reach the same handler, and only one of them is what the
//! screen looked at. An OTLP path needs no encoding, so the honest rule is that
//! it may not have any.
//!
//! The second is the credential itself. [`OtlpAuth`] follows
//! `SettingsField::Secret { path, configured, origin }`: it names which header
//! carries the credential and which credential id resolves it, and it has
//! **nowhere to put the value**. Resolution happens in the transport, at send
//! time, through the credentials service — so this crate never holds the
//! secret, and a `Debug` of the whole exporter cannot leak what it never had.

use serde::{Deserialize, Serialize};

use crate::{Label, TelemetryFault};

/// Longest endpoint address this parser will accept, in bytes.
pub(crate) const ENDPOINT_MAX_BYTES: usize = 512;

/// A bounded absolute `http`/`https` address with no credential in it.
///
/// Construction is [`Self::parse`] and deserialization routes through the same
/// constructor, so a settings file cannot restore an endpoint whose userinfo
/// the parser would have refused (GOTCHAS #161).
///
/// This type is **not** reachable from an exported payload. It describes a
/// destination for diagnostics; the document that leaves the machine says
/// nothing about where it is going.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OtlpEndpoint {
    scheme: Scheme,
    authority: String,
    path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Scheme {
    Http,
    Https,
}

impl Scheme {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}

impl OtlpEndpoint {
    /// Validate and screen an OTLP collector address.
    ///
    /// # Errors
    /// [`TelemetryFault::EndpointCarriesCredential`] when the address contains
    /// recognizable credential material, or userinfo before the host — the one
    /// position in a URL that is credential material by definition.
    /// [`TelemetryFault::MalformedEndpoint`] for anything else: a scheme other
    /// than `http`/`https`, an empty or ill-formed host, a query, a fragment,
    /// percent-encoding, or an address over [`ENDPOINT_MAX_BYTES`].
    pub fn parse(address: &str) -> Result<Self, TelemetryFault> {
        // The credential screen runs first, so an address that demonstrably
        // carries a secret is reported as carrying one wherever it sits, rather
        // than being classified by whichever structural rule it also breaks.
        // The ordering is the claim; a test pins it with an address wrong on
        // both counts.
        crate::event::screen_for_credentials(address)
            .map_err(|_| TelemetryFault::EndpointCarriesCredential)?;
        if address.is_empty() || address.len() > ENDPOINT_MAX_BYTES {
            return Err(TelemetryFault::MalformedEndpoint);
        }
        let (scheme, rest) = split_scheme(address).ok_or(TelemetryFault::MalformedEndpoint)?;
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, remainder) = rest.split_at(end);
        if authority.contains('@') {
            return Err(TelemetryFault::EndpointCarriesCredential);
        }
        if remainder.contains(['?', '#']) {
            return Err(TelemetryFault::MalformedEndpoint);
        }
        let authority = normalize_authority(authority).ok_or(TelemetryFault::MalformedEndpoint)?;
        let path = normalize_path(remainder).ok_or(TelemetryFault::MalformedEndpoint)?;
        Ok(Self {
            scheme,
            authority,
            path,
        })
    }

    /// `http` or `https`.
    #[must_use]
    pub const fn scheme(&self) -> &'static str {
        self.scheme.as_str()
    }

    /// Host and optional port, lowercased.
    #[must_use]
    pub fn authority(&self) -> &str {
        &self.authority
    }

    /// Path, always beginning with `/` and never percent-encoded.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The address as this type holds it — the only rendering there is.
    #[must_use]
    pub fn as_address(&self) -> String {
        format!("{}://{}{}", self.scheme.as_str(), self.authority, self.path)
    }
}

impl TryFrom<String> for OtlpEndpoint {
    type Error = TelemetryFault;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<OtlpEndpoint> for String {
    fn from(endpoint: OtlpEndpoint) -> Self {
        endpoint.as_address()
    }
}

impl std::fmt::Display for OtlpEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.as_address())
    }
}

impl std::fmt::Debug for OtlpEndpoint {
    /// Renders the same address [`Self::as_address`] does.
    ///
    /// Written rather than derived so that adding a field to this struct does
    /// not silently start printing it. The parser already refuses userinfo and
    /// queries, so there is nothing here to hide — but a derive would make that
    /// a property of today's fields rather than of this function.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "OtlpEndpoint({})", self.as_address())
    }
}

fn split_scheme(address: &str) -> Option<(Scheme, &str)> {
    for scheme in [Scheme::Https, Scheme::Http] {
        let prefix = format!("{}://", scheme.as_str());
        // `get`, not indexing: a multi-byte character straddling the prefix
        // length would make a slice panic, and this parser reads whatever an
        // environment variable happened to contain.
        let Some(head) = address.get(..prefix.len()) else {
            continue;
        };
        if head.eq_ignore_ascii_case(&prefix) {
            return address.get(prefix.len()..).map(|rest| (scheme, rest));
        }
    }
    None
}

/// Lowercase a host, accepting a bracketed IPv6 literal, and refuse anything a
/// collector address has no reason to contain.
fn normalize_authority(authority: &str) -> Option<String> {
    if authority.is_empty() {
        return None;
    }
    let (host, port) = match authority.strip_prefix('[') {
        Some(literal) => {
            let close = literal.find(']')?;
            let (inside, after) = literal.split_at(close);
            if inside.is_empty()
                || !inside
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() || byte == b':' || byte == b'.')
            {
                return None;
            }
            (
                format!("[{inside}]"),
                after.get(1..).unwrap_or_default().to_owned(),
            )
        }
        None => match authority.split_once(':') {
            Some((host, port)) => (host.to_owned(), format!(":{port}")),
            None => (authority.to_owned(), String::new()),
        },
    };
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if bare.is_empty()
        || !bare
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b':' | b'_'))
    {
        return None;
    }
    if !port.is_empty() {
        let digits = port.strip_prefix(':')?;
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
    }
    Some(format!("{}{port}", host.to_ascii_lowercase()))
}

/// Accept an unencoded path, defaulting an empty one to `/`.
fn normalize_path(path: &str) -> Option<String> {
    if path.is_empty() {
        return Some("/".to_owned());
    }
    if !path.starts_with('/') {
        return None;
    }
    let acceptable = path.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/')
    });
    acceptable.then(|| path.to_owned())
}

/// How a transport authenticates to a collector, stated without the credential.
///
/// The `Configured` variant names a header and a credential id. It has no field
/// for the value, which is the whole redaction strategy: there is nowhere for a
/// token to be, so no renderer, log or support bundle can print one and no
/// later edit to a render path can reintroduce the bug.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
#[non_exhaustive]
pub enum OtlpAuth {
    /// The collector is reached without authentication.
    NoneConfigured,
    /// A credential is configured and resolved by the transport at send time.
    Configured {
        /// Which request header carries it, as a bounded identifier.
        header: Label,
        /// Which credential the transport resolves, by id — never by value.
        credential: Label,
    },
}

impl OtlpAuth {
    /// Whether a credential is configured. All a diagnostic may know.
    #[must_use]
    pub const fn configured(&self) -> bool {
        matches!(self, Self::Configured { .. })
    }

    /// The header a configured credential is sent in.
    #[must_use]
    pub const fn header(&self) -> Option<&Label> {
        match self {
            Self::NoneConfigured => None,
            Self::Configured { header, .. } => Some(header),
        }
    }

    /// The credential id a transport resolves at send time.
    #[must_use]
    pub const fn credential(&self) -> Option<&Label> {
        match self {
            Self::NoneConfigured => None,
            Self::Configured { credential, .. } => Some(credential),
        }
    }
}

impl std::fmt::Display for OtlpAuth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoneConfigured => formatter.write_str("none"),
            Self::Configured { header, .. } => write!(formatter, "configured:{header}"),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const SECRET: &str = "sk-ant-api03-0123456789abcdef";

    #[test]
    fn an_endpoint_keeps_scheme_host_port_and_path() {
        let endpoint =
            OtlpEndpoint::parse("https://collector.example.com:4318/v1/metrics").unwrap();
        assert_eq!(endpoint.scheme(), "https");
        assert_eq!(endpoint.authority(), "collector.example.com:4318");
        assert_eq!(endpoint.path(), "/v1/metrics");
        assert_eq!(
            endpoint.as_address(),
            "https://collector.example.com:4318/v1/metrics"
        );
    }

    #[test]
    fn an_endpoint_lowercases_the_scheme_and_host_and_defaults_the_path() {
        let endpoint = OtlpEndpoint::parse("HTTP://Collector.Example.COM:4318").unwrap();
        assert_eq!(endpoint.as_address(), "http://collector.example.com:4318/");
    }

    #[test]
    fn a_local_collector_reached_over_ipv6_parses() {
        let endpoint = OtlpEndpoint::parse("http://[::1]:4318/v1/metrics").unwrap();
        assert_eq!(endpoint.authority(), "[::1]:4318");
        assert_eq!(endpoint.as_address(), "http://[::1]:4318/v1/metrics");
    }

    #[test]
    fn an_endpoint_refuses_userinfo_because_that_position_is_always_a_credential() {
        for address in [
            "https://token@collector.example.com/v1/metrics",
            "https://user:hunter2@collector.example.com/v1/metrics",
            "https://user:@collector.example.com/",
        ] {
            assert_eq!(
                OtlpEndpoint::parse(address),
                Err(TelemetryFault::EndpointCarriesCredential),
                "{address}"
            );
        }
    }

    #[test]
    fn an_endpoint_refuses_a_query_or_a_fragment() {
        for address in [
            "https://collector.example.com/v1/metrics?api-key=abcdef",
            "https://collector.example.com/v1/metrics#anchor",
            "https://collector.example.com?token=1",
        ] {
            assert_eq!(
                OtlpEndpoint::parse(address),
                Err(TelemetryFault::MalformedEndpoint),
                "{address}"
            );
        }
    }

    #[test]
    fn an_endpoint_carrying_a_recognized_secret_is_classified_as_carrying_one() {
        // Wrong on two counts at once — a recognized credential *and* a query.
        // The credential screen runs first, so this must report the credential;
        // without the ordering it would report the structural fault and an
        // operator would be told the address is malformed rather than leaky.
        let address = format!("https://collector.example.com/v1/metrics?token={SECRET}");
        assert_eq!(
            OtlpEndpoint::parse(&address),
            Err(TelemetryFault::EndpointCarriesCredential)
        );
        let host = format!("https://{SECRET}.example.com/v1/metrics");
        assert_eq!(
            OtlpEndpoint::parse(&host),
            Err(TelemetryFault::EndpointCarriesCredential)
        );
    }

    #[test]
    fn an_endpoint_refuses_percent_encoding_that_a_screen_could_not_see_through() {
        assert_eq!(
            OtlpEndpoint::parse("https://collector.example.com/v1/%73k-ant-api03-0123456789ab"),
            Err(TelemetryFault::MalformedEndpoint),
            "a screen that runs before decoding proves nothing about what is sent"
        );
    }

    #[test]
    fn an_endpoint_refuses_a_scheme_that_is_not_http() {
        for address in [
            "file:///etc/passwd",
            "grpc://collector.example.com",
            "collector.example.com:4318",
            "",
        ] {
            assert_eq!(
                OtlpEndpoint::parse(address),
                Err(TelemetryFault::MalformedEndpoint),
                "{address}"
            );
        }
    }

    #[test]
    fn an_endpoint_refuses_an_empty_or_ill_formed_host_and_port() {
        for address in [
            "https:///v1/metrics",
            "https://collector.example.com:/v1",
            "https://collector.example.com:port/v1",
            "https://coll ector.example.com/v1",
            "https://[::1/v1",
            "https://[]/v1",
        ] {
            assert_eq!(
                OtlpEndpoint::parse(address),
                Err(TelemetryFault::MalformedEndpoint),
                "{address}"
            );
        }
    }

    #[test]
    fn an_endpoint_refuses_an_address_over_the_size_cap() {
        let long = format!(
            "https://{}.example.com/v1/metrics",
            "a".repeat(ENDPOINT_MAX_BYTES)
        );
        assert_eq!(
            OtlpEndpoint::parse(&long),
            Err(TelemetryFault::MalformedEndpoint)
        );
    }

    #[test]
    fn an_endpoint_cannot_be_deserialized_past_its_constructor() {
        let leaky = serde_json::from_str::<OtlpEndpoint>(
            "\"https://user:hunter2@collector.example.com/v1/metrics\"",
        );
        assert!(
            leaky.is_err(),
            "deserialization must route through OtlpEndpoint::parse"
        );
        let secret =
            serde_json::from_str::<OtlpEndpoint>(&format!("\"https://c.example.com/{SECRET}\""));
        assert!(secret.is_err(), "the credential screen must run on read");
    }

    #[test]
    fn an_endpoint_round_trips_through_its_own_rendering() {
        let endpoint =
            OtlpEndpoint::parse("https://collector.example.com:4318/v1/metrics").unwrap();
        let json = serde_json::to_string(&endpoint).unwrap();
        assert_eq!(json, "\"https://collector.example.com:4318/v1/metrics\"");
        assert_eq!(
            serde_json::from_str::<OtlpEndpoint>(&json).unwrap(),
            endpoint
        );
        assert_eq!(
            format!("{endpoint:?}"),
            "OtlpEndpoint(https://collector.example.com:4318/v1/metrics)"
        );
        assert_eq!(
            format!("{endpoint}"),
            "https://collector.example.com:4318/v1/metrics"
        );
    }

    #[test]
    fn auth_reports_that_a_credential_exists_and_has_nowhere_to_hold_it() {
        let auth = OtlpAuth::Configured {
            header: Label::new("authorization").unwrap(),
            credential: Label::new("otlp/collector").unwrap(),
        };
        assert!(auth.configured());
        assert_eq!(auth.header().map(Label::as_str), Some("authorization"));
        assert_eq!(auth.credential().map(Label::as_str), Some("otlp/collector"));

        let rendered = format!("{auth:?} {auth} {}", serde_json::to_string(&auth).unwrap());
        assert!(!rendered.contains(SECRET), "{rendered}");
        assert!(
            !rendered.contains("Bearer"),
            "there is no field a token could be in: {rendered}"
        );
    }

    #[test]
    fn unauthenticated_egress_reports_no_credential_at_all() {
        let auth = OtlpAuth::NoneConfigured;
        assert!(!auth.configured());
        assert!(auth.header().is_none());
        assert!(auth.credential().is_none());
        assert_eq!(format!("{auth}"), "none");
        assert_eq!(
            serde_json::to_string(&auth).unwrap(),
            r#"{"kind":"none_configured"}"#,
            "an internally tagged enum must actually serialize (GOTCHAS #174)"
        );
    }

    #[test]
    fn an_auth_header_name_is_screened_like_every_other_label() {
        assert_eq!(
            Label::new(format!("authorization: Bearer {SECRET}")),
            Err(TelemetryFault::MalformedLabel),
            "a header name is a Label, so a whole header line cannot be one"
        );
        assert_eq!(Label::new(SECRET), Err(TelemetryFault::CredentialMaterial));
    }
}
