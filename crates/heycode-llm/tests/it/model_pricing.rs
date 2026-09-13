//! Normalized pricing and advisory performance metadata contracts (CAT06).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_llm::{
    AdapterOwnedAuth, AuthenticationBinding, CallPurpose, CapabilitySupport, ChatMessage,
    InferenceInput, InferenceTarget, InputModality, ModelCapabilities, ModelDescriptor,
    ModelLifecycle, ModelMetadataProvenance, ModelPerformance, ModelPricing, ModelProvenanceError,
    PerformanceError, PriceComponent, PriceCurrency, PricingError, Provider, ProviderDescriptor,
    ProviderProtocol, ReasoningEffortId, RequestDraft, RequestedCapability, ResolveError,
    ResolveSpec, TokenPrice, TokenPriceUnit, resolve_request,
};

fn usd_per_token(amount: &str) -> TokenPrice {
    TokenPrice::parse_decimal(PriceCurrency::Usd, TokenPriceUnit::PerToken, amount).unwrap()
}

fn provenance(source: &str, captured_at_ms: u64) -> ModelMetadataProvenance {
    ModelMetadataProvenance::new(source, captured_at_ms).unwrap()
}

fn pricing_from_components(
    provenance: ModelMetadataProvenance,
    components: impl IntoIterator<Item = (PriceComponent, TokenPrice)>,
) -> Result<ModelPricing, PricingError> {
    let mut components = components.into_iter();
    let Some((first_component, first_price)) = components.next() else {
        return Ok(ModelPricing::unknown());
    };
    let mut pricing = ModelPricing::captured(provenance, first_component, first_price);
    for (component, price) in components {
        pricing = pricing.with(component, price)?;
    }
    Ok(pricing)
}

#[test]
fn pricing_and_performance_retain_source_and_capture_time_at_the_value_layer() {
    let catalog = provenance("https://openrouter.ai/api/v1/models", 1_700_000_000_123);
    let benchmark = provenance("benchmark:local/cat06", 1_700_000_000_456);
    let pricing = ModelPricing::captured(
        catalog.clone(),
        PriceComponent::Input,
        usd_per_token("0.000000075"),
    );
    let performance =
        ModelPerformance::observed(benchmark.clone(), Some(420), Some(87_500)).unwrap();

    assert_eq!(pricing.provenance(), Some(&catalog));
    assert_eq!(pricing.provenance().unwrap().source(), catalog.source());
    assert_eq!(
        pricing.provenance().unwrap().captured_at_ms(),
        1_700_000_000_123
    );
    assert_eq!(performance.provenance(), Some(&benchmark));
    assert_eq!(performance.observed_at_ms(), Some(1_700_000_000_456));
    assert_eq!(ModelPricing::unknown().provenance(), None);
    assert_eq!(ModelPerformance::unknown().provenance(), None);

    assert_eq!(
        ModelMetadataProvenance::new("", 1),
        Err(ModelProvenanceError::InvalidSource)
    );
    assert_eq!(
        ModelMetadataProvenance::new("provider\nbody", 1),
        Err(ModelProvenanceError::InvalidSource)
    );
    assert_eq!(
        ModelMetadataProvenance::new("catalog:provider", 0),
        Err(ModelProvenanceError::MissingCaptureInstant)
    );
}

#[test]
fn token_price_keeps_currency_unit_and_exact_amount_together() {
    let price = usd_per_token("0.0000001");
    assert_eq!(price.currency(), PriceCurrency::Usd);
    assert_eq!(price.unit(), TokenPriceUnit::PerToken);
    // 1e-7 USD per token is exactly 100_000 pico-USD per token.
    assert_eq!(price.pico_units(), 100_000);
    assert!(!price.is_zero());

    // Same figure published in the other real-world unit stays exact and stays
    // labelled with the unit it was published in.
    let per_million =
        TokenPrice::parse_decimal(PriceCurrency::Usd, TokenPriceUnit::PerMillionTokens, "0.10")
            .unwrap();
    assert_eq!(per_million.unit(), TokenPriceUnit::PerMillionTokens);
    assert_eq!(per_million.pico_units(), 100_000_000_000);
    assert_ne!(price, per_million);
}

