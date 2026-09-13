//! Single integration-test binary for Azure provider behavior.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "it/catalog.rs"]
mod catalog;
#[path = "it/inference.rs"]
mod inference;
#[path = "it/plugin.rs"]
mod plugin;
