use std::ffi::OsString;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use heycode_core::{Context, CoreError, Plugin, compose};
use heycode_exec::{ProcessSpec, SubprocessService};
use heycode_http::{HttpErrorMetadata, SseDecoder, SseEvent, TransportError};
use heycode_llm::{
    FinishReason, ProviderErrorClass, SseParser, StreamChunk, classify_transport_error,
};
use heycode_session::{
    OpenError, OpenOutcome, Session, SessionEventKind, TurnEndReason, project_repair,
};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

const DEFAULT_SEED: u64 = 12_648_430;
const RUNNER_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Serialize)]
struct RunReport {
    schema_version: u8,
    seed: u64,
    scenarios: Vec<ScenarioReport>,
}

#[derive(Serialize)]
struct ScenarioReport {
    name: &'static str,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'static str>,
}

impl ScenarioReport {
    const fn passed(name: &'static str) -> Self {
        Self {
            name,
            status: "passed",
            code: None,
        }
    }

    #[cfg(not(unix))]
    const fn skipped(name: &'static str, code: &'static str) -> Self {
        Self {
            name,
            status: "skipped",
            code: Some(code),
        }
    }

    const fn failed(name: &'static str, code: &'static str) -> Self {
        Self {
            name,
            status: "failed",
            code: Some(code),
        }
    }
}

struct ChaosFailure {
    scenario: &'static str,
    code: &'static str,
}

type ChaosResult<T = ()> = Result<T, ChaosFailure>;

const fn failure(scenario: &'static str, code: &'static str) -> ChaosFailure {
    ChaosFailure { scenario, code }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> ExitCode {
    let Some(seed) = parse_seed() else {
        let report = RunReport {
            schema_version: 1,
            seed: 0,
            scenarios: vec![ScenarioReport::failed("runner", "invalid_arguments")],
        };
        print_report(&report);
        return ExitCode::from(2);
    };

    match tokio::time::timeout(RUNNER_DEADLINE, run_all(seed)).await {
        Ok(Ok(scenarios)) => {
            print_report(&RunReport {
                schema_version: 1,
                seed,
                scenarios,
            });
            ExitCode::SUCCESS
        }
        Ok(Err((scenarios, error))) => {
            let mut scenarios = scenarios;
            scenarios.push(ScenarioReport::failed(error.scenario, error.code));
            print_report(&RunReport {
                schema_version: 1,
                seed,
                scenarios,
            });
            ExitCode::FAILURE
        }
        Err(_) => {
            print_report(&RunReport {
                schema_version: 1,
                seed,
                scenarios: vec![ScenarioReport::failed("runner", "deadline_exceeded")],
            });
            ExitCode::FAILURE
        }
    }
}

fn parse_seed() -> Option<u64> {
    let mut arguments = std::env::args().skip(1);
    let mut seed = DEFAULT_SEED;
    while let Some(argument) = arguments.next() {
        if argument != "--seed" {
            return None;
        }
        let value = arguments.next()?;
        seed = value.parse().ok()?;
    }
    Some(seed)
}

fn print_report(report: &RunReport) {
    match serde_json::to_string(report) {
        Ok(encoded) => println!("{encoded}"),
        Err(_) => {
            let fallback = r#"{"schema_version":1,"seed":0,"scenarios":[{"name":"runner","status":"failed","code":"report_encoding"}]}"#;
            println!("{fallback}");
        }
    }
}

async fn run_all(seed: u64) -> Result<Vec<ScenarioReport>, (Vec<ScenarioReport>, ChaosFailure)> {
    let mut reports = Vec::new();
    run_sync(&mut reports, "composition_rollback", composition_rollback)?;
    run_sync(
        &mut reports,
        "durable_session_settlement",
        durable_session_settlement,
    )?;
    match subprocess_cancellation().await {
        Ok(report) => reports.push(report),
        Err(error) => return Err((reports, error)),
    }
    run_sync(&mut reports, "http_sse_fragmentation", || {
        http_sse_fragmentation(seed)
    })?;
    run_sync(
        &mut reports,
        "provider_failure_boundary",
        provider_failure_boundary,
    )?;
    Ok(reports)
}

fn run_sync(
    reports: &mut Vec<ScenarioReport>,
    name: &'static str,
    scenario: impl FnOnce() -> ChaosResult,
) -> Result<(), (Vec<ScenarioReport>, ChaosFailure)> {
    match scenario() {
        Ok(()) => {
            reports.push(ScenarioReport::passed(name));
            Ok(())
        }
        Err(error) => Err((std::mem::take(reports), error)),
    }
}

struct EffectPlugin {
    name: &'static str,
    effects: &'static [&'static str],
    fail: bool,
    log: Arc<Mutex<Vec<&'static str>>>,
}

