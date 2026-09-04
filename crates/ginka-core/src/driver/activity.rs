//! Turning a vendor's tool call into the one shape the transcript renders.
//!
//! Every provider names its tools differently and puts the interesting part of
//! the call in a different argument. Normalizing here is what lets the
//! transcript stay provider-agnostic (`AGENTS.md` rule 6) — and what stops
//! every view from growing a match on tool names.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What kind of work a tool call is, as far as a reader cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    /// The agent thinking out loud.
    Reasoning,
    /// A command in a shell.
    Command,
    /// A change to a file.
    FileChange,
    /// Looking for something.
    Search,
    /// A plan or todo list.
    Plan,
    /// Anything else the agent can call.
    Tool,
}

/// One tool call, and its result once it arrives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityItem {
    /// The provider's own id for the call, which is how a result finds the
    /// call it belongs to. `None` where a provider does not give one.
    pub id: Option<String>,
    pub kind: ActivityKind,
    /// One line, always non-empty: this is the row a reader scans.
    pub title: String,
    /// The result, once there is one.
    pub detail: Option<String>,
    pub failed: bool,
    pub complete: bool,
}

impl ActivityItem {
    /// How much of a tool's output is kept. Enough to see what happened,
    /// bounded so one `yes` cannot own the transcript or the database.
    pub const MAX_DETAIL_BYTES: usize = 16 * 1024;

    /// Normalize a call from the tool's name and its arguments.
    pub fn from_tool(id: Option<String>, tool: &str, input: &Value) -> Self {
        let kind = kind_of(tool);
        Self {
            id,
            kind,
            title: title_of(kind, tool, input),
            detail: None,
            failed: false,
            complete: false,
        }
    }

    /// A free-form activity that is not a tool call — reasoning, mostly.
    pub fn note(kind: ActivityKind, title: impl Into<String>) -> Self {
        Self {
            id: None,
            kind,
            title: single_line(&title.into()),
            detail: None,
            failed: false,
            complete: true,
        }
    }

    /// Attach the result. The title is never rewritten: a row that renames
    /// itself when its result lands is a row nobody can follow.
    pub fn complete_with(&mut self, output: &str, failed: bool) {
        self.detail = Some(truncate(output, Self::MAX_DETAIL_BYTES));
        self.failed = failed;
        self.complete = true;
    }
}

fn kind_of(tool: &str) -> ActivityKind {
    match base_name(tool).to_ascii_lowercase().as_str() {
        "bash" | "shell" | "run" | "exec" | "terminal" => ActivityKind::Command,
        "edit" | "write" | "multiedit" | "apply_patch" | "applypatch" | "str_replace" => {
            ActivityKind::FileChange
        }
        "grep" | "glob" | "search" | "find" | "codebase_search" => ActivityKind::Search,
        "todowrite" | "todo_write" | "plan" | "update_plan" | "exit_plan_mode" => {
            ActivityKind::Plan
        }
        _ => ActivityKind::Tool,
    }
}

/// The first argument that says something a reader wants.
///
/// A name the agent gave the call always wins. After that the order depends on
/// the kind, because the interesting argument does: a search is about its
/// pattern and only incidentally about the directory it ran in, while a file
/// tool is the other way round.
fn title_of(kind: ActivityKind, tool: &str, input: &Value) -> String {
    let subject: &[&str] = match kind {
        ActivityKind::Command => &["command"],
        ActivityKind::Search => &["pattern", "query", "regex", "path"],
        ActivityKind::FileChange => &["file_path", "path", "notebook_path"],
        _ => &["file_path", "path", "query", "pattern", "url", "command"],
    };
    for key in ["title", "description"].iter().chain(subject) {
        if let Some(text) = input.get(key).and_then(Value::as_str) {
            let line = single_line(text);
            if !line.is_empty() {
                return line;
            }
        }
    }
    let readable = humanize(base_name(tool));
    if readable.is_empty() {
        "Tool call".to_string()
    } else {
        readable
    }
}

/// MCP tools arrive namespaced (`mcp__server__do_thing`); the last segment is
/// the part that names the work.
fn base_name(tool: &str) -> &str {
    tool.rsplit("__").next().unwrap_or(tool)
}

/// `WebFetch` and `create_issue` both become "Web fetch"-shaped prose.
fn humanize(tool: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    for part in tool.split(['_', '-']).filter(|part| !part.is_empty()) {
        let mut current = String::new();
        for character in part.chars() {
            if character.is_uppercase() && !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            current.push(character);
        }
        if !current.is_empty() {
            words.push(current);
        }
    }
    let mut sentence = words.join(" ").to_ascii_lowercase();
    if let Some(first) = sentence.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    sentence
}

/// Titles are one row high. Whatever the argument contained, this is what has
/// to fit on that row.
fn single_line(text: &str) -> String {
    text.split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

fn truncate(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut cut = max_bytes;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}… truncated", &text[..cut])
}
