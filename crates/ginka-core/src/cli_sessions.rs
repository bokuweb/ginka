//! Conversations an agent's own CLI keeps, and bringing one into Ginka.
//!
//! Someone who ran `claude` or `codex` in a terminal before opening Ginka has
//! a conversation Ginka never saw. The vendor keeps it on disk — Claude Code
//! under `~/.claude/projects/<escaped cwd>/<id>.jsonl`, Codex under
//! `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` — and either CLI resumes it
//! by id. So a workspace can list the ones started in its directory, and
//! adopting one makes a Ginka session that holds the vendor's id: its next
//! turn continues that same thread, with the context the agent already has.
//!
//! What is read is only what a transcript shows: the user's prompts and the
//! agent's replies. Tool traffic, reasoning and the vendor's bookkeeping stay
//! where they are, and the import is bounded, because the vendor's thread —
//! not this copy — is what the agent works from. Both formats are the
//! vendors' own and undocumented; anything unrecognised is skipped rather
//! than failing the list, and a file whose recorded directory is not the
//! workspace's is never offered.

use chrono::DateTime;
use ginka_protocol::event::AgentEvent;
use ginka_protocol::model::{CliSession, TranscriptPayload};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// How many conversations a list offers, newest first.
pub const LIST_LIMIT: usize = 50;

/// How many Codex rollouts are opened to find a directory's. Codex keeps
/// every directory's sessions together, by date, so the newest are read
/// first and the rest are not looked at.
const CODEX_SCAN_LIMIT: usize = 600;

/// How many turns an adopted conversation brings into the transcript.
pub const IMPORT_TURNS: usize = 40;

/// What one imported message is cut to, in characters.
const MESSAGE_CHARS: usize = 20_000;

/// What a title is cut to, in characters.
const TITLE_CHARS: usize = 80;

/// Where the vendors keep their conversations on this host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roots {
    /// Claude Code's configuration directory (`~/.claude`).
    pub claude: Option<PathBuf>,
    /// Codex's home (`~/.codex`).
    pub codex: Option<PathBuf>,
}

impl Roots {
    /// The directories the CLIs use for the system login: their own home
    /// variables when set, else the defaults under the home directory. A
    /// Ginka-managed account keeps its own directory, which only Ginka's
    /// sessions write to, so there is nothing of the user's there to adopt.
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let pick = |variable: &str, default: &str| {
            std::env::var_os(variable)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .or_else(|| home.as_ref().map(|home| home.join(default)))
        };
        Self {
            claude: pick("CLAUDE_CONFIG_DIR", ".claude"),
            codex: pick("CODEX_HOME", ".codex"),
        }
    }
}

/// A conversation found on disk, with the file it lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// What a client is shown.
    pub session: CliSession,
    /// The vendor's file. Never leaves the daemon.
    pub path: PathBuf,
}

/// What was read from each file, kept while the file is unchanged.
///
/// A project's Claude store runs to hundreds of megabytes; reading it every
/// time the dialog opens would hold the daemon up for each open. A file the
/// CLI is still writing changes size, and is read again.
#[derive(Debug, Default)]
pub struct Index {
    seen: HashMap<(PathBuf, PathBuf), (Stamp, Option<Found>)>,
}

/// A file's modification time and length: what says it changed.
type Stamp = (Option<std::time::SystemTime>, u64);

impl Index {
    /// The conversation in `path`, if it was started in `cwd`, from the last
    /// read when the file has not changed since.
    fn read(
        &mut self,
        path: &Path,
        cwd: &Path,
        read: impl FnOnce() -> Option<Found>,
    ) -> Option<Found> {
        let meta = std::fs::metadata(path).ok()?;
        let stamp = (meta.modified().ok(), meta.len());
        let key = (path.to_path_buf(), cwd.to_path_buf());
        if let Some((seen, found)) = self.seen.get(&key)
            && *seen == stamp
        {
            return found.clone();
        }
        let found = read();
        self.seen.insert(key, (stamp, found.clone()));
        found
    }
}

