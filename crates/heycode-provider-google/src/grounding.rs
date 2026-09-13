//! PGCP05 — Google Search and Vertex external-API grounding: the requests these
//! routes send and the durable projection of the `groundingMetadata` a grounded
//! answer comes back with.
//!
//! # What "durable" means here
//!
//! A grounded answer is worth nothing if its attribution dies with the
//! terminal frame that drew it. The durable half of this module is
//! [`GroundingProjector::project`], which turns Gemini's response-only
//! `groundingMetadata` into the three provider-neutral events heycode already
//! knows how to persist — [`heycode_core::ServerToolCall`],
//! [`heycode_core::ServerToolResult`] and one [`heycode_core::UrlCitation`] per
//! attributed span. Once the shared Gemini adapter emits them, Agent records
//! `server-tool/call`, `server-tool/result` and `assistant/citation`, so a
//! resumed session can replay the same spans against the same sources. This
//! crate owns the projection; the still-required adapter hook is documented in
//! its README and live gate rather than claimed here.
//!
//! # Projected, not verbatim
//!
//! [`heycode_core::ProviderStateKind::GeminiModelContent`] is preserved verbatim
//! because Gemini demands a returned `thoughtSignature` "in the exact part
//! where it was received". Grounding is the opposite case, for three
//! independent reasons, and this module therefore projects rather than
//! preserving:
//!
//! 1. **Nothing can be echoed back.** `groundingMetadata` is a field of
//!    `Candidate`, which is a response type. The request-side `Content` carries
//!    only `role` and `parts`, so there is no field that would ever accept
//!    grounding metadata on a later turn. Verbatim bytes would buy no protocol
//!    guarantee at all.
//! 2. **Verbatim would break the terms.** A faithful blob contains
//!    `searchEntryPoint.renderedContent` — the Search Suggestion widget — and
//!    Google's terms forbid caching Search Suggestions. See
//!    [`GroundingProjector::search_suggestions`].
//! 3. **Verbatim would be unvalidated.** [`heycode_core::UrlCitation`] enforces a
//!    public HTTP(S) URL with no userinfo and bounded display text. A raw blob
//!    replayed into a renderer carries whatever the provider sent.
//!
//! The state kind's own contract also forbids it: `GeminiModelContent` is
//! defined as "`role: "model"` plus its ordered `parts`", and grounding
//! metadata is not a part.
//!
//! # Terms of service
//!
//! <https://ai.google.dev/gemini-api/terms#grounding-with-google-search> is
//! quoted rather than paraphrased because two of its clauses pull against each
//! other and the resolution is a design decision, not a detail:
//!
//! - "You will only use Grounding with Google Search in an application that is
//!   owned and operated by you and will only display the Grounded Results with
//!   the associated Search Suggestion(s) to the end user who submitted the
//!   prompt."
//! - "You will not, and will not allow your end user or any third party to,
//!   cache, frame, syndicate, resell, analyze, train on, or otherwise learn
//!   from Grounded Results or Search Suggestions."
//! - "You may copy and store, for up to two (2) years, the text of the Grounded
//!   Result(s): ... (2) in chat history of an end user of your application only
//!   for the purpose of allowing that end user to view their chat history".
//!
//! The third clause is what makes this module's durability lawful: a heycode
//! session log is exactly "chat history of an end user ... for the purpose of
//! allowing that end user to view their chat history". It licenses the *text of
//! the Grounded Result* and the Links inside it — which is what a
//! [`heycode_core::UrlCitation`] is — and it licenses nothing else. Search
//! Suggestions get no chat-history exemption, only a narrow legal-compliance
//! one, so [`ProjectedGrounding`] is structurally incapable of carrying them:
//! the suggestion markup never enters a durable type.
//!
//! # What is dropped, and why it is said out loud
//!
//! `GroundingSupport.confidenceScores` — "Confidence score of the support
//! references. Ranges from 0 to 1" — has no field in
//! [`heycode_core::UrlCitation`], which is the neutral citation vocabulary every
//! provider shares. This projection drops it. That is a real loss of provider
//! evidence, recorded here rather than left for a reader to discover: a
//! citation heycode renders carries no confidence, and nothing downstream should
//! imply one. Carrying it would take a `heycode-core` field, which is owner-only.
//!
//! `GroundingSupport.renderedParts` associates whole candidate parts with a
//! support source but supplies no text range. The neutral citation vocabulary
//! cannot express a media-part relationship, so this projection retains the
//! source as an unanchored citation when `segment` is absent and never guesses
//! that the whole visible text part was cited. A future durable media citation
//! needs a shared core/session field before this provider can preserve it.
//!
//! `GroundingMetadata.webSearchQueries` and `imageSearchQueries` are dropped
//! deliberately rather than for want of a field; see
//! [`GoogleSearchRequest::call_input`].
//! `retrievalQueries` and retrieved snippets are likewise excluded from the
//! external-API normalized plane: they may contain proprietary user/search
//! data, while a public `retrievedContext.uri` plus optional title is sufficient
//! for the provider-neutral citation.
//!
//! Two obligations this crate cannot discharge are recorded here rather than
//! assumed satisfied:
//!
//! - **The display obligation is unmet in a terminal.** Search Suggestions ship
//!   as `searchEntryPoint.renderedContent`, "Web content snippet that can be
//!   embedded in a web page or an app webview". A TUI cannot render HTML and
//!   CSS, and the terms separately forbid modifying Search Suggestions or
//!   interspersing other content with them — which is precisely what any
//!   text-mode rendering of a styled widget does. heycode cannot comply by
//!   displaying it. The honest options are to surface the markup to a host that
//!   can display it, or to not enable the tool; this module supplies the first
//!   and refuses to pretend the obligation is met.
//! - **The two-year bound is not enforced.** Session logs have no retention
//!   policy, and a provider crate cannot impose one.
//!
//! # Sources
//!
//! Every protocol claim below is taken from the live v1beta discovery document
//! rather than from prose, because the prose documentation now describes a
//! different endpoint (`/v1beta/interactions`, whose grounding shape is a
//! `steps` array with `url_citation` annotations) while this crate's route is
//! `models.streamGenerateContent`.
//!
//! - Discovery document (`GroundingMetadata`, `GroundingChunk`, `Segment`,
//!   `SearchEntryPoint`, `GoogleSearch`, `Tool.googleSearch`, and
//!   `Candidate.groundingMetadata`):
//!   <https://generativelanguage.googleapis.com/$discovery/rest?version=v1beta>
//! - Grounding guide and the current tool name:
//!   <https://ai.google.dev/gemini-api/docs/google-search>
//! - Vertex external search API grounding and `RetrievedContext`:
//!   <https://docs.cloud.google.com/vertex-ai/generative-ai/docs/grounding/grounding-with-your-search-api>
//! - Terms: <https://ai.google.dev/gemini-api/terms#grounding-with-google-search>
//! - Absent integers read as proto3 defaults:
//!   <https://protobuf.dev/programming-guides/json/>

