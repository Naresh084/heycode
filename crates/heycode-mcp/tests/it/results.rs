//! MCP12 rich tool results and the output-schema bridge.
//!
//! The row asks that ordered text/link/image/structured results be preserved.
//! MCP06 kept only blocks carrying a `text` field and joined them, so an image
//! between two paragraphs vanished and the paragraphs closed over the gap — the
//! model saw a result the server never sent. Every case here drives the injected
//! request channel; nothing touches a network.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_core::{Plugin, compose};
use heycode_mcp::results::{
    McpEmbeddedBody, McpResultAudience, McpResultBlockKind, McpResultContent, McpStructuredCheck,
    McpToolResult,
};
use heycode_mcp::{
    McpChannelError, McpConnectionProviderId, McpDefinitionScope, McpNotificationRouter,
    McpRegistry, McpRequestChannel, McpServerDefinition, McpServerHandshake, McpServerId,
    McpSiblingContributions, McpStreamableHttpTransport, McpToolGenerationOwner, McpToolListLimits,
    McpTransportDefinition, SERVICE_MCP, mcp_registry_plugin,
};
use heycode_tools::{ToolCallInput, ToolCtx, ToolRegistry, run_tool};
use tokio_util::sync::CancellationToken;

// ── helpers ─────────────────────────────────────────────────────────────────

fn text(value: &str) -> serde_json::Value {
    serde_json::json!({"type": "text", "text": value})
}

fn image(mime: &str, bytes: &[u8]) -> serde_json::Value {
    use base64::Engine as _;
    serde_json::json!({
        "type": "image",
        "mimeType": mime,
        "data": base64::engine::general_purpose::STANDARD.encode(bytes)
    })
}

fn result(blocks: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({"content": blocks})
}

fn parse(blocks: Vec<serde_json::Value>) -> McpToolResult {
    McpToolResult::parse(&result(blocks), None).unwrap()
}

// ── order and kind are preserved ────────────────────────────────────────────

#[test]
fn an_all_text_result_renders_exactly_as_it_did_before_mcp12() {
    // The common case must not change: MCP06 joined text blocks with a single
    // newline, and a rich renderer that reflowed them would be a regression
    // dressed as a feature.
    let parsed = parse(vec![text("first"), text("second")]);

    assert_eq!(parsed.render_for_model(), "first\nsecond");
}

#[test]
fn an_image_between_two_texts_keeps_its_position() {
    let parsed = parse(vec![
        text("before"),
        image("image/png", b"PNGDATA"),
        text("after"),
    ]);

    // Under MCP06 this rendered as "before\nafter": the image disappeared and
    // the two paragraphs closed over the gap as though it had never been sent.
    assert_eq!(
        parsed.render_for_model(),
        "before\n[image image/png · 7 bytes]\nafter"
    );
}

#[test]
fn block_order_is_the_order_the_server_sent() {
    let parsed = parse(vec![
        image("image/png", b"a"),
        text("t"),
        serde_json::json!({"type": "audio", "mimeType": "audio/wav", "data": "AAA="}),
        serde_json::json!({"type": "resource_link", "uri": "file:///a", "name": "a"}),
        serde_json::json!({
            "type": "resource",
            "resource": {"uri": "file:///b", "mimeType": "text/plain", "text": "body"}
        }),
    ]);

    let kinds: Vec<McpResultBlockKind> = parsed
        .blocks()
        .iter()
        .map(heycode_mcp::results::McpResultBlock::kind)
        .collect();
    assert_eq!(
        kinds,
        [
            McpResultBlockKind::Image,
            McpResultBlockKind::Text,
            McpResultBlockKind::Audio,
            McpResultBlockKind::ResourceLink,
            McpResultBlockKind::EmbeddedResource,
        ]
    );
}

#[test]
fn a_resource_link_is_preserved_with_its_uri_name_and_type() {
    let parsed = parse(vec![serde_json::json!({
        "type": "resource_link",
        "uri": "file:///project/src/main.rs",
        "name": "main.rs",
        "description": "Primary application entry point",
        "mimeType": "text/x-rust"
    })]);

    let McpResultContent::ResourceLink(link) = parsed.blocks()[0].content() else {
        panic!("a resource_link must survive parsing");
    };
    assert_eq!(link.uri(), "file:///project/src/main.rs");
    assert_eq!(link.name(), "main.rs");
    assert_eq!(link.mime_type(), Some("text/x-rust"));
    let rendered = parsed.render_for_model();
    assert!(
        rendered.contains("file:///project/src/main.rs"),
        "{rendered}"
    );
    assert!(rendered.contains("main.rs"), "{rendered}");
}

