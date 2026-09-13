//! MCP10 CLI parity: every operation reachable, and no operation reachable by
//! accident.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use heycode_cli::mcp_cli::{self, McpCommand};
use heycode_mcp::McpTransportKind;
use heycode_mcp::management::{
    McpDefinitionStore, McpHealth, McpManagement, McpManagementError, McpOperation, McpProbe,
    StoredServer,
};

#[derive(Default)]
struct MemoryStore(Mutex<BTreeMap<String, StoredServer>>);

impl McpDefinitionStore for MemoryStore {
    fn load(&self) -> Result<BTreeMap<String, StoredServer>, McpManagementError> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn persist(&self, servers: &BTreeMap<String, StoredServer>) -> Result<(), McpManagementError> {
        *self.0.lock().unwrap() = servers.clone();
        Ok(())
    }
}

struct Fixed(McpHealth);
impl McpProbe for Fixed {
    fn probe(&self, _server: &StoredServer) -> McpHealth {
        self.0
    }
}

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

fn management() -> McpManagement {
    McpManagement::new(Arc::new(MemoryStore::default()) as Arc<dyn McpDefinitionStore>)
}

/// THE parity test. Every operation the shared layer defines must be reachable
/// from the CLI, and must dispatch to itself. If someone adds an operation and
/// forgets the CLI, this fails — that is the whole point of the row's word
/// "parity".
#[test]
fn every_shared_operation_is_reachable_from_the_command_line() {
    for operation in McpOperation::ALL {
        let argv = match operation {
            McpOperation::Add => args(&["add", "srv", "--command", "/bin/x"]),
            McpOperation::List => args(&["list"]),
            McpOperation::Auth => args(&["auth", "srv"]),
            McpOperation::Test => args(&["test", "srv"]),
            McpOperation::Edit => args(&["edit", "srv", "--url", "https://h.test/mcp"]),
            McpOperation::Enable => args(&["enable", "srv"]),
            McpOperation::Remove => args(&["remove", "srv"]),
        };
        let parsed = mcp_cli::parse(&argv)
            .unwrap_or_else(|error| panic!("`{operation}` is not reachable: {error}"));
        assert_eq!(
            parsed.operation(),
            operation,
            "`{operation}` dispatched to the wrong operation"
        );
        assert!(
            mcp_cli::usage().contains(operation.as_str()),
            "`{operation}` is missing from usage text"
        );
    }
}

#[test]
fn a_server_can_be_added_listed_disabled_edited_and_removed_end_to_end() {
    let management = management();
    let run = |argv: &[&str]| {
        let command = mcp_cli::parse(&args(argv)).expect("parses");
        mcp_cli::run(&management, &command)
    };

    assert_eq!(
        run(&["add", "files", "--command", "/usr/bin/files"]).unwrap(),
        "added `files`"
    );
    let listed = run(&["list"]).unwrap();
    assert!(listed.contains("files"), "{listed}");
    assert!(listed.contains("stdio"), "{listed}");
    assert!(listed.contains("enabled"), "{listed}");
    assert!(
        listed.contains("unknown"),
        "health must read unknown with no probe: {listed}"
    );

    assert_eq!(
        run(&["enable", "files", "--off"]).unwrap(),
        "disabled `files`"
    );
    assert!(run(&["list"]).unwrap().contains("disabled"));

    assert_eq!(
        run(&["edit", "files", "--url", "https://files.test/mcp"]).unwrap(),
        "updated `files`"
    );
    let listed = run(&["list"]).unwrap();
    assert!(listed.contains("streamable-http"), "{listed}");
    assert!(
        listed.contains("disabled"),
        "an edit must not re-enable: {listed}"
    );

    assert_eq!(run(&["remove", "files"]).unwrap(), "removed `files`");
    assert_eq!(run(&["list"]).unwrap(), "no MCP servers configured");
}

/// A server has exactly one transport. Guessing which one the user meant is how
/// a server silently talks to the wrong endpoint, so both flags is an error.
#[test]
fn giving_both_command_and_url_is_refused_rather_than_resolved_by_precedence() {
    let error = mcp_cli::parse(&args(&[
        "add",
        "srv",
        "--command",
        "/bin/x",
        "--url",
        "https://h.test/mcp",
    ]))
    .unwrap_err();
    assert!(error.contains("exactly one"), "{error}");
}

#[test]
fn add_and_edit_require_a_transport_and_every_named_operation_requires_a_name() {
    for argv in [
        args(&["add", "srv"]),
        args(&["edit", "srv"]),
        args(&["add", "srv", "--command"]),
    ] {
        assert!(mcp_cli::parse(&argv).is_err(), "accepted {argv:?}");
    }
    for word in ["auth", "test", "remove", "enable", "edit", "add"] {
        let error = mcp_cli::parse(&args(&[word])).unwrap_err();
        assert!(
            error.contains("needs a server name"),
            "`{word}` without a name: {error}"
        );
    }
}

/// An unknown operation must show what is available rather than only say no.
#[test]
fn an_unknown_operation_lists_the_available_ones() {
    let error = mcp_cli::parse(&args(&["frobnicate"])).unwrap_err();
    assert!(
        error.contains("unknown mcp operation `frobnicate`"),
        "{error}"
    );
    for operation in McpOperation::ALL {
        assert!(error.contains(operation.as_str()), "{error}");
    }
    // No arguments at all is the same help, not a panic.
    assert!(
        mcp_cli::parse(&[])
            .unwrap_err()
            .contains("usage: heycode mcp")
    );
}

