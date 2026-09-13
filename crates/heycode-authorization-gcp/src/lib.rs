//! Google Cloud authorization: Application Default Credentials, project and
//! location health.
//!
//! PGCP01 answers three *independent* questions and reports a separate verdict
//! for each:
//!
//! - **Account** — is there an Application Default Credentials source, where
//!   did it come from, and is it usable? See [`GcpAccountHealth`].
//! - **Project** — is a target project selected, is it well formed, and does
//!   the ambient host attest it? See [`GcpProjectHealth`].
//! - **Location** — is a well-formed Vertex AI location selected? See
//!   [`GcpLocationHealth`].
//!
//! Each verdict projects to the tri-state [`GcpHealth`]. `Unknown` is a real
//! answer, never a rounded-off one: "the metadata server did not answer inside
//! the budget" is not "there are no credentials", and "no project is set" is
//! not "the project is invalid". Both distinctions live in the type.
//!
//! # Secrets
//!
//! An Application Default Credentials document contains a private key
//! (`service_account`) or a refresh token and client secret
//! (`authorized_user`). This crate reads only `type` and `project_id` out of
//! one, and nothing else in the document is ever bound to a name, formatted,
//! logged or returned.
//!
//! Reporting *where* a credential came from is explicitly in scope and safe:
//! [`GcpAdcOrigin`] names the environment variable, the well-known path or the
//! metadata host. Reporting what it contains is not, which is also why the
//! metadata probe never requests `instance/service-accounts/default/token`.
//!
//! # Sources
//!
//! - ADC search order and well-known file:
//!   <https://docs.cloud.google.com/docs/authentication/application-default-credentials>
//! - Environment variable names:
//!   <https://github.com/googleapis/google-auth-library-python/blob/main/google/auth/environment_vars.py>
//! - gcloud configuration directory and property variables:
//!   <https://docs.cloud.google.com/sdk/docs/configurations>
//! - Metadata server host, header and paths:
//!   <https://docs.cloud.google.com/compute/docs/metadata/overview>
//! - Project id rules:
//!   <https://docs.cloud.google.com/resource-manager/docs/creating-managing-projects>

mod env;
mod health;
mod metadata;
mod model;
mod plugin;
mod profile;
pub mod testing;

pub use env::{GcpEnvironment, GcpFileError, ProcessGcpEnvironment};
pub use health::{GcpAccountHealth, GcpAuthProfile, GcpLocationHealth, GcpProjectHealth};
pub use metadata::{DEFAULT_METADATA_HOST, ENV_GCE_METADATA_HOST, ENV_NO_GCE_CHECK};
pub use model::{
    GcpAdcCredentialType, GcpAdcFault, GcpAdcOrigin, GcpHealth, GcpLocation, GcpLocationError,
    GcpLocationKind, GcpLocationOrigin, GcpProjectId, GcpProjectIdError, GcpProjectIdKind,
    GcpProjectOrigin, GcpUncertainty,
};
pub use plugin::gcp_auth_plugin;
pub use profile::{
    ADC_FILE_NAME, ENV_APPDATA, ENV_CLOUDSDK_COMPUTE_REGION, ENV_CLOUDSDK_CONFIG,
    ENV_CLOUDSDK_CORE_PROJECT, ENV_GCLOUD_PROJECT, ENV_GOOGLE_APPLICATION_CREDENTIALS,
    ENV_GOOGLE_CLOUD_LOCATION, ENV_GOOGLE_CLOUD_PROJECT, ENV_HOME, ENV_SYSTEM_DRIVE,
    GCLOUD_CONFIG_DIR, GcpAuthService, GcpHostPlatform, GcpMetadataPolicy, GcpProfileRequest,
};

/// Google Cloud authentication profile service.
pub const SERVICE_GCP_AUTH: heycode_core::ServiceKey = heycode_core::ServiceKey::new("gcp-auth");