/// The conversations started in `cwd`, newest first.
///
/// `known` holds the vendor ids Ginka's own sessions already hold: those are
/// Ginka's, or were adopted, and are not offered again.
pub fn list(index: &mut Index, roots: &Roots, cwd: &Path, known: &HashSet<String>) -> Vec<Found> {
    let mut found: Vec<Found> = Vec::new();
    if let Some(root) = &roots.claude {
        found.extend(claude_sessions(index, root, cwd));
    }
    if let Some(root) = &roots.codex {
        found.extend(codex_sessions(index, root, cwd));
    }
    found.retain(|found| {
        found.session.prompts > 0 && !known.contains(&found.session.vendor_session_id)
    });
    found.sort_by_key(|found| std::cmp::Reverse(found.session.updated_at));
    found.truncate(LIST_LIMIT);
    found
}

/// One conversation by agent and vendor id, if it was started in `cwd`.
///
/// The client names a conversation by id, never by path: the file is found
/// again here, so a request cannot point the daemon at anything else.
pub fn find(
    index: &mut Index,
    roots: &Roots,
    cwd: &Path,
    agent: &str,
    vendor_id: &str,
) -> Option<Found> {
    if !is_vendor_id(vendor_id) {
        return None;
    }
    let found = match agent {
        "claude" => claude_sessions(index, roots.claude.as_ref()?, cwd),
        "codex" => codex_sessions(index, roots.codex.as_ref()?, cwd),
        _ => return None,
    };
    found
        .into_iter()
        .find(|found| found.session.vendor_session_id == vendor_id && found.session.prompts > 0)
}

/// The transcript an adopted conversation starts with: its last
/// [`IMPORT_TURNS`] turns, prompts and replies only, each turn closed the way
/// a live one is so the transcript folds it the same way.
pub fn transcript(found: &Found) -> std::io::Result<Vec<(i64, TranscriptPayload)>> {
    let messages = match found.session.agent.as_str() {
        "claude" => read_claude(&found.path, Replies::Read)?.messages,
        _ => read_codex(&found.path, Replies::Read)?.messages,
    };
    let mut turns: Vec<Vec<&Message>> = Vec::new();
    for message in &messages {
        match message.role {
            Role::User => turns.push(vec![message]),
            // A reply before any prompt has nothing to answer; skip it.
            Role::Agent => {
                if let Some(turn) = turns.last_mut() {
                    turn.push(message);
                }
            }
        }
    }
    let skip = turns.len().saturating_sub(IMPORT_TURNS);
    let mut payloads = Vec::new();
    for (index, turn) in turns.iter().enumerate().skip(skip) {
        let mut at = 0;
        for message in turn {
            at = message.at;
            let text = cut(&message.text, MESSAGE_CHARS);
            payloads.push((
                message.at,
                match message.role {
                    Role::User => TranscriptPayload::User { text },
                    Role::Agent => TranscriptPayload::Agent {
                        event: AgentEvent::TextDelta { text },
                    },
                },
            ));
        }
        payloads.push((
            at,
            TranscriptPayload::Agent {
                event: AgentEvent::TurnEnd {
                    turn: u32::try_from(index + 1).unwrap_or(u32::MAX),
                },
            },
        ));
    }
    Ok(payloads)
}

