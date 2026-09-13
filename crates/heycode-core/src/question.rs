//! Shared human question interaction modes.
use serde::{Deserialize, Serialize};

/// The answer shape requested by the model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionMode {
    /// Select one suggestion or provide a custom answer.
    #[default]
    SingleChoice,
    /// Select one or more suggestions or provide a custom answer.
    MultipleChoice,
    /// Enter a nonempty custom answer.
    FreeText,
}
