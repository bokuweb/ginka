//! The on-disk session scanner: usage from agents run outside Ginka.
//!
//! Claude Code and Codex write every session they run to disk — Claude
//! Code under `~/.claude/projects`, Codex under `~/.codex/sessions` — with
//! the tokens each request used. An agent a reader ran in a terminal is
//! spending the same plan as one Ginka started, so Reports reads these too
//! (roadmap M5). Each file is read from a watermark — the byte after the last
//! whole line taken, and what the file had said by then about its session —
//! so a scan reads only what was appended since the last one, and a line
//! still being written is left for next time.

use super::pricing::TokenTotals;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Which agent wrote a log, and so how to read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// `~/.claude/projects/<cwd>/<session>.jsonl`.
    Claude,
    /// `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`.
    Codex,
}

impl Format {
    /// The driver id the usage is filed under.
    pub fn agent(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// One request's usage, as a log recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// Unique across every log: the same request read twice is one record.
    pub key: String,
    /// The driver id it is filed under: `claude` or `codex`.
    pub agent: &'static str,
    /// The vendor's id for the conversation — what Ginka stores as a
    /// session's `vendor_session_id` when it ran the conversation itself.
    pub vendor_session: String,
    /// Model id as the log recorded it; empty when the log never named one.
    pub model: String,
    /// Where the agent ran.
    pub cwd: PathBuf,
    /// That one request's tokens, not a running total.
    pub tokens: TokenTotals,
    /// Unix seconds.
    pub at: i64,
}

/// Where a file has been read to, and what it had said by then.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watermark {
    /// The byte after the last whole line read.
    pub offset: u64,
    /// The session the file is about, once it has said.
    pub session: Option<String>,
    /// The model in use, once it has said.
    pub model: Option<String>,
    /// Where the agent runs, once it has said.
    pub cwd: Option<String>,
}

/// Read what was appended to `path` since `from`, and where to start next.
///
/// A final line without its newline is still being written and is left for
/// the next scan. A file shorter than the watermark was replaced, and is
/// read again from the start.
pub fn read_file(
    path: &Path,
    format: Format,
    from: &Watermark,
) -> std::io::Result<(Vec<Record>, Watermark)> {
    use std::io::{Read as _, Seek as _};
    let mut file = std::fs::File::open(path)?;
    let length = file.metadata()?.len();
    let mut state = if length < from.offset {
        Watermark::default()
    } else {
        from.clone()
    };
    file.seek(std::io::SeekFrom::Start(state.offset))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    // Only whole lines: everything up to the last newline.
    let whole = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |at| at + 1);
    let mut records = Vec::new();
    for line in String::from_utf8_lossy(&bytes[..whole]).lines() {
        records.extend(read_line(format, line, path, &mut state));
    }
    state.offset += whole as u64;
    Ok((records, state))
}

/// The records one line holds, updating what the file has said so far.
pub fn read_line(format: Format, line: &str, file: &Path, state: &mut Watermark) -> Option<Record> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    match format {
        Format::Claude => claude_line(&value, file),
        Format::Codex => codex_line(&value, state),
    }
}

fn claude_line(value: &Value, file: &Path) -> Option<Record> {
    if value.get("type")?.as_str()? != "assistant" {
        return None;
    }
    let message = value.get("message")?;
    let usage = message.get("usage")?;
    let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    // A session id the line leaves out is the file's name, which is it.
    let session = value
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| Some(file.file_stem()?.to_string_lossy().to_string()))?;
    let tokens = TokenTotals {
        input: count("input_tokens"),
        output: count("output_tokens"),
        cache_read: count("cache_read_input_tokens"),
        cache_write: count("cache_creation_input_tokens"),
    };
    // A request that used nothing — synthetic, or cancelled before it ran —
    // is not one worth a row.
    if tokens.total() == 0 {
        return None;
    }
    Some(Record {
        key: format!("claude:{}", message.get("id")?.as_str()?),
        agent: Format::Claude.agent(),
        vendor_session: session,
        model: message
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        cwd: PathBuf::from(value.get("cwd").and_then(Value::as_str).unwrap_or_default()),
        tokens,
        at: timestamp(value)?,
    })
}