#[test]
fn an_embedded_text_resource_keeps_its_body_and_uri() {
    let parsed = parse(vec![serde_json::json!({
        "type": "resource",
        "resource": {"uri": "file:///a.rs", "mimeType": "text/x-rust", "text": "fn main() {}"}
    })]);

    let McpResultContent::EmbeddedResource(resource) = parsed.blocks()[0].content() else {
        panic!("an embedded resource must survive parsing");
    };
    assert_eq!(resource.uri(), "file:///a.rs");
    assert_eq!(resource.body().text(), Some("fn main() {}"));
    assert!(parsed.render_for_model().contains("fn main() {}"));
}

#[test]
fn an_embedded_blob_reports_its_size_rather_than_its_bytes() {
    use base64::Engine as _;
    let parsed = parse(vec![serde_json::json!({
        "type": "resource",
        "resource": {
            "uri": "file:///a.bin",
            "blob": base64::engine::general_purpose::STANDARD.encode([0_u8; 9])
        }
    })]);

    let McpResultContent::EmbeddedResource(resource) = parsed.blocks()[0].content() else {
        panic!("an embedded blob must survive parsing");
    };
    assert_eq!(resource.body().blob().map(<[u8]>::len), Some(9));
    let rendered = parsed.render_for_model();
    assert!(rendered.contains("9 bytes"), "{rendered}");
    assert!(
        !rendered.contains('\u{0}'),
        "raw bytes must not be rendered"
    );
}

#[test]
fn images_are_reachable_for_an_attachment_consumer() {
    let parsed = parse(vec![
        text("t"),
        image("image/png", b"PNGDATA"),
        image("image/jpeg", b"JPG"),
    ]);

    let sizes: Vec<usize> = parsed
        .images()
        .map(heycode_mcp::results::McpResultMedia::len)
        .collect();
    assert_eq!(sizes, [7, 3], "decoded bytes are available to ATT01");
}

#[test]
fn block_annotations_and_attachment_identity_stay_on_the_exact_image() {
    use sha2::Digest as _;

    let parsed = parse(vec![
        text("before"),
        serde_json::json!({
            "type": "image",
            "mimeType": "IMAGE/PNG",
            "data": "UE5HREFUQQ==",
            "annotations": {
                "audience": ["user", "assistant"],
                "priority": 0.9,
                "lastModified": "2025-05-03T14:30:00Z",
                "vendorHint": {"keep": true}
            },
            "_meta": {"correlation": "image-1"}
        }),
        text("after"),
    ]);

    let image_block = &parsed.blocks()[1];
    assert_eq!(
        image_block.annotations().audience(),
        [McpResultAudience::User, McpResultAudience::Assistant]
    );
    assert_eq!(image_block.annotations().priority(), Some(0.9));
    assert_eq!(
        image_block.annotations().last_modified(),
        Some("2025-05-03T14:30:00Z")
    );
    assert_eq!(
        image_block.annotations().extensions()["vendorHint"]["keep"],
        true
    );
    assert_eq!(image_block.extensions()["_meta"]["correlation"], "image-1");

    let McpResultContent::Image(media) = image_block.content() else {
        panic!("the annotated block must remain an image");
    };
    assert_eq!(media.attachment_media_type().as_str(), "image/png");
    let digest: [u8; 32] = sha2::Sha256::digest(b"PNGDATA").into();
    assert_eq!(
        media.content_id().as_str(),
        heycode_core::AttachmentContentId::from_sha256(digest).as_str()
    );
}

#[test]
fn http_resource_links_enter_only_the_validated_public_source_plane() {
    let parsed = parse(vec![
        serde_json::json!({
            "type": "resource_link",
            "uri": "https://example.com/report?id=7",
            "name": "report",
            "title": "Public report",
            "size": 42
        }),
        serde_json::json!({
            "type": "resource_link",
            "uri": "file:///project/report.txt",
            "name": "local report"
        }),
    ]);

    let McpResultContent::ResourceLink(public) = parsed.blocks()[0].content() else {
        panic!("first block must remain a resource link");
    };
    assert_eq!(public.declared_size(), Some(42));
    let source = public
        .public_source()
        .expect("HTTP link is a public source");
    assert_eq!(source.url(), "https://example.com/report?id=7");
    assert_eq!(source.title(), Some("Public report"));

    let McpResultContent::ResourceLink(local) = parsed.blocks()[1].content() else {
        panic!("second block must remain a resource link");
    };
    assert!(local.public_source().is_none());
    assert_eq!(local.uri(), "file:///project/report.txt");
}