use heycode_core::{
    CallId, ProviderRequestOption, ServerToolResult, ServerToolSource, UrlCitation,
};
use heycode_llm::InferenceEvent;

/// Logical native-tool capability Google Search grounding implements.
pub const GOOGLE_WEB_SEARCH_LOGICAL: &str = "web_search";

/// Exact provider-native implementation id for Google Search grounding.
pub const GOOGLE_SEARCH_IMPLEMENTATION: &str = "google:google_search";

/// [`ProviderRequestOption`] kind carrying the `googleSearch` tool entry.
pub const GOOGLE_SEARCH_OPTION_KIND: &str = "google-search";

/// Provider-native tool name recorded on every normalized grounding event.
///
/// <https://ai.google.dev/gemini-api/docs/google-search> — "Older models use a
/// `google_search_retrieval` tool. For all current models, use the
/// `google_search` tool as shown in the examples."
pub const GOOGLE_SEARCH_TOOL_NAME: &str = "google_search";

/// Request-object field naming the tool inside `tools[]`.
///
/// The discovery document's `Tool` schema names the field `googleSearch`
/// (`$ref: GoogleSearch`). The guide's `{"type": "google_search"}` spelling
/// belongs to the newer `/v1beta/interactions` endpoint, which this route does
/// not speak.
const TOOL_FIELD: &str = "googleSearch";

/// Largest source list [`ServerToolResult`] accepts.
///
/// `heycode_core` bounds a result at 128 sources. Beyond that the list is a
/// bounded preview and [`ServerToolResult::output_count`] carries the true
/// total; no citation is lost either way, because each citation carries its own
/// URL and title.
const MAX_RESULT_SOURCES: usize = 128;

/// Which Google search types one grounded request enables.
///
/// Discovery `SearchTypes`: "The set of search types to enable. If not set, web
/// search is enabled by default." `webSearch` "Enables web search. Only text
/// results are returned"; `imageSearch` "Enables image search. Image bytes are
/// returned."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoogleSearchTypes {
    /// Send no `searchTypes`, taking the documented web-search default.
    Default,
    /// Name the enabled types explicitly.
    Explicit {
        /// Enable `searchTypes.webSearch`.
        web: bool,
        /// Enable `searchTypes.imageSearch`.
        image: bool,
    },
}

/// One committed Google Search grounding request.
///
/// Constructing this is what makes grounding "requested"; a projector built
/// without one reports [`GroundingStatus::NotRequested`] and can never report
/// anything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleSearchRequest {
    types: GoogleSearchTypes,
}

enum GroundingRequest {
    GoogleSearch(GoogleSearchRequest),
    External(crate::ExternalGroundingRequest),
}

impl GroundingRequest {
    fn logical(&self) -> &'static str {
        match self {
            Self::GoogleSearch(_) => GOOGLE_WEB_SEARCH_LOGICAL,
            Self::External(_) => crate::GOOGLE_EXTERNAL_GROUNDING_LOGICAL,
        }
    }

    fn provider_name(&self) -> &'static str {
        match self {
            Self::GoogleSearch(_) => GOOGLE_SEARCH_TOOL_NAME,
            Self::External(_) => crate::GOOGLE_EXTERNAL_GROUNDING_TOOL_NAME,
        }
    }

    fn call_input(&self) -> serde_json::Value {
        match self {
            Self::GoogleSearch(request) => request.call_input(),
            Self::External(request) => request.call_input(),
        }
    }
}

impl GoogleSearchRequest {
    /// Request the documented default, web search.
    #[must_use]
    pub const fn web() -> Self {
        Self {
            types: GoogleSearchTypes::Default,
        }
    }

