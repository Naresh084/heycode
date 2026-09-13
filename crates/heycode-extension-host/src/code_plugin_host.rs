//! PL09 product-side transaction for out-of-process contribution proxies.

use std::collections::BTreeMap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use heycode_core::{Context, PluginContributionSpec, ServiceKey};
use heycode_extensions::{
    CodePluginCancellationToken, CodePluginClient, CodePluginContribution,
    CodePluginContributionGeneration, CodePluginContributionHost, CodePluginInvocationError,
    ContributionKind, HostActivationFailure, PluginPermission,
};
use serde_json::Value;
use thiserror::Error;

const PRODUCT_CODE_KINDS: [ContributionKind; 6] = [
    ContributionKind::Skill,
    ContributionKind::Command,
    ContributionKind::Agent,
    ContributionKind::Hook,
    ContributionKind::Theme,
    ContributionKind::Provider,
];

/// One exact inactive product-registry proxy registration.
///
/// Registration may insert a row into its destination registry, but that row
/// must consult the supplied [`CodePluginInvocation`] gate before performing
/// any operation. The host opens every proxy together only after the runtime
/// owns the complete generation.
pub trait CodePluginProductRegistration: Send {
    /// Remove the exact proxy row.
    fn withdraw(self: Box<Self>);
}

/// Adapter for one concrete PL03 product registry.
pub trait CodePluginProductAdapter: Send + Sync {
    /// Exact contribution kind this adapter owns.
    fn kind(&self) -> ContributionKind;

    /// Exact inventory rows derived from the manifest-owned claim.
    fn inventory(&self, contribution: &CodePluginContribution) -> Vec<PluginContributionSpec>;

    /// Register one proxy while its shared invocation gate is closed.
    ///
    /// # Errors
    /// Invalid metadata, a duplicate row or an unavailable registry returns a
    /// closed host failure. The adapter must publish nothing on failure.
    fn register_inactive(
        &self,
        context: &Context,
        contribution: &CodePluginContribution,
        invocation: CodePluginInvocation,
    ) -> Result<Box<dyn CodePluginProductRegistration>, HostActivationFailure>;
}

/// Contribution-bound invocation proxy controlled by one generation gate.
#[derive(Clone)]
pub struct CodePluginInvocation {
    gate: Arc<InvocationGate>,
    kind: ContributionKind,
    public_name: String,
}

impl CodePluginInvocation {
    fn new(gate: Arc<InvocationGate>, kind: ContributionKind, public_name: String) -> Self {
        Self {
            gate,
            kind,
            public_name,
        }
    }

    /// Whether the complete process contribution generation is active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.gate.active.load(Ordering::SeqCst)
    }

    /// Exact host-admitted capability set for this process generation.
    #[must_use]
    pub fn granted_capabilities(&self) -> &[PluginPermission] {
        &self.gate.granted_capabilities
    }

    /// Invoke this proxy's exact contribution.
    ///
    /// # Errors
    /// An inactive generation or any closed PL09 invocation error is returned
    /// without exposing request/response bytes.
    pub fn invoke(
        &self,
        operation: &str,
        input: Value,
        cancellation: &CodePluginCancellationToken,
    ) -> Result<Value, CodePluginInvocationError> {
        if !self.is_active() {
            return Err(CodePluginInvocationError::Closed);
        }
        self.gate
            .client
            .invoke(self.kind, &self.public_name, operation, input, cancellation)
    }
}

impl fmt::Debug for CodePluginInvocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodePluginInvocation")
            .field("kind", &self.kind)
            .field("public_name", &self.public_name)
            .field("active", &self.is_active())
            .finish()
    }
}

struct InvocationGate {
    client: CodePluginClient,
    granted_capabilities: Vec<PluginPermission>,
    active: AtomicBool,
    retired: AtomicBool,
}

impl InvocationGate {
    fn new(client: CodePluginClient, granted_capabilities: Vec<PluginPermission>) -> Self {
        Self {
            client,
            granted_capabilities,
            active: AtomicBool::new(false),
            retired: AtomicBool::new(false),
        }
    }

    fn commit(&self) {
        if !self.retired.load(Ordering::SeqCst) {
            self.active.store(true, Ordering::SeqCst);
        }
    }

    fn retire(&self) {
        self.retired.store(true, Ordering::SeqCst);
        self.active.store(false, Ordering::SeqCst);
    }
}

/// Constructor failures for the complete six-adapter product host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProductCodePluginHostError {
    /// Two adapters claimed the same exact contribution kind.
    #[error("code plugin product host has a duplicate {0} adapter")]
    DuplicateAdapter(ContributionKind),
    /// One required PL03 contribution kind has no adapter.
    #[error("code plugin product host is missing its {0} adapter")]
    MissingAdapter(ContributionKind),
    /// MCP remains on PL04's ordinary connection owner, not the code host.
    #[error("code plugin product host cannot replace the bundled MCP owner")]
    McpAdapter,
}

