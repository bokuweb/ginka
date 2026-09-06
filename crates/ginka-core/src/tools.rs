//! The MCP servers an agent is told about when it starts.
//!
//! Rule 3 says an agent can do whatever a person can, and `ginka mcp` is how:
//! a bridge that turns tool calls into daemon requests. A bridge the user has
//! to register by hand in every vendor's own config is one that most sessions
//! run without, so the daemon registers it itself, per session, on the
//! command line of the agent it spawns. Every driver has a way to take an MCP
//! server that way, and none of them needs a file written into the user's
//! home for it.
//!
//! The same path carries two more things. `zvec-grep` (`zg`), when it is
//! installed and the worktree has been indexed, gives the agent semantic and
//! BM25 search over the workspace beside the exact search Ginka already
//! offers — it is a Node program with its own MCP server, and handing it to
//! the agent is the whole integration; Ginka does not link a vector database
//! (`docs/roadmap.md` §8, 2026-09-06). And anything the user lists under
//! `tools.servers` in `settings.json` goes along too, so one setting reaches
//! every agent rather than each vendor's file being kept in step by hand.
//!
//! Nothing here is secret. The bridge finds the daemon through `daemon.json`
//! in `GINKA_HOME`, so the agent's command line carries a path and a
//! directory, never the token.

use crate::Paths;
use std::path::{Path, PathBuf};

/// The environment variable that says where the `ginka` command is, for a
/// daemon that cannot find it beside itself.
pub const CLI_ENV: &str = "GINKA_CLI";

/// The name zvec-grep registers its server under in every agent it installs
/// into; kept so a user who ran `zg install` themselves is not given the same
/// server twice under two names.
pub const ZVEC_GREP_SERVER: &str = "zvec_grep";

/// Where `zg index` keeps a workspace's index, under the worktree root.
pub const ZVEC_GREP_INDEX_DIR: &str = ".zvec-grep";

/// One MCP server, as a driver puts it on an agent's command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServer {
    /// What the agent calls it; its tools appear as `mcp__<name>__<tool>`.
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Everything about MCP servers the user can set (`settings.json`, `tools`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolSettings {
    /// Give every agent Ginka's own MCP bridge. On by default: it is what
    /// lets an agent start a sibling, read a diff, or fan out.
    pub ginka: bool,
    /// Give every agent `zg`'s server where the workspace is indexed.
    pub zvec_grep: bool,
    /// The user's own servers, by name, given to every agent.
    pub servers: std::collections::BTreeMap<String, McpServerSettings>,
}

impl Default for ToolSettings {
    fn default() -> Self {
        Self {
            ginka: true,
            zvec_grep: true,
            servers: Default::default(),
        }
    }
}

/// One server the user configured.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerSettings {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
}

/// Which `ginka` a daemon hands to agents.
///
/// An explicit `GINKA_CLI` wins. Otherwise the development CLI beside this
/// binary (`ginka-cli`: the app crate owns the bare name in `target/debug`,
/// see the decision log), then `ginka` on `PATH`, which is what an install
/// puts there. `None` means agents are given no bridge, and the log says so
/// once per daemon rather than once per session.
pub fn locate_cli() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(CLI_ENV) {
        return Some(PathBuf::from(path));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join(format!("ginka-cli{}", std::env::consts::EXE_SUFFIX));
        if sibling.is_file() {
            return Some(sibling);
        }
    }
    on_path("ginka")
}

