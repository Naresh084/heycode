//! MCP10 command-line surface for MCP server management.
//!
//! Parsing and rendering are pure functions over `McpOperation`, so the CLI can
//! be tested without spawning a binary and — more importantly — so a TUI panel
//! (U12) drives the same `McpManagement` through the same closed operation set.
//! Parity is structural: `McpOperation` is exhaustive, and every surface that
//! matches on it stops compiling when an operation is added.

use heycode_mcp::McpTransportKind;
use heycode_mcp::management::{McpHealth, McpManagement, McpManagementError, McpOperation};

/// One fully parsed management command.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpCommand {
    /// `heycode mcp add <name> [-e K=V]… --command <cmd> [-- <args>…] | --url <url>`
    Add {
        /// Server name.
        name: String,
        /// Which transport was chosen.
        transport: McpTransportKind,
        /// Command or URL.
        target: String,
        /// Arguments after `--` for a stdio command.
        args: Vec<String>,
        /// `-e KEY=VALUE` pairs; `-e KEY` stores an empty value meaning
        /// "inherit the host's variable at launch".
        env: std::collections::BTreeMap<String, String>,
    },
    /// `heycode mcp list`
    List,
    /// `heycode mcp auth <name>`
    Auth {
        /// Server name.
        name: String,
    },
    /// `heycode mcp test <name>`
    Test {
        /// Server name.
        name: String,
    },
    /// `heycode mcp edit <name> --command <cmd> | --url <url>`
    Edit {
        /// Server name.
        name: String,
        /// Which transport was chosen.
        transport: McpTransportKind,
        /// Command or URL.
        target: String,
    },
    /// `heycode mcp enable <name>` / `--off`
    Enable {
        /// Server name.
        name: String,
        /// Whether to turn the server on.
        on: bool,
    },
    /// `heycode mcp remove <name>`
    Remove {
        /// Server name.
        name: String,
    },
}

impl McpCommand {
    /// Which operation this command invokes.
    ///
    /// The exhaustive match is the parity mechanism: an eighth operation stops
    /// this from compiling until the CLI handles it.
    #[must_use]
    pub const fn operation(&self) -> McpOperation {
        match self {
            Self::Add { .. } => McpOperation::Add,
            Self::List => McpOperation::List,
            Self::Auth { .. } => McpOperation::Auth,
            Self::Test { .. } => McpOperation::Test,
            Self::Edit { .. } => McpOperation::Edit,
            Self::Enable { .. } => McpOperation::Enable,
            Self::Remove { .. } => McpOperation::Remove,
        }
    }
}

/// Usage text for `heycode mcp`, generated from the operation set so it can never
/// omit an operation the CLI accepts.
#[must_use]
pub fn usage() -> String {
    let mut text = String::from("usage: heycode mcp <operation>\n\noperations:\n");
    for operation in McpOperation::ALL {
        let detail = match operation {
            McpOperation::Add => {
                "<name> [-e KEY[=VALUE]]... --command <cmd> [-- <args>...] | --url <url>"
            }
            McpOperation::List => "",
            McpOperation::Auth | McpOperation::Test | McpOperation::Remove => "<name>",
            McpOperation::Edit => "<name> --command <cmd> | --url <url>",
            McpOperation::Enable => "<name> [--off]",
        };
        text.push_str(&format!("  {:<7} {detail}\n", operation.as_str()));
    }
    text
}

