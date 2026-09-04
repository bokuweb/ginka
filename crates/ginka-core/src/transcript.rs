//! Stored sessions and their messages, and search over them.
//!
//! Search runs here rather than in a client because the client does not hold
//! the messages: a session runs for hours, the transcript is paged, and
//! pulling it all back to grep it would undo the virtualization the
//! performance budget depends on (`docs/roadmap.md` §6.2, §3.3 N10).

use anyhow::{Context, Result};
use chrono::Utc;
use ginka_protocol::WorkspaceId;
use ginka_protocol::provider::ProviderKind;
use ginka_protocol::session::SessionTitle;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// Matches one search returns. A person reads these; past a couple of hundred
/// the answer is "refine the query", not "scroll".
pub const SEARCH_RESULT_CAP: usize = 200;

/// Characters of context kept on each side of a match in a snippet.
const SNIPPET_LEADING_CHARS: usize = 60;
const SNIPPET_TRAILING_CHARS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Agent,
    /// Ours: a note the app itself put in the transcript.
    System,
}

impl MessageRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::System => "system",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "agent" => Self::Agent,
            "system" => Self::System,
            _ => Self::User,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub workspace: WorkspaceId,
    pub provider: ProviderKind,
    pub title: SessionTitle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMessage {
    pub session: String,
    pub role: MessageRole,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub session: String,
    pub seq: i64,
    pub role: MessageRole,
    pub text: String,
    pub created_at: i64,
}

/// Where a search looks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchScope {
    Everywhere,
    Session(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub session: String,
    pub seq: i64,
    pub role: MessageRole,
    pub text: String,
    /// The match with enough around it to recognise, elided on both sides.
    pub snippet: String,
}

pub fn upsert_session(conn: &Connection, session: &Session) -> Result<()> {
    let now = Utc::now().timestamp();
    conn.execute(
        "INSERT INTO sessions (
             id, workspace_id, provider, user_title, agent_title,
             agent_title_is_placeholder, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
         ON CONFLICT(id) DO UPDATE SET
             workspace_id = excluded.workspace_id,
             provider = excluded.provider,
             user_title = excluded.user_title,
             agent_title = excluded.agent_title,
             agent_title_is_placeholder = excluded.agent_title_is_placeholder,
             updated_at = excluded.updated_at",
        params![
            session.id,
            session.workspace.0,
            session.provider.as_str(),
            session.title.user_title(),
            session.title.agent_title(),
            session.title.agent_title_is_placeholder(),
            now,
        ],
    )
    .context("storing a session")?;
    Ok(())
}

pub fn session(conn: &Connection, id: &str) -> Result<Option<Session>> {
    let found = conn
        .query_row(
            "SELECT id, workspace_id, provider, user_title, agent_title,
                    agent_title_is_placeholder
             FROM sessions WHERE id = ?1",
            [id],
            read_session,
        )
        .optional()?;
    Ok(found)
}

