//! The MCP servers each agent CLI is already configured with — MonoCode's
//! Settings → MCP, read-only.
//!
//! An agent started by Ginka gets Ginka's own servers (`crate::tools`) on top
//! of whatever its vendor's configuration names; this module reads the
//! latter so a reader can see the whole set without opening three files.
//! Nothing here writes a vendor's configuration.
//!
//! - Claude Code: `mcpServers` in `~/.claude.json` (every project), the same
//!   key under `projects.<path>` there (one project, "local"), and a
//!   repository's `.mcp.json` (checked in, "project").
//! - Codex: `[mcp_servers.<name>]` tables in `~/.codex/config.toml`. Only
//!   the table names and their `command` are read, line by line, so no TOML
//!   parser is linked for two keys.

use ginka_protocol::model::{McpScope, McpServerEntry, McpServerSpec, McpTarget};
use serde_json::Value;
use std::path::Path;

/// Every MCP server the agent CLIs under `home` are configured with, and
/// those a repository at `project` adds. Unreadable or missing files are
/// skipped: a vendor never installed has nothing configured.
pub fn discover(home: &Path, project: Option<&Path>) -> Vec<McpServerEntry> {
    let mut found = Vec::new();
    if let Some(config) = read_json(&home.join(".claude.json")) {
        claude_servers(&config["mcpServers"], McpScope::User, &mut found);
        if let Some(project) = project
            && let Some(projects) = config["projects"].as_object()
        {
            let key = project.to_string_lossy();
            if let Some(entry) = projects.get(key.as_ref()) {
                claude_servers(&entry["mcpServers"], McpScope::Local, &mut found);
            }
        }
    }
    if let Some(project) = project
        && let Some(config) = read_json(&project.join(".mcp.json"))
    {
        claude_servers(&config["mcpServers"], McpScope::Project, &mut found);
    }
    if let Ok(text) = std::fs::read_to_string(home.join(".codex").join("config.toml")) {
        found.extend(codex_servers(&text));
    }
    found
}

/// A server address with whatever could be a credential taken out: the
/// user information before `@`, the query and the fragment. Servers are
/// often addressed with a key in the URL, and this list reaches agents over
/// MCP. A command is returned as it is — its arguments were never read.
pub fn redact(target: &str) -> String {
    let Some((scheme, rest)) = target.split_once("://") else {
        return target.to_string();
    };
    let (rest, fragment) = match rest.split_once('#') {
        Some((rest, _)) => (rest, "#…"),
        None => (rest, ""),
    };
    let (rest, query) = match rest.split_once('?') {
        Some((rest, _)) => (rest, "?…"),
        None => (rest, ""),
    };
    let (authority, path) = match rest.find('/') {
        Some(at) => rest.split_at(at),
        None => (rest, ""),
    };
    let authority = match authority.rsplit_once('@') {
        Some((_, host)) => format!("…@{host}"),
        None => authority.to_string(),
    };
    format!("{scheme}://{authority}{path}{query}{fragment}")
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn claude_servers(servers: &Value, scope: McpScope, found: &mut Vec<McpServerEntry>) {
    let Some(servers) = servers.as_object() else {
        return;
    };
    for (name, server) in servers {
        let target = server["command"]
            .as_str()
            .or_else(|| server["url"].as_str())
            .map(redact);
        found.push(McpServerEntry {
            name: name.clone(),
            provider: "claude".into(),
            scope,
            target,
        });
    }
}

/// The `[mcp_servers.<name>]` tables in a Codex `config.toml`, with each
/// one's `command` (or `url`) when it is a plain string on its own line.
pub fn codex_servers(text: &str) -> Vec<McpServerEntry> {
    let mut found: Vec<McpServerEntry> = Vec::new();
    let mut current: Option<usize> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            current = None;
            let header = line.trim_start_matches('[').trim_end_matches(']').trim();
            // A server's own table, not one nested under it (`.env`).
            if let Some(name) = header.strip_prefix("mcp_servers.")
                && !name.contains('.')
            {
                let name = name.trim_matches('"').to_string();
                found.push(McpServerEntry {
                    name,
                    provider: "codex".into(),
                    scope: McpScope::User,
                    target: None,
                });
                current = Some(found.len() - 1);
            }
            continue;
        }
        let Some(index) = current else {
            continue;
        };
        if let Some((key, value)) = line.split_once('=')
            && matches!(key.trim(), "command" | "url")
            && found[index].target.is_none()
        {
            let value = value.trim();
            if let Some(text) = value
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
            {
                found[index].target = Some(redact(text));
            }
        }
    }
    found
}

