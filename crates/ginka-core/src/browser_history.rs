//! The browser surface's history, per workspace, for the address bar to
//! complete from (roadmap §4.4 `browser_history`, M5).
//!
//! Ranked by frecency: how often a page was visited, weighted by how
//! recently — a page opened every day this week beats one opened fifty times
//! last spring. What is kept is what can be opened again: credentials,
//! fragments and query parameters that look like secrets are dropped before
//! anything is written.

use anyhow::Result;
use ginka_protocol::WorkspaceId;
use rusqlite::Connection;

/// One page the address bar can offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visited {
    pub url: String,
    pub title: Option<String>,
    pub visits: u32,
    /// Unix seconds.
    pub last_visited_at: i64,
}

/// The URL to keep for `raw`, or `None` for one not worth keeping —
/// `about:blank`, a `data:` page, anything that is not the web or a file.
pub fn keepable(raw: &str) -> Option<String> {
    /// A query parameter whose name says it carries a secret.
    const SECRET: [&str; 9] = [
        "token", "key", "secret", "password", "passwd", "auth", "session", "code", "sig",
    ];
    let mut url = url::Url::parse(raw.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https" | "file") {
        return None;
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_fragment(None);
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(name, _)| {
            let name = name.to_ascii_lowercase();
            !SECRET.iter().any(|secret| name.contains(secret))
        })
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    if kept.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(kept);
    }
    Some(url.to_string())
}

/// How much a page's visits count now: every visit, weighted by the age of
/// the last one.
pub fn frecency(visits: u32, last_visited_at: i64, now: i64) -> u64 {
    let days = (now - last_visited_at).max(0) / 86_400;
    let weight = match days {
        0..4 => 100,
        4..14 => 70,
        14..31 => 50,
        31..90 => 30,
        _ => 10,
    };
    u64::from(visits) * weight
}

/// Remember a visit to `url` in a workspace.
pub fn record(
    conn: &Connection,
    workspace: &WorkspaceId,
    url: &str,
    title: Option<&str>,
    at: i64,
) -> Result<()> {
    let Some(url) = keepable(url) else {
        return Ok(());
    };
    let title = title.map(str::trim).filter(|title| !title.is_empty());
    conn.execute(
        "INSERT INTO browser_history (workspace, url, title, visit_count, last_visited_at)
         VALUES (?1, ?2, ?3, 1, ?4)
         ON CONFLICT(workspace, url) DO UPDATE SET
             visit_count = visit_count + 1,
             last_visited_at = max(last_visited_at, excluded.last_visited_at),
             title = coalesce(excluded.title, title)",
        rusqlite::params![workspace.0, url, title, at],
    )?;
    Ok(())
}

/// The pages to offer for what is typed so far — every word of it found in
/// the URL or the title — best first.
pub fn suggestions(
    conn: &Connection,
    workspace: &WorkspaceId,
    query: &str,
    now: i64,
    limit: usize,
) -> Result<Vec<Visited>> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut statement = conn.prepare(
        "SELECT url, title, visit_count, last_visited_at FROM browser_history
          WHERE workspace = ?1",
    )?;
    let mut found: Vec<Visited> = statement
        .query_map([&workspace.0], |row| {
            Ok(Visited {
                url: row.get(0)?,
                title: row.get(1)?,
                visits: row.get(2)?,
                last_visited_at: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|visited| {
            let haystack = format!(
                "{} {}",
                visited.url.to_lowercase(),
                visited.title.as_deref().unwrap_or_default().to_lowercase()
            );
            words.iter().all(|word| haystack.contains(word))
        })
        .collect();
    found.sort_by_key(|visited| {
        (
            std::cmp::Reverse(frecency(visited.visits, visited.last_visited_at, now)),
            std::cmp::Reverse(visited.last_visited_at),
        )
    });
    found.truncate(limit);
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    const DAY: i64 = 86_400;
    const NOW: i64 = 1_790_000_000;

    fn comet() -> WorkspaceId {
        WorkspaceId("comet/main".into())
    }

    #[test]
    fn what_is_kept_can_be_opened_again_and_holds_no_secret() {
        assert_eq!(
            keepable("https://user:pw@localhost:3000/items?page=2&token=abc#top").as_deref(),
            Some("https://localhost:3000/items?page=2")
        );
        assert_eq!(
            keepable("http://localhost:5173/?api_key=1&session=2").as_deref(),
            Some("http://localhost:5173/")
        );
        assert_eq!(keepable("about:blank"), None);
        assert_eq!(keepable("data:text/html,hi"), None);
        assert_eq!(keepable("not a url"), None);
    }

    #[test]
    fn recent_visits_outweigh_old_ones() {
        let this_week_daily = frecency(7, NOW - DAY, NOW);
        let last_year_often = frecency(50, NOW - 300 * DAY, NOW);
        assert!(
            this_week_daily > last_year_often,
            "{this_week_daily} vs {last_year_often}"
        );
        assert!(frecency(2, NOW, NOW) > frecency(1, NOW, NOW));
    }

    #[test]
    fn a_visit_is_counted_per_workspace_and_offered_best_first() {
        let conn = db::open_in_memory().unwrap();
        let other = WorkspaceId("comet/try-1".into());
        for _ in 0..3 {
            record(
                &conn,
                &comet(),
                "http://localhost:3000/dashboard",
                Some("Dashboard"),
                NOW - DAY,
            )
            .unwrap();
        }
        record(
            &conn,
            &comet(),
            "http://localhost:3000/settings",
            Some("Settings"),
            NOW,
        )
        .unwrap();
        record(
            &conn,
            &comet(),
            "https://docs.rs/gpui",
            Some("gpui - Rust"),
            NOW - 200 * DAY,
        )
        .unwrap();
        record(&conn, &other, "http://localhost:3000/elsewhere", None, NOW).unwrap();

        let all = suggestions(&conn, &comet(), "", NOW, 10).unwrap();
        let urls: Vec<&str> = all.iter().map(|visited| visited.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "http://localhost:3000/dashboard",
                "http://localhost:3000/settings",
                "https://docs.rs/gpui"
            ],
            "only this workspace's, most frecent first"
        );
        assert_eq!(all[0].visits, 3);
        assert_eq!(all[0].title.as_deref(), Some("Dashboard"));

        let typed = suggestions(&conn, &comet(), "RUST gpui", NOW, 10).unwrap();
        assert_eq!(
            typed.len(),
            1,
            "every word, in the URL or the title, any case"
        );
        assert_eq!(typed[0].url, "https://docs.rs/gpui");
        assert_eq!(suggestions(&conn, &comet(), "", NOW, 1).unwrap().len(), 1);

        // A page not worth keeping is not kept.
        record(&conn, &comet(), "about:blank", None, NOW).unwrap();
        assert_eq!(suggestions(&conn, &comet(), "", NOW, 10).unwrap().len(), 3);
    }
}
