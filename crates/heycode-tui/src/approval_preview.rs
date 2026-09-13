//! Source-backed previews for native file-edit approval cards.
//!
//! The approval event itself carries a display-only argument summary.  A
//! structured diff is therefore admitted only when the transcript already
//! contains a complete successful native `read` of the exact raw path at the
//! edit's exact revision.  This module never reads the filesystem and never
//! weakens the edit service's own observation/revision checks.

use crate::app::Item;

const MAX_SOURCE_BYTES: usize = 256 * 1024;
const MAX_EDIT_BYTES: usize = 256 * 1024;
const MAX_PREVIEW_ROWS: usize = 24;
const MAX_ROW_CHARS: usize = 4_096;
const CONTEXT_LINES: usize = 3;

/// Semantic kind of one source-backed edit-preview row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditApprovalRowKind {
    /// Unchanged source surrounding the replacement.
    Context,
    /// A line from the observed source that the edit removes.
    Removed,
    /// A line that the edit proposes to add.
    Added,
}

/// One bounded, terminal-safe source line in an edit approval preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditApprovalRow {
    kind: EditApprovalRowKind,
    old_line: Option<usize>,
    new_line: Option<usize>,
    text: String,
}

impl EditApprovalRow {
    /// Semantic diff kind.
    #[must_use]
    pub const fn kind(&self) -> EditApprovalRowKind {
        self.kind
    }

    /// One-based source line, absent for an added row.
    #[must_use]
    pub const fn old_line(&self) -> Option<usize> {
        self.old_line
    }

    /// One-based proposed line, absent for a removed row.
    #[must_use]
    pub const fn new_line(&self) -> Option<usize> {
        self.new_line
    }

    /// Bounded text with terminal control characters neutralized.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// A diff proven from an earlier complete native read retained in the
/// transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditApprovalPreview {
    path: String,
    rows: Vec<EditApprovalRow>,
}

impl EditApprovalPreview {
    /// Exact raw path shared by the edit arguments and retained read result.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Complete bounded preview rows.
    #[must_use]
    pub fn rows(&self) -> &[EditApprovalRow] {
        &self.rows
    }
}