fn codex_line(value: &Value, state: &mut Watermark) -> Option<Record> {
    let payload = value.get("payload")?;
    let text = |key: &str| payload.get(key).and_then(Value::as_str).map(str::to_string);
    match value.get("type")?.as_str()? {
        "session_meta" => {
            state.session = text("id").or_else(|| text("session_id"));
            state.cwd = text("cwd").or(state.cwd.take());
            None
        }
        "turn_context" => {
            state.model = text("model").or(state.model.take());
            state.cwd = text("cwd").or(state.cwd.take());
            None
        }
        "event_msg" if payload.get("type")?.as_str()? == "token_count" => {
            let usage = payload.get("info")?.get("last_token_usage")?;
            let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
            let cached = count("cached_input_tokens");
            let session = state.session.clone()?;
            let stamp = value.get("timestamp")?.as_str()?;
            if count("input_tokens") + count("output_tokens") == 0 {
                return None;
            }
            Some(Record {
                key: format!("codex:{session}:{stamp}"),
                agent: Format::Codex.agent(),
                vendor_session: session,
                model: state.model.clone().unwrap_or_default(),
                cwd: PathBuf::from(state.cwd.clone().unwrap_or_default()),
                tokens: TokenTotals {
                    // Codex counts cached input inside input.
                    input: count("input_tokens").saturating_sub(cached),
                    output: count("output_tokens"),
                    cache_read: cached,
                    cache_write: count("cache_write_input_tokens"),
                },
                at: timestamp(value)?,
            })
        }
        _ => None,
    }
}

fn timestamp(value: &Value) -> Option<i64> {
    let text = value.get("timestamp")?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|at| at.timestamp())
}

/// Read every log under `roots` from its watermark in `marks` (keyed by the
/// file's path). Returns what was read and the watermarks that moved. A log
/// that cannot be read is skipped and tried again next time.
pub fn collect(
    roots: &[(PathBuf, Format)],
    marks: &std::collections::HashMap<String, Watermark>,
) -> (Vec<Record>, Vec<(String, Watermark)>) {
    let mut records = Vec::new();
    let mut moved = Vec::new();
    for (root, format) in roots {
        for file in files(root, *format) {
            let key = file.to_string_lossy().to_string();
            let from = marks.get(&key).cloned().unwrap_or_default();
            match read_file(&file, *format, &from) {
                Ok((read, mark)) => {
                    records.extend(read);
                    if mark != from {
                        moved.push((key, mark));
                    }
                }
                Err(error) => {
                    tracing::debug!(file = %file.display(), %error, "could not read a vendor log")
                }
            }
        }
    }
    (records, moved)
}

/// Every log file under `root` of `format`, sorted.
pub fn files(root: &Path, format: Format) -> Vec<PathBuf> {
    // Claude keeps a directory per working directory; Codex nests by date.
    let depth = match format {
        Format::Claude => 1,
        Format::Codex => 3,
    };
    let mut found = Vec::new();
    walk(root, depth, &mut found);
    found.sort();
    found
}

