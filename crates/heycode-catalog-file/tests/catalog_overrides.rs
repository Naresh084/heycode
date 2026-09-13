//! User catalog overrides: visibility and the impossibility of an unlabelled
//! assertion.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_catalog_file::{
    AssertionDirection, AttributedLimit, AttributedSupport, CapabilityEnforcement,
    CatalogOverrideLayer, CatalogOverrides, CatalogOverridesConfig, FileCatalogConfig,
    FileCatalogPersistence, LimitEnforcement, ModelAssertion, ModelCapabilityKind, ModelLimitField,
    OVERRIDE_SCHEMA_VERSION, SERVICE_CATALOG_OVERRIDES, catalog_overrides_plugin,
};
use heycode_llm::{
    CapabilitySupport, CatalogPersistence, CatalogSnapshot, ModelCapabilities, ModelDescriptor,
    ModelLifecycle, ModelPerformance, ModelPricing, ProviderDescriptor, ProviderProtocol,
};
use tokio_util::sync::CancellationToken;

const PROVIDER: &str = "openrouter";
const MODEL: &str = "stealth/ox-alpha";
const FETCHED_AT_MS: u64 = 1_787_752_741_000;

fn capabilities(tools: CapabilitySupport, native_web: CapabilitySupport) -> ModelCapabilities {
    ModelCapabilities {
        tools,
        reasoning: CapabilitySupport::Supported,
        image_input: CapabilitySupport::Unsupported,
        document_input: CapabilitySupport::Unknown,
        structured_output: CapabilitySupport::Unknown,
        native_web,
        native_compaction: CapabilitySupport::Unknown,
        prompt_cache: CapabilitySupport::Unknown,
    }
}

fn model(tools: CapabilitySupport, native_web: CapabilitySupport) -> ModelDescriptor {
    ModelDescriptor {
        id: MODEL.to_owned(),
        display_name: "Ox Alpha".to_owned(),
        aliases: vec!["stealth/ox".to_owned()],
        created_at_ms: None,
        context_window: Some(131_072),
        max_output_tokens: None,
        lifecycle: ModelLifecycle::preview(),
        capabilities: capabilities(tools, native_web),
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }
}

fn snapshot_with(models: Vec<ModelDescriptor>, revision: u64) -> CatalogSnapshot {
    CatalogSnapshot {
        provider: ProviderDescriptor {
            id: PROVIDER.to_owned(),
            display_name: "OpenRouter".to_owned(),
            protocols: vec![ProviderProtocol::OpenAiChatCompletions],
        },
        models,
        revision,
        fetched_at_ms: FETCHED_AT_MS,
    }
}

fn snapshot(tools: CapabilitySupport, native_web: CapabilitySupport) -> CatalogSnapshot {
    snapshot_with(vec![model(tools, native_web)], 7)
}

/// Write one override layer and load it under the label `user`.
fn load_layer(dir: &std::path::Path, body: &str) -> CatalogOverrides {
    let path = dir.join("catalog-overrides.toml");
    std::fs::write(&path, body).unwrap();
    CatalogOverrides::load(&CatalogOverridesConfig::new(vec![
        CatalogOverrideLayer::new("user", &path),
    ]))
    .unwrap()
}

fn load_error(dir: &std::path::Path, body: &str) -> String {
    let path = dir.join("catalog-overrides.toml");
    std::fs::write(&path, body).unwrap();
    CatalogOverrides::load(&CatalogOverridesConfig::new(vec![
        CatalogOverrideLayer::new("user", &path),
    ]))
    .expect_err("an invalid override document must fail loud")
    .to_string()
}

fn asserting_tools_supported() -> String {
    format!(
        "schema_version = {OVERRIDE_SCHEMA_VERSION}\n\
         [[model]]\n\
         provider = \"{PROVIDER}\"\n\
         model = \"{MODEL}\"\n\
         [model.capabilities]\n\
         tools = \"supported\"\n"
    )
}

