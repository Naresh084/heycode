//! Exact/estimated token counting registry contracts (P11).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use heycode_core::Context;
use heycode_llm::{
    ChatImage, ChatMessage, ChatRequest, CountableContent, EstimationMethod,
    HeuristicTokenEstimator, Role, TokenCount, TokenCountError, TokenCountFailure,
    TokenCountFailureKind, TokenCountRequest, TokenCounter, TokenCounterDescriptor, TokenCounterId,
    TokenCounterRegistry, TokenCounterScope, TokenEvidence, ToolSpec,
};
use tokio_util::sync::CancellationToken;

fn id(value: &str) -> TokenCounterId {
    TokenCounterId::new(value).unwrap()
}

fn message(role: Role, content: &str) -> ChatMessage {
    ChatMessage {
        role,
        content: content.to_owned(),
        images: Vec::new(),
        documents: Vec::new(),
        tool_calls: None,
        tool_call_id: None,
        tool_result_is_error: None,
    }
}

/// A counter whose number, declared evidence, scope and outcome are scripted,
/// so tests pin registry behaviour rather than any particular implementation.
struct ScriptedCounter {
    descriptor: TokenCounterDescriptor,
    outcome: Result<u64, TokenCountFailure>,
    calls: Arc<AtomicUsize>,
}

impl ScriptedCounter {
    fn scripted(
        counter_id: &str,
        evidence: TokenEvidence,
        scope: TokenCounterScope,
        outcome: Result<u64, TokenCountFailure>,
    ) -> (Arc<dyn TokenCounter>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Self {
            descriptor: TokenCounterDescriptor::new(id(counter_id), evidence, scope).unwrap(),
            outcome,
            calls: Arc::clone(&calls),
        };
        (Arc::new(counter), calls)
    }
}

#[async_trait]
impl TokenCounter for ScriptedCounter {
    fn descriptor(&self) -> TokenCounterDescriptor {
        self.descriptor.clone()
    }

    async fn count(
        &self,
        request: &TokenCountRequest<'_>,
        _cancellation: &CancellationToken,
    ) -> Result<u64, TokenCountFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // A counter asked for a target it never claimed says so; it never
        // guesses a number for a model it does not know.
        if !self
            .descriptor
            .scope()
            .serves(request.provider(), request.model())
        {
            return Err(TokenCountFailure::unsupported(
                "counter does not serve target",
            ));
        }
        self.outcome.clone()
    }
}

fn counted(registry: &TokenCounterRegistry, provider: &str, model: &str) -> TokenCount {
    let request = TokenCountRequest::new(provider, model)
        .unwrap()
        .with_text("hello");
    let cancellation = CancellationToken::new();
    futures::executor::block_on(registry.count(&request, &cancellation))
        .unwrap()
        .into_count()
}

#[test]
fn an_estimate_can_never_be_read_as_an_exact_count() {
    // `TokenCount` has no accessor that yields a number without stating which
    // kind of evidence produced it: `exact()` and `estimated()` are the only
    // ways in, and they return distinct types.
    let registry = TokenCounterRegistry::new();
    let context = Context::new();
    let (estimator, _) = ScriptedCounter::scripted(
        "estimator",
        TokenEvidence::Estimated(EstimationMethod::Utf8ByteRatio),
        TokenCounterScope::Any,
        Ok(11),
    );
    registry.register(&context, estimator).unwrap();

    let count = counted(&registry, "p", "m");
    assert!(count.exact().is_none());
    let estimate = count.estimated().expect("estimated arm");
    assert_eq!(estimate.tokens(), 11);
    assert_eq!(estimate.method(), EstimationMethod::Utf8ByteRatio);
    assert_eq!(estimate.counter(), &id("estimator"));
    assert_eq!(
        count.evidence(),
        TokenEvidence::Estimated(EstimationMethod::Utf8ByteRatio)
    );
    assert!(!count.evidence().is_exact());

    let exact_registry = TokenCounterRegistry::new();
    let (exact, _) = ScriptedCounter::scripted(
        "exact",
        TokenEvidence::Exact,
        TokenCounterScope::Any,
        Ok(11),
    );
    exact_registry.register(&context, exact).unwrap();
    let exact_count = counted(&exact_registry, "p", "m");
    assert!(exact_count.estimated().is_none());
    assert_eq!(exact_count.exact().expect("exact arm").tokens(), 11);
    assert!(exact_count.evidence().is_exact());

    // Same number, different evidence: the two counts are not interchangeable.
    assert_ne!(count, exact_count);
}