/// Derive a conservative preview for the native Edit tool at `tool_index`.
///
/// `None` intentionally means "render the existing raw argument preview".
/// It covers missing or partial reads, stale/mismatched revisions, ambiguous
/// replacements, `replace_all`, and payloads that cannot be represented by
/// the bounded preview without omission.
#[must_use]
pub fn edit_preview_for(items: &[Item], tool_index: usize) -> Option<EditApprovalPreview> {
    let Item::Tool {
        name,
        args,
        result: None,
        ..
    } = items.get(tool_index)?
    else {
        return None;
    };
    if canonical_tool_name(name) != "edit" {
        return None;
    }

    let path = args.get("path")?.as_str()?;
    let old = args.get("old_string")?.as_str()?;
    let new = args.get("new_string")?.as_str()?;
    let revision = args.get("expected_revision")?.as_str()?;
    if path.is_empty()
        || path.len() > 4_096
        || old.is_empty()
        || old.len().checked_add(new.len())? > MAX_EDIT_BYTES
        || args
            .get("replace_all")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        || !valid_revision(revision)
    {
        return None;
    }

    let source = items[..tool_index]
        .iter()
        .rev()
        .find_map(|item| complete_read_source(item, path, revision))?;
    if source.matches(old).count() != 1 {
        return None;
    }
    let proposed = source.replacen(old, new, 1);
    let old_lines = source.lines().collect::<Vec<_>>();
    let new_lines = proposed.lines().collect::<Vec<_>>();
    let shared_prefix = old_lines
        .iter()
        .zip(&new_lines)
        .take_while(|(old, new)| old == new)
        .count();
    let suffix_limit = old_lines
        .len()
        .saturating_sub(shared_prefix)
        .min(new_lines.len().saturating_sub(shared_prefix));
    let shared_suffix = old_lines
        .iter()
        .rev()
        .zip(new_lines.iter().rev())
        .take(suffix_limit)
        .take_while(|(old, new)| old == new)
        .count();

    let before_start = shared_prefix.saturating_sub(CONTEXT_LINES);
    let old_change_end = old_lines.len().saturating_sub(shared_suffix);
    let new_change_end = new_lines.len().saturating_sub(shared_suffix);
    let old_after_end = old_change_end
        .saturating_add(CONTEXT_LINES)
        .min(old_lines.len());
    let row_count = shared_prefix
        .saturating_sub(before_start)
        .saturating_add(old_change_end.saturating_sub(shared_prefix))
        .saturating_add(new_change_end.saturating_sub(shared_prefix))
        .saturating_add(old_after_end.saturating_sub(old_change_end));
    if row_count == 0 || row_count > MAX_PREVIEW_ROWS {
        return None;
    }

    let mut rows = Vec::with_capacity(row_count);
    for (index, text) in old_lines
        .iter()
        .enumerate()
        .take(shared_prefix)
        .skip(before_start)
    {
        rows.push(row(
            EditApprovalRowKind::Context,
            Some(index + 1),
            Some(index + 1),
            text,
        )?);
    }
    for (index, text) in old_lines
        .iter()
        .enumerate()
        .take(old_change_end)
        .skip(shared_prefix)
    {
        rows.push(row(
            EditApprovalRowKind::Removed,
            Some(index + 1),
            None,
            text,
        )?);
    }
    for (index, text) in new_lines
        .iter()
        .enumerate()
        .take(new_change_end)
        .skip(shared_prefix)
    {
        rows.push(row(
            EditApprovalRowKind::Added,
            None,
            Some(index + 1),
            text,
        )?);
    }
    for (offset, text) in old_lines
        .iter()
        .enumerate()
        .take(old_after_end)
        .skip(old_change_end)
    {
        let new_index = new_change_end + (offset - old_change_end);
        rows.push(row(
            EditApprovalRowKind::Context,
            Some(offset + 1),
            Some(new_index + 1),
            text,
        )?);
    }
    Some(EditApprovalPreview {
        path: path.to_owned(),
        rows,
    })
}

fn canonical_tool_name(name: &str) -> &str {
    name.strip_prefix("mcp__heycode__").unwrap_or(name)
}

