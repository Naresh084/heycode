//! Pure projection/ranking for inference-provider and agent-runtime choices.

use heycode_llm::ProviderProfile;
use heycode_runtime::{AgentRuntimeDescriptor, AgentRuntimeKind};

use crate::command_palette::fuzzy_score;

/// User-visible route ownership class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutePickerClass {
    /// Native loop backed by an inference provider API.
    InferenceApi,
    /// heycode or another in-process/native coding-agent loop.
    NativeAgent,
    /// Official/external coding agent owns the loop.
    DelegatedAgent,
}

impl RoutePickerClass {
    /// Stable uppercase badge.
    #[must_use]
    pub const fn badge(self) -> &'static str {
        match self {
            Self::InferenceApi => "INFERENCE API",
            Self::NativeAgent => "NATIVE AGENT",
            Self::DelegatedAgent => "DELEGATED AGENT",
        }
    }
}

/// Filter applied before fuzzy ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutePickerFilter {
    /// Every inference/runtime row.
    All,
    /// Inference APIs used by the native loop.
    InferenceApis,
    /// Native and delegated agent runtimes.
    AgentRuntimes,
}

impl RoutePickerFilter {
    /// Stable display label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::InferenceApis => "inference",
            Self::AgentRuntimes => "agent runtimes",
        }
    }

    /// Tab-cycle order.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::All => Self::InferenceApis,
            Self::InferenceApis => Self::AgentRuntimes,
            Self::AgentRuntimes => Self::All,
        }
    }

    fn accepts(self, class: RoutePickerClass) -> bool {
        match self {
            Self::All => true,
            Self::InferenceApis => class == RoutePickerClass::InferenceApi,
            Self::AgentRuntimes => class != RoutePickerClass::InferenceApi,
        }
    }
}

/// Activatable live route returned to the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutePickerSelection {
    /// Switch the native loop to one registered inference provider/default.
    Inference {
        /// ProviderRegistry key.
        provider: String,
        /// Provider-owned default used until U07 chooses another model.
        default_model: String,
    },
    /// Keep/use the already-active native runtime.
    NativeRuntime {
        /// AgentRuntimeRegistry key.
        runtime: String,
    },
    /// Persist a delegated primary runtime for the next composed shell.
    DelegatedRuntime {
        /// AgentRuntimeRegistry key.
        runtime: String,
    },
}

/// One complete safe picker row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutePickerRow {
    /// Registry id within its class.
    pub id: String,
    /// Human display name.
    pub display_name: String,
    /// Explicit loop/route ownership class.
    pub class: RoutePickerClass,
    /// Safe secondary detail (default model or capability summary).
    pub detail: String,
    /// Whether this row describes an effective current route.
    pub current: bool,
    /// Activatable selection; `None` means visible but unavailable.
    pub selection: Option<RoutePickerSelection>,
    /// Visible prerequisite for an unavailable row.
    pub unavailable_reason: Option<String>,
}

/// Ranked row plus lower-is-better score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutePickerMatch {
    /// Complete projected row.
    pub row: RoutePickerRow,
    /// Stable fuzzy score.
    pub score: usize,
}