/// Parse `heycode mcp ...` arguments.
///
/// # Errors
/// A message suitable for stderr. Unknown operations list what is available
/// rather than only saying no.
pub fn parse(args: &[String]) -> Result<McpCommand, String> {
    let Some(word) = args.first() else {
        return Err(usage());
    };
    let operation = McpOperation::parse(word)
        .ok_or_else(|| format!("unknown mcp operation `{word}`\n\n{}", usage()))?;

    let name = |index: usize| -> Result<String, String> {
        args.get(index)
            .filter(|value| !value.starts_with('-'))
            .cloned()
            .ok_or_else(|| format!("`heycode mcp {operation}` needs a server name"))
    };

    match operation {
        McpOperation::List => Ok(McpCommand::List),
        McpOperation::Auth => Ok(McpCommand::Auth { name: name(1)? }),
        McpOperation::Test => Ok(McpCommand::Test { name: name(1)? }),
        McpOperation::Remove => Ok(McpCommand::Remove { name: name(1)? }),
        McpOperation::Enable => {
            let name = name(1)?;
            let on = !args[2..].iter().any(|flag| flag == "--off");
            Ok(McpCommand::Enable { name, on })
        }
        McpOperation::Add | McpOperation::Edit => {
            let name = name(1)?;
            let launch = parse_transport(&args[2..], operation)?;
            if operation == McpOperation::Add {
                Ok(McpCommand::Add {
                    name,
                    transport: launch.transport,
                    target: launch.target,
                    args: launch.args,
                    env: launch.env,
                })
            } else {
                let (transport, target) = (launch.transport, launch.target);
                Ok(McpCommand::Edit {
                    name,
                    transport,
                    target,
                })
            }
        }
    }
}

/// One parsed transport selection plus its stdio launch details.
struct ParsedLaunch {
    transport: McpTransportKind,
    target: String,
    args: Vec<String>,
    env: std::collections::BTreeMap<String, String>,
}

fn parse_transport(rest: &[String], operation: McpOperation) -> Result<ParsedLaunch, String> {
    let mut found: Option<(McpTransportKind, String)> = None;
    let mut env = std::collections::BTreeMap::new();
    let mut args = Vec::new();
    let mut index = 0;
    while index < rest.len() {
        match rest[index].as_str() {
            "--command" | "--url" => {
                let kind = if rest[index] == "--command" {
                    McpTransportKind::Stdio
                } else {
                    McpTransportKind::StreamableHttp
                };
                index += 1;
                let value = rest
                    .get(index)
                    .ok_or_else(|| format!("`{}` needs a value", rest[index - 1]))?;
                // Both given is a real ambiguity, not something to resolve by
                // precedence: a server has exactly one transport, and guessing
                // which one the user meant is how a server silently talks to
                // the wrong endpoint.
                if found.is_some() {
                    return Err("give exactly one of `--command` or `--url`".to_owned());
                }
                found = Some((kind, value.clone()));
            }
            "-e" | "--env" => {
                index += 1;
                let pair = rest
                    .get(index)
                    .ok_or_else(|| format!("`{}` needs KEY or KEY=VALUE", rest[index - 1]))?;
                let (key, value) = pair.split_once('=').unwrap_or((pair.as_str(), ""));
                env.insert(key.to_owned(), value.to_owned());
            }
            // Everything after `--` is the server's own argument vector.
            "--" => {
                args.extend(rest[index + 1..].iter().cloned());
                break;
            }
            other => {
                // An unrecognised token is refused, never dropped: a flag the
                // user meant for the server belongs after `--`.
                return Err(format!(
                    "unknown option `{other}` — server arguments go after `--`, environment uses `-e KEY=VALUE`"
                ));
            }
        }
        index += 1;
    }
    let (transport, target) =
        found.ok_or_else(|| format!("`heycode mcp {operation}` needs `--command` or `--url`"))?;
    Ok(ParsedLaunch {
        transport,
        target,
        args,
        env,
    })
}

