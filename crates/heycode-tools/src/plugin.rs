//! Plugin wiring and composition entry points for the tools capability.

use std::sync::Arc;

use heycode_core::{Context, CoreResult, Plugin, Waterfall};

use crate::SERVICE_TOOLS;
use crate::builtins::{bash, edit, glob, grep, multi_edit, read, read_many, write};
use crate::config::ToolsConfig;
use crate::exec::{PreToolDecision, SEAM_PRE_TOOL};
use crate::registry::ToolRegistry;

/// Plugin named `"tools"`: builds the built-in registry plus an empty
/// [`SEAM_PRE_TOOL`] waterfall and provides both services.
struct ToolsPlugin {
    cfg: ToolsConfig,
}

impl Plugin for ToolsPlugin {
    fn name(&self) -> &'static str {
        "tools"
    }

    fn descriptor(&self) -> heycode_core::PluginDescriptor {
        heycode_core::PluginDescriptor::built_in(
            "tools",
            env!("CARGO_PKG_VERSION"),
            &[
                heycode_core::PluginContributionKind::Service,
                heycode_core::PluginContributionKind::Tool,
                heycode_core::PluginContributionKind::Waterfall,
            ],
        )
    }

    fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
        let mut names = vec![
            "read",
            "read_many",
            "write",
            "edit",
            "multi_edit",
            "bash",
            "glob",
            "grep",
        ];
        if self.cfg.web_enabled {
            names.push("web_fetch");
        }
        if self.cfg.terminals_enabled {
            names.extend([
                "terminal_open",
                "terminal_write",
                "terminal_read",
                "terminal_resize",
                "terminal_kill",
                "terminal_list",
            ]);
        }
        let mut rows: Vec<_> = names
            .into_iter()
            .map(|name| {
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Tool,
                    name,
                )
            })
            .collect();
        rows.push(heycode_core::PluginContributionSpec::new(
            heycode_core::ContributionKind::InterceptionSeam,
            SEAM_PRE_TOOL.as_str(),
        ));
        if self.cfg.web_enabled {
            rows.extend(["client:web_fetch", "client:web_search"].map(|name| {
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::NativeTool,
                    name,
                )
            }));
        }
        rows
    }

    fn inject(&self) -> &'static [heycode_core::ServiceKey] {
        const CORE: &[heycode_core::ServiceKey] = &[
            heycode_exec::SERVICE_FILESYSTEM,
            heycode_exec::SERVICE_SHELL,
            heycode_native_tools::SERVICE_NATIVE_TOOLS,
        ];
        const WITH_WEB: &[heycode_core::ServiceKey] = &[
            heycode_exec::SERVICE_FILESYSTEM,
            heycode_exec::SERVICE_SHELL,
            heycode_native_tools::SERVICE_NATIVE_TOOLS,
            heycode_web::SERVICE_WEB,
        ];
        const WITH_TERMINAL: &[heycode_core::ServiceKey] = &[
            heycode_exec::SERVICE_FILESYSTEM,
            heycode_exec::SERVICE_SHELL,
            heycode_native_tools::SERVICE_NATIVE_TOOLS,
            heycode_exec::SERVICE_SUBPROCESS,
            heycode_exec::SERVICE_TERMINAL,
        ];
        const WITH_BOTH: &[heycode_core::ServiceKey] = &[
            heycode_exec::SERVICE_FILESYSTEM,
            heycode_exec::SERVICE_SHELL,
            heycode_native_tools::SERVICE_NATIVE_TOOLS,
            heycode_web::SERVICE_WEB,
            heycode_exec::SERVICE_SUBPROCESS,
            heycode_exec::SERVICE_TERMINAL,
        ];
        match (self.cfg.web_enabled, self.cfg.terminals_enabled) {
            (true, true) => WITH_BOTH,
            (true, false) => WITH_WEB,
            (false, true) => WITH_TERMINAL,
            (false, false) => CORE,
        }
    }

    fn provides(&self) -> &'static [heycode_core::ServiceKey] {
        &[SERVICE_TOOLS, SEAM_PRE_TOOL]
    }

    fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
        let shell = ctx
            .get::<heycode_exec::ShellService>(heycode_exec::SERVICE_SHELL)
            .ok_or_else(|| heycode_core::CoreError::other("shell service type mismatch"))?;
        let filesystem = ctx
            .get::<heycode_exec::FileSystemService>(heycode_exec::SERVICE_FILESYSTEM)
            .ok_or_else(|| heycode_core::CoreError::other("filesystem service type mismatch"))?;
        let native_tools = ctx
            .get::<heycode_native_tools::NativeToolRegistry>(
                heycode_native_tools::SERVICE_NATIVE_TOOLS,
            )
            .ok_or_else(|| heycode_core::CoreError::other("native-tools service type mismatch"))?;
        let (mut registry, waterfall) =
            builtin_tools(&self.cfg, (*filesystem).clone(), (*shell).clone());
        if self.cfg.web_enabled {
            let web = ctx
                .get::<heycode_web::WebRegistry>(heycode_web::SERVICE_WEB)
                .ok_or_else(|| heycode_core::CoreError::other("web service type mismatch"))?;
            registry
                .register(Arc::new(crate::builtins::web::WebFetch::new(web)))
                .map_err(|e| heycode_core::CoreError::other(e.to_string()))?;
            {
                let (logical, implementation) = ("web_fetch", "client:web_fetch");
                native_tools
                    .register(
                        ctx,
                        heycode_native_tools::NativeToolImplementation::new(
                            logical,
                            implementation,
                            heycode_core::NativeToolImplementationKind::Client,
                            None,
                            100,
                        )
                        .map_err(|error| heycode_core::CoreError::other(error.to_string()))?,
                    )
                    .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            }
        }
        if self.cfg.terminals_enabled {
            let terminals = ctx
                .get::<heycode_exec::TerminalService>(heycode_exec::SERVICE_TERMINAL)
                .ok_or_else(|| heycode_core::CoreError::other("terminal service type mismatch"))?;
            let subprocess = ctx
                .get::<heycode_exec::SubprocessService>(heycode_exec::SERVICE_SUBPROCESS)
                .ok_or_else(|| {
                    heycode_core::CoreError::other("subprocess service type mismatch")
                })?;
            // One owner per composed world. The registry's owner scoping is
            // what isolates terminals; per-agent scoping additionally needs a
            // per-agent owner, which O03 owns.
            let owner = heycode_exec::TerminalOwner::new("world")
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            for tool in crate::builtins::terminal::terminal_tools(
                (*terminals).clone(),
                (*subprocess).clone(),
                owner,
                self.cfg.cwd.clone(),
            ) {
                let tool = tool.rebind_workspace(&filesystem, &shell).unwrap_or(tool);
                registry
                    .register(tool)
                    .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            }
        }
        ctx.provide(SERVICE_TOOLS, self.name(), registry)?;
        ctx.provide(SEAM_PRE_TOOL, self.name(), waterfall)?;
        Ok(())
    }
}

