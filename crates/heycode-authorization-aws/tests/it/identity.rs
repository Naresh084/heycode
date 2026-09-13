//! PAWS01 validated AWS identity newtypes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_authorization_aws::{AwsAuthError, AwsProfileName, AwsRegion};

#[test]
fn region_id_rejects_every_byte_that_could_escape_a_hostname_label() {
    for accepted in [
        "us-east-1",
        "ap-southeast-4",
        "us-gov-west-1",
        "cn-north-1",
        "eu1",
    ] {
        assert_eq!(
            AwsRegion::new(accepted).unwrap().as_str(),
            accepted,
            "{accepted} should be a region id"
        );
    }
    for rejected in [
        "",
        "u",
        "US-EAST-1",
        "us_east_1",
        "us east 1",
        "us-east-1.amazonaws.com",
        "us-east-1/foundation-models",
        "us-east-1:443",
        "-us-east-1",
        "1us-east",
        "us-east-",
        "us-east-1\n",
        "us..east",
        &"a".repeat(65),
    ] {
        assert!(
            matches!(
                AwsRegion::new(rejected),
                Err(AwsAuthError::InvalidRegion { .. })
            ),
            "{rejected:?} must not be accepted as a region id"
        );
    }
}

#[test]
fn profile_name_rejects_control_bytes_section_brackets_and_edge_whitespace() {
    for accepted in ["default", "dev", "my.profile", "team-a_1", "with space"] {
        assert_eq!(AwsProfileName::new(accepted).unwrap().as_str(), accepted);
    }
    for rejected in [
        "",
        " leading",
        "trailing ",
        "with\nnewline",
        "with\ttab",
        "[bracketed]",
        "profile]",
        "caf\u{e9}",
        &"p".repeat(65),
    ] {
        assert_eq!(
            AwsProfileName::new(rejected),
            Err(AwsAuthError::InvalidProfileName),
            "{rejected:?} must not be accepted as a profile name"
        );
    }
}

#[test]
fn the_sdk_default_profile_is_the_literal_name_aws_uses() {
    assert_eq!(
        AwsProfileName::default_profile().as_str(),
        heycode_authorization_aws::AWS_DEFAULT_PROFILE
    );
    assert_eq!(heycode_authorization_aws::AWS_DEFAULT_PROFILE, "default");
}
