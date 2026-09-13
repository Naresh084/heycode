//! Authorization flow registry with cancellation and credential commit proof.

mod error;
mod flow;
mod model;
mod plugin;
mod service;

pub use error::AuthorizationError;
pub use flow::AuthorizationFlow;
pub use model::{
    AuthorizationDescriptor, AuthorizationFlowFailure, AuthorizationFlowId, AuthorizationGrant,
    AuthorizationMethod, AuthorizationOperationId, AuthorizationReceipt, AuthorizationRequest,
};
pub use plugin::authorization_plugin;
pub use service::AuthorizationService;

/// Authorization flow registry service.
pub const SERVICE_AUTHORIZATION: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("authorization");
