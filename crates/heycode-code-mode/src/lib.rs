//! Bounded JavaScript orchestration with explicitly admitted host tools.
//!
//! The VM exposes no filesystem, network, process, module loader, or timer APIs.
//! Every external action crosses [`ToolHost`], which must apply current approval,
//! policy, cancellation, and durable operation recording at execution time.

use async_trait::async_trait;
use rquickjs::{AsyncContext, AsyncRuntime, CatchResultExt, Function, Promise, prelude::Async};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

/// Authority boundary for calls made by a script. Implementations must not
/// invoke a raw tool registry in place of the agent's guarded execution path.
#[async_trait]
pub trait ToolHost: Send + Sync {
    /// Admit, record, execute, and record the result of one call.
    async fn call(
        &self,
        name: String,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> anyhow::Result<Value>;
}

/// Resource limits applied to each fresh VM. Values outside hard limits fail.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    /// Maximum wall time including tool calls and approvals.
    pub timeout_ms: u64,
    /// QuickJS heap limit.
    pub memory_bytes: usize,
    /// Maximum UTF-8 source size.
    pub source_bytes: usize,
    /// Maximum total emitted output plus returned JSON size.
    pub output_bytes: usize,
    /// Maximum size of one tool argument or result JSON.
    pub tool_bytes: usize,
    /// Maximum admitted tool requests including queued calls.
    pub tool_calls: usize,
    /// Maximum concurrent host calls.
    pub parallel_calls: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout_ms: 60_000,
            memory_bytes: 32 * 1024 * 1024,
            source_bytes: 64 * 1024,
            output_bytes: 64 * 1024,
            tool_bytes: 1024 * 1024,
            tool_calls: 64,
            parallel_calls: 4,
        }
    }
}
impl Limits {
    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (1..=600_000).contains(&self.timeout_ms),
            "timeout_ms must be 1..600000"
        );
        anyhow::ensure!(
            (1024 * 1024..=128 * 1024 * 1024).contains(&self.memory_bytes),
            "memory_bytes outside 1..128 MiB"
        );
        anyhow::ensure!(
            (1..=1024 * 1024).contains(&self.source_bytes),
            "source_bytes outside 1..1048576"
        );
        anyhow::ensure!(
            (1..=1024 * 1024).contains(&self.output_bytes),
            "output_bytes outside 1..1048576"
        );
        anyhow::ensure!(
            (1..=4 * 1024 * 1024).contains(&self.tool_bytes),
            "tool_bytes outside 1..4194304"
        );
        anyhow::ensure!(
            (1..=512).contains(&self.tool_calls),
            "tool_calls must be 1..512"
        );
        anyhow::ensure!(
            (1..=16).contains(&self.parallel_calls),
            "parallel_calls must be 1..16"
        );
        Ok(())
    }
}

/// A script's bounded output. Tool side effects remain recorded by the host
/// even when JavaScript later fails; scripts are never automatically replayed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptResult {
    /// Explicit values emitted with `text(value)` or `console.log(value)`.
    pub output: Vec<Value>,
    /// JSON return value, or null for undefined.
    pub value: Value,
    /// Number of attempted host calls, including refused calls.
    pub tool_calls: usize,
}

