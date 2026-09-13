//! DOC03: generate the provider/model capability reference from code.
//!
//! The row's acceptance is "reference generated from descriptors, not hand
//! copied", so the value here is not the Markdown — it is that the Markdown
//! cannot drift from the code without something failing.
//!
//! Two mechanisms carry that, and neither is a convention anyone has to
//! remember:
//!
//! 1. [`capability_rows`] destructures [`ModelCapabilities`] **exhaustively**.
//!    Rust has no reflection, so a hand-written list of capability names would
//!    be exactly the hand copying this row forbids. An exhaustive `let`
//!    pattern is the compile-time substitute: adding a field to
//!    `ModelCapabilities` fails to compile here until the reference lists it.
//! 2. `docs/reference/capabilities.md` is checked in, and a test regenerates
//!    it and compares. A stale reference is a red build, not a stale document.
//!
//! What this reference deliberately does NOT do is enumerate every model of
//! every provider. Those descriptors come from `ModelCatalog::fetch`, which is
//! network-bound and credentialed; generating a document from it would produce
//! a file that says something different on every machine and cannot be
//! regenerated in CI. The reference states the vocabulary, the semantics, and
//! the per-provider routing facts that ARE static, and says plainly that
//! per-model capability data is live.

use heycode_llm::{CapabilitySupport, ModelCapabilities};

/// Number of capability fields on [`ModelCapabilities`].
///
/// Not a free-standing constant to be kept in sync by hand: the array returned
/// by [`capability_rows`] is built from an exhaustive destructuring, so this
/// length is checked by the compiler at the point the array is constructed.
pub const CAPABILITY_COUNT: usize = 8;

/// One capability's stable name and its doc-comment summary.
///
/// The summary duplicates the doc comment on the corresponding
/// `ModelCapabilities` field. That duplication is the one thing here a compiler
/// cannot check — Rust does not expose doc comments to code — so
/// `capability_summaries_match_the_field_docs` checks it against the source
/// text instead. It is the weakest link in this module and is tested as such.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityRow {
    /// Stable identifier, matching the struct field name.
    pub name: &'static str,
    /// One-line description of what the capability means.
    pub summary: &'static str,
}

/// Every capability on [`ModelCapabilities`], paired with a sample value.
///
/// The exhaustive `let` below is load-bearing. Adding a field to
/// `ModelCapabilities` without extending this function is a compile error,
/// which is what stops the generated reference from silently omitting a
/// capability — the precise failure mode a hand-maintained table has.
#[must_use]
pub fn capability_rows(
    capabilities: &ModelCapabilities,
) -> [(CapabilityRow, CapabilitySupport); CAPABILITY_COUNT] {
    let ModelCapabilities {
        tools,
        reasoning,
        image_input,
        document_input,
        structured_output,
        native_web,
        native_compaction,
        prompt_cache,
    } = *capabilities;
    [
        (
            CapabilityRow {
                name: "tools",
                summary: "Function/tool calling.",
            },
            tools,
        ),
        (
            CapabilityRow {
                name: "reasoning",
                summary: "Reasoning/thinking state.",
            },
            reasoning,
        ),
        (
            CapabilityRow {
                name: "image_input",
                summary: "Image input.",
            },
            image_input,
        ),
        (
            CapabilityRow {
                name: "document_input",
                summary: "Native document/file input.",
            },
            document_input,
        ),
        (
            CapabilityRow {
                name: "structured_output",
                summary: "Schema-constrained structured output.",
            },
            structured_output,
        ),
        (
            CapabilityRow {
                name: "native_web",
                summary: "Provider-hosted web search/fetch.",
            },
            native_web,
        ),
        (
            CapabilityRow {
                name: "native_compaction",
                summary: "Provider-native compaction/context editing.",
            },
            native_compaction,
        ),
        (
            CapabilityRow {
                name: "prompt_cache",
                summary: "Prompt-prefix caching.",
            },
            prompt_cache,
        ),
    ]
}

/// Render one tri-state value.
///
/// The three renderings must be mutually distinct, and in particular `Unknown`
/// must not render as anything a reader could mistake for `Unsupported`. A
/// table that prints "—" for both states the thing this codebase spends most
/// of its effort refusing to state: that absence of evidence is evidence of
/// absence.
#[must_use]
pub const fn support_cell(support: CapabilitySupport) -> &'static str {
    match support {
        CapabilitySupport::Supported => "yes",
        CapabilitySupport::Unsupported => "no",
        CapabilitySupport::Unknown => "unknown",
    }
}

