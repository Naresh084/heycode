//! The `heycode` executable: argument parsing, onboarding wizard, run modes.
//!
//! Printing and stdin interaction live HERE and nowhere else (clippy denies
//! stdout in libraries).

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::IsTerminal;
use std::sync::Arc;

use heycode_cli::{
    BUILTIN_PLUGIN_ORDER, WorldOptions, compose_world, diagnose_world, find_latest_session_in,
    provider_key_present, provider_uses_credential,
};
use heycode_config::{
    CONFIG_SCHEMA_VERSION, Config, ConfigMigrationChange, ConfigMigrationDisposition,
    ConfigMigrationNotice, LoadedConfig,
};
use heycode_llm::testing::FakeProvider;

mod acp;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod session_background;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(args) {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            if std::io::stderr().is_terminal() {
                // A terminal write failure must not hang again while reporting it.
                let _ = heycode_tui::terminal::restore_terminal_output(
                    format!("heycode: {err:#}\n").as_bytes(),
                );
            } else {
                eprintln!("heycode: {err:#}");
            }
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "linux")]
fn apply_landlock_and_exec(rest: &[String]) -> ! {
    use std::io::Write as _;
    let Some(rules_json) = rest.first() else {
        eprintln!("heycode: __landlock requires a rules argument");
        std::process::exit(2);
    };
    let sep = rest.iter().position(|a| a == "--");
    let Some(sep_idx) = sep else {
        eprintln!("heycode: __landlock requires `--` separator");
        std::process::exit(2);
    };
    let argv: Vec<String> = rest[sep_idx + 1..].to_vec();
    if argv.is_empty() {
        eprintln!("heycode: __landlock requires a command after `--`");
        std::process::exit(2);
    }
    let rules: heycode_sandbox::LandlockRules = match serde_json::from_str(rules_json) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("heycode: bad landlock rules: {e}");
            std::process::exit(2);
        }
    };
    match heycode_sandbox::apply_landlock(&rules, &argv) {
        Ok(()) => unreachable!("exec never returns on success"),
        Err(err) => {
            let _ = std::io::stderr().write_fmt(format_args!("heycode: landlock: {err}\n"));
            std::process::exit(126);
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn apply_landlock_and_exec(_rest: &[String]) -> ! {
    eprintln!("heycode: __landlock is Linux-only");
    std::process::exit(2);
}

fn usage() -> String {
    [
        "usage:",
        "  heycode [options]                  interactive TUI",
        "  heycode run [options] \"prompt\"    one-shot headless turn",
        "  heycode acp [options]              Agent Client Protocol server on stdio",
        "  heycode app-server --stdio-v1 --workspace <absolute-path> [--resume <session-id>]",
        "  heycode setup                      interactive provider/key/model setup",
        "  heycode sessions [list|attach <id>|stop <id>]  whole-session local hosts",
        "  heycode doctor [--json]            run redacted plugin health checks",
        "  heycode doctor --composition [--json]  inspect graph and isolated activation",
        "  heycode mcp <operation>            manage MCP servers (`heycode mcp` lists operations)",
        "  heycode plugin <operation>         manage plugins (`heycode plugin` lists operations)",
        "  heycode config show                print every effective value and where it came from",
        "  heycode release <operation>        verify/install/update/rollback signed releases",
        "",
        "  heycode update [--check]          check/install the latest stable release",
        "  heycode --version                 show installed version",
        "",
        "options:",
        "  -c, --continue         reopen the last active conversation in this folder",
        "  -r, --resume [id/path]  search this folder’s conversations, or resume one",
        "  --config <path>        config file (default: trusted ./heycode.toml)",
        "  --profile <name>       select $HEYCODE_HOME/profiles/<name>.toml",
        "  --set <k=v>            patch a config value (repeatable)",
        "  --approval <mode>      full_access | accepted_edits | default",
        "  --sandbox <mode>       off | readonly | workspace",
        "  --provider <name>      provider id (discover with `heycode setup` or `/provider`)",
        "  --model <id>           model id",
        "  --protocol <dialect>   auto | openai_chat | openai_responses | anthropic_messages",
        "  --max-output-tokens N  explicit positive provider output default",
        "  --output-format <f>    run: text (default) | json (one envelope) | stream-json (session events)",
        "  --image <path>         attach image to a one-shot prompt (repeatable)",
        "  --document <path>      attach PDF/HTML to a one-shot prompt (repeatable)",
        "  --screen-reader        flat, colorless TUI without alternate-screen control",
        "  --no-background        direct terminal process; whole-session detach unavailable",
        "  --trust-workspace      trust this workspace for this process only",
        "  --restricted-workspace open without project executable/settings authority",
        "  --fake                 offline smoke provider (no network)",
    ]
    .join("\n")
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CliAttachment {
    Image(std::path::PathBuf),
    Document(std::path::PathBuf),
}

struct Cli {
    mode_run: bool,
    mode_setup: bool,
    mode_acp: bool,
    mode_app_server: bool,
    app_server_args: Vec<String>,
    mode_doctor: bool,
    mode_mcp: bool,
    mcp_args: Vec<String>,
    mode_plugin: bool,
    plugin_args: Vec<String>,
    /// `heycode config <operation>`; `None` operation prints the operation list.
    mode_config: bool,
    config_operation: Option<String>,
    mode_release: bool,
    release_args: Vec<String>,
    doctor_composition: bool,
    json: bool,
    prompt: Option<String>,
    config_path: Option<std::path::PathBuf>,
    profile: Option<String>,
    sets: Vec<String>,
    provider: Option<String>,
    model: Option<String>,
    protocol: Option<String>,
    max_output_tokens: Option<String>,
    /// `heycode run` output contract; `None` is text.
    output_format: Option<heycode_cli::headless::OutputFormat>,
    attachments: Vec<CliAttachment>,
    workspace_trust: Option<heycode_trust::ExplicitWorkspaceTrust>,
    screen_reader: bool,
    no_background: bool,
    fake: bool,
    resume: Option<std::path::PathBuf>,
    resume_latest: bool,
    resume_picker: bool,
}

enum RunCompletion {
    Exit(i32),
    RecomposeCurrent {
        session_id: heycode_core::SessionId,
        action: heycode_tui::recomposition::RecompositionAction,
    },
    RecomposeConnection,
    RecomposeConnectionSelection,
    RecomposeProfile {
        /// `None` returns to the built-in composition.
        name: Option<String>,
        /// The session on screen: a profile switch changes the world, not the
        /// conversation, so the new world resumes it (and a failed switch
        /// comes back to it).
        current_session_id: heycode_core::SessionId,
    },
    RecomposeSession {
        session_id: heycode_core::SessionId,
        /// The session that was on screen, so a failed switch can come back to
        /// it rather than to whatever the original argv would create.
        current_session_id: heycode_core::SessionId,
    },
    RecomposeWorkspaceTrust {
        decision: heycode_trust::WorkspaceTrustDecision,
        persistence: heycode_trust::TrustPersistence,
    },
}

fn parse_args(args: &[String]) -> Result<Cli, String> {
    let mut cli = Cli {
        mode_run: false,
        mode_setup: false,
        mode_acp: false,
        mode_app_server: false,
        app_server_args: Vec::new(),
        mode_doctor: false,
        mode_mcp: false,
        mcp_args: Vec::new(),
        mode_plugin: false,
        plugin_args: Vec::new(),
        mode_config: false,
        config_operation: None,
        mode_release: false,
        release_args: Vec::new(),
        doctor_composition: false,
        json: false,
        prompt: None,
        config_path: None,
        profile: None,
        sets: vec![],
        provider: None,
        model: None,
        protocol: None,
        max_output_tokens: None,
        output_format: None,
        attachments: Vec::new(),
        workspace_trust: None,
        screen_reader: false,
        no_background: false,
        fake: false,
        resume: None,
        resume_latest: false,
        resume_picker: false,
    };
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-h" | "--help" => return Err(usage()),
            "remote" if !cli.mode_run && cli.prompt.is_none() => {
                return Err("the HTTP remote feature has been removed; use the terminal TUI, `heycode run`, or `heycode app-server --stdio-v1`".to_owned());
            }
            "run" if !cli.mode_run && !cli.mode_setup && !cli.mode_acp && !cli.mode_doctor => {
                cli.mode_run = true;
            }
            "setup" if !cli.mode_setup && !cli.mode_run && !cli.mode_acp && !cli.mode_doctor => {
                cli.mode_setup = true;
            }
            "acp" if !cli.mode_acp && !cli.mode_run && !cli.mode_setup && !cli.mode_doctor => {
                cli.mode_acp = true;
            }
            "app-server" => {
                if cli.mode_app_server
                    || cli.mode_release
                    || cli.mode_plugin
                    || cli.mode_mcp
                    || cli.mode_doctor
                    || cli.mode_run
                    || cli.mode_setup
                    || cli.mode_acp
                    || cli.prompt.is_some()
                    || cli.resume.is_some()
                    || cli.resume_latest
                    || cli.resume_picker
                    || !cli.attachments.is_empty()
                {
                    return Err(format!(
                        "app-server mode cannot be combined with another run mode\n{}",
                        usage()
                    ));
                }
                if cli.screen_reader {
                    return Err(
                        "--screen-reader is available only in the interactive TUI".to_owned()
                    );
                }
                cli.mode_app_server = true;
                cli.app_server_args = args[i + 1..].to_vec();
                return Ok(cli);
            }
            "doctor" if !cli.mode_doctor && !cli.mode_run && !cli.mode_setup && !cli.mode_acp => {
                cli.mode_doctor = true;
            }
            // Flags still apply (`heycode --model x config show` reports `x` as
            // the command-line value), so parsing continues after the word.
            "config"
                if !cli.mode_config
                    && !cli.mode_doctor
                    && !cli.mode_run
                    && !cli.mode_setup
                    && !cli.mode_acp
                    && cli.prompt.is_none() =>
            {
                cli.mode_config = true;
            }
            operation
                if cli.mode_config
                    && cli.config_operation.is_none()
                    && !operation.starts_with('-') =>
            {
                cli.config_operation = Some(operation.to_owned());
            }
            "plugin"
                if !cli.mode_plugin
                    && !cli.mode_release
                    && !cli.mode_mcp
                    && !cli.mode_doctor
                    && !cli.mode_run
                    && !cli.mode_setup
                    && !cli.mode_acp =>
            {
                if cli.screen_reader {
                    return Err(
                        "--screen-reader is available only in the interactive TUI".to_owned()
                    );
                }
                // Everything after `plugin` belongs to the subcommand, so a
                // version string or an id with a leading dash is never eaten by
                // the outer parser.
                cli.mode_plugin = true;
                cli.plugin_args = args[i + 1..].to_vec();
                return Ok(cli);
            }
            "release"
                if !cli.mode_release
                    && !cli.mode_plugin
                    && !cli.mode_mcp
                    && !cli.mode_doctor
                    && !cli.mode_run
                    && !cli.mode_setup
                    && !cli.mode_acp =>
            {
                if cli.screen_reader {
                    return Err(
                        "--screen-reader is available only in the interactive TUI".to_owned()
                    );
                }
                cli.mode_release = true;
                cli.release_args = args[i + 1..].to_vec();
                return Ok(cli);
            }
            "mcp"
                if !cli.mode_mcp
                    && !cli.mode_release
                    && !cli.mode_plugin
                    && !cli.mode_doctor
                    && !cli.mode_run
                    && !cli.mode_setup
                    && !cli.mode_acp =>
            {
                if cli.screen_reader {
                    return Err(
                        "--screen-reader is available only in the interactive TUI".to_owned()
                    );
                }
                // Everything after `mcp` is the subcommand's, so a server named
                // `--json` or a URL with a leading dash cannot be eaten by the
                // outer parser.
                cli.mode_mcp = true;
                cli.mcp_args = args[i + 1..].to_vec();
                return Ok(cli);
            }
            "--composition" => cli.doctor_composition = true,
            "--json" => cli.json = true,
            "-c" | "--continue" => {
                if cli.resume.is_some() || cli.resume_latest || cli.resume_picker {
                    return Err("choose either --continue or --resume".to_owned());
                }
                // Preserve the historical short -c <existing path> spelling.
                // The long --continue always means the last active conversation.
                match args.get(i + 1) {
                    Some(candidate)
                        if a == "-c"
                            && !candidate.starts_with('-')
                            && std::path::Path::new(candidate).exists() =>
                    {
                        cli.resume = Some(std::path::PathBuf::from(candidate));
                        i += 1;
                    }
                    _ => cli.resume_latest = true,
                }
            }
            "-r" | "--resume" => {
                if cli.resume.is_some() || cli.resume_latest || cli.resume_picker {
                    return Err("choose either --continue or --resume".to_owned());
                }
                if let Some(target) = args.get(i + 1).filter(|value| !value.starts_with('-')) {
                    cli.resume = Some(target.into());
                    i += 1;
                } else {
                    cli.resume_picker = true;
                }
            }
            "--config" => {
                i += 1;
                cli.config_path = Some(args.get(i).ok_or("--config needs a path")?.into());
            }
            "--profile" => {
                i += 1;
                let profile = args.get(i).ok_or("--profile needs a name")?.clone();
                if cli.profile.replace(profile).is_some() {
                    return Err("--profile may be supplied only once".to_owned());
                }
            }
            "--set" => {
                i += 1;
                cli.sets.push(args.get(i).ok_or("--set needs k=v")?.clone());
            }
            // First-class spellings of the two safety settings, so an
            // automation does not need to know the config key paths.
            "--approval" => {
                i += 1;
                let mode = args
                    .get(i)
                    .ok_or("--approval needs full_access|accepted_edits|default")?;
                cli.sets.push(format!("approval.mode={mode}"));
            }
            "--sandbox" => {
                i += 1;
                let mode = args
                    .get(i)
                    .ok_or("--sandbox needs off|readonly|workspace")?;
                cli.sets.push(format!("sandbox.mode={mode}"));
            }
            "--provider" => {
                i += 1;
                cli.provider = Some(args.get(i).ok_or("--provider needs a name")?.clone());
            }
            "--model" => {
                i += 1;
                cli.model = Some(args.get(i).ok_or("--model needs an id")?.clone());
            }
            "--protocol" => {
                i += 1;
                cli.protocol = Some(args.get(i).ok_or("--protocol needs a dialect")?.clone());
            }
            "--output-format" => {
                i += 1;
                cli.output_format = Some(
                    args.get(i)
                        .ok_or("--output-format needs text, json or stream-json")?
                        .parse()?,
                );
            }
            "--max-output-tokens" => {
                i += 1;
                cli.max_output_tokens = Some(
                    args.get(i)
                        .ok_or("--max-output-tokens needs a positive integer")?
                        .clone(),
                );
            }
            "--image" => {
                i += 1;
                cli.attachments.push(CliAttachment::Image(
                    args.get(i).ok_or("--image needs a path")?.into(),
                ));
            }
            "--document" => {
                i += 1;
                cli.attachments.push(CliAttachment::Document(
                    args.get(i).ok_or("--document needs a path")?.into(),
                ));
            }
            "--trust-workspace" => {
                if cli
                    .workspace_trust
                    .replace(heycode_trust::ExplicitWorkspaceTrust::TrustOnce)
                    .is_some()
                {
                    return Err("workspace trust mode may be supplied only once".to_owned());
                }
            }
            "--restricted-workspace" => {
                if cli
                    .workspace_trust
                    .replace(heycode_trust::ExplicitWorkspaceTrust::RestrictedOnce)
                    .is_some()
                {
                    return Err("workspace trust mode may be supplied only once".to_owned());
                }
            }
            "--screen-reader" => cli.screen_reader = true,
            "--no-background" => cli.no_background = true,
            "__landlock" => {
                // Hidden launcher: heycode-sandbox re-exec's us to apply a
                // Landlock ruleset then exec the wrapped command. Args:
                // __landlock <rules-json> -- <argv...>. Never documented.
                apply_landlock_and_exec(&args[i + 1..]); // diverges
            }
            "--fake" => cli.fake = true,
            other if other.starts_with("-c") && other.len() > 2 => {
                cli.resume = Some(std::path::PathBuf::from(&other[2..]));
            }
            other if cli.prompt.is_none() && !other.starts_with('-') => {
                cli.prompt = Some(other.to_owned())
            }
            other => return Err(format!("unknown argument `{other}`\n{}", usage())),
        }
        i += 1;
    }
    if cli.output_format.is_some() && !(cli.mode_run || cli.prompt.is_some()) {
        return Err(format!(
            "--output-format applies to a headless `heycode run`\n{}",
            usage()
        ));
    }
    if cli.mode_config
        && (cli.prompt.is_some()
            || cli.resume.is_some()
            || cli.resume_latest
            || cli.resume_picker
            || cli.screen_reader
            || !cli.attachments.is_empty())
    {
        return Err(format!(
            "config does not accept prompts, resume or display options\n{}",
            usage()
        ));
    }
    if cli.resume_picker
        && (cli.mode_run
            || cli.prompt.is_some()
            || cli.mode_acp
            || cli.mode_setup
            || cli.mode_app_server)
    {
        return Err("--resume without an id opens an interactive conversation picker; use --resume <id/path> for a headless run".to_owned());
    }
    if cli.mode_doctor
        && (cli.prompt.is_some() || cli.resume.is_some() || cli.resume_latest || cli.resume_picker)
    {
        return Err(format!(
            "doctor does not accept prompts or resume options\n{}",
            usage()
        ));
    }
    if !cli.mode_doctor && (cli.doctor_composition || cli.json) {
        return Err(format!(
            "--composition/--json are doctor options\n{}",
            usage()
        ));
    }
    if !cli.attachments.is_empty()
        && (cli.mode_setup || cli.mode_acp || cli.mode_doctor || cli.prompt.is_none())
    {
        return Err(format!(
            "--image/--document require a one-shot prompt and are unavailable in setup/acp/doctor\n{}",
            usage()
        ));
    }
    if cli.screen_reader
        && (cli.mode_run
            || cli.mode_setup
            || cli.mode_acp
            || cli.mode_app_server
            || cli.mode_doctor
            || cli.mode_mcp
            || cli.mode_plugin
            || cli.mode_release
            || cli.prompt.is_some())
    {
        return Err(format!(
            "--screen-reader is available only in the interactive TUI\n{}",
            usage()
        ));
    }
    Ok(cli)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppServerCommand {
    workspace: std::path::PathBuf,
    resume: Option<AppServerResumeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppServerResumeId(String);

impl AppServerResumeId {
    fn parse(value: &str) -> Result<Self, String> {
        let parsed = uuid::Uuid::parse_str(value)
            .map_err(|_| "--resume requires a canonical heycode session id".to_owned())?;
        if parsed.hyphenated().to_string() != value {
            return Err("--resume requires a canonical heycode session id".to_owned());
        }
        Ok(Self(value.to_owned()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

fn parse_app_server_command(args: &[String]) -> Result<AppServerCommand, String> {
    let mut stdio_v1 = false;
    let mut workspace = None;
    let mut resume = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--stdio-v1" if !stdio_v1 => stdio_v1 = true,
            "--workspace" if workspace.is_none() => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--workspace needs an absolute path".to_owned())?;
                let path = std::path::PathBuf::from(value);
                if !path.is_absolute() {
                    return Err("--workspace needs an absolute path".to_owned());
                }
                workspace = Some(path);
            }
            "--resume" if resume.is_none() => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--resume needs a session id".to_owned())?;
                resume = Some(AppServerResumeId::parse(value)?);
            }
            "--help" => return Err(usage()),
            unknown => {
                return Err(format!(
                    "unknown app-server argument `{unknown}`\n{}",
                    usage()
                ));
            }
        }
        index += 1;
    }
    if !stdio_v1 {
        return Err("app-server requires --stdio-v1".to_owned());
    }
    let workspace = workspace.ok_or_else(|| "app-server requires --workspace".to_owned())?;
    Ok(AppServerCommand { workspace, resume })
}

fn tui_display_mode(cli: &Cli) -> heycode_tui::TuiDisplayMode {
    if cli.screen_reader {
        heycode_tui::TuiDisplayMode::ScreenReader
    } else {
        heycode_tui::TuiDisplayMode::Automatic
    }
}

// ─── interactive helpers (main-process only) ────────────────────────────────

fn ask(prompt: &str, default: Option<&str>) -> anyhow::Result<String> {
    match default {
        Some(d) => print!("{prompt} [{d}]: "),
        None => print!("{prompt}: "),
    }
    use std::io::Write as _;
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let trimmed = line.trim();
    Ok(if trimmed.is_empty() {
        default.unwrap_or("").to_owned()
    } else {
        trimmed.to_owned()
    })
}

/// Read one line with characters masked as `*` (raw-mode key loop).
///
/// # Errors
/// Terminal control failures; Ctrl+C aborts with a cancelled error.
pub fn read_masked_line(prompt: &str) -> anyhow::Result<String> {
    use crossterm::event::{Event, KeyCode, KeyEventKind};
    use std::io::Write as _;

    print!("{prompt}");
    std::io::stdout().flush()?;
    crossterm::terminal::enable_raw_mode()?;
    let result = (|| -> anyhow::Result<String> {
        let mut out = String::new();
        loop {
            if crossterm::event::poll(std::time::Duration::from_millis(250))?
                && let Event::Key(key) = crossterm::event::read()?
            {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Enter => {
                        println!();
                        return Ok(out);
                    }
                    KeyCode::Char('c')
                        if key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL) =>
                    {
                        anyhow::bail!("setup cancelled");
                    }
                    KeyCode::Backspace => {
                        if out.pop().is_some() {
                            print!("\u{8} \u{8}");
                            std::io::stdout().flush()?;
                        }
                    }
                    KeyCode::Char(c) => {
                        out.push(c);
                        print!("*");
                        std::io::stdout().flush()?;
                    }
                    _ => {}
                }
            }
        }
    })();
    crossterm::terminal::disable_raw_mode()?;
    result
}

/// Validate a pasted key's shape so typos fail at setup, not mid-turn.
#[must_use]
pub fn key_shape_error(key: &str) -> Option<&'static str> {
    let key = key.trim();
    if key.len() < 12 {
        return Some("key looks too short");
    }
    None
}

