//! Provider-independent public web search/fetch service and registry.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};

use async_trait::async_trait;
use futures::FutureExt as _;
use heycode_core::{Context, CoreError, CoreResult, Plugin};
use tokio_util::sync::CancellationToken;

mod browser;
mod extraction;
mod policy;
mod portable;
pub use browser::{BrowserHttpRequest, BrowserHttpResponse, BrowserLocalOrigin};

pub use extraction::{
    DocumentExtraction, DocumentExtractionInput, DocumentExtractor, web_extract_plugin,
};
pub use policy::{
    WebDomainPolicy, WebPolicy, WebPolicyError, web_policy_namespace, web_policy_plugin,
};
pub use portable::{
    PortableWebConfig, ip_is_private_for_tests, parse_ddg_lite, portable_web_plugin,
    strip_html_for_tests,
};

/// Public web registry service.
pub const SERVICE_WEB: heycode_core::ServiceKey = heycode_core::ServiceKey::new("web");
/// Bounded local document extraction service provided by `web-extract`.
pub const SERVICE_DOCUMENT_EXTRACTOR: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("document-extractor");

const MAX_QUERY_BYTES: usize = 4_096;
const MAX_RESULTS: u8 = 32;
const MAX_FETCH_BYTES: u32 = 4 * 1024 * 1024;
const MAX_TITLE_BYTES: usize = 512;
const MAX_SNIPPET_BYTES: usize = 16 * 1024;
const MAX_FETCH_CONTENT_BYTES: usize = 4 * 1024 * 1024;

/// Validated provider descriptor and operation evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebProviderDescriptor {
    id: String,
    search: bool,
    fetch: bool,
}

impl WebProviderDescriptor {
    /// Construct one provider descriptor.
    ///
    /// # Errors
    /// Unsafe ids or a provider supporting no operation fail.
    pub fn new(id: impl Into<String>, search: bool, fetch: bool) -> Result<Self, WebError> {
        let descriptor = Self {
            id: id.into(),
            search,
            fetch,
        };
        if !safe_id(&descriptor.id) || (!search && !fetch) {
            return Err(WebError::invalid_request());
        }
        Ok(descriptor)
    }

    /// Stable provider id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Whether search is implemented.
    #[must_use]
    pub const fn supports_search(&self) -> bool {
        self.search
    }

    /// Whether fetch is implemented.
    #[must_use]
    pub const fn supports_fetch(&self) -> bool {
        self.fetch
    }
}

/// One bounded web-search request.
#[derive(Clone, PartialEq, Eq)]
pub struct WebSearchRequest {
    query: String,
    max_results: u8,
    domain_policy: WebDomainPolicy,
}

impl std::fmt::Debug for WebSearchRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebSearchRequest")
            .field("query", &"[REDACTED]")
            .field("max_results", &self.max_results)
            .finish()
    }
}

impl WebSearchRequest {
    /// Construct one search request.
    ///
    /// # Errors
    /// Blank/control-bearing/oversized query or result cap outside 1..=32.
    pub fn new(query: impl Into<String>, max_results: u8) -> Result<Self, WebError> {
        let query = query.into();
        if query.trim().is_empty()
            || query.len() > MAX_QUERY_BYTES
            || query
                .chars()
                .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
            || !(1..=MAX_RESULTS).contains(&max_results)
        {
            return Err(WebError::invalid_request());
        }
        Ok(Self {
            query,
            max_results,
            domain_policy: WebDomainPolicy::default(),
        })
    }

    /// Search query.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Maximum accepted result count.
    #[must_use]
    pub const fn max_results(&self) -> u8 {
        self.max_results
    }

    /// Effective search-result domain policy selected by the registry.
    #[must_use]
    pub fn domain_policy(&self) -> &WebDomainPolicy {
        &self.domain_policy
    }

    fn with_domain_policy(mut self, domain_policy: WebDomainPolicy) -> Self {
        self.domain_policy = domain_policy;
        self
    }
}

/// One bounded public search result.
#[derive(Clone, PartialEq, Eq)]
pub struct WebSearchResult {
    title: String,
    url: String,
    snippet: String,
}

impl WebSearchResult {
    /// Construct one validated result.
    ///
    /// # Errors
    /// Unsafe public URL or unbounded/control-bearing display text fails.
    pub fn new(
        title: impl Into<String>,
        url: impl Into<String>,
        snippet: impl Into<String>,
    ) -> Result<Self, WebError> {
        let result = Self {
            title: title.into(),
            url: url.into(),
            snippet: snippet.into(),
        };
        result.validate()?;
        Ok(result)
    }

    fn validate(&self) -> Result<(), WebError> {
        validate_single_line(&self.title, MAX_TITLE_BYTES)?;
        validate_public_url(&self.url)?;
        validate_multiline(&self.snippet, MAX_SNIPPET_BYTES)
    }

    /// Display title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Public result URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Bounded result snippet.
    #[must_use]
    pub fn snippet(&self) -> &str {
        &self.snippet
    }
}