fn valid_revision(revision: &str) -> bool {
    revision.len() == 64
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn complete_read_source(item: &Item, path: &str, revision: &str) -> Option<String> {
    let Item::Tool {
        name,
        args,
        result: Some((true, result)),
        ..
    } = item
    else {
        return None;
    };
    if canonical_tool_name(name) != "read"
        || args.get("path")?.as_str()? != path
        || result.get("path")?.as_str()? != path
        || result.get("revision")?.as_str()? != revision
        || result.get("offset")?.as_u64()? != 1
        || result.get("truncated")?.as_bool()?
        || result.get("partial_last_line")?.as_bool()?
        || result.get("scan_limited")?.as_bool()?
        || !result
            .get("next_offset")
            .is_none_or(serde_json::Value::is_null)
        || !result
            .get("next_byte_offset")
            .is_none_or(serde_json::Value::is_null)
        || !result
            .get("continuation")
            .is_none_or(serde_json::Value::is_null)
        || result.get("lines_remaining")?.as_u64()? != 0
    {
        return None;
    }

    let content = result.get("content")?.as_str()?;
    let lines_returned = usize::try_from(result.get("lines_returned")?.as_u64()?).ok()?;
    let total_lines = usize::try_from(result.get("total_lines")?.as_u64()?).ok()?;
    let total_bytes = usize::try_from(result.get("total_bytes")?.as_u64()?).ok()?;
    let bytes_returned = usize::try_from(result.get("bytes_returned")?.as_u64()?).ok()?;
    if total_bytes > MAX_SOURCE_BYTES
        || bytes_returned != total_bytes
        || lines_returned != total_lines
    {
        return None;
    }

    let mut raw_lines = Vec::with_capacity(lines_returned);
    if !content.is_empty() {
        for (index, encoded) in content.split('\n').enumerate() {
            let (number, text) = encoded.split_once('\t')?;
            if number.trim().parse::<usize>().ok()? != index + 1 {
                return None;
            }
            raw_lines.push(text);
        }
    }
    if raw_lines.len() != lines_returned {
        return None;
    }
    rebuild_source(
        &raw_lines,
        result.get("page_line_ending")?.as_str()?,
        total_bytes,
    )
}

fn rebuild_source(lines: &[&str], ending: &str, total_bytes: usize) -> Option<String> {
    let separator = match ending {
        "lf" => "\n",
        "crlf" => "\r\n",
        "none" if lines.len() <= 1 => "",
        _ => return None,
    };
    let mut source = lines.join(separator);
    match total_bytes.checked_sub(source.len())? {
        0 => {}
        missing if missing == separator.len() && !separator.is_empty() => {
            source.push_str(separator);
        }
        _ => return None,
    }
    (source.len() == total_bytes).then_some(source)
}

fn row(
    kind: EditApprovalRowKind,
    old_line: Option<usize>,
    new_line: Option<usize>,
    text: &str,
) -> Option<EditApprovalRow> {
    if text.chars().count() > MAX_ROW_CHARS {
        return None;
    }
    let mut safe = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\t' => safe.push_str("    "),
            value if value.is_control() => safe.push('\u{fffd}'),
            value => safe.push(value),
        }
    }
    Some(EditApprovalRow {
        kind,
        old_line,
        new_line,
        text: safe,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::app::ToolViewState;
    use serde_json::{Value, json};

    const REVISION: &str = "68e51be1877f35c23c31de14f66f805b4c87ce666d15fdb7556407ba89ade051";

    fn tool(name: &str, args: Value, result: Option<(bool, Value)>) -> Item {
        Item::Tool {
            call_id: None,
            name: name.to_owned(),
            args,
            result,
            untrusted_content: None,
            view: ToolViewState::default(),
        }
    }

    fn complete_read(path: &str, source: &str) -> Item {
        let ending = if source.contains("\r\n") {
            "crlf"
        } else if source.contains('\n') {
            "lf"
        } else {
            "none"
        };
        let content = source
            .lines()
            .enumerate()
            .map(|(index, line)| format!("{:>4}\t{line}", index + 1))
            .collect::<Vec<_>>()
            .join("\n");
        let lines = source.lines().count();
        tool(
            "read",
            json!({"path": path}),
            Some((
                true,
                json!({
                    "path": path,
                    "content": content,
                    "offset": 1,
                    "lines_returned": lines,
                    "lines_remaining": 0,
                    "total_lines": lines,
                    "total_bytes": source.len(),
                    "bytes_returned": source.len(),
                    "revision": REVISION,
                    "page_line_ending": ending,
                    "truncated": false,
                    "next_offset": null,
                    "next_byte_offset": null,
                    "partial_last_line": false,
                    "scan_limited": false,
                    "continuation": null
                }),
            )),
        )
    }

    fn edit(path: &str, old: &str, new: &str) -> Item {
        tool(
            "edit",
            json!({
                "path": path,
                "old_string": old,
                "new_string": new,
                "expected_revision": REVISION
            }),
            None,
        )
    }

    #[test]
    fn exact_complete_read_builds_numbered_context_and_change_rows() {
        let items = vec![
            complete_read("src/config.txt", "alpha\nmode=slow\ngamma\n"),
            edit("src/config.txt", "mode=slow", "mode=fast"),
        ];
        let preview = edit_preview_for(&items, 1).unwrap();
        assert_eq!(preview.path(), "src/config.txt");
        assert_eq!(
            preview.rows(),
            [
                EditApprovalRow {
                    kind: EditApprovalRowKind::Context,
                    old_line: Some(1),
                    new_line: Some(1),
                    text: "alpha".into(),
                },
                EditApprovalRow {
                    kind: EditApprovalRowKind::Removed,
                    old_line: Some(2),
                    new_line: None,
                    text: "mode=slow".into(),
                },
                EditApprovalRow {
                    kind: EditApprovalRowKind::Added,
                    old_line: None,
                    new_line: Some(2),
                    text: "mode=fast".into(),
                },
                EditApprovalRow {
                    kind: EditApprovalRowKind::Context,
                    old_line: Some(3),
                    new_line: Some(3),
                    text: "gamma".into(),
                },
            ]
        );
    }

    #[test]
    fn line_insertions_keep_independent_old_and_new_numbers() {
        let items = vec![
            complete_read("a.txt", "one\ntwo\nthree"),
            edit("a.txt", "two", "two-a\ntwo-b"),
        ];
        let preview = edit_preview_for(&items, 1).unwrap();
        let compact = preview
            .rows()
            .iter()
            .map(|row| (row.kind(), row.old_line(), row.new_line(), row.text()))
            .collect::<Vec<_>>();
        assert_eq!(
            compact,
            vec![
                (EditApprovalRowKind::Context, Some(1), Some(1), "one"),
                (EditApprovalRowKind::Removed, Some(2), None, "two"),
                (EditApprovalRowKind::Added, None, Some(2), "two-a"),
                (EditApprovalRowKind::Added, None, Some(3), "two-b"),
                (EditApprovalRowKind::Context, Some(3), Some(4), "three"),
            ]
        );
    }

    #[test]
    fn partial_stale_ambiguous_replace_all_and_oversized_inputs_fall_back() {
        let base = complete_read("a.txt", "same\nsame\n");
        let mut partial = complete_read("a.txt", "same\n");
        let Item::Tool {
            result: Some((_, value)),
            ..
        } = &mut partial
        else {
            panic!("fixture")
        };
        value["truncated"] = json!(true);

        let cases = [
            vec![partial, edit("a.txt", "same", "new")],
            vec![base.clone(), edit("a.txt", "same", "new")],
            vec![
                base.clone(),
                tool(
                    "edit",
                    json!({"path":"a.txt","old_string":"same","new_string":"new","expected_revision":REVISION,"replace_all":true}),
                    None,
                ),
            ],
            vec![base.clone(), edit("a.txt", "missing", "new")],
            vec![
                base,
                edit("a.txt", "same", &"new\n".repeat(MAX_PREVIEW_ROWS + 1)),
            ],
        ];
        for items in cases {
            assert!(edit_preview_for(&items, 1).is_none());
        }
    }

    #[test]
    fn exact_raw_path_revision_numbering_and_terminal_safety_are_required() {
        let mut read = complete_read("a.txt", "before\nbad\u{1b}[31m\tafter\n");
        let Item::Tool {
            result: Some((_, value)),
            ..
        } = &mut read
        else {
            panic!("fixture")
        };
        value["content"] = json!("   1\tbefore\n   2\tbad\u{1b}[31m\tafter");
        let items = vec![read, edit("a.txt", "bad\u{1b}[31m\tafter", "safe")];
        let preview = edit_preview_for(&items, 1).unwrap();
        assert_eq!(preview.rows()[1].text(), "bad�[31m    after");
        assert!(
            preview
                .rows()
                .iter()
                .all(|row| !row.text().chars().any(char::is_control))
        );

        let wrong_path = vec![complete_read("./a.txt", "old"), edit("a.txt", "old", "new")];
        assert!(edit_preview_for(&wrong_path, 1).is_none());

        let mut wrong_number = complete_read("a.txt", "old");
        let Item::Tool {
            result: Some((_, value)),
            ..
        } = &mut wrong_number
        else {
            panic!("fixture")
        };
        value["content"] = json!("   2\told");
        assert!(edit_preview_for(&[wrong_number, edit("a.txt", "old", "new")], 1).is_none());
    }
}
