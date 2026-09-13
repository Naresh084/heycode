//! Independent account, project and location health, and the profile that
//! carries all three.
//!
//! The three subjects are resolved and reported separately on purpose. A
//! single boolean cannot distinguish "no project is configured" from "the
//! configured project could not be confirmed", and collapsing them sends an
//! operator to the wrong repair.
//!
//! All three enums are **closed**. Adding a state must be a compiler-enforced
//! break for every consumer that renders one, rather than a silent new value
//! falling into a wildcard arm.

use std::fmt;

use crate::model::{
    GcpAdcCredentialType, GcpAdcFault, GcpAdcOrigin, GcpHealth, GcpLocation, GcpLocationError,
    GcpLocationOrigin, GcpProjectId, GcpProjectIdError, GcpProjectOrigin, GcpUncertainty,
};

/// Health of the Application Default Credentials account.
///
/// There is deliberately no `Healthy` state. Presence is not authentication:
/// proving an ADC credential usable requires a token exchange, and this
/// profile never performs one, so the best determinate positive answer is
/// [`Self::Configured`] — which projects to [`GcpHealth::Unknown`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GcpAccountHealth {
    /// Every documented ADC source was checked and none is configured.
    Absent,
    /// The highest-precedence configured ADC source cannot be used. ADC stops
    /// at this source rather than falling through to a lower one.
    Faulted {
        /// Where the unusable source was found.
        origin: GcpAdcOrigin,
        /// Why it is unusable.
        fault: GcpAdcFault,
    },
    /// An ADC source is configured. Usability is **not** proven.
    Configured {
        /// Where the credential was found.
        origin: GcpAdcOrigin,
        /// Declared document type, absent for an attached service account.
        credential: Option<GcpAdcCredentialType>,
    },
    /// No determinate answer was reached.
    Undetermined {
        /// Why the answer is unknown.
        reason: GcpUncertainty,
    },
}

impl GcpAccountHealth {
    /// Tri-state projection.
    ///
    /// [`Self::Configured`] projects to [`GcpHealth::Unknown`], never to
    /// [`GcpHealth::Healthy`].
    #[must_use]
    pub const fn verdict(&self) -> GcpHealth {
        match self {
            Self::Absent | Self::Faulted { .. } => GcpHealth::Unhealthy,
            Self::Configured { .. } | Self::Undetermined { .. } => GcpHealth::Unknown,
        }
    }

    /// Stable machine code naming the exact state.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Absent => "account-absent",
            Self::Faulted { .. } => "account-faulted",
            Self::Configured { .. } => "account-configured",
            Self::Undetermined { .. } => "account-undetermined",
        }
    }
}

impl fmt::Display for GcpAccountHealth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absent => {
                formatter.write_str("no application default credentials are configured")
            }
            Self::Faulted { origin, fault } => {
                write!(
                    formatter,
                    "credentials from {origin} are unusable ({fault})"
                )
            }
            Self::Configured { origin, credential } => match credential {
                Some(credential) => write!(
                    formatter,
                    "credentials of type {credential} from {origin} are configured but unverified"
                ),
                None => write!(
                    formatter,
                    "credentials from {origin} are configured but unverified"
                ),
            },
            Self::Undetermined { reason } => {
                write!(formatter, "credential state is unknown ({reason})")
            }
        }
    }
}

/// Health of the target project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GcpProjectHealth {
    /// Every documented project source was checked and none is set.
    Unset,
    /// A source is set but its value is not a project id or project number.
    Malformed {
        /// Where the rejected value came from.
        origin: GcpProjectOrigin,
        /// Which rule it violated.
        error: GcpProjectIdError,
    },
    /// A valid project is selected and the ambient host attests the same one.
    Confirmed {
        /// The effective project.
        project: GcpProjectId,
        /// Where the effective value came from.
        origin: GcpProjectOrigin,
    },
    /// A valid project is selected but nothing confirmed it.
    Unconfirmed {
        /// The effective project.
        project: GcpProjectId,
        /// Where the effective value came from.
        origin: GcpProjectOrigin,
        /// Why confirmation was not reached.
        reason: GcpUncertainty,
    },
    /// No determinate answer was reached: a source that could still supply a
    /// project could not be inspected.
    Undetermined {
        /// Why the answer is unknown.
        reason: GcpUncertainty,
    },
}

impl GcpProjectHealth {
    /// Tri-state projection.
    #[must_use]
    pub const fn verdict(&self) -> GcpHealth {
        match self {
            Self::Unset | Self::Malformed { .. } => GcpHealth::Unhealthy,
            Self::Confirmed { .. } => GcpHealth::Healthy,
            Self::Unconfirmed { .. } | Self::Undetermined { .. } => GcpHealth::Unknown,
        }
    }

