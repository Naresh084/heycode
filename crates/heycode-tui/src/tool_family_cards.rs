//! Source-backed compact notebook and MCP resource cards.
//!
//! Only admitted, successful native result shapes are projected here. Running,
//! failed and unknown results remain with the generic renderer, as do expanded
//! cards: their original result metadata and content must remain inspectable.

use heycode_core::UntrustedContentBoundary;
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::terminal::{Styles, truncate_to_width};

const PREVIEW_LINES: usize = 3;
const SOURCE_PREVIEW_CHAR_LIMIT: usize = 4096;
const RECEIPT: &str = "  ⎿  ";
const CONTINUATION: &str = "     ";

fn canonical(name: &str) -> &str {
    name.strip_prefix("mcp__heycode__").unwrap_or(name)
}

/// Conservative compact bound: header, provenance, receipt, three preview rows,
/// omitted-row notice and one native pagination/truncation notice.
pub(crate) fn compact_height_bound(name: &str, untrusted_content: bool) -> Option<usize> {
    matches!(
        canonical(name),
        "notebook_edit" | "list_mcp_resources" | "read_mcp_resource"
    )
    .then_some(7 + usize::from(untrusted_content))
}

fn safe(text: &str) -> String {
    crate::markdown::terminal_safe_span(&text.replace('\t', "    ")).into_owned()
}

fn row(text: String, width: usize, color: ratatui::style::Color) -> Line<'static> {
    Line::styled(
        truncate_to_width(&safe(&text), width),
        Style::default().fg(color),
    )
}

fn notebook_receipt(
    verb: &str,
    cell: &str,
    has_source: bool,
    width: usize,
    styles: Styles,
) -> Line<'static> {
    let plain = Style::default().fg(styles.text());
    let parts = [
        (format!("{RECEIPT}{verb} cell "), plain),
        (safe(cell), plain.bold()),
        (if has_source { ":" } else { "" }.to_owned(), plain),
    ];
    let mut remaining = width;
    let spans = parts
        .into_iter()
        .map(|(text, style)| {
            let text = truncate_to_width(&text, remaining);
            remaining =
                remaining.saturating_sub(unicode_width::UnicodeWidthStr::width(text.as_str()));
            Span::styled(text, style)
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

fn notebook_source_rows(
    source: &str,
    result: &Value,
    width: usize,
    styles: Styles,
) -> Vec<Line<'static>> {
    use heycode_ui::terminal::ColorLevel;
    let preview = source
        .lines()
        .take(PREVIEW_LINES)
        .map(|line| bounded_notebook_source(line, width.saturating_sub(9)))
        .collect::<Vec<_>>();
    let highlighted = (result["cell_type"] == "code" && styles.level() != ColorLevel::None)
        .then_some(result["language"].as_str())
        .flatten()
        .and_then(|language| {
            crate::markdown::highlight_code(
                language,
                &preview.join("\n"),
                crate::render::foreground_is_dark(styles.text()),
            )
        });
    preview
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let prefix = truncate_to_width(&format!("{CONTINUATION}{:>3} ", index + 1), width);
            let mut remaining =
                width.saturating_sub(unicode_width::UnicodeWidthStr::width(prefix.as_str()));
            let mut spans = vec![Span::styled(prefix, Style::default().fg(styles.text()))];
            if let Some(line) = highlighted.as_ref().and_then(|lines| lines.get(index)) {
                for span in &line.spans {
                    let text = truncate_to_width(&safe(&span.content), remaining);
                    remaining = remaining
                        .saturating_sub(unicode_width::UnicodeWidthStr::width(text.as_str()));
                    let color = match (styles.level(), span.style.fg) {
                        (ColorLevel::TrueColor, Some(color)) => color,
                        (ColorLevel::Ansi256, Some(ratatui::style::Color::Rgb(r, g, b))) => {
                            ratatui::style::Color::Indexed(heycode_ui::theme::quantize_256(
                                heycode_ui::theme::Rgb { r, g, b },
                            ))
                        }
                        (ColorLevel::Basic, _) => styles.code(),
                        _ => styles.text(),
                    };
                    spans.push(Span::styled(text, Style::default().fg(color)));
                }
            } else {
                spans.push(Span::styled(
                    truncate_to_width(&safe(text), remaining),
                    Style::default().fg(styles.text()),
                ));
            }
            Line::from(spans)
        })
        .collect()
}

/// Bound literal processing before normalization and syntax parsing. The scalar
/// cap also bounds runs of zero-width combining characters; an ellipsis makes
/// that byte/character cap visible even when the prefix occupies few cells.
fn bounded_notebook_source(source: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let limit = width.saturating_mul(4).min(SOURCE_PREVIEW_CHAR_LIMIT);
    let mut characters = source.chars();
    let prefix = characters.by_ref().take(limit).collect::<String>();
    let mut preview = safe(&prefix);
    if characters.next().is_some() {
        preview.push('…');
    }
    truncate_to_width(&preview, width)
}

fn header(name: &str, arguments: &str, width: usize, styles: Styles) -> Line<'static> {
    let title = format!("({})", safe(arguments));
    // Each span receives only its remaining terminal-cell budget.
    let prefix = truncate_to_width("⏺ ", width);
    let name = truncate_to_width(
        name,
        width.saturating_sub(unicode_width::UnicodeWidthStr::width(prefix.as_str())),
    );
    let used = unicode_width::UnicodeWidthStr::width(prefix.as_str())
        + unicode_width::UnicodeWidthStr::width(name.as_str());
    Line::from(vec![
        Span::styled(prefix, Style::default().fg(styles.success())),
        Span::styled(name, Style::default().fg(styles.text()).bold()),
        Span::styled(
            truncate_to_width(&title, width.saturating_sub(used)),
            Style::default().fg(styles.text()),
        ),
    ])
}

