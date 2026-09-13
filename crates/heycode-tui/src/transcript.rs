//! Bounded rendered-line cache and indexed transcript viewport.
//!
//! Durable items remain the source of truth. This module keeps only a cheap
//! per-item height index plus at most 256 rendered item fragments, so a
//! 100,000-event session does not become 100,000 styled terminal rows on every
//! redraw. Cache identity includes content, width, reasoning visibility and
//! the active theme generation.

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};

use ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

use crate::app::Item;

const MAX_RENDERED_ENTRIES: usize = 256;
/// Items the exact scroll walk may render before it hands over to the index.
///
/// Every scroll offset that lands inside these newest items is exact — one
/// offset unit is one rendered row — and the cap is what keeps a far offset
/// over a 100,000-event session a bounded amount of work per frame.
const MAX_SCROLL_WALK_ITEMS: usize = 32;
const MAX_PENDING_SESSION_EVENTS: usize = 1_024;

#[derive(Default)]
struct SessionBridgeState {
    subscriber: Option<tokio::sync::mpsc::Sender<heycode_session::SessionEvent>>,
    lagged: bool,
}

#[derive(Clone, Copy)]
enum SessionDelivery {
    Sent,
    Full,
    Closed,
}

/// Effect-listener target shared by the TUI plugin and its running handle.
#[derive(Clone, Default)]
pub(crate) struct SessionEventBridge {
    state: std::sync::Arc<std::sync::Mutex<SessionBridgeState>>,
}

