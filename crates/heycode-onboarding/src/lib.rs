//! Plugin-owned onboarding and connection wizard state machine.

use std::collections::BTreeMap;
use std::sync::Mutex;

use heycode_core::{Context, CoreResult, Plugin, PluginContributionKind, PluginDescriptor};

/// Onboarding state-machine service.
pub const SERVICE_ONBOARDING: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("onboarding");

/// Current generic wizard step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardingStep {
    /// Product welcome with direct connection-family choices.
    Welcome,
    /// Choose a runtime class before connector-specific flows mount.
    RuntimeClass,
    /// Choose an installed coding assistant.
    Assistant,
    /// Choose a model reported by the selected connection.
    Model,
    /// Repair a saved connection without restarting first-run setup.
    Reconnect,
    /// Choose a local or hosted model connection.
    Connection,
    /// Edit a server address before discovery.
    Endpoint,
    /// Enter provider-owned non-secret cloud coordinates before discovery.
    Parameters,
    /// Choose one connector-contributed authorization flow.
    AuthorizationMethod,
    /// Authorization committed; Continue requests a connected recomposition.
    Complete,
}

/// Runtime integration family selected by the person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeClass {
    /// Official Codex/Claude-style subscription runtime.
    Subscription,
    /// Direct API or router.
    ApiOrRouter,
    /// Managed cloud platform such as Bedrock or Vertex.
    Cloud,
    /// Local server such as LM Studio.
    Local,
}

/// One rendered selection row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardingOption {
    /// Stable row id.
    pub id: String,
    /// Primary label.
    pub label: String,
    /// Short explanatory text.
    pub description: String,
}

/// Immutable wizard view consumed by the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardingSnapshot {
    /// Whether the wizard blocks the composer.
    pub active: bool,
    /// Current generic step.
    pub step: OnboardingStep,
    /// Card title.
    pub title: &'static str,
    /// Card body.
    pub body: &'static str,
    /// Current option rows.
    pub options: Vec<OnboardingOption>,
    /// Highlighted option.
    pub selected: usize,
    /// List search text; absent on non-searchable pages.
    pub search: Option<String>,
    /// Current structured text field, when this page owns one.
    pub input: Option<OnboardingInput>,
    /// Whether `/connect` (or its `/login` alias) reopened setup inside a
    /// running session, rather than this being mandatory first-run setup.
    /// The shell presents the two differently: an in-session request is a
    /// bottom panel it can cancel, first run owns the whole viewport.
    pub from_connect: bool,
}

/// One provider-owned structured input projected by the generic wizard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardingInput {
    /// Stable non-secret routing-coordinate id.
    pub id: String,
    /// Human label shown beside the input.
    pub label: String,
    /// Provider-owned help for the expected value.
    pub description: String,
    /// Current draft value.
    pub value: String,
    /// One-based position in this form.
    pub position: usize,
    /// Total fields in this form.
    pub total: usize,
}

/// One provider-owned non-secret connection field and its initial value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardingParameter {
    /// Stable routing-coordinate id.
    pub id: String,
    /// Human label shown beside the input.
    pub label: String,
    /// Provider-owned help for the expected value.
    pub description: String,
    /// Initial value, normally restored from the effective saved connection.
    pub value: String,
}

/// Whether a structured cloud form may offer operation-scoped masked entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardingParameterCredential {
    /// The catalog resolves only provider-owned ambient or configured authority.
    Unavailable,
    /// The catalog can validate an explicitly entered masked credential.
    Masked,
}

/// Complete current structured-coordinate draft for stale-result checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardingParameterDraft {
    /// Exact connection identity.
    pub provider: String,
    /// Complete non-secret coordinate map.
    pub parameters: BTreeMap<String, String>,
}