impl Plugin for EffectPlugin {
    fn name(&self) -> &'static str {
        self.name
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        for label in self.effects {
            let label = *label;
            let log = self.log.clone();
            context.effect(move || {
                if let Ok(mut entries) = log.lock() {
                    entries.push(label);
                }
            });
        }
        if self.fail {
            Err(CoreError::other("injected apply failure"))
        } else {
            Ok(())
        }
    }
}

fn log_snapshot(
    scenario: &'static str,
    log: &Arc<Mutex<Vec<&'static str>>>,
) -> ChaosResult<Vec<&'static str>> {
    log.lock()
        .map(|entries| entries.clone())
        .map_err(|_| failure(scenario, "effect_log_unavailable"))
}

fn composition_rollback() -> ChaosResult {
    const SCENARIO: &str = "composition_rollback";
    let success_log = Arc::new(Mutex::new(Vec::new()));
    let success_plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(EffectPlugin {
            name: "chaos-success-first",
            effects: &["success-first"],
            fail: false,
            log: success_log.clone(),
        }),
        Box::new(EffectPlugin {
            name: "chaos-success-second",
            effects: &["success-second-a", "success-second-b"],
            fail: false,
            log: success_log.clone(),
        }),
    ];
    let mut context =
        compose(&success_plugins).map_err(|_| failure(SCENARIO, "compose_success"))?;
    context.shutdown();
    context.shutdown();
    if !context.is_closed() {
        return Err(failure(SCENARIO, "context_not_closed"));
    }
    if log_snapshot(SCENARIO, &success_log)?
        != ["success-second-b", "success-second-a", "success-first"]
    {
        return Err(failure(SCENARIO, "shutdown_not_lifo_exactly_once"));
    }

    let failure_log = Arc::new(Mutex::new(Vec::new()));
    let failure_plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(EffectPlugin {
            name: "chaos-prior",
            effects: &["prior"],
            fail: false,
            log: failure_log.clone(),
        }),
        Box::new(EffectPlugin {
            name: "chaos-failing",
            effects: &["failing-a", "failing-b"],
            fail: true,
            log: failure_log.clone(),
        }),
        Box::new(EffectPlugin {
            name: "chaos-never-runs",
            effects: &["never"],
            fail: false,
            log: failure_log.clone(),
        }),
    ];
    if compose(&failure_plugins).is_ok() {
        return Err(failure(SCENARIO, "injected_failure_accepted"));
    }
    if log_snapshot(SCENARIO, &failure_log)? != ["failing-b", "failing-a", "prior"] {
        return Err(failure(SCENARIO, "rollback_not_lifo_or_later_plugin_ran"));
    }
    Ok(())
}

fn append(session: &mut Session, kind: SessionEventKind, code: &'static str) -> ChaosResult {
    session
        .append(kind)
        .map(|_| ())
        .map_err(|_| failure("durable_session_settlement", code))
}

fn write_session_file(directory: &std::path::Path, bytes: &[u8]) -> ChaosResult {
    std::fs::create_dir(directory)
        .map_err(|_| failure("durable_session_settlement", "create_injected_stream"))?;
    std::fs::write(directory.join("session.jsonl"), bytes)
        .map_err(|_| failure("durable_session_settlement", "write_injected_stream"))
}