    /// Request an explicit set of search types.
    ///
    /// # Errors
    /// Enabling no search type at all fails: an empty `searchTypes` object is
    /// not the documented way to take the default, and sending one would state
    /// a request Google does not define.
    pub fn with_types(web: bool, image: bool) -> Result<Self, GroundingError> {
        if !web && !image {
            return Err(GroundingError::InvalidRequest {
                message: "google search request must enable at least one search type",
            });
        }
        Ok(Self {
            types: GoogleSearchTypes::Explicit { web, image },
        })
    }

    /// Enabled search types.
    #[must_use]
    pub const fn types(&self) -> GoogleSearchTypes {
        self.types
    }

    /// The exact `tools[]` entry this request contributes.
    #[must_use]
    pub fn tool_entry(&self) -> serde_json::Value {
        let search = match self.types {
            GoogleSearchTypes::Default => serde_json::json!({}),
            GoogleSearchTypes::Explicit { web, image } => {
                let mut types = serde_json::Map::new();
                if web {
                    types.insert("webSearch".to_owned(), serde_json::json!({}));
                }
                if image {
                    types.insert("imageSearch".to_owned(), serde_json::json!({}));
                }
                serde_json::json!({ "searchTypes": serde_json::Value::Object(types) })
            }
        };
        serde_json::json!({ TOOL_FIELD: search })
    }

    /// The provider-owned, secret-free request option carrying that entry.
    ///
    /// This is the transport-independent form: a route that accepts a Gemini
    /// provider-option dialect merges [`Self::tool_entry`] into `tools[]`.
    ///
    /// # Errors
    /// A tool entry that cannot satisfy [`ProviderRequestOption`]'s identity or
    /// size bounds fails before it can reach a request.
    pub fn provider_option(&self) -> Result<ProviderRequestOption, GroundingError> {
        ProviderRequestOption::new(
            crate::catalog::GOOGLE_PROVIDER,
            GOOGLE_SEARCH_OPTION_KIND,
            serde_json::json!({ "tool": self.tool_entry() }),
        )
        .map_err(|_| GroundingError::InvalidRequest {
            message: "google search tool entry is not a valid provider request option",
        })
    }

    /// Normalized [`heycode_core::ServerToolCall`] input describing what heycode
    /// asked for.
    ///
    /// This deliberately records the *request*, never `webSearchQueries`. The
    /// discovery document defines that field as "Web search queries for the
    /// following-up web search" — forward-looking suggestion material, not a
    /// log of what the model ran. Reporting it as executed queries would state
    /// a fact no Google source supports, and storing it would cache Search
    /// Suggestions.
    #[must_use]
    pub fn call_input(&self) -> serde_json::Value {
        let types = match self.types {
            GoogleSearchTypes::Default => vec![serde_json::json!("web")],
            GoogleSearchTypes::Explicit { web, image } => {
                let mut names = Vec::new();
                if web {
                    names.push(serde_json::json!("web"));
                }
                if image {
                    names.push(serde_json::json!("image"));
                }
                names
            }
        };
        serde_json::json!({ "search_types": serde_json::Value::Array(types) })
    }
}

/// Whether a turn was grounded, and if so how many sources it cited.
///
/// The three variants exist so that "never asked", "asked and the model chose
/// not to ground" and "grounded against zero sources" can never render
/// identically. Collapsing any pair of them would report a fact heycode does not
/// hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroundingStatus {
    /// The `googleSearch` tool was not offered on this request.
    NotRequested,
    /// The tool was offered and the response carried no `groundingMetadata`.
    ///
    /// Grounding is model-elective: offering the tool does not oblige the model
    /// to search. This is that outcome, and it is not an error.
    NotGrounded,
    /// The response carried `groundingMetadata`.
    ///
    /// `sources` may be zero — grounding metadata that produced no citable
    /// chunk is still materially different from never having grounded.
    Grounded {
        /// Citable grounding chunks accumulated across the whole response.
        sources: u32,
    },
}

impl GroundingStatus {
    /// Whether the model actually grounded this turn.
    #[must_use]
    pub const fn is_grounded(&self) -> bool {
        matches!(self, Self::Grounded { .. })
    }
}

/// One accumulated grounding chunk, holding its position even when it is not
/// citable.
///
/// Positional identity is load-bearing: `groundingChunkIndices` index into this
/// list, so a filtered-out chunk would silently re-point every later citation
/// at the wrong source.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AccumulatedChunk {
    /// A chunk carrying a public source this projection can cite.
    Citable(ServerToolSource),
    /// A chunk the exact request does not project, or an external context whose
    /// URI is an identifier rather than a public URL, kept for index alignment.
    Opaque,
}

/// Which reading of `Segment.partIndex` produced an anchor.
///
/// The discovery document states the accumulation rule for
/// `groundingChunkIndices` — "the grounding_chunk_indices refer to the indices
/// across all responses" — and states nothing equivalent for `partIndex`, whose
/// only description is "The index of a Part object within its parent Content
/// object". Each streamed chunk carries its own `Content`, so that wording
/// reads as chunk-local; but supports commonly arrive in a late chunk while
/// describing spans of the whole answer, which only the response-global reading
/// can express. Both readings coincide for a single-chunk response.
///
/// Rather than pick one and hope, both are tried and the provider's own
/// `segment.text` decides when it can. If two distinct ranges remain valid,
/// the source is retained without a span. An anchored citation is published
/// only when one range reconstructs the exact text the provider identified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartIndexReading {
    /// `partIndex` counts from the first part of the chunk it arrived in.
    ChunkLocal,
    /// `partIndex` counts from the first part of the whole response.
    ResponseGlobal,
}