impl std::fmt::Debug for WebSearchResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebSearchResult")
            .field("title_bytes", &self.title.len())
            .field("url", &"[REDACTED]")
            .field("snippet_bytes", &self.snippet.len())
            .finish()
    }
}

/// One bounded public page request.
#[derive(Clone, PartialEq, Eq)]
pub struct WebFetchRequest {
    url: String,
    max_source_bytes: u32,
    max_bytes: u32,
    domain_policy: WebDomainPolicy,
}

impl std::fmt::Debug for WebFetchRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebFetchRequest")
            .field("url", &"[REDACTED]")
            .field("max_source_bytes", &self.max_source_bytes)
            .field("max_bytes", &self.max_bytes)
            .finish()
    }
}

impl WebFetchRequest {
    /// Construct one fetch request.
    ///
    /// # Errors
    /// URL must be public-shaped HTTP(S) without userinfo; cap is 1..=4 MiB.
    pub fn new(url: impl Into<String>, max_bytes: u32) -> Result<Self, WebError> {
        Self::with_limits(url, max_bytes, max_bytes)
    }

    /// Construct with independent raw-source and readable-output ceilings.
    ///
    /// # Errors
    /// URL must be public-shaped HTTP(S) without userinfo; both caps are
    /// 1..=4 MiB and output cannot exceed the source cap.
    pub fn with_limits(
        url: impl Into<String>,
        max_source_bytes: u32,
        max_bytes: u32,
    ) -> Result<Self, WebError> {
        let url = url.into();
        validate_public_url(&url)?;
        if !(1..=MAX_FETCH_BYTES).contains(&max_source_bytes)
            || !(1..=MAX_FETCH_BYTES).contains(&max_bytes)
            || max_bytes > max_source_bytes
        {
            return Err(WebError::invalid_request());
        }
        Ok(Self {
            url,
            max_source_bytes,
            max_bytes,
            domain_policy: WebDomainPolicy::default(),
        })
    }

    /// Requested public URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Maximum readable output bytes after extraction.
    #[must_use]
    pub const fn max_bytes(&self) -> u32 {
        self.max_bytes
    }

    /// Maximum retained raw source bytes before extraction.
    #[must_use]
    pub const fn max_source_bytes(&self) -> u32 {
        self.max_source_bytes
    }

    /// Effective request/redirect/final-URL policy selected by the registry.
    #[must_use]
    pub fn domain_policy(&self) -> &WebDomainPolicy {
        &self.domain_policy
    }

    fn with_domain_policy(mut self, domain_policy: WebDomainPolicy) -> Self {
        self.domain_policy = domain_policy;
        self
    }
}

/// Citeable provenance for one fetched source.
#[derive(Clone, PartialEq, Eq)]
pub struct WebFetchSource {
    url: String,
    title: Option<String>,
    retrieved_at_ms: Option<i64>,
    source_truncated: bool,
    content_id: Option<heycode_core::AttachmentContentId>,
    page_count: Option<u32>,
}

impl WebFetchSource {
    /// Construct one bounded source record.
    ///
    /// # Errors
    /// Unsafe URL/title, negative time or page count outside 1..=10,000 fails.
    pub fn new(
        url: impl Into<String>,
        title: Option<&str>,
        retrieved_at_ms: Option<i64>,
        source_truncated: bool,
        content_id: Option<heycode_core::AttachmentContentId>,
        page_count: Option<u32>,
    ) -> Result<Self, WebError> {
        let source = Self {
            url: url.into(),
            title: title.map(str::to_owned),
            retrieved_at_ms,
            source_truncated,
            content_id,
            page_count,
        };
        source.validate()?;
        Ok(source)
    }

    fn validate(&self) -> Result<(), WebError> {
        validate_public_url(&self.url)?;
        if let Some(title) = &self.title {
            validate_single_line(title, MAX_TITLE_BYTES)?;
        }
        if self.retrieved_at_ms.is_some_and(|time| time < 0)
            || self
                .page_count
                .is_some_and(|pages| !(1..=10_000).contains(&pages))
        {
            return Err(WebError::invalid_response());
        }
        Ok(())
    }

    /// Final public source URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Extracted document title, when present.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Local retrieval instant in Unix milliseconds.
    #[must_use]
    pub const fn retrieved_at_ms(&self) -> Option<i64> {
        self.retrieved_at_ms
    }

    /// Whether transport discarded raw source bytes before capture.
    #[must_use]
    pub const fn source_truncated(&self) -> bool {
        self.source_truncated
    }

    /// Durable raw-source content address, when captured.
    #[must_use]
    pub fn content_id(&self) -> Option<&heycode_core::AttachmentContentId> {
        self.content_id.as_ref()
    }

    /// PDF page count, when known.
    #[must_use]
    pub const fn page_count(&self) -> Option<u32> {
        self.page_count
    }
}

