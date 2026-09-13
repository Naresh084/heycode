//! Presentation-only grouping; durable calls and their results stay intact.

use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};

use super::{AppState, Item};

fn tool_name(name: &str) -> &str {
    name.strip_prefix("mcp__heycode__").unwrap_or(name)
}

fn eligible(item: &Item) -> bool {
    let Item::Tool {
        name,
        result: Some((true, value)),
        view,
        ..
    } = item
    else {
        return false;
    };
    if view.merged {
        return false;
    }
    match tool_name(name) {
        "read" => true,
        "glob" | "grep" => !value.as_str().is_some_and(|text| {
            text.lines().any(|line| {
                line.starts_with("(Partial search;") || line.starts_with("(Output limited:")
            })
        }),
        "write" | "edit" | "multi_edit" => false,
        "read_many" => value
            .get("files")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|files| {
                !files.is_empty()
                    && files.iter().all(|file| {
                        file.get("status").and_then(serde_json::Value::as_str) == Some("read")
                    })
            }),
        "bash" => !value.as_str().is_some_and(|text| {
            text.lines().any(|line| {
                (line.starts_with("[exit code:") && line != "[exit code: 0]")
                    || line.starts_with("[timed out")
                    || line.starts_with("[killed by signal:")
            })
        }),
        _ => false,
    }
}

fn transparent(item: &Item, show_reasoning: bool) -> bool {
    item.is_lifecycle_diagnostic()
        || item.is_merged_tool()
        || crate::transcript::quiet_orchestration(item)
        || matches!(item, Item::Reasoning { done: true, view, .. }
            if !show_reasoning && view.expanded != Some(true))
}