/// Accumulating projection of a streamed Gemini response's grounding metadata.
///
/// Streaming forces accumulation on the client, and the discovery document says
/// so twice. `groundingChunks`: "When streaming, this only contains the
/// grounding chunks that have not been included in the grounding metadata of
/// previous responses." `groundingChunkIndices`: "If the response is streaming,
/// the grounding_chunk_indices refer to the indices across all responses. It is
/// the client's responsibility to accumulate the grounding chunks from all
/// responses (while maintaining the same order)."
pub struct GroundingProjector {
    request: Option<GroundingRequest>,
    saw_metadata: bool,
    chunks: Vec<AccumulatedChunk>,
    citations: Vec<UrlCitation>,
    /// Every part of the response so far, in arrival order. `Some(text)` marks
    /// a part the adapter publishes as visible assistant output; `None` marks
    /// every other part — a thought summary, a function call, a
    /// signature-only part — none of which appear in the text a citation spans.
    parts: Vec<Option<String>>,
    suggestions: Option<String>,
}

/// Reports what the projector holds, never the provider content it holds.
///
/// A derived `Debug` would print `searchEntryPoint.renderedContent` into any
/// log line that formatted a projector, and a log is a cache: "You will not ...
/// cache, frame, syndicate, resell, analyze, train on, or otherwise learn from
/// Grounded Results or Search Suggestions."
impl std::fmt::Debug for GroundingProjector {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GroundingProjector")
            .field("status", &self.status())
            .field("citations", &self.citations.len())
            .field("parts", &self.parts.len())
            .field("has_search_suggestions", &self.suggestions.is_some())
            .finish()
    }
}

impl GroundingProjector {
    /// Build a projector for a request that did or did not offer the tool.
    #[must_use]
    pub const fn new(request: Option<GoogleSearchRequest>) -> Self {
        Self {
            request: match request {
                Some(request) => Some(GroundingRequest::GoogleSearch(request)),
                None => None,
            },
            saw_metadata: false,
            chunks: Vec::new(),
            citations: Vec::new(),
            parts: Vec::new(),
            suggestions: None,
        }
    }

    /// Build a projector for one Vertex external-API grounding request.
    ///
    /// Public HTTP(S) `retrievedContext.uri` rows become citations. Arbitrary
    /// identifiers remain positional opaque chunks because the shared citation
    /// vocabulary cannot represent a non-URL source safely.
    #[must_use]
    pub const fn external(request: crate::ExternalGroundingRequest) -> Self {
        Self {
            request: Some(GroundingRequest::External(request)),
            saw_metadata: false,
            chunks: Vec::new(),
            citations: Vec::new(),
            parts: Vec::new(),
            suggestions: None,
        }
    }

    /// Feed one streamed `candidate` object.
    ///
    /// Every candidate must be observed, grounded or not: the projector tracks
    /// the running visible-text offset across the whole response, and a skipped
    /// candidate would rebase every later citation short by its text.
    ///
    /// # Errors
    /// Metadata on a request that never offered the tool, a malformed
    /// `groundingMetadata` shape, a chunk index outside the accumulated list,
    /// and offsets that do not reconstruct the segment text they claim all
    /// fail. None of them can be normalized into a citation that is merely
    /// approximate: a span pointing at the wrong words is worse than no span.
    pub fn observe(&mut self, candidate: &serde_json::Value) -> Result<(), GroundingError> {
        let candidate = candidate
            .as_object()
            .ok_or(GroundingError::Malformed("candidate must be an object"))?;
        // Where this chunk's parts begin inside the accumulated response, taken
        // before they are appended: it is the origin of the chunk-local reading
        // of `partIndex`.
        let chunk_start = self.parts.len();
        self.append_parts(candidate)?;
        if let Some(metadata) = candidate
            .get("groundingMetadata")
            .filter(|value| !value.is_null())
        {
            if self.request.is_none() {
                return Err(GroundingError::Unsolicited);
            }
            let metadata = metadata.as_object().ok_or(GroundingError::Malformed(
                "groundingMetadata must be an object",
            ))?;
            self.saw_metadata = true;
            self.accumulate_chunks(metadata)?;
            self.accumulate_supports(metadata, chunk_start)?;
            self.capture_suggestions(metadata)?;
        }
        Ok(())
    }