impl std::fmt::Debug for WebFetchSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebFetchSource")
            .field("url", &"[REDACTED]")
            .field("has_title", &self.title.is_some())
            .field("retrieved_at_ms", &self.retrieved_at_ms)
            .field("source_truncated", &self.source_truncated)
            .field("has_content_id", &self.content_id.is_some())
            .field("page_count", &self.page_count)
            .finish()
    }
}

/// One normalized page fetch result.
#[derive(Clone, PartialEq, Eq)]
pub struct WebFetchResult {
    source: WebFetchSource,
    content: String,
    content_type: Option<String>,
    truncated: bool,
}

impl WebFetchResult {
    /// Construct one normalized fetch result.
    ///
    /// # Errors
    /// Final URL, content/type bounds or terminal controls fail.
    pub fn new(
        final_url: impl Into<String>,
        content: impl Into<String>,
        content_type: Option<&str>,
        truncated: bool,
    ) -> Result<Self, WebError> {
        let result = Self {
            source: WebFetchSource::new(final_url, None, None, false, None, None)?,
            content: content.into(),
            content_type: content_type.map(str::to_owned),
            truncated,
        };
        result.validate()?;
        Ok(result)
    }

    /// Construct one extracted result with complete source provenance.
    ///
    /// # Errors
    /// Source/content/type bounds or terminal controls fail.
    pub fn with_source(
        source: WebFetchSource,
        content: impl Into<String>,
        content_type: Option<&str>,
        truncated: bool,
    ) -> Result<Self, WebError> {
        let result = Self {
            source,
            content: content.into(),
            content_type: content_type.map(str::to_owned),
            truncated,
        };
        result.validate()?;
        Ok(result)
    }

    fn validate(&self) -> Result<(), WebError> {
        self.source.validate()?;
        validate_multiline(&self.content, MAX_FETCH_CONTENT_BYTES)?;
        if let Some(content_type) = &self.content_type {
            validate_single_line(content_type, 256)?;
        }
        Ok(())
    }

    /// Final public URL after provider redirects.
    #[must_use]
    pub fn final_url(&self) -> &str {
        self.source.url()
    }

    /// Structured citeable source metadata.
    #[must_use]
    pub fn source(&self) -> &WebFetchSource {
        &self.source
    }

    /// Normalized readable content.
    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Safe response content type, when known.
    #[must_use]
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    /// Whether raw source or readable output was truncated.
    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }
}

impl std::fmt::Debug for WebFetchResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebFetchResult")
            .field("source", &self.source)
            .field("content_bytes", &self.content.len())
            .field("content_type", &self.content_type)
            .field("truncated", &self.truncated)
            .finish()
    }
}

/// Bounded raw HTTP document offered to extraction processors.
#[derive(Clone, PartialEq, Eq)]
pub struct WebRawDocument {
    final_url: String,
    content_type: Option<String>,
    bytes: Vec<u8>,
    source_truncated: bool,
    max_output_bytes: usize,
}

impl WebRawDocument {
    /// Construct one processor input.
    ///
    /// # Errors
    /// Unsafe URL/type, empty/>4 MiB bytes or output cap outside 1..=4 MiB.
    pub fn new(
        final_url: impl Into<String>,
        content_type: Option<&str>,
        bytes: Vec<u8>,
        source_truncated: bool,
        max_output_bytes: usize,
    ) -> Result<Self, WebError> {
        let final_url = final_url.into();
        validate_public_url(&final_url)?;
        let maximum = usize::try_from(MAX_FETCH_BYTES).unwrap_or(usize::MAX);
        if bytes.is_empty() || bytes.len() > maximum || !(1..=maximum).contains(&max_output_bytes) {
            return Err(WebError::invalid_request());
        }
        let content_type = content_type
            .map(|value| {
                value
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase()
            })
            .filter(|value| !value.is_empty());
        if let Some(content_type) = &content_type {
            heycode_core::AttachmentMediaType::new(content_type.clone())
                .map_err(|_| WebError::invalid_request())?;
        }
        Ok(Self {
            final_url,
            content_type,
            bytes,
            source_truncated,
            max_output_bytes,
        })
    }

    /// Final public source URL.
    #[must_use]
    pub fn final_url(&self) -> &str {
        &self.final_url
    }

    /// Canonical response media type without parameters.
    #[must_use]
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    /// Exact retained raw bytes. Processors only; Debug is redacted.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Whether transport discarded raw bytes.
    #[must_use]
    pub const fn source_truncated(&self) -> bool {
        self.source_truncated
    }

    /// Maximum readable UTF-8 output bytes.
    #[must_use]
    pub const fn max_output_bytes(&self) -> usize {
        self.max_output_bytes
    }
}

impl std::fmt::Debug for WebRawDocument {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebRawDocument")
            .field("final_url", &"[REDACTED]")
            .field("content_type", &self.content_type)
            .field("retained_bytes", &self.bytes.len())
            .field("source_truncated", &self.source_truncated)
            .field("max_output_bytes", &self.max_output_bytes)
            .finish()
    }
}