/// This is the row's security requirement, stated as a test: a value a user
/// asserted and a value a vendor published must never produce the same
/// rendering. If these two strings can be made equal, a surface has laundered
/// a guess into a fact.
#[test]
fn a_user_assertion_and_provider_evidence_never_render_the_same_string() {
    let dir = tempfile::tempdir().unwrap();
    let overrides = load_layer(dir.path(), &asserting_tools_supported());

    // The provider itself evidences tools as Supported...
    let evidenced = CatalogOverrides::empty().attribute(&snapshot(
        CapabilitySupport::Supported,
        CapabilitySupport::Unknown,
    ));
    let evidenced_render = evidenced
        .model(MODEL)
        .unwrap()
        .capability(ModelCapabilityKind::Tools)
        .render();

    // ...and a user asserts the identical tri-state over an unknown.
    let asserted = overrides.attribute(&snapshot(
        CapabilitySupport::Unknown,
        CapabilitySupport::Unknown,
    ));
    let asserted_render = asserted
        .model(MODEL)
        .unwrap()
        .capability(ModelCapabilityKind::Tools)
        .render();

    assert_ne!(evidenced_render, asserted_render);
    assert!(
        !evidenced_render.contains("user-"),
        "provider evidence must not be labelled as a user claim: {evidenced_render}"
    );
    assert!(
        asserted_render.contains("user-asserted"),
        "a user assertion must name its author: {asserted_render}"
    );
    assert!(
        asserted_render.contains("provider catalog: unknown"),
        "a user assertion must carry the evidence it stands against: {asserted_render}"
    );
    assert!(
        asserted_render.contains("catalog-overrides.toml"),
        "a user assertion must name the document it came from: {asserted_render}"
    );

    // Even when both are Supported, the attributed values are different
    // variants, so no consumer can compare them as equal tri-states by
    // accident.
    let both_supported = overrides.attribute(&snapshot(
        CapabilitySupport::Supported,
        CapabilitySupport::Unknown,
    ));
    let redundant = both_supported
        .model(MODEL)
        .unwrap()
        .capability(ModelCapabilityKind::Tools);
    assert!(matches!(redundant, AttributedSupport::UserAsserted(_)));
    assert_ne!(redundant.render(), evidenced_render);
}

/// Unknown never becomes Supported by inference. When a user asserts it
/// anyway, the assertion is classified as claiming what nothing evidenced, and
/// the untouched provider tri-state stays readable beside it.
#[test]
fn asserting_support_over_an_unknown_is_labelled_as_claiming_unevidenced_support() {
    let dir = tempfile::tempdir().unwrap();
    let overrides = load_layer(dir.path(), &asserting_tools_supported());
    let attributed = overrides.attribute(&snapshot(
        CapabilitySupport::Unknown,
        CapabilitySupport::Unknown,
    ));
    let row = attributed.model(MODEL).unwrap();
    let tools = row.capability(ModelCapabilityKind::Tools);

    let assertion = tools.assertion().expect("tools carries a user assertion");
    assert_eq!(assertion.asserted(), CapabilitySupport::Supported);
    assert_eq!(assertion.provider_evidence(), CapabilitySupport::Unknown);
    assert_eq!(
        assertion.direction(),
        AssertionDirection::ClaimsUnevidencedSupport
    );
    assert!(assertion.direction().permits_unevidenced_request());
    assert_eq!(tools.provider_evidence(), CapabilitySupport::Unknown);
    assert_eq!(assertion.source().layer(), "user");
    assert_eq!(assertion.provider_revision(), 7);
    assert_eq!(assertion.provider_fetched_at_ms(), FETCHED_AT_MS);
    assert_eq!(tools.enforced(), CapabilitySupport::Unknown);
    assert!(matches!(
        tools.enforcement(),
        CapabilityEnforcement::ProviderEvidence(evidence)
            if evidence.support() == CapabilitySupport::Unknown
                && evidence.revision() == 7
                && evidence.fetched_at_ms() == FETCHED_AT_MS
    ));
}

/// An assertion the provider's own catalog explicitly disproves is a distinct,
/// louder class than one it merely never spoke to.
#[test]
fn asserting_support_the_provider_denies_is_labelled_as_contradicting_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let overrides = load_layer(dir.path(), &asserting_tools_supported());
    let attributed = overrides.attribute(&snapshot(
        CapabilitySupport::Unsupported,
        CapabilitySupport::Unknown,
    ));
    let row = attributed.model(MODEL).unwrap();
    let assertion = row
        .capability(ModelCapabilityKind::Tools)
        .assertion()
        .unwrap();

    assert_eq!(
        assertion.direction(),
        AssertionDirection::ContradictsEvidence
    );
    assert!(assertion.direction().label().contains("CONTRADICTS"));
    assert_ne!(
        AssertionDirection::ContradictsEvidence.label(),
        AssertionDirection::ClaimsUnevidencedSupport.label(),
        "the two claiming directions must be distinguishable to a reader"
    );
    assert_eq!(attributed.contradictions().len(), 1);
    assert_eq!(attributed.contradictions()[0].0, MODEL);
    assert_eq!(
        row.enforced_capability(ModelCapabilityKind::Tools),
        CapabilitySupport::Unsupported,
        "a contradiction remains visible but cannot authorize a request"
    );
}

