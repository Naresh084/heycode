//! Conservative agent-mode route validation.
//!
//! PLM02 keeps three distinct facts about tool training: an explicit `true`, an
//! explicit `false`, and no evidence at all. PLM03 decides what each means for
//! agent mode, and the conservative answer is that **only proven support is
//! offered**. Being wrong about an unproven model costs a user a silently
//! broken agent loop rather than a clear refusal, so `Unknown` is refused.
//!
//! Refusing is not the same as explaining. A model LM Studio says is not
//! tool-trained is *proven incapable*; a model it says nothing about is
//! *unproven*. Those are different facts, PLM02 already keeps them apart, and
//! this layer must not flatten them into one message.

use crate::catalog::{LmStudioModelKind, LmStudioModelRecord};
use heycode_llm::CapabilitySupport;

/// Why a model may not serve agent mode.
///
/// Constructed only for a model that is actually ineligible, so a refusal can
/// never carry an "eligible" reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LmStudioAgentRefusal {
    /// LM Studio published `trained_for_tool_use: false`.
    ///
    /// This is a published denial: the model is proven not tool-trained.
    #[error("LM Studio reports this model was not trained for tool use")]
    NotToolTrained,
    /// LM Studio published no tool-training evidence for this model.
    ///
    /// Deliberately worded as unproven rather than incapable. The model may
    /// well handle tools; nothing published says so, and agent mode is not
    /// offered on a guess.
    #[error(
        "LM Studio publishes no tool-training evidence for this model, so tool use is unproven rather than ruled out"
    )]
    ToolTrainingUnproven,
    /// The model is an embedding model and cannot serve a chat turn at all.
    #[error("this is an embedding model and cannot serve a chat turn")]
    EmbeddingModel,
    /// LM Studio reported a model type this build does not recognize.
    #[error(
        "LM Studio reports a model type this build does not recognize, so chat use is unproven"
    )]
    UnrecognizedModelKind,
}

impl LmStudioAgentRefusal {
    /// Stable diagnostic identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotToolTrained => "not-tool-trained",
            Self::ToolTrainingUnproven => "tool-training-unproven",
            Self::EmbeddingModel => "embedding-model",
            Self::UnrecognizedModelKind => "unrecognized-model-kind",
        }
    }

    /// Whether the refusal rests on a published denial.
    ///
    /// `false` means heycode lacks evidence, never that LM Studio denied the
    /// capability. A consumer rendering "this model cannot do tools" must check
    /// this first.
    #[must_use]
    pub const fn is_published_denial(self) -> bool {
        matches!(self, Self::NotToolTrained | Self::EmbeddingModel)
    }
}

/// Agent-mode eligibility of one model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LmStudioAgentEligibility {
    /// A chat model LM Studio published as trained for tool use.
    Eligible,
    /// Not offered for agent mode, with the exact reason.
    Refused(LmStudioAgentRefusal),
}

impl LmStudioAgentEligibility {
    /// Whether this model may be offered for agent mode.
    #[must_use]
    pub const fn is_eligible(self) -> bool {
        matches!(self, Self::Eligible)
    }

    /// The refusal, when this model is not offered.
    #[must_use]
    pub const fn refusal(self) -> Option<LmStudioAgentRefusal> {
        match self {
            Self::Eligible => None,
            Self::Refused(refusal) => Some(refusal),
        }
    }
}

/// Why an agent-mode route was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LmStudioRouteError {
    /// The local library holds no model with the requested id.
    ///
    /// Distinct from a refusal: the model was never seen, so nothing is known
    /// about its capabilities either way.
    #[error("LM Studio has no downloaded model with that id")]
    UnknownModel,
    /// The model exists but may not serve agent mode.
    #[error("{0}")]
    NotAgentCapable(#[from] LmStudioAgentRefusal),
}

impl LmStudioModelRecord {
    /// Agent-mode eligibility of this model.
    ///
    /// Only an explicit `trained_for_tool_use: true` on a recognized chat model
    /// is eligible. Unknown is never promoted, and the refusal states which of
    /// the four distinct reasons applies.
    ///
    /// This is the **model** dimension only. It does not assert that the server
    /// speaks a protocol able to carry tool calls; that evidence lives in
    /// PLM01's `LmStudioServerReport` and joining the two is a route-composition
    /// concern above this crate.
    #[must_use]
    pub fn agent_eligibility(&self) -> LmStudioAgentEligibility {
        match self.kind {
            LmStudioModelKind::Embedding => {
                return LmStudioAgentEligibility::Refused(LmStudioAgentRefusal::EmbeddingModel);
            }
            LmStudioModelKind::Unrecognized(_) => {
                return LmStudioAgentEligibility::Refused(
                    LmStudioAgentRefusal::UnrecognizedModelKind,
                );
            }
            LmStudioModelKind::Llm => {}
        }
        match self.tool_trained {
            CapabilitySupport::Supported => LmStudioAgentEligibility::Eligible,
            CapabilitySupport::Unsupported => {
                LmStudioAgentEligibility::Refused(LmStudioAgentRefusal::NotToolTrained)
            }
            CapabilitySupport::Unknown => {
                LmStudioAgentEligibility::Refused(LmStudioAgentRefusal::ToolTrainingUnproven)
            }
        }
    }

    /// Whether this model may be offered for agent mode.
    #[must_use]
    pub fn is_agent_capable(&self) -> bool {
        self.agent_eligibility().is_eligible()
    }
}

/// Every model that may be offered for agent mode, in library order.
///
/// A model absent from this list is still a usable chat model; it is only
/// withheld from agent mode.
#[must_use]
pub fn agent_capable_models(records: &[LmStudioModelRecord]) -> Vec<&LmStudioModelRecord> {
    records
        .iter()
        .filter(|record| record.is_agent_capable())
        .collect()
}

/// Validate one requested agent-mode route against the local library.
///
/// # Errors
/// [`LmStudioRouteError::UnknownModel`] when no downloaded model has that id,
/// or [`LmStudioRouteError::NotAgentCapable`] carrying the exact refusal. The
/// two are separate because "never seen" and "seen and refused" are different
/// facts, and the refusal itself distinguishes a published denial from missing
/// evidence.
pub fn validate_agent_route<'a>(
    records: &'a [LmStudioModelRecord],
    key: &str,
) -> Result<&'a LmStudioModelRecord, LmStudioRouteError> {
    let record = records
        .iter()
        .find(|record| record.key == key)
        .ok_or(LmStudioRouteError::UnknownModel)?;
    match record.agent_eligibility() {
        LmStudioAgentEligibility::Eligible => Ok(record),
        LmStudioAgentEligibility::Refused(refusal) => Err(refusal.into()),
    }
}