/// Immutable content-processor descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebContentProcessorDescriptor {
    id: String,
}

impl WebContentProcessorDescriptor {
    /// Construct one safe processor identity.
    ///
    /// # Errors
    /// Id must be lowercase kebab-case.
    pub fn new(id: impl Into<String>) -> Result<Self, WebError> {
        let id = id.into();
        if !safe_id(&id) {
            return Err(WebError::invalid_request());
        }
        Ok(Self { id })
    }

    /// Stable processor id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// Provider contract for bounded source-to-readable extraction.
#[async_trait]
pub trait WebContentProcessor: Send + Sync {
    /// Immutable processor identity.
    fn descriptor(&self) -> WebContentProcessorDescriptor;

    /// Cheap deterministic content support check.
    fn supports(&self, document: &WebRawDocument) -> bool;

    /// Produce one readable result with source provenance.
    ///
    /// # Errors
    /// Invalid/malformed content, cancellation, storage or extraction failure.
    async fn process(
        &self,
        document: &WebRawDocument,
        cancellation: CancellationToken,
    ) -> Result<WebFetchResult, WebError>;
}

/// Provider contract behind the public web service.
#[async_trait]
pub trait WebProvider: Send + Sync {
    /// Immutable provider identity/capability facts.
    fn descriptor(&self) -> WebProviderDescriptor;

    /// Cheap local availability check used during operation-time selection.
    ///
    /// Providers must not perform network IO or mutate state here.
    fn available(&self) -> bool {
        true
    }

    /// Search the public web.
    ///
    /// # Errors
    /// Return stable body-free [`WebError`] classes.
    async fn search(
        &self,
        _request: WebSearchRequest,
        _cancellation: CancellationToken,
    ) -> Result<Vec<WebSearchResult>, WebError> {
        Err(WebError::unsupported())
    }

    /// Fetch one public page.
    ///
    /// # Errors
    /// Return stable body-free [`WebError`] classes.
    async fn fetch(
        &self,
        _request: WebFetchRequest,
        _cancellation: CancellationToken,
    ) -> Result<WebFetchResult, WebError> {
        Err(WebError::unsupported())
    }
}

struct Entry {
    descriptor: WebProviderDescriptor,
    provider: Arc<dyn WebProvider>,
    token: Arc<()>,
}

struct ProcessorEntry {
    descriptor: WebContentProcessorDescriptor,
    processor: Arc<dyn WebContentProcessor>,
    token: Arc<()>,
}

struct RegistryInner {
    entries: Mutex<Vec<Entry>>,
    processors: Mutex<Vec<ProcessorEntry>>,
    policy: RwLock<WebPolicy>,
    policy_unavailable: AtomicBool,
    closed: AtomicBool,
}

impl Default for RegistryInner {
    fn default() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            processors: Mutex::new(Vec::new()),
            policy: RwLock::new(WebPolicy::default()),
            policy_unavailable: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        }
    }
}

/// Effect-owned registry and dispatch service for public web operations.
#[derive(Clone, Default)]
pub struct WebRegistry {
    inner: Arc<RegistryInner>,
}