    /// Stable machine code naming the exact state.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unset => "project-unset",
            Self::Malformed { .. } => "project-malformed",
            Self::Confirmed { .. } => "project-confirmed",
            Self::Unconfirmed { .. } => "project-unconfirmed",
            Self::Undetermined { .. } => "project-undetermined",
        }
    }

    /// The effective project when one was selected.
    #[must_use]
    pub const fn project(&self) -> Option<&GcpProjectId> {
        match self {
            Self::Confirmed { project, .. } | Self::Unconfirmed { project, .. } => Some(project),
            Self::Unset | Self::Malformed { .. } | Self::Undetermined { .. } => None,
        }
    }
}

impl fmt::Display for GcpProjectHealth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unset => formatter.write_str("no project is configured"),
            Self::Malformed { origin, error } => {
                write!(formatter, "project from {origin} is not valid ({error})")
            }
            Self::Confirmed { project, origin } => {
                write!(formatter, "project {project} from {origin} is confirmed")
            }
            Self::Unconfirmed {
                project,
                origin,
                reason,
            } => write!(
                formatter,
                "project {project} from {origin} is unconfirmed ({reason})"
            ),
            Self::Undetermined { reason } => {
                write!(formatter, "project state is unknown ({reason})")
            }
        }
    }
}

/// Health of the target location.
///
/// The question this subject answers is whether a well-formed Vertex AI
/// location is selected. Reaching a regional endpoint is a separate fact that
/// belongs to the Vertex catalog and is not claimed here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GcpLocationHealth {
    /// Every documented location source was checked and none is set.
    Unset,
    /// A source is set but its value is not a Vertex AI location.
    Malformed {
        /// Where the rejected value came from.
        origin: GcpLocationOrigin,
        /// Which rule it violated.
        error: GcpLocationError,
    },
    /// A well-formed Vertex AI location is selected.
    Selected {
        /// The effective location.
        location: GcpLocation,
        /// Where the effective value came from.
        origin: GcpLocationOrigin,
    },
    /// No determinate answer was reached: a source that could still supply a
    /// location could not be inspected.
    Undetermined {
        /// Why the answer is unknown.
        reason: GcpUncertainty,
    },
}

impl GcpLocationHealth {
    /// Tri-state projection.
    #[must_use]
    pub const fn verdict(&self) -> GcpHealth {
        match self {
            Self::Unset | Self::Malformed { .. } => GcpHealth::Unhealthy,
            Self::Selected { .. } => GcpHealth::Healthy,
            Self::Undetermined { .. } => GcpHealth::Unknown,
        }
    }

    /// Stable machine code naming the exact state.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unset => "location-unset",
            Self::Malformed { .. } => "location-malformed",
            Self::Selected { .. } => "location-selected",
            Self::Undetermined { .. } => "location-undetermined",
        }
    }

    /// The effective location when one was selected.
    #[must_use]
    pub const fn location(&self) -> Option<&GcpLocation> {
        match self {
            Self::Selected { location, .. } => Some(location),
            Self::Unset | Self::Malformed { .. } | Self::Undetermined { .. } => None,
        }
    }
}

impl fmt::Display for GcpLocationHealth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unset => formatter.write_str("no location is configured"),
            Self::Malformed { origin, error } => {
                write!(formatter, "location from {origin} is not valid ({error})")
            }
            Self::Selected { location, origin } => {
                write!(formatter, "location {location} from {origin} is selected")
            }
            Self::Undetermined { reason } => {
                write!(formatter, "location state is unknown ({reason})")
            }
        }
    }
}

/// One resolved Google Cloud authentication profile with three independent
/// health verdicts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcpAuthProfile {
    account: GcpAccountHealth,
    project: GcpProjectHealth,
    location: GcpLocationHealth,
    checked_at_ms: Option<u64>,
}

impl GcpAuthProfile {
    pub(crate) const fn new(
        account: GcpAccountHealth,
        project: GcpProjectHealth,
        location: GcpLocationHealth,
        checked_at_ms: Option<u64>,
    ) -> Self {
        Self {
            account,
            project,
            location,
            checked_at_ms,
        }
    }

    /// Application Default Credentials health.
    #[must_use]
    pub const fn account(&self) -> &GcpAccountHealth {
        &self.account
    }

    /// Target project health.
    #[must_use]
    pub const fn project(&self) -> &GcpProjectHealth {
        &self.project
    }

    /// Target location health.
    #[must_use]
    pub const fn location(&self) -> &GcpLocationHealth {
        &self.location
    }

    /// Unix epoch milliseconds at which the profile was resolved.
    ///
    /// Absent when the system clock is before the Unix epoch, which is a
    /// missing timestamp rather than a zero one.
    #[must_use]
    pub const fn checked_at_ms(&self) -> Option<u64> {
        self.checked_at_ms
    }

    /// Aggregate verdict: the weakest of the three subjects.
    #[must_use]
    pub const fn verdict(&self) -> GcpHealth {
        self.account
            .verdict()
            .weaker(self.project.verdict())
            .weaker(self.location.verdict())
    }
}

impl fmt::Display for GcpAuthProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: {}; {}; {}",
            self.verdict(),
            self.account,
            self.project,
            self.location
        )
    }
}
