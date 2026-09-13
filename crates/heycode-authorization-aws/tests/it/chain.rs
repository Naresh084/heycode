//! PAWS01 AWS SDK credential-chain discovery.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_authorization_aws::{
    AWS_ACCESS_KEY_ID_VAR, AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR,
    AWS_CONTAINER_CREDENTIALS_RELATIVE_URI_VAR, AWS_PROFILE_VAR, AWS_ROLE_ARN_VAR,
    AWS_SECRET_ACCESS_KEY_VAR, AWS_WEB_IDENTITY_TOKEN_FILE_VAR, AwsChainDiscovery,
    AwsCredentialSource, AwsProfileName, AwsProfileResolution, AwsRejection, AwsUndetermined,
    MapAwsHost, discover_credential_chain,
};

const CONFIG: &str = "/home/dev/.aws/config";
const CREDENTIALS: &str = "/home/dev/.aws/credentials";

fn host() -> MapAwsHost {
    MapAwsHost::new().with_home("/home/dev")
}

fn discover(host: &MapAwsHost) -> AwsChainDiscovery {
    discover_credential_chain(host, &AwsProfileResolution::resolve(host))
}

fn default_profile() -> AwsProfileName {
    AwsProfileName::default_profile()
}

#[test]
fn static_environment_keys_win_over_the_shared_profile() {
    let host = host()
        .with_var(AWS_ACCESS_KEY_ID_VAR, "AKIAEXAMPLE")
        .with_var(AWS_SECRET_ACCESS_KEY_VAR, "secret-must-not-leak")
        .with_file(CREDENTIALS, "[default]\naws_access_key_id = AKIAFILE\n");
    assert_eq!(
        discover(&host),
        AwsChainDiscovery::Found(AwsCredentialSource::Environment {
            variable: AWS_ACCESS_KEY_ID_VAR
        })
    );
}

#[test]
fn half_a_static_key_pair_is_a_rejection_not_an_empty_environment() {
    let id_only = host().with_var(AWS_ACCESS_KEY_ID_VAR, "AKIAEXAMPLE");
    assert_eq!(
        discover(&id_only),
        AwsChainDiscovery::Unusable {
            source: AwsCredentialSource::Environment {
                variable: AWS_ACCESS_KEY_ID_VAR
            },
            reason: AwsRejection::IncompleteConfiguration,
        }
    );

    let secret_only = host().with_var(AWS_SECRET_ACCESS_KEY_VAR, "secret-must-not-leak");
    assert_eq!(
        discover(&secret_only),
        AwsChainDiscovery::Unusable {
            source: AwsCredentialSource::Environment {
                variable: AWS_SECRET_ACCESS_KEY_VAR
            },
            reason: AwsRejection::IncompleteConfiguration,
        }
    );
}

#[test]
fn a_web_identity_token_without_a_role_is_incomplete_configuration() {
    let complete = host()
        .with_var(AWS_WEB_IDENTITY_TOKEN_FILE_VAR, "/var/run/token")
        .with_var(AWS_ROLE_ARN_VAR, "arn:aws:iam::123456789012:role/dev");
    assert_eq!(
        discover(&complete),
        AwsChainDiscovery::Found(AwsCredentialSource::WebIdentityToken {
            variable: AWS_WEB_IDENTITY_TOKEN_FILE_VAR
        })
    );

    let partial = host().with_var(AWS_WEB_IDENTITY_TOKEN_FILE_VAR, "/var/run/token");
    assert_eq!(
        discover(&partial),
        AwsChainDiscovery::Unusable {
            source: AwsCredentialSource::WebIdentityToken {
                variable: AWS_WEB_IDENTITY_TOKEN_FILE_VAR
            },
            reason: AwsRejection::IncompleteConfiguration,
        }
    );
}

#[test]
fn each_documented_profile_shape_resolves_to_its_own_source() {
    let cases: [(&str, &str, AwsCredentialSource); 5] = [
        (
            CREDENTIALS,
            "[default]\naws_access_key_id = AKIA\naws_secret_access_key = s3cret\n",
            AwsCredentialSource::ProfileAccessKeys {
                profile: default_profile(),
            },
        ),
        (
            CONFIG,
            "[default]\nrole_arn = arn:aws:iam::123456789012:role/dev\nsource_profile = base\n",
            AwsCredentialSource::AssumeRole {
                profile: default_profile(),
            },
        ),
        (
            CONFIG,
            "[default]\nsso_session = corp\nsso_account_id = 123456789012\n",
            AwsCredentialSource::SsoSession {
                profile: default_profile(),
            },
        ),
        (
            CONFIG,
            "[default]\nsso_start_url = https://acme.awsapps.com/start\nsso_account_id = 123456789012\nsso_role_name = Dev\n",
            AwsCredentialSource::SsoSession {
                profile: default_profile(),
            },
        ),
        (
            CONFIG,
            "[default]\ncredential_process = /usr/bin/helper --json\n",
            AwsCredentialSource::CredentialProcess {
                profile: default_profile(),
            },
        ),
    ];
    for (path, contents, expected) in cases {
        let host = host().with_file(path, contents);
        assert_eq!(
            discover(&host),
            AwsChainDiscovery::Found(expected.clone()),
            "{contents:?} should resolve to {}",
            expected.code()
        );
    }
}

