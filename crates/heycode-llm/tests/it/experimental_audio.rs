//! ATT04 hidden protocol-neutral audio vocabulary and evidence fixtures.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::{
    AttachmentAudioMetadata, AttachmentContentId, AttachmentMediaType, AttachmentMetadata,
    ProviderProtocol,
};
use heycode_llm::{
    AdapterOwnedAuth, AuthenticationBinding, CapabilitySupport, ChatMessage, ChatRequest,
    ContributorTokens, EnvelopeContributor, ExperimentalAudioDescriptor, ExperimentalAudioError,
    ExperimentalAudioInput, ExperimentalAudioOutput, ExperimentalAudioRequest,
    ExperimentalFeatureVisibility, HeuristicTokenEstimator, InferenceTarget, ModelDescriptor,
    TokenCounterRegistry, UncountedReason, measure_experimental_audio_envelope,
    resolve_experimental_audio,
};

fn metadata(bytes: &[u8]) -> AttachmentMetadata {
    use sha2::{Digest as _, Sha256};
    AttachmentMetadata::new_audio(
        AttachmentContentId::from_sha256(Sha256::digest(bytes).into()),
        AttachmentMediaType::new("audio/wav").unwrap(),
        u64::try_from(bytes.len()).unwrap(),
        Some("sample.wav".to_owned()),
        AttachmentAudioMetadata::new(1, 8_000, 1, 16).unwrap(),
    )
    .unwrap()
}

fn descriptor(input: CapabilitySupport, output: CapabilitySupport) -> ExperimentalAudioDescriptor {
    ExperimentalAudioDescriptor::new(
        "fixture",
        "fixture-audio-model",
        ProviderProtocol::OpenAiChatCompletions,
        InferenceTarget::Http {
            base_url: "https://audio.example.test/v1".to_owned(),
        },
        AuthenticationBinding::AdapterOwned(AdapterOwnedAuth::new()),
    )
    .unwrap()
    .with_input(input, ["audio/wav"], 4, 32 * 1024 * 1024)
    .unwrap()
    .with_output(output, ["audio/wav"])
    .unwrap()
}

fn request(bytes: Vec<u8>) -> ExperimentalAudioRequest {
    let attachment = metadata(&bytes);
    ExperimentalAudioRequest::new(
        ChatRequest {
            model: "fixture-audio-model".to_owned(),
            messages: vec![ChatMessage::user("transcribe exactly")],
            tools: None,
            temperature: None,
            max_tokens: Some(64),
        },
        vec![ExperimentalAudioInput::new(0, attachment, bytes).unwrap()],
    )
    .unwrap()
}

#[test]
fn hidden_descriptor_resolves_only_exact_supported_model_and_format() {
    let descriptor = descriptor(CapabilitySupport::Supported, CapabilitySupport::Unknown);
    assert_eq!(
        descriptor.visibility(),
        ExperimentalFeatureVisibility::Hidden
    );
    let model = ModelDescriptor::unknown("fixture-audio-model");
    let call = resolve_experimental_audio(
        &descriptor,
        request(vec![0; 44]),
        &model,
        Some(7),
        Some(8),
        9,
    )
    .unwrap();
    assert_eq!(call.provider(), "fixture");
    assert_eq!(call.model(), "fixture-audio-model");
    assert_eq!(call.inputs()[0].bytes(), &[0; 44]);
    assert_eq!(call.inputs()[0].message_index(), 0);

    let wrong = ModelDescriptor::unknown("other-model");
    assert!(matches!(
        resolve_experimental_audio(
            &descriptor,
            request(vec![0; 44]),
            &wrong,
            Some(7),
            Some(8),
            9,
        ),
        Err(ExperimentalAudioError::ModelMismatch { .. })
    ));
}

#[test]
fn unsupported_and_unknown_input_evidence_refuse_distinctly() {
    let model = ModelDescriptor::unknown("fixture-audio-model");
    for (support, expected) in [
        (CapabilitySupport::Unsupported, "unsupported"),
        (CapabilitySupport::Unknown, "unproven"),
    ] {
        let error = resolve_experimental_audio(
            &descriptor(support, CapabilitySupport::Unknown),
            request(vec![0; 44]),
            &model,
            Some(1),
            Some(2),
            3,
        )
        .unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn pending_audio_debug_and_serde_free_types_never_expose_bytes() {
    let output = ExperimentalAudioOutput::new(
        AttachmentMediaType::new("audio/wav").unwrap(),
        AttachmentAudioMetadata::new(1, 8_000, 1, 16).unwrap(),
        b"RAW-AUDIO-OUTPUT-CANARY".to_vec(),
    )
    .unwrap();
    let debug = format!("{output:?}");
    assert!(!debug.contains("RAW-AUDIO-OUTPUT-CANARY"));
    assert_eq!(output.bytes(), b"RAW-AUDIO-OUTPUT-CANARY");
}

#[tokio::test]
async fn audio_request_measurement_never_counts_encoded_samples_as_zero() {
    let context = heycode_core::Context::new();
    let registry = TokenCounterRegistry::new();
    registry
        .register(
            &context,
            std::sync::Arc::new(HeuristicTokenEstimator::new()),
        )
        .unwrap();
    let request = request(vec![0; 44]);
    let envelope = measure_experimental_audio_envelope(
        &registry,
        "fixture",
        &request,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    let attachments = envelope
        .entries()
        .iter()
        .find(|entry| entry.contributor() == EnvelopeContributor::Attachments)
        .unwrap();
    assert_eq!(
        attachments.tokens(),
        &ContributorTokens::Uncounted(UncountedReason::Unmeasurable)
    );
}