fn durable_session_settlement() -> ChaosResult {
    const SCENARIO: &str = "durable_session_settlement";
    let root = tempfile::tempdir().map_err(|_| failure(SCENARIO, "temporary_root"))?;
    let mut session = Session::create(root.path()).map_err(|_| failure(SCENARIO, "create"))?;
    let call_id = heycode_core::CallId::from_raw("chaos-call");
    append(
        &mut session,
        SessionEventKind::TurnStart { turn: 0 },
        "turn_start",
    )?;
    append(
        &mut session,
        SessionEventKind::StepStart { turn: 0, step: 0 },
        "step_start",
    )?;
    append(
        &mut session,
        SessionEventKind::ToolCall {
            turn: 0,
            call_id: call_id.clone(),
            name: "chaos_tool".to_owned(),
            args: serde_json::json!({"case":"synthetic"}),
        },
        "tool_call",
    )?;
    append(
        &mut session,
        SessionEventKind::ToolResult {
            call_id,
            content: "synthetic outcome".to_owned(),
            is_error: true,
            untrusted_content: None,
        },
        "tool_result",
    )?;
    append(
        &mut session,
        SessionEventKind::StepEnd { turn: 0, step: 0 },
        "step_end",
    )?;
    append(
        &mut session,
        SessionEventKind::TurnEnd {
            turn: 0,
            reason: TurnEndReason::Error,
        },
        "turn_end",
    )?;
    session.flush().map_err(|_| failure(SCENARIO, "flush"))?;
    if !project_repair(session.events()).is_clean() {
        return Err(failure(SCENARIO, "handled_failure_left_open_work"));
    }

    let raw = std::fs::read_to_string(session.path())
        .map_err(|_| failure(SCENARIO, "read_committed_stream"))?;
    let lines = raw.lines().collect::<Vec<_>>();
    if lines.len() != 6 {
        return Err(failure(SCENARIO, "unexpected_committed_event_count"));
    }
    let expected_open = [0_usize, 1, 2, 3, 2, 1, 0];
    for (cut, expected) in expected_open.into_iter().enumerate() {
        let mut prefix = lines[..cut].join("\n").into_bytes();
        if !prefix.is_empty() {
            prefix.push(b'\n');
        }
        let directory = root.path().join(format!("prefix-{cut}"));
        write_session_file(&directory, &prefix)?;
        let opened =
            Session::open(&directory).map_err(|_| failure(SCENARIO, "open_whole_event_prefix"))?;
        let before = std::fs::read(opened.path())
            .map_err(|_| failure(SCENARIO, "read_prefix_before_repair"))?;
        let repair = project_repair(opened.events());
        if repair.open().len() != expected {
            return Err(failure(SCENARIO, "prefix_open_record_count"));
        }
        if !repair
            .open()
            .iter()
            .all(|record| record.outcome() == OpenOutcome::Unknown)
        {
            return Err(failure(SCENARIO, "prefix_invented_terminal_outcome"));
        }
        if repair != project_repair(opened.events()) {
            return Err(failure(SCENARIO, "repair_not_deterministic"));
        }
        let after = std::fs::read(opened.path())
            .map_err(|_| failure(SCENARIO, "read_prefix_after_repair"))?;
        if before != after {
            return Err(failure(SCENARIO, "repair_mutated_log"));
        }
    }

    let interrupted_root = root.path().join("interrupted-root");
    std::fs::create_dir(&interrupted_root)
        .map_err(|_| failure(SCENARIO, "create_interrupted_root"))?;
    let mut interrupted =
        Session::create(&interrupted_root).map_err(|_| failure(SCENARIO, "create_interrupted"))?;
    append(
        &mut interrupted,
        SessionEventKind::TurnStart { turn: 0 },
        "interrupted_turn_start",
    )?;
    append(
        &mut interrupted,
        SessionEventKind::StepStart { turn: 0, step: 0 },
        "interrupted_step_start",
    )?;
    append(
        &mut interrupted,
        SessionEventKind::ToolCall {
            turn: 0,
            call_id: heycode_core::CallId::from_raw("interrupted-call"),
            name: "chaos_tool".to_owned(),
            args: serde_json::json!({}),
        },
        "interrupted_tool_call",
    )?;
    append(
        &mut interrupted,
        SessionEventKind::TurnEnd {
            turn: 0,
            reason: TurnEndReason::Error,
        },
        "interrupted_turn_end",
    )?;
    let interrupted_repair = project_repair(interrupted.events());
    if interrupted_repair.open().len() != 2
        || !interrupted_repair
            .open()
            .iter()
            .all(|record| record.outcome() == OpenOutcome::Interrupted)
    {
        return Err(failure(SCENARIO, "outer_settlement_closed_nested_work"));
    }

    let torn = root.path().join("torn");
    write_session_file(&torn, b"{\"v\":2")?;
    if !matches!(Session::open(&torn), Err(OpenError::UnterminatedTail)) {
        return Err(failure(SCENARIO, "torn_tail_not_refused"));
    }

    let mut gap = lines[0].as_bytes().to_vec();
    gap.push(b'\n');
    gap.extend_from_slice(lines[2].as_bytes());
    gap.push(b'\n');
    let gap_dir = root.path().join("gap");
    write_session_file(&gap_dir, &gap)?;
    if !matches!(
        Session::open(&gap_dir),
        Err(OpenError::SeqGap {
            expected: 1,
            found: 2
        })
    ) {
        return Err(failure(SCENARIO, "sequence_gap_not_refused"));
    }
    Ok(())
}

