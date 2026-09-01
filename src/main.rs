//! The Ginka desktop app.
//!
//! A viewport onto `ginka-core` and, from M2, onto the daemon. It holds view
//! state and nothing authoritative: closing this window must never lose work
//! (`AGENTS.md` rule 1).

mod shell;
mod sidebar;
mod surfaces;

use anyhow::Result;
use ginka_core::{Paths, db, project, registry, settings};
use ginka_ui::workspace::SessionRow;
use gpui::{
    App, AppContext as _, Bounds, WindowBackgroundAppearance, WindowBounds, WindowOptions, px, size,
};
use gpui_component::{Root, TitleBar};

/// Read the registered projects and their worktrees into sidebar rows.
///
/// Storage problems are logged and yield an empty list rather than stopping the
/// launch: an app that will not open is a worse failure than one that opens
/// empty and says so, and the empty state tells the user how to register a
/// project.
fn load_sessions(paths: &Paths) -> Vec<SessionRow> {
    let conn = match db::open(&paths.database()) {
        Ok(conn) => conn,
        Err(error) => {
            tracing::error!(%error, "could not open the database; starting with no sessions");
            return Vec::new();
        }
    };

    let projects = match project::list_projects(&conn) {
        Ok(projects) => projects,
        Err(error) => {
            tracing::error!(%error, "could not read projects");
            return Vec::new();
        }
    };

    let mut rows = Vec::new();
    for project in projects {
        // Adopt anything created outside the app since we last looked. A failure
        // here is not fatal: show what is stored rather than nothing.
        if let Err(error) = registry::sync_worktrees(&conn, &project) {
            tracing::warn!(project = %project.name, %error, "could not sync worktrees");
        }
        match project::list_worktrees(&conn, &project.name) {
            Ok(worktrees) => rows.extend(SessionRow::from_worktrees(&project, &worktrees)),
            Err(error) => {
                tracing::warn!(project = %project.name, %error, "could not list worktrees")
            }
        }
    }
    rows
}

fn main() -> Result<()> {
    let paths = Paths::from_env()?;
    paths.ensure()?;
    let _log_guard = ginka_core::logging::init(&paths, "app")?;
    let app_settings: settings::AppSettings = settings::load(&paths.app_settings());
    let rows = load_sessions(&paths);
    let shell_paths = paths.clone();

    let application = gpui_platform::application().with_assets(ginka_ui::Assets);

    application.run(move |cx: &mut App| {
        gpui_component::init(cx);
        shell::init(cx);
        ginka_ui::theme::apply(ginka_ui::Mode::Dark, cx);

        cx.spawn(async move |cx| {
            let mut options: WindowOptions = TitleBar::window_options();
            options.window_min_size = Some(size(px(880.), px(560.)));
            // The glass surface of docs/ui.md §1: the window is translucent and
            // the desktop behind it is blurred. The theme's `bg.window` carries
            // the alpha, so painting it opaque anywhere would cancel this out.
            options.window_background = WindowBackgroundAppearance::Blurred;
            options.window_bounds = Some(cx.update(|cx| {
                WindowBounds::Windowed(Bounds::centered(None, size(px(1440.), px(920.)), cx))
            }));

            cx.open_window(options, |window, cx| {
                let shell =
                    cx.new(|cx| shell::Shell::new(shell_paths, app_settings, rows, window, cx));
                cx.new(|cx| Root::new(shell, window, cx))
            })
            .expect("failed to open the main window");

            // Launched from a terminal rather than an app bundle, the window
            // opens behind whatever was frontmost unless we ask for focus.
            cx.update(|cx| cx.activate(true));
        })
        .detach();
    });

    Ok(())
}
