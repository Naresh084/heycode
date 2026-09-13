use heycode_core::{ProviderProtocol, ProviderStateItem, ProviderStateKind, TokenUsage};
use heycode_llm::{NativeCompactionCheckpoint, NativeCompactionError};

fn state(provider: &str, model: &str, protocol: ProviderProtocol) -> ProviderStateItem {
    ProviderStateItem::new(
        provider,
        model,
        protocol,
        ProviderStateKind::ResponseOutputItem,
        serde_json::json!({
            "type":"compaction",
            "id":"cmp_1",
            "encrypted_content":"opaque"
        }),
    )
    .unwrap()
}

#[test]
fn native_checkpoint_requires_one_exact_route_and_retains_usage() {
    let usage = TokenUsage {
        prompt_tokens: 41,
        completion_tokens: 7,
    };
    let checkpoint = NativeCompactionCheckpoint::new(
        vec![state(
            "openai",
            "gpt-5.6",
            ProviderProtocol::OpenAiResponses,
        )],
        Some(usage),
    )
    .unwrap();

    assert_eq!(checkpoint.provider(), "openai");
    assert_eq!(checkpoint.model(), "gpt-5.6");
    assert_eq!(checkpoint.protocol(), ProviderProtocol::OpenAiResponses);
    assert_eq!(checkpoint.items().len(), 1);
    assert_eq!(checkpoint.usage(), Some(usage));

    assert_eq!(
        NativeCompactionCheckpoint::new(Vec::new(), None).unwrap_err(),
        NativeCompactionError::InvalidCheckpoint
    );
    assert_eq!(
        NativeCompactionCheckpoint::new(
            vec![
                state("openai", "gpt-5.6", ProviderProtocol::OpenAiResponses),
                state("openai", "gpt-5.7", ProviderProtocol::OpenAiResponses),
            ],
            None,
        )
        .unwrap_err(),
        NativeCompactionError::InvalidCheckpoint
    );
}