/// The safe direction is allowed, and it is the only direction that can never
/// permit a request the catalog did not already permit.
#[test]
fn an_override_may_remove_capability_and_that_direction_permits_nothing_new() {
    let dir = tempfile::tempdir().unwrap();
    let body = format!(
        "schema_version = {OVERRIDE_SCHEMA_VERSION}\n\
         [[model]]\n\
         provider = \"{PROVIDER}\"\n\
         model = \"{MODEL}\"\n\
         [model.capabilities]\n\
         tools = \"unsupported\"\n\
         reasoning = \"unknown\"\n"
    );
    let overrides = load_layer(dir.path(), &body);
    let attributed = overrides.attribute(&snapshot(
        CapabilitySupport::Supported,
        CapabilitySupport::Unknown,
    ));
    let row = attributed.model(MODEL).unwrap();

    for kind in [ModelCapabilityKind::Tools, ModelCapabilityKind::Reasoning] {
        let assertion = row.capability(kind).assertion().unwrap();
        assert_eq!(assertion.direction(), AssertionDirection::Narrowing);
        assert!(!assertion.direction().permits_unevidenced_request());
    }
    assert_eq!(
        row.enforced_capability(ModelCapabilityKind::Tools),
        CapabilitySupport::Unsupported
    );
    assert!(matches!(
        row.capability(ModelCapabilityKind::Tools).enforcement(),
        CapabilityEnforcement::UserConstraint(assertion)
            if assertion.source().layer() == "user"
                && assertion.provider_revision() == 7
    ));
    // The provider's own evidence is never destroyed by a narrowing override.
    assert_eq!(
        row.capability(ModelCapabilityKind::Tools)
            .provider_evidence(),
        CapabilitySupport::Supported
    );
    assert_eq!(
        row.evidence().capabilities.tools,
        CapabilitySupport::Supported
    );
}

/// A document says what a user asserts. It cannot say who asserted it: there
/// is no provenance key, and `deny_unknown_fields` makes writing one a loud
/// failure rather than a silently ignored line.
#[test]
fn an_override_document_cannot_spell_its_own_provenance() {
    let dir = tempfile::tempdir().unwrap();
    for body in [
        format!(
            "schema_version = {OVERRIDE_SCHEMA_VERSION}\n\
             [[model]]\n\
             provider = \"{PROVIDER}\"\n\
             model = \"{MODEL}\"\n\
             provenance = \"provider_evidenced\"\n\
             [model.capabilities]\n\
             tools = \"supported\"\n"
        ),
        format!(
            "schema_version = {OVERRIDE_SCHEMA_VERSION}\n\
             [[model]]\n\
             provider = \"{PROVIDER}\"\n\
             model = \"{MODEL}\"\n\
             [model.capabilities]\n\
             tools = \"supported\"\n\
             source = \"provider\"\n"
        ),
        format!(
            "schema_version = {OVERRIDE_SCHEMA_VERSION}\n\
             revision = 9\n\
             fetched_at_ms = 1\n\
             captured_at_ms = 1\n\
             precedence = 99\n\
             [[model]]\n\
             provider = \"{PROVIDER}\"\n\
             model = \"{MODEL}\"\n\
             [model.capabilities]\n\
             tools = \"supported\"\n"
        ),
    ] {
        let rendered = load_error(dir.path(), &body);
        assert!(
            rendered.contains("invalid catalog overrides"),
            "a forged provenance key must fail the document: {rendered}"
        );
    }
}

