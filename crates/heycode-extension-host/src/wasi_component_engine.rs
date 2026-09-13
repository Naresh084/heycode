//! Wasmtime 48 Component Model/WASIp3 implementation of PL10.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle, ThreadId};

use heycode_extensions::{
    CODE_PLUGIN_MAX_FRAME_BYTES, CodePluginCancellationToken, CodePluginExit, CodePluginProcess,
    CodePluginRemoteErrorCode, CodePluginTransportFault, ContributionKind, PluginPermission,
    WASI_CODE_PLUGIN_INTERFACE_V1, WasiComponentAbiReport, WasiComponentEngine,
    WasiComponentEngineFault, WasiComponentInstance, WasiComponentInstantiation,
    WasiComponentWorld, WasiPreopenAccess,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wasmtime::component::types::{ComponentFunc, ComponentItem, Type};
use wasmtime::component::{Component, Func, Instance, Linker, ResourceTable, Val};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder, UpdateDeadline};
use wasmtime_wasi::sockets::SocketAddrUse;
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxView, WasiView};

const MAX_COMPONENT_MEMORY_BYTES: usize = 64 * 1024 * 1024;
const MAX_COMPONENT_TABLE_ELEMENTS: usize = 16_384;
const MAX_COMPONENT_FUEL: u64 = 20_000_000;

type ExitListener = Arc<dyn Fn(CodePluginExit) + Send + Sync>;

/// Wasmtime 48 LTS engine for heycode code-plugin WIT v1.
///
/// Component compilation and Canonical-ABI type validation occur before a
/// process adapter is returned. Each instance owns one Store on a dedicated
/// thread, a bounded fuel/memory/table budget, and an epoch interruption path.
/// The WASI context starts with closed stdin, sink stdout/stderr, no arguments,
/// no environment, no preopens, and denied networking. Only the exact policy
/// resources supplied by [`WasiComponentInstantiation`] are added.
pub struct WasmtimeWasiComponentEngine {
    engine: Engine,
}

impl WasmtimeWasiComponentEngine {
    /// Construct the pinned Component Model/WASIp3 runtime.
    ///
    /// # Errors
    /// An unavailable Wasmtime compiler/runtime returns a closed engine fault.
    pub fn new() -> Result<Self, WasiComponentEngineFault> {
        let mut config = Config::new();
        config
            .wasm_component_model(true)
            .wasm_component_model_async(true)
            .consume_fuel(true)
            .epoch_interruption(true);
        let engine = Engine::new(&config).map_err(|_| WasiComponentEngineFault::Unavailable)?;
        Ok(Self { engine })
    }
}

impl std::fmt::Debug for WasmtimeWasiComponentEngine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WasmtimeWasiComponentEngine")
            .field("abi", &"wasmtime-48-wasip3")
            .finish()
    }
}

impl WasiComponentEngine for WasmtimeWasiComponentEngine {
    fn instantiate(
        &self,
        request: &WasiComponentInstantiation,
        cancellation: &CodePluginCancellationToken,
    ) -> Result<WasiComponentInstance, WasiComponentEngineFault> {
        if cancellation.is_cancelled() {
            return Err(WasiComponentEngineFault::Cancelled);
        }
        let component = Component::new(&self.engine, request.component_bytes())
            .map_err(|_| WasiComponentEngineFault::InvalidComponent)?;
        let (imports, exports) = validate_component_abi(&self.engine, &component, request.world())?;
        if cancellation.is_cancelled() {
            return Err(WasiComponentEngineFault::Cancelled);
        }
        let wasi = wasi_context(request)?;
        let process = WasmtimeCodePluginProcess::spawn(
            self.engine.clone(),
            component,
            wasi,
            cancellation.clone(),
        )?;
        let abi = WasiComponentAbiReport::new(
            request.world().as_str(),
            request.wit_digest().as_str(),
            imports,
            exports,
        )
        .map_err(|_| WasiComponentEngineFault::InvalidComponent)?;
        Ok(WasiComponentInstance::new(abi, process))
    }
}

struct WasiState {
    context: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
    interrupt: Arc<AtomicBool>,
}

impl WasiView for WasiState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.context,
            table: &mut self.table,
        }
    }
}