/// The vendor CLI invocation that adds `spec` to its own configuration:
/// `claude mcp add` or `codex mcp add`, so each vendor's file format stays
/// the vendor's business. Environment variables are never passed — they are
/// where tokens go, and those belong to the vendor's own prompt.
///
/// Refuses a name that is not plain, a scope Codex does not have (it keeps
/// servers per user only), and an empty command or a non-HTTP(S) URL.
pub fn add_args(spec: &McpServerSpec) -> Result<Vec<String>, String> {
    check_name(&spec.name)?;
    let mut args: Vec<String> = vec!["mcp".into(), "add".into()];
    match spec.provider.as_str() {
        "claude" => {
            args.extend(["--scope".into(), scope_word(spec.scope).into()]);
            match &spec.target {
                McpTarget::Url { url } => {
                    check_url(url)?;
                    args.extend([
                        "--transport".into(),
                        "http".into(),
                        spec.name.clone(),
                        url.clone(),
                    ]);
                }
                McpTarget::Command {
                    program,
                    args: rest,
                } => {
                    check_program(program)?;
                    args.extend([spec.name.clone(), "--".into(), program.clone()]);
                    args.extend(rest.iter().cloned());
                }
            }
        }
        "codex" => {
            if spec.scope != McpScope::User {
                return Err("Codex keeps MCP servers per user only".into());
            }
            args.push(spec.name.clone());
            match &spec.target {
                McpTarget::Url { url } => {
                    check_url(url)?;
                    args.extend(["--url".into(), url.clone()]);
                }
                McpTarget::Command {
                    program,
                    args: rest,
                } => {
                    check_program(program)?;
                    args.extend(["--".into(), program.clone()]);
                    args.extend(rest.iter().cloned());
                }
            }
        }
        other => return Err(format!("{other} has no MCP configuration Ginka can write")),
    }
    Ok(args)
}

/// The vendor CLI invocation that removes `name` from `provider`'s
/// configuration, from `scope` when given (Claude Code otherwise removes it
/// from wherever it is).
pub fn remove_args(
    provider: &str,
    name: &str,
    scope: Option<McpScope>,
) -> Result<Vec<String>, String> {
    check_name(name)?;
    let mut args: Vec<String> = vec!["mcp".into(), "remove".into()];
    match provider {
        "claude" => {
            if let Some(scope) = scope {
                args.extend(["--scope".into(), scope_word(scope).into()]);
            }
        }
        "codex" => {
            if scope.is_some_and(|scope| scope != McpScope::User) {
                return Err("Codex keeps MCP servers per user only".into());
            }
        }
        other => return Err(format!("{other} has no MCP configuration Ginka can write")),
    }
    args.push(name.to_string());
    Ok(args)
}

fn scope_word(scope: McpScope) -> &'static str {
    match scope {
        McpScope::User => "user",
        McpScope::Project => "project",
        McpScope::Local => "local",
    }
}

fn check_name(name: &str) -> Result<(), String> {
    let plain = !name.is_empty()
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if plain {
        Ok(())
    } else {
        Err(format!("{name:?} is not a plain server name"))
    }
}

fn check_program(program: &str) -> Result<(), String> {
    if program.trim().is_empty() {
        Err("an MCP server needs a command to run".into())
    } else {
        Ok(())
    }
}