#[test]
fn token_price_round_trips_through_its_exact_integer_representation() {
    for amount in [
        "0",
        "0.000000000001",
        "0.00000002",
        "0.0000001",
        "0.000075",
        "1",
    ] {
        let parsed = usd_per_token(amount);
        let rebuilt =
            TokenPrice::from_pico_units(parsed.currency(), parsed.unit(), parsed.pico_units())
                .unwrap();
        assert_eq!(parsed, rebuilt, "round trip lost `{amount}`");
    }
}

#[test]
fn absent_price_component_is_unknown_and_never_zero() {
    let free = usd_per_token("0");
    assert_eq!(
        ModelPricing::unknown().with(PriceComponent::Input, free),
        Err(PricingError::MissingProvenance),
        "a number cannot become normalized pricing before its source is known"
    );
    let pricing = ModelPricing::captured(
        provenance("test:absent-price", 1),
        PriceComponent::Input,
        free,
    );

    // A published `0` is evidence the component is free.
    assert_eq!(pricing.price(PriceComponent::Input), Some(&free));
    assert!(pricing.price(PriceComponent::Input).unwrap().is_zero());

    // Everything the provider did not publish stays unknown. It must not read
    // back as a zero price, because unknown cost is not free cost.
    assert_eq!(pricing.price(PriceComponent::Output), None);
    assert_eq!(pricing.price(PriceComponent::CachedInput), None);
    assert_eq!(pricing.price(PriceComponent::CacheWrite), None);
    assert_eq!(pricing.price(PriceComponent::Reasoning), None);

    // No evidence at all is distinct from evidence that everything is free.
    let nothing = ModelPricing::unknown();
    assert!(nothing.is_unknown());
    assert!(!pricing.is_unknown());
    assert_ne!(nothing, pricing);
    assert_eq!(nothing.price(PriceComponent::Input), None);
}

#[test]
fn unknown_currency_and_unit_and_component_strings_are_rejected_not_defaulted() {
    assert_eq!(PriceCurrency::parse("USD"), Ok(PriceCurrency::Usd));
    assert_eq!(
        PriceCurrency::parse("EUR"),
        Err(PricingError::UnknownCurrency {
            code: "EUR".to_owned()
        })
    );
    assert_eq!(
        PriceCurrency::parse("usd"),
        Err(PricingError::UnknownCurrency {
            code: "usd".to_owned()
        })
    );
    assert_eq!(
        PriceCurrency::parse(""),
        Err(PricingError::UnknownCurrency {
            code: String::new()
        })
    );

    assert_eq!(
        TokenPriceUnit::parse("per_token"),
        Ok(TokenPriceUnit::PerToken)
    );
    assert_eq!(
        TokenPriceUnit::parse("per_million_tokens"),
        Ok(TokenPriceUnit::PerMillionTokens)
    );
    assert_eq!(
        TokenPriceUnit::parse("per_request"),
        Err(PricingError::UnknownUnit {
            name: "per_request".to_owned()
        })
    );

    assert_eq!(
        PriceComponent::parse("cached_input"),
        Ok(PriceComponent::CachedInput)
    );
    assert_eq!(
        PriceComponent::parse("prompt"),
        Err(PricingError::UnknownComponent {
            name: "prompt".to_owned()
        })
    );

    // Names round-trip so an explicit file mapping cannot drift.
    for component in [
        PriceComponent::Input,
        PriceComponent::Output,
        PriceComponent::CachedInput,
        PriceComponent::CacheWrite,
        PriceComponent::Reasoning,
    ] {
        assert_eq!(PriceComponent::parse(component.name()), Ok(component));
    }
    assert_eq!(
        PriceCurrency::parse(PriceCurrency::Usd.code()),
        Ok(PriceCurrency::Usd)
    );
    for unit in [TokenPriceUnit::PerToken, TokenPriceUnit::PerMillionTokens] {
        assert_eq!(TokenPriceUnit::parse(unit.name()), Ok(unit));
    }
}