/// Credentials `heycode setup` stored during this run. A cancelled or failed
/// wizard removes them again: a half-finished setup must not leave a key in
/// the credential file that the next start then trips over.
#[derive(Default)]
struct SetupWrites {
    references: Vec<String>,
}

impl SetupWrites {
    fn rollback(&self, heycode_home: &std::path::Path) {
        for reference in &self.references {
            match heycode_cli::delete_credential_at(reference, heycode_home) {
                Ok(Some(provider)) => {
                    eprintln!(
                        "heycode: removed the {reference} credential saved this run ({provider})"
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    eprintln!(
                        "heycode: could not remove the {reference} credential saved this run: {error:#}"
                    );
                }
            }
        }
    }
}

/// Ask for a key until the provider accepts one, then store it.
///
/// The key is checked live *before* it is stored: a typo fails at this prompt
/// instead of on the first turn, and is never persisted. When the provider
/// cannot be reached the key is stored anyway with a warning — no network is
/// not the same as a wrong key — and providers without a reviewed probe are
/// stored unverified, which is said out loud.
async fn ask_and_store_key(
    provider: &str,
    reference: &str,
    base_url: Option<&str>,
    heycode_home: &std::path::Path,
    writes: &mut SetupWrites,
) -> anyhow::Result<()> {
    for _ in 0..3 {
        let key = read_masked_line(&format!("\nPaste your {provider} API key ({reference}): "))?;
        if let Some(hint) = key_shape_error(&key) {
            println!("  ✗ {hint} — try again.");
            continue;
        }
        let key = key.trim();
        match heycode_cli::check_new_key(provider, base_url, key).await {
            Ok(heycode_cli::NewKeyCheck::Accepted) => println!("  ✓ {provider} accepted the key"),
            Ok(heycode_cli::NewKeyCheck::NotCheckable) => {
                println!(
                    "  · {provider} keys cannot be checked here; it will be verified on first use"
                );
            }
            Err(heycode_authorization_api_key::ApiKeyValidationFailure::Unauthorized) => {
                println!("  ✗ {provider} rejected this key — try again.");
                continue;
            }
            Err(heycode_authorization_api_key::ApiKeyValidationFailure::Cancelled) => {
                anyhow::bail!("setup cancelled");
            }
            Err(failure) => {
                println!(
                    "  ! could not check the key ({}); saving it anyway — it will be verified on first use",
                    failure.code()
                );
            }
        }
        let credential_provider = heycode_cli::write_credential_at(reference, key, heycode_home)?;
        writes.references.push(reference.to_owned());
        println!("  ✓ saved via credential provider `{credential_provider}`");
        return Ok(());
    }
    anyhow::bail!("API key was invalid after 3 attempts")
}

/// The interactive first-run / reconfiguration wizard.
///
/// # Errors
/// Non-interactive stdin, invalid input after 3 tries, or persistence errors.
/// Any credential stored during a failed or cancelled run is removed again.
pub async fn run_setup_wizard(
    existing: &Config,
    setup: &heycode_cli::SetupCatalog,
    heycode_home: &std::path::Path,
) -> anyhow::Result<Config> {
    let mut writes = SetupWrites::default();
    let outcome = setup_steps(existing, setup, heycode_home, &mut writes).await;
    if outcome.is_err() {
        writes.rollback(heycode_home);
    }
    outcome
}

async fn setup_steps(
    existing: &Config,
    setup: &heycode_cli::SetupCatalog,
    heycode_home: &std::path::Path,
    writes: &mut SetupWrites,
) -> anyhow::Result<Config> {
    if !std::io::stdin().is_terminal() {
        anyhow::bail!("setup needs an interactive terminal");
    }
    println!("Welcome to heycode — let's configure your agent.\n");

    // 1. Provider
    let providers = setup.providers();
    println!("Pick a provider:");
    for (i, row) in providers.iter().enumerate() {
        let have = row
            .credential_reference
            .as_deref()
            .and_then(std::env::var_os)
            .is_some_and(|value| !value.is_empty());
        let state = if have {
            "(key found in environment)"
        } else if row.has_catalog {
            "(live model catalog)"
        } else {
            "(provider default; live catalog unavailable)"
        };
        println!("  {}) {} [{}] {state}", i + 1, row.display_name, row.id);
    }
    let provider_default = providers
        .iter()
        .position(|row| row.id == existing.llm.provider)
        .map_or(1, |index| index + 1)
        .to_string();
    let pick = ask("Provider number or id", Some(&provider_default))?;
    let choice = heycode_cli::resolve_setup_provider(&providers, &pick)?;
    let provider = choice.id.clone();

    // 2. Key (masked) unless already present in the environment.
    let mut cfg = existing.clone();
    if cfg.llm.provider != provider {
        cfg.llm.base_url = None;
        cfg.llm.api_key_env = None;
    }
    cfg.apply_patch(&format!("llm.provider={provider}"))?;
    if let Some(reference) = choice.credential_reference.as_deref() {
        cfg.llm.api_key_env = Some(reference.to_owned());
        if std::env::var_os(reference).is_none_or(|value| value.is_empty()) {
            ask_and_store_key(
                &provider,
                reference,
                cfg.llm.base_url.as_deref(),
                heycode_home,
                writes,
            )
            .await?;
        } else {
            println!("Using {reference} from the environment.");
        }
    } else {
        println!("This provider does not require an API-key credential.");
    }

    // 3. Model
    let models = setup
        .models(&provider, tokio_util::sync::CancellationToken::new())
        .await?;
    if let Some(warning) = models.warning.as_deref() {
        println!("  ! {warning}");
    }
    println!("Pick a model:");
    for (index, row) in models.models.iter().enumerate() {
        let recommended = if row.id == models.recommended {
            " (recommended)"
        } else {
            ""
        };
        println!(
            "  {}) {} [{}]{recommended}",
            index + 1,
            row.display_name,
            row.id
        );
    }
    let model_pick = ask("Model number or id", Some(&models.recommended))?;
    let model = heycode_cli::resolve_setup_model(&models, &model_pick)?;
    cfg.apply_patch(&format!("llm.model={model}"))?;

    // 4. Approval policy
    let approval = ask(
        "Permissions — full_access, accepted_edits, or default?",
        Some("ask"),
    )?;
    cfg.apply_patch(&format!("approval.mode={approval}"))?;

    // Persist to the home config when no project config exists; otherwise tell
    // the user what to paste (project files are theirs to own).
    let project = heycode_config::project_config_path(std::path::Path::new("."));
    if project.is_file() {
        println!(
            "\nProject heycode.toml detected — add this there:\n\n[llm]\nprovider = \"{provider}\"\nmodel = \"{model}\""
        );
    } else {
        let home_cfg = heycode_home.join("config.toml");
        if let Some(parent) = home_cfg.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let rendered = toml_render(&cfg)?;
        write_setup_config(&home_cfg, rendered.as_bytes())?;
        println!("\n✓ saved configuration to {}", home_cfg.display());
    }
    println!("You're set. Try: heycode run \"list the files here\"");
    Ok(cfg)
}

/// Minimal TOML writer for the sections we own.
fn toml_render(cfg: &Config) -> anyhow::Result<String> {
    // Deliberately no `[profile] plugins`: omitting it means "the built-in
    // profile", which tracks new plugins automatically. Writing a snapshot
    // would freeze this install's plugin set at setup time.
    let mut llm = toml::map::Map::new();
    llm.insert(
        "provider".to_owned(),
        toml::Value::String(cfg.llm.provider.clone()),
    );
    llm.insert(
        "model".to_owned(),
        toml::Value::String(cfg.llm.model.clone()),
    );
    if let Some(base_url) = cfg.llm.base_url.as_ref() {
        llm.insert("base_url".to_owned(), toml::Value::String(base_url.clone()));
    }
    if let Some(reference) = cfg.llm.api_key_env.as_ref() {
        llm.insert(
            "api_key_env".to_owned(),
            toml::Value::String(reference.clone()),
        );
    }

    let mut tools = toml::map::Map::new();
    tools.insert(
        "bash_timeout_ms".to_owned(),
        toml::Value::Integer(i64::try_from(cfg.tools.bash_timeout_ms)?),
    );
    tools.insert(
        "read_max_bytes".to_owned(),
        toml::Value::Integer(i64::try_from(cfg.tools.read_max_bytes)?),
    );
    tools.insert(
        "read_max_lines".to_owned(),
        toml::Value::Integer(i64::try_from(cfg.tools.read_max_lines)?),
    );

    let mut approval = toml::map::Map::new();
    if let Some(mode) = cfg.approval.mode {
        approval.insert("mode".to_owned(), toml::Value::String(mode.to_string()));
    }

    let mut root = toml::map::Map::new();
    root.insert(
        "schema_version".to_owned(),
        toml::Value::Integer(i64::from(CONFIG_SCHEMA_VERSION)),
    );
    root.insert("llm".to_owned(), toml::Value::Table(llm));
    root.insert("tools".to_owned(), toml::Value::Table(tools));
    root.insert("approval".to_owned(), toml::Value::Table(approval));
    Ok(toml::to_string_pretty(&toml::Value::Table(root))?)
}

fn write_setup_config(path: &std::path::Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write as _;

    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        anyhow::bail!(
            "refusing to replace non-regular setup config {}",
            path.display()
        );
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let mut options = atomic_write_file::AtomicWriteFile::options();
    #[cfg(unix)]
    {
        use atomic_write_file::unix::OpenOptionsExt as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        options.preserve_mode(false).mode(0o600);
    }
    let mut output = options.open(path)?;
    output.write_all(bytes)?;
    output.commit()?;
    Ok(())
}

fn load_config_for_startup(
    path: Option<&std::path::Path>,
    allow_project: bool,
) -> anyhow::Result<LoadedConfig> {
    let loaded = Config::load_for_startup_with_project(path, BUILTIN_PLUGIN_ORDER, allow_project)?;
    if let Some(notice) = loaded.migration.as_ref() {
        print_migration_notice(notice);
    }
    // Headless and app-server runs read stderr; the TUI repeats these on its
    // transcript because the alternate screen hides stderr.
    for warning in &loaded.warnings {
        eprintln!("heycode: config: {warning}");
    }
    Ok(loaded)
}

fn open_workspace_trust(
    cwd: &std::path::Path,
    home: &std::path::Path,
) -> anyhow::Result<heycode_trust::WorkspaceTrustService> {
    Ok(heycode_trust::WorkspaceTrustService::file(
        cwd,
        home.join("trust.toml"),
        heycode_cli::project_content_policy(),
    )?)
}

fn print_migration_notice(notice: &ConfigMigrationNotice) {
    if notice
        .changes
        .contains(&ConfigMigrationChange::RenameLegacyAutoApproval)
    {
        eprintln!("heycode: your previous automatic-approval setting is now named Full access.");
    }

    let activated = notice.changes.iter().find_map(|change| match change {
        ConfigMigrationChange::UseBuiltinProfile {
            activated_plugins, ..
        } => Some(activated_plugins.join(", ")),
        ConfigMigrationChange::RenameLegacyAutoApproval
        | ConfigMigrationChange::UseHomeCredentialStore
        | ConfigMigrationChange::SetSchemaVersion { .. }
        | ConfigMigrationChange::AddRequiredProfilePlugin { .. }
        | ConfigMigrationChange::MoveAuthorizationFlowToProviderPlugin { .. }
        | ConfigMigrationChange::ReplaceRetiredDeepSeekDefault { .. } => None,
    });
    let model_change = notice.changes.iter().find_map(|change| match change {
        ConfigMigrationChange::ReplaceRetiredDeepSeekDefault { from, to } => {
            Some((from.as_str(), to.as_str()))
        }
        ConfigMigrationChange::RenameLegacyAutoApproval
        | ConfigMigrationChange::UseHomeCredentialStore
        | ConfigMigrationChange::SetSchemaVersion { .. }
        | ConfigMigrationChange::AddRequiredProfilePlugin { .. }
        | ConfigMigrationChange::MoveAuthorizationFlowToProviderPlugin { .. }
        | ConfigMigrationChange::UseBuiltinProfile { .. } => None,
    });
    let required_profile_plugin = notice.changes.iter().find_map(|change| match change {
        ConfigMigrationChange::AddRequiredProfilePlugin {
            plugin,
            required_by,
        } => Some((plugin.as_str(), required_by.as_str())),
        ConfigMigrationChange::RenameLegacyAutoApproval
        | ConfigMigrationChange::UseHomeCredentialStore
        | ConfigMigrationChange::SetSchemaVersion { .. }
        | ConfigMigrationChange::UseBuiltinProfile { .. }
        | ConfigMigrationChange::MoveAuthorizationFlowToProviderPlugin { .. }
        | ConfigMigrationChange::ReplaceRetiredDeepSeekDefault { .. } => None,
    });
    let moved_authorization_flow = notice.changes.iter().find_map(|change| match change {
        ConfigMigrationChange::MoveAuthorizationFlowToProviderPlugin {
            flow,
            from_plugin,
            to_plugin,
        } => Some((flow.as_str(), from_plugin.as_str(), to_plugin.as_str())),
        ConfigMigrationChange::RenameLegacyAutoApproval
        | ConfigMigrationChange::UseHomeCredentialStore
        | ConfigMigrationChange::SetSchemaVersion { .. }
        | ConfigMigrationChange::UseBuiltinProfile { .. }
        | ConfigMigrationChange::AddRequiredProfilePlugin { .. }
        | ConfigMigrationChange::ReplaceRetiredDeepSeekDefault { .. } => None,
    });
    if notice
        .changes
        .contains(&ConfigMigrationChange::UseHomeCredentialStore)
    {
        eprintln!(
            "heycode: credentials-keychain is retired; use credentials-file (credentials.toml in the heycode home). OS entries are never accessed or imported."
        );
    }
    match &notice.disposition {
        ConfigMigrationDisposition::Applied { backup_path } => {
            eprintln!(
                "heycode: migrated config schema to {}: {} (backup: {})",
                notice.to,
                notice.path.display(),
                backup_path.display()
            );
            if let Some(plugins) = activated {
                eprintln!("heycode: restored built-in profile capabilities: {plugins}");
            }
            if let Some((from, to)) = model_change {
                eprintln!("heycode: replaced retired DeepSeek default `{from}` with `{to}`");
            }
            if let Some((plugin, required_by)) = required_profile_plugin {
                eprintln!("heycode: added required profile plugin `{plugin}` for `{required_by}`");
            }
            if let Some((flow, from_plugin, to_plugin)) = moved_authorization_flow {
                eprintln!(
                    "heycode: moved authorization flow `{flow}` from `{from_plugin}` to `{to_plugin}`"
                );
            }
        }
        ConfigMigrationDisposition::Pending => {
            eprintln!(
                "heycode: config migration pending for user-owned file {} (schema {}); it was not rewritten",
                notice.path.display(),
                notice.to
            );
            if let Some(plugins) = activated {
                eprintln!("heycode: applying the built-in profile would activate: {plugins}");
            }
            if let Some((from, to)) = model_change {
                eprintln!(
                    "heycode: migration would replace retired DeepSeek default `{from}` with `{to}`"
                );
            }
            if let Some((plugin, required_by)) = required_profile_plugin {
                eprintln!(
                    "heycode: migration would add required profile plugin `{plugin}` for `{required_by}`"
                );
            }
            if let Some((flow, from_plugin, to_plugin)) = moved_authorization_flow {
                eprintln!(
                    "heycode: migration would move authorization flow `{flow}` from `{from_plugin}` to `{to_plugin}`"
                );
            }
        }
    }
}

fn selected_connection_restart_args(args: &[String]) -> Vec<String> {
    let mut restarted = Vec::with_capacity(args.len());
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--provider" | "--model" | "--protocol" => index += 2,
            "--resume" | "-r" => {
                index += 1;
                if args.get(index).is_some_and(|value| !value.starts_with('-')) {
                    index += 1;
                }
            }
            "--set"
                if args.get(index + 1).is_some_and(|value| {
                    value.split_once('=').is_some_and(|(key, _)| {
                        matches!(
                            key.trim(),
                            "llm.provider"
                                | "llm.model"
                                | "llm.protocol"
                                | "llm.base_url"
                                | "llm.api_key_env"
                        )
                    })
                }) =>
            {
                index += 2
            }
            "-c" | "--continue" => {
                let legacy_path = args[index] == "-c";
                index += 1;
                if legacy_path
                    && args.get(index).is_some_and(|value| {
                        !value.starts_with('-') && std::path::Path::new(value).exists()
                    })
                {
                    index += 1;
                }
            }
            _ => {
                restarted.push(args[index].clone());
                index += 1;
            }
        }
    }
    restarted
}