impl WebRegistry {
    /// Empty live registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one unique provider until Context disposal.
    ///
    /// # Errors
    /// Invalid descriptor, duplicate id, stopped or poisoned state fails.
    pub fn register(
        &self,
        context: &Context,
        provider: Arc<dyn WebProvider>,
    ) -> Result<(), WebError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(WebError::stopped());
        }
        let descriptor = provider.descriptor();
        WebProviderDescriptor::new(descriptor.id.clone(), descriptor.search, descriptor.fetch)?;
        let mut entries = self
            .inner
            .entries
            .lock()
            .map_err(|_| WebError::unavailable())?;
        if entries
            .iter()
            .any(|entry| entry.descriptor.id == descriptor.id)
        {
            return Err(WebError::duplicate());
        }
        let token = Arc::new(());
        entries.push(Entry {
            descriptor: descriptor.clone(),
            provider,
            token: token.clone(),
        });
        drop(entries);
        let registration = ProviderRegistration {
            inner: Arc::downgrade(&self.inner),
            id: descriptor.id,
            token,
        };
        context.effect(move || drop(registration));
        Ok(())
    }

    /// Sorted provider descriptors.
    ///
    /// # Errors
    /// Poisoned state fails.
    pub fn descriptors(&self) -> Result<Vec<WebProviderDescriptor>, WebError> {
        let entries = self
            .inner
            .entries
            .lock()
            .map_err(|_| WebError::unavailable())?;
        let mut descriptors = entries
            .iter()
            .map(|entry| entry.descriptor.clone())
            .collect::<Vec<_>>();
        descriptors.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(descriptors)
    }

    /// Register one unique source-content processor as a Context effect.
    ///
    /// # Errors
    /// Invalid/duplicate id, stopped or poisoned state fails.
    pub fn register_processor(
        &self,
        context: &Context,
        processor: Arc<dyn WebContentProcessor>,
    ) -> Result<(), WebError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(WebError::stopped());
        }
        let descriptor = processor.descriptor();
        WebContentProcessorDescriptor::new(descriptor.id.clone())?;
        let mut processors = self
            .inner
            .processors
            .lock()
            .map_err(|_| WebError::unavailable())?;
        if processors
            .iter()
            .any(|entry| entry.descriptor.id == descriptor.id)
        {
            return Err(WebError::duplicate());
        }
        let token = Arc::new(());
        processors.push(ProcessorEntry {
            descriptor: descriptor.clone(),
            processor,
            token: token.clone(),
        });
        drop(processors);
        let registration = ProcessorRegistration {
            inner: Arc::downgrade(&self.inner),
            id: descriptor.id,
            token,
        };
        context.effect(move || drop(registration));
        Ok(())
    }

    /// Sorted source-content processor descriptors.
    ///
    /// # Errors
    /// Poisoned state fails.
    pub fn processor_descriptors(&self) -> Result<Vec<WebContentProcessorDescriptor>, WebError> {
        let processors = self
            .inner
            .processors
            .lock()
            .map_err(|_| WebError::unavailable())?;
        let mut descriptors = processors
            .iter()
            .map(|entry| entry.descriptor.clone())
            .collect::<Vec<_>>();
        descriptors.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(descriptors)
    }

    /// Safe provider/selection/domain snapshot for status and settings UIs.
    ///
    /// # Errors
    /// Stopped, poisoned or unavailable policy state fails.
    pub fn capability_report(&self) -> Result<WebCapabilityReport, WebError> {
        let policy = self.policy_snapshot()?;
        let providers = self.provider_snapshots()?;
        Ok(WebCapabilityReport {
            processors: self.processor_descriptors()?,
            providers: providers
                .iter()
                .map(|provider| WebProviderStatus {
                    descriptor: provider.descriptor.clone(),
                    available: provider.available,
                })
                .collect(),
            search: select_provider(WebOperation::Search, policy.search_provider(), &providers),
            fetch: select_provider(WebOperation::Fetch, policy.fetch_provider(), &providers),
            search_domains: policy.search_domains().clone(),
            fetch_domains: policy.fetch_domains().clone(),
        })
    }

    /// Process one bounded raw source through the unique matching processor.
    ///
    /// `None` means no composed processor supports the document, allowing a
    /// provider to retain its own conservative fallback.
    ///
    /// # Errors
    /// Stopped/poisoned/ambiguous processor state, cancellation, extraction or
    /// invalid processor output.
    pub async fn extract(
        &self,
        document: WebRawDocument,
        cancellation: CancellationToken,
    ) -> Result<Option<WebFetchResult>, WebError> {
        extract_with_inner(&self.inner, &document, cancellation).await
    }

    pub(crate) fn processor_handle(&self) -> WebProcessorHandle {
        WebProcessorHandle {
            inner: Arc::downgrade(&self.inner),
        }
    }

    /// Dispatch one validated search to the configured or unique provider.
    ///
    /// # Errors
    /// Stopped/unavailable provider, cancellation or invalid provider output.
    pub async fn search(
        &self,
        request: WebSearchRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<WebSearchResult>, WebError> {
        if cancellation.is_cancelled() {
            return Err(WebError::cancelled());
        }
        let (provider, policy) = self.resolve(WebOperation::Search)?;
        let request = request.with_domain_policy(policy.search_domains().clone());
        let results = provider
            .search(request.clone(), cancellation.clone())
            .await?;
        if cancellation.is_cancelled() {
            return Err(WebError::cancelled());
        }
        if results.len() > usize::from(request.max_results()) {
            return Err(WebError::invalid_response());
        }
        for result in &results {
            result
                .validate()
                .map_err(|_| WebError::invalid_response())?;
        }
        Ok(results
            .into_iter()
            .filter(|result| policy.search_domains().allows(result.url()))
            .collect())
    }

    /// Dispatch one validated fetch to the configured or unique provider.
    ///
    /// # Errors
    /// Stopped/unavailable provider, cancellation or invalid provider output.
    pub async fn fetch(
        &self,
        request: WebFetchRequest,
        cancellation: CancellationToken,
    ) -> Result<WebFetchResult, WebError> {
        if cancellation.is_cancelled() {
            return Err(WebError::cancelled());
        }
        let (provider, policy) = self.resolve(WebOperation::Fetch)?;
        if !policy.fetch_domains().allows(request.url()) {
            return Err(WebError::policy_denied());
        }
        let request = request.with_domain_policy(policy.fetch_domains().clone());
        let result = provider
            .fetch(request.clone(), cancellation.clone())
            .await?;
        if cancellation.is_cancelled() {
            return Err(WebError::cancelled());
        }
        result
            .validate()
            .map_err(|_| WebError::invalid_response())?;
        if !policy.fetch_domains().allows(result.final_url()) {
            return Err(WebError::policy_denied());
        }
        if result.content.len() > usize::try_from(request.max_bytes()).unwrap_or(usize::MAX) {
            return Err(WebError::invalid_response());
        }
        Ok(result)
    }

    fn resolve(
        &self,
        operation: WebOperation,
    ) -> Result<(Arc<dyn WebProvider>, WebPolicy), WebError> {
        let policy = self.policy_snapshot()?;
        let providers = self.provider_snapshots()?;
        let configured = match operation {
            WebOperation::Search => policy.search_provider(),
            WebOperation::Fetch => policy.fetch_provider(),
        };
        let selection = select_provider(operation, configured, &providers);
        let provider_id = match selection {
            WebProviderSelection::Selected { provider, .. } => provider,
            WebProviderSelection::Ambiguous { .. } => return Err(WebError::ambiguous()),
            WebProviderSelection::Unavailable => return Err(WebError::unsupported()),
            WebProviderSelection::ConfiguredMissing { .. }
            | WebProviderSelection::ConfiguredUnsupported { .. }
            | WebProviderSelection::ConfiguredUnavailable { .. } => {
                return Err(WebError::unavailable());
            }
        };
        providers
            .into_iter()
            .find(|provider| provider.descriptor.id == provider_id)
            .map(|provider| (provider.provider, policy))
            .ok_or_else(WebError::unavailable)
    }

    fn policy_snapshot(&self) -> Result<WebPolicy, WebError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(WebError::stopped());
        }
        if self.inner.policy_unavailable.load(Ordering::Acquire) {
            return Err(WebError::unavailable());
        }
        self.inner
            .policy
            .read()
            .map_err(|_| WebError::unavailable())
            .map(|policy| policy.clone())
    }

    fn provider_snapshots(&self) -> Result<Vec<ProviderSnapshot>, WebError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(WebError::stopped());
        }
        let entries = self
            .inner
            .entries
            .lock()
            .map_err(|_| WebError::unavailable())?;
        let mut providers = entries
            .iter()
            .map(|entry| ProviderSnapshot {
                descriptor: entry.descriptor.clone(),
                provider: entry.provider.clone(),
                available: false,
            })
            .collect::<Vec<_>>();
        drop(entries);
        for provider in &mut providers {
            provider.available = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                provider.provider.available()
            }))
            .map_err(|_| WebError::unavailable())?;
        }
        providers.sort_by(|left, right| left.descriptor.id.cmp(&right.descriptor.id));
        Ok(providers)
    }

    pub(crate) fn replace_policy(&self, policy: WebPolicy) -> Result<(), WebError> {
        let mut current = self
            .inner
            .policy
            .write()
            .map_err(|_| WebError::unavailable())?;
        *current = policy;
        self.inner
            .policy_unavailable
            .store(false, Ordering::Release);
        Ok(())
    }

    pub(crate) fn mark_policy_unavailable(&self) {
        self.inner.policy_unavailable.store(true, Ordering::Release);
    }

    pub(crate) fn reset_policy(&self) {
        match self.inner.policy.write() {
            Ok(mut policy) => {
                *policy = WebPolicy::default();
                self.inner
                    .policy_unavailable
                    .store(false, Ordering::Release);
            }
            Err(_) => self.mark_policy_unavailable(),
        }
    }

    fn shutdown(&self) {
        self.inner.closed.store(true, Ordering::Release);
    }
}