#[test]
fn negative_non_finite_and_absurd_price_amounts_are_rejected() {
    let parse = |amount: &str| {
        TokenPrice::parse_decimal(PriceCurrency::Usd, TokenPriceUnit::PerToken, amount)
    };

    assert_eq!(parse("-0.0000001"), Err(PricingError::NegativeAmount));
    assert_eq!(parse("-0"), Err(PricingError::NegativeAmount));

    for malformed in [
        "", "NaN", "nan", "inf", "-inf", "Infinity", "1e-7", "0.1.2", " 0.1", "0.1 ", "0,1", "abc",
        ".", "+1", "0x1",
    ] {
        assert_eq!(
            parse(malformed),
            Err(PricingError::MalformedAmount),
            "accepted malformed amount `{malformed}`"
        );
    }

    // Precision beyond the exact representation fails instead of rounding.
    assert_eq!(
        parse("0.0000000000001"),
        Err(PricingError::ExcessivePrecision)
    );
    // Trailing zeros past the limit carry no value and stay acceptable.
    assert_eq!(parse("0.0000001000000000").unwrap().pico_units(), 100_000);

    // Absurd magnitudes fail rather than being stored.
    assert_eq!(parse("2"), Err(PricingError::AmountOutOfRange));
    assert_eq!(
        TokenPrice::parse_decimal(
            PriceCurrency::Usd,
            TokenPriceUnit::PerMillionTokens,
            "2000000"
        ),
        Err(PricingError::AmountOutOfRange)
    );
    assert_eq!(
        TokenPrice::from_pico_units(PriceCurrency::Usd, TokenPriceUnit::PerToken, u64::MAX),
        Err(PricingError::AmountOutOfRange)
    );
}

#[test]
fn one_model_may_not_mix_currencies_units_or_duplicate_components() {
    let per_token = usd_per_token("0.0000001");
    let per_million =
        TokenPrice::parse_decimal(PriceCurrency::Usd, TokenPriceUnit::PerMillionTokens, "0.40")
            .unwrap();

    let pricing = ModelPricing::captured(
        provenance("test:denomination", 1),
        PriceComponent::Input,
        per_token,
    );
    assert_eq!(pricing.currency(), Some(PriceCurrency::Usd));
    assert_eq!(pricing.unit(), Some(TokenPriceUnit::PerToken));

    assert_eq!(
        pricing.clone().with(PriceComponent::Input, per_token),
        Err(PricingError::DuplicateComponent { component: "input" })
    );
    assert_eq!(
        pricing.with(PriceComponent::Output, per_million),
        Err(PricingError::InconsistentDenomination {
            existing: "USD per_token".to_owned(),
            added: "USD per_million_tokens".to_owned(),
        })
    );

    assert_eq!(ModelPricing::unknown().currency(), None);
    assert_eq!(ModelPricing::unknown().unit(), None);
}