pub fn sessions(conn: &Connection) -> Result<Vec<Session>> {
    let mut statement = conn.prepare(
        "SELECT id, workspace_id, provider, user_title, agent_title,
                agent_title_is_placeholder
         FROM sessions ORDER BY updated_at DESC, id",
    )?;
    let rows = statement.query_map([], read_session)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn delete_session(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM sessions WHERE id = ?1", [id])?;
    Ok(())
}

/// Append a message, returning its per-session sequence number.
pub fn append_message(conn: &Connection, message: &NewMessage) -> Result<i64> {
    let now = Utc::now().timestamp();
    conn.execute(
        "INSERT INTO messages (session_id, seq, role, text, created_at)
         SELECT ?1, COALESCE(MAX(seq), 0) + 1, ?2, ?3, ?4
         FROM messages WHERE session_id = ?1",
        params![message.session, message.role.as_str(), message.text, now],
    )
    .context("appending a message")?;
    let seq = conn.query_row(
        "SELECT seq FROM messages WHERE id = ?1",
        [conn.last_insert_rowid()],
        |row| row.get(0),
    )?;
    Ok(seq)
}

/// One page of a transcript, oldest first.
///
/// `before` walks backwards from the end, which is the direction a transcript
/// is actually read: it opens at the newest message and loads history as the
/// user scrolls up.
pub fn messages(
    conn: &Connection,
    session: &str,
    limit: usize,
    before: Option<i64>,
) -> Result<Vec<Message>> {
    let mut statement = conn.prepare(
        "SELECT session_id, seq, role, text, created_at
         FROM messages
         WHERE session_id = ?1 AND (?2 IS NULL OR seq < ?2)
         ORDER BY seq DESC LIMIT ?3",
    )?;
    let rows = statement.query_map(params![session, before, limit as i64], |row| {
        Ok(Message {
            session: row.get(0)?,
            seq: row.get(1)?,
            role: MessageRole::parse(&row.get::<_, String>(2)?),
            text: row.get(3)?,
            created_at: row.get(4)?,
        })
    })?;
    let mut page: Vec<Message> = rows.collect::<Result<_, _>>()?;
    page.reverse();
    Ok(page)
}

/// Find messages containing `query`, newest first.
pub fn search(conn: &Connection, query: &str, scope: SearchScope) -> Result<Vec<SearchHit>> {
    let needle = query.trim();
    if needle.is_empty() {
        // An empty query means "no question asked", not "return the corpus".
        return Ok(Vec::new());
    }
    let pattern = format!("%{}%", escape_like(needle));
    let session_filter = match &scope {
        SearchScope::Everywhere => None,
        SearchScope::Session(id) => Some(id.as_str()),
    };

    let mut statement = conn.prepare(
        r"SELECT session_id, seq, role, text
          FROM messages
          WHERE text LIKE ?1 ESCAPE '\' AND (?2 IS NULL OR session_id = ?2)
          ORDER BY id DESC LIMIT ?3",
    )?;
    let rows = statement.query_map(
        params![pattern, session_filter, SEARCH_RESULT_CAP as i64],
        |row| {
            let text: String = row.get(3)?;
            Ok(SearchHit {
                session: row.get(0)?,
                seq: row.get(1)?,
                role: MessageRole::parse(&row.get::<_, String>(2)?),
                snippet: snippet(&text, needle),
                text,
            })
        },
    )?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn read_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: row.get(0)?,
        workspace: WorkspaceId(row.get(1)?),
        provider: ProviderKind::parse(&row.get::<_, String>(2)?).unwrap_or(ProviderKind::Claude),
        title: SessionTitle::from_parts(row.get(3)?, row.get(4)?, row.get(5)?),
    })
}

/// `%` and `_` are wildcards in `LIKE`; a user searching for "100%" means the
/// character, and without this would match every message ever stored.
fn escape_like(query: &str) -> String {
    let mut escaped = String::with_capacity(query.len());
    for character in query.chars() {
        if matches!(character, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Cut a readable window around the first match. Always on character
/// boundaries: a message is as likely to be Japanese as English.
fn snippet(text: &str, needle: &str) -> String {
    let Some(match_start) = find_ignoring_case(text, needle) else {
        return text.chars().take(SNIPPET_TRAILING_CHARS).collect();
    };
    let match_end = match_start + needle.chars().count();

    let start = match_start.saturating_sub(SNIPPET_LEADING_CHARS);
    let end = (match_end + SNIPPET_TRAILING_CHARS).min(text.chars().count());

    let body: String = text
        .chars()
        .skip(start)
        .take(end - start)
        .collect::<String>()
        .replace('\n', " ");

    let mut snippet = String::new();
    if start > 0 {
        snippet.push('…');
    }
    snippet.push_str(body.trim());
    if end < text.chars().count() {
        snippet.push('…');
    }
    snippet
}

/// The character index of the first case-insensitive match.
fn find_ignoring_case(haystack: &str, needle: &str) -> Option<usize> {
    let haystack: Vec<char> = haystack.chars().collect();
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&start| {
        haystack[start..start + needle.len()]
            .iter()
            .zip(&needle)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
    })
}