    /// Append one candidate's `content.parts` to the accumulated response.
    fn append_parts(
        &mut self,
        candidate: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), GroundingError> {
        let Some(content) = candidate.get("content").filter(|value| !value.is_null()) else {
            return Ok(());
        };
        let content = content.as_object().ok_or(GroundingError::Malformed(
            "candidate content must be an object",
        ))?;
        let Some(parts) = content.get("parts").filter(|value| !value.is_null()) else {
            return Ok(());
        };
        let parts = parts.as_array().ok_or(GroundingError::Malformed(
            "candidate parts must be an array",
        ))?;
        for part in parts {
            let part = part.as_object().ok_or(GroundingError::Malformed(
                "candidate part must be an object",
            ))?;
            // A `thought: true` text part is a reasoning summary. The adapter
            // publishes it as a reasoning delta, so it is not in the text a
            // citation spans and must not shift a single offset.
            // <https://ai.google.dev/gemini-api/docs/generate-content/thinking>
            let thought = matches!(part.get("thought"), Some(serde_json::Value::Bool(true)));
            match part.get("text").filter(|value| !value.is_null()) {
                Some(text) if !thought => {
                    let text = text
                        .as_str()
                        .ok_or(GroundingError::Malformed("part text must be a string"))?;
                    self.parts.push(Some(text.to_owned()));
                }
                _ => self.parts.push(None),
            }
        }
        Ok(())
    }

    /// Visible bytes preceding `part` in the accumulated response.
    fn base_offset(&self, part: usize) -> usize {
        self.parts
            .iter()
            .take(part)
            .flatten()
            .map(String::len)
            .sum()
    }

    /// Append this chunk's new grounding chunks, preserving arrival order.
    fn accumulate_chunks(
        &mut self,
        metadata: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), GroundingError> {
        let Some(chunks) = metadata
            .get("groundingChunks")
            .filter(|value| !value.is_null())
        else {
            return Ok(());
        };
        let chunks = chunks.as_array().ok_or(GroundingError::Malformed(
            "groundingChunks must be an array",
        ))?;
        for chunk in chunks {
            let chunk = chunk.as_object().ok_or(GroundingError::Malformed(
                "grounding chunk must be an object",
            ))?;
            let source = self.chunk_source(chunk)?;
            self.chunks.push(source);
        }
        Ok(())
    }