/// The OpenRouter `/models` row publishes a `pricing` object of decimal
/// strings denominated in USD per token. This drives that exact shape through
/// the vocabulary the way `heycode-provider-openrouter` will, and asserts every
/// component lands exactly with nothing invented for the keys it omits.
#[test]
fn openrouter_pricing_object_shape_normalizes_exactly() {
    let wire = serde_json::json!({
        "prompt": "0.0000002",
        "completion": "0.0000011",
        "input_cache_read": "0.00000002",
        "input_cache_write": "0.00000025",
        "internal_reasoning": "0"
    });

    let normalized = normalize_openrouter_pricing(&wire).unwrap();
    assert_eq!(normalized.currency(), Some(PriceCurrency::Usd));
    assert_eq!(normalized.unit(), Some(TokenPriceUnit::PerToken));
    assert_eq!(
        normalized
            .price(PriceComponent::Input)
            .unwrap()
            .pico_units(),
        200_000
    );
    assert_eq!(
        normalized
            .price(PriceComponent::Output)
            .unwrap()
            .pico_units(),
        1_100_000
    );
    assert_eq!(
        normalized
            .price(PriceComponent::CachedInput)
            .unwrap()
            .pico_units(),
        20_000
    );
    assert_eq!(
        normalized
            .price(PriceComponent::CacheWrite)
            .unwrap()
            .pico_units(),
        250_000
    );
    assert!(
        normalized
            .price(PriceComponent::Reasoning)
            .unwrap()
            .is_zero()
    );

    // Components iterate deterministically so an explicit file mapping is stable.
    let listed: Vec<_> = normalized
        .components()
        .map(|(component, _)| component.name())
        .collect();
    assert_eq!(
        listed,
        [
            "input",
            "output",
            "cached_input",
            "cache_write",
            "reasoning"
        ]
    );

    // A row that publishes only the two mandatory components leaves the rest
    // absent — never zero-filled.
    let partial = normalize_openrouter_pricing(&serde_json::json!({
        "prompt": "0.0000001",
        "completion": "0.0000004"
    }))
    .unwrap();
    assert!(partial.price(PriceComponent::Input).is_some());
    assert_eq!(partial.price(PriceComponent::CachedInput), None);
    assert_eq!(partial.price(PriceComponent::Reasoning), None);

    // A malformed live value rejects the row instead of being coerced.
    assert_eq!(
        normalize_openrouter_pricing(&serde_json::json!({ "prompt": "-1" })),
        Err(PricingError::NegativeAmount)
    );
}

fn normalize_openrouter_pricing(wire: &serde_json::Value) -> Result<ModelPricing, PricingError> {
    let mut components = Vec::new();
    for (field, component) in [
        ("prompt", PriceComponent::Input),
        ("completion", PriceComponent::Output),
        ("input_cache_read", PriceComponent::CachedInput),
        ("input_cache_write", PriceComponent::CacheWrite),
        ("internal_reasoning", PriceComponent::Reasoning),
    ] {
        let Some(raw) = wire.get(field).and_then(serde_json::Value::as_str) else {
            continue;
        };
        let price = TokenPrice::parse_decimal(PriceCurrency::Usd, TokenPriceUnit::PerToken, raw)?;
        components.push((component, price));
    }
    pricing_from_components(
        provenance("https://openrouter.ai/api/v1/models", 1_700_000_000_123),
        components,
    )
}

#[test]
fn deepseek_catalog_shape_publishes_no_pricing_or_performance_evidence() {
    // DeepSeek's OpenAI-compatible `GET /models` row carries id/object/owned_by
    // only, so nothing may appear here until the provider publishes it.
    let provider = heycode_llm::DeepSeekProvider::from_key("test", None).unwrap();
    let descriptor = provider.describe_model("deepseek-v4-flash");
    assert!(descriptor.pricing.is_unknown());
    assert!(descriptor.performance.is_unknown());
}

#[test]
fn unknown_model_descriptor_carries_no_pricing_or_performance_evidence() {
    let descriptor = ModelDescriptor::unknown("provider/model");
    assert_eq!(descriptor.pricing, ModelPricing::unknown());
    assert_eq!(descriptor.performance, ModelPerformance::unknown());
    assert!(descriptor.pricing.is_unknown());
    assert!(descriptor.performance.is_unknown());
    assert_eq!(descriptor.performance.first_token_latency_ms(), None);
    assert_eq!(
        descriptor
            .performance
            .output_throughput_milli_tokens_per_second(),
        None
    );
    assert_eq!(descriptor.performance.observed_at_ms(), None);
}

