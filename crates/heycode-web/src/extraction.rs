//! Bounded readable HTML/PDF/text processor with durable raw-source capture.

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::{
    SERVICE_DOCUMENT_EXTRACTOR, SERVICE_WEB, WebContentProcessor, WebContentProcessorDescriptor,
    WebError, WebFetchResult, WebFetchSource, WebRawDocument, WebRegistry,
};

const MAX_PDF_PAGES: usize = 256;
const MAX_PDF_PAGE_DECOMPRESSED_BYTES: usize = 2 * 1024 * 1024;
const HTML_RENDER_WIDTH: usize = 100;
const EXTRACTION_TIMEOUT_SECS: u64 = 10;
const MAX_DOCUMENT_BYTES: usize = 32 * 1024 * 1024;
const MAX_DOCUMENT_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    Html,
    Pdf,
    Text,
}

impl SourceKind {
    const fn media_type(self) -> &'static str {
        match self {
            Self::Html => "text/html",
            Self::Pdf => "application/pdf",
            Self::Text => "text/plain",
        }
    }
}

/// One bounded local document extraction request.
#[derive(Clone)]
pub struct DocumentExtractionInput {
    media_type: heycode_core::AttachmentMediaType,
    bytes: Vec<u8>,
    max_output_bytes: usize,
}

impl DocumentExtractionInput {
    /// Construct a PDF/HTML/plain-text extraction request.
    ///
    /// # Errors
    /// Unsupported media, empty/>32 MiB bytes or output cap outside
    /// 1 byte..=4 MiB fails.
    pub fn new(
        media_type: heycode_core::AttachmentMediaType,
        bytes: Vec<u8>,
        max_output_bytes: usize,
    ) -> Result<Self, WebError> {
        if !matches!(
            media_type.as_str(),
            "application/pdf" | "text/html" | "application/xhtml+xml" | "text/plain"
        ) || bytes.is_empty()
            || bytes.len() > MAX_DOCUMENT_BYTES
            || !(1..=MAX_DOCUMENT_OUTPUT_BYTES).contains(&max_output_bytes)
        {
            return Err(WebError::invalid_request());
        }
        Ok(Self {
            media_type,
            bytes,
            max_output_bytes,
        })
    }
}

impl std::fmt::Debug for DocumentExtractionInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DocumentExtractionInput")
            .field("media_type", &self.media_type)
            .field("byte_count", &self.bytes.len())
            .field("max_output_bytes", &self.max_output_bytes)
            .finish()
    }
}

/// Bounded UTF-8 text and structural metadata derived from one document.
#[derive(Clone)]
pub struct DocumentExtraction {
    content: String,
    title: Option<String>,
    page_count: Option<u32>,
    truncated: bool,
    kind: SourceKind,
}

impl DocumentExtraction {
    /// Extracted text.
    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    /// HTML title, when present.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// PDF page count, when applicable.
    #[must_use]
    pub const fn page_count(&self) -> Option<u32> {
        self.page_count
    }

    /// Whether output was cut at the requested UTF-8-safe limit.
    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }
}

impl std::fmt::Debug for DocumentExtraction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DocumentExtraction")
            .field("content_bytes", &self.content.len())
            .field("has_title", &self.title.is_some())
            .field("page_count", &self.page_count)
            .field("truncated", &self.truncated)
            .field("kind", &self.kind)
            .finish()
    }
}

/// Effect-owned bounded document extraction service.
#[derive(Clone)]
pub struct DocumentExtractor {
    lifecycle: CancellationToken,
}

impl DocumentExtractor {
    fn new(lifecycle: CancellationToken) -> Self {
        Self { lifecycle }
    }

    /// Extract PDF/HTML/plain text without publishing attachment or web
    /// provenance. Callers durably record the selected result themselves.
    ///
    /// # Errors
    /// MIME/content mismatch, malformed or bomb-like input, timeout,
    /// cancellation or stopped lifecycle fails with a body-free class.
    pub async fn extract(
        &self,
        input: DocumentExtractionInput,
        cancellation: CancellationToken,
    ) -> Result<DocumentExtraction, WebError> {
        let kind = classify_input(&input)?;
        self.extract_kind(
            kind,
            input.bytes,
            input.max_output_bytes,
            false,
            cancellation,
        )
        .await
    }

