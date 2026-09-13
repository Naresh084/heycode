//! Manual and automatic portable compaction Consumers.
//!
//! Both paths dispatch through the composed C12 registry. The automatic layer
//! remains an around-middleware Consumer: after a durable fold it deliberately
//! short-circuits so the now-stale request is rebuilt from the log.

use std::sync::Arc;

use heycode_core::{Layer, Next};

use crate::agent::CompactionPolicy;
use crate::compaction_registry::{
    CompactionContext, CompactionOutcome, CompactionRegistry, PortableCompaction,
};
use crate::seams::{RequestDecision, RequestVerdict};

const AUTOCOMPACT_SETTINGS_NAMESPACE: &str = "autocompact";
const MIN_AUTOCOMPACT_TOKENS: u64 = 100_000;
const MAX_AUTOCOMPACT_TOKENS: u64 = 1_000_000;
const DEFAULT_AUTOCOMPACT_TOKENS: u64 = 200_000;

/// Live threshold override shared by legacy and strict-adapter request paths.
#[derive(Debug)]
pub struct AutoCompactionControl {
    enabled: bool,
    fixed_tokens: std::sync::RwLock<Option<u64>>,
}

impl AutoCompactionControl {
    pub(crate) fn new(policy: CompactionPolicy) -> Self {
        Self {
            enabled: policy.auto,
            fixed_tokens: std::sync::RwLock::new(None),
        }
    }

    /// Whether the owning Agent mounted automatic compaction.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Fixed input-token threshold, or `None` for model-aware automatic policy.
    #[must_use]
    pub fn fixed_tokens(&self) -> Option<u64> {
        *self
            .fixed_tokens
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn set_fixed_tokens(&self, fixed_tokens: Option<u64>) {
        *self
            .fixed_tokens
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = fixed_tokens;
    }

    pub(crate) fn apply(&self, budget: &mut heycode_llm::ContextBudget) {
        let Some(fixed) = self.fixed_tokens() else {
            return;
        };
        budget.compact_at = Some(
            budget
                .usable_input
                .map_or(fixed, |usable| fixed.min(usable)),
        );
    }
}

/// Default number of recent turns kept verbatim across compaction.
pub const DEFAULT_KEEP_TURNS: u64 = 2;

/// Index of the first event the keep-window retains, or `None` when there is
/// nothing worth folding.
///
/// Every local strategy uses this one rule, so portable summary, native
/// checkpoint and prune cut at the same durable boundary.
pub(crate) fn fold_boundary_of(
    events: &[heycode_session::SessionEvent],
    keep_recent_turns: u64,
) -> Option<usize> {
    let settled_turns = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match event.kind {
            heycode_session::SessionEventKind::TurnEnd { turn, .. } => Some((turn, index)),
            _ => None,
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let turn_starts: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match event.kind {
            heycode_session::SessionEventKind::TurnStart { turn }
                if settled_turns
                    .get(&turn)
                    .is_some_and(|end_index| *end_index > index) =>
            {
                Some(index)
            }
            _ => None,
        })
        .collect();
    let kept = usize::try_from(keep_recent_turns.max(1)).unwrap_or(usize::MAX);
    let turn_start = *turn_starts
        .len()
        .checked_sub(kept)
        .and_then(|index| turn_starts.get(index))?;
    let mut boundary = if turn_start > 0
        && matches!(
            events[turn_start - 1].kind,
            heycode_session::SessionEventKind::UserMessage { .. }
        ) {
        turn_start - 1
    } else {
        turn_start
    };
    if boundary > 0
        && matches!(
            events[boundary - 1].kind,
            heycode_session::SessionEventKind::UserAttachments { .. }
        )
    {
        boundary -= 1;
    }
    (boundary > 0).then_some(boundary)
}

/// Automatic pressure Consumer over the request seam.
pub(crate) struct AutoCompactionLayer {
    policy: CompactionPolicy,
    registry: Arc<CompactionRegistry>,
    context: CompactionContext,
    counters: Arc<heycode_llm::TokenCounterRegistry>,
    control: Arc<AutoCompactionControl>,
}

impl AutoCompactionLayer {
    pub(crate) const fn new(
        policy: CompactionPolicy,
        registry: Arc<CompactionRegistry>,
        context: CompactionContext,
        counters: Arc<heycode_llm::TokenCounterRegistry>,
        control: Arc<AutoCompactionControl>,
    ) -> Self {
        Self {
            policy,
            registry,
            context,
            counters,
            control,
        }
    }
}

#[async_trait::async_trait]
impl Layer<RequestDecision> for AutoCompactionLayer {
    async fn handle(
        &self,
        input: &mut RequestDecision,
        mut next: Next<'_, RequestDecision>,
    ) -> anyhow::Result<()> {
        // Strict adapters measure their final projected request after interception.
        if self.context.uses_strict_adapter() {
            return next.run(input).await;
        }
        let preliminary = self.context.request_budget(
            &input.request,
            &heycode_llm::EnvelopeTotal::Estimated(0),
            self.policy,
            &self.control,
        );
        let envelope = match heycode_llm::measure_chat_request_envelope(
            &self.counters,
            &preliminary.provider,
            &input.request,
            &input.cancellation,
        )
        .await
        {
            Ok(envelope) => envelope,
            Err(heycode_llm::EnvelopeMeasurementError::Cancelled) => return next.run(input).await,
            Err(error) => return Err(error.into()),
        };
        let budget = self.context.request_budget(
            &input.request,
            &envelope.total(),
            self.policy,
            &self.control,
        );
        if !budget.should_compact() {
            return next.run(input).await;
        }
        let cancellation = input.cancellation.clone();
        let outcome = {
            let pass = self.registry.compact(
                &self.context,
                PortableCompaction::ID,
                DEFAULT_KEEP_TURNS,
                &cancellation,
            );
            tokio::pin!(pass);
            tokio::select! {
                biased;
                () = cancellation.cancelled() => None,
                result = &mut pass => Some(result
                    .map_err(|error| anyhow::anyhow!("auto-compaction failed: {error}"))?),
            }
        };
        let Some(outcome) = outcome else {
            return next.run(input).await;
        };
        let CompactionOutcome::Applied { folded, .. } = outcome else {
            return next.run(input).await;
        };
        input.verdict = RequestVerdict::Rebuild {
            reason: format!("auto-compaction folded {folded} logged entries"),
        };
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoCompactionSetting {
    Automatic,
    Fixed(u64),
}

impl AutoCompactionSetting {
    fn from_value(value: &serde_json::Value) -> Result<Self, String> {
        let mode = value
            .get("mode")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "autocompact mode must be `auto` or `fixed`".to_owned())?;
        let tokens = value
            .get("tokens")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "autocompact tokens must be an integer".to_owned())?;
        if !(MIN_AUTOCOMPACT_TOKENS..=MAX_AUTOCOMPACT_TOKENS).contains(&tokens) {
            return Err(format!(
                "autocompact tokens must be between {MIN_AUTOCOMPACT_TOKENS} and {MAX_AUTOCOMPACT_TOKENS}"
            ));
        }
        match mode {
            "auto" => Ok(Self::Automatic),
            "fixed" => Ok(Self::Fixed(tokens)),
            _ => Err("autocompact mode must be `auto` or `fixed`".to_owned()),
        }
    }