impl SessionEventBridge {
    pub(crate) fn publish(&self, event: &heycode_session::SessionEvent) {
        let mut state = self.lock();
        if state.lagged {
            return;
        }
        let outcome =
            state
                .subscriber
                .as_ref()
                .map(|subscriber| match subscriber.try_send(event.clone()) {
                    Ok(()) => SessionDelivery::Sent,
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => SessionDelivery::Full,
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                        SessionDelivery::Closed
                    }
                });
        match outcome {
            Some(SessionDelivery::Full) => {
                state.lagged = true;
            }
            Some(SessionDelivery::Closed) => {
                state.subscriber = None;
            }
            _ => {}
        }
    }

    pub(crate) fn subscribe(&self) -> tokio::sync::mpsc::Receiver<heycode_session::SessionEvent> {
        let (sender, receiver) = tokio::sync::mpsc::channel(MAX_PENDING_SESSION_EVENTS);
        let mut state = self.lock();
        state.subscriber = Some(sender);
        state.lagged = false;
        receiver
    }

    pub(crate) fn take_lagged(&self) -> bool {
        let mut state = self.lock();
        std::mem::take(&mut state.lagged)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SessionBridgeState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Cumulative cache work counters used by the U21 performance acceptance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TranscriptCacheMetrics {
    /// Item fragments actually rendered after a cache miss.
    pub rendered_items: usize,
    /// Item fragments served from the bounded cache.
    pub cache_hits: usize,
    /// Current retained rendered fragments.
    pub retained_entries: usize,
    /// Items visited while building the current cheap height index.
    pub indexed_items: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RenderKey {
    fingerprint: u64,
    neighbors: crate::render::ItemNeighbors,
    width: usize,
    show_reasoning: bool,
    style_generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LayoutKey {
    width: usize,
    show_reasoning: bool,
}

#[derive(Default)]
struct LayoutIndex {
    key: Option<LayoutKey>,
    prefix_lines: Vec<usize>,
}

/// One AppState-owned bounded cache.
#[derive(Default)]
pub struct TranscriptRenderCache {
    rendered: HashMap<RenderKey, Vec<Line<'static>>>,
    recency: VecDeque<RenderKey>,
    layout: LayoutIndex,
    metrics: TranscriptCacheMetrics,
    pub(crate) visible_rows: Vec<(usize, usize)>,
    pub(crate) last_scroll: usize,
    pub(crate) last_width: usize,
}

/// One immutable viewport request.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ViewportSpec {
    pub(crate) width: usize,
    pub(crate) visible_lines: usize,
    pub(crate) scroll_from_bottom: usize,
    pub(crate) show_reasoning: bool,
    pub(crate) style_generation: u64,
}

impl TranscriptRenderCache {
    pub(crate) fn invalidate_layout(&mut self) {
        self.layout.key = None;
    }

    pub(crate) fn scroll_for_row<F>(
        &mut self,
        items: &[Item],
        spec: ViewportSpec,
        index: usize,
        local_row: usize,
        screen_row: usize,
        mut render: F,
    ) -> usize
    where
        F: FnMut(&Item, crate::render::ItemNeighbors) -> Vec<Line<'static>>,
    {
        let rows = (index..items.len())
            .map(|position| {
                self.rendered_item(
                    items,
                    position,
                    spec.width,
                    spec.show_reasoning,
                    spec.style_generation,
                    &mut render,
                )
                .len()
            })
            .sum::<usize>();
        rows.saturating_sub(local_row)
            .saturating_sub(spec.visible_lines.saturating_sub(screen_row))
    }

    /// Current cumulative work counters.
    #[must_use]
    pub fn metrics(&self) -> TranscriptCacheMetrics {
        TranscriptCacheMetrics {
            retained_entries: self.rendered.len(),
            ..self.metrics
        }
    }

    /// Render only the line window requested from the bottom of the transcript.
    /// `usize::MAX` is the explicit oldest-position sentinel.
    ///
    /// The offset is counted in *rendered* rows, not indexed rows: the walk
    /// drops real rows off the bottom, so one offset unit moves the screen by
    /// exactly one row for the newest `MAX_SCROLL_WALK_ITEMS` items. Deriving
    /// the window from the height index instead is what used to make the first
    /// couple of screens of scroll offset do nothing at all — the index
    /// over-counts on purpose, so an absolute line taken from it lands past
    /// the end of the real rows and the window snaps back to the bottom.
    /// Older offsets fall back to the index for the jump. The two properties
    /// that are load-bearing there are monotonicity — the window never moves
    /// back down as the offset grows — and top-reachability. Rate is not one
    /// of them: the jump skips `remaining` *bound* lines, and because the
    /// bound over-counts, skipping N bound lines skips fewer than N rendered
    /// rows, so past the walk the window advances less far than the offset
    /// asked for (measured at roughly one row per five offset units on a
    /// markdown corpus). Reasoning the other way round — "an over-counting
    /// bound moves us further" — is what produced the dead-offset defect.
    pub(crate) fn viewport<F>(
        &mut self,
        items: &[Item],
        spec: ViewportSpec,
        mut render_item: F,
    ) -> Vec<Line<'static>>
    where
        F: FnMut(&Item, crate::render::ItemNeighbors) -> Vec<Line<'static>>,
    {
        self.last_scroll = spec.scroll_from_bottom;
        self.last_width = spec.width;
        self.visible_rows.clear();
        if items.is_empty() || spec.visible_lines == 0 {
            return Vec::new();
        }
        self.ensure_layout(items, spec.width, spec.show_reasoning);

        // Drop `scroll_from_bottom` real rows off the bottom of the
        // transcript. Follow mode drops nothing and so needs no index at all,
        // which is what stops an item drawn taller than its budget from
        // pushing the newest content off the bottom of the screen.
        let mut boundary = items.len();
        let mut dropped = 0usize;
        let mut walked = 0usize;
        let mut partial: Vec<Line<'static>> = Vec::new();
        while dropped < spec.scroll_from_bottom && boundary > 0 && walked < MAX_SCROLL_WALK_ITEMS {
            boundary -= 1;
            walked = walked.saturating_add(1);
            let lines = self.rendered_item(
                items,
                boundary,
                spec.width,
                spec.show_reasoning,
                spec.style_generation,
                &mut render_item,
            );
            let remaining = spec.scroll_from_bottom.saturating_sub(dropped);
            if lines.len() <= remaining {
                dropped = dropped.saturating_add(lines.len());
            } else {
                // The offset ends inside this item: its top rows are the
                // bottom of the window.
                partial = lines
                    .iter()
                    .take(lines.len().saturating_sub(remaining))
                    .cloned()
                    .collect();
                self.visible_rows = (0..partial.len()).map(|row| (boundary, row)).collect();
                dropped = spec.scroll_from_bottom;
            }
        }
        if dropped < spec.scroll_from_bottom && boundary > 0 {
            // Past the exact walk the cheap index does the jumping. It
            // over-counts, so the jump skips at least the rows asked for, and
            // skipping at least one more item than the walk already covered
            // keeps offsets past the walk strictly older than offsets inside
            // it.
            let remaining = spec.scroll_from_bottom.saturating_sub(dropped);
            let base = self
                .layout
                .prefix_lines
                .get(boundary.saturating_sub(1))
                .copied()
                .unwrap_or(0);
            let target = base.saturating_sub(remaining);
            boundary = self
                .layout
                .prefix_lines
                .partition_point(|line| *line <= target)
                .min(boundary.saturating_sub(1));
        }
        // Rows of `items[boundary]` the offset left on screen.
        let boundary_rows = partial.len();
        let mut viewport = partial;
        if viewport.len() > spec.visible_lines {
            // One item taller than the whole window: keep its bottom rows.
            let start = viewport.len().saturating_sub(spec.visible_lines);
            viewport = viewport.split_off(start);
            self.visible_rows = self.visible_rows.split_off(start);
        }

        // Fill backwards from the boundary with real rows, so a bound that
        // over-counted never leaves the viewport short.
        let mut preceding_index = boundary;
        while viewport.len() < spec.visible_lines && preceding_index > 0 {
            preceding_index -= 1;
            let lines = self.rendered_item(
                items,
                preceding_index,
                spec.width,
                spec.show_reasoning,
                spec.style_generation,
                &mut render_item,
            );
            let missing = spec.visible_lines.saturating_sub(viewport.len());
            let from = lines.len().saturating_sub(missing);
            let mut prefix = lines.iter().skip(from).cloned().collect::<Vec<_>>();
            let mut origins: Vec<_> = (from..lines.len())
                .map(|row| (preceding_index, row))
                .collect();
            origins.append(&mut self.visible_rows);
            self.visible_rows = origins;
            prefix.append(&mut viewport);
            viewport = prefix;
        }
        if viewport.len() < spec.visible_lines {
            // The walk ran out of transcript above the window, so the offset
            // scrolled past the top: show the oldest rows rather than a
            // half-empty screen. Nothing was truncated above to get here, so
            // the first `boundary_rows` rows of `items[boundary]` are exactly
            // the ones already in the viewport.
            let mut following_index = boundary;
            let mut skip = boundary_rows;
            while viewport.len() < spec.visible_lines && following_index < items.len() {
                let lines = self.rendered_item(
                    items,
                    following_index,
                    spec.width,
                    spec.show_reasoning,
                    spec.style_generation,
                    &mut render_item,
                );
                let missing = spec.visible_lines.saturating_sub(viewport.len());
                self.visible_rows.extend(
                    (skip..lines.len().min(skip.saturating_add(missing)))
                        .map(|row| (following_index, row)),
                );
                viewport.extend(lines.iter().skip(skip).take(missing).cloned());
                skip = 0;
                following_index = following_index.saturating_add(1);
            }
        }
        while viewport.last().is_some_and(|line| line.spans.is_empty()) {
            viewport.pop();
            self.visible_rows.pop();
        }
        viewport
    }

    fn ensure_layout(&mut self, items: &[Item], width: usize, show_reasoning: bool) {
        let key = LayoutKey {
            width,
            show_reasoning,
        };
        if self.layout.key == Some(key) && self.layout.prefix_lines.len() == items.len() {
            return;
        }
        let append_from =
            if self.layout.key == Some(key) && self.layout.prefix_lines.len() < items.len() {
                // Appending a receipt changes the *previous* item's height by
                // taking away its trailing blank row, so the incremental walk
                // always re-measures the item it is extending.
                let indexed = self.layout.prefix_lines.len();
                self.layout.prefix_lines.pop();
                indexed.saturating_sub(1)
            } else {
                self.layout.prefix_lines.clear();
                self.layout.prefix_lines.reserve(items.len());
                0
            };
        let mut total = self.layout.prefix_lines.last().copied().unwrap_or(0);
        for index in append_from..items.len() {
            total = total.saturating_add(height_bound(
                &items[index],
                crate::render::item_neighbors(items, index),
                width,
                show_reasoning,
            ));
            self.layout.prefix_lines.push(total);
        }
        self.metrics.indexed_items = self
            .metrics
            .indexed_items
            .saturating_add(items.len().saturating_sub(append_from));
        self.layout.key = Some(key);
    }

    fn rendered_item<F>(
        &mut self,
        items: &[Item],
        index: usize,
        width: usize,
        show_reasoning: bool,
        style_generation: u64,
        render_item: &mut F,
    ) -> Vec<Line<'static>>
    where
        F: FnMut(&Item, crate::render::ItemNeighbors) -> Vec<Line<'static>>,
    {
        let item = &items[index];
        // A command echo and its receipt render as one block, so cache
        // identity has to include the neighbours that decide the pairing.
        let neighbors = crate::render::item_neighbors(items, index);
        let key = RenderKey {
            fingerprint: fingerprint(item),
            neighbors,
            width,
            show_reasoning,
            style_generation,
        };
        if let Some(lines) = self.rendered.get(&key).cloned() {
            self.metrics.cache_hits = self.metrics.cache_hits.saturating_add(1);
            self.touch(key);
            return lines;
        }
        let lines = render_item(item, neighbors);
        self.metrics.rendered_items = self.metrics.rendered_items.saturating_add(1);
        self.rendered.insert(key, lines.clone());
        self.touch(key);
        while self.recency.len() > MAX_RENDERED_ENTRIES {
            if let Some(oldest) = self.recency.pop_front() {
                self.rendered.remove(&oldest);
            }
        }
        lines
    }

    fn touch(&mut self, key: RenderKey) {
        self.recency.retain(|candidate| *candidate != key);
        self.recency.push_back(key);
    }
}

/// Upper bound on the rows `crate::render::render_transcript_item` produces.
///
/// The viewport picks which item to start drawing at from this index and then
/// fills forward with real rows, so an item drawn *taller* than its bound
/// silently pushes the newest content off the bottom of the screen. The index
/// is cheap on purpose — it runs over every item in a 100,000-event session —
/// so exactness is not the contract; never returning less than the renderer
/// does is. Each arm therefore counts the renderer's worst case, and the
/// per-view budgets live next to the views that spend them
/// (`crate::render::tool_view_height_bound`,
/// `crate::markdown::render_markdown_height_bound`).
///
/// `crate::transcript::tests::every_item_variant_renders_within_its_height_bound`
/// pins the contract for every variant, so a renderer that grows fails there.
pub(crate) fn height_bound(
    item: &Item,
    neighbors: crate::render::ItemNeighbors,
    width: usize,
    show_reasoning: bool,
) -> usize {
    if item.renders_no_rows() {
        return 0;
    }
    if matches!(item, Item::Tool { view, .. } if view.group_summary.is_some() && !view.expanded) {
        return 2;
    }
    let width = width.max(1);
    let wrapped = |text: &str| {
        text.lines()
            .map(|line| line.width().max(1).div_ceil(width))
            .sum::<usize>()
            .max(1)
    };
    match item {
        // A receipt spends five cells on its leader and word-wraps what is
        // left, so a character estimate under-counts it. Receipts are one row
        // group per command, so the exact projection is affordable here for
        // the same reason it is for a compaction card.
        Item::Info(_) | Item::Error(_) if neighbors.after_command => {
            crate::render::render_transcript_item(
                item,
                neighbors,
                width,
                show_reasoning,
                crate::terminal::Styles::default(),
            )
            .len()
        }
        // One row per attachment, and never fewer than the empty case.
        Item::Attachments { attachments, .. } => attachments.len().max(1),
        Item::AudioOutput { attachments } => attachments.len().max(1),
        // One row per source row, plus the trailing blank. The command band
        // keeps a trailing cell of its own, so it wraps one column earlier
        // than a user line does.
        Item::User(text) | Item::Command(text) => {
            let content_width = width.saturating_sub(3).max(1);
            text.lines()
                .map(|line| line.width().max(1).div_ceil(content_width))
                .sum::<usize>()
                .max(1)
                .saturating_add(1)
        }
        Item::Assistant(text) => {
            crate::markdown::render_markdown_height_bound(text, width.saturating_sub(2).max(1))
                .saturating_add(1)
        }
        // Live reasoning includes a two-row preview; settled blocks collapse.
        Item::Reasoning { view, done, .. }
            if !view.expanded.unwrap_or(show_reasoning) && !view.group_details =>
        {
            if *done {
                1
            } else {
                3
            }
        }
        // Header, up to `wrapped` body rows, and the trailing blank.
        Item::Reasoning { text, .. } => text
            .lines()
            .map(|line| line.width().max(1).div_ceil(width.saturating_sub(5).max(1)))
            .sum::<usize>()
            .max(1)
            .saturating_add(2),
        Item::Tool {
            name,
            args,
            result,
            untrusted_content,
            view,
            ..
        } => if !view.spawn_tree().is_empty() && !view.expanded {
            view.spawn_tree().len().saturating_mul(2).saturating_add(2)
        } else if view.expanded || view.group_details {
            crate::render::expanded_tool_height_bound(args, result.as_ref(), width).saturating_add(
                view.retrieved_output
                    .values()
                    .map(|text| {
                        text.lines()
                            .map(|line| {
                                crate::render::safe_tool_line(line)
                                    .width()
                                    .max(1)
                                    .div_ceil(width.saturating_sub(2).max(1))
                            })
                            .sum::<usize>()
                            + 64_usize.div_ceil(width.saturating_sub(2).max(1))
                    })
                    .sum::<usize>(),
            )
        } else {
            crate::render::tool_view_height_bound(
                name,
                args,
                result.as_ref(),
                untrusted_content.is_some(),
                width,
            )
        }
        .saturating_add(1 + usize::from(view.group_summary.is_some())),
        Item::ProviderState { .. } => 1,
        // Header, optional output-count and error rows, one row per source,
        // and the trailing blank.
        Item::ServerTool { result, .. } => result
            .as_ref()
            .map_or(2, |result| 4_usize.saturating_add(result.sources().len())),
        Item::ServerToolUsage { .. } => 1,
        Item::Citation { cited_text, .. } => {
            1_usize.saturating_add(usize::from(cited_text.is_some()))
        }
        Item::FindingsReport {
            report,
            expanded,
            focused,
        } => crate::render::findings_report_height_bound(report, *expanded, *focused, width),
        // Compaction receipts change only on explicit disclosure. Use the exact
        // word-wrapped projection so long summaries cannot under-budget the index.
        Item::Compaction { .. } => crate::render::render_transcript_item(
            item,
            neighbors,
            width,
            show_reasoning,
            crate::terminal::Styles::default(),
        )
        .len(),
        Item::RuntimeLink { .. } | Item::RouteChange { .. } | Item::PlanMode { .. } => 1,
        Item::Goal { objective, .. } => {
            1_usize.saturating_add(objective.as_deref().map_or(0, wrapped))
        }
        Item::Workflow { .. } | Item::Schedule { .. } => 1,
        // A notice never follows an echo, and an error that does not either,
        // so both keep the two-cell glyph gutter.
        Item::Error(text) | Item::Notice(text) => {
            let safe = crate::markdown::terminal_safe_span(text);
            let content_width = width.saturating_sub(2).max(1);
            safe.lines()
                .map(|line| {
                    let mut rows = 1usize;
                    let mut used = 0usize;
                    for character in line.chars() {
                        let character_width =
                            unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
                        if used > 0 && used.saturating_add(character_width) > content_width {
                            rows += 1;
                            used = 0;
                        }
                        used = used.saturating_add(character_width);
                    }
                    rows
                })
                .sum::<usize>()
                .max(1)
                .saturating_add(1)
        }
        Item::Info(text) => wrapped(text).saturating_add(1),
    }
}

fn fingerprint(item: &Item) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::mem::discriminant(item).hash(&mut hasher);
    match item {
        Item::User(text)
        | Item::Command(text)
        | Item::Assistant(text)
        | Item::Info(text)
        | Item::Notice(text)
        | Item::Error(text) => text.hash(&mut hasher),
        Item::Attachments {
            attachments,
            document_routes,
        } => {
            attachments.len().hash(&mut hasher);
            document_routes.len().hash(&mut hasher);
            for attachment in attachments {
                attachment.content_id().as_str().hash(&mut hasher);
            }
        }
        Item::AudioOutput { attachments } => {
            attachments.len().hash(&mut hasher);
            for attachment in attachments {
                attachment.content_id().as_str().hash(&mut hasher);
            }
        }
        Item::Reasoning { text, done, view } => {
            view.group_hidden.hash(&mut hasher);
            view.group_parent.hash(&mut hasher);
            view.group_details.hash(&mut hasher);
            text.hash(&mut hasher);
            done.hash(&mut hasher);
            view.expanded.hash(&mut hasher);
            view.elapsed_seconds.hash(&mut hasher);
            view.interrupted.hash(&mut hasher);
            view.focused.hash(&mut hasher);
        }
        Item::Tool {
            call_id,
            name,
            args,
            result,
            untrusted_content,
            view,
        } => {
            view.completed_job.hash(&mut hasher);
            view.completed_agent_label.hash(&mut hasher);
            for child in view.spawn_tree() {
                child.key.0.hash(&mut hasher);
                child.label.hash(&mut hasher);
                child.status.label().hash(&mut hasher);
                child.telemetry.current_tool.hash(&mut hasher);
            }
            view.group_summary.hash(&mut hasher);
            view.group_parent.hash(&mut hasher);
            view.group_hidden.hash(&mut hasher);
            view.group_details.hash(&mut hasher);
            view.retrieved_output.hash(&mut hasher);
            view.merged.hash(&mut hasher);
            view.expanded.hash(&mut hasher);
            view.approval.hash(&mut hasher);
            view.focused.hash(&mut hasher);
            call_id
                .as_ref()
                .map(heycode_core::CallId::as_str)
                .hash(&mut hasher);
            name.hash(&mut hasher);
            args.to_string().hash(&mut hasher);
            result
                .as_ref()
                .map(|(ok, value)| (*ok, value.to_string()))
                .hash(&mut hasher);
            untrusted_content
                .map(|boundary| format!("{:?}", boundary.source()))
                .hash(&mut hasher);
        }
        Item::ProviderState {
            provider,
            model,
            protocol,
            kind,
            output_index,
        } => {
            provider.hash(&mut hasher);
            model.hash(&mut hasher);
            protocol.hash(&mut hasher);
            kind.hash(&mut hasher);
            output_index.hash(&mut hasher);
        }
        Item::ServerTool {
            call_id,
            logical,
            provider_name,
            result,
        } => {
            call_id.as_str().hash(&mut hasher);
            logical.hash(&mut hasher);
            provider_name.hash(&mut hasher);
            result
                .as_ref()
                .map(|value| format!("{value:?}"))
                .hash(&mut hasher);
        }
        Item::ServerToolUsage {
            logical,
            requests,
            cost,
        } => {
            logical.hash(&mut hasher);
            requests.hash(&mut hasher);
            cost.hash(&mut hasher);
        }
        Item::Citation {
            url,
            title,
            cited_text,
            start_index,
            end_index,
        } => {
            url.hash(&mut hasher);
            title.hash(&mut hasher);
            cited_text.hash(&mut hasher);
            start_index.hash(&mut hasher);
            end_index.hash(&mut hasher);
        }
        Item::FindingsReport {
            report,
            expanded,
            focused,
        } => {
            report.id().as_str().hash(&mut hasher);
            report.source().workspace_revision().hash(&mut hasher);
            report.findings().len().hash(&mut hasher);
            for finding in report.findings() {
                finding.id().hash(&mut hasher);
                finding.revision().hash(&mut hasher);
            }
            expanded.hash(&mut hasher);
            focused.hash(&mut hasher);
        }
        Item::Compaction {
            native,
            strategy,
            replaced_upto_seq,
            summary,
            provider_items,
            expanded,
            focused,
        } => {
            native.hash(&mut hasher);
            strategy.hash(&mut hasher);
            replaced_upto_seq.hash(&mut hasher);
            summary.hash(&mut hasher);
            provider_items.hash(&mut hasher);
            expanded.hash(&mut hasher);
            focused.hash(&mut hasher);
        }
        Item::RuntimeLink { runtime } => runtime.hash(&mut hasher),
        Item::RouteChange { provider, model } => {
            provider.hash(&mut hasher);
            model.hash(&mut hasher);
        }
        Item::PlanMode { active } => active.hash(&mut hasher),
        Item::Goal {
            action,
            phase,
            objective,
            revision,
        } => {
            action.hash(&mut hasher);
            phase.hash(&mut hasher);
            objective.hash(&mut hasher);
            revision.hash(&mut hasher);
        }
        Item::Workflow { action, summary } => {
            action.hash(&mut hasher);
            summary.hash(&mut hasher);
        }
        Item::Schedule { action, summary } => {
            action.hash(&mut hasher);
            summary.hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// Quiet only recognized operational receipts. The item and exact payload stay
/// retained so explicit transcript expansion is always reversible.
pub(crate) fn quiet_orchestration(item: &Item) -> bool {
    let Item::Tool {
        name,
        args,
        result: Some((true, value)),
        view,
        ..
    } = item
    else {
        return false;
    };
    // Only deliberate raw-history disclosure exposes operational metadata.
    // Successful inspections may report a failed child: the inspection itself
    // is still routine, and the child's attributed settlement owns that alert.
    if view.expanded
        || view.approval.as_deref().is_some_and(|approval| {
            approval.starts_with("awaiting approval") || approval.starts_with("rejected")
        })
    {
        return false;
    }
    match name.strip_prefix("mcp__heycode__").unwrap_or(name) {
        "list_agents" | "list_jobs" => true,
        "agent_control" => matches!(
            args.get("action").and_then(serde_json::Value::as_str),
            Some("list" | "wait")
        ),
        "job_control" => args.get("action").and_then(serde_json::Value::as_str) == Some("list"),
        "interrupt_task" => args.get("action").and_then(serde_json::Value::as_str) == Some("wait"),
        "ask_user_question_async" => {
            value.get("status").and_then(serde_json::Value::as_str) == Some("pending")
                && (value
                    .get("question_id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|id| !id.is_empty())
                    || value
                        .get("question_ids")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|ids| {
                            !ids.is_empty()
                                && ids
                                    .iter()
                                    .all(|id| id.as_str().is_some_and(|id| !id.is_empty()))
                        }))
        }
        _ => false,
    }
}

/// A successful inspection can still report work that needs a human decision.
/// Keep those receipts visible, including legacy nested snapshots.
pub(crate) fn orchestration_needs_attention(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(values) => values.iter().any(orchestration_needs_attention),
        serde_json::Value::Object(fields) => fields.iter().any(|(key, value)| {
            (matches!(key.as_str(), "error" | "failure") && !value.is_null() && value != "")
                || (matches!(key.as_str(), "needs_attention" | "requires_approval")
                    && value.as_bool() == Some(true))
                || (matches!(key.as_str(), "status" | "state" | "phase" | "Settled")
                    && value.as_str().is_some_and(|status| {
                        matches!(
                            status.to_ascii_lowercase().as_str(),
                            "failed"
                                | "error"
                                | "interrupted"
                                | "cancelled"
                                | "canceled"
                                | "cancelling"
                                | "canceling"
                                | "awaiting_approval"
                                | "waiting_for_approval"
                                | "needs_attention"
                                | "waiting_for_user"
                                | "waiting_for_question"
                        )
                    }))
                || orchestration_needs_attention(value)
        }),
        _ => false,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// The variant's own name, from an exhaustive match: a new `Item` arm
    /// stops this compiling, and `sample_items` below is then missing a
    /// sample, which fails the height-bound contract test rather than
    /// silently leaving the new renderer unpinned.
    fn variant_name(item: &Item) -> &'static str {
        match item {
            Item::User(_) => "User",
            Item::Command(_) => "Command",
            Item::Attachments { .. } => "Attachments",
            Item::AudioOutput { .. } => "AudioOutput",
            Item::Assistant(_) => "Assistant",
            Item::Reasoning { .. } => "Reasoning",
            Item::Tool { .. } => "Tool",
            Item::ProviderState { .. } => "ProviderState",
            Item::ServerTool { .. } => "ServerTool",
            Item::ServerToolUsage { .. } => "ServerToolUsage",
            Item::Citation { .. } => "Citation",
            Item::FindingsReport { .. } => "FindingsReport",
            Item::Compaction { .. } => "Compaction",
            Item::RuntimeLink { .. } => "RuntimeLink",
            Item::RouteChange { .. } => "RouteChange",
            Item::PlanMode { .. } => "PlanMode",
            Item::Goal { .. } => "Goal",
            Item::Workflow { .. } => "Workflow",
            Item::Schedule { .. } => "Schedule",
            Item::Info(_) => "Info",
            Item::Notice(_) => "Notice",
            Item::Error(_) => "Error",
        }
    }

    /// Both neighbour states every item must stay within its bound for.
    const NEIGHBOR_CASES: [crate::render::ItemNeighbors; 3] = [
        crate::render::ItemNeighbors {
            after_command: false,
            before_receipt: false,
        },
        crate::render::ItemNeighbors {
            after_command: true,
            before_receipt: false,
        },
        crate::render::ItemNeighbors {
            after_command: false,
            before_receipt: true,
        },
    ];

    const ALL_VARIANTS: [&str; 22] = [
        "User",
        "Command",
        "Attachments",
        "AudioOutput",
        "Assistant",
        "Reasoning",
        "Tool",
        "ProviderState",
        "ServerTool",
        "ServerToolUsage",
        "Citation",
        "FindingsReport",
        "Compaction",
        "RuntimeLink",
        "RouteChange",
        "PlanMode",
        "Goal",
        "Workflow",
        "Schedule",
        "Info",
        "Notice",
        "Error",
    ];

    fn many(prefix: &str, rows: usize) -> String {
        (0..rows)
            .map(|row| format!("{prefix}-{row} some words that force greedy wrapping"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn tool(name: &str, value: serde_json::Value, untrusted: bool) -> Item {
        Item::Tool {
            view: Default::default(),
            call_id: None,
            name: name.to_owned(),
            args: serde_json::json!({
                "command": "ls -la",
                "path": "/workspace/file.rs",
                "pattern": "needle",
                "url": "https://example.invalid/page",
            }),
            result: Some((true, value)),
            untrusted_content: untrusted.then(heycode_core::UntrustedContentBoundary::mcp),
        }
    }

    #[test]
    fn quiet_orchestration_retains_expansion_errors_approval_and_exact_payloads() {
        let payload = serde_json::json!({"agents":[{"label":"Atlas","status":"running"}],"budget":{"remaining":17}});
        for name in [
            "list_agents",
            "mcp__heycode__list_agents",
            "list_jobs",
            "agent_control",
            "job_control",
            "interrupt_task",
            "ask_user_question_async",
        ] {
            let mut item = tool(name, payload.clone(), false);
            if let Item::Tool { args, result, .. } = &mut item {
                *args = serde_json::json!({"action": if name == "interrupt_task" { "wait" } else { "list" }});
                if name == "ask_user_question_async" {
                    *result = Some((
                        true,
                        serde_json::json!({"question_id":"retained-question-id","status":"pending","instruction":"model-only instruction"}),
                    ));
                }
            }
            let exact = if let Item::Tool { result, .. } = &item {
                result.clone()
            } else {
                unreachable!()
            };
            assert!(super::quiet_orchestration(&item), "{name}");
            assert_eq!(super::height_bound(&item, Default::default(), 60, false), 0);
            assert!(
                crate::render::render_transcript_item(
                    &item,
                    Default::default(),
                    60,
                    false,
                    Default::default()
                )
                .is_empty()
            );
            if let Item::Tool { view, .. } = &mut item {
                view.expanded = true;
            }
            assert!(!super::quiet_orchestration(&item));
            let rows = crate::render::render_transcript_item(
                &item,
                Default::default(),
                110,
                false,
                Default::default(),
            );
            assert!(
                rows.iter()
                    .any(|line| line.to_string().contains("Arguments:")),
                "{name}"
            );
            assert!(
                rows.iter().any(|line| line.to_string().contains(name)),
                "exact tool name: {name}"
            );
            if let Item::Tool { view, result, .. } = &mut item {
                assert_eq!(&exact, result);
                view.expanded = false;
                view.approval = Some("awaiting approval".into());
            }
            assert!(!super::quiet_orchestration(&item));
            if let Item::Tool { view, result, .. } = &mut item {
                view.approval = None;
                *result = Some((false, serde_json::json!({"message":"inspection failed"})));
            }
            assert!(!super::quiet_orchestration(&item));
        }
        for status in [
            "failed",
            "interrupted",
            "cancelled",
            "needs_attention",
            "waiting_for_approval",
        ] {
            let item = tool(
                "list_agents",
                serde_json::json!({"agents":[{"status":status}]}),
                false,
            );
            assert!(super::quiet_orchestration(&item), "{status}");
        }
        for state in [
            serde_json::json!("Cancelling"),
            serde_json::json!({"Settled":"Failed"}),
            serde_json::json!({"Settled":"Cancelled"}),
            serde_json::json!({"Settled":"Interrupted"}),
        ] {
            assert!(super::quiet_orchestration(&tool(
                "list_jobs",
                serde_json::json!({"jobs":[{"state":state}]}),
                false
            )));
        }
        let item = tool("mcp__other__list_agents", payload, false);
        assert!(!super::quiet_orchestration(&item));
        assert!(!super::quiet_orchestration(&Item::User(
            "[job job-1 completed] human lookalike".into()
        )));
    }

    #[test]
    fn matched_successful_agent_completions_are_named_receipts() {
        let mut item = tool(
            "job",
            serde_json::json!("[job job-1 completed]\nchild result"),
            false,
        );
        if let Item::Tool { view, .. } = &mut item {
            view.completed_job = Some("job-1".into());
        }
        assert!(!super::quiet_orchestration(&item));
        if let Item::Tool { view, .. } = &mut item {
            view.completed_agent_label = Some("Atlas".into());
        }
        assert!(!super::quiet_orchestration(&item));
        let rows = crate::render::render_transcript_item(
            &item,
            Default::default(),
            80,
            false,
            Default::default(),
        );
        assert!(
            rows.iter()
                .any(|row| row.to_string().contains("○ Atlas completed"))
        );
        if let Item::Tool { result, .. } = &mut item {
            result.as_mut().unwrap().0 = false;
        }
        assert!(!super::quiet_orchestration(&item));
    }

    fn sample_items() -> Vec<Item> {
        let attachment = heycode_core::AttachmentMetadata::new(
            heycode_core::AttachmentContentId::from_sha256([0x21; 32]),
            heycode_core::AttachmentMediaType::new("image/png").unwrap(),
            4,
            Some("shot.png".to_owned()),
            Some(heycode_core::AttachmentDimensions::new(1, 1).unwrap()),
        )
        .unwrap();
        let audio = heycode_core::AttachmentMetadata::new_audio(
            heycode_core::AttachmentContentId::from_sha256([0x22; 32]),
            heycode_core::AttachmentMediaType::new("audio/wav").unwrap(),
            16_044,
            Some("reply.wav".to_owned()),
            heycode_core::AttachmentAudioMetadata::new(1_000, 8_000, 1, 16).unwrap(),
        )
        .unwrap();
        let call = heycode_core::CallId::from_raw("server-call");
        let sources = (0..4)
            .map(|index| {
                heycode_core::ServerToolSource::new(
                    &format!("https://example.invalid/source-{index}"),
                    Some("Source"),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let body = many("out", 40);
        vec![
            Item::User(many("user", 6)),
            Item::User(String::new()),
            Item::Command("/color cyan".to_owned()),
            Item::Attachments {
                attachments: vec![attachment.clone(), attachment],
                document_routes: Vec::new(),
            },
            Item::Attachments {
                attachments: Vec::new(),
                document_routes: Vec::new(),
            },
            Item::AudioOutput {
                attachments: vec![audio],
            },
            Item::Assistant(String::new()),
            Item::Assistant("# Heading\n\n- one\n- two\n- three\n\ntail".to_owned()),
            Item::Assistant(format!(
                "```rust\n{}\n```\n\n{}",
                many("code", 20),
                many("prose", 8)
            )),
            Item::Assistant(many("# heading", 12)),
            Item::Assistant(many("- bullet", 12)),
            Item::Assistant(many("word ", 30)),
            Item::Reasoning {
                text: many("think", 30),
                done: true,
                view: Default::default(),
            },
            Item::Reasoning {
                text: many("think", 30),
                done: false,
                view: Default::default(),
            },
            Item::Reasoning {
                text: "one".to_owned(),
                done: false,
                view: Default::default(),
            },
            Item::Reasoning {
                text: String::new(),
                done: false,
                view: Default::default(),
            },
            tool(
                "bash",
                serde_json::json!(format!("{body}\n[exit code: 1]")),
                false,
            ),
            tool(
                "bash",
                serde_json::json!(format!("{body}\n[exit code: 0]")),
                true,
            ),
            Item::Tool {
                view: Default::default(),
                call_id: None,
                name: "bash".to_owned(),
                args: serde_json::json!({ "command": "sleep 1" }),
                result: None,
                untrusted_content: None,
            },
            tool(
                "edit",
                serde_json::json!({ "diff": many("+line", 40), "message": "written" }),
                false,
            ),
            tool("write", serde_json::json!({ "message": "written" }), false),
            tool("read", serde_json::json!(body.clone()), false),
            tool("grep", serde_json::json!(body.clone()), false),
            tool("glob", serde_json::json!(body.clone()), false),
            tool(
                "todo_write",
                serde_json::json!(
                    (0..12)
                        .map(|index| serde_json::json!({
                            "content": format!("todo {index}"),
                            "status": "completed",
                        }))
                        .collect::<Vec<_>>()
                ),
                false,
            ),
            tool("todo_write", serde_json::json!("not an array"), false),
            tool("web_search", serde_json::json!(body.clone()), true),
            tool("web_fetch", serde_json::json!(body.clone()), false),
            tool(
                "mcp__server__thing",
                serde_json::json!({ "nested": body }),
                true,
            ),
            Item::ProviderState {
                provider: "openrouter".to_owned(),
                model: "model-a".to_owned(),
                protocol: "chat".to_owned(),
                kind: "assistant".to_owned(),
                output_index: 3,
            },
            Item::ServerTool {
                call_id: call.clone(),
                logical: "web_search".to_owned(),
                provider_name: "openrouter:web_search".to_owned(),
                result: None,
            },
            Item::ServerTool {
                call_id: call.clone(),
                logical: "web_search".to_owned(),
                provider_name: "openrouter:web_search".to_owned(),
                result: Some(
                    heycode_core::ServerToolResult::success(call.clone(), Some(4), sources.clone())
                        .unwrap(),
                ),
            },
            Item::ServerTool {
                call_id: call.clone(),
                logical: "web_search".to_owned(),
                provider_name: "openrouter:web_search".to_owned(),
                result: Some(heycode_core::ServerToolResult::error(call, "rate_limited").unwrap()),
            },
            Item::ServerToolUsage {
                logical: "web_search".to_owned(),
                requests: 3,
                cost: "unknown".to_owned(),
            },
            Item::Citation {
                url: "https://example.invalid/page".to_owned(),
                title: Some("Page".to_owned()),
                cited_text: Some(many("cited", 4)),
                start_index: Some(1),
                end_index: Some(9),
            },
            Item::Citation {
                url: "https://example.invalid/page".to_owned(),
                title: None,
                cited_text: None,
                start_index: None,
                end_index: None,
            },
            Item::FindingsReport {
                report: Box::new(
                    heycode_session::FindingReport::new(
                        heycode_session::FindingReportId::new("report-1").unwrap(),
                        heycode_session::FindingReportSource::workspace(
                            heycode_core::SessionId::from_raw("session-1"),
                            7,
                            0,
                            None,
                        )
                        .unwrap(),
                        vec![
                            heycode_session::ReportedFinding::new(
                                "finding-1",
                                heycode_session::ReviewSeverity::High,
                                "src/lib.rs",
                                10,
                                12,
                                "a".repeat(64),
                                "An actionable finding",
                                many("trigger", 4).replace('\n', " "),
                                many("failure", 4).replace('\n', " "),
                                many("impact", 4).replace('\n', " "),
                            )
                            .unwrap(),
                        ],
                    )
                    .unwrap(),
                ),
                expanded: true,
                focused: true,
            },
            Item::Compaction {
                native: false,
                strategy: None,
                replaced_upto_seq: 12,
                summary: Some(many("summary", 9)),
                provider_items: 0,
                expanded: true,
                focused: false,
            },
            Item::Compaction {
                native: true,
                strategy: Some("provider-native".to_owned()),
                replaced_upto_seq: 12,
                summary: None,
                provider_items: 4,
                expanded: true,
                focused: false,
            },
            Item::RuntimeLink {
                runtime: "codex".to_owned(),
            },
            Item::RouteChange {
                provider: "openrouter".to_owned(),
                model: "model-b".to_owned(),
            },
            Item::PlanMode { active: true },
            Item::Goal {
                action: "set".to_owned(),
                phase: Some("explore".to_owned()),
                objective: Some(many("objective", 9)),
                revision: 2,
            },
            Item::Goal {
                action: "clear".to_owned(),
                phase: None,
                objective: None,
                revision: 3,
            },
            Item::Workflow {
                action: "started".to_owned(),
                summary: many("workflow", 3),
            },
            Item::Schedule {
                action: "added".to_owned(),
                summary: many("schedule", 3),
            },
            Item::Info(many("info", 7)),
            Item::Info(String::new()),
            Item::Notice(many("notice", 7)),
            Item::Error(many("error", 7)),
        ]
    }

    #[test]
    fn every_item_variant_renders_within_its_height_bound() {
        let state = crate::app::AppState::new("model", std::path::PathBuf::from("/workspace"));
        let styles = state.styles();
        let samples = sample_items();
        let covered = samples.iter().map(variant_name).collect::<Vec<_>>();
        for variant in ALL_VARIANTS {
            assert!(
                covered.contains(&variant),
                "Item::{variant} has no height-bound sample"
            );
        }
        for item in &samples {
            for width in [1_usize, 24, 80] {
                for show_reasoning in [false, true] {
                    for neighbors in NEIGHBOR_CASES {
                        let bound = height_bound(item, neighbors, width, show_reasoning);
                        let rendered = crate::render::render_transcript_item(
                            item,
                            neighbors,
                            width,
                            show_reasoning,
                            styles,
                        )
                        .len();
                        assert!(
                            rendered <= bound,
                            "Item::{} at width {width} (reasoning {show_reasoning}) renders \
                         {rendered} rows but the viewport index budgeted {bound}",
                            variant_name(item)
                        );
                    }
                }
            }
        }
    }

    fn row_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    /// Where `window` sits inside the whole rendered transcript.
    fn window_start(full: &[String], window: &[String]) -> usize {
        let matches = full
            .windows(window.len())
            .enumerate()
            .filter(|(_, candidate)| *candidate == window)
            .map(|(start, _)| start)
            .collect::<Vec<_>>();
        assert_eq!(
            matches.len(),
            1,
            "a viewport must be one contiguous slice of the transcript: {matches:?}\n{window:#?}"
        );
        matches[0]
    }

    #[test]
    fn scrolling_past_the_exact_walk_stays_monotone_reachable_and_bounded() {
        let state = crate::app::AppState::new("model", std::path::PathBuf::from("/workspace"));
        let styles = state.styles();
        let width = 40_usize;
        let visible_lines = 20_usize;
        // Markdown items are the ones the height index over-counts most, and
        // there are far more of them than the exact walk covers.
        let items = (0..200)
            .map(|index| Item::Assistant(format!("answer-{index:03} unique-{index:03}")))
            .collect::<Vec<_>>();
        let render = |item: &Item| {
            crate::render::render_transcript_item(
                item,
                crate::render::ItemNeighbors::default(),
                width,
                false,
                styles,
            )
        };
        let full = items
            .iter()
            .flat_map(|item| render(item).iter().map(row_text).collect::<Vec<_>>())
            .collect::<Vec<_>>();
        let bounded = items
            .iter()
            .map(|item| height_bound(item, crate::render::ItemNeighbors::default(), width, false))
            .sum::<usize>();
        assert!(
            bounded > full.len(),
            "this corpus must exercise an over-counting index: bound {bounded} rows {}",
            full.len()
        );

        let mut cache = TranscriptRenderCache::default();
        let spec = |scroll_from_bottom: usize| ViewportSpec {
            width,
            visible_lines,
            scroll_from_bottom,
            show_reasoning: false,
            style_generation: 0,
        };
        let mut starts = Vec::new();
        let mut before = cache.metrics().rendered_items;
        for offset in (0..=1_200).step_by(7) {
            let viewport = cache.viewport(&items, spec(offset), |item, _| render(item));
            let after = cache.metrics().rendered_items;
            assert!(
                after.saturating_sub(before) <= 64,
                "offset {offset} rendered {} items",
                after.saturating_sub(before)
            );
            before = after;
            let window = viewport.iter().map(row_text).collect::<Vec<_>>();
            starts.push((offset, window_start(&full, &window)));
        }
        assert!(
            starts.windows(2).all(|pair| pair[1].1 <= pair[0].1),
            "scrolling up must never move the window down: {starts:?}"
        );
        assert!(
            starts.iter().any(|(_, start)| *start == 0),
            "the oldest row must be reachable by scrolling: {starts:?}"
        );
        // Offsets inside the exact walk move one rendered row each; past it
        // the index jumps by whole items, so movement is coarser but never
        // stalls for a whole screen.
        let (_, early) = starts[1];
        assert_eq!(
            early,
            full.len().saturating_sub(visible_lines).saturating_sub(7),
            "offsets inside the exact walk move one row each: {starts:?}"
        );
        assert_eq!(
            starts.first().map(|(_, start)| *start),
            Some(full.len().saturating_sub(visible_lines)),
            "offset 0 is the newest row: {starts:?}"
        );
        let oldest = cache.viewport(&items, spec(usize::MAX), |item, _| render(item));
        assert_eq!(
            window_start(&full, &oldest.iter().map(row_text).collect::<Vec<_>>()),
            0,
            "the oldest-position sentinel must land on the first row"
        );
    }

    #[test]
    fn durable_session_listener_stops_with_its_context_effect() {
        let bus = heycode_core::EventBus::default();
        let mut context = heycode_core::Context::default();
        let bridge = SessionEventBridge::default();
        let listener = bridge.clone();
        bus.on_effect::<heycode_session::SessionEvent>(&context, move |event| {
            listener.publish(event);
        });
        let mut receiver = bridge.subscribe();
        let event = heycode_session::SessionEvent {
            v: heycode_session::CURRENT_SESSION_LOG_VERSION,
            seq: 0,
            time_ms: 1,
            kind: heycode_session::SessionEventKind::UserMessage {
                text: "committed".to_owned(),
            },
        };
        bus.emit(event.clone());
        assert_eq!(receiver.try_recv().unwrap(), event);

        context.shutdown();
        bus.emit(heycode_session::SessionEvent {
            v: heycode_session::CURRENT_SESSION_LOG_VERSION,
            seq: 1,
            time_ms: 2,
            kind: heycode_session::SessionEventKind::UserMessage {
                text: "after shutdown".to_owned(),
            },
        });
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn slow_session_consumer_is_bounded_and_requests_durable_replay() {
        let bridge = SessionEventBridge::default();
        let mut receiver = bridge.subscribe();
        for seq in 0..=MAX_PENDING_SESSION_EVENTS as u64 {
            bridge.publish(&heycode_session::SessionEvent {
                v: heycode_session::CURRENT_SESSION_LOG_VERSION,
                seq,
                time_ms: 1,
                kind: heycode_session::SessionEventKind::UserMessage {
                    text: "committed".to_owned(),
                },
            });
        }
        assert!(bridge.take_lagged());
        let mut delivered = 0;
        while receiver.try_recv().is_ok() {
            delivered += 1;
        }
        assert_eq!(delivered, MAX_PENDING_SESSION_EVENTS);
        let _replacement = bridge.subscribe();
        assert!(!bridge.take_lagged());
    }
}
