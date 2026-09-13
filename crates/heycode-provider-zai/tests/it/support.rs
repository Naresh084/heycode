//! Deterministic credential registry used by the PZA01 visibility tests.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use heycode_core::Context;
use heycode_credentials::{
    CredentialProvider, CredentialProviderId, CredentialProviderState, CredentialQuery,
    CredentialSecret, CredentialSource, CredentialsService,
};

/// Map-backed provider that records which references were inspected and which
/// were resolved, so a test can prove a caller never asked for the secret.
pub struct RecordingCredentialProvider {
    id: CredentialProviderId,
    values: BTreeMap<String, String>,
    calls: Arc<Mutex<Calls>>,
}

#[derive(Default)]
pub struct Calls {
    pub inspected: Vec<String>,
    pub resolved: Vec<String>,
}

impl RecordingCredentialProvider {
    pub fn new(values: BTreeMap<String, String>) -> (Self, Arc<Mutex<Calls>>) {
        let calls = Arc::new(Mutex::new(Calls::default()));
        (
            Self {
                id: CredentialProviderId::new("test-recording").unwrap(),
                values,
                calls: calls.clone(),
            },
            calls,
        )
    }
}

impl CredentialProvider for RecordingCredentialProvider {
    fn id(&self) -> &CredentialProviderId {
        &self.id
    }

    fn precedence(&self) -> u16 {
        0
    }

    fn inspect(&self, query: &CredentialQuery) -> Result<CredentialProviderState, String> {
        self.calls
            .lock()
            .unwrap()
            .inspected
            .push(query.reference.as_str().to_owned());
        Ok(match self.values.get(query.reference.as_str()) {
            Some(_) => CredentialProviderState::configured(CredentialSource::Environment, false),
            None => CredentialProviderState::unconfigured(false),
        })
    }

    fn resolve(&self, query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
        self.calls
            .lock()
            .unwrap()
            .resolved
            .push(query.reference.as_str().to_owned());
        Ok(self
            .values
            .get(query.reference.as_str())
            .map(CredentialSecret::new))
    }
}

/// A credentials service holding one recording provider over `values`.
///
/// The `Context` is returned so its effects outlive the service exactly as
/// they would in a composed world.
pub fn credentials_with(
    values: &[(&str, &str)],
) -> (Context, CredentialsService, Arc<Mutex<Calls>>) {
    let values = values
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    let (provider, calls) = RecordingCredentialProvider::new(values);
    let context = Context::new();
    let credentials = CredentialsService::new();
    credentials.register(&context, Arc::new(provider)).unwrap();
    (context, credentials, calls)
}