#[test]
fn observed_performance_requires_evidence_and_rejects_absurd_values() {
    let observed = ModelPerformance::observed(
        provenance("benchmark:test", 1_700_000_000_000),
        Some(420),
        Some(87_500),
    )
    .unwrap();
    assert!(!observed.is_unknown());
    assert_eq!(observed.first_token_latency_ms(), Some(420));
    assert_eq!(
        observed.output_throughput_milli_tokens_per_second(),
        Some(87_500)
    );
    assert_eq!(observed.observed_at_ms(), Some(1_700_000_000_000));

    // One field is enough, but an observation with nothing observed is not
    // evidence and must use the explicit unknown constructor instead.
    assert!(
        ModelPerformance::observed(
            provenance("benchmark:test", 1_700_000_000_000),
            Some(420),
            None
        )
        .is_ok()
    );
    assert_eq!(
        ModelPerformance::observed(provenance("benchmark:test", 1_700_000_000_000), None, None),
        Err(PerformanceError::NoObservation)
    );

    // Zero and absurd measurements are rejected rather than stored.
    assert_eq!(
        ModelPerformance::observed(provenance("benchmark:test", 1), Some(0), None),
        Err(PerformanceError::InvalidLatency)
    );
    assert_eq!(
        ModelPerformance::observed(provenance("benchmark:test", 1), Some(3_600_001), None),
        Err(PerformanceError::InvalidLatency)
    );
    assert_eq!(
        ModelPerformance::observed(provenance("benchmark:test", 1), None, Some(0)),
        Err(PerformanceError::InvalidThroughput)
    );
    assert_eq!(
        ModelPerformance::observed(provenance("benchmark:test", 1), None, Some(100_000_001)),
        Err(PerformanceError::InvalidThroughput)
    );
}

fn provider_descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        id: "provider".to_owned(),
        display_name: "Provider".to_owned(),
        protocols: vec![ProviderProtocol::OpenAiChatCompletions],
    }
}

fn resolve_spec() -> ResolveSpec {
    ResolveSpec {
        protocol: ProviderProtocol::OpenAiChatCompletions,
        target: InferenceTarget::Http {
            base_url: "https://example.test/v1".to_owned(),
        },
        authentication: AuthenticationBinding::AdapterOwned(AdapterOwnedAuth::new()),
        default_max_output_tokens: Some(1_024),
        reasoning_efforts: vec![ReasoningEffortId::new("low").unwrap()],
        default_reasoning_effort: Some(ReasoningEffortId::new("low").unwrap()),
    }
}

fn resolvable_draft() -> RequestDraft {
    RequestDraft {
        provider: "provider".to_owned(),
        model: "provider/model".to_owned(),
        catalog_revision: Some(7),
        catalog_fetched_at_ms: Some(1_000),
        effective_at_ms: 2_000,
        system: Some("system".to_owned()),
        inputs: vec![InferenceInput::Message(ChatMessage::user("hello"))],
        tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options: Vec::new(),
        temperature: Some(0.2),
        max_output_tokens: None,
        purpose: CallPurpose::Conversation,
    }
}

fn descriptor_with(
    lifecycle: ModelLifecycle,
    capabilities: ModelCapabilities,
    pricing: ModelPricing,
    performance: ModelPerformance,
) -> ModelDescriptor {
    ModelDescriptor {
        id: "provider/model".to_owned(),
        display_name: "Model".to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(16_384),
        max_output_tokens: Some(2_048),
        lifecycle,
        capabilities,
        pricing,
        performance,
        reasoning: None,
    }
}

fn rich_pricing() -> ModelPricing {
    ModelPricing::captured(
        provenance("catalog:test", 1_700_000_000_000),
        PriceComponent::Input,
        usd_per_token("0.0000002"),
    )
    .with(PriceComponent::Output, usd_per_token("0.0000011"))
    .unwrap()
}

