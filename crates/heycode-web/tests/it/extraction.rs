//! WEB03 HTML/PDF extraction, attachment source and lifecycle contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_attachments::{AttachmentStoreConfig, local_attachment_plugin};
use heycode_session::{Session, SessionEventKind};
use heycode_web::{
    DocumentExtractionInput, DocumentExtractor, SERVICE_DOCUMENT_EXTRACTOR, SERVICE_WEB,
    WebContentProcessor, WebContentProcessorDescriptor, WebError, WebErrorClass, WebFetchResult,
    WebRawDocument, WebRegistry, web_extract_plugin, web_registry_plugin,
};
use tokio_util::sync::CancellationToken;

fn world() -> (tempfile::TempDir, heycode_core::Context) {
    let root = tempfile::tempdir().unwrap();
    let plugins = vec![
        heycode_session::session_plugin(root.path().join("sessions")),
        local_attachment_plugin(
            AttachmentStoreConfig::new(root.path().join("attachments"), 4 * 1024 * 1024).unwrap(),
        ),
        web_registry_plugin(),
        web_extract_plugin(),
    ];
    let context = heycode_core::compose(&plugins).unwrap();
    (root, context)
}

#[tokio::test]
async fn composed_document_extractor_reuses_pdf_bounds_without_publishing_web_provenance() {
    let (_root, mut context) = world();
    let extractor = context
        .get::<DocumentExtractor>(SERVICE_DOCUMENT_EXTRACTOR)
        .unwrap();
    let session = context
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let before = session.lock().unwrap().events().len();

    let extracted = extractor
        .extract(
            DocumentExtractionInput::new(
                heycode_core::AttachmentMediaType::new("application/pdf").unwrap(),
                simple_pdf("Local document"),
                4_096,
            )
            .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(extracted.content().contains("[Page 1]"));
    assert!(extracted.content().contains("Local document"));
    assert_eq!(extracted.page_count(), Some(1));
    assert_eq!(session.lock().unwrap().events().len(), before);
    context.shutdown();
    assert_eq!(
        extractor
            .extract(
                DocumentExtractionInput::new(
                    heycode_core::AttachmentMediaType::new("text/plain").unwrap(),
                    b"after".to_vec(),
                    128,
                )
                .unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err()
            .class(),
        WebErrorClass::Stopped
    );
}

#[tokio::test]
async fn html_extraction_is_bounded_citeable_and_commits_raw_source_metadata() {
    let (_root, mut context) = world();
    let web = context.get::<WebRegistry>(SERVICE_WEB).unwrap();
    let html = br#"<!doctype html><html><head><title>Example &amp; Docs</title><style>hidden</style></head><body><nav>menu</nav><main><h1>Heading</h1><p>Readable body.</p><a href="https://docs.example.test/page">Reference</a></main><script>secret()</script></body></html>"#;
    let document = WebRawDocument::new(
        "https://example.test/final",
        Some("text/html; charset=utf-8"),
        html.to_vec(),
        false,
        4_096,
    )
    .unwrap();
    let debug = format!("{document:?}");
    assert!(!debug.contains("example.test"));
    assert!(!debug.contains("Readable body"));

    let result = web
        .extract(document, CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert!(result.content().contains("Heading"), "{}", result.content());
    assert!(
        result.content().contains("Readable body"),
        "{}",
        result.content()
    );
    assert!(
        result.content().contains("https://docs.example.test/page"),
        "{}",
        result.content()
    );
    assert!(
        !result.content().contains("secret()"),
        "{}",
        result.content()
    );
    assert_eq!(result.source().url(), "https://example.test/final");
    assert_eq!(result.source().title(), Some("Example & Docs"));
    assert!(result.source().retrieved_at_ms().is_some());
    assert!(result.source().content_id().is_some());
    assert_eq!(result.source().page_count(), None);

    let session = context
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let attachments = session
        .lock()
        .unwrap()
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            SessionEventKind::AttachmentAdded { attachment } => Some(attachment.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0].media_type().as_str(), "text/html");
    assert_eq!(
        attachments[0].content_id(),
        result.source().content_id().unwrap()
    );
    let durable_source = attachments[0].source().unwrap();
    assert_eq!(durable_source.url(), result.source().url());
    assert_eq!(durable_source.title(), result.source().title());
    assert_eq!(
        durable_source.retrieved_at_ms(),
        result.source().retrieved_at_ms().unwrap()
    );
    assert!(
        context
            .plugin_inventory()
            .snapshot()
            .unwrap()
            .contributions
            .iter()
            .any(|row| row.plugin == "web-extract"
                && row.kind == heycode_core::ContributionKind::WebProcessor
                && row.name == "portable-readable")
    );
    context.shutdown();
    assert_eq!(
        web.extract(
            WebRawDocument::new(
                "https://example.test/after",
                Some("text/plain"),
                b"after".to_vec(),
                false,
                128,
            )
            .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err()
        .class(),
        WebErrorClass::Stopped
    );
}

#[tokio::test]
async fn pdf_extraction_preserves_page_source_and_refuses_malformed_or_cancelled_input() {
    let (_root, context) = world();
    let web = context.get::<WebRegistry>(SERVICE_WEB).unwrap();
    let result = web
        .extract(
            WebRawDocument::new(
                "https://example.test/doc.pdf",
                Some("application/pdf"),
                simple_pdf("Hello PDF world"),
                false,
                4_096,
            )
            .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.content().contains("Hello PDF world"),
        "{}",
        result.content()
    );
    assert!(
        result.content().contains("[Page 1]"),
        "{}",
        result.content()
    );
    assert_eq!(result.source().page_count(), Some(1));
    assert_eq!(result.content_type(), Some("text/plain"));

    let session = context
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let before = session.lock().unwrap().events().len();
    let malformed = web
        .extract(
            WebRawDocument::new(
                "https://example.test/bad.pdf",
                Some("application/pdf"),
                b"%PDF-not-valid".to_vec(),
                false,
                1_024,
            )
            .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(malformed.class(), WebErrorClass::InvalidResponse);
    assert_eq!(session.lock().unwrap().events().len(), before);

    let oversized_stream = web
        .extract(
            WebRawDocument::new(
                "https://example.test/oversized.pdf",
                Some("application/pdf"),
                simple_pdf(&"x".repeat(2 * 1024 * 1024 + 1)),
                false,
                1_024,
            )
            .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(oversized_stream.class(), WebErrorClass::InvalidResponse);
    assert_eq!(session.lock().unwrap().events().len(), before);

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let error = web
        .extract(
            WebRawDocument::new(
                "https://example.test/cancelled.pdf",
                Some("application/pdf"),
                simple_pdf("cancelled"),
                false,
                1_024,
            )
            .unwrap(),
            cancelled,
        )
        .await
        .unwrap_err();
    assert_eq!(error.class(), WebErrorClass::Cancelled);
    assert_eq!(session.lock().unwrap().events().len(), before);

    let unsupported = web
        .extract(
            WebRawDocument::new(
                "https://example.test/data.json",
                Some("application/json"),
                br#"{"value":1}"#.to_vec(),
                false,
                1_024,
            )
            .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(unsupported.is_none());
    assert_eq!(session.lock().unwrap().events().len(), before);
}

#[tokio::test]
async fn output_limit_is_utf8_safe_and_source_truncation_refuses_pdf() {
    let (_root, context) = world();
    let web = context.get::<WebRegistry>(SERVICE_WEB).unwrap();
    let html = format!(
        "<html><body><main>{}</main></body></html>",
        "é".repeat(2_000)
    );
    let result = web
        .extract(
            WebRawDocument::new(
                "https://example.test/large",
                Some("text/html"),
                html.into_bytes(),
                false,
                257,
            )
            .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(result.truncated());
    assert!(result.content().len() <= 257);
    assert!(std::str::from_utf8(result.content().as_bytes()).is_ok());

    let error = web
        .extract(
            WebRawDocument::new(
                "https://example.test/truncated.pdf",
                Some("application/pdf"),
                simple_pdf("partial"),
                true,
                1_024,
            )
            .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.class(), WebErrorClass::InvalidResponse);
}

struct AlwaysProcessor {
    id: &'static str,
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl WebContentProcessor for AlwaysProcessor {
    fn descriptor(&self) -> WebContentProcessorDescriptor {
        WebContentProcessorDescriptor::new(self.id).unwrap()
    }

    fn supports(&self, _document: &WebRawDocument) -> bool {
        true
    }

    async fn process(
        &self,
        document: &WebRawDocument,
        _cancellation: CancellationToken,
    ) -> Result<WebFetchResult, WebError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        WebFetchResult::new(document.final_url(), "processed", Some("text/plain"), false)
    }
}

#[tokio::test]
async fn processor_selection_is_unique_effect_owned_and_never_registration_ordered() {
    let plugins = vec![web_registry_plugin()];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let web = context.get::<WebRegistry>(SERVICE_WEB).unwrap();
    let alpha = std::sync::Arc::new(AlwaysProcessor {
        id: "alpha",
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let bravo = std::sync::Arc::new(AlwaysProcessor {
        id: "bravo",
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    web.register_processor(&context, alpha.clone()).unwrap();
    web.register_processor(&context, bravo.clone()).unwrap();
    let error = web
        .extract(
            WebRawDocument::new(
                "https://example.test/source",
                Some("text/plain"),
                b"text".to_vec(),
                false,
                128,
            )
            .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.class(), WebErrorClass::Ambiguous);
    assert_eq!(alpha.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(bravo.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(web.register_processor(&context, alpha).is_err());
    context.shutdown();
    assert!(web.processor_descriptors().unwrap().is_empty());
}

fn simple_pdf(text: &str) -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{Document, Object, Stream, dictionary};

    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! {"F1" => font_id},
    });
    let content = Content {
        operations: vec![
            Operation::new("BT", Vec::new()),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 12.into()]),
            Operation::new("Td", vec![20.into(), 50.into()]),
            Operation::new("Tj", vec![Object::string_literal(text)]),
            Operation::new("ET", Vec::new()),
        ],
    };
    let content_id = document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 100.into()],
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {"Type" => "Catalog", "Pages" => pages_id});
    document.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}