fn connection_restart_args(args: &[String]) -> Vec<String> {
    args.to_vec()
}

// ─── entry ──────────────────────────────────────────────────────────────────

/// Set while an interactive shell is on screen for the current `run`, so a
/// failure after that point is the shell's to report and a failure before it
/// is a composition failure the previous world can recover from.
static TUI_SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Receipt or failure the next interactive run shows after recomposition.
static STARTUP_NOTICE: std::sync::Mutex<Option<heycode_tui::app::StartupNotice>> =
    std::sync::Mutex::new(None);

/// An explicit reload has retired the old plugin world. Earlier successful
/// session/display switches must not catch this later failure and retry an
/// unrelated old invocation against the same broken durable configuration.
#[derive(Debug)]
struct PluginReloadFailure {
    session_id: String,
    source: anyhow::Error,
}

impl std::fmt::Display for PluginReloadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Plugin reload failed; session {} remains saved. Repair the reported configuration and resume it",
            self.session_id
        )
    }
}

impl std::error::Error for PluginReloadFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Re-run with `restarted`; if that world fails before its shell appears, come
/// back to `previous` with the reason on screen instead of exiting the process.
///
/// `/resume` of a session another process holds, `/fork` of a log this build
/// cannot reopen and `/profile` naming a broken profile all used to end the
/// program with one line on stderr. The user asked to switch, not to quit.
fn run_or_fall_back(
    restarted: Vec<String>,
    previous: Vec<String>,
    what: &str,
) -> Result<i32, anyhow::Error> {
    TUI_SHOWN.store(false, std::sync::atomic::Ordering::SeqCst);
    match run(restarted) {
        Err(error)
            if !TUI_SHOWN.load(std::sync::atomic::Ordering::SeqCst)
                && error.downcast_ref::<PluginReloadFailure>().is_none() =>
        {
            if let Ok(mut notice) = STARTUP_NOTICE.lock() {
                *notice = Some(heycode_tui::app::StartupNotice::Error(format!(
                    "{what} failed: {error:#} — previous session restored"
                )));
            }
            run(previous)
        }
        other => other,
    }
}

