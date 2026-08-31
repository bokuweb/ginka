//! The Ginka desktop app.
//!
//! A viewport onto `ginka-core` and, from M2, onto the daemon. It holds view
//! state and nothing authoritative: closing this window must never lose work
//! (`AGENTS.md` rule 1).

mod shell;
mod sidebar;
mod surfaces;

use anyhow::Result;
use ginka_core::{Paths, settings};
use gpui::{App, AppContext as _, WindowOptions, px, size};
use gpui_component::{Root, TitleBar};

fn main() -> Result<()> {
    let paths = Paths::from_env()?;
    paths.ensure()?;
    let _log_guard = ginka_core::logging::init(&paths, "app")?;
    let app_settings: settings::AppSettings = settings::load(&paths.app_settings());

    let application = gpui_platform::application().with_assets(ginka_ui::Assets);

    application.run(move |cx: &mut App| {
        gpui_component::init(cx);
        ginka_ui::theme::apply(ginka_ui::Mode::Dark, cx);

        cx.spawn(async move |cx| {
            let mut options: WindowOptions = TitleBar::window_options();
            options.window_min_size = Some(size(px(880.), px(560.)));

            cx.open_window(options, |window, cx| {
                let shell = cx.new(|cx| shell::Shell::new(app_settings, window, cx));
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
