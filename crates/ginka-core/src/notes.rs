//! Markdown notes, kept by the daemon.
//!
//! MonoCode's notebook (`docs/monocode-parity.md`): what a session taught the reader
//! that is not code — a command that finally worked, why a design was turned
//! down. The daemon keeps them because it keeps everything else (rule 1): a
//! note written in the window is one `ginka notes` can print and an agent can
//! read over MCP.

use anyhow::{Result, bail};
use ginka_protocol::ProjectName;
use ginka_protocol::model::Note;
use rusqlite::{Connection, OptionalExtension as _};

/// What a note with no title is called: its first line, cut to this many
/// characters. A list of untitled rows is a list nobody can find anything in.
const TITLE_FROM_BODY: usize = 60;

/// The title to store: the one given, or the body's first non-empty line.
pub fn title_for(title: &str, body: &str) -> String {
    let title = title.trim();
    if !title.is_empty() {
        return title.to_string();
    }
    body.lines()
        .map(|line| line.trim().trim_start_matches('#').trim())
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(TITLE_FROM_BODY).collect())
        .unwrap_or_default()
}

/// Write a note: a new one without an `id`, the one that has it otherwise.
pub fn save(
    conn: &Connection,
    id: Option<&str>,
    project: Option<&ProjectName>,
    title: &str,
    body: &str,
    now: i64,
) -> Result<Note> {
    let title = title_for(title, body);
    match id {
        Some(id) => {
            let changed = conn.execute(
                "UPDATE notes SET title = ?2, body = ?3, updated_at = ?4 WHERE id = ?1",
                rusqlite::params![id, title, body, now],
            )?;
            if changed == 0 {
                bail!("no note with id {id}");
            }
            get(conn, id)?.ok_or_else(|| anyhow::anyhow!("no note with id {id}"))
        }
        None => {
            let note = Note {
                id: uuid::Uuid::new_v4().simple().to_string(),
                project: project.cloned(),
                title,
                body: body.to_string(),
                created_at: now,
                updated_at: now,
            };
            conn.execute(
                "INSERT INTO notes (id, project, title, body, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    note.id,
                    note.project.as_ref().map(|project| project.0.clone()),
                    note.title,
                    note.body,
                    note.created_at,
                    note.updated_at,
                ],
            )?;
            Ok(note)
        }
    }
}

/// One note.
pub fn get(conn: &Connection, id: &str) -> Result<Option<Note>> {
    Ok(conn
        .query_row(
            "SELECT id, project, title, body, created_at, updated_at FROM notes WHERE id = ?1",
            [id],
            row_to_note,
        )
        .optional()?)
}

/// A project's notes, or all of them, most recently touched first.
pub fn list(conn: &Connection, project: Option<&ProjectName>) -> Result<Vec<Note>> {
    let mut statement = conn.prepare(
        "SELECT id, project, title, body, created_at, updated_at
           FROM notes
          WHERE ?1 IS NULL OR project = ?1
          ORDER BY updated_at DESC, created_at DESC",
    )?;
    let notes = statement
        .query_map([project.map(|project| project.0.clone())], row_to_note)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(notes)
}

/// Forget a note. Forgetting one that is not there is not an error: the
/// reader wanted it gone, and it is.
pub fn remove(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM notes WHERE id = ?1", [id])?;
    Ok(())
}

fn row_to_note(row: &rusqlite::Row<'_>) -> rusqlite::Result<Note> {
    Ok(Note {
        id: row.get(0)?,
        project: row.get::<_, Option<String>>(1)?.map(ProjectName),
        title: row.get(2)?,
        body: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("ginka.db")).unwrap();
        // The file stays open after the directory handle goes; SQLite keeps it.
        std::mem::forget(dir);
        conn
    }

    #[test]
    fn an_untitled_note_is_called_by_its_first_line() {
        assert_eq!(title_for("", "\n# Release steps\nrun it"), "Release steps");
        assert_eq!(title_for("  Mine ", "# Other"), "Mine");
        assert_eq!(title_for("", ""), "");
    }

    #[test]
    fn a_note_is_saved_listed_edited_and_removed() {
        let conn = conn();
        let project = ProjectName("ginka".into());
        let first = save(&conn, None, Some(&project), "One", "body", 10).unwrap();
        let other = save(&conn, None, None, "", "loose", 20).unwrap();
        assert_eq!(list(&conn, None).unwrap().len(), 2);
        assert_eq!(list(&conn, Some(&project)).unwrap(), vec![first.clone()]);

        let edited = save(&conn, Some(&first.id), None, "One", "changed", 30).unwrap();
        assert_eq!(edited.body, "changed");
        assert_eq!(edited.project, Some(project), "an edit does not move it");
        assert_eq!(
            list(&conn, None).unwrap()[0].id,
            first.id,
            "the one just touched comes first"
        );

        remove(&conn, &other.id).unwrap();
        remove(&conn, &other.id).unwrap();
        assert_eq!(list(&conn, None).unwrap().len(), 1);
    }

    #[test]
    fn editing_a_note_that_is_not_there_says_so() {
        assert!(save(&conn(), Some("missing"), None, "t", "b", 1).is_err());
    }
}
