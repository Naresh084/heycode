//! Stable UI slot and descriptor metadata.

use crate::UiRegistryError;

/// Validated id within one UI slot.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UiContributionId(String);

impl UiContributionId {
    /// Validate a lowercase kebab/dotted id.
    ///
    /// # Errors
    /// Empty, malformed or oversized values return [`UiRegistryError::InvalidId`].
    pub fn new(value: impl Into<String>) -> Result<Self, UiRegistryError> {
        let value = value.into();
        let bytes = value.as_bytes();
        let valid = (1..=128).contains(&bytes.len())
            && bytes.first().is_some_and(u8::is_ascii_lowercase)
            && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
            && bytes.iter().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(*byte, b'-' | b'.')
            })
            && !value.contains("--")
            && !value.contains("..")
            && !value.contains("-.")
            && !value.contains(".-");
        if valid {
            Ok(Self(value))
        } else {
            Err(UiRegistryError::InvalidId)
        }
    }

    /// Stable display/lookup value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for UiContributionId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// UI-neutral contribution placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UiSlot {
    /// Primary/secondary full content surface.
    Panel,
    /// Modal or permission/question overlay.
    Dialog,
    /// Compact status-line/card contribution.
    Status,
    /// Optional secondary diff/jobs/agents surface beside or below transcript.
    SidePanel,
}

impl UiSlot {
    /// Stable inventory/diagnostic id.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Panel => "panel",
            Self::Dialog => "dialog",
            Self::Status => "status",
            Self::SidePanel => "side-panel",
        }
    }
}

/// Universal metadata for one typed opaque UI capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiContributionDescriptor {
    slot: UiSlot,
    id: UiContributionId,
    title: String,
    priority: i16,
}

impl UiContributionDescriptor {
    /// Validate and construct descriptor metadata.
    ///
    /// # Errors
    /// Invalid id or title fails before registry publication.
    pub fn new(
        slot: UiSlot,
        id: impl Into<String>,
        title: impl Into<String>,
        priority: i16,
    ) -> Result<Self, UiRegistryError> {
        let title = title.into();
        if title.is_empty()
            || title.trim() != title
            || title.len() > 256
            || title.chars().any(char::is_control)
        {
            return Err(UiRegistryError::InvalidTitle);
        }
        Ok(Self {
            slot,
            id: UiContributionId::new(id)?,
            title,
            priority,
        })
    }

    /// Placement slot.
    #[must_use]
    pub const fn slot(&self) -> UiSlot {
        self.slot
    }

    /// Stable id within the slot.
    #[must_use]
    pub fn id(&self) -> &UiContributionId {
        &self.id
    }

    /// Safe human label.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Higher values sort first within a slot.
    #[must_use]
    pub const fn priority(&self) -> i16 {
        self.priority
    }

    pub(crate) fn inventory_name(&self) -> String {
        format!("{}:{}", self.slot.as_str(), self.id)
    }
}