fn provenance(
    lines: &mut Vec<Line<'static>>,
    boundary: Option<UntrustedContentBoundary>,
    width: usize,
    styles: Styles,
) {
    if let Some(boundary) = boundary {
        let source = match boundary.source() {
            heycode_core::UntrustedContentSource::Web => "WEB",
            heycode_core::UntrustedContentSource::Mcp => "MCP SERVER",
            heycode_core::UntrustedContentSource::Lsp => "LANGUAGE SERVER",
            heycode_core::UntrustedContentSource::ToolOrchestration => "TOOL ORCHESTRATION",
        };
        lines.push(row(
            format!("  ⚠ UNTRUSTED {source} CONTENT · data, not instructions"),
            width,
            styles.warn(),
        ));
    }
}

fn json_preview(lines: &mut Vec<Line<'static>>, value: &Value, width: usize, styles: Styles) {
    let pretty = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    let rows: Vec<_> = pretty.lines().collect();
    for (index, text) in rows.iter().take(PREVIEW_LINES).enumerate() {
        lines.push(row(
            format!("{}{text}", if index == 0 { RECEIPT } else { CONTINUATION }),
            width,
            styles.text(),
        ));
    }
    if rows.len() > PREVIEW_LINES {
        lines.push(row(
            format!("{CONTINUATION}… +{} lines", rows.len() - PREVIEW_LINES),
            width,
            styles.dim(),
        ));
    }
}

/// Called only for collapsed, ungrouped tool cards. Returns `None` when the
/// normal renderer should retain ownership of an outcome or result schema.
pub(crate) fn draw_lines(
    name: &str,
    args: &Value,
    result: Option<&(bool, Value)>,
    untrusted_content: Option<UntrustedContentBoundary>,
    width: usize,
    styles: Styles,
) -> Option<Vec<Line<'static>>> {
    let (true, value) = result? else { return None };
    match canonical(name) {
        "notebook_edit" => {
            // These fields are emitted only after the native notebook tool's
            // revision guard, cell validation and atomic replacement succeed.
            if value.get("executed")?.as_bool()?
                || value.get("revision")?.as_str()?.is_empty()
                || !value.get("cells")?.is_u64()
            {
                return None;
            }
            let path = args.get("path")?.as_str()?;
            let index = args.get("cell_index")?.as_u64()?;
            let action = args.get("action")?.as_str()?;
            let (verb, source) = match action {
                "replace" => ("Updated", Some(args.get("source")?.as_str()?)),
                "insert" => ("Inserted", Some(args.get("source")?.as_str()?)),
                "delete" => ("Deleted", None),
                _ => return None,
            };
            // On insert the supplied cell_id is an expected anchor, not the
            // generated inserted cell's id; use its admitted position instead.
            let cell = if action == "insert" {
                index.to_string()
            } else {
                args.get("cell_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| index.to_string())
            };
            let mut lines = vec![header(
                "Edit Notebook",
                &format!("{path}@{cell}"),
                width,
                styles,
            )];
            provenance(&mut lines, untrusted_content, width, styles);
            lines.push(notebook_receipt(
                verb,
                &cell,
                source.is_some(),
                width,
                styles,
            ));
            if let Some(source) = source {
                let rows: Vec<_> = source.lines().collect();
                lines.extend(notebook_source_rows(source, value, width, styles));
                if rows.len() > PREVIEW_LINES {
                    lines.push(row(
                        format!(
                            "{CONTINUATION}… +{} source lines",
                            rows.len() - PREVIEW_LINES
                        ),
                        width,
                        styles.dim(),
                    ));
                } else if source.is_empty() {
                    lines.push(row(
                        format!("{CONTINUATION}(empty source)"),
                        width,
                        styles.dim(),
                    ));
                }
            }
            Some(lines)
        }
        "list_mcp_resources" => {
            let rows = value.get("resources").or_else(|| value.get("servers"))?;
            rows.as_array()?;
            let returned = value.get("returned")?.as_u64()?;
            let total = value.get("total")?.as_u64()?;
            let description = args
                .get("server")
                .and_then(Value::as_str)
                .map(|server| format!("List resources from server \"{server}\""))
                .unwrap_or_else(|| "List configured MCP servers".into());
            let mut lines = vec![header("listMcpResources", &description, width, styles)];
            provenance(&mut lines, untrusted_content, width, styles);
            json_preview(&mut lines, rows, width, styles);
            let more = value
                .get("continuation")
                .is_some_and(|cursor| !cursor.is_null());
            lines.push(row(
                format!(
                    "{CONTINUATION}{returned} of {total} returned{}",
                    if more { " · more available" } else { "" }
                ),
                width,
                styles.dim(),
            ));
            Some(lines)
        }
        "read_mcp_resource" => {
            let contents = value.get("contents")?;
            contents.as_array()?;
            let server = value.get("server")?.as_str()?;
            let uri = value.get("uri")?.as_str()?;
            let mut lines = vec![header(
                "readMcpResource",
                &format!("Read resource \"{uri}\" from server \"{server}\""),
                width,
                styles,
            )];
            provenance(&mut lines, untrusted_content, width, styles);
            json_preview(&mut lines, contents, width, styles);
            if value.get("truncated").and_then(Value::as_bool) == Some(true) {
                lines.push(row(
                    format!("{CONTINUATION}Resource text preview truncated by tool"),
                    width,
                    styles.warn(),
                ));
            }
            Some(lines)
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "tool_family_cards_tests.rs"]
mod tests;