/// Claude Code names a project's directory after its path, with everything
/// that is not a letter or a digit turned into `-`.
pub fn claude_project_dir(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// A vendor id is a UUID-like token; anything else is not looked for.
fn is_vendor_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Whether a read needs the agent's replies. A list needs only the prompts,
/// and the replies are most of a file, so their lines are passed over
/// without being parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Replies {
    Read,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    User,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Message {
    role: Role,
    text: String,
    at: i64,
}

/// What reading one vendor file yields.
#[derive(Debug, Default)]
struct Read {
    id: Option<String>,
    cwd: Option<String>,
    title: Option<String>,
    messages: Vec<Message>,
}

impl Read {
    fn into_found(self, agent: &str, path: &Path, cwd: &Path) -> Option<Found> {
        if self.cwd.as_deref().map(Path::new) != Some(cwd) {
            return None;
        }
        let id = self.id?;
        let prompts = self
            .messages
            .iter()
            .filter(|m| m.role == Role::User)
            .count();
        let title = self
            .title
            .filter(|title| !title.trim().is_empty())
            .or_else(|| {
                self.messages
                    .iter()
                    .find(|m| m.role == Role::User)
                    .map(|m| m.text.clone())
            })
            .unwrap_or_default();
        let updated_at = std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |since| i64::try_from(since.as_secs()).unwrap_or(0));
        Some(Found {
            session: CliSession {
                agent: agent.to_string(),
                vendor_session_id: id,
                title: cut(&one_line(&title), TITLE_CHARS),
                prompts: u32::try_from(prompts).unwrap_or(u32::MAX),
                updated_at,
            },
            path: path.to_path_buf(),
        })
    }
}

fn claude_sessions(index: &mut Index, root: &Path, cwd: &Path) -> Vec<Found> {
    let dir = root.join("projects").join(claude_project_dir(cwd));
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .filter_map(|path| {
            index.read(&path, cwd, || {
                read_claude(&path, Replies::Skip)
                    .ok()?
                    .into_found("claude", &path, cwd)
            })
        })
        .collect()
}

