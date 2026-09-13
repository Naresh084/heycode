//! PMM04: Token Plan MCP installation facts and fail-closed tool policy.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use heycode_core::UntrustedContentBoundary;
use heycode_provider_minimax::{
    MiniMaxImageUnderstandingPolicy, MiniMaxMcpApproval, MiniMaxMcpBundleError,
    MiniMaxMcpEnvironmentValue, MiniMaxMcpExposurePolicy, MiniMaxMcpFeatureEvidence,
    MiniMaxMcpHostFailure, MiniMaxMcpInstallScope, MiniMaxMcpResourceMode, MiniMaxProfile,
    MiniMaxRegion, MiniMaxTokenPlanMcpBundle, MiniMaxTokenPlanMcpHost, TokenPlan,
    minimax_token_plan_mcp_bundle_plugin,
};

#[test]
fn token_plan_bundle_is_a_secret_free_exact_user_install_with_all_current_tools() {
    let bundle = MiniMaxTokenPlanMcpBundle::new(
        MiniMaxProfile::<TokenPlan>::international(),
        MiniMaxImageUnderstandingPolicy::AllDocumented,
    )
    .unwrap();

    assert_eq!(bundle.server_id(), "minimax-token-plan");
    assert_eq!(bundle.display_name(), "MiniMax Token Plan MCP");
    assert_eq!(bundle.scope(), MiniMaxMcpInstallScope::User);
    assert_eq!(bundle.command(), "uvx");
    assert_eq!(bundle.arguments(), ["minimax-coding-plan-mcp", "-y"]);
    assert!(!bundle.required());
    assert_eq!(bundle.resource_mode(), &MiniMaxMcpResourceMode::Url);
    assert_eq!(bundle.exposure(), MiniMaxMcpExposurePolicy::none());

    let environment = bundle.environment();
    assert_eq!(environment.len(), 3);
    assert_eq!(
        environment.get("MINIMAX_API_HOST"),
        Some(&MiniMaxMcpEnvironmentValue::Literal(
            "https://api.minimax.io".to_owned()
        ))
    );
    assert_eq!(
        environment.get("MINIMAX_API_RESOURCE_MODE"),
        Some(&MiniMaxMcpEnvironmentValue::Literal("url".to_owned()))
    );
    let MiniMaxMcpEnvironmentValue::Credential(query) = environment.get("MINIMAX_API_KEY").unwrap()
    else {
        panic!("the key must remain a credential reference");
    };
    assert_eq!(query.reference.as_str(), "MINIMAX_TOKEN_PLAN_KEY");
    assert_eq!(query.kind.as_str(), "subscription-key");

    assert_eq!(bundle.tools().len(), 2);
    let web = &bundle.tools()[0];
    assert_eq!(web.spec().name, "web_search");
    assert_eq!(
        web.spec().parameters,
        serde_json::json!({
            "type":"object",
            "properties":{"query":{"type":"string"}},
            "required":["query"],
            "additionalProperties":false
        })
    );
    assert_eq!(web.evidence(), MiniMaxMcpFeatureEvidence::Current);
    assert_eq!(web.approval(), MiniMaxMcpApproval::Prompt);
    assert_eq!(web.untrusted_content(), &UntrustedContentBoundary::mcp());
    let image = bundle
        .tools()
        .iter()
        .find(|tool| tool.spec().name == "understand_image")
        .unwrap();
    assert_eq!(image.evidence(), MiniMaxMcpFeatureEvidence::Current);
    assert_eq!(image.approval(), MiniMaxMcpApproval::Prompt);
    assert_eq!(
        bundle.approval_for("unknown_tool"),
        MiniMaxMcpApproval::Deny
    );
}

#[test]
fn web_search_only_is_an_explicit_least_privilege_policy() {
    let bundle = MiniMaxTokenPlanMcpBundle::new(
        MiniMaxProfile::<TokenPlan>::international(),
        MiniMaxImageUnderstandingPolicy::WebSearchOnly,
    )
    .unwrap();

    assert_eq!(bundle.tools().len(), 1);
    assert_eq!(bundle.tools()[0].spec().name, "web_search");
    assert_eq!(
        bundle.approval_for("understand_image"),
        MiniMaxMcpApproval::Deny
    );
}

#[test]
fn mainland_mcp_install_is_refused_until_minimax_documents_that_host() {
    let error = MiniMaxTokenPlanMcpBundle::new(
        MiniMaxProfile::<TokenPlan>::new(MiniMaxRegion::MainlandChina),
        MiniMaxImageUnderstandingPolicy::AllDocumented,
    )
    .unwrap_err();
    assert_eq!(error, MiniMaxMcpBundleError::UnsupportedRegion);
}