/// Complete six-kind product host with a single generation availability gate.
pub struct ProductCodePluginHost {
    required_services: &'static [ServiceKey],
    adapters: BTreeMap<ContributionKind, Arc<dyn CodePluginProductAdapter>>,
}

impl ProductCodePluginHost {
    /// Bind exactly one adapter for every PL03 code contribution kind.
    ///
    /// # Errors
    /// Missing, duplicate or MCP adapters fail before a process generation can
    /// be activated.
    pub fn new(
        required_services: &'static [ServiceKey],
        adapters: Vec<Arc<dyn CodePluginProductAdapter>>,
    ) -> Result<Self, ProductCodePluginHostError> {
        let mut by_kind = BTreeMap::new();
        for adapter in adapters {
            let kind = adapter.kind();
            if kind == ContributionKind::Mcp {
                return Err(ProductCodePluginHostError::McpAdapter);
            }
            if by_kind.insert(kind, adapter).is_some() {
                return Err(ProductCodePluginHostError::DuplicateAdapter(kind));
            }
        }
        for kind in PRODUCT_CODE_KINDS {
            if !by_kind.contains_key(&kind) {
                return Err(ProductCodePluginHostError::MissingAdapter(kind));
            }
        }
        Ok(Self {
            required_services,
            adapters: by_kind,
        })
    }
}

impl CodePluginContributionHost for ProductCodePluginHost {
    fn required_services(&self) -> &'static [ServiceKey] {
        self.required_services
    }

    fn inventory(&self, contribution: &CodePluginContribution) -> Vec<PluginContributionSpec> {
        self.adapters
            .get(&contribution.kind())
            .map_or_else(Vec::new, |adapter| adapter.inventory(contribution))
    }

    fn activate_generation(
        &self,
        context: &Context,
        client: CodePluginClient,
        contributions: &[CodePluginContribution],
    ) -> Result<Arc<dyn CodePluginContributionGeneration>, HostActivationFailure> {
        let granted_capabilities = client
            .granted_capabilities()
            .map_err(|_| HostActivationFailure::Unavailable)?;
        let gate = Arc::new(InvocationGate::new(client, granted_capabilities));
        let mut registrations = Vec::with_capacity(contributions.len());
        for contribution in contributions {
            let Some(adapter) = self.adapters.get(&contribution.kind()) else {
                rollback(&gate, registrations);
                return Err(HostActivationFailure::Unsupported);
            };
            let invocation = CodePluginInvocation::new(
                Arc::clone(&gate),
                contribution.kind(),
                contribution.public_name().to_owned(),
            );
            let registered = catch_unwind(AssertUnwindSafe(|| {
                adapter.register_inactive(context, contribution, invocation)
            }));
            match registered {
                Ok(Ok(registration)) => registrations.push(registration),
                Ok(Err(failure)) => {
                    rollback(&gate, registrations);
                    return Err(failure);
                }
                Err(_) => {
                    rollback(&gate, registrations);
                    return Err(HostActivationFailure::Unavailable);
                }
            }
        }
        Ok(Arc::new(ProductCodePluginGeneration {
            gate,
            state: Mutex::new(ProductGenerationState {
                phase: ProductGenerationPhase::Prepared,
                registrations: Some(registrations),
            }),
        }))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProductGenerationPhase {
    Prepared,
    Active,
    Retired,
}

struct ProductGenerationState {
    phase: ProductGenerationPhase,
    registrations: Option<Vec<Box<dyn CodePluginProductRegistration>>>,
}

struct ProductCodePluginGeneration {
    gate: Arc<InvocationGate>,
    state: Mutex<ProductGenerationState>,
}

impl ProductCodePluginGeneration {
    fn lock_state(&self) -> MutexGuard<'_, ProductGenerationState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

impl CodePluginContributionGeneration for ProductCodePluginGeneration {
    fn commit(&self) {
        let mut state = self.lock_state();
        if state.phase != ProductGenerationPhase::Prepared {
            return;
        }
        self.gate.commit();
        state.phase = ProductGenerationPhase::Active;
    }

    fn withdraw(&self) {
        let registrations = {
            let mut state = self.lock_state();
            if state.phase == ProductGenerationPhase::Retired {
                return;
            }
            self.gate.retire();
            state.phase = ProductGenerationPhase::Retired;
            state.registrations.take().unwrap_or_default()
        };
        withdraw_reverse(registrations);
    }
}

impl Drop for ProductCodePluginGeneration {
    fn drop(&mut self) {
        self.withdraw();
    }
}

fn rollback(gate: &InvocationGate, registrations: Vec<Box<dyn CodePluginProductRegistration>>) {
    gate.retire();
    withdraw_reverse(registrations);
}

fn withdraw_reverse(mut registrations: Vec<Box<dyn CodePluginProductRegistration>>) {
    while let Some(registration) = registrations.pop() {
        let _ = catch_unwind(AssertUnwindSafe(|| registration.withdraw()));
    }
}