#[cfg(unix)]
async fn subprocess_cancellation() -> ChaosResult<ScenarioReport> {
    const SCENARIO: &str = "subprocess_cancellation";
    let root = tempfile::tempdir().map_err(|_| failure(SCENARIO, "temporary_root"))?;
    let ready = root.path().join("ready");
    let release = root.path().join("release");
    let marker = root.path().join("survived");
    let script = r#"( : > "$1"; while [ ! -f "$2" ]; do sleep 0.05; done; : > "$3" ) & wait"#;
    let arguments = vec![
        OsString::from("-c"),
        OsString::from(script),
        OsString::from("chaos-child"),
        ready.as_os_str().to_os_string(),
        release.as_os_str().to_os_string(),
        marker.as_os_str().to_os_string(),
    ];
    let spec = ProcessSpec::new("/bin/sh", root.path())
        .and_then(|spec| spec.with_args(arguments))
        .and_then(|spec| spec.with_environment(Vec::<(OsString, OsString)>::new()))
        .and_then(|spec| spec.with_timeout(Some(Duration::from_secs(5))))
        .and_then(|spec| spec.with_output_limit_bytes(4096))
        .map_err(|_| failure(SCENARIO, "process_spec"))?;
    let service = SubprocessService::local();
    let process = service
        .spawn(spec, CancellationToken::new())
        .await
        .map_err(|_| failure(SCENARIO, "spawn"))?;
    if !process.containment().tree_kill_on_drop() {
        return Err(failure(SCENARIO, "containment_not_armed"));
    }
    let ready_observed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if ready.is_file() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok();
    if !ready_observed {
        return Err(failure(SCENARIO, "child_not_ready"));
    }
    tokio::time::timeout(Duration::from_secs(5), process.cancel())
        .await
        .map_err(|_| failure(SCENARIO, "cancel_deadline"))?
        .map_err(|_| failure(SCENARIO, "cancel_failed"))?;
    std::fs::write(&release, b"release").map_err(|_| failure(SCENARIO, "release_barrier"))?;
    tokio::time::sleep(Duration::from_millis(150)).await;
    if marker.exists() {
        return Err(failure(SCENARIO, "descendant_survived_settlement"));
    }
    Ok(ScenarioReport::passed(SCENARIO))
}

#[cfg(not(unix))]
async fn subprocess_cancellation() -> ChaosResult<ScenarioReport> {
    Ok(ScenarioReport::skipped(
        "subprocess_cancellation",
        "requires_posix_shell",
    ))
}

struct DeterministicRng(u64);

impl DeterministicRng {
    const fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9e37_79b9_7f4a_7c15
        } else {
            seed
        })
    }

    fn next(&mut self) -> u64 {
        let mut value = self.0;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        value
    }

    fn chunk(&mut self, remaining: usize) -> usize {
        let maximum = remaining.clamp(1, 31);
        let maximum_u64 = u64::try_from(maximum).unwrap_or(1);
        usize::try_from(self.next() % maximum_u64)
            .unwrap_or(0)
            .saturating_add(1)
    }
}

fn decode_sse(raw: &[u8], mut rng: Option<&mut DeterministicRng>) -> ChaosResult<Vec<SseEvent>> {
    const SCENARIO: &str = "http_sse_fragmentation";
    let mut decoder = SseDecoder::with_max_event_bytes(64 * 1024);
    let mut events = Vec::new();
    let mut offset = 0;
    while offset < raw.len() {
        let size = rng
            .as_deref_mut()
            .map_or(raw.len().saturating_sub(offset), |rng| {
                rng.chunk(raw.len().saturating_sub(offset))
            });
        let end = offset.saturating_add(size).min(raw.len());
        events.extend(
            decoder
                .feed(&raw[offset..end])
                .map_err(|_| failure(SCENARIO, "decoder_feed"))?,
        );
        offset = end;
    }
    events.extend(
        decoder
            .finish()
            .map_err(|_| failure(SCENARIO, "decoder_finish"))?,
    );
    Ok(events)
}

fn parse_provider(
    raw: &[u8],
    mut rng: Option<&mut DeterministicRng>,
) -> ChaosResult<Vec<StreamChunk>> {
    const SCENARIO: &str = "http_sse_fragmentation";
    let mut parser = SseParser::new();
    let mut output = Vec::new();
    let mut offset = 0;
    while offset < raw.len() {
        let size = rng
            .as_deref_mut()
            .map_or(raw.len().saturating_sub(offset), |rng| {
                rng.chunk(raw.len().saturating_sub(offset))
            });
        let end = offset.saturating_add(size).min(raw.len());
        for item in parser.feed(&raw[offset..end]) {
            output.push(item.map_err(|_| failure(SCENARIO, "provider_feed"))?);
        }
        offset = end;
    }
    for item in parser.finish() {
        output.push(item.map_err(|_| failure(SCENARIO, "provider_finish"))?);
    }
    Ok(output)
}

