//! `[mcp.servers.<name>]` transport selection: exactly one, chosen explicitly.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_config::{Config, McpServerTransport};

fn load(document: &str) -> Config {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("heycode.toml");
    std::fs::write(&path, document).unwrap();
    Config::load(Some(&path)).unwrap()
}

#[test]
fn a_stdio_server_keeps_its_exact_argv_and_environment() {
    let config = load(
        r#"
schema_version = 17
[mcp.servers.local]
command = "my-server"
args = ["--stdio", "--strict"]
env = { TOKEN_NAME = "value" }
"#,
    );
    let transport = config.mcp.servers["local"].transport("local").unwrap();
    let McpServerTransport::Stdio { command, args, env } = transport else {
        panic!("expected stdio")
    };
    assert_eq!(command, "my-server");
    assert_eq!(args, vec!["--stdio".to_owned(), "--strict".to_owned()]);
    assert_eq!(env.get("TOKEN_NAME").map(String::as_str), Some("value"));
}

#[test]
fn a_url_selects_streamable_http() {
    let config = load(
        r#"
schema_version = 17
[mcp.servers.remote]
url = "https://mcp.example.test/v1"
"#,
    );
    assert_eq!(
        config.mcp.servers["remote"].transport("remote").unwrap(),
        McpServerTransport::StreamableHttp {
            url: "https://mcp.example.test/v1".to_owned()
        }
    );
}

#[test]
fn an_ambiguous_or_absent_transport_fails_loud_naming_the_server() {
    for (document, expected) in [
        (
            r#"
schema_version = 17
[mcp.servers.both]
command = "my-server"
url = "https://mcp.example.test/v1"
"#,
            "both `command` and `url`",
        ),
        (
            r#"
schema_version = 17
[mcp.servers.neither]
args = ["--stdio"]
"#,
            "neither `command` nor `url`",
        ),
    ] {
        let config = load(document);
        let (name, server) = config.mcp.servers.iter().next().unwrap();
        let error = server
            .transport(name)
            .expect_err("an unselected transport must fail rather than default");
        let rendered = error.to_string();
        assert!(rendered.contains(expected), "{rendered}");
        assert!(
            rendered.contains(name.as_str()),
            "the failure must name the offending server: {rendered}"
        );
    }
}

#[test]
fn stdio_only_fields_are_refused_on_an_http_endpoint() {
    // `args`/`env` describe a child process. Accepting them beside a URL would
    // silently discard operator intent.
    for document in [
        r#"
schema_version = 17
[mcp.servers.remote]
url = "https://mcp.example.test/v1"
args = ["--stdio"]
"#,
        r#"
schema_version = 17
[mcp.servers.remote]
url = "https://mcp.example.test/v1"
env = { TOKEN_NAME = "value" }
"#,
    ] {
        let config = load(document);
        let error = config.mcp.servers["remote"]
            .transport("remote")
            .expect_err("stdio-only fields must not silently apply to a URL");
        assert!(error.to_string().contains("apply only to a stdio"));
    }
}