fn walk(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if depth > 0 {
            if path.is_dir() {
                walk(&path, depth - 1, found);
            }
        } else if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            found.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE_TURN: &str = r#"{"type":"assistant","sessionId":"s-claude","cwd":"/work/comet","timestamp":"2026-09-23T15:25:20.005Z","message":{"id":"msg_1","model":"claude-opus-5-5","usage":{"input_tokens":2,"cache_creation_input_tokens":300,"cache_read_input_tokens":400,"output_tokens":50}}}"#;

    #[test]
    fn a_claude_request_is_read_once_however_many_lines_repeat_it() {
        let file = Path::new("/logs/a.jsonl");
        let mut state = Watermark::default();
        let record = read_line(Format::Claude, CLAUDE_TURN, file, &mut state).unwrap();
        assert_eq!(record.agent, "claude");
        assert_eq!(record.vendor_session, "s-claude");
        assert_eq!(record.model, "claude-opus-5-5");
        assert_eq!(record.cwd, PathBuf::from("/work/comet"));
        assert_eq!(
            record.tokens,
            TokenTotals {
                input: 2,
                output: 50,
                cache_read: 400,
                cache_write: 300,
            }
        );
        assert_eq!(record.at, 1_790_177_120);
        assert_eq!(record.key, "claude:msg_1");
        // A request that used nothing — a synthetic or cancelled one — is
        // not a request worth a row.
        let empty = CLAUDE_TURN
            .replace("\"input_tokens\":2", "\"input_tokens\":0")
            .replace(
                "\"cache_creation_input_tokens\":300",
                "\"cache_creation_input_tokens\":0",
            )
            .replace(
                "\"cache_read_input_tokens\":400",
                "\"cache_read_input_tokens\":0",
            )
            .replace("\"output_tokens\":50", "\"output_tokens\":0");
        assert!(read_line(Format::Claude, &empty, file, &mut state).is_none());
        // A user line, or one with no usage, is nothing.
        assert!(
            read_line(
                Format::Claude,
                r#"{"type":"user","message":{}}"#,
                file,
                &mut state
            )
            .is_none()
        );
        assert!(read_line(Format::Claude, "not json", file, &mut state).is_none());
    }

    #[test]
    fn a_codex_request_takes_its_session_and_model_from_earlier_lines() {
        let file = Path::new("/logs/rollout.jsonl");
        let mut state = Watermark::default();
        let meta = r#"{"timestamp":"2026-09-24T14:07:01.000Z","type":"session_meta","payload":{"id":"thread-1","cwd":"/work/comet"}}"#;
        let context = r#"{"timestamp":"2026-09-24T14:07:02.000Z","type":"turn_context","payload":{"model":"gpt-6-sol","cwd":"/work/comet"}}"#;
        let count = r#"{"timestamp":"2026-09-24T14:07:05.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":900,"cached_input_tokens":800,"output_tokens":70},"last_token_usage":{"input_tokens":500,"cached_input_tokens":300,"output_tokens":40}}}}"#;
        assert!(read_line(Format::Codex, meta, file, &mut state).is_none());
        assert!(read_line(Format::Codex, context, file, &mut state).is_none());
        let record = read_line(Format::Codex, count, file, &mut state).unwrap();
        assert_eq!(record.vendor_session, "thread-1");
        assert_eq!(record.model, "gpt-6-sol");
        // Codex counts cached input inside input; ours keeps them apart.
        assert_eq!(
            record.tokens,
            TokenTotals {
                input: 200,
                output: 40,
                cache_read: 300,
                cache_write: 0,
            }
        );
        assert_eq!(record.key, "codex:thread-1:2026-09-24T14:07:05.000Z");
        // A count with no usage in it — the rate-limit-only kind — is nothing.
        let empty = r#"{"timestamp":"2026-09-24T14:07:06.000Z","type":"event_msg","payload":{"type":"token_count","info":null}}"#;
        assert!(read_line(Format::Codex, empty, file, &mut state).is_none());
    }

    #[test]
    fn a_file_is_read_from_its_watermark_and_a_half_written_line_waits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, format!("{CLAUDE_TURN}\n")).unwrap();
        let (first, mark) = read_file(&path, Format::Claude, &Watermark::default()).unwrap();
        assert_eq!(first.len(), 1);

        // More arrives, the last line not yet finished.
        let second = CLAUDE_TURN.replace("msg_1", "msg_2");
        let third = CLAUDE_TURN.replace("msg_1", "msg_3");
        let partial = &third[..20];
        std::fs::write(&path, format!("{CLAUDE_TURN}\n{second}\n{partial}")).unwrap();
        let (more, mark) = read_file(&path, Format::Claude, &mark).unwrap();
        assert_eq!(more.len(), 1, "only the finished line");
        assert_eq!(more[0].key, "claude:msg_2");

        std::fs::write(&path, format!("{CLAUDE_TURN}\n{second}\n{third}\n")).unwrap();
        let (last, mark) = read_file(&path, Format::Claude, &mark).unwrap();
        assert_eq!(
            last.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
            ["claude:msg_3"]
        );

        // Replaced by something shorter: read again from the start.
        std::fs::write(&path, format!("{second}\n")).unwrap();
        let (again, _) = read_file(&path, Format::Claude, &mark).unwrap();
        assert_eq!(again[0].key, "claude:msg_2");
    }

    #[test]
    fn the_logs_are_found_where_each_agent_keeps_them() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("projects/-work-comet");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(claude.join("b.jsonl"), "").unwrap();
        std::fs::write(claude.join("a.jsonl"), "").unwrap();
        std::fs::write(claude.join("notes.txt"), "").unwrap();
        let found = files(&dir.path().join("projects"), Format::Claude);
        assert_eq!(found, [claude.join("a.jsonl"), claude.join("b.jsonl")]);

        let codex = dir.path().join("sessions/2026/09/24");
        std::fs::create_dir_all(&codex).unwrap();
        std::fs::write(codex.join("rollout-1.jsonl"), "").unwrap();
        assert_eq!(
            files(&dir.path().join("sessions"), Format::Codex),
            [codex.join("rollout-1.jsonl")]
        );
        assert!(files(&dir.path().join("missing"), Format::Codex).is_empty());
    }
}
