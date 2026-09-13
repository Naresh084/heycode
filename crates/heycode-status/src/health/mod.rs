//! TEL05 — bounded retained diagnostics that survive a restart.
//!
//! A doctor report answers "is the product healthy right now". The questions a
//! support conversation actually opens with are different: *was* it healthy,
//! did this check ever pass, is it slow every time or was it slow once. None
//! of those can be answered from a live run, so this module keeps a bounded
//! durable record of past runs and hands it to `/health` and to a support
//! bundle.
//!
//! Three properties carry the row, and each is enforced rather than intended:
//!
//! * **Bounded** — [`MAX_ENTRIES`] entries *and* [`MAX_BYTES`] bytes, both
//!   applied on every write and on every read, so no observer ever holds an
//!   unbounded history. Both caps are reachable; neither is decoration.
//! * **Retained** — eviction is oldest-first with the most recent
//!   [`PROTECTED_UNHEALTHY`] failing runs held back, because a burst of healthy
//!   runs otherwise evicts the one entry that explains the failure. Protection
//!   yields to the cap, never the other way round.
//! * **Survives restart** — one owner-only file, written whole through an
//!   atomic rename, read back with a damaged line costing one entry rather
//!   than the history or its tail.
//!
//! The fourth property is not in the acceptance clause and matters more than
//! any of them: this data is *shared*. Everything text-shaped that reaches the
//! file goes through [`HealthLabel`], which screens with S15's recognizers on
//! construction and on deserialization, and no evidence body is retained at
//! all.

mod error;
mod model;
mod plugin;
mod store;

pub use error::HealthHistoryError;
pub use model::{
    ENTRY_BYTES_CEILING, HEALTH_HISTORY_SCHEMA_VERSION, HealthCheckRecord, HealthEntry,
    HealthEvidenceKind, HealthLabel, LABEL_MAX_BYTES, MAX_BYTES, MAX_CHECKS_PER_ENTRY, MAX_ENTRIES,
    OVERSIZED_LABEL, PROTECTED_UNHEALTHY, REDACTED_LABEL, SLOW_CHECK_MS,
};
pub use plugin::{SERVICE_HEALTH_HISTORY, health_history_plugin};
pub use store::{HealthHistory, HealthHistoryStore, MAX_READ_BYTES};
