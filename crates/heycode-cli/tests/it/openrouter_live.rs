//! QLIVE01 explicitly gated OpenRouter GLM-5.3-Flash catalog/text/tool lane.
//!
//! A run requires `HEYCODE_E2E=1` and a nonempty process-scoped
//! `OPENROUTER_API_KEY`. The environment provider outranks the home credential file,
//! which keeps a host-global stale/dummy entry from being misread as evidence.
//! Artifacts withhold all provider content and carry only closed metadata.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use heycode_live_artifact::{
    ArtifactRecorder, FailureClass, LiveArtifact, LiveOutcome, RouteId, SkipReason,
};
use heycode_llm::{CapabilitySupport, CatalogFreshness, CatalogRefreshMode, ProviderErrorClass};
use heycode_tools::{Tool, ToolCtx, ToolError};

const MODEL: &str = "z-ai/glm-5.3-flash";
const TEXT_CANARY: &str = "QLIVE01_TEXT_OK";
const TOOL_CANARY: &str = "QLIVE01_TOOL_OK";
const FRESHNESS: Duration = Duration::from_secs(7 * 24 * 60 * 60);

struct CanaryTool(Arc<AtomicUsize>);

#[async_trait]
impl Tool for CanaryTool {
    fn spec(&self) -> heycode_core::ToolSpec {
        heycode_core::ToolSpec {
            name: "qlive01_canary".to_owned(),
            description: "Return the fixed QLIVE01 tool-loop canary.".to_owned(),
            parameters: serde_json::json!({
                "type":"object",
                "additionalProperties":false,
                "required":["value"],
                "properties":{"value":{"type":"string","const":"QLIVE01"}}
            }),
        }
    }

    async fn run(
        &self,
        args: serde_json::Value,
        _context: &ToolCtx,
    ) -> Result<serde_json::Value, ToolError> {
        if args.get("value").and_then(serde_json::Value::as_str) != Some("QLIVE01") {
            return Err(ToolError::new("qlive01 canary argument mismatch"));
        }
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(serde_json::Value::String(TOOL_CANARY.to_owned()))
    }
}

#[derive(Debug)]
enum LaneFailure {
    Provider(FailureClass),
    Assertion,
}

impl LaneFailure {
    const fn class(&self) -> FailureClass {
        match self {
            Self::Provider(class) => *class,
            Self::Assertion => FailureClass::AssertionFailed,
        }
    }
}

#[tokio::test]
async fn live_openrouter_glm_text_tool_catalog_lane_is_explicitly_gated() {
    if std::env::var("HEYCODE_E2E").ok().as_deref() != Some("1") {
        return;
    }
    let recorded_at = unix_time_ms();
    if !std::env::var_os("OPENROUTER_API_KEY").is_some_and(|value| !value.is_empty()) {
        let artifact = artifact(
            LiveOutcome::Skipped {
                reason: SkipReason::NoCredential,
            },
            recorded_at,
            None,
            &[],
        );
        write_artifact(&artifact);
        panic!("QLIVE01 requires a process-scoped OpenRouter credential");
    }

    let started = Instant::now();
    let outcome = match run_lane().await {
        Ok(()) => LiveOutcome::Passed,
        Err(failure) => LiveOutcome::Failed {
            class: failure.class(),
        },
    };
    let notes: &[&str] = if outcome.passed() {
        &["live catalog, reasoning request, text turn and tool loop passed"]
    } else {
        &[]
    };
    let artifact = artifact(outcome, recorded_at, Some(started.elapsed()), notes);
    write_artifact(&artifact);
    let round_trip = LiveArtifact::from_json(&serde_json::to_string(&artifact).unwrap()).unwrap();
    assert!(
        round_trip.is_fresh_at(unix_time_ms(), FRESHNESS),
        "QLIVE01 artifact is outside its freshness window"
    );
    assert!(outcome.passed(), "QLIVE01 live lane failed");
}

