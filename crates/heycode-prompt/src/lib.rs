//! heycode-prompt — system prompt assembly.
//!
//! Plugins register named, ordered sections; [`PromptRegistry::render`] folds
//! them into the deterministic standing instructions sent ahead of every
//! request. Sections are pure functions of the [`RenderContext`]: same inputs,
//! same prompt, byte for byte.

pub mod instructions;

use heycode_core::Plugin;

/// Deterministic prompt-section registry service.
pub const SERVICE_PROMPT: heycode_core::ServiceKey = heycode_core::ServiceKey::new("prompt");

/// Inputs a section may consult when rendering.
#[derive(Debug, Clone)]
pub struct RenderContext {
    /// Working directory of the current session.
    pub cwd: std::path::PathBuf,
    /// Active model id (e.g. `deepseek-chat`).
    pub model: String,
    /// Registered tool names in registry order.
    pub tool_names: Vec<String>,
    /// Whether plan mode is currently active for this session.
    pub plan_active: bool,
}

type SectionFn = std::sync::Arc<dyn Fn(&RenderContext) -> String + Send + Sync>;

struct Section {
    order: i32,
    name: &'static str,
    render: SectionFn,
}

/// Ordered collection of prompt sections.
#[derive(Default)]
pub struct PromptRegistry {
    sections: Vec<Section>,
    late: std::sync::Mutex<Vec<Section>>,
    /// Labels of the project-instruction files rendered into the prompt, for
    /// `/status` and `/context`.
    instruction_sources: Vec<String>,
}

impl PromptRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a section; duplicate names within the registry fail loud.
    ///
    /// # Errors
    /// Returns an error string when `name` is already registered.
    pub fn section(
        &mut self,
        name: &'static str,
        order: i32,
        render: impl Fn(&RenderContext) -> String + Send + Sync + 'static,
    ) -> Result<(), String> {
        if self.sections.iter().any(|s| s.name == name) {
            return Err(format!("prompt section `{name}` already registered"));
        }
        self.sections.push(Section {
            order,
            name,
            render: std::sync::Arc::new(render),
        });
        Ok(())
    }

    /// Register after publication (late plugins). Duplicate names fail loud.
    ///
    /// # Errors
    /// Returns an error string when `name` exists anywhere in the registry.
    pub fn section_shared(
        &self,
        name: &'static str,
        order: i32,
        render: impl Fn(&RenderContext) -> String + Send + Sync + 'static,
    ) -> Result<(), String> {
        if self.names().contains(&name) {
            return Err(format!("prompt section `{name}` already registered"));
        }
        if let Ok(mut late) = self.late.lock() {
            late.push(Section {
                order,
                name,
                render: std::sync::Arc::new(render),
            });
            return Ok(());
        }
        Err(format!("prompt registry locked; cannot register `{name}`"))
    }

    /// Fold sections into the final prompt: ascending `order`, ties broken by
    /// registration order, joined by blank lines.
    #[must_use]
    pub fn render(&self, cx: &RenderContext) -> String {
        self.render_excluding(cx, &[])
    }

    /// Render while omitting exact section names for a scoped child context.
    #[must_use]
    pub fn render_excluding(&self, cx: &RenderContext, excluded: &[&str]) -> String {
        self.render_sections_excluding(cx, excluded)
            .into_iter()
            .map(|(_, text)| text)
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Render named nonempty sections in exactly the same order as the wire prompt.
    /// Names are provenance, not text inferred from headings in user guidance.
    #[must_use]
    pub fn render_sections_excluding(
        &self,
        cx: &RenderContext,
        excluded: &[&str],
    ) -> Vec<(&'static str, String)> {
        self.render_sections_overriding(cx, excluded, &[])
    }

    /// Replace owner-supplied section text without changing its registered order
    /// or rendering the stale captured section. Empty replacements remove it.
    #[must_use]
    pub fn render_sections_overriding(
        &self,
        cx: &RenderContext,
        excluded: &[&str],
        overrides: &[(&str, String)],
    ) -> Vec<(&'static str, String)> {
        let mut entries: Vec<(i32, &'static str, SectionFn)> = self
            .sections
            .iter()
            .filter(|sec| !excluded.contains(&sec.name))
            .map(|sec| (sec.order, sec.name, sec.render.clone()))
            .collect();
        if let Ok(late) = self.late.lock() {
            entries.extend(
                late.iter()
                    .filter(|sec| !excluded.contains(&sec.name))
                    .map(|sec| (sec.order, sec.name, sec.render.clone())),
            );
        }
        entries.sort_by_key(|(order, _, _)| *order);
        entries
            .into_iter()
            .map(|(_, name, render)| {
                (
                    name,
                    overrides
                        .iter()
                        .find(|(key, _)| *key == name)
                        .map_or_else(|| render(cx), |(_, text)| text.clone()),
                )
            })
            .filter(|(_, text)| !text.trim().is_empty())
            .collect()
    }

    /// Labels of the instruction files the prompt carries, in render order.
    #[must_use]
    pub fn instruction_sources(&self) -> Vec<String> {
        self.instruction_sources.clone()
    }

    /// Registered names in registration order (diagnostics).
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        let mut all: Vec<&'static str> = self.sections.iter().map(|s| s.name).collect();
        if let Ok(late) = self.late.lock() {
            all.extend(late.iter().map(|s| s.name));
        }
        all
    }
}