fn http_sse_fragmentation(seed: u64) -> ChaosResult {
    const SCENARIO: &str = "http_sse_fragmentation";
    let raw = concat!(
        "\u{feff}: synthetic keepalive\r\n",
        "id: 7\r\n",
        "retry: 250\r\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"he\"}}]}\r\n\r\n",
        "data: {\"choices\":[{\"delta\":\r\n",
        "data: {\"content\":\"llo\"}}]}\r\n\r\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],",
        "\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\r\n\r\n",
        "data: [DONE]\r\n\r\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"late\"}}]}\r\n\r\n"
    )
    .as_bytes();

    let whole_events = decode_sse(raw, None)?;
    let mut decoder_rng = DeterministicRng::new(seed ^ 0xa5a5_a5a5_a5a5_a5a5);
    let fragmented_events = decode_sse(raw, Some(&mut decoder_rng))?;
    if whole_events != fragmented_events {
        return Err(failure(SCENARIO, "sse_events_changed_by_fragmentation"));
    }

    let whole_provider = parse_provider(raw, None)?;
    let mut provider_rng = DeterministicRng::new(seed ^ 0x5a5a_5a5a_5a5a_5a5a);
    let fragmented_provider = parse_provider(raw, Some(&mut provider_rng))?;
    if whole_provider != fragmented_provider {
        return Err(failure(
            SCENARIO,
            "provider_output_changed_by_fragmentation",
        ));
    }
    let usage = fragmented_provider
        .iter()
        .position(|item| matches!(item, StreamChunk::Usage(_)))
        .ok_or_else(|| failure(SCENARIO, "usage_missing"))?;
    let finish = fragmented_provider
        .iter()
        .position(|item| matches!(item, StreamChunk::Finish(FinishReason::Stop)))
        .ok_or_else(|| failure(SCENARIO, "finish_missing"))?;
    if usage + 1 != finish || finish + 1 != fragmented_provider.len() {
        return Err(failure(SCENARIO, "terminal_order"));
    }
    if fragmented_provider
        .iter()
        .any(|item| matches!(item, StreamChunk::TextDelta(text) if text == "late"))
    {
        return Err(failure(SCENARIO, "output_after_terminal"));
    }
    Ok(())
}

fn provider_failure_boundary() -> ChaosResult {
    const SCENARIO: &str = "provider_failure_boundary";
    const CANARY: &str = "CHAOS_PROVIDER_BODY_CANARY_7f3b";
    let cases = [
        (401_u16, "server_error", ProviderErrorClass::Authentication),
        (
            429_u16,
            "rate_limit_exceeded",
            ProviderErrorClass::RateLimited,
        ),
        (
            503_u16,
            "invalid_request_error",
            ProviderErrorClass::Overloaded,
        ),
        (
            400_u16,
            "context_length_exceeded",
            ProviderErrorClass::ContextWindowExceeded,
        ),
    ];
    for (status, code, expected) in cases {
        let body = serde_json::json!({
            "error": {"code": code, "message": CANARY}
        })
        .to_string();
        let raw = TransportError::http(status, body, HttpErrorMetadata::default());
        let TransportError::Http { body, .. } = &raw else {
            return Err(failure(SCENARIO, "http_failure_premise"));
        };
        if !body.as_str().contains(CANARY) {
            return Err(failure(SCENARIO, "body_canary_premise"));
        }
        if format!("{raw:?}").contains(CANARY) || raw.to_string().contains(CANARY) {
            return Err(failure(SCENARIO, "transport_diagnostic_leak"));
        }
        let normalized = classify_transport_error(raw);
        if normalized.class() != expected {
            return Err(failure(SCENARIO, "failure_class"));
        }
        if normalized
            .provider_failure()
            .and_then(|failure| failure.status())
            != Some(status)
        {
            return Err(failure(SCENARIO, "status_not_retained"));
        }
        if format!("{normalized:?}").contains(CANARY) || normalized.to_string().contains(CANARY) {
            return Err(failure(SCENARIO, "normalized_diagnostic_leak"));
        }
    }
    Ok(())
}
