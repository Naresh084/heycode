//! Source-attributed catalog fixtures plus raw-SSE deterministic matrices.

use std::future::Future;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use heycode_http::{
    HttpService, HttpSseRequest, HttpTransport, SseDecoder, SseEvent, SseEventStream,
    TransportError,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Current reusable catalog-fixture document schema.
pub const CATALOG_CONFORMANCE_FIXTURE_SCHEMA_VERSION: u32 = 1;
/// Fixtures are developer/test artifacts, but parsing them is still a boundary.
const MAX_CATALOG_FIXTURE_BYTES: usize = 32 * 1024 * 1024;

/// Invalid fixture identity or raw bytes.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConformanceFixtureError {
    /// Case/fixture labels are stable diagnostic ids.
    #[error("invalid conformance fixture label `{label}`")]
    InvalidLabel {
        /// Rejected label.
        label: String,
    },
    /// Fragmentation generation requires at least one raw byte.
    #[error("conformance fixture `{fixture}` has no raw SSE bytes")]
    EmptyWire {
        /// Fixture label.
        fixture: String,
    },
    /// One required provenance field is absent or unsafe.
    #[error("invalid conformance fixture metadata field `{field}`")]
    InvalidMetadata {
        /// Stable field name; rejected values are never repeated.
        field: &'static str,
    },
    /// Catalog fixture JSON is malformed or has an invalid closed shape.
    #[error("invalid catalog conformance fixture document ({reason})")]
    InvalidDocument {
        /// Stable body-free reason.
        reason: &'static str,
    },
    /// A newer/older schema needs an explicit reader decision.
    #[error("unsupported catalog conformance fixture schema {found}")]
    UnsupportedSchema {
        /// Version found in the document.
        found: u32,
    },
    /// The fixture crossed its whole-document allocation bound.
    #[error("catalog conformance fixture exceeds the {max_bytes}-byte limit")]
    FixtureTooLarge {
        /// Maximum accepted bytes.
        max_bytes: usize,
    },
}

/// How fixture bytes relate to the source named in their metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConformanceSourceKind {
    /// Verbatim or minimally formatted bytes from an official example.
    OfficialExample,
    /// Credential/content-redacted bytes captured from the named endpoint.
    RedactedCapture,
    /// Constructed bytes following the named specification. This is not a
    /// claim that a provider emitted them.
    Synthetic,
}

impl ConformanceSourceKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::OfficialExample => "official_example",
            Self::RedactedCapture => "redacted_capture",
            Self::Synthetic => "synthetic",
        }
    }

    fn parse(value: &str) -> Result<Self, ConformanceFixtureError> {
        match value {
            "official_example" => Ok(Self::OfficialExample),
            "redacted_capture" => Ok(Self::RedactedCapture),
            "synthetic" => Ok(Self::Synthetic),
            _ => Err(ConformanceFixtureError::InvalidMetadata {
                field: "source_kind",
            }),
        }
    }
}

/// Required provenance shared by catalog and protocol fixtures.
///
/// `captured_at_ms` means the instant these fixture bytes/facts were recorded
/// or last reconciled with `source`. Synthetic fixtures retain that instant
/// while their source kind prevents it being misread as a provider capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceFixtureMetadata {
    provider: String,
    source_kind: ConformanceSourceKind,
    source: String,
    source_version: String,
    captured_at_ms: NonZeroU64,
}

impl ConformanceFixtureMetadata {
    /// Validate one complete metadata record.
    ///
    /// # Errors
    /// Provider/version ids must be bounded safe tokens; source must be an
    /// absolute credential-free HTTPS URL without query or fragment; capture
    /// time must be non-zero.
    pub fn new(
        provider: impl Into<String>,
        source_kind: ConformanceSourceKind,
        source: impl AsRef<str>,
        source_version: impl Into<String>,
        captured_at_ms: u64,
    ) -> Result<Self, ConformanceFixtureError> {
        let provider = provider.into();
        if !valid_provider_id(&provider) {
            return Err(ConformanceFixtureError::InvalidMetadata { field: "provider" });
        }
        let source = reqwest::Url::parse(source.as_ref())
            .map_err(|_| ConformanceFixtureError::InvalidMetadata { field: "source" })?;
        if source.scheme() != "https"
            || source.host_str().is_none()
            || !source.username().is_empty()
            || source.password().is_some()
            || source.query().is_some()
            || source.fragment().is_some()
        {
            return Err(ConformanceFixtureError::InvalidMetadata { field: "source" });
        }
        let source_version = source_version.into();
        if !valid_metadata_token(&source_version) {
            return Err(ConformanceFixtureError::InvalidMetadata {
                field: "source_version",
            });
        }
        let captured_at_ms =
            NonZeroU64::new(captured_at_ms).ok_or(ConformanceFixtureError::InvalidMetadata {
                field: "captured_at_ms",
            })?;
        Ok(Self {
            provider,
            source_kind,
            source: source.to_string(),
            source_version,
            captured_at_ms,
        })
    }

