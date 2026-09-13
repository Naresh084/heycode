//! PGCP01 composition: the profile service is mounted, attributed and
//! dependent on the HTTP transport it actually uses.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use heycode_authorization_gcp::testing::MapGcpEnvironment;
use heycode_authorization_gcp::{
    GcpAuthService, GcpEnvironment, GcpFileError, ProcessGcpEnvironment, SERVICE_GCP_AUTH,
    gcp_auth_plugin,
};
use heycode_core::{ContributionKind, compose};
use heycode_http::http_plugin;

#[test]
fn the_plugin_provides_the_profile_service_under_its_own_name() {
    let context = compose(&[http_plugin(), gcp_auth_plugin()]).expect("composition succeeds");

    assert!(context.has(SERVICE_GCP_AUTH));
    assert_eq!(
        context.owner_of(SERVICE_GCP_AUTH),
        Some("authorization-gcp")
    );
    assert!(context.get::<GcpAuthService>(SERVICE_GCP_AUTH).is_some());
}

#[test]
fn the_service_row_is_attributed_to_this_plugin_in_the_exact_inventory() {
    let context = compose(&[http_plugin(), gcp_auth_plugin()]).expect("composition succeeds");
    let snapshot = context
        .plugin_inventory()
        .snapshot()
        .expect("inventory readable");

    assert!(
        snapshot.contributions.iter().any(|row| {
            row.plugin == "authorization-gcp"
                && row.kind == ContributionKind::Service
                && row.name == SERVICE_GCP_AUTH.as_str()
        }),
        "the mounted service must be attributable to its owning plugin"
    );
}

#[test]
fn composition_fails_loud_when_the_http_transport_is_absent() {
    let Err(error) = compose(&[gcp_auth_plugin()]) else {
        panic!("composition must not proceed without the HTTP transport");
    };

    let rendered = error.to_string();
    assert!(
        rendered.contains("http"),
        "the unsatisfied dependency must be named: {rendered}"
    );
}

#[test]
fn the_plugin_declares_the_service_it_publishes() {
    let plugin = gcp_auth_plugin();

    assert_eq!(plugin.name(), "authorization-gcp");
    assert_eq!(plugin.provides(), &[SERVICE_GCP_AUTH]);
    assert_eq!(plugin.inject(), &[heycode_http::SERVICE_HTTP]);
    assert_eq!(plugin.descriptor().id, "authorization-gcp");
}

#[test]
fn the_process_environment_reads_a_present_variable_and_reports_an_absent_one() {
    let environment = ProcessGcpEnvironment;

    assert_eq!(
        environment.var("CARGO_PKG_NAME").as_deref(),
        Some("heycode-authorization-gcp")
    );
    assert_eq!(
        environment.var("HEYCODE_GCP_VARIABLE_THAT_IS_NEVER_SET"),
        None
    );
}

#[test]
fn a_blank_variable_is_absent_rather_than_an_empty_value() {
    let environment = MapGcpEnvironment::new().with_var("GOOGLE_CLOUD_PROJECT", "   ");

    assert_eq!(environment.var("GOOGLE_CLOUD_PROJECT"), None);
}

#[test]
fn the_process_file_read_separates_missing_from_oversized() {
    let environment = ProcessGcpEnvironment;
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");

    assert!(environment.read_file(&manifest, 64 * 1024).is_ok());
    assert_eq!(
        environment.read_file(&manifest, 1),
        Err(GcpFileError::TooLarge)
    );
    assert_eq!(
        environment.read_file(Path::new("/heycode-gcp-path-that-does-not-exist"), 64),
        Err(GcpFileError::NotFound)
    );
    assert_eq!(
        environment.read_file(Path::new(env!("CARGO_MANIFEST_DIR")), 64 * 1024),
        Err(GcpFileError::Unreadable),
        "a directory exists but is not a readable credential document"
    );
}