/// Run one parsed command against the shared operations layer.
///
/// # Errors
/// The management error, rendered for a terminal.
pub fn run(management: &McpManagement, command: &McpCommand) -> Result<String, String> {
    let rendered = match command {
        McpCommand::Add {
            name,
            transport,
            target,
            args,
            env,
        } => {
            let server = heycode_mcp::management::StoredServer::new(name, *transport, target)
                .and_then(|server| server.with_stdio_launch(args.clone(), env.clone()))
                .map_err(render_error)?;
            management.add(server).map_err(render_error)?;
            format!("added `{name}`")
        }
        McpCommand::List => {
            // `heycode mcp list` is a health query, so it probes — concurrently,
            // so N servers cost about one probe.
            let rows = management.list_probed().map_err(render_error)?;
            if rows.is_empty() {
                "no MCP servers configured".to_owned()
            } else {
                rows.iter()
                    .map(|row| {
                        format!(
                            "{:<20} {:<16} {:<9} {} {}",
                            row.server.name,
                            transport_word(row.server.transport),
                            if row.server.enabled {
                                "enabled"
                            } else {
                                "disabled"
                            },
                            health_word(row.health),
                            std::iter::once(row.server.target.as_str())
                                .chain(row.server.args.iter().map(String::as_str))
                                .collect::<Vec<_>>()
                                .join(" ")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        McpCommand::Auth { name } => {
            let health = management.auth(name).map_err(render_error)?;
            match health {
                McpHealth::AuthorizationRequired => {
                    format!("`{name}` needs authorization; run `heycode mcp auth {name}` to begin")
                }
                McpHealth::Reachable => format!("`{name}` is authorized"),
                McpHealth::Unreachable => format!("`{name}` could not be reached"),
                // `McpHealth` is non-exhaustive on purpose — new states are
                // data about the world, not a breaking change. An unrecognized
                // one falls to "unknown", never to "authorized": the safe
                // direction is the one that does not claim access we cannot see.
                McpHealth::Unknown | _ => format!("`{name}` authorization state is unknown"),
            }
        }
        McpCommand::Test { name } => {
            let health = management.test(name).map_err(render_error)?;
            format!("{name}: {}", health_word(health))
        }
        McpCommand::Edit {
            name,
            transport,
            target,
        } => {
            management
                .edit(name, *transport, target)
                .map_err(render_error)?;
            format!("updated `{name}`")
        }
        McpCommand::Enable { name, on } => {
            management.enable(name, *on).map_err(render_error)?;
            format!("{} `{name}`", if *on { "enabled" } else { "disabled" })
        }
        McpCommand::Remove { name } => {
            management.remove(name).map_err(render_error)?;
            format!("removed `{name}`")
        }
    };
    Ok(rendered)
}

fn render_error(error: McpManagementError) -> String {
    error.to_string()
}

const fn transport_word(kind: McpTransportKind) -> &'static str {
    match kind {
        McpTransportKind::Stdio => "stdio",
        McpTransportKind::StreamableHttp => "streamable-http",
    }
}

/// Health rendered so an operator's next step is obvious from the word.
const fn health_word(health: McpHealth) -> &'static str {
    match health {
        McpHealth::Reachable => "reachable",
        McpHealth::Unreachable => "unreachable",
        McpHealth::AuthorizationRequired => "needs-auth",
        // A health state this build does not recognize reads as unknown, never
        // as reachable. Unlike `McpOperation`, this enum is non-exhaustive
        // because a new state must not break every surface at once.
        _ => "unknown",
    }
}

/// Compose the smallest world MCP management needs.
///
/// Deliberately **not** the full product world. Managing server definitions
/// edits user settings; it runs no inference, needs no catalog and holds no
/// provider credential. Composing everything would make `heycode mcp list` fail
/// for a user who has not configured an API key yet — which is precisely the
/// user most likely to be setting servers up.
///
/// # Errors
/// Settings or plugin activation failure.
pub fn compose_management_world(
    settings_user_path: std::path::PathBuf,
) -> anyhow::Result<(heycode_core::Context, std::sync::Arc<McpManagement>)> {
    let plugins: Vec<Box<dyn heycode_core::Plugin>> = vec![
        heycode_settings_file::file_settings_plugin(
            heycode_settings_file::FileSettingsConfig::user(settings_user_path).without_watch(),
        ),
        // `heycode mcp test|auth|list` run outside a session, so the only
        // evidence available is a live probe: spawn/initialize the definition,
        // bounded. Plain listing still never connects.
        heycode_mcp::management::mcp_management_plugin_with_probe(Some(std::sync::Arc::new(
            heycode_mcp::management::LiveMcpProbe::default(),
        ))),
    ];
    let context = heycode_core::compose(&plugins)?;
    let management = context
        .get::<McpManagement>(heycode_mcp::SERVICE_MCP_MANAGEMENT)
        .ok_or_else(|| anyhow::anyhow!("MCP management service is not mounted"))?;
    Ok((context, management))
}