#[test]
fn local_resource_delivery_requires_an_explicit_existing_absolute_directory() {
    assert_eq!(
        MiniMaxMcpResourceMode::local(PathBuf::from("relative")),
        Err(MiniMaxMcpBundleError::InvalidLocalRoot)
    );
    let root = std::env::current_dir().unwrap();
    let mode = MiniMaxMcpResourceMode::local(&root).unwrap();
    assert_eq!(mode.local_root(), Some(root.as_path()));

    let bundle = MiniMaxTokenPlanMcpBundle::new(
        MiniMaxProfile::<TokenPlan>::international(),
        MiniMaxImageUnderstandingPolicy::AllDocumented,
    )
    .unwrap()
    .with_resource_mode(mode);
    assert_eq!(
        bundle.environment().get("MINIMAX_API_RESOURCE_MODE"),
        Some(&MiniMaxMcpEnvironmentValue::Literal("local".to_owned()))
    );
    assert_eq!(
        bundle.environment().get("MINIMAX_MCP_BASE_PATH"),
        Some(&MiniMaxMcpEnvironmentValue::Literal(
            root.to_string_lossy().into_owned()
        ))
    );
    assert!(!format!("{bundle:?}").contains(&root.to_string_lossy().into_owned()));
}

#[derive(Default)]
struct Host {
    live: Arc<Mutex<Vec<Arc<()>>>>,
    registrations: Arc<Mutex<usize>>,
}

impl MiniMaxTokenPlanMcpHost for Host {
    fn required_services(&self) -> &'static [heycode_core::ServiceKey] {
        &[]
    }

    fn descriptor_families(&self) -> &'static [heycode_core::PluginContributionKind] {
        &[]
    }

    fn register(
        &self,
        context: &heycode_core::Context,
        bundle: &MiniMaxTokenPlanMcpBundle,
    ) -> Result<(), MiniMaxMcpHostFailure> {
        assert_eq!(bundle.server_id(), "minimax-token-plan");
        *self.registrations.lock().unwrap() += 1;
        let token = Arc::new(());
        self.live.lock().unwrap().push(token.clone());
        let rows = Arc::downgrade(&self.live);
        context.effect(move || {
            if let Some(rows) = rows.upgrade()
                && let Ok(mut rows) = rows.lock()
            {
                rows.retain(|row| !Arc::ptr_eq(row, &token));
            }
        });
        Ok(())
    }
}

#[test]
fn token_plan_bundle_is_an_effect_owned_plugin_not_only_a_detached_description() {
    let bundle = MiniMaxTokenPlanMcpBundle::new(
        MiniMaxProfile::<TokenPlan>::international(),
        MiniMaxImageUnderstandingPolicy::AllDocumented,
    )
    .unwrap();
    let host = Arc::new(Host::default());
    let plugin = minimax_token_plan_mcp_bundle_plugin(
        bundle,
        host.clone() as Arc<dyn MiniMaxTokenPlanMcpHost>,
    );
    let mut context = heycode_core::compose(&[plugin]).unwrap();
    assert_eq!(*host.registrations.lock().unwrap(), 1);
    assert_eq!(host.live.lock().unwrap().len(), 1);
    context.shutdown();
    assert!(host.live.lock().unwrap().is_empty());
}

#[test]
fn token_plan_launch_requires_resolved_executable_and_cwd_authority() {
    let bundle = MiniMaxTokenPlanMcpBundle::new(
        MiniMaxProfile::<TokenPlan>::international(),
        MiniMaxImageUnderstandingPolicy::AllDocumented,
    )
    .unwrap();
    let executable = std::env::current_exe().unwrap();
    let cwd = std::env::current_dir().unwrap();
    let launch = bundle.resolve_launch(&executable, &cwd).unwrap();
    assert_eq!(
        launch.executable(),
        std::fs::canonicalize(&executable).unwrap()
    );
    assert_eq!(launch.cwd(), std::fs::canonicalize(&cwd).unwrap());
    assert_eq!(launch.arguments(), ["minimax-coding-plan-mcp", "-y"]);
    assert_eq!(launch.environment(), &bundle.environment());
    let debug = format!("{launch:?}");
    assert!(!debug.contains(&executable.display().to_string()));
    assert!(!debug.contains("https://api.minimax.io"));
    assert!(!debug.contains("MINIMAX_TOKEN_PLAN_KEY"));

    assert!(bundle.resolve_launch("relative-uvx", &cwd).is_err());
    assert!(bundle.resolve_launch(&executable, "relative-cwd").is_err());
}
