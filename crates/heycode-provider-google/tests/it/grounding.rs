//! PGCP05 Google Search grounding: the request, the durable projection and the
//! three states a grounded turn can be in.
//!
//! Every fixture in this file is shaped from the live v1beta discovery document
//! rather than from a remembered example, because the prose documentation now
//! describes a different endpoint. The schemas quoted in
//! `src/grounding.rs` are the contract these cases pin.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use futures::StreamExt as _;
use heycode_core::{Plugin, compose};
use heycode_http::HttpService;
use heycode_llm::{
    CallPurpose, CapabilitySupport, ChatMessage, InferenceEvent, InferenceInput, InputModality,
    ModelDescriptor, NativeFeature, Provider, ProviderOptionContext, RequestDraft, RouteCredential,
};
use heycode_provider_google::{
    GOOGLE_SEARCH_IMPLEMENTATION, GOOGLE_SEARCH_OPTION_KIND, GOOGLE_SEARCH_TOOL_NAME,
    GOOGLE_WEB_SEARCH_LOGICAL, GoogleGeminiProvider, GoogleSearchRequest, GroundingError,
    GroundingProjector, GroundingStatus, google_search_native_tools_plugin, reanchor,
};

/// One candidate carrying visible text parts and optional grounding metadata.
fn candidate(parts: serde_json::Value, metadata: Option<serde_json::Value>) -> serde_json::Value {
    let mut candidate = serde_json::json!({
        "index": 0,
        "content": { "role": "model", "parts": parts },
    });
    if let Some(metadata) = metadata {
        candidate["groundingMetadata"] = metadata;
    }
    candidate
}

/// One `Web` grounding chunk.
fn web_chunk(uri: &str, title: &str) -> serde_json::Value {
    serde_json::json!({ "web": { "uri": uri, "title": title } })
}

/// One grounding support over `part_index`, byte range `start..end`.
fn support(
    part_index: u64,
    start: u64,
    end: u64,
    text: &str,
    indices: &[u64],
) -> serde_json::Value {
    serde_json::json!({
        "segment": {
            "partIndex": part_index,
            "startIndex": start,
            "endIndex": end,
            "text": text,
        },
        "groundingChunkIndices": indices,
    })
}

/// Project one single-chunk grounded response.
fn project_one(
    parts: serde_json::Value,
    metadata: serde_json::Value,
) -> Result<heycode_provider_google::ProjectedGrounding, GroundingError> {
    let mut projector = GroundingProjector::new(Some(GoogleSearchRequest::web()));
    projector.observe(&candidate(parts, Some(metadata)))?;
    projector.project(0, "resp_1")
}