    async fn extract_kind(
        &self,
        kind: SourceKind,
        bytes: Vec<u8>,
        maximum: usize,
        source_truncated: bool,
        cancellation: CancellationToken,
    ) -> Result<DocumentExtraction, WebError> {
        if self.lifecycle.is_cancelled() {
            return Err(WebError::stopped());
        }
        if cancellation.is_cancelled() {
            return Err(WebError::cancelled());
        }
        if kind == SourceKind::Pdf && source_truncated {
            return Err(WebError::invalid_response());
        }
        let worker_cancellation = cancellation.clone();
        let worker_lifecycle = self.lifecycle.clone();
        let extraction_cancellation = CancellationToken::new();
        let worker_extraction = extraction_cancellation.clone();
        let mut worker = tokio::task::spawn_blocking(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match kind {
                SourceKind::Html => extract_html(&bytes, maximum, source_truncated),
                SourceKind::Pdf => extract_pdf(
                    &bytes,
                    maximum,
                    &worker_cancellation,
                    &worker_lifecycle,
                    &worker_extraction,
                ),
                SourceKind::Text => extract_text(&bytes, maximum, source_truncated),
            }))
            .map_err(|_| WebError::invalid_response())?
        });
        let extracted = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                extraction_cancellation.cancel();
                let _settled = (&mut worker).await;
                return Err(WebError::cancelled());
            }
            () = self.lifecycle.cancelled() => {
                extraction_cancellation.cancel();
                let _settled = (&mut worker).await;
                return Err(WebError::stopped());
            }
            () = tokio::time::sleep(std::time::Duration::from_secs(EXTRACTION_TIMEOUT_SECS)) => {
                extraction_cancellation.cancel();
                let _settled = (&mut worker).await;
                return Err(WebError::timeout());
            }
            result = &mut worker => result
                .map_err(|_| WebError::unavailable())??,
        };
        if cancellation.is_cancelled() {
            return Err(WebError::cancelled());
        }
        if self.lifecycle.is_cancelled() {
            return Err(WebError::stopped());
        }
        Ok(extracted)
    }
}

fn classify_input(input: &DocumentExtractionInput) -> Result<SourceKind, WebError> {
    let declared = match input.media_type.as_str() {
        "application/pdf" => SourceKind::Pdf,
        "text/html" | "application/xhtml+xml" => SourceKind::Html,
        "text/plain" => SourceKind::Text,
        _ => return Err(WebError::unsupported()),
    };
    let sniffed = sniff(&input.bytes).ok_or_else(WebError::invalid_response)?;
    if declared != sniffed {
        return Err(WebError::invalid_response());
    }
    Ok(declared)
}

struct PortableReadableProcessor {
    attachments: Arc<heycode_attachments::AttachmentStore>,
    extractor: DocumentExtractor,
}

#[async_trait]
impl WebContentProcessor for PortableReadableProcessor {
    fn descriptor(&self) -> WebContentProcessorDescriptor {
        WebContentProcessorDescriptor {
            id: "portable-readable".to_owned(),
        }
    }

    fn supports(&self, document: &WebRawDocument) -> bool {
        classify(document).is_some()
    }

    async fn process(
        &self,
        document: &WebRawDocument,
        cancellation: CancellationToken,
    ) -> Result<WebFetchResult, WebError> {
        let kind = classify_checked(document)?;
        let extracted = self
            .extractor
            .extract_kind(
                kind,
                document.bytes().to_vec(),
                document.max_output_bytes(),
                document.source_truncated(),
                cancellation.clone(),
            )
            .await?;
        let retrieved_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_millis()).ok())
            .ok_or_else(WebError::unavailable)?;
        let durable_source = heycode_core::AttachmentSourceMetadata::new(
            document.final_url(),
            extracted.title.clone(),
            retrieved_at_ms,
            document.source_truncated(),
            extracted.page_count,
        )
        .map_err(|_| WebError::invalid_response())?;
        let input = heycode_attachments::AttachmentInput::new(
            document.bytes().to_vec(),
            Some(extracted.kind.media_type()),
            None,
        )
        .map_err(map_attachment_error)?
        .with_source(durable_source.clone());
        let admission = self
            .attachments
            .admit(input, cancellation.clone())
            .map_err(map_attachment_error)?;
        if cancellation.is_cancelled() || self.extractor.lifecycle.is_cancelled() {
            return Err(WebError::cancelled());
        }
        let source = WebFetchSource::new(
            durable_source.url(),
            durable_source.title(),
            Some(durable_source.retrieved_at_ms()),
            durable_source.source_truncated(),
            Some(admission.metadata().content_id().clone()),
            durable_source.page_count(),
        )?;
        WebFetchResult::with_source(
            source,
            extracted.content,
            Some("text/plain"),
            extracted.truncated,
        )
    }
}