/// Run an async JavaScript function body in a fresh restricted VM.
///
/// `tools[name](arguments)` returns the guarded host result. `text(value)` emits
/// JSON. Normal JavaScript loops, branches and Promise.all are supported.
/// Tool names must be explicitly selected; access to any other name fails.
///
/// # Errors
/// Returns validation, cancellation, deadline, VM, or resource-limit failures.
pub async fn run(
    source: &str,
    selected_tools: BTreeSet<String>,
    host: Arc<dyn ToolHost>,
    limits: Limits,
    cancellation: CancellationToken,
) -> anyhow::Result<ScriptResult> {
    limits.validate()?;
    anyhow::ensure!(
        source.len() <= limits.source_bytes,
        "script source limit exceeded"
    );
    anyhow::ensure!(selected_tools.len() <= 512, "selected tool limit exceeded");
    anyhow::ensure!(!cancellation.is_cancelled(), "script cancelled");
    let cancellation = cancellation.child_token();
    let _cancel_on_exit = cancellation.clone().drop_guard();
    let deadline = Instant::now() + Duration::from_millis(limits.timeout_ms);
    let runtime = AsyncRuntime::new()?;
    runtime.set_memory_limit(limits.memory_bytes).await;
    runtime.set_max_stack_size(256 * 1024).await;
    let interrupt = cancellation.clone();
    runtime
        .set_interrupt_handler(Some(Box::new(move || {
            interrupt.is_cancelled() || Instant::now() >= deadline
        })))
        .await;
    let context = AsyncContext::full(&runtime).await?;
    let count = Arc::new(AtomicUsize::new(0));
    let output = Arc::new(Mutex::new((0usize, Vec::<Value>::new())));
    let permits = Arc::new(Semaphore::new(limits.parallel_calls));
    let names = serde_json::to_string(&selected_tools)?;
    let program = format!(
        r#"(async () => {{
        'use strict';
        const call = globalThis.__hostCall, emit = globalThis.__emit;
        const parse = JSON.parse, stringify = JSON.stringify;
        const tools = Object.create(null);
        for (const name of {names}) Object.defineProperty(tools, name, {{value: async (args = {{}}) => {{
            const raw = stringify(args);
            if (typeof raw !== 'string') throw new Error('tool arguments must be JSON');
            const envelope = parse(await call(name, raw));
            if (!envelope.ok) throw new Error(envelope.error);
            return envelope.value;
        }}}});
        Object.freeze(tools);
        const text = value => {{
            const raw = stringify(value === undefined ? null : value);
            const error = emit(raw);
            if (error) throw new Error(error);
        }};
        const console = Object.freeze({{ log: (...args) => text(args.length === 1 ? args[0] : args) }});
        const value = await (async () => {{
{source}
        }})();
        return stringify(value === undefined ? null : value);
    }})()"#
    );
    let call_count = count.clone();
    let call_cancel = cancellation.clone();
    let emitted = output.clone();
    let execute = context.async_with(async move |ctx| -> anyhow::Result<String> {
        let callback = Function::new(ctx.clone(), Async(move |name: String, raw: String| {
            let host = host.clone();
            let cancellation = call_cancel.clone();
            let allowed = selected_tools.contains(&name);
            let sequence = call_count.fetch_add(1, Ordering::Relaxed);
            let permits = permits.clone();
            async move {
                let result = async {
                    anyhow::ensure!(allowed, "tool is not selected");
                    anyhow::ensure!(sequence < limits.tool_calls, "tool call limit exceeded");
                    anyhow::ensure!(raw.len() <= limits.tool_bytes, "tool argument limit exceeded");
                    let arguments: Value = serde_json::from_str(&raw)?;
                    let _permit = permits.acquire().await?;
                    anyhow::ensure!(!cancellation.is_cancelled(), "script cancelled");
                    let value = host.call(name, arguments, cancellation.clone()).await?;
                    anyhow::ensure!(serde_json::to_vec(&value)?.len() <= limits.tool_bytes, "tool result limit exceeded");
                    Ok::<_, anyhow::Error>(value)
                };
                let result = tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => Err(anyhow::anyhow!("script cancelled")),
                    result = result => result,
                };
                let envelope = match result {
                    Ok(value) => json!({"ok":true,"value":value}),
                    Err(error) => json!({"ok":false,"error":error.to_string().chars().take(2048).collect::<String>()}),
                };
                Ok::<_, rquickjs::Error>(envelope.to_string())
            }
        }))?;
        ctx.globals().set("__hostCall", callback)?;
        let emit = Function::new(ctx.clone(), move |raw: String| -> String {
            let mut state = emitted.lock().unwrap_or_else(|error| error.into_inner());
            if raw.len() > limits.output_bytes.saturating_sub(state.0) {
                return "script output limit exceeded".into();
            }
            match serde_json::from_str(&raw) {
                Ok(value) => { state.0 += raw.len(); state.1.push(value); String::new() }
                Err(_) => "output must be JSON".into(),
            }
        })?;
        ctx.globals().set("__emit", emit)?;
        let promise = ctx.eval::<Promise, _>(program).catch(&ctx).map_err(|error| anyhow::anyhow!("JavaScript: {error}"))?;
        promise.into_future::<String>().await.catch(&ctx).map_err(|error| anyhow::anyhow!("JavaScript: {error}"))
    });
    let raw = tokio::select! {
        biased;
        _ = cancellation.cancelled() => anyhow::bail!("script cancelled"),
        result = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), execute) => {
            result.map_err(|_| anyhow::anyhow!("script deadline exceeded"))??
        }
    };
    anyhow::ensure!(!cancellation.is_cancelled(), "script cancelled");
    anyhow::ensure!(Instant::now() < deadline, "script deadline exceeded");
    let mut output = output.lock().unwrap_or_else(|error| error.into_inner());
    anyhow::ensure!(
        raw.len() <= limits.output_bytes.saturating_sub(output.0),
        "script output limit exceeded"
    );
    Ok(ScriptResult {
        output: std::mem::take(&mut output.1),
        value: serde_json::from_str(&raw)?,
        tool_calls: count.load(Ordering::Relaxed),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Host {
        calls: Mutex<Vec<(String, Value)>>,
    }
    #[async_trait]
    impl ToolHost for Host {
        async fn call(
            &self,
            name: String,
            args: Value,
            token: CancellationToken,
        ) -> anyhow::Result<Value> {
            self.calls
                .lock()
                .unwrap()
                .push((name.clone(), args.clone()));
            match name.as_str() {
                "double" => Ok(json!(args["n"].as_i64().unwrap_or(0) * 2)),
                "deny" => anyhow::bail!("denied by current plan policy"),
                "wait" => {
                    token.cancelled().await;
                    anyhow::bail!("cancelled")
                }
                _ => Ok(args),
            }
        }
    }
    async fn execute(source: &str, limits: Limits) -> anyhow::Result<ScriptResult> {
        run(
            source,
            ["double", "deny", "wait"].map(String::from).into(),
            Arc::new(Host::default()),
            limits,
            CancellationToken::new(),
        )
        .await
    }
    #[tokio::test]
    async fn branches_loops_parallel_results_and_output() {
        let result = execute("const values = await Promise.all([1,2,3].map(n => tools.double({n}))); let total=0; for(const n of values) if(n>2) total+=n; text(values); return {total};", Limits::default()).await.unwrap();
        assert_eq!(result.value, json!({"total":10}));
        assert_eq!(result.output, vec![json!([2, 4, 6])]);
        assert_eq!(result.tool_calls, 3);
    }
    #[tokio::test]
    async fn no_ambient_process_network_filesystem_or_module_access() {
        let result = execute("return [typeof process, typeof require, typeof fetch, typeof std, typeof os, typeof setTimeout];", Limits::default()).await.unwrap();
        assert_eq!(
            result.value,
            json!([
                "undefined",
                "undefined",
                "undefined",
                "undefined",
                "undefined",
                "undefined"
            ])
        );
        assert!(
            execute("return await import('node:fs');", Limits::default())
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn host_bridge_always_enforces_selected_tools() {
        let host = Arc::new(Host::default());
        let result = run(
            "return JSON.parse(await __hostCall('forbidden','{}'));",
            BTreeSet::new(),
            host.clone(),
            Limits::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result.value["ok"], false);
        assert!(host.calls.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn rejection_can_be_handled_without_losing_prior_results() {
        let result = execute("const first = await tools.double({n:3}); try { await tools.deny({}); } catch(e) { text(e.message); } return first;", Limits::default()).await.unwrap();
        assert_eq!(result.value, json!(6));
        assert!(
            result.output[0]
                .as_str()
                .unwrap()
                .contains("denied by current plan policy")
        );
    }
    #[tokio::test]
    async fn call_and_output_limits_enforced() {
        let limits = Limits {
            tool_calls: 1,
            ..Limits::default()
        };
        assert!(
            execute(
                "await tools.double({n:1}); return await tools.double({n:2});",
                limits
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("tool call limit")
        );
        let limits = Limits {
            output_bytes: 10,
            ..Limits::default()
        };
        assert!(
            execute("text('12345'); return '12345';", limits)
                .await
                .unwrap_err()
                .to_string()
                .contains("output limit")
        );
    }
    #[tokio::test]
    async fn cpu_loop_and_never_resolving_promise_are_bounded() {
        for source in ["while(true) {}", "await new Promise(() => {});"] {
            let start = Instant::now();
            assert!(
                execute(
                    source,
                    Limits {
                        timeout_ms: 25,
                        ..Limits::default()
                    }
                )
                .await
                .is_err()
            );
            assert!(start.elapsed() < Duration::from_secs(2));
        }
    }
    #[tokio::test]
    async fn cancellation_interrupts_waiting_host() {
        let token = CancellationToken::new();
        let cancel = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            cancel.cancel();
        });
        let result = run(
            "return await tools.wait({});",
            ["wait".to_owned()].into(),
            Arc::new(Host::default()),
            Limits::default(),
            token,
        )
        .await;
        assert!(result.unwrap_err().to_string().contains("cancelled"));
    }
    #[tokio::test]
    async fn heap_exhaustion_is_an_error() {
        assert!(
            execute(
                "return new Array(10000000).fill('x');",
                Limits {
                    memory_bytes: 2 * 1024 * 1024,
                    ..Limits::default()
                }
            )
            .await
            .is_err()
        );
    }
}
