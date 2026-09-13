//! heycode-core — the plugin system.
//!
//! Owns the [`Context`] service map, the [`Plugin`] trait and composition,
//! the typed [`EventBus`], [`Waterfall`] interception seams, and opaque ids.
//! This crate depends on nothing internal; every other crate builds on it.

mod activation;
mod attachment;
mod context;
mod descriptor;
mod error;
mod events;
mod generation;
mod id;
mod inspection;
mod inventory;
mod plugin;
mod provider_response;
mod rich_tool_result;
mod server_tool;
mod service;
mod untrusted_content;
pub mod vocab;

pub use activation::{
    ActivationDiagnosticPlugin, ActivationDiagnosticReport, ActivationDiagnosticState,
    ActivationFailure, ActivationReport, ActivationStage, ComposedActivation, PluginActivation,
    PluginActivationOutcome,
};
pub use attachment::{
    AttachmentAudioMetadata, AttachmentContentId, AttachmentDimensions, AttachmentMediaType,
    AttachmentMetadata, AttachmentMetadataError, AttachmentSourceMetadata, DocumentInputRoute,
    DocumentInputRouteKind,
};
pub use context::Context;
pub use descriptor::{
    AppliedPlugin, PluginContributionKind, PluginDescriptor, PluginScope, PluginSource,
};
pub use error::CoreError;
pub use events::{EventBus, Layer, Next, Waterfall, WaterfallCompletion};
pub use generation::{
    Generation, GenerationContext, GenerationRegistry, ReloadOutcome, ReloadRejected,
};
pub use id::{CallId, RequestId, SessionId};
pub use inspection::{
    CompositionContributionReport, CompositionDiagnostic, CompositionDoctorReport,
    CompositionPluginReport, CompositionReport, inspect_composition,
};
pub use inventory::{
    ContributionKind, PluginContribution, PluginContributionSpec, PluginInventory,
    PluginInventorySnapshot,
};
pub use plugin::{
    Plugin, ScopedPlugin, compose, compose_activation, compose_scoped, compose_scoped_activation,
};
pub use provider_response::{
    CachePrefixImpact, ContextEditKind, ProviderCacheActivity, ProviderCacheUsage,
    ProviderContextEdit, ProviderResponseMetadata, ProviderResponseMetadataError,
};
pub use rich_tool_result::{
    DurableToolResult, DurableToolResultBlock, RichToolResultError, ToolResultAnnotations,
    ToolResultAudience, ToolResultBlockMetadata, ToolResultMediaReference, ToolResultResourceLink,
    ToolResultSchemaCheck, ToolStructuredContent,
};
pub use server_tool::{
    ServerToolCall, ServerToolError, ServerToolOutcome, ServerToolPublishedCost, ServerToolResult,
    ServerToolSource, ServerToolUsage, ServerToolUsageCost, ServerToolUsageEvidence,
    ServerToolWebMetadata, UrlCitation,
};
pub use service::ServiceKey;
pub use untrusted_content::{UntrustedContentBoundary, UntrustedContentSource};
pub use vocab::{
    NativeToolImplementationKind, NativeToolRoute, NativeToolRouteError, ProviderProtocol,
    ProviderRequestOption, ProviderRequestOptionError, ProviderStateError, ProviderStateItem,
    ProviderStateKind, TokenUsage, ToolSpec, canonical_tool_specs, canonicalize_json,
};

/// Convenient result alias for plugin-system fallible operations.
pub type CoreResult<T> = Result<T, CoreError>;

mod context_budget;
pub use context_budget::{ContextActivity, ContextBudget, ContextConfidence, ContextLimitSource};

mod question;
pub use question::QuestionMode;
