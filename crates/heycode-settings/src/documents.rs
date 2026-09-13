//! Detached user and project document layers.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::redaction::{FieldRoles, redact_for_debug};
use crate::{SettingsError, SettingsLayer, SettingsNamespace};

/// Provider-supplied raw namespace sections for S01's read-only resolution.
#[derive(Clone, Default)]
pub struct SettingsDocuments {
    user: BTreeMap<SettingsNamespace, Value>,
    project: BTreeMap<SettingsNamespace, Value>,
    managed: BTreeMap<SettingsNamespace, Value>,
}

impl std::fmt::Debug for SettingsDocuments {
    /// Provider documents exist before any schema, so the rendered form uses
    /// the always-on screen with no owner roles available.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let roles = FieldRoles::default();
        let render = |sections: &BTreeMap<SettingsNamespace, Value>| {
            sections
                .iter()
                .map(|(namespace, section)| (namespace.clone(), redact_for_debug(section, &roles)))
                .collect::<BTreeMap<_, _>>()
        };
        formatter
            .debug_struct("SettingsDocuments")
            .field("user", &render(&self.user))
            .field("project", &render(&self.project))
            .field("managed", &render(&self.managed))
            .finish()
    }
}

impl SettingsDocuments {
    /// Empty documents.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a detached user-wide namespace section.
    ///
    /// # Errors
    /// Sections must be JSON objects.
    pub fn set_user(
        &mut self,
        namespace: SettingsNamespace,
        section: Value,
    ) -> Result<(), SettingsError> {
        Self::insert(&mut self.user, namespace, section, SettingsLayer::User)
    }

    /// Set a detached trusted-project namespace section.
    ///
    /// # Errors
    /// Sections must be JSON objects.
    pub fn set_project(
        &mut self,
        namespace: SettingsNamespace,
        section: Value,
    ) -> Result<(), SettingsError> {
        Self::insert(
            &mut self.project,
            namespace,
            section,
            SettingsLayer::Project,
        )
    }

    /// Set a detached administrator-managed namespace section.
    ///
    /// # Errors
    /// Sections must be JSON objects.
    pub fn set_managed(
        &mut self,
        namespace: SettingsNamespace,
        section: Value,
    ) -> Result<(), SettingsError> {
        Self::insert(
            &mut self.managed,
            namespace,
            section,
            SettingsLayer::Managed,
        )
    }

    pub(crate) fn managed(&self, namespace: &SettingsNamespace) -> Option<&Value> {
        self.managed.get(namespace)
    }

    pub(crate) fn user(&self, namespace: &SettingsNamespace) -> Option<&Value> {
        self.user.get(namespace)
    }

    pub(crate) fn project(&self, namespace: &SettingsNamespace) -> Option<&Value> {
        self.project.get(namespace)
    }

    /// Borrow one detached raw user section for a trusted composition owner.
    #[must_use]
    pub fn user_section(&self, namespace: &SettingsNamespace) -> Option<&Value> {
        self.user.get(namespace)
    }

    /// Borrow one detached raw project section for a trusted composition owner.
    #[must_use]
    pub fn project_section(&self, namespace: &SettingsNamespace) -> Option<&Value> {
        self.project.get(namespace)
    }

    /// Borrow one detached raw managed section for a trusted composition owner.
    #[must_use]
    pub fn managed_section(&self, namespace: &SettingsNamespace) -> Option<&Value> {
        self.managed.get(namespace)
    }

    fn insert(
        target: &mut BTreeMap<SettingsNamespace, Value>,
        namespace: SettingsNamespace,
        section: Value,
        layer: SettingsLayer,
    ) -> Result<(), SettingsError> {
        if !section.is_object() {
            return Err(SettingsError::LayerMustBeObject {
                namespace: namespace.to_string(),
                layer,
            });
        }
        target.insert(namespace, section);
        Ok(())
    }
}