/// `needs-auth` and `unreachable` must read differently, because they send the
/// operator to different next steps.
#[test]
fn health_words_distinguish_needing_auth_from_being_down() {
    for (health, expected) in [
        (McpHealth::Reachable, "reachable"),
        (McpHealth::Unreachable, "unreachable"),
        (McpHealth::AuthorizationRequired, "needs-auth"),
        (McpHealth::Unknown, "unknown"),
    ] {
        let management =
            McpManagement::new(Arc::new(MemoryStore::default()) as Arc<dyn McpDefinitionStore>)
                .with_probe(Arc::new(Fixed(health)));
        let add = mcp_cli::parse(&args(&["add", "srv", "--command", "/bin/x"])).unwrap();
        mcp_cli::run(&management, &add).unwrap();

        let test = mcp_cli::parse(&args(&["test", "srv"])).unwrap();
        assert_eq!(
            mcp_cli::run(&management, &test).unwrap(),
            format!("srv: {expected}")
        );
    }
}

#[test]
fn a_management_failure_is_reported_to_the_operator_verbatim() {
    let management = management();
    let add = mcp_cli::parse(&args(&["add", "srv", "--command", "/bin/x"])).unwrap();
    mcp_cli::run(&management, &add).unwrap();

    let error = mcp_cli::run(&management, &add).unwrap_err();
    assert!(error.contains("already exists"), "{error}");

    let ghost = mcp_cli::parse(&args(&["remove", "ghost"])).unwrap();
    assert!(
        mcp_cli::run(&management, &ghost)
            .unwrap_err()
            .contains("no MCP server named `ghost`")
    );
}

/// `enable` defaults to on; only `--off` turns a server off.
#[test]
fn enable_defaults_to_on_and_off_is_explicit() {
    assert_eq!(
        mcp_cli::parse(&args(&["enable", "srv"])).unwrap(),
        McpCommand::Enable {
            name: "srv".to_owned(),
            on: true
        }
    );
    assert_eq!(
        mcp_cli::parse(&args(&["enable", "srv", "--off"])).unwrap(),
        McpCommand::Enable {
            name: "srv".to_owned(),
            on: false
        }
    );
}

#[test]
fn add_parses_both_transports() {
    assert_eq!(
        mcp_cli::parse(&args(&["add", "srv", "--command", "/bin/x"])).unwrap(),
        McpCommand::Add {
            name: "srv".to_owned(),
            transport: McpTransportKind::Stdio,
            target: "/bin/x".to_owned(),
            args: Vec::new(),
            env: BTreeMap::new(),
        }
    );
    assert_eq!(
        mcp_cli::parse(&args(&["add", "srv", "--url", "https://h.test/mcp"])).unwrap(),
        McpCommand::Add {
            name: "srv".to_owned(),
            transport: McpTransportKind::StreamableHttp,
            target: "https://h.test/mcp".to_owned(),
            args: Vec::new(),
            env: BTreeMap::new(),
        }
    );
}

/// `heycode mcp add` can carry what a real server needs — arguments after `--`
/// and environment via `-e` — and refuses what it does not understand instead
/// of silently dropping it. Reference: `claude mcp add name -e K=V -- cmd args`.
#[test]
fn add_accepts_arguments_and_environment_and_refuses_unknown_flags() {
    let parsed = mcp_cli::parse(&args(&[
        "add",
        "srv",
        "-e",
        "MODE=fast",
        "-e",
        "HOME_TOKEN",
        "--command",
        "server-bin",
        "--",
        "--port",
        "8080",
        "-v",
    ]))
    .unwrap();
    let McpCommand::Add {
        name,
        transport,
        target,
        args: server_args,
        env,
    } = parsed
    else {
        panic!("expected add: {parsed:?}");
    };
    assert_eq!(name, "srv");
    assert_eq!(transport, McpTransportKind::Stdio);
    assert_eq!(target, "server-bin");
    assert_eq!(server_args, ["--port", "8080", "-v"]);
    assert_eq!(env.get("MODE").map(String::as_str), Some("fast"));
    assert_eq!(
        env.get("HOME_TOKEN").map(String::as_str),
        Some(""),
        "`-e KEY` with no value inherits the host variable at launch"
    );

    let refused = mcp_cli::parse(&args(&["add", "srv", "--command", "x", "--foo"])).unwrap_err();
    assert!(refused.contains("--foo"), "{refused}");

    let management = management();
    let run = mcp_cli::run(
        &management,
        &mcp_cli::parse(&args(&[
            "add",
            "srv",
            "-e",
            "MODE=fast",
            "--command",
            "server-bin",
            "--",
            "--port",
            "8080",
        ]))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(run, "added `srv`");
    let listed = mcp_cli::run(&management, &mcp_cli::parse(&args(&["list"])).unwrap()).unwrap();
    assert!(listed.contains("server-bin --port 8080"), "{listed}");

    let secret = mcp_cli::run(
        &management,
        &mcp_cli::parse(&args(&[
            "add",
            "leaky",
            "-e",
            "KEY=sk-0123456789abcdef", // gitleaks:allow -- deliberate rejected test credential
            "--command",
            "x",
        ]))
        .unwrap(),
    )
    .unwrap_err();
    assert!(
        secret.contains("inherit"),
        "credential-looking values are refused: {secret}"
    );

    let bad_url = mcp_cli::run(
        &management,
        &mcp_cli::parse(&args(&["add", "bad", "--url", "notaurl"])).unwrap(),
    )
    .unwrap_err();
    assert!(bad_url.contains("URL"), "{bad_url}");
}