#[derive(Clone)]
pub(crate) struct WebProcessorHandle {
    inner: Weak<RegistryInner>,
}

impl WebProcessorHandle {
    pub(crate) async fn extract(
        &self,
        document: &WebRawDocument,
        cancellation: CancellationToken,
    ) -> Result<Option<WebFetchResult>, WebError> {
        let inner = self.inner.upgrade().ok_or_else(WebError::stopped)?;
        extract_with_inner(&inner, document, cancellation).await
    }
}

async fn extract_with_inner(
    inner: &Arc<RegistryInner>,
    document: &WebRawDocument,
    cancellation: CancellationToken,
) -> Result<Option<WebFetchResult>, WebError> {
    if inner.closed.load(Ordering::Acquire) {
        return Err(WebError::stopped());
    }
    if cancellation.is_cancelled() {
        return Err(WebError::cancelled());
    }
    let processors = inner
        .processors
        .lock()
        .map_err(|_| WebError::unavailable())?
        .iter()
        .map(|entry| (entry.descriptor.clone(), entry.processor.clone()))
        .collect::<Vec<_>>();
    let mut matching = Vec::new();
    for (descriptor, processor) in processors {
        let supported = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            processor.supports(document)
        }))
        .map_err(|_| WebError::unavailable())?;
        if supported {
            matching.push((descriptor, processor));
        }
    }
    matching.sort_by(|left, right| left.0.id.cmp(&right.0.id));
    let [(_descriptor, processor)] = matching.as_slice() else {
        return if matching.is_empty() {
            Ok(None)
        } else {
            Err(WebError::ambiguous())
        };
    };
    let future = processor.process(document, cancellation.clone());
    let result = std::panic::AssertUnwindSafe(future)
        .catch_unwind()
        .await
        .map_err(|_| WebError::unavailable())??;
    if cancellation.is_cancelled() {
        return Err(WebError::cancelled());
    }
    result
        .validate()
        .map_err(|_| WebError::invalid_response())?;
    if result.final_url() != document.final_url()
        || result.content().len() > document.max_output_bytes()
    {
        return Err(WebError::invalid_response());
    }
    Ok(Some(result))
}