/// Register the portable readable-content processor.
#[must_use]
pub fn web_extract_plugin() -> Box<dyn heycode_core::Plugin> {
    struct WebExtractPlugin;

    impl heycode_core::Plugin for WebExtractPlugin {
        fn name(&self) -> &'static str {
            "web-extract"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::Provider,
                ],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_DOCUMENT_EXTRACTOR]
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::WebProcessor,
                "portable-readable",
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_WEB, heycode_attachments::SERVICE_ATTACHMENTS]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let web = context
                .get::<WebRegistry>(SERVICE_WEB)
                .ok_or_else(|| heycode_core::CoreError::other("web service type mismatch"))?;
            let attachments = context
                .get::<heycode_attachments::AttachmentStore>(
                    heycode_attachments::SERVICE_ATTACHMENTS,
                )
                .ok_or_else(|| {
                    heycode_core::CoreError::other("attachments service type mismatch")
                })?;
            let lifecycle = CancellationToken::new();
            let extractor = DocumentExtractor::new(lifecycle.clone());
            context.provide(SERVICE_DOCUMENT_EXTRACTOR, self.name(), extractor.clone())?;
            let processor = Arc::new(PortableReadableProcessor {
                attachments,
                extractor,
            });
            web.register_processor(context, processor)
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            context.effect(move || lifecycle.cancel());
            Ok(())
        }
    }

    Box::new(WebExtractPlugin)
}

fn classify(document: &WebRawDocument) -> Option<SourceKind> {
    match document.content_type() {
        Some("text/html" | "application/xhtml+xml") => Some(SourceKind::Html),
        Some("application/pdf") => Some(SourceKind::Pdf),
        Some("text/plain") => Some(SourceKind::Text),
        None | Some("application/octet-stream") => sniff(document.bytes()),
        Some(_) => None,
    }
}

fn classify_checked(document: &WebRawDocument) -> Result<SourceKind, WebError> {
    let sniffed = sniff(document.bytes()).ok_or_else(WebError::invalid_response)?;
    let declared = match document.content_type() {
        None | Some("application/octet-stream") => None,
        Some("text/html" | "application/xhtml+xml") => Some(SourceKind::Html),
        Some("application/pdf") => Some(SourceKind::Pdf),
        Some("text/plain") => Some(SourceKind::Text),
        Some(_) => return Err(WebError::unsupported()),
    };
    if declared.is_some_and(|declared| declared != sniffed) {
        return Err(WebError::invalid_response());
    }
    Ok(sniffed)
}

fn sniff(bytes: &[u8]) -> Option<SourceKind> {
    if bytes.starts_with(b"%PDF-") {
        return Some(SourceKind::Pdf);
    }
    let text = std::str::from_utf8(bytes).ok()?;
    if looks_like_html(text) {
        Some(SourceKind::Html)
    } else if !bytes.contains(&0) {
        Some(SourceKind::Text)
    } else {
        None
    }
}

fn looks_like_html(text: &str) -> bool {
    let prefix = text
        .trim_start_matches(|character: char| character.is_whitespace() || character == '\u{feff}')
        .chars()
        .take(1_024)
        .collect::<String>()
        .to_ascii_lowercase();
    ["<!doctype html", "<html", "<head", "<body"]
        .iter()
        .any(|marker| prefix.starts_with(marker) || prefix.contains(&format!(">{marker}")))
}

fn extract_html(
    bytes: &[u8],
    maximum: usize,
    source_truncated: bool,
) -> Result<DocumentExtraction, WebError> {
    let rendered = html2text::from_read(std::io::Cursor::new(bytes), HTML_RENDER_WIDTH)
        .map_err(|_| WebError::invalid_response())?;
    let (content, output_truncated) = normalize_and_cap(&rendered, maximum)?;
    let title = std::str::from_utf8(bytes).ok().and_then(extract_html_title);
    Ok(DocumentExtraction {
        content,
        title,
        page_count: None,
        truncated: source_truncated || output_truncated,
        kind: SourceKind::Html,
    })
}

fn extract_text(
    bytes: &[u8],
    maximum: usize,
    source_truncated: bool,
) -> Result<DocumentExtraction, WebError> {
    let text = std::str::from_utf8(bytes).map_err(|_| WebError::invalid_response())?;
    let (content, output_truncated) = normalize_and_cap(text, maximum)?;
    Ok(DocumentExtraction {
        content,
        title: None,
        page_count: None,
        truncated: source_truncated || output_truncated,
        kind: SourceKind::Text,
    })
}

