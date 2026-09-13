#![no_main]

use heycode_llm::{FinishReason, ProviderErrorClass, SseParser, StreamChunk};
use libfuzzer_sys::fuzz_target;

const MAX_INPUT_BYTES: usize = 32 * 1024;

#[derive(PartialEq, Eq)]
enum Observed {
    Text(String),
    Reasoning(String),
    Tool {
        index: u16,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    },
    Usage(u64, u64),
    Finish(FinishReason),
    Error(ProviderErrorClass),
}

fn parse(input: &[u8], chunk_size: usize) -> Vec<Result<StreamChunk, heycode_llm::LlmError>> {
    let mut parser = SseParser::new();
    let mut output = Vec::new();
    for chunk in input.chunks(chunk_size.max(1)) {
        output.extend(parser.feed(chunk));
    }
    output.extend(parser.finish());
    output
}

fn observe(items: &[Result<StreamChunk, heycode_llm::LlmError>]) -> Vec<Observed> {
    items
        .iter()
        .map(|item| match item {
            Ok(StreamChunk::TextDelta(text)) => Observed::Text(text.clone()),
            Ok(StreamChunk::ReasoningDelta(text)) => Observed::Reasoning(text.clone()),
            Ok(StreamChunk::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            }) => Observed::Tool {
                index: *index,
                id: id.clone(),
                name: name.clone(),
                arguments: arguments_delta.clone(),
            },
            Ok(StreamChunk::Usage(usage)) => {
                Observed::Usage(usage.prompt_tokens, usage.completion_tokens)
            }
            Ok(StreamChunk::Finish(reason)) => Observed::Finish(*reason),
            Err(error) => Observed::Error(error.class()),
        })
        .collect()
}

fn assert_terminal_contract(items: &[Result<StreamChunk, heycode_llm::LlmError>]) {
    let mut usage = Vec::new();
    let mut finish = Vec::new();
    for (index, item) in items.iter().enumerate() {
        match item {
            Ok(StreamChunk::Usage(_)) => usage.push(index),
            Ok(StreamChunk::Finish(_)) => finish.push(index),
            _ => {}
        }
    }
    assert!(
        usage.len() <= 1,
        "provider parser must emit at most one usage item"
    );
    assert!(
        finish.len() <= 1,
        "provider parser must emit at most one finish item"
    );
    if let Some(finish_index) = finish.first().copied() {
        assert!(
            finish_index + 1 == items.len(),
            "provider finish must be the terminal parser output"
        );
        if let Some(usage_index) = usage.first().copied() {
            assert!(
                usage_index + 1 == finish_index,
                "provider usage must sit immediately before finish"
            );
        }
    }
}

fuzz_target!(|input: &[u8]| {
    let input = &input[..input.len().min(MAX_INPUT_BYTES)];
    let chunk_size = input
        .first()
        .map_or(1, |byte| usize::from(*byte % 64).saturating_add(1));

    let fragmented = parse(input, chunk_size);
    let repeated = parse(input, chunk_size);
    assert!(
        observe(&fragmented) == observe(&repeated),
        "provider parsing must be deterministic for one fragmentation plan"
    );
    assert!(
        fragmented.len() <= input.len().saturating_add(2),
        "provider parser output must remain bounded by input work"
    );
    assert_terminal_contract(&fragmented);

    let whole = parse(input, input.len().max(1));
    assert_terminal_contract(&whole);
    let whole_observed = observe(&whole);
    let fragmented_observed = observe(&fragmented);
    let both_successful = !whole_observed
        .iter()
        .chain(fragmented_observed.iter())
        .any(|item| matches!(item, Observed::Error(_)));
    if both_successful {
        assert!(
            whole_observed == fragmented_observed,
            "successful provider parsing must be invariant to raw byte fragmentation"
        );
    }
});