/// Keyboard-neutral wizard action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardingAction {
    /// Select previous row.
    Previous,
    /// Select next row.
    Next,
    /// Confirm highlighted row.
    Confirm,
    /// Append a printable character to the current list search.
    Search(char),
    /// Remove the last search character.
    Backspace,
    /// Clear the current endpoint input or list search.
    ClearInput,
    /// Exit/cancel onboarding.
    Cancel,
}

/// Semantic result returned to the product shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnboardingOutcome {
    /// State changed without leaving the generic wizard.
    None,
    /// A connector family should take over.
    RuntimeClassSelected(RuntimeClass),
    /// Probe the selected coding assistant.
    AssistantSelected(String),
    /// Commit a model from the current connection catalog.
    ModelSelected(String),
    /// Discover or authorize the selected local/hosted connection.
    ConnectionSelected(String),
    /// Discover models at an explicitly entered server address.
    EndpointSelected {
        /// Exact connection identity.
        provider: String,
        /// Draft address; the provider validates it before making a request.
        endpoint: String,
        /// Whether to request a masked key for this endpoint before discovery.
        authenticate: bool,
    },
    /// Discover a cloud connection using one complete coordinate draft.
    ParametersSelected {
        /// Exact connection identity.
        provider: String,
        /// Complete non-secret coordinate map.
        parameters: BTreeMap<String, String>,
        /// Whether to request a masked credential before discovery.
        authenticate: bool,
    },
    /// A concrete authorization flow should run.
    AuthorizationFlowSelected(String),
    /// Authorization committed and the product shell must rebuild the world
    /// from durable credentials before exposing the composer.
    ReadyToRecompose,
    /// Person explicitly exited heycode (first-run Welcome only).
    Exit,
    /// Person closed a wizard opened from a running session; the session
    /// continues with its composer.
    Dismissed,
}

/// Onboarding state failures.
#[derive(Debug, thiserror::Error)]
pub enum OnboardingError {
    /// State mutex was poisoned.
    #[error("onboarding state is unavailable after a previous panic")]
    StateUnavailable,
    /// A structured form was empty or contained unsafe field metadata.
    #[error("onboarding parameter form is invalid")]
    InvalidParameters,
}

struct State {
    active: bool,
    step: OnboardingStep,
    selected: usize,
    method_options: Vec<OnboardingOption>,
    connection_options: Vec<OnboardingOption>,
    local_connections: bool,
    assistant_options: Vec<OnboardingOption>,
    model_options: Vec<OnboardingOption>,
    model_parent: OnboardingStep,
    model_allows_explicit: bool,
    search: String,
    endpoint_provider: String,
    endpoint_value: String,
    parameter_provider: String,
    parameter_fields: Vec<OnboardingParameter>,
    parameter_index: usize,
    parameter_credential: OnboardingParameterCredential,
    /// Opened with `/connect` from a running session, where Escape must close
    /// the wizard and return to the composer rather than exit the product.
    from_connect: bool,
}

/// Shared generic onboarding state machine.
pub struct OnboardingService {
    state: Mutex<State>,
}

impl OnboardingService {
    /// Build active first-run state or an inactive ready state.
    #[must_use]
    pub fn new(required: bool) -> Self {
        Self {
            state: Mutex::new(State {
                active: required,
                step: OnboardingStep::Welcome,
                selected: 0,
                method_options: Vec::new(),
                connection_options: Vec::new(),
                local_connections: false,
                assistant_options: Vec::new(),
                model_options: Vec::new(),
                model_parent: OnboardingStep::Assistant,
                model_allows_explicit: false,
                search: String::new(),
                endpoint_provider: String::new(),
                endpoint_value: String::new(),
                parameter_provider: String::new(),
                parameter_fields: Vec::new(),
                parameter_index: 0,
                parameter_credential: OnboardingParameterCredential::Unavailable,
                from_connect: false,
            }),
        }
    }