fn rich_performance() -> ModelPerformance {
    ModelPerformance::observed(
        provenance("benchmark:test", 1_700_000_000_000),
        Some(120),
        Some(250_000),
    )
    .unwrap()
}

#[test]
fn advisory_pricing_and_performance_never_change_request_resolution() {
    let bare = descriptor_with(
        ModelLifecycle::stable(),
        ModelCapabilities::unknown(),
        ModelPricing::unknown(),
        ModelPerformance::unknown(),
    );
    let annotated = descriptor_with(
        ModelLifecycle::stable(),
        ModelCapabilities::unknown(),
        rich_pricing(),
        rich_performance(),
    );
    let recaptured = descriptor_with(
        ModelLifecycle::stable(),
        ModelCapabilities::unknown(),
        ModelPricing::captured(
            provenance("catalog:second-source", 1_800_000_000_000),
            PriceComponent::Input,
            usd_per_token("0.0000002"),
        )
        .with(PriceComponent::Output, usd_per_token("0.0000011"))
        .unwrap(),
        ModelPerformance::observed(
            provenance("benchmark:second-source", 1_800_000_000_001),
            Some(120),
            Some(250_000),
        )
        .unwrap(),
    );
    assert_ne!(bare, annotated);
    assert_ne!(annotated, recaptured);

    let from_bare = resolve_request(
        &provider_descriptor(),
        resolvable_draft(),
        &bare,
        &resolve_spec(),
    )
    .unwrap();
    let from_annotated = resolve_request(
        &provider_descriptor(),
        resolvable_draft(),
        &annotated,
        &resolve_spec(),
    )
    .unwrap();
    let from_recaptured = resolve_request(
        &provider_descriptor(),
        resolvable_draft(),
        &recaptured,
        &resolve_spec(),
    )
    .unwrap();
    assert_eq!(from_bare, from_annotated);
    assert_eq!(from_annotated, from_recaptured);
}

#[test]
fn advisory_performance_cannot_substitute_for_capability_or_lifecycle_evidence() {
    // Unproven tool support stays unproven no matter how fast or cheap the row is.
    let mut draft = resolvable_draft();
    draft.tools = vec![heycode_llm::ToolSpec {
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({"type": "object"}),
    }];
    let fast_unknown = descriptor_with(
        ModelLifecycle::stable(),
        ModelCapabilities::unknown(),
        rich_pricing(),
        rich_performance(),
    );
    assert_eq!(
        resolve_request(
            &provider_descriptor(),
            draft,
            &fast_unknown,
            &resolve_spec()
        ),
        Err(ResolveError::Unproven {
            provider: "provider".to_owned(),
            model: "provider/model".to_owned(),
            capability: RequestedCapability::Tools,
        })
    );

    // An explicitly unsupported capability is still distinctly unsupported.
    let mut denied = ModelCapabilities::unknown();
    denied.tools = CapabilitySupport::Unsupported;
    let mut tool_draft = resolvable_draft();
    tool_draft.tools = vec![heycode_llm::ToolSpec {
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({"type": "object"}),
    }];
    assert_eq!(
        resolve_request(
            &provider_descriptor(),
            tool_draft,
            &descriptor_with(
                ModelLifecycle::stable(),
                denied,
                rich_pricing(),
                rich_performance()
            ),
            &resolve_spec()
        ),
        Err(ResolveError::Unsupported {
            provider: "provider".to_owned(),
            model: "provider/model".to_owned(),
            capability: RequestedCapability::Tools,
        })
    );

    // A retired row stays unselectable however attractive its advisory data is.
    let retired = descriptor_with(
        ModelLifecycle::retired(Some(1_500), Vec::new()),
        ModelCapabilities::unknown(),
        rich_pricing(),
        rich_performance(),
    );
    assert!(!retired.lifecycle.is_selectable(2_000));
    assert!(matches!(
        resolve_request(
            &provider_descriptor(),
            resolvable_draft(),
            &retired,
            &resolve_spec()
        ),
        Err(ResolveError::RetiredModel { .. })
    ));
}

