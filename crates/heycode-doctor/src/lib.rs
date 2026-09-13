//! Plugin-contributed health checks and redacted diagnostic reports.

mod error;
mod model;
mod plugin;
mod registry;

pub use error::DoctorError;
pub use model::{
    ConfigMigrationChangeEvidence, ConfigMigrationDispositionEvidence, ConfigMigrationEvidence,
    ConfigVersionEvidence, DOCTOR_REPORT_SCHEMA_VERSION, DoctorCheckId, DoctorCheckResult,
    DoctorCode, DoctorEvidence, DoctorOutcome, DoctorReport, DoctorStatus, DoctorSummary,
};
pub use plugin::{composition_doctor_plugin, doctor_plugin};
pub use registry::{DoctorCheck, DoctorCheckTiming, DoctorRegistry, DoctorRun};

/// Plugin-contributed diagnostic registry service.
pub const SERVICE_DOCTOR: heycode_core::ServiceKey = heycode_core::ServiceKey::new("doctor");