/// Project both registries without conflating inference providers and coding
/// agent runtimes. Runtime rows precede inference rows; each source is id-sorted.
#[must_use]
pub fn build_route_rows(
    providers: &[ProviderProfile],
    runtimes: &[AgentRuntimeDescriptor],
    current_provider: &str,
    current_runtime: &str,
) -> Vec<RoutePickerRow> {
    let mut runtimes = runtimes.to_vec();
    runtimes.sort_by(|left, right| {
        (left.id().as_str() != current_runtime)
            .cmp(&(right.id().as_str() != current_runtime))
            .then_with(|| left.id().cmp(right.id()))
    });
    let mut providers = providers.to_vec();
    providers.sort_by(|left, right| {
        (left.registry_name != current_provider)
            .cmp(&(right.registry_name != current_provider))
            .then_with(|| left.registry_name.cmp(&right.registry_name))
    });

    let mut rows = runtimes
        .into_iter()
        .map(|runtime| {
            let id = runtime.id().as_str().to_owned();
            let class = match runtime.kind() {
                AgentRuntimeKind::Native => RoutePickerClass::NativeAgent,
                AgentRuntimeKind::Delegated => RoutePickerClass::DelegatedAgent,
            };
            let current = id == current_runtime;
            let (selection, unavailable_reason) = match runtime.kind() {
                AgentRuntimeKind::Native if current => (
                    Some(RoutePickerSelection::NativeRuntime {
                        runtime: id.clone(),
                    }),
                    None,
                ),
                AgentRuntimeKind::Native => (
                    None,
                    Some("This native runtime is not active in the current shell.".to_owned()),
                ),
                AgentRuntimeKind::Delegated
                    if runtime.capabilities().supports_primary_sessions() =>
                {
                    (
                        Some(RoutePickerSelection::DelegatedRuntime {
                            runtime: id.clone(),
                        }),
                        None,
                    )
                }
                AgentRuntimeKind::Delegated => (
                    None,
                    Some("Delegated primary runtime bridge is incomplete.".to_owned()),
                ),
            };
            RoutePickerRow {
                detail: runtime_capability_summary(runtime.capabilities()),
                display_name: runtime.display_name().to_owned(),
                id,
                class,
                current,
                selection,
                unavailable_reason,
            }
        })
        .collect::<Vec<_>>();
    rows.extend(providers.into_iter().map(|provider| RoutePickerRow {
        id: provider.registry_name.clone(),
        display_name: provider.descriptor.display_name,
        class: RoutePickerClass::InferenceApi,
        detail: format!("default model {}", provider.default_model),
        current: provider.registry_name == current_provider,
        selection: Some(RoutePickerSelection::Inference {
            provider: provider.registry_name,
            default_model: provider.default_model,
        }),
        unavailable_reason: None,
    }));
    rows
}

/// Filter by explicit route class, then fuzzy id/display/detail/class badge.
#[must_use]
pub fn filter_routes(
    rows: &[RoutePickerRow],
    filter: RoutePickerFilter,
    query: &str,
) -> Vec<RoutePickerMatch> {
    let query = query.trim().to_ascii_lowercase();
    let eligible = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| filter.accepts(row.class));
    if query.is_empty() {
        return eligible
            .map(|(_, row)| RoutePickerMatch {
                row: row.clone(),
                score: 0,
            })
            .collect();
    }
    let mut matched = eligible
        .filter_map(|(index, row)| {
            let score = [
                fuzzy_score(&row.id.to_ascii_lowercase(), &query, 0),
                fuzzy_score(&row.display_name.to_ascii_lowercase(), &query, 100),
                fuzzy_score(&row.detail.to_ascii_lowercase(), &query, 200),
                fuzzy_score(&row.class.badge().to_ascii_lowercase(), &query, 300),
            ]
            .into_iter()
            .flatten()
            .min()?;
            Some((
                index,
                RoutePickerMatch {
                    row: row.clone(),
                    score,
                },
            ))
        })
        .collect::<Vec<_>>();
    matched.sort_by(|(left_index, left), (right_index, right)| {
        left.score
            .cmp(&right.score)
            .then_with(|| left_index.cmp(right_index))
    });
    matched.into_iter().map(|(_, row)| row).collect()
}

fn runtime_capability_summary(capabilities: &heycode_runtime::RuntimeCapabilities) -> String {
    let mut supported = Vec::new();
    for (label, support) in [
        ("models", capabilities.models),
        ("resume", capabilities.resume),
        ("fork", capabilities.fork),
        ("steer", capabilities.steer),
        ("compact", capabilities.compaction),
    ] {
        if support.is_supported() {
            supported.push(label);
        }
    }
    if supported.is_empty() {
        "capabilities unproven".to_owned()
    } else {
        supported.join(" · ")
    }
}