/// The durable cache holds fetched evidence and nothing else. Attribution is a
/// projection over a borrowed generation, so an assertion has no route into
/// `models.json` and therefore cannot be read back on the next start as though
/// a provider had published it.
#[test]
fn a_user_assertion_never_reaches_the_durable_catalog_cache() {
    let dir = tempfile::tempdir().unwrap();
    let overrides = load_layer(dir.path(), &asserting_tools_supported());
    let fetched = snapshot(CapabilitySupport::Unknown, CapabilitySupport::Unknown);

    let attributed = overrides.attribute(&fetched);
    assert_eq!(
        attributed
            .model(MODEL)
            .unwrap()
            .enforced_capability(ModelCapabilityKind::Tools),
        CapabilitySupport::Unknown,
        "an assertion is visible but cannot promote missing provider evidence"
    );

    // Attribution did not touch the generation the registry persists.
    assert_eq!(
        fetched.models[0].capabilities.tools,
        CapabilitySupport::Unknown
    );

    // The only descriptor an attributed row can hand back is the provider's
    // own. Persisting it and reading it again is the exact round-trip an
    // assertion would have to survive in order to come back as vendor fact.
    let persisted = CatalogSnapshot {
        models: attributed
            .models()
            .iter()
            .map(|row| row.evidence().clone())
            .collect(),
        ..fetched.clone()
    };
    let cache = dir.path().join("cache").join("models.json");
    let store = FileCatalogPersistence::open(FileCatalogConfig::new(&cache)).unwrap();
    store
        .save(&[Arc::new(persisted)], &CancellationToken::new())
        .unwrap();

    let raw = std::fs::read_to_string(&cache).unwrap();
    let document: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        document["generations"][0]["models"][0]["capabilities"]["tools"], "unknown",
        "the asserted tri-state must not be written into the cache: {raw}"
    );
    assert!(
        !raw.contains("user") && !raw.contains("override") && !raw.contains("layer"),
        "the cache schema has no place to record who asserted a value: {raw}"
    );

    let reloaded = store.load().unwrap();
    assert_eq!(reloaded, vec![fetched]);
    assert_eq!(
        reloaded[0].models[0].capabilities.tools,
        CapabilitySupport::Unknown,
        "a reload sees provider evidence, never the assertion"
    );
}

/// Precedence resolves per field, and every surviving field names the exact
/// layer that supplied it, so "where did this come from" has one answer.
#[test]
fn higher_layers_win_per_field_and_each_field_names_its_own_layer() {
    let dir = tempfile::tempdir().unwrap();
    let user = dir.path().join("user.toml");
    let project = dir.path().join("project.toml");
    std::fs::write(
        &user,
        format!(
            "schema_version = {OVERRIDE_SCHEMA_VERSION}\n\
             [[model]]\n\
             provider = \"{PROVIDER}\"\n\
             model = \"{MODEL}\"\n\
             [model.capabilities]\n\
             tools = \"supported\"\n\
             native_web = \"supported\"\n"
        ),
    )
    .unwrap();
    std::fs::write(
        &project,
        format!(
            "schema_version = {OVERRIDE_SCHEMA_VERSION}\n\
             [[model]]\n\
             provider = \"{PROVIDER}\"\n\
             model = \"{MODEL}\"\n\
             [model.capabilities]\n\
             tools = \"unsupported\"\n"
        ),
    )
    .unwrap();

    let overrides = CatalogOverrides::load(&CatalogOverridesConfig::new(vec![
        CatalogOverrideLayer::new("user", &user),
        CatalogOverrideLayer::new("project", &project),
    ]))
    .unwrap();
    assert_eq!(overrides.len(), 1);

    let attributed = overrides.attribute(&snapshot(
        CapabilitySupport::Unknown,
        CapabilitySupport::Unknown,
    ));
    let row = attributed.model(MODEL).unwrap();

    let tools = row
        .capability(ModelCapabilityKind::Tools)
        .assertion()
        .unwrap();
    assert_eq!(tools.asserted(), CapabilitySupport::Unsupported);
    assert_eq!(tools.source().layer(), "project");

    let web = row
        .capability(ModelCapabilityKind::NativeWeb)
        .assertion()
        .unwrap();
    assert_eq!(web.asserted(), CapabilitySupport::Supported);
    assert_eq!(
        web.source().layer(),
        "user",
        "a higher layer that says nothing about a field must not erase a lower one"
    );
}