    /// Read the immutable current view.
    ///
    /// # Errors
    /// Poisoned state fails rather than inventing a wizard position.
    pub fn snapshot(&self) -> Result<OnboardingSnapshot, OnboardingError> {
        let state = self
            .state
            .lock()
            .map_err(|_| OnboardingError::StateUnavailable)?;
        Ok(snapshot(&state))
    }

    /// Read the complete current structured-coordinate draft.
    ///
    /// Product shells use this to discard a discovery result when input changed
    /// while its provider request was running.
    ///
    /// # Errors
    /// Poisoned state fails rather than returning an incomplete draft.
    pub fn parameter_draft(&self) -> Result<Option<OnboardingParameterDraft>, OnboardingError> {
        let state = self
            .state
            .lock()
            .map_err(|_| OnboardingError::StateUnavailable)?;
        if state.step != OnboardingStep::Parameters {
            return Ok(None);
        }
        Ok(Some(OnboardingParameterDraft {
            provider: state.parameter_provider.clone(),
            parameters: state
                .parameter_fields
                .iter()
                .map(|field| (field.id.clone(), field.value.clone()))
                .collect(),
        }))
    }

    /// Reopen connection setup directly at runtime-class selection.
    ///
    /// # Errors
    /// Poisoned state fails rather than publishing a partial page.
    pub fn begin_connect(&self) -> Result<(), OnboardingError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| OnboardingError::StateUnavailable)?;
        state.active = true;
        state.step = OnboardingStep::RuntimeClass;
        state.selected = 0;
        state.method_options.clear();
        state.search.clear();
        state.from_connect = true;
        Ok(())
    }

    /// Apply one keyboard-neutral action.
    ///
    /// # Errors
    /// Poisoned state fails rather than dropping the action.
    pub fn apply(&self, action: OnboardingAction) -> Result<OnboardingOutcome, OnboardingError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| OnboardingError::StateUnavailable)?;
        if !state.active {
            return Ok(OnboardingOutcome::None);
        }
        let count = options(&state).len();
        let parameter_index = state.parameter_index;
        match action {
            OnboardingAction::Search(character) => {
                if state.step == OnboardingStep::Endpoint
                    && !character.is_control()
                    && state.endpoint_value.len() + character.len_utf8() <= 2048
                {
                    state.endpoint_value.push(character);
                }
                if state.step == OnboardingStep::Parameters
                    && !character.is_control()
                    && let Some(field) = state.parameter_fields.get_mut(parameter_index)
                    && field.value.len() + character.len_utf8() <= 256
                {
                    field.value.push(character);
                }
                if searchable(state.step)
                    && !character.is_control()
                    && state.search.chars().count() < 256
                {
                    state.search.push(character);
                    state.selected = 0;
                }
                Ok(OnboardingOutcome::None)
            }
            OnboardingAction::ClearInput => {
                if state.step == OnboardingStep::Endpoint {
                    state.endpoint_value.clear();
                }
                if state.step == OnboardingStep::Parameters
                    && let Some(field) = state.parameter_fields.get_mut(parameter_index)
                {
                    field.value.clear();
                }
                state.search.clear();
                state.selected = 0;
                Ok(OnboardingOutcome::None)
            }
            OnboardingAction::Backspace => {
                if state.step == OnboardingStep::Endpoint {
                    state.endpoint_value.pop();
                }
                if state.step == OnboardingStep::Parameters
                    && let Some(field) = state.parameter_fields.get_mut(parameter_index)
                {
                    field.value.pop();
                }
                state.search.pop();
                state.selected = 0;
                Ok(OnboardingOutcome::None)
            }
            OnboardingAction::Previous => {
                state.selected = state
                    .selected
                    .checked_sub(1)
                    .unwrap_or(count.saturating_sub(1));
                Ok(OnboardingOutcome::None)
            }
            OnboardingAction::Next => {
                state.selected = (state.selected + 1) % count.max(1);
                Ok(OnboardingOutcome::None)
            }
            // Escape means "back", and at the first page "close" — never "quit
            // heycode" unless there is nothing to go back to: the first-run
            // Welcome, where leaving is the only alternative to setting up.
            OnboardingAction::Cancel => {
                state.search.clear();
                match state.step {
                    OnboardingStep::Reconnect if state.from_connect => {
                        state.active = false;
                        Ok(OnboardingOutcome::Dismissed)
                    }
                    OnboardingStep::Welcome | OnboardingStep::Reconnect => {
                        state.active = false;
                        Ok(OnboardingOutcome::Exit)
                    }
                    OnboardingStep::RuntimeClass | OnboardingStep::Complete
                        if state.from_connect =>
                    {
                        state.active = false;
                        Ok(OnboardingOutcome::Dismissed)
                    }
                    OnboardingStep::RuntimeClass | OnboardingStep::Complete => {
                        state.step = OnboardingStep::Welcome;
                        state.selected = 0;
                        Ok(OnboardingOutcome::None)
                    }
                    OnboardingStep::Endpoint => {
                        state.step = OnboardingStep::Connection;
                        state.selected = 0;
                        Ok(OnboardingOutcome::None)
                    }
                    OnboardingStep::Parameters if state.parameter_index > 0 => {
                        state.parameter_index -= 1;
                        state.selected = 0;
                        Ok(OnboardingOutcome::None)
                    }
                    OnboardingStep::Parameters => {
                        state.step = OnboardingStep::Connection;
                        state.selected = 0;
                        Ok(OnboardingOutcome::None)
                    }
                    OnboardingStep::Model => {
                        state.step = state.model_parent;
                        state.selected = 0;
                        state.model_options.clear();
                        Ok(OnboardingOutcome::None)
                    }
                    OnboardingStep::Assistant | OnboardingStep::Connection => {
                        state.step = if state.from_connect {
                            OnboardingStep::RuntimeClass
                        } else {
                            OnboardingStep::Welcome
                        };
                        state.selected = 0;
                        Ok(OnboardingOutcome::None)
                    }
                    OnboardingStep::AuthorizationMethod => {
                        state.step = OnboardingStep::RuntimeClass;
                        state.selected = 0;
                        state.method_options.clear();
                        Ok(OnboardingOutcome::None)
                    }
                }
            }
            OnboardingAction::Confirm => confirm(&mut state),
        }
    }

    /// Repair a saved connection, keeping its provider identity visible.
    ///
    /// # Errors
    /// Poisoned state fails before publication.
    pub fn begin_reconnect(&self, connection: OnboardingOption) -> Result<(), OnboardingError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| OnboardingError::StateUnavailable)?;
        state.active = true;
        state.step = OnboardingStep::Reconnect;
        state.selected = 0;
        state.search.clear();
        state.connection_options = vec![
            connection,
            OnboardingOption {
                id: "change-connection".into(),
                label: "Choose another connection".into(),
                description: "Use a different account, provider or local server".into(),
            },
        ];
        Ok(())
    }

    /// Show local or hosted connection choices supplied by provider plugins.
    ///
    /// # Errors
    /// Poisoned state fails before publication.
    pub fn show_connections(
        &self,
        local: bool,
        options: Vec<OnboardingOption>,
    ) -> Result<(), OnboardingError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| OnboardingError::StateUnavailable)?;
        state.connection_options = options;
        state.local_connections = local;
        state.step = OnboardingStep::Connection;
        state.selected = 0;
        state.search.clear();
        Ok(())
    }

    /// Edit a draft server address before read-only discovery.
    ///
    /// # Errors
    /// Poisoned state fails before publication.
    pub fn show_endpoint(&self, provider: &str, endpoint: &str) -> Result<(), OnboardingError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| OnboardingError::StateUnavailable)?;
        state.endpoint_provider = provider.into();
        state.endpoint_value = endpoint.into();
        state.step = OnboardingStep::Endpoint;
        state.selected = 0;
        state.search.clear();
        Ok(())
    }

    /// Enter provider-owned non-secret coordinates before live discovery.
    ///
    /// # Errors
    /// Poisoned state or an empty/structurally invalid form fails before publication.
    pub fn show_parameters(
        &self,
        provider: &str,
        credential: OnboardingParameterCredential,
        fields: Vec<OnboardingParameter>,
    ) -> Result<(), OnboardingError> {
        if fields.is_empty()
            || fields.iter().any(|field| {
                field.id.is_empty()
                    || field.label.is_empty()
                    || field.id.len() > 64
                    || field.value.len() > 256
                    || field.id.chars().any(char::is_control)
                    || field.value.chars().any(char::is_control)
            })
        {
            return Err(OnboardingError::InvalidParameters);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| OnboardingError::StateUnavailable)?;
        state.parameter_provider = provider.into();
        state.parameter_fields = fields;
        state.parameter_index = 0;
        state.parameter_credential = credential;
        state.step = OnboardingStep::Parameters;
        state.selected = 0;
        state.search.clear();
        Ok(())
    }

    /// Show installed assistant choices supplied by runtime plugins.
    ///
    /// # Errors
    /// Poisoned state fails before publication.
    pub fn show_assistants(&self, options: Vec<OnboardingOption>) -> Result<(), OnboardingError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| OnboardingError::StateUnavailable)?;
        state.assistant_options = options;
        state.step = OnboardingStep::Assistant;
        state.selected = 0;
        state.search.clear();
        Ok(())
    }

    /// Show the selected connection's live model catalog.
    ///
    /// # Errors
    /// Poisoned state fails before publication.
    pub fn show_models(&self, options: Vec<OnboardingOption>) -> Result<(), OnboardingError> {
        self.show_models_inner(options, false)
    }

    /// Show discovered models while also permitting one exact model id absent
    /// from the catalog.
    ///
    /// The explicit row appears only when the typed text matches no discovered
    /// row, so catalog identities retain precedence over free-form input.
    ///
    /// # Errors
    /// Poisoned state fails before publication.
    pub fn show_models_with_explicit(
        &self,
        options: Vec<OnboardingOption>,
    ) -> Result<(), OnboardingError> {
        self.show_models_inner(options, true)
    }

    fn show_models_inner(
        &self,
        options: Vec<OnboardingOption>,
        model_allows_explicit: bool,
    ) -> Result<(), OnboardingError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| OnboardingError::StateUnavailable)?;
        if state.step != OnboardingStep::Model {
            state.model_parent = state.step;
        }
        state.model_options = options;
        state.model_allows_explicit = model_allows_explicit;
        state.step = OnboardingStep::Model;
        state.selected = 0;
        state.search.clear();
        Ok(())
    }

    /// Show connector-contributed authorization choices.
    ///
    /// An empty catalog renders one Back row rather than a dead-end card.
    ///
    /// # Errors
    /// Poisoned state fails rather than publishing a partial page.
    pub fn show_authorization_methods(
        &self,
        mut options: Vec<OnboardingOption>,
    ) -> Result<(), OnboardingError> {
        if options.is_empty() {
            options.push(OnboardingOption {
                id: "back".to_owned(),
                label: "No compatible method installed".to_owned(),
                description: "Go back and choose another runtime class".to_owned(),
            });
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| OnboardingError::StateUnavailable)?;
        state.method_options = options;
        state.step = OnboardingStep::AuthorizationMethod;
        state.selected = 0;
        state.search.clear();
        Ok(())
    }

    /// Publish the committed completion page. The composer remains blocked
    /// until the product shell recomposes a credential-backed world.
    ///
    /// # Errors
    /// Poisoned state fails loud.
    pub fn complete(&self) -> Result<(), OnboardingError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| OnboardingError::StateUnavailable)?;
        state.active = true;
        state.step = OnboardingStep::Complete;
        state.selected = 0;
        state.search.clear();
        Ok(())
    }
}

