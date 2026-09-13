//! TEL04 metrics derived only from committed session events.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use heycode_core::{Context, CoreError, Plugin};
use heycode_session::{SessionEvent, SessionEventKind, SessionSource};
use heycode_telemetry::{Dimension, Label, TelemetryEvent, TelemetryEventName, TelemetryService};

const MAX_REQUEST_ROUTES: usize = 1_024;

#[derive(Clone)]
struct RequestRoute {
    provider: Label,
    model: Option<Label>,
}

#[derive(Default)]
struct MetricContext {
    lineage: Option<Label>,
    runtime: Option<Label>,
    requests: HashMap<heycode_core::RequestId, RequestRoute>,
    request_order: VecDeque<heycode_core::RequestId>,
}

impl MetricContext {
    fn observe_without_emitting(&mut self, event: &SessionEvent) {
        match &event.kind {
            SessionEventKind::SessionCreated { creation } => {
                self.lineage = Label::new(source_name(creation.metadata().source())).ok();
                self.runtime = creation
                    .metadata()
                    .runtime()
                    .and_then(|value| Label::new(value).ok());
            }
            SessionEventKind::RuntimeLinked { runtime, .. } => {
                self.runtime = Label::new(runtime).ok();
            }
            SessionEventKind::RequestHeader {
                request_id, header, ..
            } => {
                let Ok(provider) = Label::new(&header.provider) else {
                    return;
                };
                self.remember_request(
                    request_id.clone(),
                    RequestRoute {
                        provider,
                        model: Label::new(&header.model).ok(),
                    },
                );
            }
            _ => {}
        }
    }

    fn remember_request(&mut self, id: heycode_core::RequestId, route: RequestRoute) {
        if !self.requests.contains_key(&id) {
            self.request_order.push_back(id.clone());
        }
        self.requests.insert(id, route);
        while self.request_order.len() > MAX_REQUEST_ROUTES {
            let Some(oldest) = self.request_order.pop_front() else {
                break;
            };
            self.requests.remove(&oldest);
        }
    }
}

fn emit_committed(event: &SessionEvent, context: &mut MetricContext, telemetry: &TelemetryService) {
    match &event.kind {
        SessionEventKind::SessionCreated { .. } | SessionEventKind::RuntimeLinked { .. } => {
            context.observe_without_emitting(event);
        }
        SessionEventKind::RequestHeader {
            request_id, header, ..
        } => {
            let Ok(provider) = Label::new(&header.provider) else {
                return;
            };
            let route = RequestRoute {
                provider: provider.clone(),
                model: Label::new(&header.model).ok(),
            };
            context.remember_request(request_id.clone(), route.clone());
            let mut metric = TelemetryEvent::new(TelemetryEventName::ProviderRequest, at(event));
            metric = match metric.with_dimension(Dimension::Provider, provider) {
                Ok(metric) => metric,
                Err(_) => return,
            };
            if let Some(model) = route.model {
                metric = match metric.with_dimension(Dimension::Model, model) {
                    Ok(metric) => metric,
                    Err(_) => return,
                };
            }
            if let Ok(purpose) = Label::new(&header.options.purpose) {
                metric = match metric.with_dimension(Dimension::Purpose, purpose) {
                    Ok(metric) => metric,
                    Err(_) => return,
                };
            }
            record_with_context(metric, context, telemetry);
        }
        SessionEventKind::ToolCall { name, .. } => {
            emit_tool(event, name, "local", 1, None, context, telemetry);
        }
        SessionEventKind::ServerToolCall {
            request_id, call, ..
        } => {
            emit_tool(
                event,
                call.logical(),
                "provider_exact",
                1,
                context.requests.get(request_id),
                context,
                telemetry,
            );
        }
        SessionEventKind::ServerToolUsage {
            request_id, usage, ..
        } => {
            emit_tool(
                event,
                usage.logical(),
                "provider_aggregate",
                u64::from(usage.requests()),
                context.requests.get(request_id),
                context,
                telemetry,
            );
        }
        SessionEventKind::CompactionApplied { .. } => {
            emit_compaction(event, "portable", None, context, telemetry);
        }
        SessionEventKind::NativeCompactionApplied {
            strategy, items, ..
        } => {
            let provider = items
                .first()
                .and_then(|item| Label::new(item.provider()).ok());
            emit_compaction(event, strategy, provider, context, telemetry);
        }
        SessionEventKind::AssistantResponseMetadata {
            request_id,
            metadata,
            ..
        } => {
            let Some(cache) = metadata.cache_usage() else {
                return;
            };
            let activity = match cache.activity() {
                heycode_core::ProviderCacheActivity::None => "none",
                heycode_core::ProviderCacheActivity::Read => "read",
                heycode_core::ProviderCacheActivity::Write => "write",
                heycode_core::ProviderCacheActivity::ReadAndWrite => "read_write",
            };
            let Ok(activity) = Label::new(activity) else {
                return;
            };
            let mut metric = TelemetryEvent::new(TelemetryEventName::CacheObserved, at(event));
            metric = match metric.with_dimension(Dimension::Cache, activity) {
                Ok(metric) => metric,
                Err(_) => return,
            };
            if let Some(route) = context.requests.get(request_id) {
                metric = match metric.with_dimension(Dimension::Provider, route.provider.clone()) {
                    Ok(metric) => metric,
                    Err(_) => return,
                };
                if let Some(model) = route.model.clone() {
                    metric = match metric.with_dimension(Dimension::Model, model) {
                        Ok(metric) => metric,
                        Err(_) => return,
                    };
                }
            }
            record_with_context(metric, context, telemetry);
        }
        _ => {}
    }
}