#[test]
fn override_generation_is_immutable_and_exposes_winning_precedence_source_and_capture_time() {
    let dir = tempfile::tempdir().unwrap();
    let user = dir.path().join("user.toml");
    let project = dir.path().join("project.toml");
    std::fs::write(
        &user,
        format!(
            "schema_version = {OVERRIDE_SCHEMA_VERSION}\n\
             [[model]]\n\
             provider = \"{PROVIDER}\"\n\
             model = \"{MODEL}\"\n\
             [model.capabilities]\n\
             tools = \"supported\"\n\
             native_web = \"supported\"\n"
        ),
    )
    .unwrap();
    std::fs::write(
        &project,
        format!(
            "schema_version = {OVERRIDE_SCHEMA_VERSION}\n\
             [[model]]\n\
             provider = \"{PROVIDER}\"\n\
             model = \"{MODEL}\"\n\
             [model.capabilities]\n\
             tools = \"unsupported\"\n"
        ),
    )
    .unwrap();
    let config = CatalogOverridesConfig::new(vec![
        CatalogOverrideLayer::new("user", &user),
        CatalogOverrideLayer::new("project", &project),
    ]);
    let overrides = CatalogOverrides::load_at(&config, 2_000).unwrap();
    let again = CatalogOverrides::load_at(&config, 2_000).unwrap();
    assert_eq!(
        overrides, again,
        "same bytes and capture instant are deterministic"
    );
    assert_eq!(overrides.captured_at_ms(), Some(2_000));
    assert!(matches!(
        CatalogOverrides::load_at(&config, 0),
        Err(heycode_catalog_file::CatalogOverrideError::MissingCaptureInstant)
    ));

    let provider = snapshot(CapabilitySupport::Unknown, CapabilitySupport::Unknown);
    let attributed = overrides.attribute(&provider);
    assert_eq!(attributed.override_captured_at_ms(), Some(2_000));
    let row = attributed.model(MODEL).unwrap();
    let tools_source = row
        .capability(ModelCapabilityKind::Tools)
        .assertion()
        .unwrap()
        .source();
    assert_eq!(tools_source.layer(), "project");
    assert_eq!(tools_source.precedence(), 1);
    assert_eq!(tools_source.captured_at_ms(), 2_000);
    assert!(tools_source.to_string().contains("captured at 2000"));
    let web_source = row
        .capability(ModelCapabilityKind::NativeWeb)
        .assertion()
        .unwrap()
        .source();
    assert_eq!(web_source.layer(), "user");
    assert_eq!(web_source.precedence(), 0);

    // Changing the source file cannot mutate either already-loaded value.
    std::fs::write(
        &project,
        format!(
            "schema_version = {OVERRIDE_SCHEMA_VERSION}\n\
             [[model]]\n\
             provider = \"{PROVIDER}\"\n\
             model = \"{MODEL}\"\n\
             [model.capabilities]\n\
             tools = \"supported\"\n"
        ),
    )
    .unwrap();
    assert_eq!(
        attributed
            .model(MODEL)
            .unwrap()
            .capability(ModelCapabilityKind::Tools)
            .assertion()
            .unwrap()
            .asserted(),
        CapabilitySupport::Unsupported
    );
    assert_eq!(
        provider.models[0].capabilities.tools,
        CapabilitySupport::Unknown
    );

    let reloaded = CatalogOverrides::load_at(&config, 3_000).unwrap();
    let refreshed = reloaded.attribute(&provider);
    let refreshed_tools = refreshed
        .model(MODEL)
        .unwrap()
        .capability(ModelCapabilityKind::Tools)
        .assertion()
        .unwrap();
    assert_eq!(refreshed_tools.asserted(), CapabilitySupport::Supported);
    assert_eq!(refreshed_tools.source().captured_at_ms(), 3_000);
    assert_eq!(attributed.override_captured_at_ms(), Some(2_000));
}

/// An override attributes rows a provider published; it never conjures one.
/// A user who names a model that does not exist is told, not left believing
/// their assertion took effect.
#[test]
fn an_override_naming_an_unpublished_model_is_reported_inert_and_adds_no_row() {
    let dir = tempfile::tempdir().unwrap();
    let body = format!(
        "schema_version = {OVERRIDE_SCHEMA_VERSION}\n\
         [[model]]\n\
         provider = \"{PROVIDER}\"\n\
         model = \"stealth/ox\"\n\
         [model.capabilities]\n\
         tools = \"supported\"\n\
         \n\
         [[model]]\n\
         provider = \"deepseek\"\n\
         model = \"deepseek-v4-pro\"\n\
         [model.capabilities]\n\
         tools = \"supported\"\n"
    );
    let overrides = load_layer(dir.path(), &body);
    let attributed = overrides.attribute(&snapshot(
        CapabilitySupport::Unknown,
        CapabilitySupport::Unknown,
    ));

    assert_eq!(attributed.models().len(), 1);
    assert!(attributed.model("stealth/ox").is_none());
    assert!(
        !attributed.models()[0].has_user_assertion(),
        "an alias is not a canonical id and must not silently collect the override"
    );

    let unmatched = attributed.unmatched();
    assert_eq!(
        unmatched.len(),
        1,
        "only this provider's overrides report here"
    );
    assert_eq!(unmatched[0].model(), "stealth/ox");
    assert!(unmatched[0].describe().contains("was not applied"));
    assert!(unmatched[0].describe().contains("catalog-overrides.toml"));
}

