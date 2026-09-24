//! Quick commands: saved shell commands and prompts.
//!
//! Orca's idea. `cargo test -p ginka-core`, `pnpm dev`, "review the diff
//! against main and list what is risky" — the things a reader runs in every
//! workspace of a project, kept once and run from the terminal dock or the
//! palette. The daemon keeps them (rule 1), so the CLI and an agent over MCP
//! see the same list the window does.

use anyhow::{Result, bail};
use ginka_protocol::ProjectName;
use ginka_protocol::model::{QuickCommand, QuickCommandKind};
use rusqlite::{Connection, OptionalExtension as _};

/// A project's commands and the global ones, or only the global ones, by name.
pub fn list(conn: &Connection, project: Option<&ProjectName>) -> Result<Vec<QuickCommand>> {
    let mut statement = conn.prepare(
        "SELECT id, project, name, kind, body FROM quick_commands
          WHERE project IS NULL OR project = ?1
          ORDER BY name COLLATE NOCASE, id",
    )?;
    let commands = statement
        .query_map([project.map(|project| project.0.clone())], row_to_command)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(commands)
}

/// One command.
pub fn get(conn: &Connection, id: &str) -> Result<Option<QuickCommand>> {
    Ok(conn
        .query_row(
            "SELECT id, project, name, kind, body FROM quick_commands WHERE id = ?1",
            [id],
            row_to_command,
        )
        .optional()?)
}

/// Save one: new without an `id`, a replacement with one.
pub fn save(
    conn: &Connection,
    id: Option<&str>,
    project: Option<&ProjectName>,
    name: &str,
    kind: QuickCommandKind,
    body: &str,
) -> Result<QuickCommand> {
    let name = name.trim();
    if name.is_empty() {
        bail!("a quick command needs a name");
    }
    if body.trim().is_empty() {
        bail!("a quick command needs something to run");
    }
    let command = QuickCommand {
        id: id
            .map(str::to_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string()),
        project: project.cloned(),
        name: name.to_string(),
        kind,
        body: body.to_string(),
    };
    if id.is_some() && get(conn, &command.id)?.is_none() {
        bail!("no quick command with id {}", command.id);
    }
    conn.execute(
        "INSERT INTO quick_commands (id, project, name, kind, body) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(id) DO UPDATE SET project = ?2, name = ?3, kind = ?4, body = ?5",
        rusqlite::params![
            command.id,
            command.project.as_ref().map(|project| project.0.clone()),
            command.name,
            command.kind.as_str(),
            command.body,
        ],
    )?;
    Ok(command)
}

/// Forget one. Forgetting one that is not there is not an error.
pub fn remove(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM quick_commands WHERE id = ?1", [id])?;
    Ok(())
}

/// The shell arguments a command runs with: the command, then a line with
/// its exit status, then an interactive shell in its place — so the output
/// stays readable and the tab is somewhere to carry on.
pub fn shell_args(body: &str) -> Vec<String> {
    vec![
        "-lc".to_string(),
        format!(
            "{body}\nstatus=$?\nprintf '\\n[exit %s]\\n' \"$status\"\nexec \"${{SHELL:-/bin/sh}}\" -l"
        ),
    ]
}

fn row_to_command(row: &rusqlite::Row<'_>) -> rusqlite::Result<QuickCommand> {
    let kind: String = row.get(3)?;
    Ok(QuickCommand {
        id: row.get(0)?,
        project: row.get::<_, Option<String>>(1)?.map(ProjectName),
        name: row.get(2)?,
        kind: QuickCommandKind::parse(&kind).unwrap_or(QuickCommandKind::Shell),
        body: row.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        crate::db::open_in_memory().unwrap()
    }

    #[test]
    fn a_project_sees_its_own_commands_and_the_global_ones() {
        let conn = conn();
        let ginka = ProjectName("ginka".into());
        let other = ProjectName("other".into());
        save(
            &conn,
            None,
            Some(&ginka),
            "test",
            QuickCommandKind::Shell,
            "cargo test",
        )
        .unwrap();
        save(
            &conn,
            None,
            None,
            "Review",
            QuickCommandKind::Prompt,
            "review the diff",
        )
        .unwrap();
        save(
            &conn,
            None,
            Some(&other),
            "dev",
            QuickCommandKind::Shell,
            "pnpm dev",
        )
        .unwrap();

        let names = |project| {
            list(&conn, project)
                .unwrap()
                .into_iter()
                .map(|command| command.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(Some(&ginka)),
            vec!["Review", "test"],
            "by name, case aside"
        );
        assert_eq!(names(None), vec!["Review"], "only the global ones");
    }

    #[test]
    fn a_command_is_edited_in_place_and_removed() {
        let conn = conn();
        let saved = save(&conn, None, None, "t", QuickCommandKind::Shell, "true").unwrap();
        let edited = save(
            &conn,
            Some(&saved.id),
            None,
            "t2",
            QuickCommandKind::Shell,
            "false",
        )
        .unwrap();
        assert_eq!(edited.id, saved.id);
        assert_eq!(get(&conn, &saved.id).unwrap().unwrap().body, "false");
        remove(&conn, &saved.id).unwrap();
        remove(&conn, &saved.id).unwrap();
        assert!(list(&conn, None).unwrap().is_empty());
    }

    #[test]
    fn a_nameless_or_empty_command_is_refused() {
        let conn = conn();
        assert!(save(&conn, None, None, " ", QuickCommandKind::Shell, "ls").is_err());
        assert!(save(&conn, None, None, "ls", QuickCommandKind::Shell, "  ").is_err());
        assert!(
            save(
                &conn,
                Some("nope"),
                None,
                "ls",
                QuickCommandKind::Shell,
                "ls"
            )
            .is_err()
        );
    }

    #[test]
    fn the_shell_keeps_the_tab_open_after_the_command() {
        let args = shell_args("cargo test");
        assert_eq!(args[0], "-lc");
        assert!(args[1].starts_with("cargo test\n"));
        assert!(args[1].contains("[exit %s]"));
        assert!(args[1].ends_with("-l"));
    }
}