fn extract_pdf(
    bytes: &[u8],
    maximum: usize,
    cancellation: &CancellationToken,
    lifecycle: &CancellationToken,
    extraction_cancellation: &CancellationToken,
) -> Result<DocumentExtraction, WebError> {
    check_cancel(cancellation, lifecycle, extraction_cancellation)?;
    let document = lopdf::Document::load_mem_with_options(
        bytes,
        lopdf::LoadOptions::with_max_decompressed_size(MAX_PDF_PAGE_DECOMPRESSED_BYTES),
    )
    .map_err(|_| WebError::invalid_response())?;
    if document.is_encrypted() {
        return Err(WebError::unsupported());
    }
    let pages = document.get_pages();
    if pages.is_empty() || pages.len() > MAX_PDF_PAGES {
        return Err(WebError::invalid_response());
    }
    let mut text = String::new();
    let mut truncated = false;
    for page in pages.keys() {
        check_cancel(cancellation, lifecycle, extraction_cancellation)?;
        push_bounded(
            &mut text,
            &format!("[Page {page}]\n"),
            maximum,
            &mut truncated,
        );
        if truncated {
            break;
        }
        let chunks =
            document.extract_text_chunks_with_limit(&[*page], MAX_PDF_PAGE_DECOMPRESSED_BYTES);
        for chunk in chunks {
            let chunk = chunk.map_err(|_| WebError::invalid_response())?;
            push_bounded(&mut text, &chunk, maximum, &mut truncated);
            if truncated {
                break;
            }
        }
        if truncated {
            break;
        }
        text.push('\n');
    }
    let (content, normalized_truncated) = normalize_and_cap(&text, maximum)?;
    let page_count = u32::try_from(pages.len()).map_err(|_| WebError::invalid_response())?;
    Ok(DocumentExtraction {
        content,
        title: None,
        page_count: Some(page_count),
        truncated: truncated || normalized_truncated,
        kind: SourceKind::Pdf,
    })
}

fn extract_html_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let open = lower[start..].find('>')?.checked_add(start + 1)?;
    let end = lower[open..].find("</title>")?.checked_add(open)?;
    let rendered = html2text::from_read(
        std::io::Cursor::new(&html.as_bytes()[open..end]),
        HTML_RENDER_WIDTH,
    )
    .ok()?;
    let title = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        return None;
    }
    let mut title = title;
    truncate_utf8(&mut title, 512);
    Some(title)
}

fn normalize_and_cap(value: &str, maximum: usize) -> Result<(String, bool), WebError> {
    let normalized = value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if normalized.is_empty() {
        return Err(WebError::invalid_response());
    }
    let mut normalized = normalized;
    let truncated = truncate_utf8(&mut normalized, maximum);
    Ok((normalized, truncated))
}

fn push_bounded(output: &mut String, value: &str, maximum: usize, truncated: &mut bool) {
    let remaining = maximum.saturating_sub(output.len());
    if value.len() <= remaining {
        output.push_str(value);
        return;
    }
    let mut retained = value.to_owned();
    truncate_utf8(&mut retained, remaining);
    output.push_str(&retained);
    *truncated = true;
}

fn truncate_utf8(value: &mut String, maximum: usize) -> bool {
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

fn check_cancel(
    cancellation: &CancellationToken,
    lifecycle: &CancellationToken,
    extraction_cancellation: &CancellationToken,
) -> Result<(), WebError> {
    if cancellation.is_cancelled()
        || lifecycle.is_cancelled()
        || extraction_cancellation.is_cancelled()
    {
        Err(WebError::cancelled())
    } else {
        Ok(())
    }
}

fn map_attachment_error(error: heycode_attachments::AttachmentStoreError) -> WebError {
    match error.class() {
        heycode_attachments::AttachmentStoreErrorClass::Cancelled => WebError::cancelled(),
        heycode_attachments::AttachmentStoreErrorClass::Stopped
        | heycode_attachments::AttachmentStoreErrorClass::Unavailable
        | heycode_attachments::AttachmentStoreErrorClass::Io
        | heycode_attachments::AttachmentStoreErrorClass::Session
        | heycode_attachments::AttachmentStoreErrorClass::UnsupportedSecurity => {
            WebError::unavailable()
        }
        heycode_attachments::AttachmentStoreErrorClass::InvalidInput
        | heycode_attachments::AttachmentStoreErrorClass::SizeLimit
        | heycode_attachments::AttachmentStoreErrorClass::MimeMismatch
        | heycode_attachments::AttachmentStoreErrorClass::UnsupportedMedia
        | heycode_attachments::AttachmentStoreErrorClass::InvalidImage
        | heycode_attachments::AttachmentStoreErrorClass::Corrupt => WebError::invalid_response(),
    }
}