fn run(args: Vec<String>) -> Result<i32, anyhow::Error> {
    if args == ["--version"] || args == ["-V"] {
        println!("HeyCode {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }
    if args.first().is_some_and(|argument| argument == "update") {
        println!("{}", heycode_cli::update::run(&args[1..])?);
        return Ok(0);
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if let Some(code) = session_background::dispatch(&args)? {
        return Ok(code);
    }
    let cli = parse_args(&args).map_err(anyhow::Error::msg)?;
    if args.is_empty() && std::io::stdin().is_terminal() {
        heycode_cli::update::start_automatic();
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if let Some(code) = session_background::interactive(&args, &cli)? {
        return Ok(code);
    }
    let app_server_command = cli
        .mode_app_server
        .then(|| parse_app_server_command(&cli.app_server_args))
        .transpose()
        .map_err(anyhow::Error::msg)?;
    if let Some(command) = app_server_command.as_ref() {
        let canonical = std::fs::canonicalize(&command.workspace).map_err(|_| {
            anyhow::anyhow!("app-server workspace must be an existing absolute directory")
        })?;
        if !canonical.is_dir() {
            return Err(anyhow::anyhow!(
                "app-server workspace must be an existing absolute directory"
            ));
        }
        std::env::set_current_dir(canonical)?;
    }
    let heycode_home = heycode_cli::heycode_home()?;
    let sessions_root = heycode_home.join("sessions");

    if cli.mode_setup {
        if cli.profile.is_some() {
            return Err(anyhow::anyhow!(
                "--profile cannot be used with setup; setup edits base connection settings"
            ));
        }
        let loaded = load_config_for_startup(cli.config_path.as_deref(), false)?;
        let existing = loaded.config;
        let mut setup_world = heycode_cli::compose_setup_world(heycode_cli::SetupWorldOptions {
            settings_user_path: heycode_home.join("settings.toml"),
            credentials_root: heycode_home.clone(),
            catalog_cache_path: heycode_home.join("cache/models.json"),
        })?;
        let runtime = tokio::runtime::Runtime::new()?;
        let outcome = runtime.block_on(run_setup_wizard(
            &existing,
            setup_world.catalog(),
            &heycode_home,
        ));
        setup_world.shutdown();
        outcome?;
        return Ok(0);
    }

    if cli.mode_release {
        let command =
            heycode_cli::release_cli::parse(&cli.release_args).map_err(std::io::Error::other)?;
        let loaded = load_config_for_startup(cli.config_path.as_deref(), false)?;
        let config_state =
            heycode_config::ConfigVersionState::Current(loaded.config.schema_version);
        return match heycode_cli::release_cli::run(
            &command,
            heycode_home.join("settings.toml"),
            heycode_home.join("plugins"),
            config_state,
        ) {
            Ok(rendered) => {
                println!("{rendered}");
                Ok(0)
            }
            Err(message) => {
                eprintln!("{message}");
                Ok(1)
            }
        };
    }

    let process_cwd = std::env::current_dir()?;
    let trust_home = heycode_home.clone();
    let workspace_trust = open_workspace_trust(&process_cwd, &trust_home)?;
    let trust_frontend = if cli.mode_acp {
        heycode_trust::TrustFrontend::Acp
    } else if cli.mode_doctor
        || cli.mode_config
        || cli.mode_mcp
        || cli.mode_plugin
        || cli.mode_run
        || cli.prompt.is_some()
        || !std::io::stdin().is_terminal()
    {
        heycode_trust::TrustFrontend::Headless
    } else {
        heycode_trust::TrustFrontend::Interactive
    };
    // `heycode config show`, `heycode mcp …` and `heycode plugin …` in a workspace
    // nobody has trusted yet must still answer: they read the home only, so
    // the workspace is treated as restricted for this run (no `heycode.toml`).
    let startup_explicit_trust =
        if (cli.mode_app_server || cli.mode_config || cli.mode_mcp || cli.mode_plugin)
            && cli.workspace_trust.is_none()
        {
            Some(heycode_trust::ExplicitWorkspaceTrust::RestrictedOnce)
        } else {
            cli.workspace_trust
        };
    let trust_startup = workspace_trust.prepare_startup(trust_frontend, startup_explicit_trust)?;
    let (allow_project, trust_dialog) = match trust_startup {
        heycode_trust::TrustStartupState::Ready(snapshot) => (
            snapshot.decision() == heycode_trust::WorkspaceTrustDecision::Trusted,
            None,
        ),
        heycode_trust::TrustStartupState::Prompt(_) => {
            (false, Some(workspace_trust.dialog_prompt()?))
        }
    };
    let loaded = load_config_for_startup(cli.config_path.as_deref(), allow_project)?;
    let config_migration = loaded.migration;
    let mut startup_warnings: Vec<String> = loaded
        .warnings
        .iter()
        .map(|warning| format!("config: {warning}"))
        .collect();
    let mut cfg = loaded.config;
    for patch in &cli.sets {
        cfg.apply_patch(patch)?;
    }
    if (cli.mode_run || cli.prompt.is_some())
        && cfg.approval.effective(false) == heycode_config::ApprovalMode::Ask
    {
        eprintln!(
            "heycode: approval.mode=ask has no prompt in a headless run; tool calls will be denied \
             (pass --approval full_access to allow them, or use the interactive TUI)"
        );
    }
    if let Some(p) = cli.provider.clone() {
        cfg.apply_patch(&format!("llm.provider={p}"))?;
    }
    if let Some(m) = cli.model.clone() {
        cfg.apply_patch(&format!("llm.model={m}"))?;
    }
    if let Some(protocol) = cli.protocol.clone() {
        cfg.apply_patch(&format!("llm.protocol={protocol}"))?;
    }
    if let Some(max_output_tokens) = cli.max_output_tokens.clone() {
        cfg.apply_patch(&format!("llm.max_output_tokens={max_output_tokens}"))?;
    }
    if cli.mode_config {
        return Ok(match cli.config_operation.as_deref() {
            Some("show") => {
                if !allow_project
                    && heycode_config::project_config_path(std::path::Path::new(".")).is_file()
                {
                    eprintln!(
                        "heycode: ./heycode.toml not included: this workspace is not trusted (pass --trust-workspace or answer the trust prompt in the TUI)"
                    );
                }
                let project_settings = workspace_trust
                    .access(heycode_trust::ProjectInputKind::Settings)?
                    .is_allowed()
                    .then(|| heycode_config::project_state_dir(&process_cwd).join("settings.toml"));
                let route = heycode_cli::apply_startup_connection_from_files(
                    &mut cfg,
                    heycode_home.join("settings.toml"),
                    project_settings,
                )?;
                println!("{}", cfg.report().render());
                println!(
                    "runtime = {} (effective startup connection)",
                    route.runtime()
                );
                0
            }
            Some(other) => {
                eprintln!("heycode config: unknown operation `{other}`; operations: show");
                2
            }
            None => {
                println!(
                    "heycode config <operation>\n  show    print every effective value and the file or flag it came from"
                );
                0
            }
        });
    }
    let profile_layers: Vec<heycode_config::ProfileLayer> = cli
        .profile
        .as_deref()
        .map(|name| heycode_config::NamedProfileStore::new(heycode_home.clone()).load(name))
        .transpose()?
        .into_iter()
        .collect();
    if cli.mode_plugin {
        let command =
            heycode_cli::plugin_cli::parse(&cli.plugin_args).map_err(std::io::Error::other)?;
        let (_context, lifecycle) = heycode_cli::plugin_cli::compose_lifecycle_world(
            heycode_home.join("settings.toml"),
            heycode_home.join("plugins"),
        )?;
        return match heycode_cli::plugin_cli::run_with_cache(
            &lifecycle,
            Some(&heycode_home.join("plugins")),
            command.as_ref(),
        ) {
            Ok(rendered) => {
                println!("{rendered}");
                Ok(0)
            }
            Err(message) => {
                eprintln!("{message}");
                Ok(1)
            }
        };
    }
    if cli.mode_mcp {
        // Parsed before anything is composed: a malformed command must fail
        // instantly rather than after plugin activation.
        let command = heycode_cli::mcp_cli::parse(&cli.mcp_args).map_err(std::io::Error::other)?;
        let (_context, management) =
            heycode_cli::mcp_cli::compose_management_world(heycode_home.join("settings.toml"))?;
        return match heycode_cli::mcp_cli::run(&management, &command) {
            Ok(rendered) => {
                println!("{rendered}");
                Ok(0)
            }
            Err(message) => {
                eprintln!("{message}");
                Ok(1)
            }
        };
    }
    if cli.mode_doctor {
        let fake: Option<Arc<dyn heycode_llm::Provider>> = (cli.fake
            || std::env::var_os("HEYCODE_FAKE").is_some())
        .then(|| Arc::new(FakeProvider::new(Vec::new())) as Arc<dyn heycode_llm::Provider>);
        let options = WorldOptions {
            config: &cfg,
            trust: workspace_trust.clone(),
            config_migration: config_migration.as_ref(),
            profile_layers: &profile_layers,
            sessions_dir: sessions_root.clone(),
            attachments_dir: heycode_home.join("attachments"),
            attachment_max_bytes: heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
            session_source: heycode_session::SessionSource::Headless,
            approval_prompter: heycode_cli::ApprovalPrompter::Proxied,
            settings_user_path: heycode_home.join("settings.toml"),
            credentials_root: heycode_home.clone(),
            catalog_cache_path: heycode_home.join("cache/models.json"),
            settings_watch: false,
            onboarding_required: false,
            credential_validated_at_ms: None,
            cwd: std::env::current_dir()?,
            fake,
            resume: None,
        };
        if cli.doctor_composition {
            let graph = heycode_cli::inspect_world(&options);
            let activation = graph
                .healthy
                .then(|| heycode_cli::probe_world_activation(&options));
            let report = heycode_core::CompositionDoctorReport::new(graph, activation);
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("{}", report.render_human());
            }
            return Ok(i32::from(!report.healthy));
        }
        let runtime = tokio::runtime::Runtime::new()?;
        let report = runtime.block_on(diagnose_world(
            &options,
            tokio_util::sync::CancellationToken::new(),
        ))?;
        if cli.json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            println!("{}", report.render_human());
        }
        return Ok(i32::from(!report.healthy));
    }

    // Interactive first-run enters the TUI-owned wizard. Headless/ACP fail
    // before composition because they have no human dialog surface.
    let interactive = std::io::stdin().is_terminal();
    let offline = cli.fake || std::env::var_os("HEYCODE_FAKE").is_some();
    let startup_cwd = std::env::current_dir()?;
    let project_settings = workspace_trust
        .access(heycode_trust::ProjectInputKind::Settings)?
        .is_allowed()
        .then(|| heycode_config::project_state_dir(&startup_cwd).join("settings.toml"));
    let setup_required = heycode_cli::startup_requires_setup_from_files(
        heycode_home.join("settings.toml"),
        project_settings.clone(),
    )?;
    let requested_runtime = heycode_cli::apply_startup_connection_from_files(
        &mut cfg,
        heycode_home.join("settings.toml"),
        project_settings,
    )?
    .runtime()
    .to_owned();
    let delegated_runtime = requested_runtime != "native";
    if delegated_runtime && offline {
        return Err(anyhow::anyhow!(
            "--fake cannot activate delegated runtime `{requested_runtime}`"
        ));
    }
    if delegated_runtime
        && (cli.prompt.is_some() || cli.mode_acp || (!interactive && !cli.mode_app_server))
    {
        return Err(anyhow::anyhow!(
            "delegated runtime `{requested_runtime}` currently requires the interactive TUI"
        ));
    }
    let runtime = tokio::runtime::Runtime::new()?;
    let can_run_setup = interactive
        && cli.prompt.is_none()
        && !cli.mode_run
        && !cli.mode_acp
        && !cli.mode_app_server;
    if setup_required && !can_run_setup {
        return Err(anyhow::anyhow!(
            "the active route was logged out; start heycode interactively and finish connection setup"
        ));
    }
    let mut onboarding_required = setup_required;
    let mut credential_validated_at_ms = None;
    if !setup_required && !offline && !delegated_runtime && provider_uses_credential(&cfg) {
        if !provider_key_present(&cfg)? {
            if interactive && cli.prompt.is_none() && !cli.mode_acp && !cli.mode_app_server {
                onboarding_required = true;
            } else if cli.prompt.is_some() || !interactive {
                let reference = match cfg.llm.api_key_env.as_deref() {
                    Some(reference) => reference.to_owned(),
                    None => heycode_cli::default_provider_reference(&cfg.llm.provider)
                        .map_or_else(|_| "its API key".to_owned(), str::to_owned),
                };
                return Err(anyhow::anyhow!(
                    "no API key for `{}` — run `heycode setup`, export {reference}, or use --fake",
                    cfg.llm.provider
                ));
            }
        } else if heycode_cli::provider_has_preflight_validation(&cfg.llm.provider) {
            let source = heycode_cli::credential_source_label(heycode_cli::credential_source_at(
                &cfg,
                &heycode_home,
            )?);
            match runtime.block_on(heycode_cli::validate_provider_credential(&cfg)) {
                Ok(checked_at_ms) => credential_validated_at_ms = Some(checked_at_ms),
                // No network is not a broken key. Start anyway and say so; the
                // first turn reports the real error if there is one.
                Err(heycode_authorization_api_key::ApiKeyValidationFailure::Network) => {
                    let banner = format!(
                        "could not reach {} to check the stored credential (source: {source}); continuing unverified",
                        cfg.llm.provider
                    );
                    eprintln!("heycode: {banner}");
                    startup_warnings.push(banner);
                }
                Err(failure)
                    if interactive
                        && cli.prompt.is_none()
                        && !cli.mode_acp
                        && !cli.mode_app_server =>
                {
                    eprintln!(
                        "heycode: the stored {} credential was rejected ({}; source: {source}) — /connect replaces it",
                        cfg.llm.provider,
                        failure.code()
                    );
                    onboarding_required = true;
                }
                Err(failure) => {
                    return Err(anyhow::anyhow!(
                        "{} credential validation failed: {} (source: {source}) — run `heycode setup` to replace it",
                        cfg.llm.provider,
                        failure.code()
                    ));
                }
            }
        }
    }

    let resume = if setup_required {
        None
    } else if let Some(command) = app_server_command.as_ref() {
        command
            .resume
            .as_ref()
            .map(|id| sessions_root.join(id.as_str()))
    } else {
        match cli.resume.clone() {
            Some(path) => {
                // A bare canonical id is relative to the durable session store.
                let target = path.to_string_lossy();
                let by_id = AppServerResumeId::parse(target.as_ref());
                Some(if by_id.is_ok() && !path.exists() {
                    sessions_root.join(path)
                } else {
                    path
                })
            }
            None if cli.resume_latest => find_latest_session_in(&sessions_root, &startup_cwd)?,
            None => None,
        }
    };
    if cli.resume_latest && resume.is_none() {
        return Err(anyhow::anyhow!(
            "--continue: no previous conversation in {}; start heycode to begin one",
            startup_cwd.display()
        ));
    }

    // The unknown-workspace world exists only to render the trust modal. Its
    // session is intentionally ephemeral: no durable product session is
    // published before the user commits a trust decision and composition is
    // rebuilt from authoritative inputs.
    let pretrust_sessions = trust_dialog
        .as_ref()
        .map(|_| tempfile::tempdir())
        .transpose()?;
    let composed_sessions_root = pretrust_sessions
        .as_ref()
        .map_or_else(|| sessions_root.clone(), |root| root.path().to_path_buf());
    let composed_resume = if pretrust_sessions.is_some() {
        None
    } else {
        resume
    };

    let restart_sessions_root = sessions_root.clone();
    let completion = runtime.block_on(async move {
        let pretrust_sessions_active = pretrust_sessions.as_ref().map(|_| ());
        let _pretrust_sessions = pretrust_sessions;
        let fake: Option<Arc<dyn heycode_llm::Provider>> = if offline {
            Some(Arc::new(FakeProvider::repeating(vec![
                heycode_llm::StreamChunk::TextDelta("FAKE-REPLY: offline smoke response.".to_owned()),
                heycode_llm::StreamChunk::Finish(heycode_llm::FinishReason::Stop),
            ])))
        } else {
            None
        };

        // ACP boots one world per session/new; nothing shared to build here.
        if cli.mode_acp {
            let mut cfg_ref = cfg.clone();
            let acp_config_migration = config_migration.clone();
            let acp_profile_layers = profile_layers.clone();
            let acp_trust_home = trust_home.clone();
            let acp_explicit_trust = startup_explicit_trust;
            let acp_home = heycode_home.clone();
            let acp_sessions_root = sessions_root.clone();
            let startup_workspace_id = workspace_trust.snapshot()?.identity().id().clone();
            if std::env::var_os("HEYCODE_ACP_APPROVAL").is_some_and(|v| v == "ask") {
                let _ = cfg_ref.apply_patch("approval.mode=ask");
            }
            let fake_provider: Option<Arc<dyn heycode_llm::Provider>> = std::env::var_os("HEYCODE_FAKE")
                .map(|_| {
                    Arc::new(heycode_llm::testing::FakeProvider::new(vec![vec![
                        heycode_llm::StreamChunk::TextDelta(
                            "FAKE-REPLY: offline smoke response.".to_owned(),
                        ),
                        heycode_llm::StreamChunk::Usage(heycode_core::TokenUsage {
                            prompt_tokens: 8,
                            completion_tokens: 4,
                        }),
                        heycode_llm::StreamChunk::Finish(heycode_llm::FinishReason::Stop),
                    ]])) as Arc<dyn heycode_llm::Provider>
                });
            let make_world = move |client_cwd: Option<std::path::PathBuf>| {
                let process_cwd = std::env::current_dir()?;
                let cwd = match client_cwd {
                    Some(dir) if dir.is_absolute() => dir,
                    Some(dir) => process_cwd.join(dir),
                    None => process_cwd,
                };
                let client_trust = open_workspace_trust(&cwd, &acp_trust_home)?;
                let client_id = client_trust.snapshot()?.identity().id().clone();
                if allow_project && client_id != startup_workspace_id {
                    return Err(anyhow::anyhow!(
                        "ACP client cwd differs from the trusted project-config workspace"
                    ));
                }
                let explicit = match acp_explicit_trust {
                    Some(heycode_trust::ExplicitWorkspaceTrust::TrustOnce)
                        if client_id != startup_workspace_id =>
                    {
                        None
                    }
                    other => other,
                };
                let heycode_trust::TrustStartupState::Ready(_) =
                    client_trust.prepare_startup(heycode_trust::TrustFrontend::Acp, explicit)?
                else {
                    return Err(anyhow::anyhow!("ACP workspace trust dialog is unavailable"));
                };
                compose_world(&WorldOptions {
                    config: &cfg_ref,
                    trust: client_trust,
                    config_migration: acp_config_migration.as_ref(),
                    profile_layers: &acp_profile_layers,
                    sessions_dir: acp_sessions_root.clone(),
                    attachments_dir: acp_home.join("attachments"),
                    attachment_max_bytes: heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
                    session_source: heycode_session::SessionSource::Acp,
                    approval_prompter: heycode_cli::ApprovalPrompter::Proxied,
                    settings_user_path: acp_home.join("settings.toml"),
                    credentials_root: acp_home.clone(),
                    catalog_cache_path: acp_home.join("cache/models.json"),
                    settings_watch: true,
                    onboarding_required: false,
                    credential_validated_at_ms: None,
                    cwd,
                    fake: fake_provider.clone(),
                    resume: None,
                })
            };
            return acp::serve(Box::new(make_world))
                .await
                .map(|code| (RunCompletion::Exit(code), None));
        }

        let cwd = std::env::current_dir()?;
        let mut ctx = compose_world(&WorldOptions {
            config: &cfg,
            trust: workspace_trust.clone(),
            config_migration: config_migration.as_ref(),
            profile_layers: &profile_layers,
            sessions_dir: composed_sessions_root.clone(),
            attachments_dir: heycode_home.join("attachments"),
            attachment_max_bytes: heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
            session_source: if cli.mode_run || cli.prompt.is_some() {
                heycode_session::SessionSource::Headless
            } else {
                heycode_session::SessionSource::Interactive
            },
            approval_prompter: if cli.mode_run || cli.prompt.is_some() {
                heycode_cli::ApprovalPrompter::None
            } else {
                heycode_cli::ApprovalPrompter::Interactive
            },
            settings_user_path: heycode_home.join("settings.toml"),
            credentials_root: heycode_home.clone(),
            catalog_cache_path: heycode_home.join("cache/models.json"),
            settings_watch: true,
            onboarding_required,
            credential_validated_at_ms,
            cwd,
            fake,
            resume: composed_resume.clone(),
        })?;
        let agent = ctx
            .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
            .ok_or_else(|| anyhow::anyhow!("agent service missing after composition"))?;
        let agent_after = agent.clone();

        // Headless printing subscribes before any turn runs — and only
        // headless: inside the TUI a stdout printer writes over the screen
        // (and duplicates every line in flat mode).
        let headless = (cli.mode_run || cli.prompt.is_some()).then(|| {
            heycode_cli::headless::HeadlessOutput::new(
                cli.output_format.unwrap_or_default(),
                heycode_cli::headless::Sinks::process(),
            )
        });
        if let Some(headless) = headless.as_ref() {
            agent.ui().on::<heycode_agent::UiEvent>(headless.ui_listener());
            let session_bus = agent
                .session()
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .bus();
            session_bus.on::<heycode_session::SessionEvent>(headless.session_listener());
        }

        let outcome = async {
            if cli.mode_app_server {
                let server = ctx
                    .get::<heycode_app_server::AppServer>(heycode_app_server::SERVICE_APP_SERVER)
                    .ok_or_else(|| anyhow::anyhow!("app-server service missing"))?;
                let cancellation = tokio_util::sync::CancellationToken::new();
                let serving = heycode_app_server::serve_stdio_transport(
                    server,
                    tokio::io::stdin(),
                    tokio::io::stdout(),
                    cancellation.clone(),
                );
                tokio::pin!(serving);
                let result = tokio::select! {
                    result = &mut serving => result,
                    signal = tokio::signal::ctrl_c() => {
                        signal?;
                        cancellation.cancel();
                        serving.await
                    }
                };
                result?;
                return Ok(RunCompletion::Exit(0));
            }
            match (&cli.mode_run, &cli.prompt) {
                (true, Some(prompt)) | (false, Some(prompt)) => {
                    let mut attachments = Vec::with_capacity(cli.attachments.len());
                    if !cli.attachments.is_empty() {
                        let store = ctx
                            .get::<heycode_attachments::AttachmentStore>(
                                heycode_attachments::SERVICE_ATTACHMENTS,
                            )
                            .ok_or_else(|| anyhow::anyhow!("attachment service missing"))?;
                        for requested in &cli.attachments {
                            let requested_path = match requested {
                                CliAttachment::Image(path) | CliAttachment::Document(path) => path,
                            };
                            let path = if requested_path.is_absolute() {
                                requested_path.clone()
                            } else {
                                agent.cwd().join(requested_path)
                            };
                            let admission = match requested {
                                CliAttachment::Image(_) => store.admit_image_path(
                                    &path,
                                    tokio_util::sync::CancellationToken::new(),
                                )?,
                                CliAttachment::Document(_) => store.admit_document_path(
                                    &path,
                                    tokio_util::sync::CancellationToken::new(),
                                )?,
                            };
                            attachments.push(admission.metadata().clone());
                        }
                    }
                    // `echo SPEC | heycode run summarise`: the piped text is part
                    // of the request, not something to discard.
                    let appendix =
                        heycode_cli::headless::stdin_appendix(std::time::Duration::from_millis(500));
                    if let heycode_cli::headless::StdinAppendix::TimedOut(wait) = &appendix {
                        eprintln!(
                            "heycode: stdin is not a terminal but produced no data within {}ms; using the prompt as typed",
                            wait.as_millis()
                        );
                    }
                    let prompt =
                        heycode_cli::headless::prompt_with_appendix(prompt, appendix.text());
                    let report = agent.send_with_attachments(&prompt, attachments).await?;
                    if report.reason != "aborted" && report.reason != "error" {
                        agent.wait_for_background(tokio_util::sync::CancellationToken::new()).await?;
                    }
                    let session_id = agent
                        .session()
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .id()
                        .clone();
                    let code = match headless.as_ref() {
                        Some(headless) => headless.finish(&session_id, &report)?,
                        None => heycode_cli::headless::exit_code(&report),
                    };
                    Ok(RunCompletion::Exit(code))
                }
                (false, None) => {
                    let handle = ctx
                        .get::<heycode_tui::TuiHandle>(heycode_tui::SERVICE_TUI)
                        .ok_or_else(|| anyhow::anyhow!("tui service missing"))?;
                    let commands = ctx
                        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
                        .ok_or_else(|| anyhow::anyhow!("commands service missing"))?;
                    let settings = ctx
                        .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
                        .ok_or_else(|| anyhow::anyhow!("settings service missing"))?;
                    let mcp_management = ctx.get::<heycode_mcp::management::McpManagement>(
                        heycode_mcp::SERVICE_MCP_MANAGEMENT,
                    );
                    let plugin_lifecycle = ctx.get::<heycode_extensions::lifecycle::PluginLifecycle>(
                        heycode_extensions::lifecycle::SERVICE_PLUGIN_LIFECYCLE,
                    );
                    let plugin_packages = plugin_lifecycle
                        .as_ref()
                        .map(|_| heycode_cli::plugin_cli::package_index(&heycode_home.join("plugins")))
                        .transpose()?
                        .map(Arc::new);
                    let approvals = ctx.get::<heycode_agent::InteractiveApproval>(
                        heycode_agent::SERVICE_APPROVAL_INTERACTIVE,
                    );
                    let onboarding = ctx.get::<heycode_onboarding::OnboardingService>(
                        heycode_onboarding::SERVICE_ONBOARDING,
                    );
                    let secret_prompt = ctx
                        .get::<heycode_authorization_api_key::InteractiveSecretPrompt>(
                            heycode_authorization_api_key::SERVICE_SECRET_PROMPT,
                        );
                    let authorization = ctx.get::<heycode_authorization::AuthorizationService>(
                        heycode_authorization::SERVICE_AUTHORIZATION,
                    );
                    let doctor =
                        ctx.get::<heycode_doctor::DoctorRegistry>(heycode_doctor::SERVICE_DOCTOR);
                    let models = ctx.get::<heycode_llm::CatalogRegistry>(heycode_llm::SERVICE_MODELS);
                    let runtimes = ctx
                        .get::<heycode_runtime::AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
                        .ok_or_else(|| anyhow::anyhow!("runtime registry missing"))?;
                    let routing = ctx
                        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
                        .ok_or_else(|| anyhow::anyhow!("routing service missing"))?;
                    let cwd = std::env::current_dir()?;
                    TUI_SHOWN.store(true, std::sync::atomic::Ordering::SeqCst);
                    let session_on_screen = agent.clone();
                    let startup_notice =
                        STARTUP_NOTICE.lock().ok().and_then(|mut slot| slot.take());
                    if cli.resume_picker {
                        handle.session_commands().request(heycode_tui::session_browser::SessionCommandRequest::Browse);
                    }
                    let tui_outcome = handle
                        .run_with_display(
                            heycode_tui::app::LoopDeps {
                                agent,
                                app_server: ctx
                                    .get::<heycode_app_server::AppServer>(
                                        heycode_app_server::SERVICE_APP_SERVER,
                                    )
                                    .ok_or_else(|| anyhow::anyhow!("app-server service missing"))?,
                                questions: ctx
                                    .get::<heycode_agent::InteractiveQuestion>(
                                        heycode_agent::SERVICE_QUESTIONS,
                                    )
                                    .ok_or_else(|| anyhow::anyhow!("question service missing"))?,
                                commands,
                                settings,
                                settings_ui: ctx
                                    .get::<heycode_ui::settings_ui::SettingsUiRegistry>(
                                        heycode_ui::SERVICE_SETTINGS_UI,
                                    )
                                    .ok_or_else(|| {
                                        anyhow::anyhow!("settings UI registry missing")
                                    })?,
                                mcp_management,
                                mcp_runtime_control: ctx.get::<heycode_mcp::McpRuntimeControl>(heycode_mcp::SERVICE_MCP_RUNTIME_CONTROL),
                                mcp_registry: ctx
                                    .get::<heycode_mcp::McpRegistry>(heycode_mcp::SERVICE_MCP),
                                plugin_lifecycle,
                                plugin_packages,
                                attachments: ctx.get::<heycode_attachments::AttachmentStore>(heycode_attachments::SERVICE_ATTACHMENTS),
                                memory_sources: ctx
                                    .get::<heycode_tui::memory_commands::MemorySourceManagerHandle>(
                                        heycode_tui::memory_commands::SERVICE_MEMORY_SOURCES,
                                    )
                                    .map(|handle| handle.0.clone()),
                                skills: ctx
                                    .get::<heycode_skills::SkillSet>(heycode_skills::SERVICE_SKILLS),
                                subagents: ctx.get::<heycode_agent::SubagentRegistry>(
                                    heycode_agent::SERVICE_SUBAGENTS,
                                ),
                                hooks: ctx
                                    .get::<heycode_hooks::HookService>(heycode_hooks::SERVICE_HOOKS),
                                jobs: ctx
                                    .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
                                    .map(|jobs| (*jobs).clone()),
                                subprocess: ctx
                                    .get::<heycode_exec::SubprocessService>(
                                        heycode_exec::SERVICE_SUBPROCESS,
                                    )
                                    .map(|subprocess| (*subprocess).clone())
                                    .ok_or_else(|| {
                                        anyhow::anyhow!("subprocess service missing")
                                    })?,
                                cwd,
                                workspace_trust: trust_dialog,
                                approvals,
                                context_window: cfg.compaction.context_window,
                                context_warn_ratio: (cfg.compaction.threshold_ratio - 0.1)
                                    .clamp(0.1, 1.0),
                                onboarding,
                                secret_prompt,
                                authorization,
                                doctor,
                                models,
                                catalog_overrides: ctx.get::<heycode_catalog_file::CatalogOverrides>(
                                    heycode_catalog_file::SERVICE_CATALOG_OVERRIDES,
                                ),
                                runtimes,
                                routing,
                                profiles: ctx
                                    .get::<heycode_config::NamedProfileService>(
                                        heycode_config::SERVICE_PROFILES,
                                    )
                                    .ok_or_else(|| {
                                        anyhow::anyhow!("named profile service missing")
                                    })?,
                                current_profile: cli.profile.clone(),
                                startup_notice,
                                startup_warnings: startup_warnings.clone(),
                                prompt_history_path: Some(heycode_home.join("history")),
                            },
                            tui_display_mode(&cli),
                        )
                        .await?;
                    match tui_outcome {
                        heycode_tui::app::TuiRunOutcome::Exit => Ok(RunCompletion::Exit(0)),
                        heycode_tui::app::TuiRunOutcome::RecomposeCurrent { session_id, action } => {
                            Ok(RunCompletion::RecomposeCurrent { session_id, action })
                        }
                        heycode_tui::app::TuiRunOutcome::RecomposeLoggedOut { cleanup_warning } => {
                            if let Some(warning) = cleanup_warning {
                                let mut notice = STARTUP_NOTICE.lock().map_err(|_| {
                                    anyhow::anyhow!("logout notice is unavailable")
                                })?;
                                *notice = Some(heycode_tui::app::StartupNotice::Error(warning));
                            }
                            Ok(RunCompletion::RecomposeConnectionSelection)
                        }
                        heycode_tui::app::TuiRunOutcome::RecomposeConnectionSelection => {
                            Ok(RunCompletion::RecomposeConnectionSelection)
                        }
                        heycode_tui::app::TuiRunOutcome::RecomposeConnection => {
                            Ok(RunCompletion::RecomposeConnection)
                        }
                        heycode_tui::app::TuiRunOutcome::RecomposeProfile { name } => {
                            let current_session_id = session_on_screen
                                .session()
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .id()
                                .clone();
                            Ok(RunCompletion::RecomposeProfile {
                                name,
                                current_session_id,
                            })
                        }
                        heycode_tui::app::TuiRunOutcome::RecomposeSession(recompose) => {
                            let session_id = match recompose {
                                heycode_tui::SessionRecompose::Created { session_id, command } => {
                                    if let Some(command) = command
                                        && let Ok(mut notice) = STARTUP_NOTICE.lock() {
                                        *notice = Some(heycode_tui::app::StartupNotice::Command(command));
                                    }
                                    session_id
                                }
                                heycode_tui::SessionRecompose::Resume { session_id } => session_id,
                                heycode_tui::SessionRecompose::Forked { session_id, parent_session_id, title, parent_title, command } => {
                                    if let Ok(mut notice) = STARTUP_NOTICE.lock() {
                                        *notice = Some(heycode_tui::app::StartupNotice::CommandResult {
                                            command,
                                            message: heycode_tui::branch_receipt(&session_id, &parent_session_id, title.as_deref(), parent_title.as_deref()),
                                        });
                                    }
                                    session_id
                                }
                            };
                            let current_session_id = session_on_screen
                                .session()
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .id()
                                .clone();
                            Ok(RunCompletion::RecomposeSession {
                                session_id,
                                current_session_id,
                            })
                        }
                        heycode_tui::app::TuiRunOutcome::RecomposeWorkspaceTrust {
                            decision,
                            persistence,
                            ..
                        } => Ok(RunCompletion::RecomposeWorkspaceTrust {
                            decision,
                            persistence,
                        }),
                    }
                }
                (true, None) => Err(anyhow::anyhow!("`run` needs a prompt\n{}", usage())),
            }
        }
        .await;
        // A fresh interactive session that never saw a turn is not worth a
        // row in the picker. Decide here, while the agent is still alive;
        // remove after shutdown, when the log is closed.
        let unused_session = (composed_resume.is_none()
            && !matches!(&outcome, Ok(RunCompletion::RecomposeCurrent { .. }))
            && pretrust_sessions_active.is_none()
            && !cli.mode_run
            && cli.prompt.is_none()
            && !cli.mode_app_server)
            .then(|| {
                let session = agent_after
                    .session()
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                heycode_session::is_unused_session(session.events()).then(|| session.id().clone())
            })
            .flatten();
        // AGENTS.md §1.2: registrations are effects; shutdown unwinds the
        // disposers LIFO. Exiting via `process::exit` without this leaves MCP
        // children and temp dirs behind.
        ctx.shutdown();
        let discarded = unused_session.filter(|id| {
            heycode_session::discard_unused_session(&composed_sessions_root, id).unwrap_or(false)
        });
        outcome.map(|completion| (completion, discarded))
    })?;
    let (completion, discarded_session) = completion;
    match completion {
        RunCompletion::Exit(code) => Ok(code),
        RunCompletion::RecomposeCurrent { session_id, action } => {
            let previous = session_restart_args(&args, &restart_sessions_root, &session_id);
            let restarted = recomposition_restart_args(&previous, action);
            drop(runtime);
            match action {
                heycode_tui::recomposition::RecompositionAction::Display(_) => {
                    run_or_fall_back(restarted, previous, "changing terminal presentation")
                }
                heycode_tui::recomposition::RecompositionAction::Sandbox(mode) => {
                    if let Ok(mut notice) = STARTUP_NOTICE.lock() {
                        *notice = Some(heycode_tui::app::StartupNotice::CommandResult {
                            command: "/sandbox".to_owned(),
                            message: format!(
                                "Sandbox mode set to: {}",
                                heycode_tui::permission_picker::sandbox_config_value(mode)
                            ),
                        });
                    }
                    run_or_fall_back(restarted, previous, "changing sandbox mode")
                }
                heycode_tui::recomposition::RecompositionAction::ReloadPlugins => {
                    // Reload reads the same durable configuration again. Retrying
                    // that identical failed configuration cannot restore the old
                    // in-memory plugin generation; retain the conversation and
                    // report the real activation error instead.
                    run(restarted).map_err(|error| {
                        if error.downcast_ref::<PluginReloadFailure>().is_some() {
                            error
                        } else {
                            PluginReloadFailure {
                                session_id: session_id.to_string(),
                                source: error,
                            }
                            .into()
                        }
                    })
                }
            }
        }
        RunCompletion::RecomposeConnectionSelection => {
            let restarted = selected_connection_restart_args(&args);
            drop(runtime);
            run(restarted)
        }
        RunCompletion::RecomposeConnection => {
            let restarted = connection_restart_args(&args);
            drop(runtime);
            run(restarted)
        }
        RunCompletion::RecomposeProfile {
            name,
            current_session_id,
        } => {
            // An empty session was just discarded: the new world starts fresh
            // instead of resuming a log that no longer exists.
            let previous = if discarded_session.as_ref() == Some(&current_session_id) {
                args.clone()
            } else {
                session_restart_args(&args, &restart_sessions_root, &current_session_id)
            };
            let restarted = profile_restart_args(&previous, name.as_deref());
            drop(runtime);
            let what = name.as_deref().map_or_else(
                || "switching to the built-in profile".to_owned(),
                |name| format!("switching to profile `{name}`"),
            );
            run_or_fall_back(restarted, previous, &what)
        }
        RunCompletion::RecomposeSession {
            session_id,
            current_session_id,
        } => {
            let restarted = session_restart_args(&args, &restart_sessions_root, &session_id);
            let previous = session_restart_args(&args, &restart_sessions_root, &current_session_id);
            drop(runtime);
            run_or_fall_back(restarted, previous, "switching session")
        }
        RunCompletion::RecomposeWorkspaceTrust {
            decision,
            persistence,
        } => {
            let flag = match decision {
                heycode_trust::WorkspaceTrustDecision::Trusted => "--trust-workspace",
                heycode_trust::WorkspaceTrustDecision::Restricted => "--restricted-workspace",
                heycode_trust::WorkspaceTrustDecision::Unknown => {
                    return Err(anyhow::anyhow!(
                        "workspace trust recompose returned an unresolved decision"
                    ));
                }
            };
            let mut restarted = args;
            restarted.retain(|arg| {
                !matches!(arg.as_str(), "--trust-workspace" | "--restricted-workspace")
            });
            if persistence != heycode_trust::TrustPersistence::Persistent {
                restarted.insert(0, flag.to_owned());
            }
            drop(runtime);
            run(restarted)
        }
    }
}

/// Replace any `--profile` with `name`, or drop it for the built-in profile.
fn profile_restart_args(args: &[String], name: Option<&str>) -> Vec<String> {
    let mut restarted = Vec::with_capacity(args.len().saturating_add(2));
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--profile" {
            index = index.saturating_add(2);
            continue;
        }
        restarted.push(args[index].clone());
        index += 1;
    }
    if let Some(name) = name {
        restarted.insert(0, name.to_owned());
        restarted.insert(0, "--profile".to_owned());
    }
    restarted
}