/// The override is a standing assertion re-applied to every generation, so it
/// neither wins nor loses against a later fetch: the fresh evidence is always
/// carried beside it and the conflict becomes visible the moment it appears.
#[test]
fn a_later_generation_that_disproves_a_standing_assertion_surfaces_the_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let overrides = load_layer(dir.path(), &asserting_tools_supported());

    let before = overrides.attribute(&snapshot(
        CapabilitySupport::Unknown,
        CapabilitySupport::Unknown,
    ));
    assert!(before.contradictions().is_empty());
    assert_eq!(
        before
            .model(MODEL)
            .unwrap()
            .capability(ModelCapabilityKind::Tools)
            .assertion()
            .unwrap()
            .direction(),
        AssertionDirection::ClaimsUnevidencedSupport
    );

    // A later refresh publishes explicit evidence against the assertion.
    let refreshed = snapshot_with(
        vec![model(
            CapabilitySupport::Unsupported,
            CapabilitySupport::Unknown,
        )],
        8,
    );
    let after = overrides.attribute(&refreshed);
    let assertion = after
        .model(MODEL)
        .unwrap()
        .capability(ModelCapabilityKind::Tools)
        .assertion()
        .unwrap();

    assert_eq!(
        assertion.direction(),
        AssertionDirection::ContradictsEvidence
    );
    assert_eq!(
        assertion.provider_evidence(),
        CapabilitySupport::Unsupported,
        "the assertion always stands against the newest evidence, not a remembered one"
    );
    assert_eq!(after.contradictions().len(), 1);
    assert_eq!(after.revision(), 8);
}

/// A row nobody overrode reports the exact generation that published each of
/// its capabilities, so "unknown" from a real catalog is distinguishable from
/// "unknown" nobody has refreshed.
#[test]
fn an_unoverridden_row_attributes_every_field_to_the_generation_that_published_it() {
    let attributed = CatalogOverrides::empty().attribute(&snapshot(
        CapabilitySupport::Supported,
        CapabilitySupport::Unknown,
    ));
    let row = attributed.model(MODEL).unwrap();

    assert!(!row.has_user_assertion());
    assert!(row.assertions().is_empty());
    for kind in ModelCapabilityKind::ALL {
        match row.capability(kind) {
            AttributedSupport::ProviderEvidenced(evidence) => {
                assert_eq!(evidence.revision(), 7);
                assert_eq!(evidence.fetched_at_ms(), 1_787_752_741_000);
                assert_eq!(evidence.support(), kind.of(&row.evidence().capabilities));
            }
            AttributedSupport::UserAsserted(_) => panic!("{} was not overridden", kind.name()),
        }
    }
    match row.context_window() {
        AttributedLimit::ProviderEvidenced(evidence) => {
            assert_eq!(evidence.value(), Some(131_072));
            assert_eq!(evidence.revision(), 7);
        }
        AttributedLimit::UserAsserted(_) => panic!("context window was not overridden"),
    }
}

/// A limit assertion is classified by the same taxonomy: smaller narrows,
/// larger contradicts published evidence, and asserting one the catalog never
/// published claims what nothing evidenced.
#[test]
fn a_limit_assertion_is_classified_against_the_published_limit() {
    let dir = tempfile::tempdir().unwrap();
    let body = format!(
        "schema_version = {OVERRIDE_SCHEMA_VERSION}\n\
         [[model]]\n\
         provider = \"{PROVIDER}\"\n\
         model = \"{MODEL}\"\n\
         context_window = 1048576\n\
         max_output_tokens = 65536\n"
    );
    let overrides = load_layer(dir.path(), &body);
    let attributed = overrides.attribute(&snapshot(
        CapabilitySupport::Unknown,
        CapabilitySupport::Unknown,
    ));
    let row = attributed.model(MODEL).unwrap();

    let window = row.context_window().assertion().unwrap();
    assert_eq!(window.field(), ModelLimitField::ContextWindow);
    assert_eq!(window.asserted().get(), 1_048_576);
    assert_eq!(window.provider_evidence(), Some(131_072));
    assert_eq!(window.provider_revision(), 7);
    assert_eq!(window.provider_fetched_at_ms(), FETCHED_AT_MS);
    assert_eq!(window.direction(), AssertionDirection::ContradictsEvidence);
    assert_eq!(row.enforced_context_window(), Some(131_072));
    assert!(matches!(
        row.context_window().enforcement(),
        LimitEnforcement::ProviderEvidence(evidence)
            if evidence.value() == Some(131_072) && evidence.revision() == 7
    ));
    assert_eq!(row.evidence().context_window, Some(131_072));

    let output = row.max_output_tokens().assertion().unwrap();
    assert_eq!(output.provider_evidence(), None);
    assert_eq!(
        output.direction(),
        AssertionDirection::ClaimsUnevidencedSupport
    );
    assert_eq!(row.enforced_max_output_tokens(), None);
    assert!(row.context_window().render().contains("user-asserted"));
    assert!(
        row.context_window()
            .render()
            .contains("provider catalog: 131072")
    );

    let smaller = format!(
        "schema_version = {OVERRIDE_SCHEMA_VERSION}\n\
         [[model]]\n\
         provider = \"{PROVIDER}\"\n\
         model = \"{MODEL}\"\n\
         context_window = 8192\n"
    );
    let narrowed = load_layer(dir.path(), &smaller).attribute(&snapshot(
        CapabilitySupport::Unknown,
        CapabilitySupport::Unknown,
    ));
    assert_eq!(
        narrowed
            .model(MODEL)
            .unwrap()
            .context_window()
            .assertion()
            .unwrap()
            .direction(),
        AssertionDirection::Narrowing
    );
    assert!(matches!(
        narrowed.model(MODEL).unwrap().context_window().enforcement(),
        LimitEnforcement::UserConstraint(assertion)
            if assertion.asserted().get() == 8_192
                && assertion.provider_revision() == 7
    ));
}