#[test]
fn credential_bearing_or_relative_resource_links_are_refused() {
    for uri in ["https://user:secret@example.com/a", "relative/path"] {
        let error = McpToolResult::parse(
            &result(vec![serde_json::json!({
                "type": "resource_link", "uri": uri, "name": "bad"
            })]),
            None,
        )
        .unwrap_err();

        assert!(matches!(error, McpChannelError::Protocol { .. }), "{uri}");
    }
}

#[test]
fn newer_result_and_block_extension_members_are_retained_not_flattened() {
    let parsed = McpToolResult::parse(
        &serde_json::json!({
            "resultType": "complete",
            "content": [{"type": "text", "text": "ok", "vendor": {"id": 7}}],
            "_meta": {"request": "r1"}
        }),
        None,
    )
    .unwrap();

    assert_eq!(parsed.extensions()["resultType"], "complete");
    assert_eq!(parsed.extensions()["_meta"]["request"], "r1");
    assert_eq!(parsed.blocks()[0].extensions()["vendor"]["id"], 7);
}

// ── structured content and the output schema ────────────────────────────────

fn schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "temperature": {"type": "number"},
            "conditions": {"type": "string"}
        },
        "required": ["temperature", "conditions"]
    })
}

fn with_structured(value: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"content": [text("summary")], "structuredContent": value})
}

#[test]
fn structured_content_is_preserved_and_rendered_after_the_blocks() {
    let parsed = McpToolResult::parse(
        &with_structured(serde_json::json!({"temperature": 22.5, "conditions": "cloudy"})),
        Some(&schema()),
    )
    .unwrap();

    assert_eq!(
        parsed.structured().and_then(|v| v.get("conditions")),
        Some(&serde_json::Value::String("cloudy".to_owned()))
    );
    let rendered = parsed.render_for_model();
    assert!(
        rendered.starts_with("summary\n[structured content]"),
        "{rendered}"
    );
    assert!(
        rendered.contains("\"conditions\": \"cloudy\""),
        "{rendered}"
    );
}

#[test]
fn a_conforming_object_passes_the_checks_this_build_performs() {
    let parsed = McpToolResult::parse(
        &with_structured(serde_json::json!({"temperature": 1, "conditions": "x"})),
        Some(&schema()),
    )
    .unwrap();

    assert_eq!(parsed.check(), McpStructuredCheck::Conforms);
    assert!(parsed.check().conforms());
}

#[test]
fn no_declared_schema_is_no_schema_not_conformance() {
    let parsed = McpToolResult::parse(
        &with_structured(serde_json::json!({"anything": true})),
        None,
    )
    .unwrap();

    assert_eq!(parsed.check(), McpStructuredCheck::NoSchema);
    assert!(!parsed.check().conforms(), "unchecked is never conformance");
    assert!(!parsed.check().is_violation());
}

#[test]
fn a_declared_schema_with_no_structured_content_is_a_violation() {
    let parsed = McpToolResult::parse(&result(vec![text("only prose")]), Some(&schema())).unwrap();

    assert_eq!(parsed.check(), McpStructuredCheck::Missing);
    assert!(parsed.check().is_violation());
}

#[test]
fn a_missing_required_property_violates() {
    let parsed = McpToolResult::parse(
        &with_structured(serde_json::json!({"temperature": 1})),
        Some(&schema()),
    )
    .unwrap();

    assert!(matches!(
        parsed.check(),
        McpStructuredCheck::Violates {
            requirement: "structuredContent is missing a required property"
        }
    ));
}

#[test]
fn a_wrong_top_level_type_violates() {
    let parsed = McpToolResult::parse(
        &with_structured(serde_json::json!("a string, not an object")),
        Some(&schema()),
    )
    .unwrap();

    assert!(matches!(
        parsed.check(),
        McpStructuredCheck::Violates {
            requirement: "structuredContent does not have the declared type"
        }
    ));
}

#[test]
fn a_nested_property_of_the_wrong_type_violates() {
    let parsed = McpToolResult::parse(
        &with_structured(serde_json::json!({"temperature": "warm", "conditions": "x"})),
        Some(&schema()),
    )
    .unwrap();

    assert!(parsed.check().is_violation(), "{:?}", parsed.check());
}

#[test]
fn an_unevaluated_schema_construct_is_not_checked_and_never_conforms() {
    // `anyOf` changes what conformance means. Ignoring it would let a schema
    // look satisfied that this build cannot actually decide.
    let parsed = McpToolResult::parse(
        &with_structured(serde_json::json!({"anything": 1})),
        Some(&serde_json::json!({"anyOf": [{"type": "object"}, {"type": "array"}]})),
    )
    .unwrap();

    assert_eq!(
        parsed.check(),
        McpStructuredCheck::NotChecked { construct: "anyOf" }
    );
    assert!(!parsed.check().conforms());
    assert!(
        !parsed.check().is_violation(),
        "declining to look is not a finding"
    );
}

