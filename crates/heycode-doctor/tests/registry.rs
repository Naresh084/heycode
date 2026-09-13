//! S11 doctor registry, lifecycle, schema, cancellation and rendering contract.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use heycode_core::{ContributionKind, Plugin, compose};
use heycode_doctor::{
    DoctorCheck, DoctorCheckId, DoctorOutcome, DoctorRegistry, DoctorStatus, SERVICE_DOCTOR,
    doctor_plugin,
};
use tokio_util::sync::CancellationToken;

struct StaticCheck {
    id: DoctorCheckId,
    outcome: DoctorOutcome,
    runs: Arc<AtomicUsize>,
}

#[async_trait]
impl DoctorCheck for StaticCheck {
    fn id(&self) -> &DoctorCheckId {
        &self.id
    }

    async fn run(
        &self,
        _cancellation: CancellationToken,
    ) -> Result<DoctorOutcome, heycode_doctor::DoctorError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(self.outcome.clone())
    }
}

struct ChecksPlugin {
    first_runs: Arc<AtomicUsize>,
    second_runs: Arc<AtomicUsize>,
}

impl Plugin for ChecksPlugin {
    fn name(&self) -> &'static str {
        "test-doctor-checks"
    }

    fn inject(&self) -> &'static [heycode_core::ServiceKey] {
        &[SERVICE_DOCTOR]
    }

    fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
        vec![
            heycode_core::PluginContributionSpec::new(ContributionKind::DoctorCheck, "alpha"),
            heycode_core::PluginContributionSpec::new(ContributionKind::DoctorCheck, "beta"),
        ]
    }

    fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
        let doctor = context
            .get::<DoctorRegistry>(SERVICE_DOCTOR)
            .ok_or_else(|| heycode_core::CoreError::other("doctor missing"))?;
        doctor
            .register(
                context,
                Arc::new(StaticCheck {
                    id: DoctorCheckId::new("alpha").unwrap(),
                    outcome: DoctorOutcome::pass("alpha.ready", "Alpha is ready.").unwrap(),
                    runs: self.first_runs.clone(),
                }),
            )
            .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
        doctor
            .register(
                context,
                Arc::new(StaticCheck {
                    id: DoctorCheckId::new("beta").unwrap(),
                    outcome: DoctorOutcome::warning("beta.degraded", "Beta is degraded.")
                        .unwrap()
                        .with_repair("Reconnect beta and retry.")
                        .unwrap(),
                    runs: self.second_runs.clone(),
                }),
            )
            .map_err(|error| heycode_core::CoreError::other(error.to_string()))
    }
}

#[tokio::test]
async fn plugins_contribute_ordered_checks_and_reports_are_stable_and_redacted() {
    let first_runs = Arc::new(AtomicUsize::new(0));
    let second_runs = Arc::new(AtomicUsize::new(0));
    let plugins: Vec<Box<dyn Plugin>> = vec![
        doctor_plugin(),
        Box::new(ChecksPlugin {
            first_runs: first_runs.clone(),
            second_runs: second_runs.clone(),
        }),
    ];
    let mut context = compose(&plugins).unwrap();
    let doctor = context.get::<DoctorRegistry>(SERVICE_DOCTOR).unwrap();

    let report = doctor.run(CancellationToken::new()).await.unwrap();
    assert!(report.healthy);
    assert_eq!(report.schema_version, 1);
    assert_eq!(report.summary.passed, 1);
    assert_eq!(report.summary.warnings, 1);
    assert_eq!(report.summary.failed, 0);
    assert_eq!(
        report
            .checks
            .iter()
            .map(|check| (check.id.as_str(), check.status))
            .collect::<Vec<_>>(),
        [
            ("alpha", DoctorStatus::Pass),
            ("beta", DoctorStatus::Warning)
        ]
    );
    let json = serde_json::to_string_pretty(&report).unwrap();
    assert!(json.contains("\"schema_version\": 1"));
    assert!(json.contains("Reconnect beta and retry."));
    assert!(!json.contains("sk-live-canary-never-emit"));
    let human = report.render_human();
    assert!(human.starts_with("doctor: healthy"), "{human}");
    assert!(human.contains("[warning] beta (beta.degraded)"), "{human}");
    assert_eq!(first_runs.load(Ordering::SeqCst), 1);
    assert_eq!(second_runs.load(Ordering::SeqCst), 1);

    let inventory = context.plugin_inventory().snapshot().unwrap();
    assert!(inventory.contributions.iter().any(|row| {
        row.plugin == "test-doctor-checks"
            && row.kind == ContributionKind::DoctorCheck
            && row.name == "alpha"
    }));
    context.shutdown();
    assert_eq!(doctor.check_ids().unwrap(), Vec::<DoctorCheckId>::new());
}

#[tokio::test]
async fn duplicate_ids_fail_and_precancelled_runs_skip_without_invocation() {
    let plugins = vec![doctor_plugin()];
    let mut context = compose(&plugins).unwrap();
    let doctor = context.get::<DoctorRegistry>(SERVICE_DOCTOR).unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let make = || {
        Arc::new(StaticCheck {
            id: DoctorCheckId::new("cancelled").unwrap(),
            outcome: DoctorOutcome::failure("should.not-run", "This must not run.").unwrap(),
            runs: runs.clone(),
        }) as Arc<dyn DoctorCheck>
    };
    doctor.register(&context, make()).unwrap();
    let error = doctor.register(&context, make()).unwrap_err().to_string();
    assert!(error.contains("cancelled"), "{error}");

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let report = doctor.run(cancellation).await.unwrap();
    assert_eq!(report.checks[0].status, DoctorStatus::Skipped);
    assert_eq!(report.checks[0].code.as_str(), "doctor.cancelled");
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    context.shutdown();
}

#[test]
fn ids_codes_and_runtime_text_boundaries_fail_loud() {
    for invalid in ["", "UPPER", ".start", "two words", "slash/value"] {
        assert!(DoctorCheckId::new(invalid).is_err(), "accepted {invalid:?}");
    }
    assert!(DoctorOutcome::pass("Bad Code", "static").is_err());
}
