//! POR06 explicit OpenRouter request-plugin transforms.
//!
//! The claim under test is about request intent: heycode never omits a known
//! transform and then mistakes omission for disabled. Account policy can
//! override a request, so actual execution remains Unknown until response
//! evidence exists. Every case below fails if a transform drops out of the
//! request decision, loses its activation record, or reports an unpublished
//! price as zero.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_llm::{CapabilitySupport, PriceCurrency};
use heycode_provider_openrouter::{
    OPENROUTER_TRANSFORM_OPTION_KIND, OPENROUTER_TRANSFORM_WIRE_FIELD, OpenRouterPdfEngine,
    OpenRouterTransform, OpenRouterTransformCost, OpenRouterTransformEffect,
    OpenRouterTransformPolicy, OpenRouterTransformPolicyError, OpenRouterTransformRequest,
    OpenRouterTransformRequestContext,
};
use serde_json::{Value, json};

/// Every policy a caller can build from the public API, so an invariant can be
/// asserted across the whole configuration space rather than one sample.
fn every_policy() -> Vec<OpenRouterTransformPolicy> {
    let engines = [
        None,
        Some(OpenRouterPdfEngine::MistralOcr),
        Some(OpenRouterPdfEngine::CloudflareAi),
        Some(OpenRouterPdfEngine::Native),
    ];
    let mut policies = Vec::new();
    for compression in [false, true] {
        for healing in [false, true] {
            for engine in engines {
                let mut policy = OpenRouterTransformPolicy::all_disabled();
                if compression {
                    policy = policy.with_context_compression();
                }
                if healing {
                    policy = policy.with_response_healing();
                }
                if let Some(engine) = engine {
                    policy = policy.with_document_parsing(engine);
                }
                policies.push(policy);
            }
        }
    }
    policies
}

fn entries(policy: &OpenRouterTransformPolicy) -> Vec<Value> {
    match policy.plugins_wire() {
        Value::Array(entries) => entries,
        other => panic!("plugins wire must be an array, got {other}"),
    }
}

fn entry_id(entry: &Value) -> &str {
    entry
        .get("id")
        .and_then(Value::as_str)
        .expect("every plugin entry carries an id")
}

#[test]
fn the_default_policy_disables_every_known_transform_explicitly_instead_of_omitting_it() {
    // Omission is not "off": OpenRouter defaults context compression on for
    // 8k-or-smaller endpoints, and an account default can switch on any
    // plugin a request does not mention. Only an explicit `enabled: false`
    // closes both doors.
    let wire = OpenRouterTransformPolicy::all_disabled().plugins_wire();
    assert_eq!(
        wire,
        json!([
            {"id": "context-compression", "enabled": false},
            {"id": "file-parser", "enabled": false},
            {"id": "response-healing", "enabled": false},
        ])
    );
}

#[test]
fn the_plugins_array_always_carries_one_entry_per_known_transform() {
    for policy in every_policy() {
        let entries = entries(&policy);
        let ids: Vec<&str> = entries.iter().map(entry_id).collect();
        let expected: Vec<&str> = OpenRouterTransform::ALL
            .into_iter()
            .map(OpenRouterTransform::id)
            .collect();
        assert_eq!(ids, expected, "policy {policy:?} dropped a transform");
    }
}

#[test]
fn every_transform_the_wire_does_not_disable_is_one_the_policy_reports_as_enabled() {
    // The exact silent-enable shape: an entry that neither disables the
    // transform nor is recorded as a deliberate enable.
    for policy in every_policy() {
        for (transform, entry) in OpenRouterTransform::ALL.into_iter().zip(entries(&policy)) {
            let disabled_on_wire = entry.get("enabled") == Some(&Value::Bool(false));
            let requested = policy.request(transform);
            assert_eq!(
                disabled_on_wire,
                requested == OpenRouterTransformRequest::Disabled,
                "{} wire form and recorded request disagree in {policy:?}",
                transform.id()
            );
        }
    }
}