    const fn fixed_tokens(self) -> Option<u64> {
        match self {
            Self::Automatic => None,
            Self::Fixed(tokens) => Some(tokens),
        }
    }
}

fn autocompact_settings_definition()
-> Result<heycode_settings::SettingsDefinition, heycode_settings::SettingsError> {
    let schema = heycode_settings::SettingsSchema::new(
        serde_json::json!({
            "type":"object",
            "additionalProperties":false,
            "properties":{
                "mode":{"type":"string","enum":["auto","fixed"]},
                "tokens":{
                    "type":"integer",
                    "minimum":MIN_AUTOCOMPACT_TOKENS,
                    "maximum":MAX_AUTOCOMPACT_TOKENS
                }
            }
        }),
        serde_json::json!({
            "mode":"auto",
            "tokens":DEFAULT_AUTOCOMPACT_TOKENS
        }),
        |value| AutoCompactionSetting::from_value(value).map(|_| ()),
    )?
    .with_wire_exposure();
    Ok(heycode_settings::SettingsDefinition::new(
        heycode_settings::SettingsNamespace::new(AUTOCOMPACT_SETTINGS_NAMESPACE)?,
        schema,
    ))
}

struct AutoCompactCommand {
    descriptor: crate::CommandDescriptor,
    settings: Arc<heycode_settings::SettingsService>,
    control: Arc<AutoCompactionControl>,
    read_only: crate::CommandAvailability,
    disabled: crate::CommandAvailability,
}

#[async_trait::async_trait]
impl crate::Command for AutoCompactCommand {
    fn descriptor(&self) -> &crate::CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> crate::CommandAvailability {
        if !self.control.enabled() {
            self.disabled.clone()
        } else if !self.settings.writable() {
            self.read_only.clone()
        } else {
            crate::CommandAvailability::available()
        }
    }