fn read_claude(path: &Path, replies: Replies) -> std::io::Result<Read> {
    let mut read = Read::default();
    let mut last_agent_id: Option<String> = None;
    for line in BufReader::new(std::fs::File::open(path)?).lines() {
        let line = line?;
        // A tool's output comes back as a user line, and is often the
        // largest thing in the file; it is never a prompt.
        if replies == Replies::Skip
            && (line.contains(r#""type":"tool_result""#)
                || !(line.contains(r#""type":"user""#)
                    || line.contains(r#""type":"custom-title""#)))
        {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let kind = value["type"].as_str().unwrap_or_default();
        if kind == "custom-title" {
            if let Some(title) = value["customTitle"].as_str() {
                read.title = Some(title.to_string());
            }
            continue;
        }
        if kind != "user" && kind != "assistant" {
            continue;
        }
        // Subagents' own traffic and the CLI's injected context are not the
        // conversation the user had.
        if value["isSidechain"].as_bool() == Some(true) || value["isMeta"].as_bool() == Some(true) {
            continue;
        }
        if read.id.is_none() {
            read.id = value["sessionId"].as_str().map(str::to_string);
        }
        if read.cwd.is_none() {
            read.cwd = value["cwd"].as_str().map(str::to_string);
        }
        let at = timestamp(&value["timestamp"]);
        let content = &value["message"]["content"];
        if kind == "user" {
            let text = match content {
                serde_json::Value::String(text) => text.clone(),
                serde_json::Value::Array(blocks) => texts(blocks, "text"),
                _ => String::new(),
            };
            if is_prompt(&text) {
                read.messages.push(Message {
                    role: Role::User,
                    text,
                    at,
                });
            }
            last_agent_id = None;
            continue;
        }
        let serde_json::Value::Array(blocks) = content else {
            continue;
        };
        let text = texts(blocks, "text");
        if text.trim().is_empty() {
            continue;
        }
        // One reply streams as several lines sharing a message id.
        let id = value["message"]["id"].as_str().map(str::to_string);
        match read.messages.last_mut() {
            Some(last) if last.role == Role::Agent && id.is_some() && id == last_agent_id => {
                last.text.push_str("\n\n");
                last.text.push_str(&text);
                last.at = at;
            }
            _ => read.messages.push(Message {
                role: Role::Agent,
                text,
                at,
            }),
        }
        last_agent_id = id;
    }
    Ok(read)
}

fn codex_sessions(index: &mut Index, root: &Path, cwd: &Path) -> Vec<Found> {
    let mut files = Vec::new();
    collect_rollouts(&root.join("sessions"), 0, &mut files);
    // Paths sort by date, and the file name carries the time.
    files.sort_by(|a, b| b.cmp(a));
    files.truncate(CODEX_SCAN_LIMIT);
    // The first line also carries the whole system prompt; one that does
    // not mention the directory is not parsed.
    let needle = serde_json::to_string(&cwd.to_string_lossy()).unwrap_or_default();
    files
        .into_iter()
        .filter_map(|path| {
            index.read(&path, cwd, || {
                if codex_cwd(&path, &needle).as_deref().map(Path::new) != Some(cwd) {
                    return None;
                }
                read_codex(&path, Replies::Skip)
                    .ok()?
                    .into_found("codex", &path, cwd)
            })
        })
        .collect()
}

/// Walk `sessions/YYYY/MM/DD`, newest first, stopping once enough are found.
fn collect_rollouts(dir: &Path, depth: usize, files: &mut Vec<PathBuf>) {
    if files.len() >= CODEX_SCAN_LIMIT {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    entries.sort_by(|a, b| b.cmp(a));
    for path in entries {
        if depth < 3 && path.is_dir() {
            collect_rollouts(&path, depth + 1, files);
        } else if depth == 3
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
        {
            files.push(path);
        }
        if files.len() >= CODEX_SCAN_LIMIT {
            return;
        }
    }
}

/// The directory a rollout was started in, from its first line alone, when
/// that line mentions `needle` at all.
fn codex_cwd(path: &Path, needle: &str) -> Option<String> {
    let mut first = String::new();
    BufReader::new(std::fs::File::open(path).ok()?)
        .read_line(&mut first)
        .ok()?;
    if !first.contains(needle) {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&first).ok()?;
    (value["type"] == "session_meta")
        .then(|| value["payload"]["cwd"].as_str().map(str::to_string))
        .flatten()
}

fn read_codex(path: &Path, replies: Replies) -> std::io::Result<Read> {
    let mut read = Read::default();
    for line in BufReader::new(std::fs::File::open(path)?).lines() {
        let line = line?;
        if replies == Replies::Skip
            && !line.contains(r#""type":"session_meta""#)
            && !line.contains(r#""role":"user""#)
        {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let payload = &value["payload"];
        match value["type"].as_str().unwrap_or_default() {
            "session_meta" if read.id.is_none() => {
                read.id = payload["id"].as_str().map(str::to_string);
                read.cwd = payload["cwd"].as_str().map(str::to_string);
            }
            "response_item" if payload["type"] == "message" => {
                let serde_json::Value::Array(blocks) = &payload["content"] else {
                    continue;
                };
                let at = timestamp(&value["timestamp"]);
                match payload["role"].as_str() {
                    Some("user") => {
                        let text = texts(blocks, "input_text");
                        if is_prompt(&text) {
                            read.messages.push(Message {
                                role: Role::User,
                                text,
                                at,
                            });
                        }
                    }
                    Some("assistant") => {
                        let text = texts(blocks, "output_text");
                        if !text.trim().is_empty() {
                            read.messages.push(Message {
                                role: Role::Agent,
                                text,
                                at,
                            });
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    Ok(read)
}

/// The text of every block of `kind`, joined.
fn texts(blocks: &[serde_json::Value], kind: &str) -> String {
    blocks
        .iter()
        .filter(|block| block["type"] == kind)
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Whether a user line is something the user typed. The CLIs put their own
/// context in user messages too — `<environment_context>`, `<command-name>`,
/// the `AGENTS.md` preamble — always as a leading tag or header.
fn is_prompt(text: &str) -> bool {
    let text = text.trim_start();
    !text.is_empty() && !text.starts_with('<') && !text.starts_with("# AGENTS.md")
}

fn timestamp(value: &serde_json::Value) -> i64 {
    value
        .as_str()
        .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
        .map_or(0, |time| time.timestamp())
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cut(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CWD: &str = "/work/my.app";

    fn write(path: &Path, lines: &[serde_json::Value]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        std::fs::write(path, body.join("\n") + "\n").unwrap();
    }

    fn claude_line(kind: &str, content: serde_json::Value, id: &str) -> serde_json::Value {
        serde_json::json!({
            "type": kind, "sessionId": "c-1", "cwd": CWD,
            "timestamp": "2026-09-26T01:00:00Z",
            "message": {"id": id, "role": kind, "content": content},
        })
    }

    fn claude_fixture(root: &Path) -> PathBuf {
        let path = root.join("projects/-work-my-app/c-1.jsonl");
        write(
            &path,
            &[
                claude_line(
                    "user",
                    serde_json::json!("<command-name>/clear</command-name>"),
                    "",
                ),
                claude_line("user", serde_json::json!("Fix the login bug"), ""),
                claude_line(
                    "assistant",
                    serde_json::json!([{"type": "thinking", "thinking": "hmm"}, {"type": "text", "text": "Looking."}]),
                    "m1",
                ),
                claude_line(
                    "assistant",
                    serde_json::json!([{"type": "tool_use", "name": "Read"}]),
                    "m1",
                ),
                claude_line(
                    "user",
                    serde_json::json!([{"type": "tool_result", "content": "file"}]),
                    "",
                ),
                claude_line(
                    "assistant",
                    serde_json::json!([{"type": "text", "text": "Fixed it."}]),
                    "m2",
                ),
                serde_json::json!({"type": "custom-title", "customTitle": "Login bug", "sessionId": "c-1"}),
                serde_json::json!({"type": "user", "isSidechain": true, "sessionId": "c-1", "cwd": CWD,
                    "message": {"role": "user", "content": "subagent prompt"}}),
            ],
        );
        path
    }

    fn codex_fixture(root: &Path, day: &str, id: &str, cwd: &str) -> PathBuf {
        let path = root.join(format!(
            "sessions/2026/09/{day}/rollout-2026-09-{day}T10-00-00-{id}.jsonl"
        ));
        let message = |role: &str, kind: &str, text: &str| {
            serde_json::json!({"timestamp": "2026-09-26T02:00:00Z", "type": "response_item",
                "payload": {"type": "message", "role": role, "content": [{"type": kind, "text": text}]}})
        };
        write(
            &path,
            &[
                serde_json::json!({"type": "session_meta", "payload": {"id": id, "cwd": cwd}}),
                message("user", "input_text", "# AGENTS.md instructions for /work"),
                message(
                    "user",
                    "input_text",
                    "<environment_context>...</environment_context>",
                ),
                message("user", "input_text", "Add a  dark\nmode"),
                serde_json::json!({"type": "response_item", "payload": {"type": "reasoning"}}),
                message("assistant", "output_text", "Done."),
            ],
        );
        path
    }

    #[test]
    fn claude_names_a_project_directory_after_its_path() {
        assert_eq!(
            claude_project_dir(Path::new("/Volumes/a.b/c_d")),
            "-Volumes-a-b-c-d"
        );
    }

    #[test]
    fn a_claude_conversation_is_listed_with_its_title_and_prompt_count() {
        let dir = tempfile::tempdir().unwrap();
        claude_fixture(dir.path());
        let roots = Roots {
            claude: Some(dir.path().to_path_buf()),
            codex: None,
        };
        let found = list(
            &mut Index::default(),
            &roots,
            Path::new(CWD),
            &HashSet::new(),
        );
        assert_eq!(found.len(), 1);
        let session = &found[0].session;
        assert_eq!(session.agent, "claude");
        assert_eq!(session.vendor_session_id, "c-1");
        assert_eq!(session.title, "Login bug");
        assert_eq!(
            session.prompts, 1,
            "tool results, commands and sidechains are not prompts"
        );
    }

    #[test]
    fn a_codex_conversation_is_found_by_the_directory_it_was_started_in() {
        let dir = tempfile::tempdir().unwrap();
        codex_fixture(dir.path(), "25", "x-1", CWD);
        codex_fixture(dir.path(), "26", "x-2", "/elsewhere");
        let roots = Roots {
            claude: None,
            codex: Some(dir.path().to_path_buf()),
        };
        let found = list(
            &mut Index::default(),
            &roots,
            Path::new(CWD),
            &HashSet::new(),
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].session.vendor_session_id, "x-1");
        assert_eq!(
            found[0].session.title, "Add a dark mode",
            "the opening prompt, on one line"
        );
        assert_eq!(
            found[0].session.prompts, 1,
            "the injected preamble is not a prompt"
        );
    }

    #[test]
    fn conversations_ginka_already_holds_are_not_offered() {
        let dir = tempfile::tempdir().unwrap();
        claude_fixture(dir.path());
        codex_fixture(dir.path(), "25", "x-1", CWD);
        let roots = Roots {
            claude: Some(dir.path().to_path_buf()),
            codex: Some(dir.path().to_path_buf()),
        };
        let known: HashSet<String> = ["c-1".to_string()].into();
        let found = list(&mut Index::default(), &roots, Path::new(CWD), &known);
        assert_eq!(
            found
                .iter()
                .map(|f| f.session.vendor_session_id.as_str())
                .collect::<Vec<_>>(),
            ["x-1"]
        );
    }

    #[test]
    fn a_file_recording_another_directory_is_never_offered() {
        let dir = tempfile::tempdir().unwrap();
        // Two paths can escape to the same directory name.
        let path = dir.path().join("projects/-work-my-app/c-9.jsonl");
        write(
            &path,
            &[
                serde_json::json!({"type": "user", "sessionId": "c-9", "cwd": "/work/my-app",
                "message": {"role": "user", "content": "hello"}}),
            ],
        );
        let roots = Roots {
            claude: Some(dir.path().to_path_buf()),
            codex: None,
        };
        assert!(
            list(
                &mut Index::default(),
                &roots,
                Path::new(CWD),
                &HashSet::new()
            )
            .is_empty()
        );
        assert!(
            find(
                &mut Index::default(),
                &roots,
                Path::new(CWD),
                "claude",
                "c-9"
            )
            .is_none()
        );
    }

    #[test]
    fn an_unchanged_file_is_not_read_again_and_a_changed_one_is() {
        let dir = tempfile::tempdir().unwrap();
        let path = claude_fixture(dir.path());
        let roots = Roots {
            claude: Some(dir.path().to_path_buf()),
            codex: None,
        };
        let mut index = Index::default();
        let prompts = |index: &mut Index| {
            list(index, &roots, Path::new(CWD), &HashSet::new())[0]
                .session
                .prompts
        };
        assert_eq!(prompts(&mut index), 1);
        // The CLI carries on in the terminal: the file grows.
        let mut more = std::fs::read_to_string(&path).unwrap();
        more.push_str(
            &claude_line("user", serde_json::json!("And the logout one"), "").to_string(),
        );
        more.push('\n');
        std::fs::write(&path, more).unwrap();
        assert_eq!(prompts(&mut index), 2);
        assert_eq!(index.seen.len(), 1);
    }

    #[test]
    fn find_looks_only_for_well_formed_ids_of_known_agents() {
        let dir = tempfile::tempdir().unwrap();
        claude_fixture(dir.path());
        let roots = Roots {
            claude: Some(dir.path().to_path_buf()),
            codex: None,
        };
        assert!(
            find(
                &mut Index::default(),
                &roots,
                Path::new(CWD),
                "claude",
                "c-1"
            )
            .is_some()
        );
        assert!(
            find(
                &mut Index::default(),
                &roots,
                Path::new(CWD),
                "claude",
                "../c-1"
            )
            .is_none()
        );
        assert!(
            find(
                &mut Index::default(),
                &roots,
                Path::new(CWD),
                "gemini",
                "c-1"
            )
            .is_none()
        );
    }

    #[test]
    fn an_adopted_claude_conversation_brings_prompts_and_replies_as_closed_turns() {
        let dir = tempfile::tempdir().unwrap();
        claude_fixture(dir.path());
        let roots = Roots {
            claude: Some(dir.path().to_path_buf()),
            codex: None,
        };
        let found = find(
            &mut Index::default(),
            &roots,
            Path::new(CWD),
            "claude",
            "c-1",
        )
        .unwrap();
        let payloads: Vec<TranscriptPayload> = transcript(&found)
            .unwrap()
            .into_iter()
            .map(|(_, p)| p)
            .collect();
        assert_eq!(
            payloads,
            [
                TranscriptPayload::User {
                    text: "Fix the login bug".into()
                },
                TranscriptPayload::Agent {
                    event: AgentEvent::TextDelta {
                        text: "Looking.".into()
                    }
                },
                TranscriptPayload::Agent {
                    event: AgentEvent::TextDelta {
                        text: "Fixed it.".into()
                    }
                },
                TranscriptPayload::Agent {
                    event: AgentEvent::TurnEnd { turn: 1 }
                },
            ]
        );
    }

    #[test]
    fn an_adopted_codex_conversation_keeps_its_timestamps() {
        let dir = tempfile::tempdir().unwrap();
        codex_fixture(dir.path(), "25", "x-1", CWD);
        let roots = Roots {
            claude: None,
            codex: Some(dir.path().to_path_buf()),
        };
        let found = find(
            &mut Index::default(),
            &roots,
            Path::new(CWD),
            "codex",
            "x-1",
        )
        .unwrap();
        let imported = transcript(&found).unwrap();
        assert_eq!(imported.len(), 3);
        assert_eq!(imported[0].0, 1_790_388_000);
        assert_eq!(
            imported[1].1,
            TranscriptPayload::Agent {
                event: AgentEvent::TextDelta {
                    text: "Done.".into()
                }
            }
        );
    }

    #[test]
    fn only_the_last_turns_are_imported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects/-work-my-app/c-2.jsonl");
        let lines: Vec<serde_json::Value> = (0..IMPORT_TURNS + 5)
            .flat_map(|i| {
                let mut prompt = claude_line("user", serde_json::json!(format!("prompt {i}")), "");
                prompt["sessionId"] = "c-2".into();
                let mut reply = claude_line(
                    "assistant",
                    serde_json::json!([{"type": "text", "text": format!("reply {i}")}]),
                    &format!("m{i}"),
                );
                reply["sessionId"] = "c-2".into();
                [prompt, reply]
            })
            .collect();
        write(&path, &lines);
        let roots = Roots {
            claude: Some(dir.path().to_path_buf()),
            codex: None,
        };
        let found = find(
            &mut Index::default(),
            &roots,
            Path::new(CWD),
            "claude",
            "c-2",
        )
        .unwrap();
        assert_eq!(
            found.session.prompts,
            u32::try_from(IMPORT_TURNS + 5).unwrap()
        );
        let imported = transcript(&found).unwrap();
        assert_eq!(imported.len(), IMPORT_TURNS * 3);
        assert_eq!(
            imported[0].1,
            TranscriptPayload::User {
                text: "prompt 5".into()
            }
        );
        assert_eq!(
            imported.last().unwrap().1,
            TranscriptPayload::Agent {
                event: AgentEvent::TurnEnd {
                    turn: u32::try_from(IMPORT_TURNS + 5).unwrap()
                }
            }
        );
    }
}
