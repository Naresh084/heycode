//! PAWS01 effective profile and region resolution.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_authorization_aws::{
    AWS_DEFAULT_REGION_VAR, AWS_PROFILE_VAR, AWS_REGION_VAR, AwsProfileName, AwsProfileResolution,
    AwsRegionOrigin, AwsRegionResolution, MapAwsHost,
};

const CONFIG: &str = "/home/dev/.aws/config";

fn config_host() -> MapAwsHost {
    MapAwsHost::new().with_home("/home/dev")
}

fn resolve_region(host: &MapAwsHost) -> AwsRegionResolution {
    AwsRegionResolution::resolve(host, &AwsProfileResolution::resolve(host))
}

#[test]
fn profile_comes_from_aws_profile_and_otherwise_from_the_sdk_default() {
    let named = MapAwsHost::new().with_var(AWS_PROFILE_VAR, "staging");
    assert_eq!(
        AwsProfileResolution::resolve(&named),
        AwsProfileResolution::Environment {
            profile: AwsProfileName::new("staging").unwrap()
        }
    );
    assert_eq!(
        AwsProfileResolution::resolve(&MapAwsHost::new()),
        AwsProfileResolution::Default {
            profile: AwsProfileName::default_profile()
        }
    );
}

#[test]
fn a_malformed_aws_profile_never_silently_becomes_the_default_profile() {
    let host = MapAwsHost::new().with_var(AWS_PROFILE_VAR, "[not a profile]");
    let resolution = AwsProfileResolution::resolve(&host);
    assert_eq!(resolution, AwsProfileResolution::Malformed);
    assert_eq!(resolution.profile(), None);
}

#[test]
fn region_precedence_is_aws_region_then_aws_default_region_then_the_profile() {
    let both = config_host()
        .with_var(AWS_REGION_VAR, "eu-west-2")
        .with_var(AWS_DEFAULT_REGION_VAR, "us-east-1")
        .with_file(CONFIG, "[default]\nregion = ap-south-1\n");
    assert_eq!(
        resolve_region(&both),
        AwsRegionResolution::Resolved {
            region: heycode_authorization_aws::AwsRegion::new("eu-west-2").unwrap(),
            origin: AwsRegionOrigin::Environment {
                variable: AWS_REGION_VAR
            },
        }
    );

    let legacy = config_host()
        .with_var(AWS_DEFAULT_REGION_VAR, "us-east-1")
        .with_file(CONFIG, "[default]\nregion = ap-south-1\n");
    assert_eq!(
        resolve_region(&legacy),
        AwsRegionResolution::Resolved {
            region: heycode_authorization_aws::AwsRegion::new("us-east-1").unwrap(),
            origin: AwsRegionOrigin::Environment {
                variable: AWS_DEFAULT_REGION_VAR
            },
        }
    );

    let file = config_host().with_file(CONFIG, "[default]\nregion = ap-south-1\n");
    assert_eq!(
        resolve_region(&file),
        AwsRegionResolution::Resolved {
            region: heycode_authorization_aws::AwsRegion::new("ap-south-1").unwrap(),
            origin: AwsRegionOrigin::SharedConfigFile {
                profile: AwsProfileName::default_profile()
            },
        }
    );
}

#[test]
fn a_region_is_never_invented_when_nothing_configures_one() {
    let resolution = resolve_region(&config_host());
    assert_eq!(resolution, AwsRegionResolution::Unresolved);
    assert_eq!(resolution.region(), None);
}

#[test]
fn an_unreadable_shared_config_leaves_the_region_undetermined_not_unresolved() {
    let host = config_host().with_unreadable_file(CONFIG);
    let resolution = resolve_region(&host);
    assert_eq!(
        resolution,
        AwsRegionResolution::Undetermined,
        "an unreadable config proves nothing about whether a region is set"
    );
    assert_eq!(resolution.region(), None);
}

#[test]
fn a_malformed_region_reports_its_origin_and_yields_no_region() {
    let host = config_host().with_var(AWS_REGION_VAR, "US EAST 1");
    let resolution = resolve_region(&host);
    assert_eq!(
        resolution,
        AwsRegionResolution::Malformed {
            origin: AwsRegionOrigin::Environment {
                variable: AWS_REGION_VAR
            }
        }
    );
    assert_eq!(resolution.region(), None);
}

#[test]
fn a_malformed_profile_stops_the_profile_region_lookup_rather_than_reading_default() {
    let host = config_host()
        .with_var(AWS_PROFILE_VAR, "[bad]")
        .with_file(CONFIG, "[default]\nregion = ap-south-1\n");
    assert_eq!(resolve_region(&host), AwsRegionResolution::Unresolved);
}

#[test]
fn the_config_file_override_variable_replaces_the_home_relative_path() {
    let host = MapAwsHost::new()
        .with_home("/home/dev")
        .with_var("AWS_CONFIG_FILE", "/etc/aws/config")
        .with_file("/etc/aws/config", "[default]\nregion = sa-east-1\n")
        .with_file(CONFIG, "[default]\nregion = ap-south-1\n");
    assert_eq!(
        resolve_region(&host).region().map(|region| region.as_str()),
        Some("sa-east-1")
    );
}