struct ProviderSnapshot {
    descriptor: WebProviderDescriptor,
    provider: Arc<dyn WebProvider>,
    available: bool,
}

#[derive(Clone, Copy)]
enum WebOperation {
    Search,
    Fetch,
}

impl WebOperation {
    const fn supported_by(self, descriptor: &WebProviderDescriptor) -> bool {
        match self {
            Self::Search => descriptor.search,
            Self::Fetch => descriptor.fetch,
        }
    }
}

/// One provider row in the safe capability report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebProviderStatus {
    /// Validated immutable descriptor.
    pub descriptor: WebProviderDescriptor,
    /// Cheap local operation-time availability.
    pub available: bool,
}

/// Effective provider selection for one web operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebProviderSelection {
    /// One provider is selected.
    Selected {
        /// Selected provider id.
        provider: String,
        /// True when Settings named the provider; false for unique automatic selection.
        configured: bool,
    },
    /// No usable provider implements the operation.
    Unavailable,
    /// Settings names an id not present in the current registry.
    ConfiguredMissing {
        /// Missing provider id.
        provider: String,
    },
    /// Settings names a provider that lacks this operation.
    ConfiguredUnsupported {
        /// Provider id lacking the operation.
        provider: String,
    },
    /// Settings names a registered provider whose local availability is false.
    ConfiguredUnavailable {
        /// Unavailable provider id.
        provider: String,
    },
    /// More than one usable provider exists and Settings selected none.
    Ambiguous {
        /// Sorted usable provider ids.
        providers: Vec<String>,
    },
}

/// Safe live provider/selection/domain snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebCapabilityReport {
    /// Sorted registered source-content processors.
    pub processors: Vec<WebContentProcessorDescriptor>,
    /// Sorted registered providers and local availability.
    pub providers: Vec<WebProviderStatus>,
    /// Effective search selection.
    pub search: WebProviderSelection,
    /// Effective fetch selection.
    pub fetch: WebProviderSelection,
    /// Effective search-result domain rules.
    pub search_domains: WebDomainPolicy,
    /// Effective fetch request/redirect/final URL rules.
    pub fetch_domains: WebDomainPolicy,
}

fn select_provider(
    operation: WebOperation,
    configured: Option<&str>,
    providers: &[ProviderSnapshot],
) -> WebProviderSelection {
    if let Some(configured) = configured {
        let Some(provider) = providers
            .iter()
            .find(|provider| provider.descriptor.id == configured)
        else {
            return WebProviderSelection::ConfiguredMissing {
                provider: configured.to_owned(),
            };
        };
        if !operation.supported_by(&provider.descriptor) {
            return WebProviderSelection::ConfiguredUnsupported {
                provider: configured.to_owned(),
            };
        }
        if !provider.available {
            return WebProviderSelection::ConfiguredUnavailable {
                provider: configured.to_owned(),
            };
        }
        return WebProviderSelection::Selected {
            provider: configured.to_owned(),
            configured: true,
        };
    }
    let usable = providers
        .iter()
        .filter(|provider| operation.supported_by(&provider.descriptor) && provider.available)
        .map(|provider| provider.descriptor.id.clone())
        .collect::<Vec<_>>();
    match usable.as_slice() {
        [] => WebProviderSelection::Unavailable,
        [provider] => WebProviderSelection::Selected {
            provider: provider.clone(),
            configured: false,
        },
        _ => WebProviderSelection::Ambiguous { providers: usable },
    }
}

struct ProviderRegistration {
    inner: Weak<RegistryInner>,
    id: String,
    token: Arc<()>,
}

struct ProcessorRegistration {
    inner: Weak<RegistryInner>,
    id: String,
    token: Arc<()>,
}

impl Drop for ProcessorRegistration {
    fn drop(&mut self) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let Ok(mut processors) = inner.processors.lock() else {
            return;
        };
        processors.retain(|entry| {
            entry.descriptor.id != self.id || !Arc::ptr_eq(&entry.token, &self.token)
        });
    }
}

impl Drop for ProviderRegistration {
    fn drop(&mut self) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let Ok(mut entries) = inner.entries.lock() else {
            return;
        };
        entries.retain(|entry| {
            entry.descriptor.id != self.id || !Arc::ptr_eq(&entry.token, &self.token)
        });
    }
}