/// How a provider can be reached, as the composed default world sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRoute {
    /// Selectable as `[llm] provider`; requests can be sent.
    Inference,
    /// Ships a profile, catalog or authorization flow, but direct inference is
    /// unavailable, so it cannot be selected as `[llm] provider`.
    ConfiguredOnly,
}

impl ProviderRoute {
    /// Stable word for the reference table.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Inference => "inference",
            Self::ConfiguredOnly => "configured only",
        }
    }
}

/// Every provider heycode knows, with how it can actually be reached.
///
/// Built from the same two constants the provider-selection error message
/// reads, so the reference cannot claim a provider is selectable while the
/// binary refuses it.
#[must_use]
pub fn provider_routes() -> Vec<(&'static str, ProviderRoute)> {
    let mut routes: Vec<(&'static str, ProviderRoute)> = crate::INFERENCE_PROVIDERS
        .iter()
        .map(|provider| (*provider, ProviderRoute::Inference))
        .chain(
            crate::CONFIGURED_ONLY_PROVIDERS
                .iter()
                .map(|provider| (*provider, ProviderRoute::ConfiguredOnly)),
        )
        .collect();
    routes.sort_by(|left, right| left.0.cmp(right.0));
    routes
}

/// Render the complete capability reference as Markdown.
#[must_use]
pub fn render_capability_reference() -> String {
    let mut out = String::new();
    out.push_str(
        "<!-- GENERATED by `heycode-cli`'s docs_cli module (DOC03). Do not edit by hand. -->\n\
         <!-- Regenerate: `cargo test -p heycode-cli capability_reference` reports the diff. -->\n\n\
         # Provider and model capability reference\n\n\
         Generated from the descriptors in code, not hand copied. If this file and\n\
         the code disagree, the build fails rather than the file quietly aging.\n\n",
    );

    out.push_str(
        "## Capability evidence is tri-state\n\n\
         Every capability below is `Supported`, `Unsupported`, or `Unknown`, and the\n\
         three are different claims:\n\n\
         | Value | Rendered | Means |\n\
         | --- | --- | --- |\n",
    );
    for support in [
        CapabilitySupport::Supported,
        CapabilitySupport::Unsupported,
        CapabilitySupport::Unknown,
    ] {
        let meaning = match support {
            CapabilitySupport::Supported => "The provider or model explicitly supports it.",
            CapabilitySupport::Unsupported => "The provider or model explicitly does not.",
            CapabilitySupport::Unknown => {
                "No trustworthy evidence yet. This is **not** a synonym for \"no\"."
            }
        };
        out.push_str(&format!(
            "| `{support:?}` | `{}` | {meaning} |\n",
            support_cell(support)
        ));
    }
    out.push_str(
        "\n`Unknown` never becomes `Supported` by inference. A capability is marked\n\
         supported because a provider evidenced it, not because a similar model has it\n\
         or because the name suggests it.\n\n\
         One nuance that has come up repeatedly and is easy to get backwards: a\n\
         **gateway-provided** capability may legitimately be marked supported by the\n\
         layer that provides it — a router that documents fallback web search for any\n\
         model can honour that claim whatever the upstream model does. A\n\
         **model-intrinsic** capability, such as image input, cannot be supplied by any\n\
         layer below the model, so no gateway's documentation can evidence it.\n\n",
    );

    out.push_str("## Capabilities\n\n| Capability | Meaning |\n| --- | --- |\n");
    for (row, _) in capability_rows(&ModelCapabilities::unknown()) {
        out.push_str(&format!("| `{}` | {} |\n", row.name, row.summary));
    }

    out.push_str(
        "\n## Providers\n\n\
         Knowing a provider is not the same as being able to send it a request. A\n\
         provider marked `configured only` ships a profile, a catalog, or an\n\
         authorization flow, but direct inference is unavailable, so it cannot be\n\
         selected as `[llm] provider`. Z.ai Coding Plan restricts usage to officially\n\
         supported tools; heycode is not listed. Use the separate general API route\n\
         with its own key. See <https://docs.z.ai/devpack/usage-policy>.\n\n\
         | Provider | Route |\n| --- | --- |\n",
    );
    for (provider, route) in provider_routes() {
        out.push_str(&format!("| `{provider}` | {} |\n", route.label()));
    }

    out.push_str(
        "\n## Per-model capability data is live, and is not in this file\n\n\
         Model lists and their capability snapshots come from each provider's\n\
         `ModelCatalog::fetch`, which is a network call against the provider's own\n\
         model-listing endpoint and usually needs a credential. A table of models\n\
         checked in here would state one machine's cache as though it were a fact\n\
         about heycode, and could not be regenerated in CI.\n\n\
         To see the live capability data for the models you can actually reach, use\n\
         the model picker or the catalog surfaces in the running application. A model\n\
         absent from a live catalog is described by a conservative descriptor whose\n\
         every capability is `Unknown` — again, not `Unsupported`.\n",
    );

    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// The whole point of the tri-state is lost if two states print alike.
    #[test]
    fn the_three_support_states_render_distinctly() {
        let supported = support_cell(CapabilitySupport::Supported);
        let unsupported = support_cell(CapabilitySupport::Unsupported);
        let unknown = support_cell(CapabilitySupport::Unknown);
        assert_ne!(supported, unsupported);
        assert_ne!(unsupported, unknown, "unknown must not read as unsupported");
        assert_ne!(supported, unknown);
        for cell in [supported, unsupported, unknown] {
            assert!(!cell.trim().is_empty(), "a blank cell renders as absence");
            assert_ne!(cell, "-");
            assert_ne!(cell, "—");
        }
    }

    #[test]
    fn every_capability_field_reaches_the_reference() {
        let rows = capability_rows(&ModelCapabilities::unknown());
        assert_eq!(rows.len(), CAPABILITY_COUNT);
        let rendered = render_capability_reference();
        for (row, _) in rows {
            assert!(!row.name.is_empty());
            assert!(!row.summary.is_empty());
            assert!(
                rendered.contains(&format!("| `{}` |", row.name)),
                "capability `{}` is missing from the generated reference",
                row.name
            );
        }
    }

    #[test]
    fn capability_names_are_unique() {
        let rows = capability_rows(&ModelCapabilities::unknown());
        let mut names: Vec<&str> = rows.iter().map(|(row, _)| row.name).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "duplicate capability name");
    }

    /// `capability_rows` must report the value it was given, not a constant.
    ///
    /// Without this, a body that returned `Unknown` for every field would pass
    /// every other test in this module, because they all pass
    /// `ModelCapabilities::unknown()`.
    #[test]
    fn capability_rows_report_the_values_they_are_given() {
        let mut capabilities = ModelCapabilities::unknown();
        capabilities.tools = CapabilitySupport::Supported;
        capabilities.image_input = CapabilitySupport::Unsupported;
        let rows = capability_rows(&capabilities);
        for (row, support) in rows {
            let expected = match row.name {
                "tools" => CapabilitySupport::Supported,
                "image_input" => CapabilitySupport::Unsupported,
                _ => CapabilitySupport::Unknown,
            };
            assert_eq!(support, expected, "wrong value reported for `{}`", row.name);
        }
    }

    /// The reference must not claim a provider is selectable when the binary
    /// would refuse it.
    #[test]
    fn provider_routes_agree_with_the_constants_the_binary_enforces() {
        let routes = provider_routes();
        assert_eq!(
            routes.len(),
            crate::INFERENCE_PROVIDERS.len() + crate::CONFIGURED_ONLY_PROVIDERS.len()
        );
        for (provider, route) in &routes {
            let expected = if crate::INFERENCE_PROVIDERS.contains(provider) {
                ProviderRoute::Inference
            } else {
                ProviderRoute::ConfiguredOnly
            };
            assert_eq!(*route, expected, "wrong route for `{provider}`");
        }
        for provider in crate::CONFIGURED_ONLY_PROVIDERS {
            assert!(
                !crate::INFERENCE_PROVIDERS.contains(provider),
                "`{provider}` is in both lists, so its route is ambiguous"
            );
        }
    }

    #[test]
    fn a_configured_only_provider_is_never_rendered_as_an_inference_route() {
        let rendered = render_capability_reference();
        for provider in crate::CONFIGURED_ONLY_PROVIDERS {
            assert!(
                rendered.contains(&format!("| `{provider}` | configured only |")),
                "`{provider}` must be shown as configured only"
            );
            assert!(
                !rendered.contains(&format!("| `{provider}` | inference |")),
                "`{provider}` must not be shown as an inference route"
            );
        }
        for provider in crate::INFERENCE_PROVIDERS {
            assert!(
                rendered.contains(&format!("| `{provider}` | inference |")),
                "`{provider}` must be shown as an inference route"
            );
        }
    }

    /// The reference must state the tri-state distinction, not just use it.
    #[test]
    fn the_reference_explains_that_unknown_is_not_no() {
        let rendered = render_capability_reference();
        assert!(rendered.contains("not** a synonym for \"no\""));
        assert!(rendered.contains("`Unknown` never becomes `Supported` by inference."));
        assert!(rendered.contains("gateway-provided"));
        assert!(rendered.contains("model-intrinsic"));
    }

    /// Per-model data is live; the reference must say so rather than ship an
    /// empty table that reads as "no models".
    #[test]
    fn the_reference_states_that_per_model_data_is_live() {
        let rendered = render_capability_reference();
        assert!(rendered.contains("Per-model capability data is live"));
        assert!(rendered.contains("ModelCatalog::fetch"));
    }

    /// The summaries duplicate doc comments the compiler cannot check, so
    /// check them against the source text.
    ///
    /// This is the one hand-copied thing in the module. It is copied because
    /// Rust does not expose doc comments to code, and it is tested because
    /// "copied and untested" is what this row exists to prevent.
    #[test]
    fn capability_summaries_match_the_field_docs() {
        let source = include_str!("../../heycode-llm/src/catalog.rs");
        let start = source
            .find("pub struct ModelCapabilities {")
            .expect("ModelCapabilities is declared in heycode-llm's catalog module");
        let body = &source[start..];
        let end = body.find("\n}").expect("the struct declaration is closed");
        let body = &body[..end];
        for (row, _) in capability_rows(&ModelCapabilities::unknown()) {
            assert!(
                body.contains(&format!("/// {}", row.summary)),
                "summary for `{}` does not match its field doc in heycode-llm; \
                 the reference and the source have drifted",
                row.name
            );
            assert!(
                body.contains(&format!("pub {}:", row.name)),
                "`{}` is not a field of ModelCapabilities",
                row.name
            );
        }
    }

    /// The checked-in reference must match what the generator produces.
    ///
    /// This is the mechanism that makes "generated, not hand copied" a fact
    /// rather than an intention. Editing the Markdown by hand, or changing the
    /// code without regenerating, turns the build red.
    ///
    /// Regenerate with `HEYCODE_REGENERATE_DOCS=1 cargo test -p heycode-cli
    /// docs_cli`. The env var writes the file and the test then passes; it is
    /// deliberately opt-in, because a test that silently rewrites its own
    /// expectation can never fail.
    #[test]
    fn the_checked_in_reference_matches_the_generator() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/reference/capabilities.md");
        let generated = render_capability_reference();
        if std::env::var_os("HEYCODE_REGENERATE_DOCS").is_some() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create the reference directory");
            }
            std::fs::write(&path, &generated).expect("write the regenerated reference");
            return;
        }
        let checked_in = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "cannot read {}: {error}. Regenerate with \
                 HEYCODE_REGENERATE_DOCS=1 cargo test -p heycode-cli docs_cli",
                path.display()
            )
        });
        assert_eq!(
            checked_in,
            generated,
            "{} is stale. Regenerate with HEYCODE_REGENERATE_DOCS=1 cargo test -p heycode-cli docs_cli",
            path.display()
        );
    }

    /// Every field of the struct is listed, not merely every listed field.
    ///
    /// The previous test proves each name we render exists. This one proves we
    /// render each name that exists, which is the direction that catches a
    /// field added upstream — belt and braces around the exhaustive `let`,
    /// which already fails to compile in that case.
    #[test]
    fn no_capability_field_is_missing_from_the_reference() {
        let source = include_str!("../../heycode-llm/src/catalog.rs");
        let declaration = "pub struct ModelCapabilities {";
        let start = source
            .find(declaration)
            .expect("ModelCapabilities is declared in heycode-llm's catalog module")
            + declaration.len();
        let body = &source[start..];
        let end = body.find("\n}").expect("the struct declaration is closed");
        let body = &body[..end];
        // Field lines only: the declaration line itself is excluded above, and
        // a `pub ` prefix with a `:` is what distinguishes a field from a doc
        // comment or an attribute.
        let declared: Vec<&str> = body
            .lines()
            .filter_map(|line| line.trim().strip_prefix("pub "))
            .filter_map(|line| line.split_once(':'))
            .map(|(name, _)| name)
            .collect();
        assert_eq!(
            declared.len(),
            CAPABILITY_COUNT,
            "ModelCapabilities declares {} fields but the reference lists {CAPABILITY_COUNT}",
            declared.len()
        );
        let rendered: Vec<&str> = capability_rows(&ModelCapabilities::unknown())
            .iter()
            .map(|(row, _)| row.name)
            .collect();
        for field in declared {
            assert!(
                rendered.contains(&field),
                "field `{field}` is missing from the generated reference"
            );
        }
    }
}