/// The first `program` on `PATH` that can be run.
pub fn on_path(program: &str) -> Option<PathBuf> {
    let name = format!("{program}{}", std::env::consts::EXE_SUFFIX);
    std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .map(|dir| dir.join(&name))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// The servers an agent starting in `worktree` is given.
///
/// `cli` is where `ginka` is, if anywhere; `zg` is where zvec-grep is. Both
/// are looked up once by the caller rather than here, so a test can say
/// what is installed without touching `PATH`. The bridge is only offered
/// when it exists, and `zg` only when the worktree has an index: a server
/// whose every answer is "run `zg index` first" is noise in the agent's tool
/// list.
pub fn servers_for(
    settings: &ToolSettings,
    paths: &Paths,
    worktree: &Path,
    cli: Option<&Path>,
    zg: Option<&Path>,
) -> Vec<McpServer> {
    let mut servers = Vec::new();
    if settings.ginka
        && let Some(cli) = cli
    {
        servers.push(McpServer {
            name: "ginka".to_string(),
            command: cli.to_string_lossy().into_owned(),
            args: vec!["mcp".to_string()],
            // So the bridge finds *this* daemon, whichever state directory
            // it runs against.
            env: vec![(
                "GINKA_HOME".to_string(),
                paths.root().to_string_lossy().into_owned(),
            )],
        });
    }
    if settings.zvec_grep
        && let Some(zg) = zg
        && worktree.join(ZVEC_GREP_INDEX_DIR).is_dir()
        && !settings.servers.contains_key(ZVEC_GREP_SERVER)
    {
        servers.push(McpServer {
            name: ZVEC_GREP_SERVER.to_string(),
            command: zg.to_string_lossy().into_owned(),
            args: vec!["server".to_string(), "--stdio".to_string()],
            env: Vec::new(),
        });
    }
    for (name, server) in &settings.servers {
        servers.push(McpServer {
            name: name.clone(),
            command: server.command.clone(),
            args: server.args.clone(),
            env: server
                .env
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        });
    }
    servers
}

/// The servers as Claude Code's `--mcp-config` takes them: one JSON object
/// with an `mcpServers` map, passed as a string rather than a file so the
/// daemon writes nothing into the user's home for a session.
pub fn claude_config(servers: &[McpServer]) -> String {
    let map: serde_json::Map<String, serde_json::Value> = servers
        .iter()
        .map(|server| {
            let mut entry = serde_json::json!({
                "command": server.command,
                "args": server.args,
            });
            if !server.env.is_empty() {
                entry["env"] = server
                    .env
                    .iter()
                    .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
                    .collect::<serde_json::Map<_, _>>()
                    .into();
            }
            (server.name.clone(), entry)
        })
        .collect();
    serde_json::json!({ "mcpServers": map }).to_string()
}

/// The servers as Codex's `-c key=value` overrides, one per field, which is
/// how a server is added to a session without touching `config.toml`.
pub fn codex_overrides(servers: &[McpServer]) -> Vec<String> {
    let mut overrides = Vec::new();
    for server in servers {
        let prefix = format!("mcp_servers.{}", server.name);
        overrides.push(format!("{prefix}.command={}", toml_string(&server.command)));
        overrides.push(format!(
            "{prefix}.args=[{}]",
            server
                .args
                .iter()
                .map(|arg| toml_string(arg))
                .collect::<Vec<_>>()
                .join(",")
        ));
        if !server.env.is_empty() {
            overrides.push(format!(
                "{prefix}.env={{{}}}",
                server
                    .env
                    .iter()
                    .map(|(key, value)| format!("{key}={}", toml_string(value)))
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
    }
    overrides
}

/// A TOML basic string. JSON's escapes are a subset of TOML's for everything
/// a path or an argument can contain, so the JSON encoder is the one used.
fn toml_string(text: &str) -> String {
    serde_json::to_string(text).expect("a string always serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path().join("state"));
        (dir, paths)
    }

    #[test]
    fn the_bridge_is_offered_when_the_cli_is_found_and_told_where_home_is() {
        let (dir, paths) = paths();
        let servers = servers_for(
            &ToolSettings::default(),
            &paths,
            dir.path(),
            Some(Path::new("/opt/ginka")),
            None,
        );
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "ginka");
        assert_eq!(servers[0].command, "/opt/ginka");
        assert_eq!(servers[0].args, ["mcp"]);
        assert_eq!(
            servers[0].env,
            [(
                "GINKA_HOME".to_string(),
                paths.root().to_string_lossy().into_owned()
            )]
        );
    }

    #[test]
    fn no_cli_means_no_bridge_rather_than_a_broken_one() {
        let (dir, paths) = paths();
        assert!(servers_for(&ToolSettings::default(), &paths, dir.path(), None, None).is_empty());
    }

    #[test]
    fn zvec_grep_is_offered_only_where_the_worktree_is_indexed() {
        let (dir, paths) = paths();
        let zg = Path::new("/usr/local/bin/zg");
        let settings = ToolSettings::default();
        assert!(servers_for(&settings, &paths, dir.path(), None, Some(zg)).is_empty());

        std::fs::create_dir(dir.path().join(ZVEC_GREP_INDEX_DIR)).unwrap();
        let servers = servers_for(&settings, &paths, dir.path(), None, Some(zg));
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, ZVEC_GREP_SERVER);
        assert_eq!(servers[0].args, ["server", "--stdio"]);
    }

    #[test]
    fn the_user_can_switch_either_off_and_add_their_own() {
        let (dir, paths) = paths();
        std::fs::create_dir(dir.path().join(ZVEC_GREP_INDEX_DIR)).unwrap();
        let settings: ToolSettings = serde_json::from_str(
            r#"{"ginka": false, "zvec_grep": false,
                "servers": {"docs": {"command": "npx", "args": ["-y", "docs-mcp"],
                                     "env": {"DOCS_ROOT": "/srv/docs"}}}}"#,
        )
        .unwrap();
        let servers = servers_for(
            &settings,
            &paths,
            dir.path(),
            Some(Path::new("/opt/ginka")),
            Some(Path::new("/usr/local/bin/zg")),
        );
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "docs");
        assert_eq!(servers[0].command, "npx");
        assert_eq!(
            servers[0].env,
            [("DOCS_ROOT".to_string(), "/srv/docs".to_string())]
        );
    }

    #[test]
    fn a_zvec_grep_the_user_configured_themselves_is_not_doubled() {
        let (dir, paths) = paths();
        std::fs::create_dir(dir.path().join(ZVEC_GREP_INDEX_DIR)).unwrap();
        let settings: ToolSettings = serde_json::from_str(
            r#"{"servers": {"zvec_grep": {"command": "/home/me/zg", "args": ["server", "--stdio"]}}}"#,
        )
        .unwrap();
        let servers = servers_for(
            &settings,
            &paths,
            dir.path(),
            None,
            Some(Path::new("/usr/bin/zg")),
        );
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].command, "/home/me/zg");
    }

    #[test]
    fn claudes_config_is_one_json_object_the_cli_can_parse() {
        let servers = vec![McpServer {
            name: "ginka".into(),
            command: "/opt/ginka".into(),
            args: vec!["mcp".into()],
            env: vec![("GINKA_HOME".into(), "/home/me/.ginka".into())],
        }];
        let parsed: serde_json::Value = serde_json::from_str(&claude_config(&servers)).unwrap();
        assert_eq!(parsed["mcpServers"]["ginka"]["command"], "/opt/ginka");
        assert_eq!(parsed["mcpServers"]["ginka"]["args"][0], "mcp");
        assert_eq!(
            parsed["mcpServers"]["ginka"]["env"]["GINKA_HOME"],
            "/home/me/.ginka"
        );
    }

    #[test]
    fn codexs_overrides_are_one_toml_assignment_per_field() {
        let servers = vec![McpServer {
            name: "zvec_grep".into(),
            command: "/usr/local/bin/zg".into(),
            args: vec!["server".into(), "--stdio".into()],
            env: vec![("A".into(), "quote \" here".into())],
        }];
        assert_eq!(
            codex_overrides(&servers),
            [
                r#"mcp_servers.zvec_grep.command="/usr/local/bin/zg""#,
                r#"mcp_servers.zvec_grep.args=["server","--stdio"]"#,
                r#"mcp_servers.zvec_grep.env={A="quote \" here"}"#,
            ]
        );
    }
}