/// Stable public web failure class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebErrorClass {
    /// Request shape is invalid.
    InvalidRequest,
    /// No provider supports the operation.
    Unsupported,
    /// Multiple usable providers require explicit selection.
    Ambiguous,
    /// The effective domain policy denied the URL.
    PolicyDenied,
    /// Registry/provider infrastructure is unavailable.
    Unavailable,
    /// Provider id is duplicated.
    Duplicate,
    /// Network request failed.
    Network,
    /// Provider operation timed out.
    Timeout,
    /// Provider returned non-success HTTP status.
    Http,
    /// Provider output violated the contract.
    InvalidResponse,
    /// Caller cancelled the operation.
    Cancelled,
    /// Context shutdown is terminal.
    Stopped,
}

/// Body-free public web error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct WebError {
    class: WebErrorClass,
    message: &'static str,
}

impl WebError {
    fn new(class: WebErrorClass, message: &'static str) -> Self {
        Self { class, message }
    }

    /// Stable class for policy/UI handling.
    #[must_use]
    pub const fn class(&self) -> WebErrorClass {
        self.class
    }

    /// Invalid request.
    #[must_use]
    pub fn invalid_request() -> Self {
        Self::new(WebErrorClass::InvalidRequest, "invalid web request")
    }

    /// Unsupported operation.
    #[must_use]
    pub fn unsupported() -> Self {
        Self::new(WebErrorClass::Unsupported, "web operation is unsupported")
    }

    /// Ambiguous automatic provider selection.
    #[must_use]
    pub fn ambiguous() -> Self {
        Self::new(
            WebErrorClass::Ambiguous,
            "web provider selection is ambiguous",
        )
    }

    /// Domain policy denial.
    #[must_use]
    pub fn policy_denied() -> Self {
        Self::new(
            WebErrorClass::PolicyDenied,
            "web domain policy denied the URL",
        )
    }

    /// Unavailable infrastructure.
    #[must_use]
    pub fn unavailable() -> Self {
        Self::new(WebErrorClass::Unavailable, "web provider is unavailable")
    }

    /// Duplicate provider.
    #[must_use]
    pub fn duplicate() -> Self {
        Self::new(WebErrorClass::Duplicate, "web provider id is duplicated")
    }

    /// Network failure.
    #[must_use]
    pub fn network() -> Self {
        Self::new(WebErrorClass::Network, "web network request failed")
    }

    /// Timeout.
    #[must_use]
    pub fn timeout() -> Self {
        Self::new(WebErrorClass::Timeout, "web request timed out")
    }

    /// HTTP status failure.
    #[must_use]
    pub fn http() -> Self {
        Self::new(WebErrorClass::Http, "web provider returned an HTTP error")
    }

    /// Invalid provider output.
    #[must_use]
    pub fn invalid_response() -> Self {
        Self::new(
            WebErrorClass::InvalidResponse,
            "web provider response is invalid",
        )
    }

    /// Caller cancellation.
    #[must_use]
    pub fn cancelled() -> Self {
        Self::new(WebErrorClass::Cancelled, "web operation was cancelled")
    }

    /// Terminal stopped service.
    #[must_use]
    pub fn stopped() -> Self {
        Self::new(WebErrorClass::Stopped, "web service is stopped")
    }
}

/// Publish the empty public web registry.
#[must_use]
pub fn web_registry_plugin() -> Box<dyn Plugin> {
    struct WebRegistryPlugin;

    impl Plugin for WebRegistryPlugin {
        fn name(&self) -> &'static str {
            "web"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_WEB]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let registry = WebRegistry::new();
            context.provide(SERVICE_WEB, self.name(), registry.clone())?;
            context.effect(move || registry.shutdown());
            Ok(())
        }
    }

    Box::new(WebRegistryPlugin)
}

pub(crate) fn safe_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        && bytes.len() <= 64
}

fn validate_public_url(value: &str) -> Result<(), WebError> {
    if value.is_empty()
        || value.len() > 4_096
        || value.chars().any(char::is_control)
        || value.chars().any(char::is_whitespace)
    {
        return Err(WebError::invalid_request());
    }
    let parsed = url::Url::parse(value).map_err(|_| WebError::invalid_request())?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || portable::url_host_is_obviously_private(&parsed)
    {
        return Err(WebError::invalid_request());
    }
    Ok(())
}

fn validate_single_line(value: &str, maximum: usize) -> Result<(), WebError> {
    if value.trim().is_empty()
        || value.len() > maximum
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(WebError::invalid_response());
    }
    Ok(())
}

fn validate_multiline(value: &str, maximum: usize) -> Result<(), WebError> {
    if value.len() > maximum
        || value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(WebError::invalid_response());
    }
    Ok(())
}

impl From<CoreError> for WebError {
    fn from(_value: CoreError) -> Self {
        Self::unavailable()
    }
}