/// The default prompt plugin: identity + environment sections.
///
/// Order bands (AGENTS §"prompt bands"): `-100` identity, `0` persona/env,
/// `100+` per-tool guidance added by other plugins via `ctx.get("prompt")`.
#[must_use]
pub fn prompt_plugin() -> Box<dyn Plugin> {
    prompt_plugin_with_instructions(instructions::InstructionSources::default())
}

/// The prompt plugin, also rendering the user's project instructions.
///
/// `sources` says where `AGENTS.md` / `CLAUDE.md` may be read from; the
/// composition root decides that from workspace trust. Files are read once at
/// composition, so the prompt stays a pure function of its inputs and the
/// `/status` source list matches what the model sees.
#[must_use]
pub fn prompt_plugin_with_instructions(
    sources: instructions::InstructionSources,
) -> Box<dyn Plugin> {
    struct PromptPlugin(instructions::InstructionSources);
    impl Plugin for PromptPlugin {
        fn name(&self) -> &'static str {
            "prompt"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "prompt",
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::PromptSection,
                ],
            )
        }
        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            [
                "identity",
                "environment",
                "user-instructions",
                "project-instructions",
            ]
            .into_iter()
            .map(|name| {
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::PromptSection,
                    name,
                )
            })
            .collect()
        }
        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_PROMPT]
        }
        fn apply(&self, ctx: &mut heycode_core::Context) -> Result<(), heycode_core::CoreError> {
            let mut registry = PromptRegistry::new();
            registry
                .section("identity", -100, |cx| {
                    format!(
                        "You are heycode, an expert software engineering agent running {} in {}.",
                        cx.model,
                        cx.cwd.display()
                    )
                })
                .map_err(heycode_core::CoreError::other)?; // impossible: fresh registry
            registry
                .section("environment", 0, |cx| {
                    format!(
                        "# Environment\n- Working directory: {}\n- Model: {}\n- Platform: {} ({})",
                        cx.cwd.display(),
                        cx.model,
                        std::env::consts::OS,
                        std::env::consts::ARCH,
                    )
                })
                .map_err(heycode_core::CoreError::other)?;
            let files = instructions::discover_scoped_instructions(&self.0);
            registry.instruction_sources = files
                .user
                .iter()
                .chain(&files.project)
                .map(|file| file.label.clone())
                .collect();
            let user = instructions::render_instructions(&files.user);
            let project = instructions::render_instructions(&files.project);
            registry
                .section("user-instructions", 48, move |_cx| user.clone())
                .map_err(heycode_core::CoreError::other)?;
            registry
                .section("project-instructions", 50, move |_cx| project.clone())
                .map_err(heycode_core::CoreError::other)?;
            ctx.provide(SERVICE_PROMPT, "prompt", registry)?;
            Ok(())
        }
    }
    Box::new(PromptPlugin(sources))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn cx() -> RenderContext {
        RenderContext {
            cwd: std::path::PathBuf::from("/tmp/proj"),
            model: "deepseek-chat".into(),
            tool_names: vec![],
            plan_active: false,
        }
    }

    #[test]
    fn renders_in_priority_order_with_ties_by_registration() {
        let mut r = PromptRegistry::new();
        r.section("late", 10, |_| "LATE".into()).unwrap();
        r.section("early", -5, |_| "EARLY".into()).unwrap();
        r.section("tie-a", 0, |_| "A".into()).unwrap();
        r.section("tie-b", 0, |_| "B".into()).unwrap();
        assert_eq!(r.render(&cx()), "EARLY\n\nA\n\nB\n\nLATE");
    }

    #[test]
    fn section_provenance_preserves_exact_wire_text_and_exclusions() {
        let mut r = PromptRegistry::new();
        r.section("identity", -100, |_| "base界".into()).unwrap();
        r.section("project-instructions", 50, |_| {
            "# identity\nUser guidance".into()
        })
        .unwrap();
        r.section_shared("skills-catalog", 100, |_| " skill ".into())
            .unwrap();
        let sections = r.render_sections_excluding(&cx(), &[]);
        assert_eq!(
            sections
                .iter()
                .map(|(_, s)| s.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
            r.render(&cx())
        );
        assert_eq!(sections[1].0, "project-instructions");
        assert_eq!(
            r.render_sections_excluding(&cx(), &["skills-catalog"])
                .len(),
            2
        );
    }

    #[test]
    fn native_and_imported_instruction_scopes_keep_priority_during_dynamic_replacement() {
        let root = tempfile::tempdir().unwrap();
        let user = root.path().join("user");
        let project = root.path().join("project");
        let next = root.path().join("next");
        for path in [&user, &project, &next] {
            std::fs::create_dir(path).unwrap();
        }
        std::fs::write(user.join("AGENTS.md"), "NATIVE_USER").unwrap();
        std::fs::write(project.join("AGENTS.md"), "NATIVE_PROJECT").unwrap();
        std::fs::write(next.join("AGENTS.md"), "NEXT_PROJECT").unwrap();
        let sources = instructions::InstructionSources {
            user_home: Some(user),
            workspace: Some(project),
        };
        let mut context =
            heycode_core::compose(&[prompt_plugin_with_instructions(sources.clone())]).unwrap();
        let registry = context.get::<PromptRegistry>(SERVICE_PROMPT).unwrap();
        registry
            .section_shared("imported-user-instructions", 47, |_| {
                "IMPORTED_USER".to_owned()
            })
            .unwrap();
        registry
            .section_shared("imported-project-instructions", 49, |_| {
                "IMPORTED_PROJECT".to_owned()
            })
            .unwrap();
        let original = registry.render_sections_excluding(&cx(), &[]);
        let text = original
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let positions = [
            "IMPORTED_USER",
            "NATIVE_USER",
            "IMPORTED_PROJECT",
            "NATIVE_PROJECT",
        ]
        .map(|marker| text.find(marker).unwrap());
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        let same = registry.render_sections_overriding(
            &cx(),
            &[],
            &instructions::render_scoped_instructions(&sources),
        );
        assert_eq!(
            same, original,
            "dynamic and initial wire provenance must be identical"
        );
        let next_sources = instructions::InstructionSources {
            workspace: Some(next),
            ..sources
        };
        let replaced = registry.render_sections_overriding(
            &cx(),
            &[],
            &instructions::render_scoped_instructions(&next_sources),
        );
        assert!(
            replaced
                .iter()
                .any(|(name, text)| *name == "user-instructions"
                    && text.contains("NATIVE_USER")
                    && !text.contains("PROJECT"))
        );
        assert!(
            replaced
                .iter()
                .any(|(name, text)| *name == "project-instructions"
                    && text.contains("NEXT_PROJECT")
                    && !text.contains("NATIVE_USER"))
        );
        assert!(
            !replaced
                .iter()
                .any(|(_, text)| text.contains("NATIVE_PROJECT"))
        );
        context.shutdown();
    }

    #[test]
    fn empty_sections_are_dropped() {
        let mut r = PromptRegistry::new();
        r.section("blank", 0, |_| "   ".into()).unwrap();
        r.section("real", 1, |_| "REAL".into()).unwrap();
        assert_eq!(r.render(&cx()), "REAL");
    }

    #[test]
    fn duplicate_names_fail_loud() {
        let mut r = PromptRegistry::new();
        r.section("dup", 0, |_| "one".into()).unwrap();
        assert!(r.section("dup", 1, |_| "two".into()).is_err());
    }

    #[test]
    fn plugin_provides_service_and_default_sections() {
        let plugins: Vec<Box<dyn Plugin>> = vec![prompt_plugin()];
        let ctx = heycode_core::compose(&plugins).unwrap();
        let reg = ctx.get::<PromptRegistry>(SERVICE_PROMPT).unwrap();
        let text = reg.render(&cx());
        assert!(text.contains("You are heycode"));
        assert!(text.contains("/tmp/proj"));
    }
}
