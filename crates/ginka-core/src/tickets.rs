//! Tickets: work an agent noticed and handed to the reader.
//!
//! An agent asked to fix one thing keeps finding others — dead code, a stale
//! doc, a missing test. Doing them bloats the change it was asked for;
//! mentioning them in prose loses them. A ticket is the third way: a card the
//! reader sees beside the composer and starts in a session of its own with
//! one click, or dismisses. The daemon keeps them (rule 1), so the window,
//! `ginka tickets` and the MCP tools see the same ones.

use anyhow::{Result, bail};
use ginka_protocol::model::{Ticket, TicketState};
use ginka_protocol::{SessionId, WorkspaceId};
use rusqlite::{Connection, OptionalExtension as _};

/// The longest title kept; a card heading is a line, not a paragraph.
const MAX_TITLE: usize = 80;

/// Everything a new ticket is raised with.
pub struct NewTicket<'a> {
    /// Where it was raised, and where it runs unless a branch is asked for.
    pub workspace: &'a WorkspaceId,
    /// The session that raised it, when an agent did; `None` for a person.
    pub from_session: Option<&'a SessionId>,
    /// The card's heading; blank takes the prompt's first line. Cut to 80
    /// characters.
    pub title: &'a str,
    /// One or two sentences for the card: why now, and what it will do. Trimmed.
    pub summary: &'a str,
    /// What the new session is told, self-contained. Must not be blank:
    /// [`raise`] refuses it.
    pub prompt: &'a str,
}

/// Record a new open ticket.
pub fn raise(conn: &Connection, new: NewTicket<'_>, now: i64) -> Result<Ticket> {
    let prompt = new.prompt.trim();
    if prompt.is_empty() {
        bail!("a ticket needs a prompt: it is all the new session will be told");
    }
    let title = match new.title.trim() {
        "" => prompt.lines().next().unwrap_or_default().trim(),
        title => title,
    };
    let ticket = Ticket {
        id: uuid::Uuid::new_v4().simple().to_string(),
        workspace: new.workspace.clone(),
        from_session: new.from_session.cloned(),
        title: title.chars().take(MAX_TITLE).collect(),
        summary: new.summary.trim().to_string(),
        prompt: prompt.to_string(),
        state: TicketState::Open,
        session: None,
        created_at: now,
        updated_at: now,
    };
    conn.execute(
        "INSERT INTO tickets (id, workspace, from_session, title, summary, prompt, state,
                              session, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?8)",
        rusqlite::params![
            ticket.id,
            ticket.workspace.0,
            ticket
                .from_session
                .as_ref()
                .map(|session| session.0.clone()),
            ticket.title,
            ticket.summary,
            ticket.prompt,
            ticket.state.as_str(),
            now,
        ],
    )?;
    Ok(ticket)
}

/// One ticket.
pub fn get(conn: &Connection, id: &str) -> Result<Option<Ticket>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM tickets WHERE id = ?1"),
            [id],
            row_to_ticket,
        )
        .optional()?)
}

/// A workspace's tickets, or every one, newest first; open ones only unless
/// `all`.
pub fn list(conn: &Connection, workspace: Option<&WorkspaceId>, all: bool) -> Result<Vec<Ticket>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS}
           FROM tickets
          WHERE (?1 IS NULL OR workspace = ?1)
            AND (?2 OR state = 'open')
          ORDER BY created_at DESC, rowid DESC"
    ))?;
    let tickets = statement
        .query_map(
            rusqlite::params![workspace.map(|workspace| workspace.0.clone()), all],
            row_to_ticket,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(tickets)
}

/// Move an open ticket on: to started with the session that took it, or to
/// dismissed. A ticket that already moved says where it went instead, so two
/// windows pressing *Start* at once cannot start it twice.
pub fn close(
    conn: &Connection,
    id: &str,
    state: TicketState,
    session: Option<&SessionId>,
    now: i64,
) -> Result<Ticket> {
    let changed = conn.execute(
        "UPDATE tickets SET state = ?2, session = ?3, updated_at = ?4
          WHERE id = ?1 AND state = 'open'",
        rusqlite::params![
            id,
            state.as_str(),
            session.map(|session| session.0.clone()),
            now
        ],
    )?;
    let ticket = get(conn, id)?.ok_or_else(|| anyhow::anyhow!("no ticket with id {id}"))?;
    if changed == 0 {
        bail!("{}", already(&ticket));
    }
    Ok(ticket)
}

/// Why an open-only operation was refused.
pub fn already(ticket: &Ticket) -> String {
    match (&ticket.state, &ticket.session) {
        (TicketState::Started, Some(session)) => {
            format!(
                "ticket {} was already started in session {session}",
                ticket.id
            )
        }
        (TicketState::Dismissed, _) => format!("ticket {} was dismissed", ticket.id),
        _ => format!("ticket {} is no longer open", ticket.id),
    }
}

const COLUMNS: &str =
    "id, workspace, from_session, title, summary, prompt, state, session, created_at, updated_at";

fn row_to_ticket(row: &rusqlite::Row<'_>) -> rusqlite::Result<Ticket> {
    let state: String = row.get(6)?;
    Ok(Ticket {
        id: row.get(0)?,
        workspace: WorkspaceId(row.get(1)?),
        from_session: row.get::<_, Option<String>>(2)?.map(SessionId),
        title: row.get(3)?,
        summary: row.get(4)?,
        prompt: row.get(5)?,
        state: TicketState::parse(&state).unwrap_or(TicketState::Open),
        session: row.get::<_, Option<String>>(7)?.map(SessionId),
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("ginka.db")).unwrap();
        std::mem::forget(dir);
        conn
    }

    fn new<'a>(workspace: &'a WorkspaceId, title: &'a str, prompt: &'a str) -> NewTicket<'a> {
        NewTicket {
            workspace,
            from_session: None,
            title,
            summary: "",
            prompt,
        }
    }

    #[test]
    fn a_ticket_is_raised_listed_and_started_once() {
        let conn = conn();
        let here = WorkspaceId("comet/harbor".into());
        let there = WorkspaceId("comet/other".into());
        let first = raise(&conn, new(&here, "Remove dead code", "Delete foo()"), 10).unwrap();
        let second = raise(&conn, new(&there, "", "Fix the README\nIt is stale"), 20).unwrap();
        assert_eq!(
            second.title, "Fix the README",
            "untitled takes the first line"
        );
        assert_eq!(list(&conn, None, false).unwrap().len(), 2);
        assert_eq!(
            list(&conn, Some(&here), false).unwrap(),
            vec![first.clone()]
        );

        let session = SessionId("s1".into());
        let started = close(&conn, &first.id, TicketState::Started, Some(&session), 30).unwrap();
        assert_eq!(started.session, Some(session));
        assert!(list(&conn, Some(&here), false).unwrap().is_empty());
        assert_eq!(list(&conn, Some(&here), true).unwrap().len(), 1);

        let twice = close(&conn, &first.id, TicketState::Dismissed, None, 40).unwrap_err();
        assert!(twice.to_string().contains("already started in session s1"));
    }

    #[test]
    fn a_ticket_without_a_prompt_is_refused() {
        let here = WorkspaceId("comet/harbor".into());
        assert!(raise(&conn(), new(&here, "t", "  "), 1).is_err());
    }
}