/// Every citation in a projection, in publication order.
fn citations(
    projection: &heycode_provider_google::ProjectedGrounding,
) -> Vec<heycode_core::UrlCitation> {
    projection
        .events()
        .iter()
        .filter_map(|event| match event {
            InferenceEvent::Citation { citation, .. } => Some(citation.clone()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------- request

#[test]
fn the_google_search_tool_entry_uses_the_discovery_documents_field_name() {
    // The `Tool` schema names the field `googleSearch` with `$ref: GoogleSearch`.
    // `{"type": "google_search"}` is the newer `/v1beta/interactions` spelling
    // and would be an unknown field on `generateContent`.
    assert_eq!(
        GoogleSearchRequest::web().tool_entry(),
        serde_json::json!({ "googleSearch": {} })
    );
}

#[test]
fn an_explicit_search_type_set_names_only_the_types_it_enables() {
    assert_eq!(
        GoogleSearchRequest::with_types(true, false)
            .unwrap()
            .tool_entry(),
        serde_json::json!({ "googleSearch": { "searchTypes": { "webSearch": {} } } })
    );
    assert_eq!(
        GoogleSearchRequest::with_types(true, true)
            .unwrap()
            .tool_entry(),
        serde_json::json!({
            "googleSearch": { "searchTypes": { "webSearch": {}, "imageSearch": {} } }
        })
    );
}

#[test]
fn a_search_request_that_enables_no_search_type_is_refused() {
    // "If not set, web search is enabled by default" describes an absent
    // `searchTypes`, not an empty one. Sending `{}` would state a request the
    // schema does not define.
    assert!(matches!(
        GoogleSearchRequest::with_types(false, false),
        Err(GroundingError::InvalidRequest { .. })
    ));
}

#[test]
fn the_provider_request_option_is_owned_by_google_and_carries_the_tool_entry() {
    let option = GoogleSearchRequest::web().provider_option().unwrap();
    assert_eq!(option.provider(), "google");
    assert_eq!(option.kind(), GOOGLE_SEARCH_OPTION_KIND);
    assert_eq!(option.schema_version(), 1);
    assert_eq!(
        option.data(),
        &serde_json::json!({ "tool": { "googleSearch": {} } })
    );
    option.validate().unwrap();
}

#[test]
fn the_recorded_call_input_describes_the_request_and_never_the_returned_queries() {
    // `webSearchQueries` is "Web search queries for the following-up web
    // search" — suggestion material, not a log of what the model ran. Recording
    // it as executed queries would state a fact no Google source supports, and
    // storing it would cache Search Suggestions.
    let projection = project_one(
        serde_json::json!([{ "text": "Spain won." }]),
        serde_json::json!({
            "webSearchQueries": ["UEFA Euro 2024 winner"],
            "groundingChunks": [web_chunk("https://uefa.test/euro", "uefa.test")],
            "groundingSupports": [support(0, 0, 10, "Spain won.", &[0])],
        }),
    )
    .unwrap();
    let InferenceEvent::ServerToolCall { call, .. } = &projection.events()[0] else {
        panic!("first grounding event must be the server-tool call");
    };
    assert_eq!(call.logical(), GOOGLE_WEB_SEARCH_LOGICAL);
    assert_eq!(call.provider_name(), GOOGLE_SEARCH_TOOL_NAME);
    assert_eq!(
        call.input(),
        &serde_json::json!({ "search_types": ["web"] })
    );
    let encoded = serde_json::to_string(call.input()).unwrap();
    assert!(
        !encoded.contains("UEFA"),
        "returned queries must not persist"
    );
}

// ------------------------------------------------------------- tri-state

#[test]
fn a_turn_that_never_offered_the_tool_reports_not_requested() {
    let mut projector = GroundingProjector::new(None);
    projector
        .observe(&candidate(serde_json::json!([{ "text": "hello" }]), None))
        .unwrap();
    assert_eq!(projector.status(), GroundingStatus::NotRequested);
}

#[test]
fn a_requested_turn_the_model_declined_to_ground_is_distinct_from_never_asking() {
    // Offering the tool does not oblige the model to search. "Asked and the
    // model chose not to" is a real outcome and must not read as "never asked".
    let mut projector = GroundingProjector::new(Some(GoogleSearchRequest::web()));
    projector
        .observe(&candidate(serde_json::json!([{ "text": "hello" }]), None))
        .unwrap();
    assert_eq!(projector.status(), GroundingStatus::NotGrounded);
    assert_ne!(projector.status(), GroundingStatus::NotRequested);
    assert!(!projector.status().is_grounded());
}

#[test]
fn grounding_that_returned_no_sources_is_still_grounded_with_zero_sources() {
    // The third distinct state: metadata arrived, so the model did ground, but
    // nothing citable came back. Collapsing this into `NotGrounded` would
    // report that no search happened.
    let mut projector = GroundingProjector::new(Some(GoogleSearchRequest::web()));
    projector
        .observe(&candidate(
            serde_json::json!([{ "text": "hello" }]),
            Some(serde_json::json!({ "webSearchQueries": ["anything"] })),
        ))
        .unwrap();
    assert_eq!(projector.status(), GroundingStatus::Grounded { sources: 0 });
    assert!(projector.status().is_grounded());
    assert_ne!(projector.status(), GroundingStatus::NotGrounded);
    assert_ne!(projector.status(), GroundingStatus::NotRequested);
}

#[test]
fn a_turn_that_never_grounded_projects_no_server_tool_call_at_all() {
    // A `server-tool/call` in the session log asserts the tool ran. Publishing
    // an empty one for a turn that never grounded would put a false claim in
    // durable history.
    let mut projector = GroundingProjector::new(Some(GoogleSearchRequest::web()));
    projector
        .observe(&candidate(serde_json::json!([{ "text": "hello" }]), None))
        .unwrap();
    let projection = projector.project(0, "resp_1").unwrap();
    assert!(projection.events().is_empty());
    assert_eq!(projection.status(), GroundingStatus::NotGrounded);
}

#[test]
fn unsolicited_grounding_metadata_is_refused() {
    let mut projector = GroundingProjector::new(None);
    let error = projector
        .observe(&candidate(
            serde_json::json!([{ "text": "hello" }]),
            Some(serde_json::json!({ "groundingChunks": [] })),
        ))
        .unwrap_err();
    assert_eq!(error, GroundingError::Unsolicited);
}

// ------------------------------------------------------------- durability

#[test]
fn a_grounded_turn_projects_one_call_one_result_then_a_citation_per_span() {
    let projection = project_one(
        serde_json::json!([{ "text": "Spain won Euro 2024. England lost." }]),
        serde_json::json!({
            "groundingChunks": [
                web_chunk("https://uefa.test/euro", "uefa.test"),
                web_chunk("https://aljazeera.test/final", "aljazeera.test"),
            ],
            "groundingSupports": [
                support(0, 0, 20, "Spain won Euro 2024.", &[0]),
                support(0, 21, 34, "England lost.", &[1]),
            ],
        }),
    )
    .unwrap();
    assert_eq!(
        projection.status(),
        GroundingStatus::Grounded { sources: 2 }
    );
    assert_eq!(projection.events().len(), 4);
    assert!(matches!(
        projection.events()[0],
        InferenceEvent::ServerToolCall { .. }
    ));
    let InferenceEvent::ServerToolResult { result, .. } = &projection.events()[1] else {
        panic!("the result must follow the call");
    };
    assert_eq!(result.outcome(), heycode_core::ServerToolOutcome::Success);
    assert_eq!(result.output_count(), Some(2));
    assert_eq!(result.sources().len(), 2);
    assert_eq!(result.sources()[0].url(), "https://uefa.test/euro");
    let citations = citations(&projection);
    assert_eq!(citations.len(), 2);
    assert_eq!(citations[0].url(), "https://uefa.test/euro");
    assert_eq!(citations[0].cited_text(), Some("Spain won Euro 2024."));
    assert_eq!(citations[1].url(), "https://aljazeera.test/final");
    assert_eq!(citations[1].cited_text(), Some("England lost."));
}

#[test]
fn the_call_and_result_share_one_call_id_so_the_pair_survives_replay() {
    let projection = project_one(
        serde_json::json!([{ "text": "Spain won." }]),
        serde_json::json!({
            "groundingChunks": [web_chunk("https://uefa.test/euro", "uefa.test")],
            "groundingSupports": [support(0, 0, 10, "Spain won.", &[0])],
        }),
    )
    .unwrap();
    let (
        InferenceEvent::ServerToolCall { call, .. },
        InferenceEvent::ServerToolResult { result, .. },
    ) = (&projection.events()[0], &projection.events()[1])
    else {
        panic!("a projection must open with a call and its result");
    };
    assert_eq!(call.id(), result.call_id());
    // Derived from the response id, so the same response replays the same id.
    assert_eq!(call.id().as_str(), "resp_1/grounding");
}

#[test]
fn every_projected_citation_survives_a_json_round_trip_with_its_span_intact() {
    // The durability claim. `SessionEventKind::AssistantCitation` stores a
    // `UrlCitation` and revalidates it on load, so a citation that round-trips
    // through JSON and revalidates is one a resumed session can replay. This
    // exercises `serde_json` plus `UrlCitation::validate` directly rather than
    // `heycode-session`, which this crate does not depend on.
    let projection = project_one(
        serde_json::json!([{ "text": "Spain won Euro 2024. England lost." }]),
        serde_json::json!({
            "groundingChunks": [
                web_chunk("https://uefa.test/euro", "uefa.test"),
                web_chunk("https://aljazeera.test/final", "aljazeera.test"),
            ],
            "groundingSupports": [
                support(0, 0, 20, "Spain won Euro 2024.", &[0]),
                support(0, 21, 34, "England lost.", &[1]),
            ],
        }),
    )
    .unwrap();
    let citations = citations(&projection);
    assert_eq!(citations.len(), 2);
    for citation in &citations {
        let encoded = serde_json::to_string(citation).unwrap();
        let restored: heycode_core::UrlCitation = serde_json::from_str(&encoded).unwrap();
        restored.validate().unwrap();
        assert_eq!(&restored, citation);
        // A citation that survives as a bare URL is not durable: the span and
        // the excerpt that anchors it must come back too.
        assert!(restored.start_index().is_some());
        assert!(restored.end_index().is_some());
        assert!(restored.cited_text().is_some());
        assert!(restored.title().is_some());
    }
}

#[test]
fn the_durable_projection_never_carries_search_suggestion_markup() {
    // Google's terms forbid caching Search Suggestions, and the chat-history
    // exemption covers only "the text of the Grounded Result(s)". The widget
    // must be reachable for a host that can display it and absent from
    // everything that persists.
    let widget = "<style>.container{}</style><div class=\"gs\">suggestion</div>";
    let mut projector = GroundingProjector::new(Some(GoogleSearchRequest::web()));
    projector
        .observe(&candidate(
            serde_json::json!([{ "text": "Spain won." }]),
            Some(serde_json::json!({
                "searchEntryPoint": { "renderedContent": widget },
                "groundingChunks": [web_chunk("https://uefa.test/euro", "uefa.test")],
                "groundingSupports": [support(0, 0, 10, "Spain won.", &[0])],
            })),
        ))
        .unwrap();
    assert_eq!(projector.search_suggestions(), Some(widget));
    let projection = projector.project(0, "resp_1").unwrap();
    // Serialized exactly as the session log stores them: `InferenceEvent`
    // itself is not `Serialize`, so each durable payload is encoded directly.
    let encoded = durable_payloads(&projection);
    assert!(
        !encoded.is_empty(),
        "the fixture must produce durable payloads"
    );
    for payload in &encoded {
        assert!(
            !payload.contains("suggestion") && !payload.contains("<style>"),
            "search suggestion markup must never reach a durable event"
        );
    }
}

/// Every durable payload in a projection, serialized as the session log stores
/// it.
fn durable_payloads(projection: &heycode_provider_google::ProjectedGrounding) -> Vec<String> {
    projection
        .events()
        .iter()
        .map(|event| match event {
            InferenceEvent::ServerToolCall { call, .. } => serde_json::to_string(call),
            InferenceEvent::ServerToolResult { result, .. } => serde_json::to_string(result),
            InferenceEvent::Citation { citation, .. } => serde_json::to_string(citation),
            other => panic!("a grounding projection published {other:?}"),
        })
        .map(|encoded| encoded.unwrap())
        .collect()
}

// ----------------------------------------------------------------- spans

#[test]
fn segment_offsets_are_rebased_from_their_part_onto_the_visible_message() {
    // `Segment.startIndex` is "measured in bytes ... from the start of the
    // Part", and `partIndex` selects that part. Copying the raw offsets through
    // would point the second part's citation at the first part's words.
    let projection = project_one(
        serde_json::json!([{ "text": "Spain won. " }, { "text": "England lost." }]),
        serde_json::json!({
            "groundingChunks": [web_chunk("https://uefa.test/euro", "uefa.test")],
            "groundingSupports": [support(1, 0, 13, "England lost.", &[0])],
        }),
    )
    .unwrap();
    let citations = citations(&projection);
    // "Spain won. " is 11 bytes, so the second part starts at 11.
    assert_eq!(citations[0].start_index(), Some(11));
    assert_eq!(citations[0].end_index(), Some(24));
    let message = "Spain won. England lost.";
    assert_eq!(&message[11..24], "England lost.");
}

#[test]
fn a_thought_part_does_not_shift_a_single_citation_offset() {
    // A `thought: true` part is published as a reasoning delta, not as visible
    // assistant text, so it contributes no bytes to the span a citation covers.
    // Counting it would push every later citation right by its length.
    let projection = project_one(
        serde_json::json!([
            { "text": "I should search for the final result.", "thought": true },
            { "text": "Spain won." },
        ]),
        serde_json::json!({
            "groundingChunks": [web_chunk("https://uefa.test/euro", "uefa.test")],
            "groundingSupports": [support(1, 0, 10, "Spain won.", &[0])],
        }),
    )
    .unwrap();
    let citations = citations(&projection);
    assert_eq!(citations[0].start_index(), Some(0));
    assert_eq!(citations[0].end_index(), Some(10));
}

#[test]
fn offsets_accumulate_across_streamed_chunks() {
    // Each streamed chunk carries its own `Content`, so its part offsets restart
    // at zero while the visible message keeps growing.
    let mut projector = GroundingProjector::new(Some(GoogleSearchRequest::web()));
    projector
        .observe(&candidate(
            serde_json::json!([{ "text": "Spain won. " }]),
            Some(serde_json::json!({
                "groundingChunks": [web_chunk("https://uefa.test/euro", "uefa.test")],
            })),
        ))
        .unwrap();
    projector
        .observe(&candidate(
            serde_json::json!([{ "text": "England lost." }]),
            Some(serde_json::json!({
                "groundingSupports": [support(0, 0, 13, "England lost.", &[0])],
            })),
        ))
        .unwrap();
    let projection = projector.project(0, "resp_1").unwrap();
    let citations = citations(&projection);
    assert_eq!(citations[0].start_index(), Some(11));
    assert_eq!(citations[0].end_index(), Some(24));
}

#[test]
fn an_ungrounded_streamed_chunk_still_advances_the_offset_baseline() {
    // A chunk with no grounding metadata still contributes visible text. If it
    // were skipped, the next chunk's citation would be short by its length.
    let mut projector = GroundingProjector::new(Some(GoogleSearchRequest::web()));
    projector
        .observe(&candidate(
            serde_json::json!([{ "text": "Spain won. " }]),
            None,
        ))
        .unwrap();
    projector
        .observe(&candidate(
            serde_json::json!([{ "text": "England lost." }]),
            Some(serde_json::json!({
                "groundingChunks": [web_chunk("https://uefa.test/euro", "uefa.test")],
                "groundingSupports": [support(0, 0, 13, "England lost.", &[0])],
            })),
        ))
        .unwrap();
    let citations = citations(&projector.project(0, "resp_1").unwrap());
    assert_eq!(citations[0].start_index(), Some(11));
}

#[test]
fn a_support_arriving_late_and_indexing_the_whole_response_is_anchored_correctly() {
    // `partIndex` has two defensible readings and the documentation settles
    // neither. A support that arrives in the final chunk while describing a
    // span of the first one only resolves under the response-global reading;
    // the chunk-local attempt is rejected by `segment.text` rather than
    // published as a wrong span.
    let mut projector = GroundingProjector::new(Some(GoogleSearchRequest::web()));
    projector
        .observe(&candidate(
            serde_json::json!([{ "text": "Spain won. " }]),
            None,
        ))
        .unwrap();
    projector
        .observe(&candidate(
            serde_json::json!([{ "text": "England lost." }]),
            Some(serde_json::json!({
                "groundingChunks": [web_chunk("https://uefa.test/euro", "uefa.test")],
                "groundingSupports": [support(0, 0, 10, "Spain won.", &[0])],
            })),
        ))
        .unwrap();
    let citations = citations(&projector.project(0, "resp_1").unwrap());
    assert_eq!(citations[0].cited_text(), Some("Spain won."));
    assert_eq!(citations[0].start_index(), Some(0));
    assert_eq!(citations[0].end_index(), Some(10));
}

#[test]
fn an_ambiguous_stream_part_index_degrades_to_an_unanchored_source() {
    let mut projector = GroundingProjector::new(Some(GoogleSearchRequest::web()));
    projector
        .observe(&candidate(serde_json::json!([{ "text": "first" }]), None))
        .unwrap();
    projector
        .observe(&candidate(
            serde_json::json!([{ "text": "second" }]),
            Some(serde_json::json!({
                "groundingChunks": [web_chunk("https://source.test/item", "source.test")],
                "groundingSupports": [{
                    "segment": {
                        "partIndex": 0,
                        "startIndex": 0,
                        "endIndex": 5
                    },
                    "groundingChunkIndices": [0]
                }]
            })),
        ))
        .unwrap();
    let citations = citations(&projector.project(0, "resp_1").unwrap());
    assert_eq!(citations.len(), 1);
    // With no declared segment text, both the chunk-local and response-global
    // readings are valid and point at different words. Keep the source, but do
    // not guess a span.
    assert_eq!(citations[0].cited_text(), None);
    assert_eq!(citations[0].start_index(), None);
    assert_eq!(citations[0].end_index(), None);
}

#[test]
fn segment_offsets_are_byte_offsets_and_not_character_offsets() {
    // "measured in bytes" is the documented unit. Reading them as characters
    // would slice this segment in the middle of the em dash.
    let text = "Espana — Spain won.";
    let projection = project_one(
        serde_json::json!([{ "text": text }]),
        serde_json::json!({
            "groundingChunks": [web_chunk("https://uefa.test/euro", "uefa.test")],
            // "Espana — " is 9 characters but 11 bytes; the em dash is 3 bytes.
            "groundingSupports": [support(0, 11, 21, "Spain won.", &[0])],
        }),
    )
    .unwrap();
    let citations = citations(&projection);
    assert_eq!(citations[0].cited_text(), Some("Spain won."));
    assert_eq!(citations[0].start_index(), Some(11));
    assert_ne!(text.chars().count(), text.len());
}

#[test]
fn offsets_that_split_a_multibyte_character_are_refused() {
    // `&text[start..end]` would panic here, and panicking is denied in this
    // workspace. The range is rejected instead.
    let error = project_one(
        serde_json::json!([{ "text": "Espana — Spain" }]),
        serde_json::json!({
            "groundingChunks": [web_chunk("https://uefa.test/euro", "uefa.test")],
            "groundingSupports": [support(0, 8, 12, "", &[0])],
        }),
    )
    .unwrap_err();
    assert_eq!(error, GroundingError::SegmentRange);
}

#[test]
fn a_segment_whose_declared_text_disagrees_with_its_offsets_is_refused() {
    // The check that makes the chunk-local reading of `partIndex` safe rather
    // than assumed: a wrong reading fails here instead of shipping a span that
    // points at the wrong words.
    let error = project_one(
        serde_json::json!([{ "text": "Spain won Euro 2024." }]),
        serde_json::json!({
            "groundingChunks": [web_chunk("https://uefa.test/euro", "uefa.test")],
            "groundingSupports": [support(0, 0, 10, "England lost.", &[0])],
        }),
    )
    .unwrap_err();
    assert_eq!(error, GroundingError::SegmentMismatch);
}

#[test]
fn offsets_reaching_past_the_end_of_their_part_are_refused() {
    let error = project_one(
        serde_json::json!([{ "text": "Spain won." }]),
        serde_json::json!({
            "groundingChunks": [web_chunk("https://uefa.test/euro", "uefa.test")],
            "groundingSupports": [support(0, 0, 999, "", &[0])],
        }),
    )
    .unwrap_err();
    assert_eq!(error, GroundingError::SegmentRange);
}

#[test]
fn a_segment_naming_a_part_that_carries_no_visible_text_is_refused() {
    let error = project_one(
        serde_json::json!([
            { "text": "reasoning", "thought": true },
            { "text": "Spain won." },
        ]),
        serde_json::json!({
            "groundingChunks": [web_chunk("https://uefa.test/euro", "uefa.test")],
            "groundingSupports": [support(0, 0, 9, "reasoning", &[0])],
        }),
    )
    .unwrap_err();
    assert_eq!(error, GroundingError::SegmentPart);
}

#[test]
fn a_support_with_no_segment_still_cites_its_source_without_a_span() {
    // Losing the source because the provider sent no segment would be worse
    // than publishing an unanchored citation, but the missing span must be
    // visibly missing rather than defaulted to zero.
    let projection = project_one(
        serde_json::json!([{ "text": "Spain won." }]),
        serde_json::json!({
            "groundingChunks": [web_chunk("https://uefa.test/euro", "uefa.test")],
            "groundingSupports": [{ "groundingChunkIndices": [0] }],
        }),
    )
    .unwrap();
    let citations = citations(&projection);
    assert_eq!(citations.len(), 1);
    assert_eq!(citations[0].url(), "https://uefa.test/euro");
    assert_eq!(citations[0].start_index(), None);
    assert_eq!(citations[0].end_index(), None);
    assert_eq!(citations[0].cited_text(), None);
}

#[test]
fn rendered_part_associations_do_not_invent_a_text_span() {
    // The current discovery schema can associate whole rendered parts with a
    // support source, but it supplies no byte range. Until shared citation
    // vocabulary can name a media/text part identity, preserve the source and
    // keep the span explicitly absent.
    let projection = project_one(
        serde_json::json!([{ "text": "Spain won." }]),
        serde_json::json!({
            "groundingChunks": [web_chunk("https://uefa.test/euro", "uefa.test")],
            "groundingSupports": [{
                "renderedParts": [0],
                "groundingChunkIndices": [0]
            }],
        }),
    )
    .unwrap();
    let citations = citations(&projection);
    assert_eq!(citations.len(), 1);
    assert_eq!(citations[0].url(), "https://uefa.test/euro");
    assert_eq!(citations[0].start_index(), None);
    assert_eq!(citations[0].end_index(), None);
    assert_eq!(citations[0].cited_text(), None);
}

#[test]
fn an_empty_segment_range_beside_declared_text_is_refused() {
    // Absent integers are proto3 defaults, so an omitted `endIndex` reads as
    // zero. A zero-length span alongside non-empty text is a contradiction, not
    // a citation to publish at offset zero.
    let error = project_one(
        serde_json::json!([{ "text": "Spain won." }]),
        serde_json::json!({
            "groundingChunks": [web_chunk("https://uefa.test/euro", "uefa.test")],
            "groundingSupports": [{
                "segment": { "text": "Spain won." },
                "groundingChunkIndices": [0],
            }],
        }),
    )
    .unwrap_err();
    assert_eq!(error, GroundingError::SegmentMismatch);
}

// --------------------------------------------------------- chunk indices

#[test]
fn grounding_chunks_accumulate_across_chunks_and_indices_span_the_whole_response() {
    // "When streaming, this only contains the grounding chunks that have not
    // been included in the grounding metadata of previous responses" and "the
    // grounding_chunk_indices refer to the indices across all responses".
    let mut projector = GroundingProjector::new(Some(GoogleSearchRequest::web()));
    projector
        .observe(&candidate(
            serde_json::json!([{ "text": "Spain won. " }]),
            Some(serde_json::json!({
                "groundingChunks": [web_chunk("https://uefa.test/euro", "uefa.test")],
            })),
        ))
        .unwrap();
    projector
        .observe(&candidate(
            serde_json::json!([{ "text": "England lost." }]),
            Some(serde_json::json!({
                "groundingChunks": [web_chunk("https://aljazeera.test/final", "aljazeera.test")],
                // Index 1 only resolves if the first chunk's source was kept.
                "groundingSupports": [support(0, 0, 13, "England lost.", &[1])],
            })),
        ))
        .unwrap();
    let projection = projector.project(0, "resp_1").unwrap();
    assert_eq!(
        projection.status(),
        GroundingStatus::Grounded { sources: 2 }
    );
    let citations = citations(&projection);
    assert_eq!(citations.len(), 1);
    assert_eq!(citations[0].url(), "https://aljazeera.test/final");
}

#[test]
fn an_uncitable_chunk_holds_its_index_so_later_citations_stay_correct() {
    // A Maps or file-search chunk cannot be cited by this row, but dropping it
    // would shift every later index and silently re-point the citation at the
    // wrong source.
    let projection = project_one(
        serde_json::json!([{ "text": "Spain won." }]),
        serde_json::json!({
            "groundingChunks": [
                { "maps": { "uri": "https://maps.test/place", "title": "place" } },
                web_chunk("https://uefa.test/euro", "uefa.test"),
            ],
            "groundingSupports": [support(0, 0, 10, "Spain won.", &[1])],
        }),
    )
    .unwrap();
    let citations = citations(&projection);
    assert_eq!(citations.len(), 1);
    assert_eq!(citations[0].url(), "https://uefa.test/euro");
    // The uncitable chunk is not a source, so the count stays honest.
    assert_eq!(
        projection.status(),
        GroundingStatus::Grounded { sources: 1 }
    );
}

#[test]
fn a_support_pointing_at_an_uncitable_chunk_produces_no_citation() {
    let projection = project_one(
        serde_json::json!([{ "text": "Spain won." }]),
        serde_json::json!({
            "groundingChunks": [{ "maps": { "uri": "https://maps.test/place" } }],
            "groundingSupports": [support(0, 0, 10, "Spain won.", &[0])],
        }),
    )
    .unwrap();
    assert!(citations(&projection).is_empty());
    assert_eq!(
        projection.status(),
        GroundingStatus::Grounded { sources: 0 }
    );
}

#[test]
fn a_chunk_index_outside_the_accumulated_list_is_refused() {
    // The accumulation contract says indices span every response, so an index
    // that does not resolve means the accumulation itself is wrong.
    let error = project_one(
        serde_json::json!([{ "text": "Spain won." }]),
        serde_json::json!({
            "groundingChunks": [web_chunk("https://uefa.test/euro", "uefa.test")],
            "groundingSupports": [support(0, 0, 10, "Spain won.", &[7])],
        }),
    )
    .unwrap_err();
    assert_eq!(error, GroundingError::ChunkIndexOutOfRange);
}

#[test]
fn one_support_naming_several_chunks_cites_every_one_of_them() {
    let projection = project_one(
        serde_json::json!([{ "text": "Spain won." }]),
        serde_json::json!({
            "groundingChunks": [
                web_chunk("https://uefa.test/euro", "uefa.test"),
                web_chunk("https://aljazeera.test/final", "aljazeera.test"),
            ],
            "groundingSupports": [support(0, 0, 10, "Spain won.", &[0, 1])],
        }),
    )
    .unwrap();
    let citations = citations(&projection);
    assert_eq!(citations.len(), 2);
    assert_eq!(citations[0].start_index(), citations[1].start_index());
    assert_eq!(citations[0].url(), "https://uefa.test/euro");
    assert_eq!(citations[1].url(), "https://aljazeera.test/final");
}

#[test]
fn an_image_search_chunk_is_cited_by_its_attribution_url() {
    // `Image.sourceUri` is "The web page URI for attribution" — the citable
    // field. `imageUri` is the asset and is not what a citation points at.
    let projection = project_one(
        serde_json::json!([{ "text": "Spain won." }]),
        serde_json::json!({
            "groundingChunks": [{ "image": {
                "sourceUri": "https://uefa.test/gallery",
                "imageUri": "https://uefa.test/photo.png",
                "title": "uefa.test",
            }}],
            "groundingSupports": [support(0, 0, 10, "Spain won.", &[0])],
        }),
    )
    .unwrap();
    let citations = citations(&projection);
    assert_eq!(citations[0].url(), "https://uefa.test/gallery");
}

#[test]
fn a_chunk_with_no_public_url_holds_its_index_without_becoming_a_source() {
    // `uri` is a read-only output field with no documented presence guarantee.
    let projection = project_one(
        serde_json::json!([{ "text": "Spain won." }]),
        serde_json::json!({
            "groundingChunks": [
                { "web": { "title": "no uri" } },
                web_chunk("https://uefa.test/euro", "uefa.test"),
            ],
            "groundingSupports": [support(0, 0, 10, "Spain won.", &[1])],
        }),
    )
    .unwrap();
    assert_eq!(
        projection.status(),
        GroundingStatus::Grounded { sources: 1 }
    );
    assert_eq!(citations(&projection)[0].url(), "https://uefa.test/euro");
}

#[test]
fn a_non_public_source_url_is_refused_rather_than_persisted() {
    // `UrlCitation` requires a public HTTP(S) URL without userinfo. A raw blob
    // would have replayed this into a renderer unchecked.
    let error = project_one(
        serde_json::json!([{ "text": "Spain won." }]),
        serde_json::json!({
            "groundingChunks": [web_chunk("file:///etc/passwd", "local")],
            "groundingSupports": [support(0, 0, 10, "Spain won.", &[0])],
        }),
    )
    .unwrap_err();
    assert_eq!(error, GroundingError::UnsafeSource);
}

#[test]
fn the_source_list_is_a_bounded_preview_while_the_reported_count_stays_true() {
    // `ServerToolResult` bounds its source list at 128. Truncating the preview
    // loses nothing a reader needs, because every citation carries its own URL,
    // and the count keeps telling the truth.
    let chunks = (0..150)
        .map(|index| web_chunk(&format!("https://source.test/{index}"), "source.test"))
        .collect::<Vec<_>>();
    let projection = project_one(
        serde_json::json!([{ "text": "Spain won." }]),
        serde_json::json!({
            "groundingChunks": chunks,
            "groundingSupports": [support(0, 0, 10, "Spain won.", &[149])],
        }),
    )
    .unwrap();
    let InferenceEvent::ServerToolResult { result, .. } = &projection.events()[1] else {
        panic!("the result must follow the call");
    };
    assert_eq!(result.output_count(), Some(150));
    assert_eq!(result.sources().len(), 128);
    assert_eq!(
        projection.status(),
        GroundingStatus::Grounded { sources: 150 }
    );
    // The citation for the 150th source is published even though the preview
    // could not list it.
    assert_eq!(citations(&projection)[0].url(), "https://source.test/149");
}

// -------------------------------------------------------------- reflow

#[test]
fn a_citation_re_anchors_by_its_excerpt_after_the_renderer_reflows_the_text() {
    // The reflow case the offsets alone cannot survive. Byte offsets are valid
    // only against the exact bytes they were measured on; the excerpt is
    // content, so it survives any transformation that keeps the words.
    let projection = project_one(
        serde_json::json!([{ "text": "Spain won Euro 2024. England lost." }]),
        serde_json::json!({
            "groundingChunks": [web_chunk("https://aljazeera.test/final", "aljazeera.test")],
            "groundingSupports": [support(0, 21, 34, "England lost.", &[0])],
        }),
    )
    .unwrap();
    let citation = &citations(&projection)[0];
    assert_eq!(citation.start_index(), Some(21));
    let reflowed = "  Spain won Euro 2024.\n  England lost.";
    // The stored offsets now select the wrong words entirely.
    assert_ne!(
        reflowed.get(21..34),
        Some("England lost."),
        "the reflow must actually move the span, or this case proves nothing"
    );
    let (start, end) = reanchor(citation, reflowed).unwrap();
    assert_eq!(&reflowed[start..end], "England lost.");
}

#[test]
fn re_anchoring_confirms_the_stored_offsets_when_the_text_did_not_move() {
    let projection = project_one(
        serde_json::json!([{ "text": "Spain won Euro 2024. England lost." }]),
        serde_json::json!({
            "groundingChunks": [web_chunk("https://aljazeera.test/final", "aljazeera.test")],
            "groundingSupports": [support(0, 21, 34, "England lost.", &[0])],
        }),
    )
    .unwrap();
    let citation = &citations(&projection)[0];
    let rendered = "Spain won Euro 2024. England lost.";
    assert_eq!(reanchor(citation, rendered), Some((21, 34)));
}

#[test]
fn a_citation_with_no_excerpt_cannot_be_re_anchored() {
    // An unanchored citation renders as a source, never as a guessed span.
    let projection = project_one(
        serde_json::json!([{ "text": "Spain won." }]),
        serde_json::json!({
            "groundingChunks": [web_chunk("https://uefa.test/euro", "uefa.test")],
            "groundingSupports": [{ "groundingChunkIndices": [0] }],
        }),
    )
    .unwrap();
    assert_eq!(reanchor(&citations(&projection)[0], "Spain won."), None);
}

#[test]
fn re_anchoring_reports_absence_when_the_excerpt_is_gone_from_the_rendering() {
    let projection = project_one(
        serde_json::json!([{ "text": "Spain won." }]),
        serde_json::json!({
            "groundingChunks": [web_chunk("https://uefa.test/euro", "uefa.test")],
            "groundingSupports": [support(0, 0, 10, "Spain won.", &[0])],
        }),
    )
    .unwrap();
    assert_eq!(
        reanchor(&citations(&projection)[0], "something else entirely"),
        None
    );
}

#[test]
fn re_anchoring_refuses_to_guess_between_duplicate_excerpts() {
    let projection = project_one(
        serde_json::json!([{ "text": "Spain won. England lost." }]),
        serde_json::json!({
            "groundingChunks": [web_chunk("https://uefa.test/euro", "uefa.test")],
            "groundingSupports": [support(0, 11, 24, "England lost.", &[0])],
        }),
    )
    .unwrap();
    let rendered = "England lost. Spain won. England lost.";
    // Reflow invalidates the stored range, and the excerpt now appears twice.
    // Picking the first hit would attach the source to a claim the provider did
    // not identify, so the renderer must fall back to an unanchored source.
    assert_eq!(reanchor(&citations(&projection)[0], rendered), None);
}

// -------------------------------------------------------------- registry

#[test]
fn the_native_tool_plugin_offers_google_search_for_the_web_search_capability() {
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_native_tools::native_tools_plugin(),
        google_search_native_tools_plugin(),
    ];
    let context = compose(&plugins).unwrap();
    let registry = context
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap();
    let routes = registry.resolve("google").unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].logical(), GOOGLE_WEB_SEARCH_LOGICAL);
    assert_eq!(routes[0].implementation(), GOOGLE_SEARCH_IMPLEMENTATION);
    assert_eq!(routes[0].provider(), Some("google"));
    // A provider-native candidate is route-specific: it is not offered to a
    // different provider's request.
    assert!(registry.resolve("openrouter").unwrap().is_empty());
}

// ------------------------------------------------------------ live smoke

#[tokio::test]
async fn live_grounding_requires_explicit_operator_model_and_tool_evidence() {
    // Ordinary tests inspect no ambient credential. The second flag is the
    // operator's explicit assertion that the selected model currently supports
    // this hosted tool; model-list discovery does not publish that fact.
    if std::env::var("HEYCODE_E2E").ok().as_deref() != Some("1")
        || std::env::var("HEYCODE_E2E_GEMINI_SEARCH").ok().as_deref() != Some("1")
    {
        return;
    }
    let (Ok(api_key), Ok(model_id)) = (
        std::env::var("GEMINI_API_KEY"),
        std::env::var("HEYCODE_E2E_GEMINI_MODEL"),
    ) else {
        return;
    };
    let transport = heycode_http::ReqwestHttpTransport::new().unwrap();
    let provider = GoogleGeminiProvider::developer(
        HttpService::new(std::sync::Arc::new(transport)),
        RouteCredential::fixed(api_key),
        "GEMINI_API_KEY",
        model_id.clone(),
    )
    .unwrap()
    .with_google_search(GoogleSearchRequest::web(), vec![model_id.clone()])
    .unwrap();
    let route = heycode_core::NativeToolRoute::new(
        GOOGLE_WEB_SEARCH_LOGICAL,
        GOOGLE_SEARCH_IMPLEMENTATION,
        heycode_core::NativeToolImplementationKind::Provider,
        Some("google".to_owned()),
    )
    .unwrap();
    let mut model = ModelDescriptor::unknown(&model_id);
    model.capabilities.native_web = CapabilitySupport::Supported;
    let options = provider
        .request_options_for(ProviderOptionContext::new(
            &model,
            std::slice::from_ref(&route),
        ))
        .unwrap();
    let draft = RequestDraft {
        provider: "google".to_owned(),
        model: model_id,
        catalog_revision: None,
        catalog_fetched_at_ms: None,
        effective_at_ms: 1,
        system: None,
        inputs: vec![InferenceInput::Message(ChatMessage::user(
            "Use Google Search and cite a current official source for today's UTC date.",
        ))],
        tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: vec![NativeFeature::Web],
        native_tool_routes: vec![route],
        provider_options: options,
        temperature: None,
        max_output_tokens: None,
        purpose: CallPurpose::Conversation,
    };
    let adapter = provider.inference_adapter().unwrap();
    let call = adapter.resolve(draft, &model).unwrap();
    let events = adapter
        .stream(call)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    assert!(events.iter().any(|event| matches!(event, InferenceEvent::ServerToolCall { call, .. } if call.logical() == GOOGLE_WEB_SEARCH_LOGICAL)));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, InferenceEvent::Citation { .. }))
    );
}