    async fn execute(&self, agent: &crate::Agent, args: &str) -> anyhow::Result<()> {
        let namespace = heycode_settings::SettingsNamespace::new(AUTOCOMPACT_SETTINGS_NAMESPACE)?;
        let snapshot = self
            .settings
            .get(&namespace)?
            .ok_or_else(|| anyhow::anyhow!("autocompact settings are not registered"))?;
        let input = args.trim();
        if input.is_empty() {
            let current = AutoCompactionSetting::from_value(snapshot.resolved())
                .map_err(anyhow::Error::msg)?;
            agent.ui().emit(crate::UiEvent::AutoCompactPickerRequested {
                enabled: self.control.enabled(),
                current_tokens: current.fixed_tokens(),
            });
            return Ok(());
        }
        let requested = if input.eq_ignore_ascii_case("auto") {
            AutoCompactionSetting::Automatic
        } else {
            AutoCompactionSetting::Fixed(parse_autocompact_tokens(input)?)
        };
        let section = match requested {
            AutoCompactionSetting::Automatic => serde_json::json!({
                "mode":"auto",
                "tokens":DEFAULT_AUTOCOMPACT_TOKENS
            }),
            AutoCompactionSetting::Fixed(tokens) => serde_json::json!({
                "mode":"fixed",
                "tokens":tokens
            }),
        };
        let committed =
            self.settings
                .replace_user(&namespace, section, Some(snapshot.revision()))?;
        let applied =
            AutoCompactionSetting::from_value(committed.resolved()).map_err(anyhow::Error::msg)?;
        self.control.set_fixed_tokens(applied.fixed_tokens());
        agent.ui().emit(crate::UiEvent::Info {
            text: format!(
                "{}; saved and applied to this session",
                autocompact_status(applied, self.control.enabled())
            ),
        });
        Ok(())
    }
}

fn parse_autocompact_tokens(input: &str) -> anyhow::Result<u64> {
    let lower = input.trim().to_ascii_lowercase();
    let (number, multiplier) = if let Some(number) = lower.strip_suffix('k') {
        (number, 1_000)
    } else if let Some(number) = lower.strip_suffix('m') {
        (number, 1_000_000)
    } else {
        (lower.as_str(), 1)
    };
    let mut tokens = number
        .parse::<u64>()
        .ok()
        .and_then(|value| value.checked_mul(multiplier))
        .ok_or_else(|| anyhow::anyhow!("usage: /autocompact [auto|100k-1m]"))?;
    if multiplier == 1 && (100..=1_000).contains(&tokens) {
        tokens = tokens.saturating_mul(1_000);
    }
    if !(MIN_AUTOCOMPACT_TOKENS..=MAX_AUTOCOMPACT_TOKENS).contains(&tokens) {
        anyhow::bail!("usage: /autocompact [auto|100k-1m]");
    }
    Ok(tokens)
}

fn autocompact_status(setting: AutoCompactionSetting, enabled: bool) -> String {
    let status = match setting {
        AutoCompactionSetting::Automatic => {
            "Auto-compact window: auto (tuned for the active model)".to_owned()
        }
        AutoCompactionSetting::Fixed(tokens) => format!(
            "Auto-compact window: {} input tokens",
            format_token_count(tokens)
        ),
    };
    if enabled {
        status
    } else {
        format!("{status}; automatic compaction is disabled by the active profile")
    }
}

fn format_token_count(tokens: u64) -> String {
    let digits = tokens.to_string();
    let mut output = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            output.push(',');
        }
        output.push(character);
    }
    output
}