#[test]
fn an_unevaluated_nested_construct_downgrades_the_whole_check() {
    let parsed = McpToolResult::parse(
        &with_structured(serde_json::json!({"temperature": 1, "conditions": "x"})),
        Some(&serde_json::json!({
            "type": "object",
            "required": ["temperature"],
            "properties": {"temperature": {"oneOf": [{"type": "number"}]}}
        })),
    )
    .unwrap();

    assert!(
        matches!(parsed.check(), McpStructuredCheck::NotChecked { .. }),
        "{:?}",
        parsed.check()
    );
}

#[test]
fn an_array_structured_content_is_accepted_for_an_array_schema() {
    // `2025-11-25` types structuredContent as an object; `2026-07-28` widens it
    // to any JSON value. Rejecting an array outright would break a dual-era
    // server, so the declared schema decides, not a hardcoded type rule.
    let parsed = McpToolResult::parse(
        &with_structured(serde_json::json!([{"id": "1"}, {"id": "2"}])),
        Some(&serde_json::json!({"type": "array"})),
    )
    .unwrap();

    assert_eq!(parsed.check(), McpStructuredCheck::Conforms);
}

#[test]
fn null_structured_content_is_preserved_for_a_null_schema() {
    let parsed = McpToolResult::parse(
        &with_structured(serde_json::Value::Null),
        Some(&serde_json::json!({"type": "null"})),
    )
    .unwrap();

    assert_eq!(parsed.structured(), Some(&serde_json::Value::Null));
    assert_eq!(parsed.check(), McpStructuredCheck::Conforms);
}

#[test]
fn an_unsupported_assertion_keyword_never_passes_as_conforming() {
    let parsed = McpToolResult::parse(
        &with_structured(serde_json::json!(1)),
        Some(&serde_json::json!({"type": "number", "minimum": 10})),
    )
    .unwrap();

    assert!(
        matches!(parsed.check(), McpStructuredCheck::NotChecked { .. }),
        "a partial evaluator must not claim conformance while ignoring `minimum`: {:?}",
        parsed.check()
    );
    assert!(!parsed.check().conforms());
}

#[test]
fn a_json_number_with_no_fraction_conforms_to_integer() {
    let parsed = McpToolResult::parse(
        &with_structured(serde_json::json!(1.0)),
        Some(&serde_json::json!({"type": "integer"})),
    )
    .unwrap();

    assert_eq!(parsed.check(), McpStructuredCheck::Conforms);
}

#[test]
fn a_schema_violation_is_reported_to_the_model_not_enforced() {
    // This build validates a subset of JSON Schema. Refusing a call on a subset
    // validator's opinion would turn its incompleteness into a broken server, so
    // the finding is made visible instead.
    let parsed = McpToolResult::parse(
        &with_structured(serde_json::json!({"temperature": 1})),
        Some(&schema()),
    )
    .unwrap();

    let rendered = parsed.render_for_model();
    assert!(
        rendered.contains("does not match the tool's declared output schema"),
        "{rendered}"
    );
    assert!(
        rendered.starts_with("summary"),
        "the result itself survives"
    );
}

// ── refusals: nothing is silently dropped ───────────────────────────────────

#[test]
fn an_unmodelled_block_type_refuses_the_result_rather_than_dropping_it() {
    let error = McpToolResult::parse(
        &result(vec![text("kept"), serde_json::json!({"type": "hologram"})]),
        None,
    )
    .unwrap_err();

    assert!(matches!(error, McpChannelError::Protocol { .. }));
}

#[test]
fn a_result_without_content_is_refused_not_read_as_empty() {
    // A 2026-07-28 `InputRequiredResult` reaches this path and carries no
    // `content`; reading it as an empty result would drop the server's request.
    let error = McpToolResult::parse(
        &serde_json::json!({"resultType": "input_required", "inputRequests": {}}),
        None,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        McpChannelError::Protocol {
            requirement: "tools/call result must contain a content array"
        }
    ));
}

#[test]
fn a_non_boolean_is_error_refuses_the_result() {
    let error = McpToolResult::parse(
        &serde_json::json!({"content": [text("x")], "isError": "yes"}),
        None,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        McpChannelError::Protocol {
            requirement: "isError must be absent or a boolean"
        }
    ));
}

#[test]
fn null_is_not_an_absent_is_error_field() {
    let error = McpToolResult::parse(
        &serde_json::json!({"content": [text("x")], "isError": null}),
        None,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        McpChannelError::Protocol {
            requirement: "isError must be absent or a boolean"
        }
    ));
}