#[test]
fn context_compression_is_the_only_transform_with_a_builtin_api_default() {
    for transform in OpenRouterTransform::ALL {
        assert_eq!(
            transform.defaults_on_upstream(),
            transform == OpenRouterTransform::ContextCompression,
            "{} upstream default is misreported",
            transform.id()
        );
    }
}

#[test]
fn every_transform_can_be_enabled_by_an_openrouter_account_default() {
    for transform in OpenRouterTransform::ALL {
        assert!(
            transform.account_default_can_enable(),
            "{} could disappear from the explicit disable set",
            transform.id()
        );
    }
}

#[test]
fn response_healing_records_its_non_streaming_structured_output_prerequisites() {
    for transform in OpenRouterTransform::ALL {
        assert_eq!(
            transform.requires_non_streaming(),
            transform == OpenRouterTransform::ResponseHealing,
            "{} has the wrong streaming prerequisite",
            transform.id()
        );
        assert_eq!(
            transform.requires_structured_output(),
            transform == OpenRouterTransform::ResponseHealing,
            "{} has the wrong output-shape prerequisite",
            transform.id()
        );
    }
}

#[test]
fn a_transform_openrouter_defaults_on_is_still_disabled_by_the_default_policy() {
    let policy = OpenRouterTransformPolicy::all_disabled();
    for transform in OpenRouterTransform::ALL.into_iter() {
        if transform.defaults_on_upstream() {
            assert_eq!(
                policy.request(transform),
                OpenRouterTransformRequest::Disabled
            );
        }
    }
}

#[test]
fn an_enabled_transform_uses_openrouters_documented_bare_id_form() {
    let wire = OpenRouterTransformPolicy::all_disabled()
        .with_context_compression()
        .plugins_wire();
    assert_eq!(
        wire[0],
        json!({"id": "context-compression"}),
        "enabling must use the documented bare-id form"
    );
}

#[test]
fn enabling_one_transform_leaves_every_other_transform_explicitly_disabled() {
    let wire = OpenRouterTransformPolicy::all_disabled()
        .with_response_healing()
        .plugins_wire();
    assert_eq!(
        wire,
        json!([
            {"id": "context-compression", "enabled": false},
            {"id": "file-parser", "enabled": false},
            {"id": "response-healing"},
        ])
    );
}

#[test]
fn enabling_document_parsing_pins_the_engine_on_the_wire() {
    let wire = OpenRouterTransformPolicy::all_disabled()
        .with_document_parsing(OpenRouterPdfEngine::MistralOcr)
        .plugins_wire();
    assert_eq!(
        wire[1],
        json!({"id": "file-parser", "pdf": {"engine": "mistral-ocr"}}),
        "an enabled parser must name its engine rather than let OpenRouter choose"
    );
}

#[test]
fn every_transform_heycode_enables_produces_an_activation_that_names_it() {
    let cases = [
        (
            OpenRouterTransformPolicy::all_disabled().with_context_compression(),
            OpenRouterTransform::ContextCompression,
        ),
        (
            OpenRouterTransformPolicy::all_disabled()
                .with_document_parsing(OpenRouterPdfEngine::Native),
            OpenRouterTransform::FileParser,
        ),
        (
            OpenRouterTransformPolicy::all_disabled().with_response_healing(),
            OpenRouterTransform::ResponseHealing,
        ),
    ];
    for (policy, expected) in cases {
        let enabled = policy.enabled_activations();
        assert_eq!(enabled.len(), 1, "{policy:?} recorded the wrong row count");
        assert_eq!(enabled[0].transform(), expected);
        assert_eq!(enabled[0].request(), OpenRouterTransformRequest::Enabled);
    }
}

#[test]
fn the_activation_records_line_up_with_the_wire_entries_one_for_one() {
    for policy in every_policy() {
        let activations = policy.activations();
        let entries = entries(&policy);
        assert_eq!(activations.len(), entries.len());
        for (activation, entry) in activations.iter().zip(entries) {
            assert_eq!(activation.transform().id(), entry_id(&entry));
        }
    }
}

