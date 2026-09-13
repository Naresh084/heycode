//! Pure live-catalog model filtering and fuzzy ranking.

use heycode_llm::{
    CapabilityFilter, CatalogSnapshot, ModelDescriptor, ModelFilter, ModelLifecycleFilter,
};

use crate::command_palette::fuzzy_score;

/// User-visible model filter. Unknown capability evidence never satisfies an
/// explicit supported filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelPickerFilter {
    /// Every effectively non-retired model.
    Selectable,
    /// Stable lifecycle only.
    Stable,
    /// Explicit tool/function support.
    Tools,
    /// Explicit reasoning/thinking support.
    Reasoning,
}

impl ModelPickerFilter {
    /// Stable display id.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Selectable => "selectable",
            Self::Stable => "stable",
            Self::Tools => "tools",
            Self::Reasoning => "reasoning",
        }
    }

    /// Cycle order used by Tab.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Selectable => Self::Stable,
            Self::Stable => Self::Tools,
            Self::Tools => Self::Reasoning,
            Self::Reasoning => Self::Selectable,
        }
    }

    fn filter(self) -> ModelFilter {
        match self {
            Self::Selectable => ModelFilter::selectable(),
            Self::Stable => ModelFilter {
                lifecycle: ModelLifecycleFilter::Stable,
                ..ModelFilter::default()
            },
            Self::Tools => ModelFilter {
                tools: CapabilityFilter::Supported,
                lifecycle: ModelLifecycleFilter::Selectable,
                ..ModelFilter::default()
            },
            Self::Reasoning => ModelFilter {
                reasoning: CapabilityFilter::Supported,
                lifecycle: ModelLifecycleFilter::Selectable,
                ..ModelFilter::default()
            },
        }
    }
}

/// One ranked model row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelPickerMatch {
    /// Complete safe provider model descriptor.
    pub model: ModelDescriptor,
    /// Lower-is-better fuzzy score.
    pub score: usize,
    /// Visible user/project assertion descriptions; provider evidence remains
    /// untouched in [`Self::model`].
    pub assertions: Vec<String>,
    /// Whether at least one assertion contradicts explicit provider evidence.
    pub has_contradiction: bool,
}

/// Filter against explicit lifecycle/capability evidence, then rank model id,
/// display name and aliases. Empty queries preserve the catalog's newest-first order.
#[must_use]
pub fn filter_models(
    snapshot: &CatalogSnapshot,
    filter: ModelPickerFilter,
    query: &str,
    at_ms: u64,
) -> Vec<ModelPickerMatch> {
    let query = query.trim().to_ascii_lowercase();
    let filtered = snapshot.filter_models(&filter.filter(), at_ms);
    if query.is_empty() {
        return filtered
            .into_iter()
            .cloned()
            .map(|model| ModelPickerMatch {
                model,
                score: 0,
                assertions: Vec::new(),
                has_contradiction: false,
            })
            .collect();
    }
    let mut matches: Vec<_> = filtered
        .into_iter()
        .enumerate()
        .filter_map(|(index, model)| {
            let id = model.id.to_ascii_lowercase();
            let display_name = model.display_name.to_ascii_lowercase();
            let score = std::iter::once(fuzzy_score(&id, &query, 0))
                .chain(std::iter::once(fuzzy_score(&display_name, &query, 200)))
                .chain(
                    model
                        .aliases
                        .iter()
                        .map(|alias| fuzzy_score(&alias.to_ascii_lowercase(), &query, 100)),
                )
                .flatten()
                .min()?;
            Some((
                index,
                ModelPickerMatch {
                    model: model.clone(),
                    score,
                    assertions: Vec::new(),
                    has_contradiction: false,
                },
            ))
        })
        .collect();
    matches.sort_by(|(left_index, left), (right_index, right)| {
        left.score
            .cmp(&right.score)
            .then_with(|| left_index.cmp(right_index))
    });
    matches.into_iter().map(|(_, row)| row).collect()
}
