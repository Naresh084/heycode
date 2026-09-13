//! Transport-neutral client for the stable heycode app-server protocol.

mod client;
mod wire;

pub use client::{AppClient, AppTransport};
pub use wire::*;