#[test]
fn no_transform_can_be_enabled_without_a_cost_record() {
    for policy in every_policy() {
        for activation in policy.enabled_activations() {
            assert!(
                activation.cost().is_some(),
                "{} was enabled with no recorded cost",
                activation.transform().id()
            );
        }
    }
}

#[test]
fn a_disabled_transform_records_no_cost_rather_than_a_free_one() {
    for policy in every_policy() {
        for activation in policy.activations() {
            if activation.request() == OpenRouterTransformRequest::Disabled {
                assert_eq!(
                    activation.cost(),
                    None,
                    "{} claimed a price for a transform heycode did not buy",
                    activation.transform().id()
                );
            }
        }
    }
}

#[test]
fn mistral_ocr_publishes_two_us_dollars_per_thousand_pages_as_exact_pico_units() {
    let price = OpenRouterPdfEngine::MistralOcr
        .cost()
        .published_price()
        .expect("the OCR engine publishes a per-page fee");
    assert_eq!(price.currency(), PriceCurrency::Usd);
    assert_eq!(price.pico_units_per_thousand_pages(), 2_000_000_000_000);
}

#[test]
fn the_cloudflare_engine_is_documented_free_and_the_native_engine_bills_input_tokens() {
    assert_eq!(
        OpenRouterPdfEngine::CloudflareAi.cost(),
        OpenRouterTransformCost::DocumentedFree
    );
    assert_eq!(
        OpenRouterPdfEngine::Native.cost(),
        OpenRouterTransformCost::UpstreamInputTokens
    );
}

#[test]
fn an_unpinned_document_engine_costs_unknown_and_never_reads_as_free() {
    // OpenRouter picks native or billed OCR per model when no engine is
    // pinned, so the fee is not knowable from the request. Rendering that as
    // zero is the silent-billing failure.
    let unpinned = OpenRouterTransformCost::UNPINNED_DOCUMENT;
    assert_eq!(unpinned, OpenRouterTransformCost::Unknown);
    assert!(!unpinned.is_documented_free());
    assert_eq!(unpinned.published_price(), None);
    assert_ne!(unpinned, OpenRouterTransformCost::DocumentedFree);
}

#[test]
fn context_compression_publishes_no_price_so_its_cost_is_unknown_not_free() {
    let cost = OpenRouterTransformPolicy::all_disabled()
        .with_context_compression()
        .enabled_activations()[0]
        .cost()
        .expect("an enabled transform records a cost");
    assert_eq!(cost, OpenRouterTransformCost::Unknown);
    assert!(!cost.is_documented_free());
}

#[test]
fn response_healing_is_documented_free() {
    let cost = OpenRouterTransformPolicy::all_disabled()
        .with_response_healing()
        .enabled_activations()[0]
        .cost()
        .expect("an enabled transform records a cost");
    assert_eq!(cost, OpenRouterTransformCost::DocumentedFree);
    assert!(cost.is_documented_free());
}

#[test]
fn only_a_documented_zero_answers_true_to_is_documented_free() {
    let costs = [
        OpenRouterTransformCost::Unknown,
        OpenRouterTransformCost::DocumentedFree,
        OpenRouterTransformCost::UpstreamInputTokens,
        OpenRouterPdfEngine::MistralOcr.cost(),
    ];
    let free: Vec<bool> = costs.iter().map(|cost| cost.is_documented_free()).collect();
    assert_eq!(free, vec![false, true, false, false]);
}

#[test]
fn the_effective_state_of_every_activation_stays_unknown_because_an_account_can_prevent_overrides()
{
    // heycode holds the request, not the account setting. "Prevent overrides"
    // lets an account enforce a plugin configuration a request cannot change,
    // so a request is never evidence of what ran.
    for policy in every_policy() {
        for activation in policy.activations() {
            assert_eq!(
                activation.effective(),
                CapabilitySupport::Unknown,
                "{} promoted a request into an outcome",
                activation.transform().id()
            );
        }
    }
}