    /// Project one `GroundingChunk` into a citable source, or mark it opaque.
    ///
    /// `Web` carries `uri` and `title`; `Image` carries `sourceUri`, "The web
    /// page URI for attribution", plus the page `title`. An external grounding
    /// request instead admits a public `RetrievedContext.uri`. Maps and source
    /// kinds not enabled by the exact request hold their slot without being
    /// cited.
    fn chunk_source(
        &self,
        chunk: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<AccumulatedChunk, GroundingError> {
        let Some(request) = self.request.as_ref() else {
            return Ok(AccumulatedChunk::Opaque);
        };
        let (uri_field, source, opaque_invalid_uri) = match request {
            GroundingRequest::GoogleSearch(_) => {
                let web = chunk
                    .get("web")
                    .filter(|value| !value.is_null())
                    .map(|web| ("uri", web, false));
                let image = chunk
                    .get("image")
                    .filter(|value| !value.is_null())
                    .map(|image| ("sourceUri", image, false));
                let Some(source) = web.or(image) else {
                    return Ok(AccumulatedChunk::Opaque);
                };
                source
            }
            GroundingRequest::External(_) => {
                let Some(source) = chunk
                    .get("retrievedContext")
                    .filter(|value| !value.is_null())
                else {
                    return Ok(AccumulatedChunk::Opaque);
                };
                ("uri", source, true)
            }
        };
        let source = source.as_object().ok_or(GroundingError::Malformed(
            "grounding chunk source must be an object",
        ))?;
        let Some(uri) = source
            .get(uri_field)
            .and_then(serde_json::Value::as_str)
            .filter(|uri| !uri.is_empty())
        else {
            // Both `uri` fields are `readOnly` output fields with no documented
            // guarantee of presence. A source with no URL cannot be cited, but
            // it still occupies its index.
            return Ok(AccumulatedChunk::Opaque);
        };
        let title = source
            .get("title")
            .and_then(serde_json::Value::as_str)
            .filter(|title| !title.is_empty());
        // A title that fails the neutral display bounds is dropped rather than
        // failing the turn: the URL is the citation, the title is decoration.
        match ServerToolSource::new(uri, title) {
            Ok(source) => Ok(AccumulatedChunk::Citable(source)),
            Err(_) => match title.and_then(|_| ServerToolSource::new(uri, None).ok()) {
                Some(source) => Ok(AccumulatedChunk::Citable(source)),
                None if opaque_invalid_uri => Ok(AccumulatedChunk::Opaque),
                None => Err(GroundingError::UnsafeSource),
            },
        }
    }

    /// Turn this chunk's grounding supports into anchored citations.
    fn accumulate_supports(
        &mut self,
        metadata: &serde_json::Map<String, serde_json::Value>,
        chunk_start: usize,
    ) -> Result<(), GroundingError> {
        let Some(supports) = metadata
            .get("groundingSupports")
            .filter(|value| !value.is_null())
        else {
            return Ok(());
        };
        let supports = supports.as_array().ok_or(GroundingError::Malformed(
            "groundingSupports must be an array",
        ))?;
        for support in supports {
            let support = support.as_object().ok_or(GroundingError::Malformed(
                "grounding support must be an object",
            ))?;
            let anchor = self.anchor(support, chunk_start)?;
            let indices = support
                .get("groundingChunkIndices")
                .filter(|value| !value.is_null());
            let indices = match indices {
                Some(indices) => indices
                    .as_array()
                    .ok_or(GroundingError::Malformed(
                        "groundingChunkIndices must be an array",
                    ))?
                    .as_slice(),
                None => &[],
            };
            for index in indices {
                let index = index
                    .as_u64()
                    .and_then(|index| usize::try_from(index).ok())
                    .ok_or(GroundingError::Malformed(
                        "grounding chunk index must be a non-negative integer",
                    ))?;
                // Out of range is a protocol violation, not a chunk to skip:
                // the accumulation contract says indices span every response,
                // so an unresolvable index means the accumulation is wrong.
                let chunk = self
                    .chunks
                    .get(index)
                    .ok_or(GroundingError::ChunkIndexOutOfRange)?;
                let AccumulatedChunk::Citable(source) = chunk else {
                    continue;
                };
                let citation = UrlCitation::new(
                    source.url(),
                    source.title(),
                    anchor.text.as_deref(),
                    anchor.start,
                    anchor.end,
                )
                .map_err(|_| GroundingError::UnsafeSource)?;
                self.citations.push(citation);
            }
        }
        Ok(())
    }

    /// Resolve one support's `segment` into a message-relative anchor.
    ///
    /// `Segment.startIndex`/`endIndex` are "measured in bytes ... from the
    /// start of the Part", so they are relative to one part rather than to the
    /// answer. Rebasing them onto the visible message is mandatory: copying
    /// them through unchanged is correct only for a single-part response, and
    /// wrong — silently — for every other one.
    ///
    /// Which part they are relative to is the ambiguity
    /// [`PartIndexReading`] describes. Both readings are tried and the
    /// provider's own `segment.text` decides between them when possible. Two
    /// distinct valid readings degrade to an unanchored source instead of
    /// shipping a span that may point at the wrong words.
    fn anchor(
        &self,
        support: &serde_json::Map<String, serde_json::Value>,
        chunk_start: usize,
    ) -> Result<SegmentAnchor, GroundingError> {
        let Some(segment) = support.get("segment").filter(|value| !value.is_null()) else {
            return Ok(SegmentAnchor::unanchored());
        };
        let segment = segment.as_object().ok_or(GroundingError::Malformed(
            "grounding segment must be an object",
        ))?;
        // Absent integers are proto3 defaults, and Google's JSON mapping omits
        // zero-valued fields. <https://protobuf.dev/programming-guides/json/>
        let part_index = Self::segment_index(segment, "partIndex")?;
        let start = Self::segment_index(segment, "startIndex")?;
        let end = Self::segment_index(segment, "endIndex")?;
        let declared = segment
            .get("text")
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.is_empty());
        if end <= start {
            // An empty span alongside declared text is a contradiction the
            // provider must not be given the benefit of the doubt on.
            if declared.is_some() {
                return Err(GroundingError::SegmentMismatch);
            }
            return Ok(SegmentAnchor::unanchored());
        }
        // Chunk-local is tried first because it is the literal reading of
        // "within its parent Content object"; response-global is the fallback
        // that a support arriving in a late chunk needs. They are the same
        // index for the first chunk of any response.
        let mut failure = None;
        let mut anchors = Vec::with_capacity(2);
        for reading in [
            PartIndexReading::ChunkLocal,
            PartIndexReading::ResponseGlobal,
        ] {
            let resolved = match reading {
                PartIndexReading::ChunkLocal => chunk_start.checked_add(part_index),
                PartIndexReading::ResponseGlobal => Some(part_index),
            };
            let Some(resolved) = resolved else {
                failure = failure.or(Some(GroundingError::SegmentPart));
                continue;
            };
            match self.anchor_at(resolved, start, end, declared) {
                Ok(anchor) if !anchors.contains(&anchor) => anchors.push(anchor),
                Ok(_) => {}
                Err(error) => failure = failure.or(Some(error)),
            }
        }
        match anchors.len() {
            0 => Err(failure.unwrap_or(GroundingError::SegmentPart)),
            1 => Ok(anchors.remove(0)),
            // The source remains useful, but neither valid range has enough
            // evidence to win. Publishing no span is honest; picking one would
            // attach the citation to provider text it may not support.
            _ => Ok(SegmentAnchor::unanchored()),
        }
    }

    /// Resolve one segment against one exact accumulated part index.
    ///
    /// The slice is compared against `segment.text` when the provider declared
    /// it. That comparison is the whole safety argument for reading offsets
    /// whose frame of reference the documentation leaves ambiguous.
    fn anchor_at(
        &self,
        part: usize,
        start: usize,
        end: usize,
        declared: Option<&str>,
    ) -> Result<SegmentAnchor, GroundingError> {
        let Some(Some(text)) = self.parts.get(part) else {
            return Err(GroundingError::SegmentPart);
        };
        // `get` rather than indexing: a range that splits a multi-byte
        // character yields `None` here and a panic under `&text[start..end]`,
        // and panicking is denied in this workspace for exactly this reason.
        let slice = text.get(start..end).ok_or(GroundingError::SegmentRange)?;
        if declared.is_some_and(|declared| declared != slice) {
            return Err(GroundingError::SegmentMismatch);
        }
        let base = self.base_offset(part);
        let rebased_start = base
            .checked_add(start)
            .and_then(|start| u32::try_from(start).ok())
            .ok_or(GroundingError::SegmentRange)?;
        let rebased_end = base
            .checked_add(end)
            .and_then(|end| u32::try_from(end).ok())
            .ok_or(GroundingError::SegmentRange)?;
        Ok(SegmentAnchor {
            text: Some(slice.to_owned()),
            start: Some(rebased_start),
            end: Some(rebased_end),
        })
    }