    /// Provider whose catalog/protocol the fixture exercises.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Relationship between fixture bytes and their source.
    #[must_use]
    pub const fn source_kind(&self) -> ConformanceSourceKind {
        self.source_kind
    }

    /// Exact primary-source URL, normalized by the URL parser.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// API/specification version or reviewed source revision.
    #[must_use]
    pub fn source_version(&self) -> &str {
        &self.source_version
    }

    /// Non-zero Unix-millisecond capture/reconciliation instant.
    #[must_use]
    pub const fn captured_at_ms(&self) -> u64 {
        self.captured_at_ms.get()
    }
}

/// One schema-v1 provider catalog fixture with mandatory source provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogConformanceFixture {
    name: String,
    metadata: ConformanceFixtureMetadata,
    payload: serde_json::Value,
}

impl CatalogConformanceFixture {
    /// Build one in-memory fixture using the same validation as the wire
    /// reader.
    ///
    /// # Errors
    /// Invalid fixture label or a non-object/non-array catalog payload.
    pub fn new(
        name: impl Into<String>,
        metadata: ConformanceFixtureMetadata,
        payload: serde_json::Value,
    ) -> Result<Self, ConformanceFixtureError> {
        let name = valid_label(name.into())?;
        if !payload.is_object() && !payload.is_array() {
            return Err(ConformanceFixtureError::InvalidDocument {
                reason: "payload must be a JSON object or array",
            });
        }
        Ok(Self {
            name,
            metadata,
            payload,
        })
    }

    /// Parse one strict schema-v1 fixture document.
    ///
    /// # Errors
    /// Oversize/malformed JSON, unknown fields, unsupported schema, invalid
    /// provenance or invalid payload shape.
    pub fn from_json(bytes: &[u8]) -> Result<Self, ConformanceFixtureError> {
        if bytes.len() > MAX_CATALOG_FIXTURE_BYTES {
            return Err(ConformanceFixtureError::FixtureTooLarge {
                max_bytes: MAX_CATALOG_FIXTURE_BYTES,
            });
        }
        let probe: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| {
            ConformanceFixtureError::InvalidDocument {
                reason: "JSON is malformed",
            }
        })?;
        let found = probe
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or(ConformanceFixtureError::InvalidDocument {
                reason: "schema_version is missing or invalid",
            })?;
        if found != CATALOG_CONFORMANCE_FIXTURE_SCHEMA_VERSION {
            return Err(ConformanceFixtureError::UnsupportedSchema { found });
        }
        let wire: CatalogFixtureWire = serde_json::from_value(probe).map_err(|_| {
            ConformanceFixtureError::InvalidDocument {
                reason: "document shape is invalid",
            }
        })?;
        if wire.schema_version != CATALOG_CONFORMANCE_FIXTURE_SCHEMA_VERSION {
            return Err(ConformanceFixtureError::UnsupportedSchema {
                found: wire.schema_version,
            });
        }
        let source_kind = ConformanceSourceKind::parse(&wire.source_kind)?;
        let metadata = ConformanceFixtureMetadata::new(
            wire.provider,
            source_kind,
            wire.source,
            wire.source_version,
            wire.captured_at_ms,
        )?;
        Self::new(wire.name, metadata, wire.payload)
    }

    /// Serialize one deterministic schema-v1 document.
    ///
    /// # Errors
    /// JSON serialization failure.
    pub fn to_json(&self) -> Result<Vec<u8>, ConformanceFixtureError> {
        serde_json::to_vec(&CatalogFixtureWireRef {
            schema_version: CATALOG_CONFORMANCE_FIXTURE_SCHEMA_VERSION,
            name: &self.name,
            provider: self.metadata.provider(),
            source_kind: self.metadata.source_kind().as_str(),
            source: self.metadata.source(),
            source_version: self.metadata.source_version(),
            captured_at_ms: self.metadata.captured_at_ms(),
            payload: &self.payload,
        })
        .map_err(|_| ConformanceFixtureError::InvalidDocument {
            reason: "document could not be serialized",
        })
    }

    /// Stable fixture id.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Mandatory source/version/capture metadata.
    #[must_use]
    pub const fn metadata(&self) -> &ConformanceFixtureMetadata {
        &self.metadata
    }

    /// Provider response payload.
    #[must_use]
    pub const fn payload(&self) -> &serde_json::Value {
        &self.payload
    }

    /// Consume the envelope and return the provider response payload.
    #[must_use]
    pub fn into_payload(self) -> serde_json::Value {
        self.payload
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogFixtureWire {
    schema_version: u32,
    name: String,
    provider: String,
    source_kind: String,
    source: String,
    source_version: String,
    captured_at_ms: u64,
    payload: serde_json::Value,
}

#[derive(serde::Serialize)]
struct CatalogFixtureWireRef<'fixture> {
    schema_version: u32,
    name: &'fixture str,
    provider: &'fixture str,
    source_kind: &'fixture str,
    source: &'fixture str,
    source_version: &'fixture str,
    captured_at_ms: u64,
    payload: &'fixture serde_json::Value,
}

