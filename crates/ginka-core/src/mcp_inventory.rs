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

use ginka_protocol::model::{McpScope, McpServerEntry};
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
            .map(str::to_string);
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
                found[index].target = Some(text.to_string());
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
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