    /// Read one optional non-negative `Segment` index.
    fn segment_index(
        segment: &serde_json::Map<String, serde_json::Value>,
        field: &'static str,
    ) -> Result<usize, GroundingError> {
        match segment.get(field).filter(|value| !value.is_null()) {
            None => Ok(0),
            Some(value) => value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or(GroundingError::Malformed(
                    "grounding segment index must be a non-negative integer",
                )),
        }
    }

    /// Capture the Search Suggestion markup without letting it become durable.
    fn capture_suggestions(
        &mut self,
        metadata: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), GroundingError> {
        let Some(entry) = metadata
            .get("searchEntryPoint")
            .filter(|value| !value.is_null())
        else {
            return Ok(());
        };
        let entry = entry.as_object().ok_or(GroundingError::Malformed(
            "searchEntryPoint must be an object",
        ))?;
        if let Some(rendered) = entry
            .get("renderedContent")
            .and_then(serde_json::Value::as_str)
            .filter(|rendered| !rendered.is_empty())
        {
            self.suggestions = Some(rendered.to_owned());
        }
        Ok(())
    }

    /// Tri-state grounding outcome for this response.
    #[must_use]
    pub fn status(&self) -> GroundingStatus {
        if self.request.is_none() {
            return GroundingStatus::NotRequested;
        }
        if !self.saw_metadata {
            return GroundingStatus::NotGrounded;
        }
        GroundingStatus::Grounded {
            sources: u32::try_from(
                self.chunks
                    .iter()
                    .filter(|chunk| matches!(chunk, AccumulatedChunk::Citable(_)))
                    .count(),
            )
            .unwrap_or(u32::MAX),
        }
    }

    /// Search Suggestion markup for this response, if Google sent any.
    ///
    /// **This value must not be persisted.** It is
    /// `searchEntryPoint.renderedContent`, "Web content snippet that can be
    /// embedded in a web page or an app webview", and it is a Search Suggestion
    /// under Google's terms: "You will not ... cache, frame, syndicate, resell,
    /// analyze, train on, or otherwise learn from Grounded Results or Search
    /// Suggestions." The chat-history exemption covers "the text of the
    /// Grounded Result(s)" and does not reach Search Suggestions.
    ///
    /// It is exposed so a host that *can* display it — a web or webview
    /// surface — may satisfy the display obligation the terms impose. A
    /// terminal cannot, and [`ProjectedGrounding`] therefore has nowhere to put
    /// this string: the durable path is structurally unable to carry it.
    #[must_use]
    pub fn search_suggestions(&self) -> Option<&str> {
        self.suggestions.as_deref()
    }

    /// Project the accumulated grounding into durable, replayable events.
    ///
    /// A response that never grounded projects nothing — no empty call, no
    /// zero-source result — because a `server-tool/call` in the session log
    /// asserts the tool ran.
    ///
    /// # Errors
    /// A response id that cannot form a valid call identity, or sources that
    /// cannot satisfy the neutral result bounds, fail rather than persisting a
    /// half-formed attribution.
    pub fn project(
        &self,
        output_index: u32,
        response_id: &str,
    ) -> Result<ProjectedGrounding, GroundingError> {
        let (Some(request), true) = (self.request.as_ref(), self.saw_metadata) else {
            return Ok(ProjectedGrounding {
                status: self.status(),
                events: Vec::new(),
            });
        };
        let call_id = CallId::from_raw(format!("{response_id}/grounding"));
        let call = heycode_core::ServerToolCall::new(
            call_id.clone(),
            request.logical(),
            request.provider_name(),
            request.call_input(),
        )
        .map_err(|_| GroundingError::InvalidCallIdentity)?;
        let sources = self
            .chunks
            .iter()
            .filter_map(|chunk| match chunk {
                AccumulatedChunk::Citable(source) => Some(source.clone()),
                AccumulatedChunk::Opaque => None,
            })
            .collect::<Vec<_>>();
        let total = u32::try_from(sources.len()).unwrap_or(u32::MAX);
        let mut preview = sources;
        preview.truncate(MAX_RESULT_SOURCES);
        let result = ServerToolResult::success(call_id, Some(total), preview)
            .map_err(|_| GroundingError::UnsafeSource)?;
        let mut events = Vec::with_capacity(self.citations.len().saturating_add(2));
        events.push(InferenceEvent::ServerToolCall { output_index, call });
        events.push(InferenceEvent::ServerToolResult {
            output_index,
            result,
        });
        for citation in &self.citations {
            events.push(InferenceEvent::Citation {
                output_index,
                citation: citation.clone(),
            });
        }
        Ok(ProjectedGrounding {
            status: self.status(),
            events,
        })
    }
}

/// One support's resolved anchor into the visible assistant text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SegmentAnchor {
    text: Option<String>,
    start: Option<u32>,
    end: Option<u32>,
}

impl SegmentAnchor {
    /// A citation with a source but no span.
    const fn unanchored() -> Self {
        Self {
            text: None,
            start: None,
            end: None,
        }
    }
}

/// The durable half of one grounded turn.
///
/// This type is deliberately unable to carry Search Suggestion markup,
/// retrieval queries, or retrieved snippets. Google Search projections retain
/// only fields covered by the chat-history design above; external projections
/// retain only public source links owned by the caller's retrieval result.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectedGrounding {
    status: GroundingStatus,
    events: Vec<InferenceEvent>,
}