#[test]
fn an_embedded_resource_with_both_text_and_blob_is_refused() {
    let error = McpToolResult::parse(
        &result(vec![serde_json::json!({
            "type": "resource",
            "resource": {"uri": "file:///a", "text": "t", "blob": "AAA="}
        })]),
        None,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        McpChannelError::Protocol {
            requirement: "embedded resource contents must carry exactly one of text or blob"
        }
    ));
}

#[test]
fn invalid_base64_media_is_refused() {
    let error = McpToolResult::parse(
        &result(vec![
            serde_json::json!({"type": "image", "mimeType": "image/png", "data": "not base64!!"}),
        ]),
        None,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        McpChannelError::Protocol {
            requirement: "media content data must be valid base64"
        }
    ));
}

#[test]
fn a_control_bearing_mime_type_is_refused() {
    let error = McpToolResult::parse(
        &result(vec![serde_json::json!({
            "type": "image", "mimeType": "image/png\u{1b}[2J", "data": "AAA="
        })]),
        None,
    )
    .unwrap_err();

    assert!(matches!(error, McpChannelError::Protocol { .. }));
}

#[test]
fn media_without_a_mime_type_is_refused() {
    // The specification requires a valid MIME type on both media kinds; guessing
    // one is how an unknown binary becomes a rendered image.
    let error = McpToolResult::parse(
        &result(vec![serde_json::json!({"type": "image", "data": "AAA="})]),
        None,
    )
    .unwrap_err();

    assert!(matches!(error, McpChannelError::Protocol { .. }));
}

// ── bounds ──────────────────────────────────────────────────────────────────

#[test]
fn an_oversized_result_is_refused_before_any_block_is_normalized() {
    let oversized = serde_json::json!({
        "content": [{"type": "hologram"}],
        "_meta": {"padding": "p".repeat(4 * 1024 * 1024 + 1)}
    });

    let error = McpToolResult::parse(&oversized, None).unwrap_err();

    assert!(
        matches!(
            error,
            McpChannelError::Protocol {
                requirement: "tools/call result exceeds the 4194304-byte bound"
            }
        ),
        "expected the size refusal, got {error:?}"
    );
}

#[test]
fn the_block_count_is_bounded() {
    let blocks: Vec<serde_json::Value> = (0..257).map(|_| text("x")).collect();

    let error = McpToolResult::parse(&result(blocks), None).unwrap_err();

    assert!(matches!(
        error,
        McpChannelError::Protocol {
            requirement: "tools/call result exceeds the 256-block bound"
        }
    ));
}

#[test]
fn one_text_block_is_bounded() {
    let error =
        McpToolResult::parse(&result(vec![text(&"x".repeat(64 * 1024 + 1))]), None).unwrap_err();

    assert!(matches!(
        error,
        McpChannelError::Protocol {
            requirement: "a text content block must be at most 65536 bytes"
        }
    ));
}

#[test]
fn total_text_across_blocks_is_bounded() {
    let blocks: Vec<serde_json::Value> = (0..5).map(|_| text(&"x".repeat(64 * 1024))).collect();

    let error = McpToolResult::parse(&result(blocks), None).unwrap_err();

    assert!(matches!(
        error,
        McpChannelError::Protocol {
            requirement: "tools/call text exceeds the 262144-byte total bound"
        }
    ));
}

#[test]
fn structured_content_is_bounded() {
    let error = McpToolResult::parse(
        &with_structured(serde_json::json!({"blob": "b".repeat(1024 * 1024 + 1)})),
        None,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        McpChannelError::Protocol {
            requirement: "structuredContent exceeds the 1048576-byte bound"
        }
    ));
}

#[test]
fn debug_reports_counts_never_bodies() {
    let parsed = McpToolResult::parse(
        &serde_json::json!({
            "content": [text("secret prose"), image("image/png", b"SECRETBYTES")],
            "structuredContent": {"token": "sk-live-abc"}
        }),
        None,
    )
    .unwrap();

    let rendered = format!("{parsed:?}");
    assert!(!rendered.contains("secret prose"), "{rendered}");
    assert!(!rendered.contains("sk-live-abc"), "{rendered}");
    assert!(rendered.contains("blocks: 2"), "{rendered}");

    let McpResultContent::Image(media) = parsed.blocks()[1].content() else {
        panic!("expected an image block");
    };
    let media_debug = format!("{media:?}");
    assert!(!media_debug.contains("SECRETBYTES"), "{media_debug}");
    assert!(media_debug.contains("11"), "{media_debug}");
}

#[test]
fn an_embedded_body_debug_reports_a_kind_and_a_length() {
    let body = McpEmbeddedBody::Text("hello".to_owned());
    assert_eq!(format!("{body:?}"), "McpEmbeddedBody::text(5 bytes)");
}