fn confirm(state: &mut State) -> Result<OnboardingOutcome, OnboardingError> {
    match state.step {
        OnboardingStep::Welcome | OnboardingStep::RuntimeClass => {
            let selected = match state.selected {
                0 => RuntimeClass::Subscription,
                1 => RuntimeClass::Local,
                _ => RuntimeClass::ApiOrRouter,
            };
            Ok(OnboardingOutcome::RuntimeClassSelected(selected))
        }
        OnboardingStep::Endpoint => Ok(OnboardingOutcome::EndpointSelected {
            provider: state.endpoint_provider.clone(),
            endpoint: state.endpoint_value.clone(),
            authenticate: state.selected == 1,
        }),
        OnboardingStep::Parameters => {
            let Some(field) = state.parameter_fields.get(state.parameter_index) else {
                return Ok(OnboardingOutcome::None);
            };
            if field.value.is_empty() || field.value.trim() != field.value {
                return Ok(OnboardingOutcome::None);
            }
            if state.parameter_index + 1 < state.parameter_fields.len() {
                state.parameter_index += 1;
                state.selected = 0;
                return Ok(OnboardingOutcome::None);
            }
            let parameters = state
                .parameter_fields
                .iter()
                .map(|field| (field.id.clone(), field.value.clone()))
                .collect();
            Ok(OnboardingOutcome::ParametersSelected {
                provider: state.parameter_provider.clone(),
                parameters,
                authenticate: state.selected == 1,
            })
        }
        OnboardingStep::Assistant => Ok(options(state)
            .get(state.selected)
            .map_or(OnboardingOutcome::None, |row| {
                OnboardingOutcome::AssistantSelected(row.id.clone())
            })),
        OnboardingStep::Model => Ok(options(state)
            .get(state.selected)
            .map_or(OnboardingOutcome::None, |row| {
                OnboardingOutcome::ModelSelected(row.id.clone())
            })),
        OnboardingStep::Reconnect if state.selected > 0 => {
            state.step = OnboardingStep::RuntimeClass;
            state.selected = 0;
            Ok(OnboardingOutcome::None)
        }
        OnboardingStep::Reconnect | OnboardingStep::Connection => Ok(options(state)
            .get(state.selected)
            .map_or(OnboardingOutcome::None, |row| {
                OnboardingOutcome::ConnectionSelected(row.id.clone())
            })),
        OnboardingStep::AuthorizationMethod => {
            let filtered = options(state);
            let Some(option) = filtered.get(state.selected) else {
                return Ok(OnboardingOutcome::None);
            };
            if option.id == "back" {
                state.step = OnboardingStep::RuntimeClass;
                state.selected = 0;
                return Ok(OnboardingOutcome::None);
            }
            Ok(OnboardingOutcome::AuthorizationFlowSelected(
                option.id.clone(),
            ))
        }
        OnboardingStep::Complete => {
            state.active = false;
            Ok(OnboardingOutcome::ReadyToRecompose)
        }
    }
}