fn wasi_context(request: &WasiComponentInstantiation) -> Result<WasiCtx, WasiComponentEngineFault> {
    let mut builder = WasiCtx::builder();
    for preopen in request.preopens() {
        let permissions = match preopen.access() {
            WasiPreopenAccess::ReadOnly => FsPerms::ReadOnly,
            WasiPreopenAccess::ReadWrite => FsPerms::ReadWrite,
            // Wasmtime's host API has no write-without-read descriptor. Never
            // widen a write-only mint to read-write.
            WasiPreopenAccess::WriteOnly => return Err(WasiComponentEngineFault::Start),
        };
        #[cfg(unix)]
        {
            let directory = std::fs::File::open(preopen.host_path())
                .map_err(|_| WasiComponentEngineFault::Start)?;
            if !preopen.matches_open_directory(&directory) {
                return Err(WasiComponentEngineFault::Start);
            }
            builder
                .preopened_dir(preopen.host_path(), preopen.guest_path(), permissions)
                .map_err(|_| WasiComponentEngineFault::Start)?;
            let current = std::fs::File::open(preopen.host_path())
                .map_err(|_| WasiComponentEngineFault::Start)?;
            if !preopen.matches_open_directory(&current) {
                return Err(WasiComponentEngineFault::Start);
            }
            drop(directory);
        }
        #[cfg(not(unix))]
        {
            let _ = permissions;
            return Err(WasiComponentEngineFault::Start);
        }
    }

    let mut allowed = HashSet::new();
    for endpoint in request.network_endpoints() {
        let address = endpoint
            .host()
            .parse::<IpAddr>()
            // Wasmtime exposes only a global name-lookup switch. Enabling it
            // for one hostname would let the guest query every hostname, so
            // this engine accepts only already-minted IP endpoints.
            .map_err(|_| WasiComponentEngineFault::Start)?;
        allowed.insert(SocketAddr::new(address, endpoint.port()));
    }
    if !allowed.is_empty() {
        let allowed = Arc::new(allowed);
        builder.allow_tcp(true).allow_ip_name_lookup(false);
        builder.socket_addr_check(move |address, use_| {
            let allowed = Arc::clone(&allowed);
            Box::pin(async move {
                match use_ {
                    SocketAddrUse::TcpConnect => allowed.contains(&address),
                    // TCP connect performs an implicit ephemeral bind first.
                    SocketAddrUse::TcpBind => address.ip().is_unspecified() && address.port() == 0,
                    SocketAddrUse::TcpListen
                    | SocketAddrUse::TcpAccept
                    | SocketAddrUse::UdpBind
                    | SocketAddrUse::UdpSend
                    | SocketAddrUse::UdpReceive => false,
                }
            })
        });
    }
    Ok(builder.build())
}

fn validate_component_abi(
    engine: &Engine,
    component: &Component,
    world: WasiComponentWorld,
) -> Result<(Vec<String>, Vec<String>), WasiComponentEngineFault> {
    let expected = WasiComponentAbiReport::exact_v1(world);
    let ty = component.component_type();
    let imports = ty
        .imports(engine)
        .map(|(name, _)| name.to_owned())
        .collect::<BTreeSet<_>>();
    let expected_imports = expected.imports().iter().cloned().collect::<BTreeSet<_>>();
    if !imports.is_subset(&expected_imports) {
        return Err(WasiComponentEngineFault::InvalidComponent);
    }
    let exports = ty.exports(engine).collect::<Vec<_>>();
    if exports.len() != 1 || exports[0].0 != WASI_CODE_PLUGIN_INTERFACE_V1 {
        return Err(WasiComponentEngineFault::InvalidComponent);
    }
    let ComponentItem::ComponentInstance(interface) = &exports[0].1.ty else {
        return Err(WasiComponentEngineFault::InvalidComponent);
    };
    let Some(initialize) = interface.get_export(engine, "initialize") else {
        return Err(WasiComponentEngineFault::InvalidComponent);
    };
    let Some(invoke) = interface.get_export(engine, "invoke") else {
        return Err(WasiComponentEngineFault::InvalidComponent);
    };
    let ComponentItem::ComponentFunc(initialize) = initialize.ty else {
        return Err(WasiComponentEngineFault::InvalidComponent);
    };
    let ComponentItem::ComponentFunc(invoke) = invoke.ty else {
        return Err(WasiComponentEngineFault::InvalidComponent);
    };
    if !valid_initialize_type(&initialize) || !valid_invoke_type(&invoke) {
        return Err(WasiComponentEngineFault::InvalidComponent);
    }
    Ok((
        imports.into_iter().collect(),
        exports
            .into_iter()
            .map(|(name, _)| name.to_owned())
            .collect(),
    ))
}

fn valid_initialize_type(function: &ComponentFunc) -> bool {
    if function.async_() {
        return false;
    }
    let params = function.params().collect::<Vec<_>>();
    let results = function.results().collect::<Vec<_>>();
    params.len() == 1
        && params[0].0 == "request"
        && initialize_request_type(&params[0].1)
        && results.len() == 1
        && result_type(&results[0], ready_type, protocol_error_type)
}