impl ProjectedGrounding {
    /// Tri-state grounding outcome.
    #[must_use]
    pub const fn status(&self) -> GroundingStatus {
        self.status
    }

    /// Events to publish, in order: one call, one result, then one citation per
    /// attributed span.
    #[must_use]
    pub fn events(&self) -> &[InferenceEvent] {
        &self.events
    }

    /// Take the events for publication.
    #[must_use]
    pub fn into_events(self) -> Vec<InferenceEvent> {
        self.events
    }
}

/// Re-anchor one citation against text a renderer has since reflowed.
///
/// Byte offsets are only valid against the exact bytes they were measured on.
/// A renderer that wraps, indents or re-joins the assistant text invalidates
/// them, silently — the span still resolves, it just points at the wrong words.
/// That is why every anchored citation this module produces also carries
/// [`heycode_core::UrlCitation::cited_text`]: the excerpt is content, not
/// position, and it survives any transformation that preserves the words.
///
/// Returns the byte range of the citation's excerpt inside `rendered`, or
/// `None` when the citation has no excerpt, the excerpt is not present, or a
/// reflow left more than one possible anchor.
/// Callers that get `None` should render the citation as a source without a
/// span rather than falling back to the stored offsets.
#[must_use]
pub fn reanchor(citation: &UrlCitation, rendered: &str) -> Option<(usize, usize)> {
    let excerpt = citation.cited_text()?;
    // The stored offsets are tried first and confirmed, not trusted: when the
    // text did not move this is exact, and when it did the confirmation fails
    // and the search below finds the excerpt where it actually landed.
    if let (Some(start), Some(end)) = (citation.start_index(), citation.end_index()) {
        let start = usize::try_from(start).ok()?;
        let end = usize::try_from(end).ok()?;
        if rendered.get(start..end) == Some(excerpt) {
            return Some((start, end));
        }
    }
    let mut matches = rendered
        .char_indices()
        .filter_map(|(index, _)| rendered[index..].starts_with(excerpt).then_some(index));
    let start = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some((start, start.saturating_add(excerpt.len())))
}

/// Grounding request or projection failure.
///
/// Every message is structural. None of them carries provider content, a URL or
/// a fragment of the model's answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GroundingError {
    /// The grounding request itself is invalid.
    #[error("invalid google search grounding request: {message}")]
    InvalidRequest {
        /// Safe structural detail.
        message: &'static str,
    },
    /// The response shape does not match the documented schema.
    #[error("invalid google grounding metadata: {0}")]
    Malformed(&'static str),
    /// Grounding metadata arrived for a request that never offered the tool.
    #[error("google grounding metadata arrived for a request that did not offer the tool")]
    Unsolicited,
    /// A support named a chunk outside the accumulated list.
    #[error("google grounding chunk index is outside the accumulated chunk list")]
    ChunkIndexOutOfRange,
    /// A segment named a part that carries no visible text.
    #[error("google grounding segment names a part with no visible text")]
    SegmentPart,
    /// Segment offsets fall outside the part or split a character.
    #[error("google grounding segment offsets are not a valid range in their part")]
    SegmentRange,
    /// Segment text and segment offsets describe different spans.
    #[error("google grounding segment text disagrees with its byte offsets")]
    SegmentMismatch,
    /// A source could not satisfy the neutral citation safety bounds.
    #[error("google grounding source is not a safe public citation")]
    UnsafeSource,
    /// The response id cannot form a valid server-tool call identity.
    #[error("google grounding call identity is invalid")]
    InvalidCallIdentity,
}

/// Register Google Search grounding as the provider-native implementation of
/// the logical `web_search` capability.
///
/// N01's registry resolves one implementation per logical capability, and a
/// provider-native candidate wins for its own provider under the default
/// prefer-native policy. Registering here rather than assuming the route
/// supports grounding is what lets a `local-only` policy pick a client
/// implementation instead, and what lets the absence of this plugin mean
/// exactly "this build does not offer Google Search".
#[must_use]
pub fn google_search_native_tools_plugin() -> Box<dyn heycode_core::Plugin> {
    struct GoogleSearchNativeToolsPlugin;

    impl heycode_core::Plugin for GoogleSearchNativeToolsPlugin {
        fn name(&self) -> &'static str {
            "native-google"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Tool],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::NativeTool,
                GOOGLE_SEARCH_IMPLEMENTATION,
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_native_tools::SERVICE_NATIVE_TOOLS]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let registry = context
                .get::<heycode_native_tools::NativeToolRegistry>(
                    heycode_native_tools::SERVICE_NATIVE_TOOLS,
                )
                .ok_or_else(|| {
                    heycode_core::CoreError::other("native-tools service type mismatch")
                })?;
            let implementation = heycode_native_tools::NativeToolImplementation::new(
                GOOGLE_WEB_SEARCH_LOGICAL,
                GOOGLE_SEARCH_IMPLEMENTATION,
                heycode_core::NativeToolImplementationKind::Provider,
                Some(crate::catalog::GOOGLE_PROVIDER.to_owned()),
                100,
            )
            .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            registry
                .register(context, implementation)
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))
        }
    }

    Box::new(GoogleSearchNativeToolsPlugin)
}