/// Build the `"tools"` plugin with `cfg` as the built-ins' configuration.
#[must_use]
pub fn tools_plugin(cfg: ToolsConfig) -> Box<dyn Plugin> {
    Box::new(ToolsPlugin { cfg })
}

/// Build the built-in tool set without the plugin system.
///
/// The returned [`ToolRegistry`] and [`Waterfall`] are exactly what
/// [`tools_plugin`] would provide; tests and alternative compositions use this
/// to wire tools directly.
#[must_use]
pub fn builtin_tools(
    cfg: &ToolsConfig,
    filesystem: heycode_exec::FileSystemService,
    shell: heycode_exec::ShellService,
) -> (ToolRegistry, Waterfall<PreToolDecision>) {
    let cfg = Arc::new(cfg.clone());
    let mut registry = ToolRegistry::with_filesystem_observations(filesystem.clone());
    let builtins: [Arc<dyn crate::tool::Tool>; 8] = [
        read::tool(cfg.clone(), filesystem.clone()),
        read_many::tool(cfg.clone(), filesystem.clone()),
        write::tool(filesystem.clone()),
        edit::tool(filesystem.clone()),
        multi_edit::tool(filesystem.clone()),
        bash::tool(shell),
        glob::tool(filesystem.clone()),
        grep::tool(filesystem),
    ];
    for tool in builtins {
        // Names above are statically unique and declare object schemas, so
        // register cannot fail here; a failure would mean two built-ins were
        // added under one name, which review catches at compile time via the
        // array length.
        let _ = registry.register(tool);
    }
    (registry, Waterfall::new())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use heycode_core::ToolSpec as ToolSpecT;
    use heycode_core::compose;

    fn shell_config() -> heycode_exec::LocalShellConfig {
        heycode_exec::LocalShellConfig::platform(
            std::env::current_dir().unwrap(),
            std::time::Duration::from_secs(30),
        )
        .unwrap()
    }

    fn shell() -> heycode_exec::ShellService {
        heycode_exec::ShellService::local(shell_config())
    }

    fn filesystem() -> heycode_exec::FileSystemService {
        let root = std::env::current_dir().unwrap();
        let policy = heycode_exec::FileSystemPolicy::new([heycode_exec::FileSystemRoot::new(
            root,
            heycode_exec::FileSystemRootAccess::ReadWrite,
        )
        .unwrap()])
        .unwrap();
        heycode_exec::FileSystemService::local(policy).unwrap()
    }

    #[test]
    fn builtin_tools_registers_core_eight_without_web() {
        let cfg = ToolsConfig {
            web_enabled: false,
            ..ToolsConfig::default()
        };
        let (registry, waterfall) = builtin_tools(&cfg, filesystem(), shell());
        assert_eq!(
            registry.names(),
            [
                "read",
                "read_many",
                "write",
                "edit",
                "multi_edit",
                "bash",
                "glob",
                "grep"
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
        );
        for spec in registry.specs() {
            assert!(!spec.description.is_empty());
            assert!(
                spec.parameters.is_object(),
                "{} must declare an object schema",
                spec.name
            );
        }
        assert!(waterfall.is_empty());
    }

    /// The scheduler overlaps only calls whose tool declared itself
    /// observed-only, so each shipped tool must state the truth about itself:
    /// the claim lives with the tool, not in a list beside the scheduler.
    #[test]
    fn every_builtin_declares_whether_it_only_observes() {
        let (registry, _waterfall) = builtin_tools(&ToolsConfig::default(), filesystem(), shell());
        let effects: Vec<(String, crate::ToolEffect)> = registry
            .names()
            .into_iter()
            .map(|name| {
                let effect = registry.get(&name).expect("registered").effect();
                (name, effect)
            })
            .collect();
        for (name, effect) in &effects {
            let expected = match name.as_str() {
                "read" | "read_many" | "glob" | "grep" | "web_fetch" | "web_search" => {
                    crate::ToolEffect::ReadOnly
                }
                _ => crate::ToolEffect::Mutates,
            };
            assert_eq!(effect, &expected, "{name} declared the wrong effect");
        }
        assert!(
            effects
                .iter()
                .any(|(_, effect)| effect == &crate::ToolEffect::ReadOnly),
            "the fixture would pass vacuously if nothing were read-only"
        );
    }

    #[test]
    fn plugin_provides_registry_and_pre_tool_waterfall_services() {
        let cfg = ToolsConfig {
            web_enabled: false,
            ..ToolsConfig::default()
        };
        let plugins: Vec<Box<dyn Plugin>> = vec![
            heycode_exec::local_execution_plugin(shell_config()),
            heycode_exec::local_filesystem_plugin(),
            heycode_native_tools::native_tools_plugin(),
            tools_plugin(cfg),
        ];
        let ctx = compose(&plugins).unwrap();
        assert_eq!(ctx.owner_of(SERVICE_TOOLS), Some("tools"));
        let registry: Arc<ToolRegistry> = ctx
            .get::<ToolRegistry>(SERVICE_TOOLS)
            .expect("tools service");
        // Web OFF: exactly the eight core filesystem/shell tools.
        assert_eq!(registry.names().len(), 8);
        let seam: Arc<Waterfall<PreToolDecision>> = ctx
            .get::<Waterfall<PreToolDecision>>(SEAM_PRE_TOOL)
            .expect("seam service");
        assert!(seam.is_empty());
        // Specs are model-facing vocabulary from core, untouched by this crate.
        let names: Vec<String> = registry
            .specs()
            .into_iter()
            .map(|s: ToolSpecT| s.name)
            .collect();
        assert!(!names.contains(&"todo_write".to_owned()));
    }

    #[test]
    fn production_search_has_no_local_fallback_and_preserves_native_model_scope() {
        let plugins: Vec<Box<dyn Plugin>> = vec![
            heycode_exec::local_execution_plugin(shell_config()),
            heycode_exec::local_filesystem_plugin(),
            heycode_native_tools::native_tools_plugin(),
            heycode_web::web_registry_plugin(),
            tools_plugin(ToolsConfig::default()),
        ];
        let ctx = compose(&plugins).unwrap();
        let tools = ctx.get::<ToolRegistry>(SERVICE_TOOLS).unwrap();
        assert!(tools.get("web_fetch").is_some());
        assert!(tools.get("web_search").is_none());
        let native = ctx
            .get::<heycode_native_tools::NativeToolRegistry>(
                heycode_native_tools::SERVICE_NATIVE_TOOLS,
            )
            .unwrap();
        native
            .register(
                &ctx,
                heycode_native_tools::NativeToolImplementation::new(
                    "web_search",
                    "provider:web_search",
                    heycode_core::NativeToolImplementationKind::Provider,
                    Some("provider".into()),
                    100,
                )
                .unwrap()
                .with_models(vec!["supported".into()])
                .unwrap(),
            )
            .unwrap();
        for (provider, model, expected) in [
            ("provider", "supported", true),
            ("provider", "unsupported", false),
            ("other", "supported", false),
        ] {
            let routes = native.resolve_for_model(provider, model).unwrap();
            assert_eq!(
                routes.iter().any(|route| route.logical() == "web_search"),
                expected
            );
            assert!(routes.iter().any(|route| route.logical() == "web_fetch"));
        }
    }

    #[test]
    fn duplicate_service_claims_fail_loud() {
        struct Squatter;
        impl Plugin for Squatter {
            fn name(&self) -> &'static str {
                "squatter"
            }
            fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
                ctx.provide(SERVICE_TOOLS, "squatter", ToolRegistry::new())
            }
        }
        let plugins: Vec<Box<dyn Plugin>> = vec![
            heycode_exec::local_execution_plugin(shell_config()),
            heycode_exec::local_filesystem_plugin(),
            heycode_native_tools::native_tools_plugin(),
            heycode_web::web_registry_plugin(),
            Box::new(Squatter),
            tools_plugin(ToolsConfig::default()),
        ];
        assert!(compose(&plugins).is_err());
    }
}