#[test]
fn the_provider_option_wraps_the_plugins_array_under_the_exact_wire_field() {
    let policy = OpenRouterTransformPolicy::all_disabled()
        .with_document_parsing(OpenRouterPdfEngine::Native);
    let option = policy
        .provider_option(OpenRouterTransformRequestContext::new(true, false))
        .expect("the bounded plugin array is a valid provider option");
    assert_eq!(option.kind(), OPENROUTER_TRANSFORM_OPTION_KIND);
    assert_eq!(option.provider(), heycode_llm::OpenRouterProvider::NAME);
    assert_eq!(
        option.data().get(OPENROUTER_TRANSFORM_WIRE_FIELD),
        Some(&policy.plugins_wire()),
        "the option must carry the exact array the wire field takes"
    );
}

#[test]
fn the_provider_option_of_the_default_policy_still_disables_every_transform() {
    // The durable option is what actually reaches the request; a wire form
    // that disables while the option does not would be silent enabling.
    let option = OpenRouterTransformPolicy::all_disabled()
        .provider_option(OpenRouterTransformRequestContext::new(true, false))
        .expect("the default policy is a valid provider option");
    assert_eq!(
        option.data().get(OPENROUTER_TRANSFORM_WIRE_FIELD),
        Some(&json!([
            {"id": "context-compression", "enabled": false},
            {"id": "file-parser", "enabled": false},
            {"id": "response-healing", "enabled": false},
        ]))
    );
}

#[test]
fn response_healing_only_materializes_for_non_streaming_structured_output() {
    let policy = OpenRouterTransformPolicy::all_disabled().with_response_healing();
    assert_eq!(
        policy.provider_option(OpenRouterTransformRequestContext::new(true, true)),
        Err(OpenRouterTransformPolicyError::ResponseHealingRequiresNonStreaming)
    );
    assert_eq!(
        policy.provider_option(OpenRouterTransformRequestContext::new(false, false)),
        Err(OpenRouterTransformPolicyError::ResponseHealingRequiresStructuredOutput)
    );

    let option = policy
        .provider_option(OpenRouterTransformRequestContext::new(false, true))
        .expect("documented response-healing prerequisites are sufficient");
    assert_eq!(
        option.data().get(OPENROUTER_TRANSFORM_WIRE_FIELD),
        Some(&json!([
            {"id": "context-compression", "enabled": false},
            {"id": "file-parser", "enabled": false},
            {"id": "response-healing"},
        ]))
    );
}

#[test]
fn each_transform_declares_the_effect_a_surface_must_show() {
    assert_eq!(
        OpenRouterTransform::ContextCompression.effect(),
        OpenRouterTransformEffect::RewritesPromptAndRoute
    );
    assert_eq!(
        OpenRouterTransform::FileParser.effect(),
        OpenRouterTransformEffect::ParsesDocuments
    );
    assert_eq!(
        OpenRouterTransform::ResponseHealing.effect(),
        OpenRouterTransformEffect::RewritesResponse
    );
}

#[test]
fn transform_and_engine_ids_match_the_documented_openrouter_plugin_names() {
    let transforms: Vec<&str> = OpenRouterTransform::ALL
        .into_iter()
        .map(OpenRouterTransform::id)
        .collect();
    assert_eq!(
        transforms,
        vec!["context-compression", "file-parser", "response-healing"]
    );
    let engines: Vec<&str> = [
        OpenRouterPdfEngine::MistralOcr,
        OpenRouterPdfEngine::CloudflareAi,
        OpenRouterPdfEngine::Native,
    ]
    .into_iter()
    .map(OpenRouterPdfEngine::id)
    .collect();
    assert_eq!(engines, vec!["mistral-ocr", "cloudflare-ai", "native"]);
}

#[test]
fn the_web_search_candidate_is_not_one_of_these_transforms() {
    // POR05 reaches OpenRouter search through the server-tool path. If a
    // future change moves it into the plugins array it must be a deliberate
    // one, not a quiet second mechanism for the same feature.
    for transform in OpenRouterTransform::ALL {
        assert_ne!(transform.id(), "web");
        assert_ne!(transform.id(), "openrouter:web_search");
    }
}