fn snapshot(state: &State) -> OnboardingSnapshot {
    let (title, body) = match state.step {
        OnboardingStep::Welcome => ("Welcome to heycode", "Choose how you'd like to connect."),
        OnboardingStep::RuntimeClass => {
            ("Choose a connection", "Choose how you'd like to connect.")
        }
        OnboardingStep::Assistant => (
            "Choose a subscription",
            "Use an account signed in through its official app.",
        ),
        OnboardingStep::Model if state.model_allows_explicit => (
            "Choose a model",
            "Choose a discovered model, or type an exact model ID.",
        ),
        OnboardingStep::Model => (
            "Choose a model",
            "Search by name or ID. Newest first when dates are available.",
        ),
        OnboardingStep::Reconnect => (
            "Reconnect your account",
            "Your saved connection needs attention.",
        ),
        OnboardingStep::Endpoint => (
            "Connect a local server",
            "Enter the server address. Ctrl+U clears the field.",
        ),
        OnboardingStep::Parameters => (
            "Configure the cloud connection",
            "Enter each required non-secret coordinate. Ctrl+U clears the field.",
        ),
        OnboardingStep::Connection if state.local_connections => (
            "Choose a local model server",
            "Select a running server to see its models.",
        ),
        OnboardingStep::Connection => (
            "Select a provider",
            "Choose the service you'd like to connect.",
        ),
        OnboardingStep::AuthorizationMethod => (
            "Connect your account",
            "Choose an available sign-in method.",
        ),
        OnboardingStep::Complete => ("Connected", "Your connection is ready."),
    };
    OnboardingSnapshot {
        active: state.active,
        step: state.step,
        title,
        body,
        options: options(state),
        selected: state.selected,
        search: if state.step == OnboardingStep::Endpoint {
            Some(state.endpoint_value.clone())
        } else if state.step == OnboardingStep::Parameters {
            state
                .parameter_fields
                .get(state.parameter_index)
                .map(|field| field.value.clone())
        } else {
            searchable(state.step).then(|| state.search.clone())
        },
        from_connect: state.from_connect,
        input: (state.step == OnboardingStep::Parameters)
            .then(|| {
                state
                    .parameter_fields
                    .get(state.parameter_index)
                    .map(|field| OnboardingInput {
                        id: field.id.clone(),
                        label: field.label.clone(),
                        description: field.description.clone(),
                        value: field.value.clone(),
                        position: state.parameter_index + 1,
                        total: state.parameter_fields.len(),
                    })
            })
            .flatten(),
    }
}