fn valid_invoke_type(function: &ComponentFunc) -> bool {
    if function.async_() {
        return false;
    }
    let params = function.params().collect::<Vec<_>>();
    let results = function.results().collect::<Vec<_>>();
    params.len() == 1
        && params[0].0 == "request"
        && invocation_request_type(&params[0].1)
        && results.len() == 1
        && result_type(&results[0], byte_list_type, invocation_error_type)
}

fn result_type(ty: &Type, ok: fn(&Type) -> bool, error: fn(&Type) -> bool) -> bool {
    let Type::Result(result) = ty else {
        return false;
    };
    result.ok().is_some_and(|ty| ok(&ty)) && result.err().is_some_and(|ty| error(&ty))
}

type TypePredicate = fn(&Type) -> bool;
type RecordFieldShape<'a> = (&'a str, TypePredicate);

fn record_type(ty: &Type, expected: &[RecordFieldShape<'_>]) -> bool {
    let Type::Record(record) = ty else {
        return false;
    };
    let fields = record.fields().collect::<Vec<_>>();
    fields.len() == expected.len()
        && fields
            .iter()
            .zip(expected)
            .all(|(field, (name, predicate))| field.name == *name && predicate(&field.ty))
}

fn initialize_request_type(ty: &Type) -> bool {
    record_type(
        ty,
        &[
            ("protocol-version", u32_type),
            ("session-id", string_type),
            ("package-id", string_type),
            ("package-version", string_type),
            ("package-digest", string_type),
            ("component-digest", string_type),
            ("granted-capabilities", permission_list_type),
            ("contributions", contribution_list_type),
        ],
    )
}

fn ready_type(ty: &Type) -> bool {
    record_type(
        ty,
        &[
            ("protocol-version", u32_type),
            ("session-id", string_type),
            ("package-id", string_type),
            ("package-version", string_type),
            ("package-digest", string_type),
            ("component-digest", string_type),
            ("accepted-capabilities", permission_list_type),
            ("contributions", contribution_list_type),
        ],
    )
}

fn invocation_request_type(ty: &Type) -> bool {
    record_type(
        ty,
        &[
            ("protocol-version", u32_type),
            ("session-id", string_type),
            ("request-id", u64_type),
            ("contribution", contribution_type),
            ("operation", string_type),
            ("input-json", byte_list_type),
        ],
    )
}

fn contribution_type(ty: &Type) -> bool {
    record_type(
        ty,
        &[("kind", contribution_kind_type), ("name", string_type)],
    )
}

fn contribution_list_type(ty: &Type) -> bool {
    list_type(ty, contribution_type)
}

fn permission_list_type(ty: &Type) -> bool {
    list_type(ty, permission_type)
}

fn byte_list_type(ty: &Type) -> bool {
    list_type(ty, u8_type)
}

fn list_type(ty: &Type, element: fn(&Type) -> bool) -> bool {
    matches!(ty, Type::List(list) if element(&list.ty()))
}

fn contribution_kind_type(ty: &Type) -> bool {
    enum_type(
        ty,
        &["skill", "command", "agent", "hook", "theme", "provider"],
    )
}

fn permission_type(ty: &Type) -> bool {
    enum_type(
        ty,
        &[
            "filesystem-read",
            "filesystem-write",
            "network-access",
            "process-spawn",
            "credential-use",
            "mcp-connect",
            "hook-registration",
            "contribution-override",
        ],
    )
}

fn protocol_error_type(ty: &Type) -> bool {
    variant_type(
        ty,
        &[
            "invalid-request",
            "identity-mismatch",
            "capability-mismatch",
            "contribution-mismatch",
            "failed",
        ],
    )
}

fn invocation_error_type(ty: &Type) -> bool {
    variant_type(ty, &["invalid-request", "denied", "unavailable", "failed"])
}

fn enum_type(ty: &Type, expected: &[&str]) -> bool {
    let Type::Enum(value) = ty else {
        return false;
    };
    value.names().eq(expected.iter().copied())
}

fn variant_type(ty: &Type, expected: &[&str]) -> bool {
    let Type::Variant(value) = ty else {
        return false;
    };
    let cases = value.cases().collect::<Vec<_>>();
    cases.len() == expected.len()
        && cases
            .iter()
            .zip(expected)
            .all(|(case, expected)| case.name == *expected && case.ty.is_none())
}

const fn u8_type(ty: &Type) -> bool {
    matches!(ty, Type::U8)
}

const fn u32_type(ty: &Type) -> bool {
    matches!(ty, Type::U32)
}

const fn u64_type(ty: &Type) -> bool {
    matches!(ty, Type::U64)
}

const fn string_type(ty: &Type) -> bool {
    matches!(ty, Type::String)
}

struct WasmCommand {
    request: Vec<u8>,
    cancellation: CodePluginCancellationToken,
    reply: mpsc::SyncSender<Result<Vec<u8>, CodePluginTransportFault>>,
}

struct WasmNotification {
    exit: Option<CodePluginExit>,
    listener: Option<ExitListener>,
    listener_installed: bool,
}

struct WasmShared {
    engine: Engine,
    lifecycle: CodePluginCancellationToken,
    interrupt: Arc<AtomicBool>,
    notification: Mutex<WasmNotification>,
    driver: Mutex<Option<JoinHandle<()>>>,
    driver_id: Mutex<Option<ThreadId>>,
    shutdown_gate: Mutex<()>,
}

impl WasmShared {
    fn publish_exit(&self, exit: CodePluginExit) {
        let listener = {
            let mut state = lock(&self.notification);
            if state.exit.is_some() {
                return;
            }
            state.exit = Some(exit);
            state.listener.take()
        };
        if let Some(listener) = listener {
            listener(exit);
        }
    }

    fn install_listener(&self, listener: ExitListener) -> Result<(), CodePluginTransportFault> {
        let exit = {
            let mut state = lock(&self.notification);
            if state.listener_installed {
                return Err(CodePluginTransportFault::Protocol);
            }
            state.listener_installed = true;
            match state.exit {
                Some(exit) => Some(exit),
                None => {
                    state.listener = Some(Arc::clone(&listener));
                    None
                }
            }
        };
        if let Some(exit) = exit {
            listener(exit);
        }
        Ok(())
    }

    fn stop_and_join(&self) {
        let _gate = lock(&self.shutdown_gate);
        self.lifecycle.cancel();
        self.interrupt.store(true, Ordering::SeqCst);
        self.engine.increment_epoch();
        let driver = lock(&self.driver).take();
        if let Some(driver) = driver {
            let current = thread::current().id();
            let is_driver = lock(&self.driver_id).as_ref() == Some(&current);
            if !is_driver {
                let _settled = driver.join();
            }
        }
    }
}

struct WasmtimeCodePluginProcess {
    sender: tokio::sync::mpsc::UnboundedSender<WasmCommand>,
    shared: Arc<WasmShared>,
    exchange_gate: Mutex<()>,
}

impl WasmtimeCodePluginProcess {
    fn spawn(
        engine: Engine,
        component: Component,
        wasi: WasiCtx,
        cancellation: CodePluginCancellationToken,
    ) -> Result<Arc<dyn CodePluginProcess>, WasiComponentEngineFault> {
        let lifecycle = CodePluginCancellationToken::new();
        let interrupt = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(WasmShared {
            engine: engine.clone(),
            lifecycle: lifecycle.clone(),
            interrupt: Arc::clone(&interrupt),
            notification: Mutex::new(WasmNotification {
                exit: None,
                listener: None,
                listener_installed: false,
            }),
            driver: Mutex::new(None),
            driver_id: Mutex::new(None),
            shutdown_gate: Mutex::new(()),
        });
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (started, startup) = mpsc::sync_channel(1);
        let thread_shared = Arc::clone(&shared);
        let driver = thread::Builder::new()
            .name("heycode-wasi-plugin".to_owned())
            .spawn(move || {
                *lock(&thread_shared.driver_id) = Some(thread::current().id());
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(_) => {
                        let _sent = started.send(Err(WasiComponentEngineFault::Unavailable));
                        return;
                    }
                };
                let limits = StoreLimitsBuilder::new()
                    .memory_size(MAX_COMPONENT_MEMORY_BYTES)
                    .table_elements(MAX_COMPONENT_TABLE_ELEMENTS)
                    .instances(64)
                    .tables(64)
                    .memories(16)
                    .trap_on_grow_failure(true)
                    .build();
                let mut store = Store::new(
                    &engine,
                    WasiState {
                        context: wasi,
                        table: ResourceTable::new(),
                        limits,
                        interrupt,
                    },
                );
                store.limiter(|state| &mut state.limits);
                store.epoch_deadline_callback(|state| {
                    Ok(if state.data().interrupt.load(Ordering::SeqCst) {
                        UpdateDeadline::Interrupt
                    } else {
                        UpdateDeadline::Continue(1)
                    })
                });
                store.set_epoch_deadline(1);
                if store.set_fuel(MAX_COMPONENT_FUEL).is_err() {
                    let _sent = started.send(Err(WasiComponentEngineFault::Start));
                    return;
                }
                let mut linker = Linker::new(&engine);
                if wasmtime_wasi::p3::add_to_linker(&mut linker).is_err() {
                    let _sent = started.send(Err(WasiComponentEngineFault::Unavailable));
                    return;
                }
                let instantiated =
                    runtime.block_on(linker.instantiate_async(&mut store, &component));
                let instance = match instantiated {
                    Ok(instance) => instance,
                    Err(_) => {
                        let _sent = started.send(Err(WasiComponentEngineFault::Start));
                        return;
                    }
                };
                let functions = match plugin_functions(&mut store, instance) {
                    Ok(functions) => functions,
                    Err(fault) => {
                        let _sent = started.send(Err(fault));
                        return;
                    }
                };
                if cancellation.is_cancelled() {
                    lifecycle.cancel();
                }
                if started.send(Ok(())).is_err() {
                    lifecycle.cancel();
                }
                let exit = runtime.block_on(wasm_driver(
                    receiver, &engine, &mut store, functions, lifecycle,
                ));
                thread_shared.publish_exit(exit);
            })
            .map_err(|_| WasiComponentEngineFault::Unavailable)?;
        *lock(&shared.driver) = Some(driver);
        match startup.recv() {
            Ok(Ok(())) => Ok(Arc::new(Self {
                sender,
                shared,
                exchange_gate: Mutex::new(()),
            })),
            Ok(Err(fault)) => {
                shared.stop_and_join();
                Err(fault)
            }
            Err(_) => {
                shared.stop_and_join();
                Err(WasiComponentEngineFault::Start)
            }
        }
    }
}

