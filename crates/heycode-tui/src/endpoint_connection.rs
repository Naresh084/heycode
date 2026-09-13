//! Masked endpoint authorization through the provider-owned catalog boundary.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_authorization::{AuthorizationFlowId, AuthorizationService};
use heycode_authorization_api_key::{
    ApiKeyAuthorizationFlow, ApiKeyFlowConfig, ApiKeyValidationFailure, ApiKeyValidator,
    SecretPrompt,
};
use heycode_credentials::{CredentialKind, CredentialQuery, CredentialReference, CredentialSecret};
use heycode_llm::{CatalogError, CatalogFailureKind, CatalogRegistry, CatalogSnapshot};
use tokio_util::sync::CancellationToken;

pub(crate) async fn authorize_endpoint(
    models: Arc<CatalogRegistry>,
    authorization: Arc<AuthorizationService>,
    prompt: Arc<dyn SecretPrompt>,
    provider: String,
    endpoint: String,
    reference: CredentialReference,
    cancellation: CancellationToken,
) -> Result<CatalogSnapshot, CatalogError> {
    let failure = |message: &str| CatalogError::Refresh {
        provider: provider.clone(),
        kind: CatalogFailureKind::Unavailable,
        message: message.into(),
    };
    if !models.supports_endpoint_credentials(&provider)? {
        return Err(failure(
            "This server integration does not support API-key setup",
        ));
    }
    // Validate the provider's address and server identity before asking for a secret.
    match models
        .probe_endpoint(&provider, &endpoint, cancellation.clone())
        .await
    {
        Ok(_)
        | Err(CatalogError::Refresh {
            kind: CatalogFailureKind::Unauthorized,
            ..
        }) => {}
        Err(error) => return Err(error),
    }
    let validator = Arc::new(EndpointValidator {
        models,
        provider: provider.clone(),
        endpoint: endpoint.clone(),
        snapshot: Mutex::new(None),
    });
    let flow = ApiKeyAuthorizationFlow::new(
        ApiKeyFlowConfig {
            id: AuthorizationFlowId::new("endpoint-api-key")
                .map_err(|_| failure("Endpoint authorization is unavailable"))?,
            label: "Server API key".into(),
            query: CredentialQuery::new(
                reference,
                CredentialKind::new("api-key")
                    .map_err(|_| failure("Endpoint authorization is unavailable"))?,
            ),
            prompt: format!("API key for {endpoint}"),
        },
        prompt,
        validator.clone(),
    );
    authorization
        .authorize_once(&flow, None, cancellation)
        .await
        .map_err(|error| failure(&error.to_string()))?;
    validator
        .snapshot
        .lock()
        .map_err(|_| CatalogError::RegistryUnavailable)?
        .take()
        .ok_or_else(|| failure("The validated server catalog is unavailable"))
}

pub(crate) async fn authorize_parameters(
    models: Arc<CatalogRegistry>,
    authorization: Arc<AuthorizationService>,
    prompt: Arc<dyn SecretPrompt>,
    provider: String,
    parameters: BTreeMap<String, String>,
    reference: CredentialReference,
    cancellation: CancellationToken,
) -> Result<CatalogSnapshot, CatalogError> {
    let failure = |message: &str| CatalogError::Refresh {
        provider: provider.clone(),
        kind: CatalogFailureKind::Unavailable,
        message: message.into(),
    };
    if !models.supports_parameter_credentials(&provider)? {
        return Err(failure(
            "This cloud integration does not support credential setup",
        ));
    }
    match models
        .probe_parameters(&provider, &parameters, cancellation.clone())
        .await
    {
        Ok(_)
        | Err(CatalogError::Refresh {
            kind: CatalogFailureKind::Unauthorized,
            ..
        }) => {}
        Err(error) => return Err(error),
    }
    let validator = Arc::new(ParameterValidator {
        models,
        provider: provider.clone(),
        parameters,
        snapshot: Mutex::new(None),
    });
    let flow = ApiKeyAuthorizationFlow::new(
        ApiKeyFlowConfig {
            id: AuthorizationFlowId::new("connection-credential")
                .map_err(|_| failure("Cloud authorization is unavailable"))?,
            label: "Cloud credential".into(),
            query: CredentialQuery::new(
                reference,
                CredentialKind::new("api-key")
                    .map_err(|_| failure("Cloud authorization is unavailable"))?,
            ),
            prompt: format!("Credential for {provider}"),
        },
        prompt,
        validator.clone(),
    );
    authorization
        .authorize_once(&flow, None, cancellation)
        .await
        .map_err(|error| failure(&error.to_string()))?;
    validator
        .snapshot
        .lock()
        .map_err(|_| CatalogError::RegistryUnavailable)?
        .take()
        .ok_or_else(|| failure("The validated cloud catalog is unavailable"))
}