/// One call lists every assertion on a row in stable field order, so a surface
/// cannot show a model while missing one of the overrides applied to it.
#[test]
fn every_assertion_on_a_row_is_listed_once_in_stable_field_order() {
    let dir = tempfile::tempdir().unwrap();
    let body = format!(
        "schema_version = {OVERRIDE_SCHEMA_VERSION}\n\
         [[model]]\n\
         provider = \"{PROVIDER}\"\n\
         model = \"{MODEL}\"\n\
         context_window = 262144\n\
         [model.capabilities]\n\
         prompt_cache = \"supported\"\n\
         tools = \"supported\"\n"
    );
    let overrides = load_layer(dir.path(), &body);
    let attributed = overrides.attribute(&snapshot(
        CapabilitySupport::Unknown,
        CapabilitySupport::Unknown,
    ));
    let row = attributed.model(MODEL).unwrap();

    let fields: Vec<&str> = row
        .assertions()
        .iter()
        .map(ModelAssertion::field_name)
        .collect();
    assert_eq!(fields, ["tools", "prompt_cache", "context_window"]);
    assert!(row.has_user_assertion());

    let catalog_wide: Vec<(&str, &str)> = attributed
        .assertions()
        .iter()
        .map(|(id, assertion)| (*id, assertion.field_name()))
        .collect();
    assert_eq!(
        catalog_wide,
        [
            (MODEL, "tools"),
            (MODEL, "prompt_cache"),
            (MODEL, "context_window")
        ]
    );
    for assertion in row.assertions() {
        assert!(assertion.describe().contains("user-"));
        assert_eq!(assertion.source().layer(), "user");
    }
}

/// Every malformed entry fails the whole load. A partially applied override
/// set would leave a user believing an assertion took effect when it did not.
#[test]
fn malformed_override_entries_fail_the_whole_load() {
    let dir = tempfile::tempdir().unwrap();
    let header = format!("schema_version = {OVERRIDE_SCHEMA_VERSION}\n");

    let cases: [(String, &str); 6] = [
        (
            format!(
                "{header}[[model]]\nprovider = \"{PROVIDER}\"\nmodel = \"{MODEL}\"\n[model.capabilities]\ntools = \"yes\"\n"
            ),
            "expected supported, unsupported or unknown",
        ),
        (
            format!(
                "{header}[[model]]\nprovider = \"{PROVIDER}\"\nmodel = \"{MODEL}\"\ncontext_window = 0\n"
            ),
            "must be greater than zero",
        ),
        (
            format!("{header}[[model]]\nprovider = \"{PROVIDER}\"\nmodel = \"{MODEL}\"\n"),
            "asserts no field",
        ),
        (
            format!(
                "{header}[[model]]\nprovider = \"{PROVIDER}\"\nmodel = \"{MODEL}\"\n[model.capabilities]\ntools = \"supported\"\n\n[[model]]\nprovider = \"{PROVIDER}\"\nmodel = \"{MODEL}\"\n[model.capabilities]\nreasoning = \"supported\"\n"
            ),
            "overridden more than once",
        ),
        (
            format!(
                "{header}[[model]]\nprovider = \"  \"\nmodel = \"{MODEL}\"\n[model.capabilities]\ntools = \"supported\"\n"
            ),
            "`provider` must not be blank",
        ),
        (
            format!(
                "{header}[[model]]\nprovider = \"{PROVIDER}\"\nmodel = \"\"\n[model.capabilities]\ntools = \"supported\"\n"
            ),
            "`model` must not be blank",
        ),
    ];

    for (body, expected) in cases {
        let rendered = load_error(dir.path(), &body);
        assert!(
            rendered.contains(expected),
            "expected `{expected}` in: {rendered}"
        );
        assert!(!rendered.contains("panicked"), "{rendered}");
    }
}

