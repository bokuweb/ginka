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
use rusqlite::{Connection, OptionalExtension as _, types::Type};

/// A note search shared by the daemon and the window.
///
/// Text matches title, body or any tag as a case-insensitive substring;
/// the optional tag matches one normalized tag exactly.
pub struct NoteFilter {
    query: Option<String>,
    tag: Option<String>,
}

impl NoteFilter {
    /// Trim and case-fold both conditions; blank conditions do not filter.
    pub fn new(query: Option<&str>, tag: Option<&str>) -> Self {
        let normalize = |value: Option<&str>| {
            value
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_lowercase)
        };
        Self {
            query: normalize(query),
            tag: normalize(tag),
        }
    }

    /// Whether a note satisfies both active conditions.
    pub fn matches(&self, note: &Note) -> bool {
        self.query.as_ref().is_none_or(|query| {
            note.title.to_lowercase().contains(query)
                || note.body.to_lowercase().contains(query)
                || note.tags.iter().any(|tag| tag.contains(query))
        }) && self.tag.as_ref().is_none_or(|tag| note.tags.contains(tag))
    }
}

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
    tags: Option<&[String]>,
    now: i64,
) -> Result<Note> {
    let title = title_for(title, body);
    match id {
        Some(id) => {
            let existing = get(conn, id)?.ok_or_else(|| anyhow::anyhow!("no note with id {id}"))?;
            let tags = tags.map(normalize_tags).unwrap_or(existing.tags);
            let changed = conn.execute(
                "UPDATE notes SET title = ?2, body = ?3, tags = ?4, updated_at = ?5 WHERE id = ?1",
                rusqlite::params![id, title, body, serde_json::to_string(&tags)?, now],
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
                tags: tags.map(normalize_tags).unwrap_or_default(),
                created_at: now,
                updated_at: now,
            };
            conn.execute(
                "INSERT INTO notes (id, project, title, body, tags, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    note.id,
                    note.project.as_ref().map(|project| project.0.clone()),
                    note.title,
                    note.body,
                    serde_json::to_string(&note.tags)?,
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
            "SELECT id, project, title, body, created_at, updated_at, tags FROM notes WHERE id = ?1",
            [id],
            row_to_note,
        )
        .optional()?)
}

/// A project's notes, or all of them, filtered by text and exact tag, newest first.
pub fn list(
    conn: &Connection,
    project: Option<&ProjectName>,
    query: Option<&str>,
    tag: Option<&str>,
) -> Result<Vec<Note>> {
    let mut statement = conn.prepare(
        "SELECT id, project, title, body, created_at, updated_at, tags
           FROM notes
          WHERE ?1 IS NULL OR project = ?1
          ORDER BY updated_at DESC, created_at DESC",
    )?;
    let notes = statement
        .query_map([project.map(|project| project.0.clone())], row_to_note)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let filter = NoteFilter::new(query, tag);
    Ok(notes
        .into_iter()
        .filter(|note| filter.matches(note))
        .collect())
}

fn normalize_tags(tags: &[String]) -> Vec<String> {
    let mut normalized = Vec::new();
    for tag in tags {
        let tag = tag.trim().to_lowercase();
        if !tag.is_empty() && !normalized.contains(&tag) {
            normalized.push(tag);
        }
    }
    normalized
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
        tags: serde_json::from_str(&row.get::<_, String>(6)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(6, Type::Text, Box::new(error))
        })?,
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
        let first = save(&conn, None, Some(&project), "One", "body", None, 10).unwrap();
        let other = save(&conn, None, None, "", "loose", None, 20).unwrap();
        assert_eq!(list(&conn, None, None, None).unwrap().len(), 2);
        assert_eq!(
            list(&conn, Some(&project), None, None).unwrap(),
            vec![first.clone()]
        );

        let edited = save(&conn, Some(&first.id), None, "One", "changed", None, 30).unwrap();
        assert_eq!(edited.body, "changed");
        assert_eq!(edited.project, Some(project), "an edit does not move it");
        assert_eq!(
            list(&conn, None, None, None).unwrap()[0].id,
            first.id,
            "the one just touched comes first"
        );

        remove(&conn, &other.id).unwrap();
        remove(&conn, &other.id).unwrap();
        assert_eq!(list(&conn, None, None, None).unwrap().len(), 1);
    }

    #[test]
    fn editing_a_note_that_is_not_there_says_so() {
        assert!(save(&conn(), Some("missing"), None, "t", "b", None, 1).is_err());
    }

    #[test]
    fn tags_survive_edits_and_search_matches_title_body_and_tags() {
        let conn = conn();
        let project = ProjectName("ginka".into());
        let note = save(
            &conn,
            None,
            Some(&project),
            "Release plan",
            "Run the migration",
            Some(&["Deploy".into(), " deploy ".into(), "Ops".into()]),
            10,
        )
        .unwrap();
        assert_eq!(note.tags, vec!["deploy", "ops"]);
        assert_eq!(
            list(&conn, Some(&project), Some("RELEASE"), None).unwrap(),
            vec![note.clone()]
        );
        assert_eq!(
            list(&conn, None, Some("MIGRATION"), None).unwrap(),
            vec![note.clone()]
        );
        assert_eq!(
            list(&conn, None, Some("OPS"), None).unwrap(),
            vec![note.clone()]
        );
        assert_eq!(
            list(&conn, None, None, Some("DEPLOY")).unwrap(),
            vec![note.clone()]
        );
        assert!(list(&conn, None, None, Some("missing")).unwrap().is_empty());

        let edited = save(&conn, Some(&note.id), None, "Changed", "body", None, 20).unwrap();
        assert_eq!(edited.tags, vec!["deploy", "ops"]);
        let cleared = save(
            &conn,
            Some(&note.id),
            None,
            "Changed",
            "body",
            Some(&[]),
            30,
        )
        .unwrap();
        assert!(cleared.tags.is_empty());
        assert!(list(&conn, None, None, Some("deploy")).unwrap().is_empty());
    }

    #[test]
    fn note_filter_combines_text_with_an_exact_case_insensitive_tag() {
        let note = Note {
            id: "one".into(),
            project: None,
            title: "Release plan".into(),
            body: "Run migration".into(),
            tags: vec!["deploy".into(), "operations".into()],
            created_at: 0,
            updated_at: 0,
        };
        assert!(NoteFilter::new(Some("  RELEASE  "), Some(" DEPLOY ")).matches(&note));
        assert!(NoteFilter::new(Some("migration"), Some("deploy")).matches(&note));
        assert!(NoteFilter::new(Some("operations"), Some("deploy")).matches(&note));
        assert!(!NoteFilter::new(Some("missing"), Some("deploy")).matches(&note));
        assert!(!NoteFilter::new(None, Some("operation")).matches(&note));
        assert!(NoteFilter::new(Some("  "), Some(" ")).matches(&note));
    }
}