struct EndpointValidator {
    models: Arc<CatalogRegistry>,
    provider: String,
    endpoint: String,
    snapshot: Mutex<Option<CatalogSnapshot>>,
}

struct ParameterValidator {
    models: Arc<CatalogRegistry>,
    provider: String,
    parameters: BTreeMap<String, String>,
    snapshot: Mutex<Option<CatalogSnapshot>>,
}

#[async_trait]
impl ApiKeyValidator for ParameterValidator {
    async fn validate(
        &self,
        secret: &CredentialSecret,
        cancellation: CancellationToken,
    ) -> Result<(), ApiKeyValidationFailure> {
        let snapshot = self
            .models
            .probe_parameters_with_credential(
                &self.provider,
                &self.parameters,
                Some(secret),
                cancellation,
            )
            .await
            .map_err(map_catalog_validation_failure)?;
        *self
            .snapshot
            .lock()
            .map_err(|_| ApiKeyValidationFailure::Host)? = Some(snapshot);
        Ok(())
    }
}

fn map_catalog_validation_failure(error: CatalogError) -> ApiKeyValidationFailure {
    match error {
        CatalogError::Cancelled { .. } => ApiKeyValidationFailure::Cancelled,
        CatalogError::Refresh {
            kind: CatalogFailureKind::Unauthorized,
            ..
        } => ApiKeyValidationFailure::Unauthorized,
        CatalogError::Refresh {
            kind: CatalogFailureKind::Network,
            ..
        } => ApiKeyValidationFailure::Network,
        _ => ApiKeyValidationFailure::Host,
    }
}