/// Settings-backed live `/autocompact` threshold control.
#[must_use]
pub fn autocompact_plugin() -> Box<dyn heycode_core::Plugin> {
    struct AutoCompactPlugin;

    impl heycode_core::Plugin for AutoCompactPlugin {
        fn name(&self) -> &'static str {
            "autocompact"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::Command,
                ],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::SettingsNamespace,
                    AUTOCOMPACT_SETTINGS_NAMESPACE,
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "autocompact",
                ),
            ]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_settings::SERVICE_SETTINGS,
                crate::SERVICE_AGENT,
                crate::SERVICE_COMMANDS,
            ]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let settings = context
                .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
                .ok_or_else(|| heycode_core::CoreError::other("settings service type mismatch"))?;
            let agent = context
                .get::<crate::Agent>(crate::SERVICE_AGENT)
                .ok_or_else(|| heycode_core::CoreError::other("agent service type mismatch"))?;
            let commands = context
                .get::<crate::CommandRegistry>(crate::SERVICE_COMMANDS)
                .ok_or_else(|| heycode_core::CoreError::other("commands service type mismatch"))?;
            let namespace =
                heycode_settings::SettingsNamespace::new(AUTOCOMPACT_SETTINGS_NAMESPACE)
                    .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            let snapshot = settings
                .register(
                    context,
                    autocompact_settings_definition()
                        .map_err(|error| heycode_core::CoreError::other(error.to_string()))?,
                )
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            let configured = AutoCompactionSetting::from_value(snapshot.resolved())
                .map_err(heycode_core::CoreError::other)?;
            let control = agent.auto_compaction_control().clone();
            let previous = control.fixed_tokens();
            control.set_fixed_tokens(configured.fixed_tokens());
            let restore = control.clone();
            context.effect(move || restore.set_fixed_tokens(previous));
            let watcher_control = control.clone();
            settings
                .watch(context, &namespace, move |change| {
                    if let Ok(configured) =
                        AutoCompactionSetting::from_value(change.next().resolved())
                    {
                        watcher_control.set_fixed_tokens(configured.fixed_tokens());
                    }
                })
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            let source = crate::CommandSource::from_plugin(self.name())
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            let descriptor = crate::CommandDescriptor::new(
                "autocompact",
                "Show or set the live automatic compaction token threshold",
                vec![
                    crate::CommandArgument::optional(
                        "threshold",
                        "auto or a token count from 100k through 1m",
                    )
                    .map_err(|error| heycode_core::CoreError::other(error.to_string()))?,
                ],
                crate::CommandTiming::Immediate,
                source,
            )
            .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            let read_only = crate::CommandAvailability::unavailable(
                "autocompact settings are read-only in this profile",
            )
            .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            let disabled = crate::CommandAvailability::unavailable(
                "automatic compaction is disabled by the active profile",
            )
            .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            commands
                .register_effect(
                    context,
                    Arc::new(AutoCompactCommand {
                        descriptor,
                        settings,
                        control,
                        read_only,
                        disabled,
                    }),
                )
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))
        }
    }

    Box::new(AutoCompactPlugin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_grammar_is_bounded_and_unambiguous() {
        for (input, expected) in [
            ("100", 100_000),
            ("250k", 250_000),
            ("1M", 1_000_000),
            ("100000", 100_000),
        ] {
            assert_eq!(parse_autocompact_tokens(input).ok(), Some(expected));
        }
        for input in ["99", "99k", "2m", "0.5m", "100k extra"] {
            assert!(parse_autocompact_tokens(input).is_err(), "{input}");
        }
    }

    #[test]
    fn fixed_threshold_never_inflates_the_usable_model_budget() {
        let control = AutoCompactionControl::new(CompactionPolicy::default());
        control.set_fixed_tokens(Some(500_000));
        let mut large = heycode_llm::context_budget(
            "p".to_owned(),
            "large".to_owned(),
            &heycode_llm::EnvelopeTotal::Exact(1),
            Some(1_000_000),
            0,
            8_192,
            0.8,
            true,
        );
        control.apply(&mut large);
        assert_eq!(large.compact_at, Some(500_000));

        let mut small = heycode_llm::context_budget(
            "p".to_owned(),
            "small".to_owned(),
            &heycode_llm::EnvelopeTotal::Exact(1),
            Some(200_000),
            0,
            40_000,
            0.8,
            true,
        );
        control.apply(&mut small);
        assert_eq!(small.compact_at, small.usable_input);
        assert_eq!(small.window, Some(200_000));

        let mut unknown = heycode_llm::context_budget(
            "p".to_owned(),
            "unknown".to_owned(),
            &heycode_llm::EnvelopeTotal::Exact(1),
            None,
            0,
            8_192,
            0.8,
            true,
        );
        control.apply(&mut unknown);
        assert_eq!(unknown.window, None);
        assert_eq!(unknown.compact_at, Some(500_000));
    }
}