impl CodePluginProcess for WasmtimeCodePluginProcess {
    fn exchange(
        &self,
        request: &[u8],
        cancellation: &CodePluginCancellationToken,
    ) -> Result<Vec<u8>, CodePluginTransportFault> {
        if cancellation.is_cancelled() {
            return Err(CodePluginTransportFault::Cancelled);
        }
        if request.is_empty() || request.len() > CODE_PLUGIN_MAX_FRAME_BYTES {
            return Err(CodePluginTransportFault::Protocol);
        }
        let _gate = lock(&self.exchange_gate);
        if cancellation.is_cancelled() {
            return Err(CodePluginTransportFault::Cancelled);
        }
        if lock(&self.shared.notification).exit.is_some() {
            return Err(CodePluginTransportFault::Crashed);
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.sender
            .send(WasmCommand {
                request: request.to_vec(),
                cancellation: cancellation.clone(),
                reply,
            })
            .map_err(|_| CodePluginTransportFault::Crashed)?;
        response
            .recv()
            .unwrap_or(Err(CodePluginTransportFault::Crashed))
    }

    fn set_exit_listener(&self, listener: ExitListener) -> Result<(), CodePluginTransportFault> {
        self.shared.install_listener(listener)
    }

    fn shutdown(&self) {
        self.shared.stop_and_join();
    }
}

impl Drop for WasmtimeCodePluginProcess {
    fn drop(&mut self) {
        self.shared.stop_and_join();
    }
}

struct PluginFunctions {
    initialize: Func,
    invoke: Func,
}

fn plugin_functions(
    store: &mut Store<WasiState>,
    instance: Instance,
) -> Result<PluginFunctions, WasiComponentEngineFault> {
    let interface = instance
        .get_export_index(&mut *store, None, WASI_CODE_PLUGIN_INTERFACE_V1)
        .ok_or(WasiComponentEngineFault::InvalidComponent)?;
    let initialize = instance
        .get_export_index(&mut *store, Some(&interface), "initialize")
        .and_then(|index| instance.get_func(&mut *store, index))
        .ok_or(WasiComponentEngineFault::InvalidComponent)?;
    let invoke = instance
        .get_export_index(&mut *store, Some(&interface), "invoke")
        .and_then(|index| instance.get_func(&mut *store, index))
        .ok_or(WasiComponentEngineFault::InvalidComponent)?;
    Ok(PluginFunctions { initialize, invoke })
}

async fn wasm_driver(
    mut receiver: tokio::sync::mpsc::UnboundedReceiver<WasmCommand>,
    engine: &Engine,
    store: &mut Store<WasiState>,
    functions: PluginFunctions,
    lifecycle: CodePluginCancellationToken,
) -> CodePluginExit {
    loop {
        let command = tokio::select! {
            biased;
            () = lifecycle.cancelled() => return CodePluginExit::Cancelled,
            command = receiver.recv() => command,
        };
        let Some(command) = command else {
            return CodePluginExit::Cancelled;
        };
        let response = invoke_wasm(engine, store, &functions, &command, &lifecycle).await;
        match response {
            Ok(response) => {
                let _sent = command.reply.send(Ok(response));
            }
            Err(fault) => {
                let exit = if fault == CodePluginTransportFault::Cancelled {
                    CodePluginExit::Cancelled
                } else {
                    CodePluginExit::Crashed
                };
                let _sent = command.reply.send(Err(fault));
                return exit;
            }
        }
    }
}

async fn invoke_wasm(
    engine: &Engine,
    store: &mut Store<WasiState>,
    functions: &PluginFunctions,
    command: &WasmCommand,
    lifecycle: &CodePluginCancellationToken,
) -> Result<Vec<u8>, CodePluginTransportFault> {
    if command.cancellation.is_cancelled() {
        return Err(CodePluginTransportFault::Cancelled);
    }
    store
        .set_fuel(MAX_COMPONENT_FUEL)
        .map_err(|_| CodePluginTransportFault::Unavailable)?;
    let interrupt = Arc::clone(&store.data().interrupt);
    interrupt.store(false, Ordering::SeqCst);
    let frame: HostFrame =
        serde_json::from_slice(&command.request).map_err(|_| CodePluginTransportFault::Protocol)?;
    let call = async {
        match frame {
            HostFrame::Initialize(frame) => {
                call_initialize(store, &functions.initialize, frame).await
            }
            HostFrame::Invoke(frame) => call_invoke(store, &functions.invoke, frame).await,
        }
    };
    tokio::pin!(call);
    tokio::select! {
        biased;
        () = command.cancellation.cancelled() => {
            interrupt.store(true, Ordering::SeqCst);
            engine.increment_epoch();
            let _settled = call.await;
            Err(CodePluginTransportFault::Cancelled)
        }
        () = lifecycle.cancelled() => {
            interrupt.store(true, Ordering::SeqCst);
            engine.increment_epoch();
            let _settled = call.await;
            Err(CodePluginTransportFault::Cancelled)
        }
        result = &mut call => result,
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum HostFrame {
    Initialize(InitializeFrame),
    Invoke(InvokeFrame),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InitializeFrame {
    protocol_version: u32,
    session_id: String,
    package_id: String,
    package_version: String,
    package_digest: String,
    executable_digest: String,
    granted_capabilities: Vec<PluginPermission>,
    contributions: Vec<WireContribution>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InvokeFrame {
    protocol_version: u32,
    session_id: String,
    request_id: u64,
    contribution: WireContribution,
    operation: String,
    input: Value,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WireContribution {
    kind: ContributionKind,
    name: String,
}

#[derive(Serialize)]
struct ReadyFrame {
    protocol_version: u32,
    kind: &'static str,
    session_id: String,
    package_id: String,
    package_version: String,
    package_digest: String,
    executable_digest: String,
    accepted_capabilities: Vec<PluginPermission>,
    contributions: Vec<WireContribution>,
}

#[derive(Serialize)]
struct ResultFrame {
    protocol_version: u32,
    kind: &'static str,
    session_id: String,
    request_id: u64,
    output: Value,
}

#[derive(Serialize)]
struct ErrorFrame {
    protocol_version: u32,
    kind: &'static str,
    session_id: String,
    request_id: u64,
    code: CodePluginRemoteErrorCode,
}

async fn call_initialize(
    store: &mut Store<WasiState>,
    function: &Func,
    frame: InitializeFrame,
) -> Result<Vec<u8>, CodePluginTransportFault> {
    let request = Val::Record(vec![
        (
            "protocol-version".to_owned(),
            Val::U32(frame.protocol_version),
        ),
        (
            "session-id".to_owned(),
            Val::String(frame.session_id.clone()),
        ),
        (
            "package-id".to_owned(),
            Val::String(frame.package_id.clone()),
        ),
        (
            "package-version".to_owned(),
            Val::String(frame.package_version.clone()),
        ),
        (
            "package-digest".to_owned(),
            Val::String(frame.package_digest.clone()),
        ),
        (
            "component-digest".to_owned(),
            Val::String(frame.executable_digest.clone()),
        ),
        (
            "granted-capabilities".to_owned(),
            Val::List(
                frame
                    .granted_capabilities
                    .iter()
                    .map(|permission| Val::Enum(wit_permission(*permission).to_owned()))
                    .collect(),
            ),
        ),
        (
            "contributions".to_owned(),
            Val::List(frame.contributions.iter().map(wit_contribution).collect()),
        ),
    ]);
    let mut results = [Val::Bool(false)];
    function
        .call_async(&mut *store, &[request], &mut results)
        .await
        .map_err(|_| CodePluginTransportFault::Crashed)?;
    let ready = match &results[0] {
        Val::Result(Ok(Some(ready))) => ready_record(ready)?,
        Val::Result(Err(_)) => return Err(CodePluginTransportFault::Protocol),
        _ => return Err(CodePluginTransportFault::Protocol),
    };
    let response = ReadyFrame {
        protocol_version: ready.protocol_version,
        kind: "ready",
        session_id: ready.session_id,
        package_id: ready.package_id,
        package_version: ready.package_version,
        package_digest: ready.package_digest,
        executable_digest: ready.component_digest,
        accepted_capabilities: ready.accepted_capabilities,
        contributions: ready.contributions,
    };
    encode_response(&response)
}

async fn call_invoke(
    store: &mut Store<WasiState>,
    function: &Func,
    frame: InvokeFrame,
) -> Result<Vec<u8>, CodePluginTransportFault> {
    let input = serde_json::to_vec(&frame.input).map_err(|_| CodePluginTransportFault::Protocol)?;
    if input.len() > CODE_PLUGIN_MAX_FRAME_BYTES {
        return Err(CodePluginTransportFault::Protocol);
    }
    let request = Val::Record(vec![
        (
            "protocol-version".to_owned(),
            Val::U32(frame.protocol_version),
        ),
        (
            "session-id".to_owned(),
            Val::String(frame.session_id.clone()),
        ),
        ("request-id".to_owned(), Val::U64(frame.request_id)),
        (
            "contribution".to_owned(),
            wit_contribution(&frame.contribution),
        ),
        ("operation".to_owned(), Val::String(frame.operation)),
        (
            "input-json".to_owned(),
            Val::List(input.into_iter().map(Val::U8).collect()),
        ),
    ]);
    let mut results = [Val::Bool(false)];
    function
        .call_async(&mut *store, &[request], &mut results)
        .await
        .map_err(|_| CodePluginTransportFault::Crashed)?;
    match &results[0] {
        Val::Result(Ok(Some(bytes))) => {
            let bytes = val_bytes(bytes)?;
            let output: Value =
                serde_json::from_slice(&bytes).map_err(|_| CodePluginTransportFault::Protocol)?;
            encode_response(&ResultFrame {
                protocol_version: frame.protocol_version,
                kind: "result",
                session_id: frame.session_id,
                request_id: frame.request_id,
                output,
            })
        }
        Val::Result(Err(Some(error))) => {
            let code = invocation_error(error)?;
            encode_response(&ErrorFrame {
                protocol_version: frame.protocol_version,
                kind: "error",
                session_id: frame.session_id,
                request_id: frame.request_id,
                code,
            })
        }
        _ => Err(CodePluginTransportFault::Protocol),
    }
}

struct ReadyValue {
    protocol_version: u32,
    session_id: String,
    package_id: String,
    package_version: String,
    package_digest: String,
    component_digest: String,
    accepted_capabilities: Vec<PluginPermission>,
    contributions: Vec<WireContribution>,
}

fn ready_record(value: &Val) -> Result<ReadyValue, CodePluginTransportFault> {
    let Val::Record(fields) = value else {
        return Err(CodePluginTransportFault::Protocol);
    };
    let fields = fields
        .iter()
        .map(|(name, value)| (name.as_str(), value))
        .collect::<HashMap<_, _>>();
    if fields.len() != 8 {
        return Err(CodePluginTransportFault::Protocol);
    }
    Ok(ReadyValue {
        protocol_version: val_u32(field(&fields, "protocol-version")?)?,
        session_id: val_string(field(&fields, "session-id")?)?,
        package_id: val_string(field(&fields, "package-id")?)?,
        package_version: val_string(field(&fields, "package-version")?)?,
        package_digest: val_string(field(&fields, "package-digest")?)?,
        component_digest: val_string(field(&fields, "component-digest")?)?,
        accepted_capabilities: val_permissions(field(&fields, "accepted-capabilities")?)?,
        contributions: val_contributions(field(&fields, "contributions")?)?,
    })
}

fn field<'a>(
    fields: &'a HashMap<&str, &'a Val>,
    name: &str,
) -> Result<&'a Val, CodePluginTransportFault> {
    fields
        .get(name)
        .copied()
        .ok_or(CodePluginTransportFault::Protocol)
}

fn val_u32(value: &Val) -> Result<u32, CodePluginTransportFault> {
    match value {
        Val::U32(value) => Ok(*value),
        _ => Err(CodePluginTransportFault::Protocol),
    }
}

fn val_string(value: &Val) -> Result<String, CodePluginTransportFault> {
    match value {
        Val::String(value) => Ok(value.clone()),
        _ => Err(CodePluginTransportFault::Protocol),
    }
}

fn val_bytes(value: &Val) -> Result<Vec<u8>, CodePluginTransportFault> {
    let Val::List(values) = value else {
        return Err(CodePluginTransportFault::Protocol);
    };
    if values.len() > CODE_PLUGIN_MAX_FRAME_BYTES {
        return Err(CodePluginTransportFault::Protocol);
    }
    values
        .iter()
        .map(|value| match value {
            Val::U8(byte) => Ok(*byte),
            _ => Err(CodePluginTransportFault::Protocol),
        })
        .collect()
}

fn val_permissions(value: &Val) -> Result<Vec<PluginPermission>, CodePluginTransportFault> {
    let Val::List(values) = value else {
        return Err(CodePluginTransportFault::Protocol);
    };
    values
        .iter()
        .map(|value| match value {
            Val::Enum(name) => permission_from_wit(name),
            _ => Err(CodePluginTransportFault::Protocol),
        })
        .collect()
}

fn val_contributions(value: &Val) -> Result<Vec<WireContribution>, CodePluginTransportFault> {
    let Val::List(values) = value else {
        return Err(CodePluginTransportFault::Protocol);
    };
    values.iter().map(contribution_from_wit).collect()
}

fn contribution_from_wit(value: &Val) -> Result<WireContribution, CodePluginTransportFault> {
    let Val::Record(fields) = value else {
        return Err(CodePluginTransportFault::Protocol);
    };
    if fields.len() != 2 {
        return Err(CodePluginTransportFault::Protocol);
    }
    let fields = fields
        .iter()
        .map(|(name, value)| (name.as_str(), value))
        .collect::<HashMap<_, _>>();
    let kind = match field(&fields, "kind")? {
        Val::Enum(name) => contribution_from_name(name)?,
        _ => return Err(CodePluginTransportFault::Protocol),
    };
    Ok(WireContribution {
        kind,
        name: val_string(field(&fields, "name")?)?,
    })
}

fn wit_contribution(contribution: &WireContribution) -> Val {
    Val::Record(vec![
        (
            "kind".to_owned(),
            Val::Enum(contribution_name(contribution.kind).to_owned()),
        ),
        ("name".to_owned(), Val::String(contribution.name.clone())),
    ])
}

const fn contribution_name(kind: ContributionKind) -> &'static str {
    match kind {
        ContributionKind::Skill => "skill",
        ContributionKind::Command => "command",
        ContributionKind::Agent => "agent",
        ContributionKind::Hook => "hook",
        ContributionKind::Theme => "theme",
        ContributionKind::Provider => "provider",
        ContributionKind::Mcp => "mcp",
    }
}

fn contribution_from_name(name: &str) -> Result<ContributionKind, CodePluginTransportFault> {
    match name {
        "skill" => Ok(ContributionKind::Skill),
        "command" => Ok(ContributionKind::Command),
        "agent" => Ok(ContributionKind::Agent),
        "hook" => Ok(ContributionKind::Hook),
        "theme" => Ok(ContributionKind::Theme),
        "provider" => Ok(ContributionKind::Provider),
        _ => Err(CodePluginTransportFault::Protocol),
    }
}

const fn wit_permission(permission: PluginPermission) -> &'static str {
    match permission {
        PluginPermission::FilesystemRead => "filesystem-read",
        PluginPermission::FilesystemWrite => "filesystem-write",
        PluginPermission::NetworkAccess => "network-access",
        PluginPermission::ProcessSpawn => "process-spawn",
        PluginPermission::CredentialUse => "credential-use",
        PluginPermission::McpConnect => "mcp-connect",
        PluginPermission::HookRegistration => "hook-registration",
        PluginPermission::ContributionOverride => "contribution-override",
    }
}

fn permission_from_wit(name: &str) -> Result<PluginPermission, CodePluginTransportFault> {
    match name {
        "filesystem-read" => Ok(PluginPermission::FilesystemRead),
        "filesystem-write" => Ok(PluginPermission::FilesystemWrite),
        "network-access" => Ok(PluginPermission::NetworkAccess),
        "process-spawn" => Ok(PluginPermission::ProcessSpawn),
        "credential-use" => Ok(PluginPermission::CredentialUse),
        "mcp-connect" => Ok(PluginPermission::McpConnect),
        "hook-registration" => Ok(PluginPermission::HookRegistration),
        "contribution-override" => Ok(PluginPermission::ContributionOverride),
        _ => Err(CodePluginTransportFault::Protocol),
    }
}

fn invocation_error(value: &Val) -> Result<CodePluginRemoteErrorCode, CodePluginTransportFault> {
    let Val::Variant(name, None) = value else {
        return Err(CodePluginTransportFault::Protocol);
    };
    match name.as_str() {
        "invalid-request" => Ok(CodePluginRemoteErrorCode::InvalidRequest),
        "denied" => Ok(CodePluginRemoteErrorCode::Denied),
        "unavailable" => Ok(CodePluginRemoteErrorCode::Unavailable),
        "failed" => Ok(CodePluginRemoteErrorCode::Failed),
        _ => Err(CodePluginTransportFault::Protocol),
    }
}

fn encode_response<T: Serialize>(response: &T) -> Result<Vec<u8>, CodePluginTransportFault> {
    let bytes = serde_json::to_vec(response).map_err(|_| CodePluginTransportFault::Protocol)?;
    if bytes.len() > CODE_PLUGIN_MAX_FRAME_BYTES {
        Err(CodePluginTransportFault::Protocol)
    } else {
        Ok(bytes)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}