fn summary(items: &[Item], members: &[usize]) -> String {
    let mut reads = BTreeSet::new();
    let mut changes = BTreeSet::new();
    let mut searches = 0;
    let mut commands = 0;
    let mut added = 0;
    let mut removed = 0;
    for &index in members {
        let Item::Tool {
            name, args, result, ..
        } = &items[index]
        else {
            continue;
        };
        let path = args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("file");
        match tool_name(name) {
            "read" => {
                reads.insert(path);
            }
            "read_many" => {
                if let Some(files) = args.get("files").and_then(serde_json::Value::as_array) {
                    for file in files {
                        if let Some(path) = file.get("path").and_then(serde_json::Value::as_str) {
                            reads.insert(path);
                        }
                    }
                }
            }
            "grep" | "glob" => searches += 1,
            "bash" => commands += 1,
            "write" | "edit" | "multi_edit" => {
                changes.insert(path);
                if let Some(diff) = result
                    .as_ref()
                    .filter(|(_, value)| {
                        tool_name(name) != "multi_edit"
                            && value
                                .get("diff_truncated")
                                .and_then(serde_json::Value::as_bool)
                                != Some(true)
                    })
                    .and_then(|(_, value)| value.get("diff"))
                    .and_then(serde_json::Value::as_str)
                {
                    for line in diff.lines() {
                        added += usize::from(line.starts_with('+') && !line.starts_with("+++"));
                        removed += usize::from(line.starts_with('-') && !line.starts_with("---"));
                    }
                }
            }
            _ => {}
        }
    }
    let plural =
        |count: usize, word: &str| format!("{count} {word}{}", if count == 1 { "" } else { "s" });
    let mut parts = Vec::new();
    if !reads.is_empty() {
        parts.push(format!("Read {}", plural(reads.len(), "file")));
    }
    if searches > 0 {
        parts.push(format!("Searched for {}", plural(searches, "pattern")));
    }
    if !changes.is_empty() {
        let mut changed = format!("Changed {}", plural(changes.len(), "file"));
        if added + removed > 0 {
            changed.push_str(&format!(" (+{added} −{removed})"));
        }
        parts.push(changed);
    }
    if commands > 0 {
        parts.push(format!("Ran {}", plural(commands, "shell command")));
    }
    // The reference reads as one sentence: `Searched for 2 patterns, ran 1
    // shell command`, so only the first clause keeps its capital.
    parts
        .iter()
        .enumerate()
        .map(|(index, part)| {
            if index == 0 {
                part.clone()
            } else {
                let mut characters = part.chars();
                characters.next().map_or_else(String::new, |first| {
                    first.to_lowercase().collect::<String>() + characters.as_str()
                })
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

impl AppState {
    /// Rebuild only when presentation-relevant state changes. Streaming text and
    /// spinner ticks do not walk the historical transcript on every frame.
    pub(crate) fn refresh_tool_groups(&mut self) {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.items.len().hash(&mut hash);
        self.show_reasoning.hash(&mut hash);
        for item in self.items.iter().rev().take(32) {
            std::mem::discriminant(item).hash(&mut hash);
            match item {
                Item::Tool {
                    name, result, view, ..
                } => {
                    name.hash(&mut hash);
                    result.as_ref().map(|(ok, _)| ok).hash(&mut hash);
                    eligible(item).hash(&mut hash);
                    view.expanded.hash(&mut hash);
                    view.merged.hash(&mut hash);
                    view.approval.hash(&mut hash);
                    crate::transcript::quiet_orchestration(item).hash(&mut hash);
                }
                Item::Reasoning { done, view, .. } => {
                    done.hash(&mut hash);
                    view.expanded.hash(&mut hash);
                }
                _ => {}
            }
        }
        let signature = hash.finish();
        if self.tool_group_signature == Some(signature) {
            return;
        }
        self.expand_detailed_transcript();
        self.tool_group_signature = Some(signature);
        group_items(&mut self.items, self.show_reasoning);
        if let Some(parent) = self
            .reasoning_focus
            .and_then(|index| self.items.get(index))
            .and_then(Item::group_parent)
        {
            self.focus_reasoning_item(Some(parent));
        }
        self.transcript_cache.invalidate_layout();
    }
}

/// Shared semantic grouping for main and child conversation projections.
pub(crate) fn group_items(items: &mut [Item], show_reasoning: bool) {
    for item in items.iter_mut() {
        match item {
            Item::Tool { view, .. } => {
                view.spawn_group_children.clear();
                view.group_summary = None;
                view.group_parent = None;
                view.group_hidden = false;
                view.group_details = false;
            }
            Item::Reasoning { view, .. } => {
                view.group_parent = None;
                view.group_hidden = false;
                view.group_details = false;
            }
            _ => {}
        }
    }
    let mut cursor = 0;
    group_spawn_calls(items, show_reasoning);
    while cursor < items.len() {
        if !eligible(&items[cursor]) {
            cursor += 1;
            continue;
        }
        let anchor = cursor;
        let mut members = vec![anchor];
        let mut end = anchor;
        cursor += 1;
        while cursor < items.len() {
            if eligible(&items[cursor]) {
                members.push(cursor);
                end = cursor;
            } else if !transparent(&items[cursor], show_reasoning) {
                break;
            }
            cursor += 1;
        }
        let label = summary(items, &members);
        let expanded = if let Item::Tool { view, .. } = &mut items[anchor] {
            view.group_summary = Some(label);
            view.expanded
        } else {
            false
        };
        let mut start = anchor;
        while start > 0 && transparent(&items[start - 1], show_reasoning) {
            start -= 1;
        }
        for (index, item) in items.iter_mut().enumerate().take(end + 1).skip(start) {
            if index == anchor {
                continue;
            }
            match item {
                Item::Tool { view, .. } if !view.merged => {
                    view.group_parent = Some(anchor);
                    view.group_hidden = !expanded;
                    view.group_details = expanded;
                }
                Item::Reasoning { view, .. } => {
                    view.group_parent = Some(anchor);
                    view.group_hidden = !expanded;
                    view.group_details = expanded;
                }
                _ => {}
            }
        }
    }
}

fn group_spawn_calls(items: &mut [Item], show_reasoning: bool) {
    let mut cursor = 0;
    while cursor < items.len() {
        let anchor = cursor;
        let Item::Tool { view, .. } = &items[anchor] else {
            cursor += 1;
            continue;
        };
        if view.spawn_children.is_empty() || view.expanded {
            cursor += 1;
            continue;
        }
        let mut children = view.spawn_children.clone();
        let mut members = Vec::new();
        cursor += 1;
        while cursor < items.len() {
            match &items[cursor] {
                Item::Tool { view, .. } if !view.spawn_children.is_empty() && !view.expanded => {
                    children.extend(view.spawn_children.clone());
                    members.push(cursor);
                }
                item if transparent(item, show_reasoning) => {}
                _ => break,
            }
            cursor += 1;
        }
        if members.is_empty() {
            continue;
        }
        if let Item::Tool { view, .. } = &mut items[anchor] {
            view.spawn_group_children = children;
        }
        for index in members {
            if let Item::Tool { view, .. } = &mut items[index] {
                view.group_parent = Some(anchor);
                view.group_hidden = true;
            }
        }
    }
}