fn recomposition_restart_args(
    session_args: &[String],
    action: heycode_tui::recomposition::RecompositionAction,
) -> Vec<String> {
    let mut restarted = session_args.to_vec();
    if let heycode_tui::recomposition::RecompositionAction::Display(display) = action {
        restarted.retain(|argument| argument != "--screen-reader");
        if display == heycode_tui::TuiDisplayMode::ScreenReader {
            restarted.push("--screen-reader".to_owned());
        }
    }
    if let heycode_tui::recomposition::RecompositionAction::Sandbox(mode) = action {
        restarted.push("--set".to_owned());
        restarted.push(format!(
            "sandbox.mode={}",
            heycode_tui::permission_picker::sandbox_config_value(mode)
        ));
    }
    restarted
}

fn session_restart_args(
    args: &[String],
    sessions_root: &std::path::Path,
    session_id: &heycode_core::SessionId,
) -> Vec<String> {
    let mut restarted = Vec::with_capacity(args.len().saturating_add(2));
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--resume" | "-r" => {
                index += 1;
                if args.get(index).is_some_and(|value| !value.starts_with('-')) {
                    index += 1;
                }
            }
            "-c" | "--continue" => {
                let legacy_path = args[index] == "-c";
                index += 1;
                if legacy_path
                    && args.get(index).is_some_and(|value| {
                        !value.starts_with('-') && std::path::Path::new(value).exists()
                    })
                {
                    index += 1;
                }
            }
            compact if compact.starts_with("-c") && compact.len() > 2 => {
                index += 1;
            }
            _ => {
                restarted.push(args[index].clone());
                index += 1;
            }
        }
    }
    restarted.insert(
        0,
        sessions_root
            .join(session_id.as_str())
            .join("session.jsonl")
            .to_string_lossy()
            .into_owned(),
    );
    restarted.insert(0, "--resume".to_owned());
    restarted
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_recomposition_preserves_previous_flags_and_last_override_wins() {
        use heycode_tui::recomposition::RecompositionAction;
        let previous = [
            "--resume",
            "/sessions/existing/session.jsonl",
            "--provider",
            "deepseek",
            "--model",
            "retained-model",
            "--protocol",
            "openai_chat",
            "--profile",
            "retained",
            "--screen-reader",
            "--restricted-workspace",
            "--fake",
            "--no-background",
            "--approval",
            "default",
            "--sandbox",
            "off",
            "--set",
            "sandbox.mode=workspace",
            "--set",
            "llm.max_output_tokens=321",
        ]
        .map(str::to_owned)
        .to_vec();
        let baseline = previous.clone();
        for (mode, expected) in [
            (heycode_exec::SandboxMode::ReadOnly, "readonly"),
            (heycode_exec::SandboxMode::WorkspaceWrite, "workspace"),
            (heycode_exec::SandboxMode::Off, "off"),
        ] {
            let restarted =
                recomposition_restart_args(&previous, RecompositionAction::Sandbox(mode));
            assert_eq!(&restarted[..previous.len()], &previous);
            assert_eq!(
                &restarted[previous.len()..],
                &["--set".to_owned(), format!("sandbox.mode={expected}")]
            );
            let parsed = parse_args(&restarted).unwrap();
            assert_eq!(parsed.provider.as_deref(), Some("deepseek"));
            assert_eq!(parsed.model.as_deref(), Some("retained-model"));
            assert_eq!(parsed.profile.as_deref(), Some("retained"));
            assert!(parsed.screen_reader);
            assert!(parsed.fake);
            assert_eq!(
                parsed.sets.last().unwrap(),
                &format!("sandbox.mode={expected}")
            );
            let mut applied = heycode_config::Config::default();
            for patch in parsed.sets {
                applied.apply_patch(&patch).unwrap();
            }
            let mut wanted = heycode_config::Config::default();
            wanted
                .apply_patch(&format!("sandbox.mode={expected}"))
                .unwrap();
            assert_eq!(applied.sandbox.mode, wanted.sandbox.mode);
        }
        assert_eq!(
            previous, baseline,
            "failed restart fallback must retain the exact prior invocation"
        );
    }

    #[test]
    fn selected_connection_clears_old_route_and_resume_but_keeps_workspace_policy() {
        let args: Vec<String> = [
            "--provider",
            "deepseek",
            "--model",
            "old",
            "--protocol",
            "openai_chat",
            "--set",
            "llm.base_url=https://old.example",
            "--set",
            "llm.model=old",
            "--resume",
            "/tmp/old-session.jsonl",
            "--restricted-workspace",
            "--config",
            "/tmp/config.toml",
            "--set",
            "sandbox.mode=readonly",
            "--screen-reader",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        assert_eq!(
            selected_connection_restart_args(&args),
            [
                "--restricted-workspace",
                "--config",
                "/tmp/config.toml",
                "--set",
                "sandbox.mode=readonly",
                "--screen-reader",
            ]
        );
    }

    #[test]
    fn connection_recompose_preserves_every_explicit_startup_choice() {
        let args = vec![
            "--screen-reader".to_owned(),
            "--restricted-workspace".to_owned(),
            "--profile".to_owned(),
            "minimal".to_owned(),
            "--provider".to_owned(),
            "deepseek".to_owned(),
        ];
        assert_eq!(connection_restart_args(&args), args);
    }

    #[test]
    fn profile_recompose_replaces_only_the_profile_and_preserves_every_other_argument() {
        let args = vec![
            "--screen-reader".to_owned(),
            "--restricted-workspace".to_owned(),
            "--profile".to_owned(),
            "old".to_owned(),
            "--provider".to_owned(),
            "deepseek".to_owned(),
            "--fake".to_owned(),
        ];
        assert_eq!(
            profile_restart_args(&args, Some("new")),
            [
                "--profile",
                "new",
                "--screen-reader",
                "--restricted-workspace",
                "--provider",
                "deepseek",
                "--fake",
            ]
            .map(str::to_owned)
        );
        assert_eq!(
            profile_restart_args(&args, None),
            [
                "--screen-reader",
                "--restricted-workspace",
                "--provider",
                "deepseek",
                "--fake",
            ]
            .map(str::to_owned),
            "the built-in row drops --profile entirely"
        );
        let root = std::path::Path::new("/sessions");
        let id = heycode_core::SessionId::from_raw("0123456789abcdef0123456789abcdef");
        let with_session =
            profile_restart_args(&session_restart_args(&args, root, &id), Some("new"));
        assert_eq!(
            &with_session[..4],
            &[
                "--profile",
                "new",
                "--resume",
                "/sessions/0123456789abcdef0123456789abcdef/session.jsonl"
            ]
            .map(str::to_owned)
        );
    }

    #[test]
    fn session_recompose_replaces_resume_selectors_and_preserves_other_arguments() {
        let args = vec![
            "--screen-reader".to_owned(),
            "--profile".to_owned(),
            "minimal".to_owned(),
            "--continue".to_owned(),
            "--provider".to_owned(),
            "deepseek".to_owned(),
            "--resume".to_owned(),
            "/old/session.jsonl".to_owned(),
            "-c/older/session.jsonl".to_owned(),
            "--fake".to_owned(),
        ];
        let root = std::path::Path::new("/state/sessions");
        let id = heycode_core::SessionId::from_raw("session-01");
        assert_eq!(
            session_restart_args(&args, root, &id),
            [
                "--resume",
                "/state/sessions/session-01/session.jsonl",
                "--screen-reader",
                "--profile",
                "minimal",
                "--provider",
                "deepseek",
                "--fake",
            ]
            .map(str::to_owned)
        );
    }

    #[test]
    fn setup_writer_emits_current_schema_and_never_freezes_the_builtin_profile() {
        let mut config = Config::defaults();
        config.profile.plugins = vec!["historical-snapshot".to_owned()];
        config.llm.api_key_env = Some("DEEPSEEK_API_KEY".to_owned());
        // The wizard always asks and records the answer; an unset mode is
        // left unset so the surface default applies.
        config.approval.mode = Some(heycode_config::ApprovalMode::FullAccess);

        let rendered = toml_render(&config).unwrap();

        assert!(
            rendered.starts_with(&format!("schema_version = {CONFIG_SCHEMA_VERSION}\n")),
            "{rendered}"
        );
        assert!(!rendered.contains("[profile]"), "{rendered}");
        assert!(!rendered.contains("historical-snapshot"), "{rendered}");
        assert!(rendered.contains("api_key_env = \"DEEPSEEK_API_KEY\""));
        assert!(rendered.contains("[approval]\nmode = \"full_access\""));
        assert!(rendered.contains("model = \"deepseek-v4-flash\""));
    }

    #[test]
    fn setup_writer_escapes_provider_model_and_reference_as_toml_values() {
        let mut config = Config::defaults();
        config.llm.provider = "provider-with-\"quote".to_owned();
        config.llm.model = "line-one\nline-two".to_owned();
        config.llm.api_key_env = Some("KEY_WITH_\"QUOTE".to_owned());

        let rendered = toml_render(&config).unwrap();
        let parsed: Config = toml::from_str(&rendered).unwrap();

        assert_eq!(parsed.llm.provider, config.llm.provider);
        assert_eq!(parsed.llm.model, config.llm.model);
        assert_eq!(parsed.llm.api_key_env, config.llm.api_key_env);
        assert!(!rendered.contains("[profile]"));
    }

    #[test]
    fn setup_writer_is_atomic_owner_only_and_refuses_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("home/config.toml");
        let bytes = format!("schema_version = {CONFIG_SCHEMA_VERSION}\n");
        write_setup_config(&path, bytes.as_bytes()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes.as_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
            assert_eq!(
                std::fs::metadata(path.parent().unwrap()).unwrap().mode() & 0o777,
                0o700
            );
            let target = dir.path().join("target.toml");
            std::fs::write(&target, "untouched").unwrap();
            let link = dir.path().join("link.toml");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(write_setup_config(&link, b"changed").is_err());
            assert_eq!(std::fs::read_to_string(target).unwrap(), "untouched");
        }
    }

    /// `--help` documents `-c <path>`. A token after `-c` that names an
    /// existing session log or directory is that path, not a prompt; anything
    /// else keeps `-c` meaning "resume the latest".
    #[test]
    fn continue_with_an_existing_path_resumes_that_path_not_a_prompt() {
        let home = tempfile::tempdir().unwrap();
        let log = home.path().join("session.jsonl");
        std::fs::write(&log, "").unwrap();
        let path = log.to_string_lossy().into_owned();
        let parsed = parse_args(&[
            "-c".to_owned(),
            path.clone(),
            "run".to_owned(),
            "hi".to_owned(),
        ])
        .unwrap();
        assert_eq!(parsed.resume.as_deref(), Some(log.as_path()));
        assert!(!parsed.resume_latest);
        assert_eq!(parsed.prompt.as_deref(), Some("hi"));

        let latest = parse_args(&["-c".to_owned(), "run".to_owned(), "hi".to_owned()]).unwrap();
        assert!(latest.resume_latest);
        assert!(latest.resume.is_none());
        assert_eq!(latest.prompt.as_deref(), Some("hi"));

        let latest_bare = parse_args(&["--continue".to_owned()]).unwrap();
        assert!(latest_bare.resume_latest);
    }

    #[test]
    fn resume_picker_is_optional_and_preserves_following_flags() {
        let args = |values: &[&str]| {
            values
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>()
        };
        let picker = parse_args(&args(&["--resume", "--fake", "--restricted-workspace"])).unwrap();
        assert!(picker.resume_picker);
        assert!(picker.resume.is_none());
        let short = parse_args(&args(&["-r"])).unwrap();
        assert!(short.resume_picker);
        let exact = parse_args(&args(&["--resume", "saved/session.jsonl"])).unwrap();
        assert_eq!(
            exact.resume.as_deref(),
            Some(std::path::Path::new("saved/session.jsonl"))
        );
        assert!(!exact.resume_picker);
        assert!(parse_args(&args(&["--resume", "--continue"])).is_err());
        assert!(parse_args(&args(&["run", "--resume"])).is_err());
        let restarted = session_restart_args(
            &args(&["--resume", "--fake", "--restricted-workspace"]),
            std::path::Path::new("/sessions"),
            &heycode_core::SessionId::from_raw("test"),
        );
        assert!(restarted.contains(&"--fake".to_owned()));
        assert!(restarted.contains(&"--restricted-workspace".to_owned()));
    }

    #[test]
    fn profile_flag_requires_and_captures_one_safe_name() {
        let parsed = parse_args(&[
            "--profile".to_owned(),
            "minimal".to_owned(),
            "--fake".to_owned(),
        ])
        .unwrap();
        assert_eq!(parsed.profile.as_deref(), Some("minimal"));
        assert!(parse_args(&["--profile".to_owned()]).is_err());
        assert!(
            parse_args(&[
                "--profile".to_owned(),
                "one".to_owned(),
                "--profile".to_owned(),
                "two".to_owned(),
            ])
            .is_err()
        );
    }

    #[test]
    fn protocol_and_output_default_flags_reuse_schema_validation_inputs() {
        let parsed = parse_args(&[
            "--provider".to_owned(),
            "bedrock-mantle".to_owned(),
            "--protocol".to_owned(),
            "anthropic_messages".to_owned(),
            "--max-output-tokens".to_owned(),
            "4096".to_owned(),
            "--fake".to_owned(),
        ])
        .unwrap();
        assert_eq!(parsed.protocol.as_deref(), Some("anthropic_messages"));
        assert_eq!(parsed.max_output_tokens.as_deref(), Some("4096"));
        assert!(parse_args(&["--protocol".to_owned()]).is_err());
        assert!(parse_args(&["--max-output-tokens".to_owned()]).is_err());
    }

    #[test]
    fn media_flags_preserve_order_and_are_restricted_to_one_shot_prompts() {
        let parsed = parse_args(&[
            "run".to_owned(),
            "--image".to_owned(),
            "first.png".to_owned(),
            "--document".to_owned(),
            "guide.pdf".to_owned(),
            "--image".to_owned(),
            "nested/second.webp".to_owned(),
            "describe both".to_owned(),
        ])
        .unwrap();
        assert_eq!(
            parsed.attachments,
            [
                CliAttachment::Image(std::path::PathBuf::from("first.png")),
                CliAttachment::Document(std::path::PathBuf::from("guide.pdf")),
                CliAttachment::Image(std::path::PathBuf::from("nested/second.webp")),
            ]
        );
        assert_eq!(parsed.prompt.as_deref(), Some("describe both"));
        assert!(parse_args(&["--image".to_owned(), "image.png".to_owned()]).is_err());
        assert!(
            parse_args(&[
                "doctor".to_owned(),
                "--image".to_owned(),
                "image.png".to_owned(),
                "prompt".to_owned(),
            ])
            .is_err()
        );
    }

    #[test]
    fn workspace_trust_flags_are_explicit_session_only_and_mutually_exclusive() {
        let trusted = parse_args(&["--trust-workspace".to_owned()]).unwrap();
        assert_eq!(
            trusted.workspace_trust,
            Some(heycode_trust::ExplicitWorkspaceTrust::TrustOnce)
        );
        let restricted = parse_args(&["--restricted-workspace".to_owned()]).unwrap();
        assert_eq!(
            restricted.workspace_trust,
            Some(heycode_trust::ExplicitWorkspaceTrust::RestrictedOnce)
        );
        let error = match parse_args(&[
            "--trust-workspace".to_owned(),
            "--restricted-workspace".to_owned(),
        ]) {
            Ok(_) => panic!("conflicting workspace trust flags must fail"),
            Err(error) => error,
        };
        assert!(error.contains("only once"), "{error}");
    }

    #[test]
    fn screen_reader_flag_selects_only_the_interactive_tui() {
        let parsed = parse_args(&["--screen-reader".to_owned(), "--fake".to_owned()]).unwrap();
        assert!(parsed.screen_reader);
        assert_eq!(
            tui_display_mode(&parsed),
            heycode_tui::TuiDisplayMode::ScreenReader
        );

        for args in [
            vec![
                "run".to_owned(),
                "--screen-reader".to_owned(),
                "hello".to_owned(),
            ],
            vec!["setup".to_owned(), "--screen-reader".to_owned()],
            vec!["acp".to_owned(), "--screen-reader".to_owned()],
            vec!["doctor".to_owned(), "--screen-reader".to_owned()],
            vec![
                "--screen-reader".to_owned(),
                "mcp".to_owned(),
                "list".to_owned(),
            ],
            vec![
                "--screen-reader".to_owned(),
                "plugin".to_owned(),
                "list".to_owned(),
            ],
            vec![
                "--screen-reader".to_owned(),
                "release".to_owned(),
                "rollback".to_owned(),
            ],
            vec![
                "--screen-reader".to_owned(),
                "app-server".to_owned(),
                "--stdio-v1".to_owned(),
                "--workspace".to_owned(),
                std::env::current_dir().unwrap().display().to_string(),
            ],
        ] {
            assert!(parse_args(&args).is_err(), "accepted {args:?}");
        }
    }

    #[test]
    fn app_server_mode_preserves_its_closed_arguments_and_global_choices() {
        let workspace = std::env::current_dir().unwrap();
        let parsed = parse_args(&[
            "--restricted-workspace".to_owned(),
            "--fake".to_owned(),
            "app-server".to_owned(),
            "--stdio-v1".to_owned(),
            "--workspace".to_owned(),
            workspace.display().to_string(),
            "--resume".to_owned(),
            "0199c3d0-1c3f-7a11-9f31-52d4b1f0aa10".to_owned(),
        ])
        .unwrap();
        assert!(parsed.mode_app_server && parsed.fake);
        assert_eq!(
            parsed.workspace_trust,
            Some(heycode_trust::ExplicitWorkspaceTrust::RestrictedOnce)
        );
        let command = parse_app_server_command(&parsed.app_server_args).unwrap();
        assert_eq!(command.workspace, workspace);
        assert_eq!(
            command.resume.unwrap().as_str(),
            "0199c3d0-1c3f-7a11-9f31-52d4b1f0aa10"
        );
    }

    #[test]
    fn removed_remote_commands_fail_before_prompt_or_provider_admission() {
        for operation in [
            "start", "resume", "list", "status", "send", "stop", "token", "api",
        ] {
            let args = vec!["remote".to_owned(), operation.to_owned()];
            let error = match parse_args(&args) {
                Err(error) => error,
                Ok(_) => panic!("removed remote command was accepted: {operation}"),
            };
            assert!(error.contains("HTTP remote feature has been removed"));
            assert!(
                run(args)
                    .unwrap_err()
                    .to_string()
                    .contains("HTTP remote feature has been removed")
            );
        }
        for prefix in [vec!["--fake"], vec!["--provider", "openai"]] {
            let args: Vec<String> = prefix
                .into_iter()
                .chain(["remote", "start"])
                .map(str::to_owned)
                .collect();
            assert!(
                run(args)
                    .unwrap_err()
                    .to_string()
                    .contains("HTTP remote feature has been removed")
            );
        }
        // Explicit run prompts remain ordinary user text.
        assert_eq!(
            parse_args(&["run".to_owned(), "remote".to_owned()])
                .unwrap()
                .prompt
                .as_deref(),
            Some("remote")
        );
    }

    #[test]
    fn app_server_rejects_removed_http_flags_even_with_stdio_transport() {
        let workspace = std::env::current_dir().unwrap().display().to_string();
        for flag in ["--http-state", "--http-port"] {
            for stdio in [false, true] {
                let mut args = vec!["--workspace".to_owned(), workspace.clone()];
                if stdio {
                    args.push("--stdio-v1".to_owned());
                }
                args.extend([flag.to_owned(), "8765".to_owned()]);
                let error = parse_app_server_command(&args).unwrap_err();
                assert!(error.contains(&format!("unknown app-server argument `{flag}`")));
            }
        }
        assert!(!usage().contains("heycode remote"));
        assert!(!usage().contains("--http-"));
    }

    #[test]
    fn app_server_mode_rejects_ambiguous_transport_workspace_and_resume_values() {
        let workspace = std::env::current_dir().unwrap().display().to_string();
        assert!(parse_app_server_command(&["--workspace".to_owned(), workspace.clone(),]).is_err());
        assert!(parse_app_server_command(&["--stdio-v1".to_owned()]).is_err());
        assert!(
            parse_app_server_command(&[
                "--stdio-v1".to_owned(),
                "--workspace".to_owned(),
                "relative".to_owned(),
            ])
            .is_err()
        );
        assert!(
            parse_app_server_command(&[
                "--stdio-v1".to_owned(),
                "--workspace".to_owned(),
                workspace,
                "--resume".to_owned(),
                "../foreign".to_owned(),
            ])
            .is_err()
        );
        assert!(
            parse_args(&[
                "run".to_owned(),
                "app-server".to_owned(),
                "--stdio-v1".to_owned(),
            ])
            .is_err()
        );
    }

    #[test]
    fn release_mode_preserves_all_subcommand_arguments_for_its_plugin_surface() {
        let parsed = parse_args(&[
            "release".to_owned(),
            "apply".to_owned(),
            "--root".to_owned(),
            "/install".to_owned(),
            "--channel".to_owned(),
            "stable".to_owned(),
        ])
        .unwrap();
        assert!(parsed.mode_release);
        assert_eq!(
            parsed.release_args,
            ["apply", "--root", "/install", "--channel", "stable"]
        );
    }

    #[test]
    fn config_show_keeps_flags_and_refuses_prompt_shaped_arguments() {
        let parsed = parse_args(&[
            "--model".to_owned(),
            "x".to_owned(),
            "config".to_owned(),
            "show".to_owned(),
        ])
        .unwrap();
        assert!(parsed.mode_config);
        assert_eq!(parsed.config_operation.as_deref(), Some("show"));
        assert_eq!(parsed.model.as_deref(), Some("x"));
        assert!(
            parsed.prompt.is_none(),
            "`show` is an operation, not a prompt"
        );
        let bare = parse_args(&["config".to_owned()]).unwrap();
        assert!(bare.mode_config && bare.config_operation.is_none());
        assert!(parse_args(&["config".to_owned(), "show".to_owned(), "-c".to_owned()]).is_err());
        assert!(
            parse_args(&["hello".to_owned(), "config".to_owned()]).is_err(),
            "after a prompt, `config` is not a subcommand but a second positional"
        );
    }

    #[test]
    fn doctor_flags_are_closed_and_mode_specific() {
        let parsed = parse_args(&[
            "doctor".to_owned(),
            "--composition".to_owned(),
            "--json".to_owned(),
        ])
        .unwrap();
        assert!(parsed.mode_doctor && parsed.doctor_composition && parsed.json);
        let unified = parse_args(&["doctor".to_owned()]).unwrap();
        assert!(unified.mode_doctor && !unified.doctor_composition && !unified.json);
        assert!(parse_args(&["run".to_owned(), "--json".to_owned()]).is_err());
        assert!(
            parse_args(&[
                "doctor".to_owned(),
                "--composition".to_owned(),
                "prompt".to_owned(),
            ])
            .is_err()
        );
    }
}
