//! PZA05 Coding Plan MCP bundle contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use heycode_core::{Context, PluginContributionKind, compose};
use heycode_provider_zai::{
    ZAI_CODING_API_KEY_REFERENCE, ZAI_MCP_READER_ENDPOINT, ZAI_MCP_SEARCH_ENDPOINT,
    ZAI_MCP_ZREAD_ENDPOINT, ZaiCodingMcpBundle, ZaiCodingMcpHost, ZaiMcpApproval,
    ZaiMcpHostFailure, ZaiMcpServerSpec, ZaiMcpTransportSpec, zai_coding_mcp_bundle_plugin,
};

#[derive(Clone)]
struct Entry {
    spec: ZaiMcpServerSpec,
    token: Arc<()>,
}

#[derive(Default)]
struct Host {
    entries: Arc<Mutex<BTreeMap<String, Entry>>>,
    attempted: Arc<Mutex<Vec<String>>>,
    fail_on: Option<&'static str>,
}

impl Host {
    fn failing(id: &'static str) -> Self {
        Self {
            fail_on: Some(id),
            ..Self::default()
        }
    }

    fn ids(&self) -> Vec<String> {
        self.entries.lock().unwrap().keys().cloned().collect()
    }
}

impl ZaiCodingMcpHost for Host {
    fn required_services(&self) -> &'static [heycode_core::ServiceKey] {
        &[]
    }

    fn descriptor_families(&self) -> &'static [PluginContributionKind] {
        &[]
    }

    fn register(
        &self,
        context: &Context,
        server: &ZaiMcpServerSpec,
    ) -> Result<(), ZaiMcpHostFailure> {
        self.attempted.lock().unwrap().push(server.id().to_owned());
        if self.fail_on == Some(server.id()) {
            return Err(ZaiMcpHostFailure::Unavailable);
        }
        let id = server.id().to_owned();
        let token = Arc::new(());
        let mut entries = self.entries.lock().unwrap();
        if entries.contains_key(&id) {
            return Err(ZaiMcpHostFailure::Duplicate);
        }
        entries.insert(
            id.clone(),
            Entry {
                spec: server.clone(),
                token: Arc::clone(&token),
            },
        );
        drop(entries);
        let registry = Arc::downgrade(&self.entries);
        context.effect(move || {
            let Some(registry) = registry.upgrade() else {
                return;
            };
            let Ok(mut entries) = registry.lock() else {
                return;
            };
            let matches = entries
                .get(&id)
                .is_some_and(|entry| Arc::ptr_eq(&entry.token, &token));
            if matches {
                entries.remove(&id);
            }
        });
        Ok(())
    }
}

#[test]
fn official_bundle_keeps_all_four_transport_auth_and_tool_contracts() {
    let bundle = ZaiCodingMcpBundle::official().unwrap();
    assert_eq!(
        bundle
            .servers()
            .iter()
            .map(ZaiMcpServerSpec::id)
            .collect::<Vec<_>>(),
        ["web-search-prime", "web-reader", "zai-vision", "zread"]
    );

    let search = &bundle.servers()[0];
    assert!(!search.required());
    assert_eq!(search.default_approval(), ZaiMcpApproval::Prompt);
    assert!(!search.exposure().resources);
    assert!(!search.exposure().prompts);
    assert!(!search.exposure().instructions);
    assert_eq!(search.tools(), ["webSearchPrime"]);
    let ZaiMcpTransportSpec::StreamableHttp {
        endpoint,
        authorization,
    } = search.transport()
    else {
        panic!("search must be remote HTTP");
    };
    assert_eq!(*endpoint, ZAI_MCP_SEARCH_ENDPOINT);
    assert_eq!(
        authorization.credential_reference(),
        ZAI_CODING_API_KEY_REFERENCE
    );
    assert_eq!(authorization.scheme(), "Bearer");
    assert_eq!(
        authorization.credential_query().reference.as_str(),
        ZAI_CODING_API_KEY_REFERENCE
    );
    assert_eq!(authorization.credential_query().kind.as_str(), "api-key");

    let reader = &bundle.servers()[1];
    assert_eq!(reader.tools(), ["webReader"]);
    assert!(matches!(
        reader.transport(),
        ZaiMcpTransportSpec::StreamableHttp { endpoint, .. }
            if *endpoint == ZAI_MCP_READER_ENDPOINT
    ));

    let vision = &bundle.servers()[2];
    assert_eq!(vision.tools().len(), 8);
    assert!(vision.tools().contains(&"ui_to_artifact"));
    assert!(vision.tools().contains(&"video_analysis"));
    let ZaiMcpTransportSpec::Npx {
        package,
        minimum_package_version,
        minimum_node_major,
        credential_environment,
        static_environment,
    } = vision.transport()
    else {
        panic!("vision must be the documented local npx server");
    };
    assert_eq!(*package, "@z_ai/mcp-server");
    assert_eq!(*minimum_package_version, "0.1.2");
    assert_eq!(*minimum_node_major, 22);
    assert_eq!(credential_environment.name(), "Z_AI_API_KEY");
    assert_eq!(
        credential_environment.credential_reference(),
        ZAI_CODING_API_KEY_REFERENCE
    );
    assert_eq!(
        credential_environment.credential_query().kind.as_str(),
        "api-key"
    );
    assert_eq!(static_environment, &[("Z_AI_MODE", "ZAI")]);

    let zread = &bundle.servers()[3];
    assert_eq!(
        zread.tools(),
        ["search_doc", "get_repo_structure", "read_file"]
    );
    assert!(matches!(
        zread.transport(),
        ZaiMcpTransportSpec::StreamableHttp { endpoint, .. }
            if *endpoint == ZAI_MCP_ZREAD_ENDPOINT
    ));

    let snapshot = bundle.snapshot();
    let rendered = format!("{bundle:?}{snapshot}");
    assert!(!rendered.contains("test-key"));
    assert!(rendered.contains(ZAI_CODING_API_KEY_REFERENCE));
}