/// Minimal stand-in for the explicit schema-v1 row `heycode-catalog-file` will
/// write. Named fields only, converted by hand — never serde over the runtime
/// structs, and never reflection.
#[derive(Debug, PartialEq, Eq)]
struct WirePrice {
    component: String,
    currency: String,
    unit: String,
    pico_units: u64,
}

#[derive(Debug, PartialEq, Eq)]
struct WirePricing {
    provenance: ModelMetadataProvenance,
    components: Vec<WirePrice>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct WirePerformance {
    source: Option<String>,
    first_token_latency_ms: Option<u32>,
    output_throughput_milli_tps: Option<u32>,
    captured_at_ms: Option<u64>,
}

fn pricing_to_wire(pricing: &ModelPricing) -> Option<WirePricing> {
    Some(WirePricing {
        provenance: pricing.provenance()?.clone(),
        components: pricing
            .components()
            .map(|(component, price)| WirePrice {
                component: component.name().to_owned(),
                currency: price.currency().code().to_owned(),
                unit: price.unit().name().to_owned(),
                pico_units: price.pico_units(),
            })
            .collect(),
    })
}

fn pricing_from_wire(wire: Option<WirePricing>) -> Result<ModelPricing, PricingError> {
    let Some(wire) = wire else {
        return Ok(ModelPricing::unknown());
    };
    let mut components = Vec::new();
    for row in wire.components {
        let price = TokenPrice::from_pico_units(
            PriceCurrency::parse(&row.currency)?,
            TokenPriceUnit::parse(&row.unit)?,
            row.pico_units,
        )?;
        components.push((PriceComponent::parse(&row.component)?, price));
    }
    pricing_from_components(wire.provenance, components)
}

fn performance_to_wire(performance: &ModelPerformance) -> WirePerformance {
    WirePerformance {
        source: performance
            .provenance()
            .map(|provenance| provenance.source().to_owned()),
        first_token_latency_ms: performance.first_token_latency_ms(),
        output_throughput_milli_tps: performance.output_throughput_milli_tokens_per_second(),
        captured_at_ms: performance.observed_at_ms(),
    }
}

fn performance_from_wire(wire: WirePerformance) -> Result<ModelPerformance, String> {
    if wire.first_token_latency_ms.is_none()
        && wire.output_throughput_milli_tps.is_none()
        && wire.source.is_none()
        && wire.captured_at_ms.is_none()
    {
        return Ok(ModelPerformance::unknown());
    }
    let provenance = ModelMetadataProvenance::new(
        wire.source.unwrap_or_default(),
        wire.captured_at_ms.unwrap_or_default(),
    )
    .map_err(|_| "invalid performance provenance".to_owned())?;
    ModelPerformance::observed(
        provenance,
        wire.first_token_latency_ms,
        wire.output_throughput_milli_tps,
    )
    .map_err(|_| "invalid observed model performance".to_owned())
}

#[test]
fn pricing_and_performance_round_trip_through_an_explicit_wire_mapping() {
    let full = ModelPricing::captured(
        provenance("catalog:test", 1_700_000_000_000),
        PriceComponent::Input,
        usd_per_token("0.0000002"),
    )
    .with(PriceComponent::Output, usd_per_token("0.0000011"))
    .unwrap()
    .with(PriceComponent::CachedInput, usd_per_token("0.00000002"))
    .unwrap()
    .with(PriceComponent::CacheWrite, usd_per_token("0.00000025"))
    .unwrap()
    .with(PriceComponent::Reasoning, usd_per_token("0"))
    .unwrap();

    // Every component survives name/integer conversion in both directions.
    let wire = pricing_to_wire(&full).unwrap();
    assert_eq!(wire.components.len(), 5);
    assert_eq!(pricing_from_wire(Some(wire)).unwrap(), full);

    // Absence survives too: no rows means no evidence, not a free model.
    let empty = pricing_to_wire(&ModelPricing::unknown());
    assert!(empty.is_none());
    assert_eq!(pricing_from_wire(empty).unwrap(), ModelPricing::unknown());

    for performance in [
        ModelPerformance::unknown(),
        ModelPerformance::observed(
            provenance("benchmark:test", 1_700_000_000_000),
            Some(120),
            Some(250_000),
        )
        .unwrap(),
        ModelPerformance::observed(
            provenance("benchmark:test", 1_700_000_000_000),
            Some(120),
            None,
        )
        .unwrap(),
        ModelPerformance::observed(
            provenance("benchmark:test", 1_700_000_000_000),
            None,
            Some(250_000),
        )
        .unwrap(),
    ] {
        let wire = performance_to_wire(&performance);
        assert_eq!(performance_from_wire(wire).unwrap(), performance);
    }
}

#[test]
fn wire_mapping_fails_loud_on_unknown_or_corrupt_values_instead_of_downgrading() {
    let row = |component: &str, currency: &str, unit: &str, pico_units: u64| WirePrice {
        component: component.to_owned(),
        currency: currency.to_owned(),
        unit: unit.to_owned(),
        pico_units,
    };
    let pricing = |components| {
        Some(WirePricing {
            provenance: provenance("catalog:test", 1_700_000_000_000),
            components,
        })
    };

    // A newer file naming a component/currency/unit this build does not know
    // must fail the read, never silently drop the row or fall back.
    assert_eq!(
        pricing_from_wire(pricing(vec![row("audio_input", "USD", "per_token", 1)])),
        Err(PricingError::UnknownComponent {
            name: "audio_input".to_owned()
        })
    );
    assert_eq!(
        pricing_from_wire(pricing(vec![row("input", "EUR", "per_token", 1)])),
        Err(PricingError::UnknownCurrency {
            code: "EUR".to_owned()
        })
    );
    assert_eq!(
        pricing_from_wire(pricing(vec![row("input", "USD", "per_request", 1)])),
        Err(PricingError::UnknownUnit {
            name: "per_request".to_owned()
        })
    );

    // Corrupt integers and internally inconsistent files fail on read.
    assert_eq!(
        pricing_from_wire(pricing(vec![row("input", "USD", "per_token", u64::MAX,)])),
        Err(PricingError::AmountOutOfRange)
    );
    assert_eq!(
        pricing_from_wire(pricing(vec![
            row("input", "USD", "per_token", 1),
            row("input", "USD", "per_token", 2),
        ])),
        Err(PricingError::DuplicateComponent { component: "input" })
    );
    assert_eq!(
        pricing_from_wire(pricing(vec![
            row("input", "USD", "per_token", 1),
            row("output", "USD", "per_million_tokens", 2),
        ])),
        Err(PricingError::InconsistentDenomination {
            existing: "USD per_token".to_owned(),
            added: "USD per_million_tokens".to_owned(),
        })
    );

    // A performance row cannot claim an observation it does not carry, nor
    // carry measurements with no collection instant.
    assert_eq!(
        performance_from_wire(WirePerformance {
            source: Some("benchmark:test".to_owned()),
            captured_at_ms: Some(1_700_000_000_000),
            ..WirePerformance::default()
        }),
        Err("invalid observed model performance".to_owned())
    );
    assert_eq!(
        performance_from_wire(WirePerformance {
            first_token_latency_ms: Some(120),
            ..WirePerformance::default()
        }),
        Err("invalid performance provenance".to_owned())
    );
}

#[test]
fn derived_defaults_are_the_unknown_evidence_state() {
    // Nothing may default into a priced or measured state.
    assert_eq!(ModelPricing::default(), ModelPricing::unknown());
    assert_eq!(ModelPerformance::default(), ModelPerformance::unknown());
    assert!(ModelPricing::default().is_unknown());
    assert!(ModelPerformance::default().is_unknown());
}