/// One named raw-SSE fixture capable of generating network fragmentations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseConformanceFixture {
    name: String,
    metadata: ConformanceFixtureMetadata,
    wire: Vec<u8>,
}

impl SseConformanceFixture {
    /// Build one attributed provider SSE response body.
    ///
    /// # Errors
    /// Blank/non-trimmed/overlong labels or an empty body. Metadata has already
    /// crossed its validating constructor.
    pub fn new(
        name: impl Into<String>,
        metadata: ConformanceFixtureMetadata,
        wire: impl AsRef<[u8]>,
    ) -> Result<Self, ConformanceFixtureError> {
        let name = valid_label(name.into())?;
        let wire = wire.as_ref().to_vec();
        if wire.is_empty() {
            return Err(ConformanceFixtureError::EmptyWire { fixture: name });
        }
        Ok(Self {
            name,
            metadata,
            wire,
        })
    }

    /// Mandatory source/version/capture metadata.
    #[must_use]
    pub const fn metadata(&self) -> &ConformanceFixtureMetadata {
        &self.metadata
    }

    /// Generate whole-body, bytewise, and every two-fragment split case in a
    /// stable order. For `N` bytes this returns `N + 1` cases.
    #[must_use]
    pub fn fragmentation_cases(&self) -> Vec<SseFixtureCase> {
        let mut cases = Vec::with_capacity(self.wire.len().saturating_add(1));
        cases.push(SseFixtureCase {
            name: format!("{}/whole", self.name),
            metadata: self.metadata.clone(),
            chunks: vec![self.wire.clone()],
            terminal_error: None,
        });
        cases.push(SseFixtureCase {
            name: format!("{}/bytewise", self.name),
            metadata: self.metadata.clone(),
            chunks: self.wire.iter().map(|byte| vec![*byte]).collect(),
            terminal_error: None,
        });
        for split in 1..self.wire.len() {
            cases.push(SseFixtureCase {
                name: format!("{}/split-{split}", self.name),
                metadata: self.metadata.clone(),
                chunks: vec![self.wire[..split].to_vec(), self.wire[split..].to_vec()],
                terminal_error: None,
            });
        }
        cases
    }
}

/// One explicit raw network-chunk script with an optional terminal transport
/// failure. A failure suppresses clean EOF flushing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFixtureCase {
    name: String,
    metadata: ConformanceFixtureMetadata,
    chunks: Vec<Vec<u8>>,
    terminal_error: Option<TransportError>,
}