fn emit_tool(
    event: &SessionEvent,
    logical: &str,
    execution: &str,
    count: u64,
    route: Option<&RequestRoute>,
    context: &MetricContext,
    telemetry: &TelemetryService,
) {
    let (Ok(tool), Ok(execution)) = (Label::new(logical), Label::new(execution)) else {
        return;
    };
    let mut metric = TelemetryEvent::new(TelemetryEventName::ToolInvoked, at(event));
    metric = match metric.with_count(count) {
        Ok(metric) => metric,
        Err(_) => return,
    };
    metric = match metric.with_dimension(Dimension::Tool, tool) {
        Ok(metric) => metric,
        Err(_) => return,
    };
    metric = match metric.with_dimension(Dimension::Execution, execution) {
        Ok(metric) => metric,
        Err(_) => return,
    };
    if let Some(route) = route {
        metric = match metric.with_dimension(Dimension::Provider, route.provider.clone()) {
            Ok(metric) => metric,
            Err(_) => return,
        };
    }
    record_with_context(metric, context, telemetry);
}

fn emit_compaction(
    event: &SessionEvent,
    kind: &str,
    provider: Option<Label>,
    context: &MetricContext,
    telemetry: &TelemetryService,
) {
    let Ok(kind) = Label::new(kind) else {
        return;
    };
    let mut metric = TelemetryEvent::new(TelemetryEventName::CompactionCompleted, at(event));
    metric = match metric.with_dimension(Dimension::Compaction, kind) {
        Ok(metric) => metric,
        Err(_) => return,
    };
    if let Some(provider) = provider {
        metric = match metric.with_dimension(Dimension::Provider, provider) {
            Ok(metric) => metric,
            Err(_) => return,
        };
    }
    record_with_context(metric, context, telemetry);
}

fn record_with_context(
    mut event: TelemetryEvent,
    context: &MetricContext,
    telemetry: &TelemetryService,
) {
    if let Some(lineage) = context.lineage.clone() {
        event = match event.with_dimension(Dimension::Lineage, lineage) {
            Ok(event) => event,
            Err(_) => return,
        };
    }
    if let Some(runtime) = context.runtime.clone() {
        event = match event.with_dimension(Dimension::Runtime, runtime) {
            Ok(event) => event,
            Err(_) => return,
        };
    }
    telemetry.record(&event);
}

const fn source_name(source: SessionSource) -> &'static str {
    match source {
        SessionSource::Interactive => "interactive",
        SessionSource::Headless => "headless",
        SessionSource::Acp => "acp",
        SessionSource::Subagent => "subagent",
        SessionSource::Scheduled => "scheduled",
        SessionSource::Delegated => "delegated",
        SessionSource::Fork => "fork",
    }
}

const fn at(event: &SessionEvent) -> u64 {
    if event.time_ms < 0 {
        0
    } else {
        event.time_ms as u64
    }
}

/// Register committed provider/tool/compaction/cache telemetry metrics.
#[must_use]
pub fn telemetry_metrics_plugin() -> Box<dyn Plugin> {
    struct TelemetryMetricsPlugin;

    impl Plugin for TelemetryMetricsPlugin {
        fn name(&self) -> &'static str {
            "telemetry-metrics"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Diagnostic],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            ["provider_request", "tool", "compaction", "cache"]
                .into_iter()
                .map(|name| {
                    heycode_core::PluginContributionSpec::new(
                        heycode_core::ContributionKind::TelemetryMetric,
                        name,
                    )
                })
                .collect()
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_session::SERVICE_SESSION,
                heycode_telemetry::SERVICE_TELEMETRY,
            ]
        }

        fn apply(&self, context: &mut Context) -> heycode_core::CoreResult<()> {
            let session = context
                .get::<std::sync::Mutex<heycode_session::Session>>(heycode_session::SERVICE_SESSION)
                .ok_or_else(|| CoreError::other("session missing"))?;
            let telemetry = context
                .get::<TelemetryService>(heycode_telemetry::SERVICE_TELEMETRY)
                .ok_or_else(|| CoreError::other("telemetry missing"))?;
            let state = Arc::new(Mutex::new(MetricContext::default()));
            let bus;
            {
                let events = session
                    .lock()
                    .map_err(|_| CoreError::other("session unavailable"))?;
                bus = events.bus();
                let mut state = state
                    .lock()
                    .map_err(|_| CoreError::other("telemetry metrics unavailable"))?;
                for event in events.events() {
                    state.observe_without_emitting(event);
                }
            }
            bus.on_effect::<SessionEvent>(context, move |event| {
                let Ok(mut state) = state.lock() else {
                    return;
                };
                emit_committed(event, &mut state, &telemetry);
            });
            Ok(())
        }
    }

    Box::new(TelemetryMetricsPlugin)
}