#[test]
fn individual_block_debug_never_exposes_server_text_or_resource_uris() {
    let parsed = parse(vec![
        text("SERVER-TEXT-CANARY"),
        serde_json::json!({
            "type": "resource_link",
            "uri": "file:///SECRET-RESOURCE-CANARY",
            "name": "SECRET-NAME-CANARY",
            "description": "SECRET-DESCRIPTION-CANARY"
        }),
    ]);

    let rendered = format!("{:?} {:?}", parsed.blocks()[0], parsed.blocks()[1]);
    for canary in [
        "SERVER-TEXT-CANARY",
        "SECRET-RESOURCE-CANARY",
        "SECRET-NAME-CANARY",
        "SECRET-DESCRIPTION-CANARY",
    ] {
        assert!(!rendered.contains(canary), "{rendered}");
    }
}

// ── through a registered tool ───────────────────────────────────────────────

struct ScriptedChannel {
    responses: Mutex<VecDeque<Result<serde_json::Value, McpChannelError>>>,
}

impl ScriptedChannel {
    fn with(responses: Vec<Result<serde_json::Value, McpChannelError>>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into_iter().collect()),
        })
    }
}

#[async_trait]
impl McpRequestChannel for ScriptedChannel {
    async fn call(
        &self,
        method: &str,
        _params: serde_json::Value,
        _cancellation: &CancellationToken,
    ) -> Result<serde_json::Value, McpChannelError> {
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("unscripted MCP request `{method}`"))
    }
}

struct CancellationProbeChannel {
    listed: AtomicBool,
    cancellation_observed: AtomicBool,
}

impl CancellationProbeChannel {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            listed: AtomicBool::new(false),
            cancellation_observed: AtomicBool::new(false),
        })
    }
}

#[async_trait]
impl McpRequestChannel for CancellationProbeChannel {
    async fn call(
        &self,
        method: &str,
        _params: serde_json::Value,
        cancellation: &CancellationToken,
    ) -> Result<serde_json::Value, McpChannelError> {
        match method {
            "tools/list" if !self.listed.swap(true, Ordering::SeqCst) => {
                Ok(serde_json::json!({"tools": [tool_row()]}))
            }
            "tools/call" => {
                cancellation.cancelled().await;
                self.cancellation_observed.store(true, Ordering::SeqCst);
                Err(McpChannelError::Cancelled)
            }
            _ => panic!("unexpected MCP request `{method}`"),
        }
    }
}

fn handshake() -> McpServerHandshake {
    McpServerHandshake::from_initialize_result(&serde_json::json!({
        "protocolVersion": "2025-11-25",
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "fixture", "version": "1.0"}
    }))
    .unwrap()
}

/// Register one MCP tool whose `tools/list` row and `tools/call` reply are given.
async fn registered(
    row: serde_json::Value,
    reply: serde_json::Value,
) -> (
    heycode_core::Context,
    Arc<ToolRegistry>,
    McpToolGenerationOwner,
) {
    let channel = ScriptedChannel::with(vec![Ok(serde_json::json!({"tools": [row]})), Ok(reply)]);
    registered_with_channel(channel as Arc<dyn McpRequestChannel>).await
}