#[async_trait]
impl ApiKeyValidator for EndpointValidator {
    async fn validate(
        &self,
        secret: &CredentialSecret,
        cancellation: CancellationToken,
    ) -> Result<(), ApiKeyValidationFailure> {
        let snapshot = self
            .models
            .probe_endpoint_with_credential(
                &self.provider,
                &self.endpoint,
                Some(secret),
                cancellation,
            )
            .await
            .map_err(map_catalog_validation_failure)?;
        *self
            .snapshot
            .lock()
            .map_err(|_| ApiKeyValidationFailure::Host)? = Some(snapshot);
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use heycode_credentials::{
        CredentialProvider, CredentialProviderId, CredentialProviderState, CredentialSource,
        CredentialsService,
    };
    use heycode_llm::{CatalogFetchError, ModelCatalog, ModelDescriptor, ProviderDescriptor};
    use std::collections::BTreeMap;

    struct Store {
        id: CredentialProviderId,
        values: Mutex<BTreeMap<String, String>>,
    }
    impl CredentialProvider for Store {
        fn id(&self) -> &CredentialProviderId {
            &self.id
        }
        fn precedence(&self) -> u16 {
            20
        }
        fn inspect(&self, query: &CredentialQuery) -> Result<CredentialProviderState, String> {
            Ok(
                if self
                    .values
                    .lock()
                    .unwrap()
                    .contains_key(query.reference.as_str())
                {
                    CredentialProviderState::configured(CredentialSource::File, true)
                } else {
                    CredentialProviderState::unconfigured(true)
                },
            )
        }
        fn resolve(&self, query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
            Ok(self
                .values
                .lock()
                .unwrap()
                .get(query.reference.as_str())
                .cloned()
                .map(CredentialSecret::new))
        }
        fn write(&self, query: &CredentialQuery, secret: &CredentialSecret) -> Result<(), String> {
            self.values
                .lock()
                .unwrap()
                .insert(query.reference.as_str().into(), secret.expose().into());
            Ok(())
        }
    }
    struct Source;
    #[async_trait]
    impl ModelCatalog for Source {
        fn provider(&self) -> ProviderDescriptor {
            ProviderDescriptor {
                id: "local-test".into(),
                display_name: "Local test".into(),
                protocols: vec![],
            }
        }
        fn supports_endpoint_credentials(&self) -> bool {
            true
        }
        async fn fetch(
            &self,
            _: CancellationToken,
        ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
            panic!("active route must not be queried")
        }
        async fn fetch_endpoint(
            &self,
            endpoint: &str,
            _: CancellationToken,
        ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
            assert_eq!(endpoint, "http://localhost:2234");
            Err(CatalogFetchError::new(
                CatalogFailureKind::Unauthorized,
                "key required",
            ))
        }
        async fn fetch_endpoint_with_credential(
            &self,
            endpoint: &str,
            credential: Option<&CredentialSecret>,
            cancellation: CancellationToken,
        ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
            let Some(credential) = credential else {
                return self.fetch_endpoint(endpoint, cancellation).await;
            };
            assert_eq!(endpoint, "http://localhost:2234");
            if credential.expose() != "valid-test-key" {
                return Err(CatalogFetchError::new(
                    CatalogFailureKind::Unauthorized,
                    "key rejected",
                ));
            }
            Ok(vec![ModelDescriptor::unknown("local-model")])
        }
    }

    struct ParameterSource;
    #[async_trait]
    impl ModelCatalog for ParameterSource {
        fn provider(&self) -> ProviderDescriptor {
            ProviderDescriptor {
                id: "cloud-test".into(),
                display_name: "Cloud test".into(),
                protocols: vec![],
            }
        }
        fn supports_parameter_credentials(&self) -> bool {
            true
        }
        async fn fetch(
            &self,
            _: CancellationToken,
        ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
            panic!("active route must not be queried")
        }
        async fn fetch_parameters(
            &self,
            parameters: &BTreeMap<String, String>,
            _: CancellationToken,
        ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
            assert_eq!(
                parameters,
                &BTreeMap::from([("region".into(), "ap-southeast-2".into())])
            );
            Err(CatalogFetchError::new(
                CatalogFailureKind::Unauthorized,
                "credential required",
            ))
        }
        async fn fetch_parameters_with_credential(
            &self,
            parameters: &BTreeMap<String, String>,
            credential: Option<&CredentialSecret>,
            cancellation: CancellationToken,
        ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
            let Some(credential) = credential else {
                return self.fetch_parameters(parameters, cancellation).await;
            };
            assert_eq!(
                parameters,
                &BTreeMap::from([("region".into(), "ap-southeast-2".into())])
            );
            if credential.expose() != "valid-cloud-key" {
                return Err(CatalogFetchError::new(
                    CatalogFailureKind::Unauthorized,
                    "credential rejected",
                ));
            }
            Ok(vec![ModelDescriptor::unknown("cloud-model")])
        }
    }
    struct Prompt(&'static str);
    #[async_trait]
    impl SecretPrompt for Prompt {
        async fn prompt(
            &self,
            request: heycode_authorization_api_key::SecretPromptRequest,
            _: CancellationToken,
        ) -> Result<CredentialSecret, String> {
            assert!(request.masked);
            assert!(request.prompt.contains("http://localhost:2234"));
            Ok(CredentialSecret::new(self.0))
        }
    }

    struct CloudPrompt(&'static str);
    #[async_trait]
    impl SecretPrompt for CloudPrompt {
        async fn prompt(
            &self,
            request: heycode_authorization_api_key::SecretPromptRequest,
            _: CancellationToken,
        ) -> Result<CredentialSecret, String> {
            assert!(request.masked);
            assert!(request.prompt.contains("cloud-test"));
            Ok(CredentialSecret::new(self.0))
        }
    }
    #[tokio::test]
    async fn endpoint_key_is_committed_only_after_validation_and_preserves_previous_key() {
        for key in ["invalid-test-key", "valid-test-key"] {
            let context = heycode_core::compose(&[]).unwrap();
            let models = Arc::new(CatalogRegistry::new(std::time::Duration::from_secs(60)));
            models.register(&context, Arc::new(Source)).unwrap();
            let credentials = Arc::new(CredentialsService::new());
            let store = Arc::new(Store {
                id: CredentialProviderId::new("test-store").unwrap(),
                values: Mutex::new(BTreeMap::from([(
                    "PREVIOUS_KEY".into(),
                    "previous-secret".into(),
                )])),
            });
            credentials.register(&context, store.clone()).unwrap();
            let authorization = Arc::new(AuthorizationService::new(credentials));
            let result = authorize_endpoint(
                models.clone(),
                authorization.clone(),
                Arc::new(Prompt(key)),
                "local-test".into(),
                "http://localhost:2234".into(),
                CredentialReference::new("NEW_ENDPOINT_KEY").unwrap(),
                CancellationToken::new(),
            )
            .await;
            let values = store.values.lock().unwrap();
            assert_eq!(
                values.get("PREVIOUS_KEY").map(String::as_str),
                Some("previous-secret")
            );
            if key == "valid-test-key" {
                assert_eq!(result.unwrap().models[0].id, "local-model");
                assert_eq!(
                    values.get("NEW_ENDPOINT_KEY").map(String::as_str),
                    Some(key)
                );
            } else {
                assert!(result.is_err());
                assert!(!values.contains_key("NEW_ENDPOINT_KEY"));
            }
            assert!(models.cached("local-test").is_err());
            assert!(authorization.descriptors().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn cloud_credential_is_committed_only_after_exact_parameter_validation() {
        for key in ["invalid-cloud-key", "valid-cloud-key"] {
            let context = heycode_core::compose(&[]).unwrap();
            let models = Arc::new(CatalogRegistry::new(std::time::Duration::from_secs(60)));
            models
                .register(&context, Arc::new(ParameterSource))
                .unwrap();
            let credentials = Arc::new(CredentialsService::new());
            let store = Arc::new(Store {
                id: CredentialProviderId::new("test-store").unwrap(),
                values: Mutex::new(BTreeMap::from([(
                    "PREVIOUS_CLOUD_KEY".into(),
                    "previous-secret".into(),
                )])),
            });
            credentials.register(&context, store.clone()).unwrap();
            let authorization = Arc::new(AuthorizationService::new(credentials));
            let result = authorize_parameters(
                models.clone(),
                authorization.clone(),
                Arc::new(CloudPrompt(key)),
                "cloud-test".into(),
                BTreeMap::from([("region".into(), "ap-southeast-2".into())]),
                CredentialReference::new("NEW_CLOUD_KEY").unwrap(),
                CancellationToken::new(),
            )
            .await;
            let values = store.values.lock().unwrap();
            assert_eq!(
                values.get("PREVIOUS_CLOUD_KEY").map(String::as_str),
                Some("previous-secret")
            );
            if key == "valid-cloud-key" {
                assert_eq!(result.unwrap().models[0].id, "cloud-model");
                assert_eq!(values.get("NEW_CLOUD_KEY").map(String::as_str), Some(key));
            } else {
                assert!(result.is_err());
                assert!(!values.contains_key("NEW_CLOUD_KEY"));
            }
            assert!(models.cached("cloud-test").is_err());
            assert!(authorization.descriptors().unwrap().is_empty());
        }
    }
}