async fn run_lane() -> Result<(), LaneFailure> {
    let mut harness = heycode_cli::testing::RealCompositionHarness::new()
        .map_err(|_| LaneFailure::Provider(FailureClass::Unclassified))?;
    harness.config_mut().llm.provider = "openrouter".to_owned();
    harness.config_mut().llm.model = MODEL.to_owned();
    harness.config_mut().llm.api_key_env = Some("OPENROUTER_API_KEY".to_owned());
    harness.config_mut().web.enabled = false;
    harness.config_mut().ui.auto_title = false;
    let world = harness
        .without_fake_provider()
        .compose()
        .map_err(|error| classify_anyhow(&error))?;
    let context = world.context();
    let catalogs = context
        .get::<heycode_llm::CatalogRegistry>(heycode_llm::SERVICE_MODELS)
        .ok_or(LaneFailure::Assertion)?;
    let catalog = catalogs
        .refresh(
            "openrouter",
            CatalogRefreshMode::Force,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .map_err(|_| LaneFailure::Provider(FailureClass::Unclassified))?;
    if catalog.freshness != CatalogFreshness::Live {
        return Err(LaneFailure::Assertion);
    }
    let model = catalog
        .snapshot
        .models
        .iter()
        .find(|model| model.id == MODEL)
        .ok_or(LaneFailure::Assertion)?;
    if model.capabilities.reasoning != CapabilitySupport::Supported
        || model.capabilities.tools != CapabilitySupport::Supported
    {
        return Err(LaneFailure::Assertion);
    }

    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .ok_or(LaneFailure::Assertion)?;
    let text = agent
        .send(&format!(
            "Reply with exactly {TEXT_CANARY}. Do not call a tool."
        ))
        .await
        .map_err(|error| classify_anyhow(&error))?;
    if !text.text.contains(TEXT_CANARY) {
        return Err(LaneFailure::Assertion);
    }

    let calls = Arc::new(AtomicUsize::new(0));
    let tools = context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .ok_or(LaneFailure::Assertion)?;
    tools
        .register_shared(Arc::new(CanaryTool(calls.clone())))
        .map_err(|_| LaneFailure::Assertion)?;
    let tool = agent
        .send(&format!(
            "Call qlive01_canary exactly once with value QLIVE01. After its result, reply with exactly {TOOL_CANARY}. Do not call any other tool."
        ))
        .await
        .map_err(|error| classify_anyhow(&error))?;
    if calls.load(Ordering::SeqCst) != 1 || !tool.text.contains(TOOL_CANARY) {
        return Err(LaneFailure::Assertion);
    }

    let session = agent
        .session()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let headers = session
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            heycode_session::SessionEventKind::RequestHeader { header, .. } => {
                Some(header.as_ref())
            }
            _ => None,
        });
    let mut reasoning = false;
    let mut canary_schema = false;
    for header in headers {
        if header.provider == "openrouter"
            && header.model == MODEL
            && header.options.reasoning_effort.is_some()
        {
            reasoning = true;
        }
        if header
            .tools
            .iter()
            .any(|tool| tool.name == "qlive01_canary")
        {
            canary_schema = true;
        }
    }
    drop(session);
    if !reasoning || !canary_schema {
        return Err(LaneFailure::Assertion);
    }
    world.shutdown();
    Ok(())
}

fn classify_anyhow(error: &anyhow::Error) -> LaneFailure {
    let class = error
        .downcast_ref::<heycode_llm::LlmError>()
        .map(heycode_llm::LlmError::class);
    LaneFailure::Provider(match class {
        Some(ProviderErrorClass::Authentication) => FailureClass::Unauthorized,
        Some(ProviderErrorClass::RateLimited | ProviderErrorClass::Overloaded) => {
            FailureClass::RateLimited
        }
        Some(ProviderErrorClass::Timeout) => FailureClass::Timeout,
        Some(ProviderErrorClass::Network) => FailureClass::HostUnreachable,
        Some(ProviderErrorClass::Protocol) => FailureClass::ProtocolMismatch,
        Some(
            ProviderErrorClass::Server
            | ProviderErrorClass::Overflow
            | ProviderErrorClass::ContextWindowExceeded
            | ProviderErrorClass::Conflict
            | ProviderErrorClass::InvalidRequest
            | ProviderErrorClass::Cancelled,
        )
        | None => FailureClass::Unclassified,
    })
}

fn artifact(
    outcome: LiveOutcome,
    recorded_at: u64,
    latency: Option<Duration>,
    notes: &[&str],
) -> LiveArtifact {
    ArtifactRecorder::withholding()
        .record(
            RouteId::new("openrouter", MODEL).unwrap(),
            outcome,
            recorded_at,
            latency,
            None,
            notes,
        )
        .unwrap()
}

fn write_artifact(artifact: &LiveArtifact) {
    let directory = std::env::var_os("HEYCODE_LIVE_ARTIFACT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("target/live-artifacts"));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("openrouter-glm-5.3-flash.json");
    let bytes = serde_json::to_vec_pretty(artifact).unwrap();
    let mut output = atomic_write_file::AtomicWriteFile::open(&path).unwrap();
    output.write_all(&bytes).unwrap();
    output.commit().unwrap();
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}