impl SseFixtureCase {
    /// Build an explicit fragmentation/failure case.
    ///
    /// # Errors
    /// Blank/non-trimmed/overlong case labels. Metadata has already crossed
    /// its validating constructor.
    pub fn new<I, B>(
        name: impl Into<String>,
        metadata: ConformanceFixtureMetadata,
        chunks: I,
    ) -> Result<Self, ConformanceFixtureError>
    where
        I: IntoIterator<Item = B>,
        B: AsRef<[u8]>,
    {
        Ok(Self {
            name: valid_label(name.into())?,
            metadata,
            chunks: chunks
                .into_iter()
                .map(|chunk| chunk.as_ref().to_vec())
                .collect(),
            terminal_error: None,
        })
    }

    /// Inject a terminal transport failure after the declared chunks. The
    /// decoder does not treat a failed connection as clean EOF.
    #[must_use]
    pub fn with_terminal_error(mut self, error: TransportError) -> Self {
        self.terminal_error = Some(error);
        self
    }

    /// Stable case label.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Mandatory source/version/capture metadata.
    #[must_use]
    pub const fn metadata(&self) -> &ConformanceFixtureMetadata {
        &self.metadata
    }
}

/// Decoder-backed transport for one fixture case.
pub struct SseFixtureTransport {
    case: SseFixtureCase,
    calls: AtomicUsize,
}

impl SseFixtureTransport {
    /// Bind one raw chunk/failure case.
    #[must_use]
    pub fn new(case: SseFixtureCase) -> Self {
        Self {
            case,
            calls: AtomicUsize::new(0),
        }
    }

    /// Number of SSE operations requested by the adapter.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl HttpTransport for SseFixtureTransport {
    fn sse(&self, _request: HttpSseRequest, cancellation: CancellationToken) -> SseEventStream {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if cancellation.is_cancelled() {
            return Box::pin(futures::stream::once(async {
                Err(TransportError::Cancelled)
            }));
        }
        Box::pin(futures::stream::iter(decode_case(&self.case)))
    }
}

/// One case label paired with the adapter-specific runner output.
#[derive(Debug, Clone, PartialEq)]
pub struct ConformanceRun<T> {
    /// Stable input case label.
    pub case: String,
    /// Source/version/capture metadata copied from the exact case.
    pub metadata: ConformanceFixtureMetadata,
    /// Adapter/test-owned normalized result.
    pub output: T,
    /// Number of transport calls made for this isolated case.
    pub transport_calls: usize,
}

/// Run every case with a fresh decoder-backed HTTP service in declaration
/// order. The closure builds/runs whichever protocol/provider adapter is under
/// test, making the same matrix reusable across adapters.
pub async fn run_sse_conformance<T, F, Fut>(
    cases: &[SseFixtureCase],
    mut run: F,
) -> Vec<ConformanceRun<T>>
where
    F: FnMut(HttpService) -> Fut,
    Fut: Future<Output = T>,
{
    let mut output = Vec::with_capacity(cases.len());
    for case in cases {
        let transport = Arc::new(SseFixtureTransport::new(case.clone()));
        let result = run(HttpService::new(transport.clone())).await;
        output.push(ConformanceRun {
            case: case.name.clone(),
            metadata: case.metadata.clone(),
            output: result,
            transport_calls: transport.calls(),
        });
    }
    output
}

fn decode_case(case: &SseFixtureCase) -> Vec<Result<SseEvent, TransportError>> {
    let mut decoder = SseDecoder::new();
    let mut output = Vec::new();
    for chunk in &case.chunks {
        match decoder.feed(chunk) {
            Ok(events) => output.extend(events.into_iter().map(Ok)),
            Err(error) => {
                output.push(Err(TransportError::InvalidSse {
                    message: error.to_string(),
                }));
                return output;
            }
        }
    }
    if let Some(error) = &case.terminal_error {
        output.push(Err(error.clone()));
        return output;
    }
    match decoder.finish() {
        Ok(events) => output.extend(events.into_iter().map(Ok)),
        Err(error) => output.push(Err(TransportError::InvalidSse {
            message: error.to_string(),
        })),
    }
    output
}

fn valid_label(label: String) -> Result<String, ConformanceFixtureError> {
    if label.is_empty()
        || label.len() > 256
        || !label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
    {
        Err(ConformanceFixtureError::InvalidLabel { label })
    } else {
        Ok(label)
    }
}

fn valid_provider_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value == value.trim()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

fn valid_metadata_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value == value.trim()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
}
