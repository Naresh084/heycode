#![no_main]

use heycode_mcp::{
    McpNotificationKind, McpServerHandshake, results::McpStructuredCheck, results::McpToolResult,
};
use libfuzzer_sys::fuzz_target;

const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_RENDER_BYTES: usize = 1024 * 1024;

fuzz_target!(|input: &[u8]| {
    let input = &input[..input.len().min(MAX_INPUT_BYTES)];
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(input) else {
        return;
    };

    let output_schema = value.get("_fuzzOutputSchema");
    let first_result = McpToolResult::parse(&value, output_schema);
    let second_result = McpToolResult::parse(&value, output_schema);
    match (first_result, second_result) {
        (Ok(first), Ok(second)) => {
            assert!(
                first == second,
                "MCP tool-result parsing must be deterministic"
            );
            assert!(
                (output_schema.is_none() && first.check() == McpStructuredCheck::NoSchema)
                    || (output_schema.is_some() && first.check() != McpStructuredCheck::NoSchema),
                "MCP structured-content evidence must preserve schema presence"
            );
            assert!(
                first.blocks().len() <= 256,
                "accepted MCP result must respect the block cap"
            );

            let ui = first.ui_value();
            assert!(
                ui.get("schemaVersion").and_then(serde_json::Value::as_u64) == Some(1),
                "MCP UI projection must identify its schema"
            );
            assert!(
                ui.get("blocks")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|blocks| blocks.len() == first.blocks().len()),
                "MCP UI projection must preserve block count and order"
            );
            assert!(
                ui.get("isError").and_then(serde_json::Value::as_bool) == Some(first.is_error()),
                "MCP UI projection must preserve remote error state"
            );
            assert!(
                ui.get("structuredContent").is_some() == first.structured().is_some(),
                "MCP UI projection must preserve structured-content presence including null"
            );
            let rendered = first.render_for_model();
            assert!(
                rendered == first.render_for_model(),
                "MCP model projection must be deterministic"
            );
            assert!(
                rendered.len() <= MAX_RENDER_BYTES,
                "MCP model projection must remain bounded"
            );
        }
        (Err(_), Err(_)) => {}
        _ => panic!("MCP tool-result parse outcome must be deterministic"),
    }

    let first_handshake = McpServerHandshake::from_initialize_result(&value);
    let second_handshake = McpServerHandshake::from_initialize_result(&value);
    match (first_handshake, second_handshake) {
        (Ok(first), Ok(second)) => assert!(
            first == second,
            "MCP initialize handshake parsing must be deterministic"
        ),
        (Err(_), Err(_)) => {}
        _ => panic!("MCP handshake parse outcome must be deterministic"),
    }

    let notification = McpNotificationKind::parse(&value);
    assert!(
        notification == McpNotificationKind::parse(&value),
        "MCP notification parsing must be deterministic"
    );
    if let Some(notification) = notification {
        assert!(
            value.get("id").is_none(),
            "responses must not classify as notifications"
        );
        assert!(
            value.get("method").and_then(serde_json::Value::as_str) == Some(notification.method()),
            "classified MCP notification must preserve its exact method"
        );
    }
});
