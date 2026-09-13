#![allow(clippy::unwrap_used)]

use super::*;
use serde_json::json;

fn styles() -> Styles {
    Styles::default()
}
fn text(lines: &[Line<'_>]) -> String {
    lines
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn notebook_only_projects_verified_success_and_retains_requested_source() {
    let args = json!({"path":"sample.ipynb","cell_index":0,"cell_id":"one","action":"replace","source":"a\nb\nc\nd\ne\n"});
    let result = (
        true,
        json!({"revision":"verified","cells":1,"executed":false}),
    );
    let lines = draw_lines("notebook_edit", &args, Some(&result), None, 110, styles()).unwrap();
    let rendered = text(&lines);
    assert!(rendered.contains("Edit Notebook(sample.ipynb@one)"));
    assert!(rendered.contains("Updated cell one:"));
    assert!(lines[1].spans.iter().any(|span| {
        span.content == "one"
            && span
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
    }));
    assert!(
        rendered.contains("1 a")
            && rendered.contains("3 c")
            && rendered.contains("+2 source lines")
    );
    assert_eq!(args["source"], "a\nb\nc\nd\ne\n");
    for result in [
        (false, json!({"message":"revision conflict"})),
        (true, json!({"status":"queued"})),
        (true, json!({"revision":"x","cells":1,"executed":true})),
    ] {
        assert!(draw_lines("notebook_edit", &args, Some(&result), None, 80, styles()).is_none());
    }
}

#[test]
fn notebook_receipt_keeps_unicode_and_control_text_within_cell_budget() {
    for width in [0, 1, 8, 20, 32, 110] {
        let receipt = notebook_receipt("Updated", "界界\u{1b}[31m unsafe", true, width, styles());
        assert!(receipt.width() <= width);
        assert!(!receipt.to_string().contains('\u{1b}'));
    }
}

#[test]
fn notebook_syntax_requires_authoritative_code_language_and_obeys_palette() {
    use heycode_ui::terminal::ColorLevel;
    use ratatui::style::Color;
    let source = "def calculate():\n\treturn 42\n# 界\u{1b}[31m\nfourth row";
    let metadata = json!({"cell_type":"code","language":"python"});
    for theme in heycode_ui::theme::builtin_themes().unwrap() {
        for level in [
            ColorLevel::None,
            ColorLevel::Basic,
            ColorLevel::Ansi256,
            ColorLevel::TrueColor,
        ] {
            let styles = Styles::new(&theme.resolve(level));
            for width in [0, 1, 10, 24, 110] {
                let rows = notebook_source_rows(source, &metadata, width, styles);
                assert_eq!(rows.len(), 3);
                assert!(rows.iter().all(|line| line.width() <= width));
                assert!(!text(&rows).contains('\u{1b}'));
                for span in rows.iter().flat_map(|row| &row.spans) {
                    match level {
                        ColorLevel::None => assert_eq!(span.style.fg, Some(Color::Reset)),
                        ColorLevel::Basic => assert!(!matches!(
                            span.style.fg,
                            Some(Color::Rgb(..) | Color::Indexed(16..=255))
                        )),
                        ColorLevel::Ansi256 => {
                            assert!(!matches!(span.style.fg, Some(Color::Rgb(..))))
                        }
                        ColorLevel::TrueColor => {}
                    }
                }
            }
        }
    }
    let colors = |metadata: &Value| {
        notebook_source_rows(source, metadata, 110, styles())
            .into_iter()
            .flat_map(|line| line.spans.into_iter().map(|span| span.style.fg))
            .collect::<Vec<_>>()
    };
    assert!(
        colors(&metadata)
            .iter()
            .any(|color| *color != Some(styles().text()))
    );
    for metadata in [
        json!({}),
        json!({"cell_type":"code","language":"unrecognized-fixture-language"}),
        json!({"cell_type":"markdown","language":"python"}),
        json!({"cell_type":"code"}),
    ] {
        assert!(
            colors(&metadata)
                .iter()
                .all(|color| *color == Some(styles().text()))
        );
    }
}

#[test]
fn notebook_syntax_input_is_bounded_before_processing_long_or_zero_width_lines() {
    for source in [
        "x".repeat(262_144),
        "\u{301}".repeat(131_072),
        "\u{1b}\t界".repeat(60_000),
    ] {
        for width in [0, 1, 10, 100, 100_000] {
            let preview = bounded_notebook_source(&source, width);
            assert!(preview.len() <= SOURCE_PREVIEW_CHAR_LIMIT * 4 + 3);
            assert!(unicode_width::UnicodeWidthStr::width(preview.as_str()) <= width);
            assert!(!preview.contains('\u{1b}'));
            if width > 0 {
                assert!(preview.contains('…'));
            }
        }
    }
}

#[test]
fn insertion_does_not_relabel_expected_anchor_as_created_cell() {
    let args = json!({"path":"sample.ipynb","cell_index":2,"cell_id":"existing-anchor","action":"insert","source":""});
    let result = (
        true,
        json!({"revision":"verified","cells":4,"executed":false}),
    );
    let lines = draw_lines(
        "mcp__heycode__notebook_edit",
        &args,
        Some(&result),
        None,
        110,
        styles(),
    )
    .unwrap();
    assert!(text(&lines).contains("Inserted cell 2:") && !text(&lines).contains("existing-anchor"));
    assert!(text(&lines).contains("empty source"));
}

#[test]
fn resource_preview_preserves_provenance_pagination_and_full_value() {
    let value = json!({"resources":[{"uri":"fixture://a","name":"alpha"},{"uri":"fixture://b","name":"beta"}],"returned":2,"total":9,"continuation":{"offset":2}});
    let original = value.clone();
    let result = (true, value);
    let lines = draw_lines(
        "mcp__heycode__list_mcp_resources",
        &json!({"server":"fixture"}),
        Some(&result),
        Some(UntrustedContentBoundary::mcp()),
        110,
        styles(),
    )
    .unwrap();
    let rendered = text(&lines);
    assert!(rendered.contains("listMcpResources") && rendered.contains("UNTRUSTED"));
    assert!(rendered.contains("2 of 9 returned · more available") && rendered.contains("… +"));
    assert_eq!(result.1, original);
    assert!(lines.len() <= compact_height_bound("list_mcp_resources", true).unwrap());
}

#[test]
fn read_resource_tool_truncation_and_narrow_unicode_remain_bounded() {
    let result = (
        true,
        json!({"server":"界界界","uri":"fixture://長い名前","contents":[{"body":{"kind":"text","text":"hello\u{1b}[31m world"}}],"truncated":true}),
    );
    for width in [1, 4, 12, 40, 110] {
        let lines = draw_lines(
            "read_mcp_resource",
            &json!({}),
            Some(&result),
            Some(UntrustedContentBoundary::mcp()),
            width,
            styles(),
        )
        .unwrap();
        assert!(
            lines.iter().all(|line| line.width() <= width),
            "width {width}: {}",
            text(&lines)
        );
        assert!(lines.len() <= compact_height_bound("read_mcp_resource", true).unwrap());
        assert!(!text(&lines).contains('\u{1b}'));
    }
    let lines = draw_lines(
        "read_mcp_resource",
        &json!({}),
        Some(&result),
        None,
        110,
        styles(),
    )
    .unwrap();
    assert!(text(&lines).contains("truncated by tool"));
    assert!(
        draw_lines(
            "read_mcp_resource",
            &json!({}),
            Some(&(false, json!({"error":"not ready"}))),
            None,
            110,
            styles()
        )
        .is_none()
    );
    assert!(draw_lines("workflow", &json!({}), Some(&result), None, 110, styles()).is_none());
    assert!(draw_lines("read_mcp_resource", &json!({}), None, None, 110, styles()).is_none());
}