/// An unsupported schema fails before any assertion is applied, in both
/// directions, exactly as the catalog cache does.
#[test]
fn override_documents_outside_the_supported_schema_fail_loud() {
    let dir = tempfile::tempdir().unwrap();
    let newer = load_error(
        dir.path(),
        &format!("schema_version = {}\n", OVERRIDE_SCHEMA_VERSION + 1),
    );
    assert!(newer.contains("newer schema"), "{newer}");

    let missing = load_error(dir.path(), "[[model]]\nprovider = \"a\"\nmodel = \"b\"\n");
    assert!(missing.contains("unsupported schema 0"), "{missing}");
}

/// Overrides are optional, and an absent layer contributes nothing rather than
/// failing startup.
#[test]
fn an_absent_override_layer_is_empty_rather_than_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let overrides = CatalogOverrides::load(&CatalogOverridesConfig::new(vec![
        CatalogOverrideLayer::new("user", dir.path().join("missing.toml")),
    ]))
    .unwrap();
    assert!(overrides.is_empty());

    let attributed = overrides.attribute(&snapshot(
        CapabilitySupport::Supported,
        CapabilitySupport::Unknown,
    ));
    assert!(attributed.assertions().is_empty());
    assert!(attributed.unmatched().is_empty());
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_override_path_is_refused() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.toml");
    std::fs::write(
        &target,
        format!("schema_version = {OVERRIDE_SCHEMA_VERSION}\n"),
    )
    .unwrap();
    let link = dir.path().join("catalog-overrides.toml");
    symlink(&target, &link).unwrap();

    let rendered = CatalogOverrides::load(&CatalogOverridesConfig::new(vec![
        CatalogOverrideLayer::new("user", &link),
    ]))
    .expect_err("a symlinked override path must be refused")
    .to_string();
    assert!(rendered.contains("symbolic link"), "{rendered}");
}

/// An assertion must always be able to name its origin, so two layers cannot
/// share a label and a label cannot be blank.
#[test]
fn override_layers_must_carry_distinct_nonblank_labels() {
    let dir = tempfile::tempdir().unwrap();
    let duplicate = CatalogOverrides::load(&CatalogOverridesConfig::new(vec![
        CatalogOverrideLayer::new("user", dir.path().join("a.toml")),
        CatalogOverrideLayer::new("user", dir.path().join("b.toml")),
    ]))
    .expect_err("duplicate labels must be refused")
    .to_string();
    assert!(duplicate.contains("used more than once"), "{duplicate}");

    let blank = CatalogOverrides::load(&CatalogOverridesConfig::new(vec![
        CatalogOverrideLayer::new("  ", dir.path().join("a.toml")),
    ]))
    .expect_err("a blank label must be refused")
    .to_string();
    assert!(blank.contains("must not be blank"), "{blank}");
}

/// The overrides plugin publishes its service, and a malformed document fails
/// composition rather than starting with a user's stated intent dropped.
#[test]
fn the_overrides_plugin_publishes_its_service_and_fails_composition_on_bad_input() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("catalog-overrides.toml");
    std::fs::write(&path, asserting_tools_supported()).unwrap();

    let plugins = vec![catalog_overrides_plugin(CatalogOverridesConfig::new(vec![
        CatalogOverrideLayer::new("user", &path),
    ]))];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let overrides = context
        .get::<CatalogOverrides>(SERVICE_CATALOG_OVERRIDES)
        .unwrap();
    assert_eq!(overrides.len(), 1);
    context.shutdown();

    std::fs::write(&path, "schema_version = 1\n[[model]]\nprovider = \"a\"\n").unwrap();
    let broken = vec![catalog_overrides_plugin(CatalogOverridesConfig::new(vec![
        CatalogOverrideLayer::new("user", &path),
    ]))];
    let error = match heycode_core::compose(&broken) {
        Ok(_) => panic!("a malformed override document must fail composition"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("catalog overrides"), "{error}");
}