#[test]
fn an_incomplete_assume_role_or_sso_profile_is_rejected_rather_than_found() {
    let orphan_role = host().with_file(
        CONFIG,
        "[default]\nrole_arn = arn:aws:iam::123456789012:role/dev\n",
    );
    assert_eq!(
        discover(&orphan_role),
        AwsChainDiscovery::Unusable {
            source: AwsCredentialSource::AssumeRole {
                profile: default_profile()
            },
            reason: AwsRejection::IncompleteConfiguration,
        }
    );

    let legacy_sso = host().with_file(
        CONFIG,
        "[default]\nsso_start_url = https://acme.awsapps.com/start\n",
    );
    assert_eq!(
        discover(&legacy_sso),
        AwsChainDiscovery::Unusable {
            source: AwsCredentialSource::SsoSession {
                profile: default_profile()
            },
            reason: AwsRejection::IncompleteConfiguration,
        }
    );
}

#[test]
fn unreadable_shared_configuration_blocks_discovery_instead_of_reporting_absent() {
    for path in [CREDENTIALS, CONFIG] {
        let host = host().with_unreadable_file(path);
        assert_eq!(
            discover(&host),
            AwsChainDiscovery::Blocked(AwsUndetermined::ConfigurationUnreadable),
            "an unreadable {path} proves nothing about what is configured"
        );
    }
}

#[test]
fn the_relative_container_uri_wins_over_the_absolute_alternative() {
    let both = host()
        .with_var(
            AWS_CONTAINER_CREDENTIALS_RELATIVE_URI_VAR,
            "/v2/credentials",
        )
        .with_var(
            AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR,
            "http://localhost/creds",
        );
    assert_eq!(
        discover(&both),
        AwsChainDiscovery::Found(AwsCredentialSource::ContainerRole {
            variable: AWS_CONTAINER_CREDENTIALS_RELATIVE_URI_VAR
        })
    );

    let absolute = host().with_var(
        AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR,
        "http://localhost/creds",
    );
    assert_eq!(
        discover(&absolute),
        AwsChainDiscovery::Found(AwsCredentialSource::ContainerRole {
            variable: AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR
        })
    );
}

#[test]
fn nothing_configured_is_absent_and_claims_no_unobserved_source() {
    assert_eq!(discover(&host()), AwsChainDiscovery::Absent);
}

#[test]
fn profile_sections_follow_each_files_own_heading_dialect() {
    let named = MapAwsHost::new()
        .with_home("/home/dev")
        .with_var(AWS_PROFILE_VAR, "work")
        .with_file(
            CONFIG,
            "[profile work]\ncredential_process = /usr/bin/helper\n",
        );
    assert_eq!(
        discover(&named),
        AwsChainDiscovery::Found(AwsCredentialSource::CredentialProcess {
            profile: AwsProfileName::new("work").unwrap()
        })
    );

    // `[work]` is a credentials-file heading; the config file requires the
    // `profile ` prefix and must not match it.
    let unprefixed = MapAwsHost::new()
        .with_home("/home/dev")
        .with_var(AWS_PROFILE_VAR, "work")
        .with_file(CONFIG, "[work]\ncredential_process = /usr/bin/helper\n");
    assert_eq!(discover(&unprefixed), AwsChainDiscovery::Absent);

    let credentials = MapAwsHost::new()
        .with_home("/home/dev")
        .with_var(AWS_PROFILE_VAR, "work")
        .with_file(
            CREDENTIALS,
            "[work]\naws_access_key_id = AKIA\naws_secret_access_key = s3cret\n",
        );
    assert_eq!(
        discover(&credentials),
        AwsChainDiscovery::Found(AwsCredentialSource::ProfileAccessKeys {
            profile: AwsProfileName::new("work").unwrap()
        })
    );
}

#[test]
fn indented_sub_properties_and_comments_do_not_become_profile_settings() {
    let host = host().with_file(
        CONFIG,
        "[default]\n# credential_process = /commented/out\n; role_arn = arn:aws:iam::1:role/x\ns3 =\n  credential_process = /nested/helper\n",
    );
    assert_eq!(discover(&host), AwsChainDiscovery::Absent);
}