#[test]
fn bundle_host_registers_and_disposes_all_four_in_one_real_composition() {
    let host = Arc::new(Host::default());
    let plugin =
        zai_coding_mcp_bundle_plugin(Arc::clone(&host) as Arc<dyn ZaiCodingMcpHost>).unwrap();
    let mut context = compose(&[plugin]).unwrap();
    assert_eq!(
        host.ids(),
        ["web-reader", "web-search-prime", "zai-vision", "zread"]
    );
    assert_eq!(
        host.entries.lock().unwrap()["zai-vision"]
            .spec
            .tools()
            .len(),
        8
    );
    context.shutdown();
    assert!(host.ids().is_empty());
}

#[test]
fn one_failed_server_rolls_back_the_registered_prefix_and_stops_the_suffix() {
    let host = Arc::new(Host::failing("zai-vision"));
    let plugin =
        zai_coding_mcp_bundle_plugin(Arc::clone(&host) as Arc<dyn ZaiCodingMcpHost>).unwrap();
    assert!(compose(&[plugin]).is_err());
    assert!(host.ids().is_empty());
    assert_eq!(
        *host.attempted.lock().unwrap(),
        ["web-search-prime", "web-reader", "zai-vision"]
    );
}

#[test]
fn vision_launch_requires_resolved_runtime_and_pins_the_observed_package_version() {
    let bundle = ZaiCodingMcpBundle::official().unwrap();
    let vision = &bundle.servers()[2];
    let executable = std::env::current_exe().unwrap();
    let cwd = std::env::current_dir().unwrap();
    let launch = vision
        .resolve_vision_launch(&executable, &cwd, 22, "0.1.2")
        .unwrap();
    assert_eq!(
        launch.executable(),
        std::fs::canonicalize(&executable).unwrap()
    );
    assert_eq!(launch.cwd(), std::fs::canonicalize(&cwd).unwrap());
    assert_eq!(launch.arguments(), ["-y", "@z_ai/mcp-server@0.1.2"]);
    assert_eq!(
        launch
            .credential_environment()
            .credential_query()
            .kind
            .as_str(),
        "api-key"
    );
    assert!(!format!("{launch:?}").contains(&executable.display().to_string()));

    assert!(
        vision
            .resolve_vision_launch(&executable, &cwd, 21, "0.1.2")
            .is_err()
    );
    assert!(
        vision
            .resolve_vision_launch(&executable, &cwd, 22, "0.1.1")
            .is_err()
    );
    assert!(
        vision
            .resolve_vision_launch(&executable, &cwd, 22, "00.01.002")
            .is_err(),
        "the package pin must be canonical rather than merely numerically equivalent"
    );
    assert!(
        vision
            .resolve_vision_launch("relative-npx", &cwd, 22, "0.1.2")
            .is_err()
    );
    assert_eq!(
        bundle.servers()[0]
            .resolve_vision_launch(&executable, &cwd, 22, "0.1.2")
            .unwrap_err(),
        heycode_provider_zai::ZaiMcpBundleError::NotVisionServer
    );
}