#[test]
fn exactness_comes_from_the_declaration_not_from_the_returned_number() {
    // Two counters return the identical number; only the declared evidence
    // decides which one produced an exact count.
    let context = Context::new();
    for (evidence, expect_exact) in [
        (TokenEvidence::Exact, true),
        (
            TokenEvidence::Estimated(EstimationMethod::ProviderTokenizer),
            false,
        ),
        (
            TokenEvidence::Estimated(EstimationMethod::Utf8ByteRatio),
            false,
        ),
    ] {
        let registry = TokenCounterRegistry::new();
        let (counter, _) =
            ScriptedCounter::scripted("counter", evidence, TokenCounterScope::Any, Ok(7));
        registry.register(&context, counter).unwrap();
        let count = counted(&registry, "p", "m");
        assert_eq!(count.exact().is_some(), expect_exact);
        assert_eq!(count.estimated().is_some(), !expect_exact);
    }
}

#[test]
fn evidence_ranks_exact_ahead_of_every_estimation_method() {
    // Declaration order is the rank, so a better method added later sorts
    // ahead of the ratio without any placeholder being reserved for it.
    assert!(TokenEvidence::Exact < TokenEvidence::Estimated(EstimationMethod::Utf8ByteRatio));
    assert!(
        TokenEvidence::Estimated(EstimationMethod::ProviderTokenizer)
            < TokenEvidence::Estimated(EstimationMethod::Utf8ByteRatio)
    );
    assert!(TokenEvidence::Exact.is_exact());
    assert!(!TokenEvidence::Estimated(EstimationMethod::ProviderTokenizer).is_exact());
    assert!(!TokenEvidence::Estimated(EstimationMethod::Utf8ByteRatio).is_exact());
}