async fn registered_with_channel(
    channel: Arc<dyn McpRequestChannel>,
) -> (
    heycode_core::Context,
    Arc<ToolRegistry>,
    McpToolGenerationOwner,
) {
    let plugins: Vec<Box<dyn Plugin>> = vec![mcp_registry_plugin()];
    let context = compose(&plugins).unwrap();
    let registry = context.get::<McpRegistry>(SERVICE_MCP).unwrap();
    let id = McpServerId::new("fixture").unwrap();
    let transport =
        McpStreamableHttpTransport::new("https://example.test/mcp", Default::default()).unwrap();
    registry
        .register_definition(
            &context,
            McpServerDefinition::new(
                "fixture",
                "fixture",
                McpDefinitionScope::User,
                McpTransportDefinition::StreamableHttp(transport),
            )
            .unwrap(),
        )
        .unwrap();
    let publisher = registry
        .register_connection(
            &context,
            &id,
            McpConnectionProviderId::new("test").unwrap(),
            0,
        )
        .unwrap();
    let tools = Arc::new(ToolRegistry::new());
    let definition = registry.definition(&id).unwrap().unwrap();
    let owner = McpToolGenerationOwner::new(
        &definition,
        Arc::clone(&tools),
        publisher,
        McpNotificationRouter::new().tools(),
        McpToolListLimits::default(),
    );
    owner
        .refresh(
            channel,
            &handshake(),
            McpSiblingContributions::none(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    (context, tools, owner)
}

fn tool_row() -> serde_json::Value {
    serde_json::json!({
        "name": "weather",
        "description": "w",
        "inputSchema": {"type": "object"},
        "outputSchema": {"type": "object", "required": ["temperature"]}
    })
}

#[tokio::test]
async fn an_mcp_tool_classifies_its_output_as_untrusted_mcp_content() {
    let (mut context, tools, owner) = registered(tool_row(), result(vec![text("ok")])).await;

    let tool = tools.get("mcp__fixture__weather").unwrap();
    let boundary = tool.untrusted_content().expect("MCP output is untrusted");

    assert_eq!(boundary.source(), heycode_core::UntrustedContentSource::Mcp);
    assert!(
        boundary
            .render_for_model("body")
            .contains("UNTRUSTED MCP SERVER CONTENT")
    );
    drop(owner);
    context.shutdown();
}

#[tokio::test]
async fn rich_calls_thread_the_operation_token_and_rows_remain_owner_scoped() {
    let channel = CancellationProbeChannel::new();
    let (mut context, tools, owner) =
        registered_with_channel(Arc::clone(&channel) as Arc<dyn McpRequestChannel>).await;
    let tool = tools.get("mcp__fixture__weather").unwrap();
    let cancellation = CancellationToken::new();
    let operation = cancellation.clone();
    let task = tokio::spawn(async move {
        tool.run(
            serde_json::json!({}),
            &ToolCtx {
                cancellation: operation,
                ..ToolCtx::default()
            },
        )
        .await
    });

    tokio::task::yield_now().await;
    cancellation.cancel();
    let error = tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .expect("the cancelled call must settle")
        .expect("the call task must not panic")
        .unwrap_err();

    assert!(error.to_string().contains("cancelled"), "{error}");
    assert!(channel.cancellation_observed.load(Ordering::SeqCst));
    assert!(tools.get("mcp__fixture__weather").is_some());
    drop(owner);
    assert!(
        tools.get("mcp__fixture__weather").is_none(),
        "the rich row must not outlive its generation owner"
    );
    context.shutdown();
}

#[tokio::test]
async fn a_declared_output_schema_reaches_the_call_that_validates_against_it() {
    let (mut context, tools, owner) = registered(
        tool_row(),
        serde_json::json!({"content": [text("summary")], "structuredContent": {"other": 1}}),
    )
    .await;

    let output = tools
        .get("mcp__fixture__weather")
        .unwrap()
        .run(serde_json::json!({}), &ToolCtx::default())
        .await
        .unwrap();

    assert_eq!(output["schemaCheck"]["status"], "violates");
    assert_eq!(
        output["schemaCheck"]["requirement"], "structuredContent is missing a required property",
        "the schema from tools/list must be applied to tools/call"
    );
    drop(owner);
    context.shutdown();
}

#[tokio::test]
async fn a_registered_rich_result_is_typed_instead_of_flattened_to_display_text() {
    let (mut context, tools, owner) = registered(
        serde_json::json!({
            "name": "weather",
            "description": "w",
            "inputSchema": {"type": "object"}
        }),
        serde_json::json!({
            "content": [
                text("before"),
                serde_json::json!({
                    "type": "resource_link",
                    "uri": "https://example.test/report",
                    "name": "report"
                }),
                image("image/png", b"IMAGE-BYTES"),
                text("after")
            ],
            "structuredContent": {"temperature": 21}
        }),
    )
    .await;

    let output = tools
        .get("mcp__fixture__weather")
        .unwrap()
        .run(serde_json::json!({}), &ToolCtx::default())
        .await
        .unwrap();

    assert_eq!(output["schemaVersion"], 1);
    assert_eq!(output["blocks"][0]["type"], "text");
    assert_eq!(output["blocks"][1]["type"], "resource_link");
    assert_eq!(output["blocks"][2]["type"], "image");
    assert_eq!(output["blocks"][3]["type"], "text");
    assert_eq!(output["structuredContent"]["temperature"], 21);
    drop(owner);
    context.shutdown();
}

#[tokio::test]
async fn a_tool_execution_error_carries_every_block_not_just_the_text() {
    let (mut context, tools, owner) = registered(
        serde_json::json!({"name": "weather", "description": "w", "inputSchema": {"type": "object"}}),
        serde_json::json!({
            "content": [text("failed because"), image("image/png", b"DIAGRAM")],
            "isError": true
        }),
    )
    .await;

    let error = tools
        .get("mcp__fixture__weather")
        .unwrap()
        .run(serde_json::json!({}), &ToolCtx::default())
        .await
        .unwrap_err();

    let message = error.to_string();
    assert!(message.contains("failed because"), "{message}");
    assert!(message.contains("[image image/png · 7 bytes]"), "{message}");
    drop(owner);
    context.shutdown();
}

#[tokio::test]
async fn guarded_pipeline_preserves_typed_blocks_when_mcp_reports_is_error() {
    let (mut context, tools, owner) = registered(
        serde_json::json!({"name":"weather","description":"w","inputSchema":{"type":"object"}}),
        serde_json::json!({
            "content": [text("failed because"), image("image/png", b"DIAGRAM")],
            "structuredContent": {"code":"failed"},
            "isError": true
        }),
    )
    .await;

    let outcome = run_tool(
        &tools,
        ToolCallInput {
            name: "mcp__fixture__weather".to_owned(),
            args: serde_json::json!({}),
        },
        &ToolCtx::default(),
    )
    .await
    .unwrap();

    assert!(outcome.reported_error);
    let rich = outcome
        .rich_result
        .expect("error result keeps typed blocks");
    let (blocks, structured, _, _) = rich.into_parts();
    assert!(matches!(
        blocks.as_slice(),
        [
            heycode_tools::PendingToolResultBlock::Text { .. },
            heycode_tools::PendingToolResultBlock::Image { .. }
        ]
    ));
    assert_eq!(
        structured.value(),
        Some(&serde_json::json!({"code":"failed"}))
    );
    drop(owner);
    context.shutdown();
}

#[tokio::test]
async fn a_non_object_output_schema_rejects_the_generation() {
    let plugins: Vec<Box<dyn Plugin>> = vec![mcp_registry_plugin()];
    let mut context = compose(&plugins).unwrap();
    let registry = context.get::<McpRegistry>(SERVICE_MCP).unwrap();
    let id = McpServerId::new("fixture").unwrap();
    let transport =
        McpStreamableHttpTransport::new("https://example.test/mcp", Default::default()).unwrap();
    registry
        .register_definition(
            &context,
            McpServerDefinition::new(
                "fixture",
                "fixture",
                McpDefinitionScope::User,
                McpTransportDefinition::StreamableHttp(transport),
            )
            .unwrap(),
        )
        .unwrap();
    let publisher = registry
        .register_connection(
            &context,
            &id,
            McpConnectionProviderId::new("test").unwrap(),
            0,
        )
        .unwrap();
    let definition = registry.definition(&id).unwrap().unwrap();
    let owner = McpToolGenerationOwner::new(
        &definition,
        Arc::new(ToolRegistry::new()),
        publisher,
        McpNotificationRouter::new().tools(),
        McpToolListLimits::default(),
    );
    let channel = ScriptedChannel::with(vec![Ok(serde_json::json!({"tools": [{
        "name": "weather", "description": "w",
        "inputSchema": {"type": "object"}, "outputSchema": "not a schema"
    }]}))]);

    let error = owner
        .refresh(
            channel as Arc<dyn McpRequestChannel>,
            &handshake(),
            McpSiblingContributions::none(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        McpChannelError::Protocol {
            requirement: "tool outputSchema must be absent or a JSON object"
        }
    ));
    context.shutdown();
}

#[tokio::test]
async fn a_null_output_schema_does_not_weaken_the_tool_definition_schema() {
    let plugins: Vec<Box<dyn Plugin>> = vec![mcp_registry_plugin()];
    let mut context = compose(&plugins).unwrap();
    let registry = context.get::<McpRegistry>(SERVICE_MCP).unwrap();
    let id = McpServerId::new("fixture").unwrap();
    let transport =
        McpStreamableHttpTransport::new("https://example.test/mcp", Default::default()).unwrap();
    registry
        .register_definition(
            &context,
            McpServerDefinition::new(
                "fixture",
                "fixture",
                McpDefinitionScope::User,
                McpTransportDefinition::StreamableHttp(transport),
            )
            .unwrap(),
        )
        .unwrap();
    let publisher = registry
        .register_connection(
            &context,
            &id,
            McpConnectionProviderId::new("test").unwrap(),
            0,
        )
        .unwrap();
    let definition = registry.definition(&id).unwrap().unwrap();
    let owner = McpToolGenerationOwner::new(
        &definition,
        Arc::new(ToolRegistry::new()),
        publisher,
        McpNotificationRouter::new().tools(),
        McpToolListLimits::default(),
    );
    let channel = ScriptedChannel::with(vec![Ok(serde_json::json!({"tools": [{
        "name": "weather", "description": "w",
        "inputSchema": {"type": "object"}, "outputSchema": null
    }]}))]);

    let error = owner
        .refresh(
            channel as Arc<dyn McpRequestChannel>,
            &handshake(),
            McpSiblingContributions::none(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        McpChannelError::Protocol {
            requirement: "tool outputSchema must be absent or a JSON object"
        }
    ));
    context.shutdown();
}