fn searchable(step: OnboardingStep) -> bool {
    matches!(
        step,
        OnboardingStep::Assistant
            | OnboardingStep::Model
            | OnboardingStep::AuthorizationMethod
            | OnboardingStep::Connection
    )
}

fn options(state: &State) -> Vec<OnboardingOption> {
    let rows = unfiltered_options(state);
    if !searchable(state.step) || state.search.is_empty() {
        return rows;
    }
    let query = state.search.to_lowercase();
    let filtered = rows
        .into_iter()
        .filter(|row| {
            query.split_whitespace().all(|term| {
                [&row.id, &row.label, &row.description]
                    .iter()
                    .any(|text| text.to_lowercase().contains(term))
            })
        })
        .collect::<Vec<_>>();
    if state.step == OnboardingStep::Model
        && state.model_allows_explicit
        && filtered.is_empty()
        && valid_explicit_model(&state.search)
    {
        return vec![OnboardingOption {
            id: state.search.clone(),
            label: format!("Use model ID {}", state.search),
            description: "Exact model ID was not discovered by this server".to_owned(),
        }];
    }
    filtered
}

fn valid_explicit_model(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn unfiltered_options(state: &State) -> Vec<OnboardingOption> {
    match state.step {
        OnboardingStep::Welcome | OnboardingStep::RuntimeClass => vec![
            OnboardingOption {
                id: "subscription".to_owned(),
                label: "Use a subscription".to_owned(),
                description: "Connect your ChatGPT, Claude or Grok account".to_owned(),
            },
            OnboardingOption {
                id: "local".to_owned(),
                label: "Use a local model".to_owned(),
                description: "Connect to LM Studio, Ollama or a custom server".to_owned(),
            },
            OnboardingOption {
                id: "provider".to_owned(),
                label: "Select a provider".to_owned(),
                description: "Connect to your API provider".to_owned(),
            },
        ],
        OnboardingStep::Endpoint => vec![
            OnboardingOption {
                id: "discover".into(),
                label: "Find models".into(),
                description: "Check this server before saving the connection".into(),
            },
            OnboardingOption {
                id: "authenticate".into(),
                label: "Use an API key".into(),
                description: "Enter a masked key for this server and find models".into(),
            },
        ],
        OnboardingStep::Parameters => {
            if state.parameter_index + 1 < state.parameter_fields.len() {
                vec![OnboardingOption {
                    id: "continue".into(),
                    label: "Continue".into(),
                    description: "Enter the next required coordinate".into(),
                }]
            } else {
                let mut options = vec![OnboardingOption {
                    id: "discover".into(),
                    label: "Find models".into(),
                    description: "Check these coordinates before saving the connection".into(),
                }];
                if state.parameter_credential == OnboardingParameterCredential::Masked {
                    options.push(OnboardingOption {
                        id: "authenticate".into(),
                        label: "Use a credential".into(),
                        description: "Enter a masked credential and find models".into(),
                    });
                }
                options
            }
        }
        OnboardingStep::Assistant => state.assistant_options.clone(),
        OnboardingStep::Reconnect | OnboardingStep::Connection => state.connection_options.clone(),
        OnboardingStep::Model => state.model_options.clone(),
        OnboardingStep::AuthorizationMethod => state.method_options.clone(),
        OnboardingStep::Complete => vec![OnboardingOption {
            id: "continue".to_owned(),
            label: "Continue to heycode".to_owned(),
            description: "Start your first task".to_owned(),
        }],
    }
}

/// Mount the onboarding state service.
#[must_use]
pub fn onboarding_plugin(required: bool) -> Box<dyn Plugin> {
    onboarding_plugin_with_reconnect(required, None)
}

/// Mount first-run setup or targeted repair for an existing saved connection.
#[must_use]
pub fn onboarding_plugin_with_reconnect(
    required: bool,
    reconnect: Option<OnboardingOption>,
) -> Box<dyn Plugin> {
    struct OnboardingPlugin(bool, Option<OnboardingOption>);
    impl Plugin for OnboardingPlugin {
        fn name(&self) -> &'static str {
            "onboarding"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                "onboarding",
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_ONBOARDING]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let service = OnboardingService::new(self.0);
            if self.0
                && let Some(connection) = self.1.as_ref()
            {
                service
                    .begin_reconnect(connection.clone())
                    .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            }
            context.provide(SERVICE_ONBOARDING, "onboarding", service)
        }
    }
    Box::new(OnboardingPlugin(required, reconnect))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod cancel_tests {
    use super::*;

    /// Escape walks back and closes; it exits heycode only from the first-run
    /// Welcome, where there is nothing to go back to.
    #[test]
    fn cancel_goes_back_closes_a_connect_wizard_and_exits_only_from_welcome() {
        let first_run = OnboardingService::new(true);
        assert_eq!(
            first_run.apply(OnboardingAction::Confirm).unwrap(),
            OnboardingOutcome::RuntimeClassSelected(RuntimeClass::Subscription)
        );
        first_run.show_authorization_methods(Vec::new()).unwrap();
        assert_eq!(
            first_run.apply(OnboardingAction::Cancel).unwrap(),
            OnboardingOutcome::None
        );
        assert_eq!(
            first_run.snapshot().unwrap().step,
            OnboardingStep::RuntimeClass
        );
        first_run.apply(OnboardingAction::Cancel).unwrap();
        assert_eq!(first_run.snapshot().unwrap().step, OnboardingStep::Welcome);
        assert_eq!(
            first_run.apply(OnboardingAction::Cancel).unwrap(),
            OnboardingOutcome::Exit
        );

        let connect = OnboardingService::new(false);
        connect.begin_connect().unwrap();
        assert_eq!(
            connect.snapshot().unwrap().step,
            OnboardingStep::RuntimeClass
        );
        assert_eq!(
            connect.apply(OnboardingAction::Cancel).unwrap(),
            OnboardingOutcome::Dismissed,
            "closing a /connect wizard returns to the session, never quits"
        );
        assert!(!connect.snapshot().unwrap().active);
    }
}