fn check_url(url: &str) -> Result<(), String> {
    if url.starts_with("https://") || url.starts_with("http://") {
        Ok(())
    } else {
        Err(format!("{url:?} is not an HTTP address"))
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_url_never_carries_its_credentials_out() {
        assert_eq!(
            redact("https://mcp.example/sse?apiKey=s3cret&x=1"),
            "https://mcp.example/sse?…"
        );
        assert_eq!(
            redact("https://me:pw@mcp.example/mcp"),
            "https://…@mcp.example/mcp"
        );
        assert_eq!(
            redact("https://mcp.example/mcp#token=abc"),
            "https://mcp.example/mcp#…"
        );
        assert_eq!(redact("https://mcp.example/mcp"), "https://mcp.example/mcp");
        assert_eq!(redact("npx"), "npx", "a command is not a URL");

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".claude.json"),
            r#"{"mcpServers": {"x": {"url": "https://u:p@h.example/mcp?key=k"}}}"#,
        )
        .unwrap();
        let found = discover(dir.path(), None);
        assert_eq!(
            found[0].target.as_deref(),
            Some("https://…@h.example/mcp?…")
        );
    }

    fn spec(provider: &str, scope: McpScope, target: McpTarget) -> McpServerSpec {
        McpServerSpec {
            name: "docs".into(),
            provider: provider.into(),
            scope,
            target,
        }
    }

    #[test]
    fn adding_speaks_each_vendors_own_command_line() {
        let command = McpTarget::Command {
            program: "npx".into(),
            args: vec!["-y".into(), "docs-mcp".into()],
        };
        assert_eq!(
            add_args(&spec("claude", McpScope::Project, command.clone())).unwrap(),
            [
                "mcp", "add", "--scope", "project", "docs", "--", "npx", "-y", "docs-mcp"
            ]
        );
        assert_eq!(
            add_args(&spec(
                "claude",
                McpScope::User,
                McpTarget::Url {
                    url: "https://docs.example/mcp".into()
                }
            ))
            .unwrap(),
            [
                "mcp",
                "add",
                "--scope",
                "user",
                "--transport",
                "http",
                "docs",
                "https://docs.example/mcp"
            ]
        );
        assert_eq!(
            add_args(&spec("codex", McpScope::User, command)).unwrap(),
            ["mcp", "add", "docs", "--", "npx", "-y", "docs-mcp"]
        );
        assert_eq!(
            add_args(&spec(
                "codex",
                McpScope::User,
                McpTarget::Url {
                    url: "https://docs.example/mcp".into()
                }
            ))
            .unwrap(),
            ["mcp", "add", "docs", "--url", "https://docs.example/mcp"]
        );
    }

    #[test]
    fn what_a_vendor_cannot_hold_or_a_name_that_is_not_plain_is_refused() {
        let command = McpTarget::Command {
            program: "x".into(),
            args: Vec::new(),
        };
        assert!(add_args(&spec("codex", McpScope::Project, command.clone())).is_err());
        assert!(add_args(&spec("gemini", McpScope::User, command.clone())).is_err());
        let mut bad = spec("claude", McpScope::User, command);
        bad.name = "--scope".into();
        assert!(add_args(&bad).is_err());
        assert!(
            add_args(&spec(
                "claude",
                McpScope::User,
                McpTarget::Url {
                    url: "file:///etc/passwd".into()
                }
            ))
            .is_err()
        );
        assert!(
            add_args(&spec(
                "claude",
                McpScope::User,
                McpTarget::Command {
                    program: " ".into(),
                    args: Vec::new()
                }
            ))
            .is_err()
        );
    }

    #[test]
    fn removing_names_the_scope_only_where_the_vendor_has_one() {
        assert_eq!(
            remove_args("claude", "docs", Some(McpScope::Local)).unwrap(),
            ["mcp", "remove", "--scope", "local", "docs"]
        );
        assert_eq!(
            remove_args("claude", "docs", None).unwrap(),
            ["mcp", "remove", "docs"]
        );
        assert_eq!(
            remove_args("codex", "docs", Some(McpScope::User)).unwrap(),
            ["mcp", "remove", "docs"]
        );
        assert!(remove_args("codex", "docs", Some(McpScope::Project)).is_err());
        assert!(remove_args("claude", "-rf", None).is_err());
    }

    use super::*;

    fn names(found: &[McpServerEntry]) -> Vec<(String, String, McpScope, Option<String>)> {
        found
            .iter()
            .map(|entry| {
                (
                    entry.provider.clone(),
                    entry.name.clone(),
                    entry.scope,
                    entry.target.clone(),
                )
            })
            .collect()
    }

    #[test]
    fn every_vendor_file_and_scope_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let project = dir.path().join("repo");
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            home.join(".claude.json"),
            serde_json::json!({
                "mcpServers": {"github": {"type": "http", "url": "https://api.example/mcp"}},
                "projects": {
                    project.to_string_lossy(): {"mcpServers": {"db": {"command": "pg-mcp"}}},
                    "/somewhere/else": {"mcpServers": {"other": {"command": "x"}}}
                }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            project.join(".mcp.json"),
            r#"{"mcpServers": {"playwright": {"command": "npx", "args": ["@playwright/mcp"]}}}"#,
        )
        .unwrap();
        std::fs::write(
            home.join(".codex/config.toml"),
            "model = \"gpt-5\"\n\n[mcp_servers.sentry]\ncommand = \"sentry-mcp\"\nargs = [\"--stdio\"]\n\n[mcp_servers.sentry.env]\nTOKEN = \"secret\"\n\n[mcp_servers.\"docs\"]\nurl = \"https://docs.example/mcp\"\n",
        )
        .unwrap();

        let found = discover(&home, Some(&project));
        assert_eq!(
            names(&found),
            vec![
                (
                    "claude".into(),
                    "github".into(),
                    McpScope::User,
                    Some("https://api.example/mcp".into())
                ),
                (
                    "claude".into(),
                    "db".into(),
                    McpScope::Local,
                    Some("pg-mcp".into())
                ),
                (
                    "claude".into(),
                    "playwright".into(),
                    McpScope::Project,
                    Some("npx".into())
                ),
                (
                    "codex".into(),
                    "sentry".into(),
                    McpScope::User,
                    Some("sentry-mcp".into())
                ),
                (
                    "codex".into(),
                    "docs".into(),
                    McpScope::User,
                    Some("https://docs.example/mcp".into())
                ),
            ]
        );
        assert!(
            !format!("{found:?}").contains("secret"),
            "an environment table's values are never read"
        );
    }

    #[test]
    fn nothing_configured_is_an_empty_list_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(discover(dir.path(), Some(dir.path())).is_empty());
        std::fs::write(dir.path().join(".claude.json"), "not json").unwrap();
        assert!(discover(dir.path(), None).is_empty());
    }
}