#[test]
fn selection_prefers_declared_evidence_over_registration_order() {
    let context = Context::new();
    let registry = TokenCounterRegistry::new();
    // The worse counter registers first and has the lexicographically smaller
    // id, so only evidence rank can decide this.
    let (estimate, estimate_calls) = ScriptedCounter::scripted(
        "aaa-estimate",
        TokenEvidence::Estimated(EstimationMethod::Utf8ByteRatio),
        TokenCounterScope::Any,
        Ok(1),
    );
    let (exact, exact_calls) = ScriptedCounter::scripted(
        "zzz-exact",
        TokenEvidence::Exact,
        TokenCounterScope::Any,
        Ok(2),
    );
    registry.register(&context, estimate).unwrap();
    registry.register(&context, exact).unwrap();

    let candidates = registry.candidates("p", "m").unwrap();
    assert_eq!(
        candidates
            .iter()
            .map(|descriptor| descriptor.id().as_str().to_owned())
            .collect::<Vec<_>>(),
        ["zzz-exact", "aaa-estimate"]
    );

    let count = counted(&registry, "p", "m");
    assert_eq!(count.exact().expect("exact wins").tokens(), 2);
    assert_eq!(exact_calls.load(Ordering::SeqCst), 1);
    assert_eq!(estimate_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn equal_evidence_ties_break_by_stable_id_not_registration_order() {
    let context = Context::new();
    let mut winners = Vec::new();
    for order in [["zeta", "alpha"], ["alpha", "zeta"]] {
        let registry = TokenCounterRegistry::new();
        for name in order {
            let (counter, _) = ScriptedCounter::scripted(
                name,
                TokenEvidence::Estimated(EstimationMethod::Utf8ByteRatio),
                TokenCounterScope::Any,
                Ok(3),
            );
            registry.register(&context, counter).unwrap();
        }
        assert_eq!(
            registry
                .candidates("p", "m")
                .unwrap()
                .iter()
                .map(|descriptor| descriptor.id().as_str().to_owned())
                .collect::<Vec<_>>(),
            ["alpha", "zeta"],
            "registration order {order:?} changed candidate order"
        );
        winners.push(
            counted(&registry, "p", "m")
                .estimated()
                .expect("estimated")
                .counter()
                .clone(),
        );
    }
    assert_eq!(winners, [id("alpha"), id("alpha")]);
}

#[test]
fn a_narrower_scope_never_outranks_better_evidence() {
    let context = Context::new();
    let registry = TokenCounterRegistry::new();
    let (model_estimate, _) = ScriptedCounter::scripted(
        "model-estimate",
        TokenEvidence::Estimated(EstimationMethod::Utf8ByteRatio),
        TokenCounterScope::model("p", "m").unwrap(),
        Ok(5),
    );
    let (provider_exact, _) = ScriptedCounter::scripted(
        "provider-exact",
        TokenEvidence::Exact,
        TokenCounterScope::provider("p").unwrap(),
        Ok(6),
    );
    registry.register(&context, model_estimate).unwrap();
    registry.register(&context, provider_exact).unwrap();

    let count = counted(&registry, "p", "m");
    assert_eq!(count.exact().expect("exact wins").tokens(), 6);
}

#[test]
fn out_of_scope_counters_are_never_selected_and_refuse_when_asked_directly() {
    let context = Context::new();
    let registry = TokenCounterRegistry::new();
    let (scoped, calls) = ScriptedCounter::scripted(
        "deepseek-exact",
        TokenEvidence::Exact,
        TokenCounterScope::provider("deepseek").unwrap(),
        Ok(99),
    );
    registry.register(&context, Arc::clone(&scoped)).unwrap();

    // Selection filters by declared scope before any counter runs.
    assert!(registry.candidates("openrouter", "glm").unwrap().is_empty());
    assert_eq!(
        registry
            .candidates("deepseek", "anything")
            .unwrap()
            .iter()
            .map(|descriptor| descriptor.id().as_str().to_owned())
            .collect::<Vec<_>>(),
        ["deepseek-exact"]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    // Asked directly for a target it never claimed, it refuses distinctly
    // instead of returning a number.
    let request = TokenCountRequest::new("openrouter", "glm").unwrap();
    let failure = futures::executor::block_on(scoped.count(&request, &CancellationToken::new()))
        .expect_err("out-of-scope count must refuse");
    assert_eq!(failure.kind(), TokenCountFailureKind::Unsupported);
}

#[test]
fn a_target_no_counter_serves_fails_loud_instead_of_guessing() {
    let context = Context::new();
    let registry = TokenCounterRegistry::new();
    let (scoped, _) = ScriptedCounter::scripted(
        "deepseek-exact",
        TokenEvidence::Exact,
        TokenCounterScope::provider("deepseek").unwrap(),
        Ok(99),
    );
    registry.register(&context, scoped).unwrap();

    let request = TokenCountRequest::new("openrouter", "glm").unwrap();
    let error = futures::executor::block_on(registry.count(&request, &CancellationToken::new()))
        .expect_err("no counter serves this target");
    assert_eq!(
        error,
        TokenCountError::NoCounter {
            provider: "openrouter".to_owned(),
            model: "glm".to_owned(),
        }
    );
}

#[test]
fn an_unsupported_refusal_falls_through_to_the_next_rank_visibly() {
    let context = Context::new();
    let registry = TokenCounterRegistry::new();
    let (exact, exact_calls) = ScriptedCounter::scripted(
        "exact",
        TokenEvidence::Exact,
        TokenCounterScope::Any,
        Err(TokenCountFailure::unsupported("cannot count images")),
    );
    let (estimate, estimate_calls) = ScriptedCounter::scripted(
        "estimate",
        TokenEvidence::Estimated(EstimationMethod::Utf8ByteRatio),
        TokenCounterScope::Any,
        Ok(4),
    );
    registry.register(&context, exact).unwrap();
    registry.register(&context, estimate).unwrap();

    let request = TokenCountRequest::new("p", "m").unwrap().with_text("x");
    let outcome =
        futures::executor::block_on(registry.count(&request, &CancellationToken::new())).unwrap();
    assert_eq!(exact_calls.load(Ordering::SeqCst), 1);
    assert_eq!(estimate_calls.load(Ordering::SeqCst), 1);
    assert_eq!(outcome.count().estimated().expect("estimate").tokens(), 4);
    // The fallback is reported, never silent.
    assert_eq!(
        outcome
            .refused()
            .iter()
            .map(|refusal| refusal.counter().as_str().to_owned())
            .collect::<Vec<_>>(),
        ["exact"]
    );
    assert_eq!(outcome.refused()[0].message(), "cannot count images");

    // Every eligible counter refusing is a distinct failure, not a number.
    let all_refuse = TokenCounterRegistry::new();
    let (only, _) = ScriptedCounter::scripted(
        "only",
        TokenEvidence::Exact,
        TokenCounterScope::Any,
        Err(TokenCountFailure::unsupported("no")),
    );
    all_refuse.register(&context, only).unwrap();
    let error = futures::executor::block_on(all_refuse.count(&request, &CancellationToken::new()))
        .expect_err("every counter refused");
    assert!(matches!(
        error,
        TokenCountError::Unsupported {
            ref provider,
            ref model,
            ref refusals,
        } if provider == "p" && model == "m" && refusals.len() == 1
    ));
}

#[test]
fn a_counter_failure_or_cancellation_never_degrades_into_an_estimate() {
    let context = Context::new();
    for (outcome, expected) in [
        (
            TokenCountFailure::failed("count endpoint rejected the request"),
            TokenCountError::CounterFailed {
                counter: id("exact"),
                message: "count endpoint rejected the request".to_owned(),
            },
        ),
        (
            TokenCountFailure::cancelled(),
            TokenCountError::Cancelled {
                counter: id("exact"),
            },
        ),
    ] {
        let registry = TokenCounterRegistry::new();
        let (exact, _) = ScriptedCounter::scripted(
            "exact",
            TokenEvidence::Exact,
            TokenCounterScope::Any,
            Err(outcome),
        );
        let (estimate, estimate_calls) = ScriptedCounter::scripted(
            "estimate",
            TokenEvidence::Estimated(EstimationMethod::Utf8ByteRatio),
            TokenCounterScope::Any,
            Ok(4),
        );
        registry.register(&context, exact).unwrap();
        registry.register(&context, estimate).unwrap();

        let request = TokenCountRequest::new("p", "m").unwrap().with_text("x");
        let error =
            futures::executor::block_on(registry.count(&request, &CancellationToken::new()))
                .expect_err("a failed exact count must not become an estimate");
        assert_eq!(error, expected);
        assert_eq!(estimate_calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn registration_disposal_removes_exactly_that_counter() {
    let registry = TokenCounterRegistry::new();
    let mut owned = Context::new();
    let other = Context::new();

    let (first, _) = ScriptedCounter::scripted(
        "first",
        TokenEvidence::Estimated(EstimationMethod::Utf8ByteRatio),
        TokenCounterScope::Any,
        Ok(1),
    );
    let (second, _) = ScriptedCounter::scripted(
        "second",
        TokenEvidence::Estimated(EstimationMethod::Utf8ByteRatio),
        TokenCounterScope::Any,
        Ok(2),
    );
    registry.register(&owned, first).unwrap();
    registry.register(&other, second).unwrap();
    assert_eq!(
        registry
            .descriptors()
            .unwrap()
            .iter()
            .map(|descriptor| descriptor.id().as_str().to_owned())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );

    owned.shutdown();
    assert_eq!(
        registry
            .descriptors()
            .unwrap()
            .iter()
            .map(|descriptor| descriptor.id().as_str().to_owned())
            .collect::<Vec<_>>(),
        ["second"]
    );
}

#[test]
fn a_disposed_id_is_free_again_and_the_re_registered_row_is_the_live_one() {
    let registry = TokenCounterRegistry::new();
    let live = Context::new();
    let (replacement, _) = ScriptedCounter::scripted(
        "shared",
        TokenEvidence::Exact,
        TokenCounterScope::Any,
        Ok(9),
    );

    let mut first = Context::new();
    let (original, original_calls) = ScriptedCounter::scripted(
        "shared",
        TokenEvidence::Estimated(EstimationMethod::Utf8ByteRatio),
        TokenCounterScope::Any,
        Ok(1),
    );
    registry.register(&first, original).unwrap();
    // The id is taken until its owner disposes it.
    assert!(matches!(
        registry.register(&live, Arc::clone(&replacement)),
        Err(TokenCountError::DuplicateCounter { .. })
    ));
    first.shutdown();

    registry.register(&live, replacement).unwrap();
    let descriptors = registry.descriptors().unwrap();
    assert_eq!(descriptors.len(), 1);
    assert_eq!(descriptors[0].id().as_str(), "shared");
    assert_eq!(descriptors[0].evidence(), TokenEvidence::Exact);
    assert_eq!(counted(&registry, "p", "m").exact().unwrap().tokens(), 9);
    // The disposed counter is gone, not merely shadowed.
    assert_eq!(original_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn duplicate_counter_ids_fail_registration() {
    let context = Context::new();
    let registry = TokenCounterRegistry::new();
    for _ in 0..1 {
        let (counter, _) =
            ScriptedCounter::scripted("dup", TokenEvidence::Exact, TokenCounterScope::Any, Ok(1));
        registry.register(&context, counter).unwrap();
    }
    let (again, _) = ScriptedCounter::scripted(
        "dup",
        TokenEvidence::Estimated(EstimationMethod::Utf8ByteRatio),
        TokenCounterScope::Any,
        Ok(2),
    );
    assert_eq!(
        registry.register(&context, again),
        Err(TokenCountError::DuplicateCounter { id: id("dup") })
    );
}

#[test]
fn counter_ids_and_scopes_reject_blank_or_malformed_values() {
    for bad in ["", " ", "Upper", "has space", "sym#bol", &"a".repeat(65)] {
        assert!(
            TokenCounterId::new(bad).is_err(),
            "`{bad}` must not be a valid counter id"
        );
    }
    assert_eq!(
        id("local-utf8-byte-ratio").as_str(),
        "local-utf8-byte-ratio"
    );
    assert!(TokenCounterScope::provider("").is_err());
    assert!(TokenCounterScope::model("p", " ").is_err());
    assert!(TokenCountRequest::new("", "m").is_err());
    assert!(TokenCountRequest::new("p", "").is_err());
}

#[test]
fn scope_membership_is_exact() {
    let any = TokenCounterScope::Any;
    let provider = TokenCounterScope::provider("deepseek").unwrap();
    let model = TokenCounterScope::model("deepseek", "deepseek-v4-pro").unwrap();

    assert!(any.serves("anything", "anything"));
    assert!(provider.serves("deepseek", "deepseek-v4-pro"));
    assert!(!provider.serves("deepseek-x", "deepseek-v4-pro"));
    assert!(model.serves("deepseek", "deepseek-v4-pro"));
    assert!(!model.serves("deepseek", "deepseek-v4-flash"));
}

#[test]
fn heuristic_estimator_declares_an_estimate_and_never_exactness() {
    let estimator = HeuristicTokenEstimator::new();
    let descriptor = estimator.descriptor();
    assert_eq!(descriptor.id().as_str(), HeuristicTokenEstimator::ID);
    assert_eq!(
        descriptor.evidence(),
        TokenEvidence::Estimated(EstimationMethod::Utf8ByteRatio)
    );
    assert!(!descriptor.evidence().is_exact());
    assert_eq!(descriptor.scope(), &TokenCounterScope::Any);
}

#[test]
fn heuristic_estimator_counts_empty_ascii_and_multibyte_input_as_documented() {
    let estimator = HeuristicTokenEstimator::new();
    // ceil(utf-8 bytes / 4), summed over every countable part.
    for (text, expected) in [
        ("", 0_u64),
        ("a", 1),
        ("abcd", 1),
        ("abcde", 2),
        ("hello world", 3),
        // Multibyte text is counted by its UTF-8 bytes, not its characters:
        // three CJK characters are nine bytes, so three tokens, where a
        // character ratio would have claimed zero.
        ("\u{65e5}\u{672c}\u{8a9e}", 3),
        // A combining sequence keeps every byte it will actually send.
        ("e\u{301}", 1),
        ("\u{1f600}", 1),
    ] {
        let request = TokenCountRequest::new("p", "m").unwrap().with_text(text);
        let estimate = estimator.estimate(&request).unwrap();
        assert_eq!(estimate.tokens(), expected, "text {text:?}");
        assert_eq!(estimate.method(), EstimationMethod::Utf8ByteRatio);
    }

    // An empty request is zero tokens, not one and not an error.
    let empty = TokenCountRequest::new("p", "m").unwrap();
    assert_eq!(estimator.estimate(&empty).unwrap().tokens(), 0);
}

#[test]
fn heuristic_estimator_is_deterministic_and_bounded_on_very_large_input() {
    let estimator = HeuristicTokenEstimator::new();
    let ascii = "a".repeat(1024 * 1024);
    let request = TokenCountRequest::new("p", "m").unwrap().with_text(&ascii);
    let first = estimator.estimate(&request).unwrap();
    let second = estimator.estimate(&request).unwrap();
    assert_eq!(first.tokens(), 262_144);
    assert_eq!(first.tokens(), second.tokens());

    // Many parts sum before the single rounding step, so splitting the same
    // bytes across parts cannot change the answer.
    let split = TokenCountRequest::new("p", "m")
        .unwrap()
        .with_text(&ascii[..512 * 1024])
        .with_text(&ascii[512 * 1024..]);
    assert_eq!(estimator.estimate(&split).unwrap().tokens(), 262_144);

    let multibyte = "\u{65e5}".repeat(100_000);
    let wide = TokenCountRequest::new("p", "m")
        .unwrap()
        .with_text(&multibyte);
    assert_eq!(estimator.estimate(&wide).unwrap().tokens(), 75_000);
}

#[test]
fn heuristic_estimator_refuses_media_it_cannot_honestly_count() {
    let estimator = HeuristicTokenEstimator::new();
    let mut with_image = message(Role::User, "look at this");
    with_image.images.push(
        ChatImage::new(
            heycode_core::AttachmentMediaType::new("image/png").unwrap(),
            vec![1, 2, 3, 4],
        )
        .unwrap(),
    );
    let request = TokenCountRequest::new("p", "m")
        .unwrap()
        .with_message(&with_image);
    let failure = estimator
        .estimate(&request)
        .expect_err("a byte ratio cannot count an image");
    assert_eq!(failure.kind(), TokenCountFailureKind::Unsupported);

    // Through the registry the same refusal is a distinct error, never a number.
    let context = Context::new();
    let registry = TokenCounterRegistry::new();
    registry
        .register(&context, Arc::new(HeuristicTokenEstimator::new()))
        .unwrap();
    let error = futures::executor::block_on(registry.count(&request, &CancellationToken::new()))
        .expect_err("registry must surface the refusal");
    assert!(matches!(error, TokenCountError::Unsupported { .. }));
}

#[test]
fn counting_a_chat_request_covers_system_text_tools_and_tool_calls() {
    let estimator = HeuristicTokenEstimator::new();
    let request = ChatRequest {
        model: "m".to_owned(),
        messages: vec![
            message(Role::System, "abcd"),
            message(Role::User, "efgh"),
            ChatMessage {
                tool_calls: Some(vec![heycode_llm::ChatToolCall {
                    id: "call_1".to_owned(),
                    name: "read".to_owned(),
                    arguments: "{\"path\":\"x\"}".to_owned(),
                }]),
                ..message(Role::Assistant, "")
            },
        ],
        tools: Some(vec![ToolSpec {
            name: "read".to_owned(),
            description: "Read a file".to_owned(),
            parameters: serde_json::json!({"type":"object"}),
        }]),
        temperature: None,
        max_tokens: None,
    };
    let countable = TokenCountRequest::for_chat_request("p", &request).unwrap();
    assert_eq!(countable.provider(), "p");
    assert_eq!(countable.model(), "m");
    // Messages in transcript order, then every offered tool definition.
    assert!(matches!(
        countable.content(),
        [
            CountableContent::Message(_),
            CountableContent::Message(_),
            CountableContent::Message(_),
            CountableContent::Tool(_),
        ]
    ));

    // Everything the request will send contributes: message text, tool-call
    // identity and arguments, and each tool definition with its schema.
    let bytes = 4
        + 4
        + "call_1".len()
        + "read".len()
        + "{\"path\":\"x\"}".len()
        + "read".len()
        + "Read a file".len()
        + serde_json::to_string(&serde_json::json!({"type":"object"}))
            .unwrap()
            .len();
    let expected = bytes.div_ceil(4) as u64;
    assert_eq!(estimator.estimate(&countable).unwrap().tokens(), expected);

    // Dropping the tools drops their tokens, so tools are really counted.
    let without_tools = ChatRequest {
        tools: None,
        ..request.clone()
    };
    let smaller = TokenCountRequest::for_chat_request("p", &without_tools).unwrap();
    assert!(estimator.estimate(&smaller).unwrap().tokens() < expected);
}

#[test]
fn a_registered_heuristic_serves_every_provider_and_model() {
    let context = Context::new();
    let registry = TokenCounterRegistry::new();
    registry
        .register(&context, Arc::new(HeuristicTokenEstimator::new()))
        .unwrap();

    for (provider, model) in [("deepseek", "deepseek-v4-pro"), ("openrouter", "glm")] {
        let request = TokenCountRequest::new(provider, model)
            .unwrap()
            .with_text("abcd");
        let outcome =
            futures::executor::block_on(registry.count(&request, &CancellationToken::new()))
                .unwrap();
        let estimate = outcome.count().estimated().expect("estimate");
        assert_eq!(estimate.tokens(), 1);
        assert_eq!(estimate.counter().as_str(), HeuristicTokenEstimator::ID);
        assert!(outcome.refused().is_empty());
    }
}
