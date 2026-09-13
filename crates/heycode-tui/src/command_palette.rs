//! Pure command-catalog fuzzy ranking for slash/Ctrl+P consumers.

use heycode_agent::CommandCatalogEntry;

/// One catalog row plus its lower-is-better fuzzy score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPaletteMatch {
    /// Complete registry projection; no metadata is reconstructed here.
    pub entry: CommandCatalogEntry,
    /// Stable lower-is-better score.
    pub score: usize,
    /// Whether the query names this command closely enough — an exact,
    /// prefix or substring match on the id — for the palette to highlight it
    /// without the user arrowing to it. Description, source, subsequence and
    /// typo matches are listed but never preselected, so Enter on `/memory`
    /// cannot run `/compact` because a description happened to match.
    pub preselectable: bool,
}

/// Filter/rank by command id first, then description and source plugin.
/// Empty queries preserve registry order. Exact, prefix, substring,
/// subsequence and edit-distance≤2 matches are supported.
#[must_use]
pub fn filter_commands(catalog: &[CommandCatalogEntry], query: &str) -> Vec<CommandPaletteMatch> {
    let query = query.trim().trim_start_matches('/').to_ascii_lowercase();
    if query.is_empty() {
        return catalog
            .iter()
            .cloned()
            .map(|entry| CommandPaletteMatch {
                entry,
                score: 0,
                preselectable: true,
            })
            .collect();
    }
    let mut matched: Vec<_> = catalog
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let id = entry.descriptor.id().to_ascii_lowercase();
            let description = entry.descriptor.description().to_ascii_lowercase();
            let source = entry.descriptor.source().plugin().to_ascii_lowercase();
            let id_score = std::iter::once(id.as_str())
                .chain(entry.descriptor.aliases().iter().copied())
                .filter_map(|name| fuzzy_score(name, &query, 0))
                .min();
            let score = [
                id_score,
                fuzzy_score(&description, &query, 200),
                fuzzy_score(&source, &query, 300),
            ]
            .into_iter()
            .flatten()
            .min()?;
            Some((
                index,
                CommandPaletteMatch {
                    entry: entry.clone(),
                    score,
                    preselectable: id_score.is_some_and(|score| score < ID_SUBSEQUENCE_SCORE),
                },
            ))
        })
        .collect();
    matched.sort_by(|(left_index, left), (right_index, right)| {
        left.score
            .cmp(&right.score)
            .then_with(|| left_index.cmp(right_index))
    });
    matched.into_iter().map(|(_, row)| row).collect()
}

/// Id scores at or above this are subsequence/typo matches, not names.
const ID_SUBSEQUENCE_SCORE: usize = 80;

pub(crate) fn fuzzy_score(value: &str, query: &str, base: usize) -> Option<usize> {
    if value == query {
        return Some(base);
    }
    if value.starts_with(query) {
        return Some(base + 10 + value.len().saturating_sub(query.len()));
    }
    if let Some(position) = value.find(query) {
        return Some(base + 40 + position);
    }
    if let Some(gaps) = subsequence_gaps(value, query) {
        return Some(base + 80 + gaps);
    }
    let distance = edit_distance(value, query);
    (distance <= 2).then_some(base + 120 + distance * 10)
}

fn subsequence_gaps(value: &str, query: &str) -> Option<usize> {
    let mut value_indices = value.char_indices();
    let mut previous = None;
    let mut gaps = 0;
    for query_character in query.chars() {
        let (index, _) = value_indices.find(|(_, character)| *character == query_character)?;
        if let Some(previous) = previous {
            gaps += index.saturating_sub(previous + 1);
        }
        previous = Some(index);
    }
    Some(gaps)
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (left_index, left_character) in left.chars().enumerate() {
        let mut current = vec![left_index + 1];
        for (right_index, right_character) in right.iter().enumerate() {
            current.push(std::cmp::min(
                std::cmp::min(current[right_index] + 1, previous[right_index + 1] + 1),
                previous[right_index] + usize::from(left_character != *right_character),
            ));
        }
        previous = current;
    }
    previous[right.len()]
}
