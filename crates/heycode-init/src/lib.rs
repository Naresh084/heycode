//! Plugin-owned, preview-first workspace instruction initialization.

mod error;
mod model;
mod plugin;
mod service;

pub use error::InitError;
pub use model::{
    InitApplyOutcome, InitChangeKind, InitPreview, InitPreviewToken, MANAGED_END, MANAGED_START,
};
pub use plugin::init_plugin;
pub use service::InitService;
