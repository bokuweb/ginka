//! The Ginka desktop app.
//!
//! A viewport onto the daemon. It holds view state and nothing authoritative:
//! closing this window must never lose work, and the agents it was watching
//! keep running without it (`AGENTS.md` rule 1).

// The window's strings come from the workspace's `locales/`; `ginka-core`
// decides which language, because the CLI has to make the same choice without
// linking a UI toolkit.
rust_i18n::i18n!("locales", fallback = "en");

mod assets;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod browser;
mod daemon;
mod inbox;
mod lsp;
mod notes;
mod shell;
mod sidebar;
mod surfaces;

use anyhow::Result;
use ginka_core::{Paths, settings};
use gpui::{
    App, AppContext as _, Bounds, TitlebarOptions, WindowBackgroundAppearance, WindowBounds,
    WindowOptions, point, px, size,
};
use gpui_component::Root;

fn main() -> Result<()> {
    let paths = Paths::from_env()?;
    paths.ensure()?;
    let _log_guard = ginka_core::logging::init(&paths, "app")?;
    let app_settings: settings::AppSettings = settings::load(&paths.app_settings());
    let locale = ginka_core::i18n::init(app_settings.locale.as_deref());
    tracing::info!(%locale, "language");
    let shell_paths = paths.clone();

    let application = gpui_platform::application().with_assets(assets::Assets);

    application.run(move |cx: &mut App| {
        gpui_component::init(cx);
        shell::init(cx);
        ginka_ui::theme::apply(ginka_ui::Mode::Dark, cx);
        #[cfg(feature = "github")]
        {
            // The GitHub client binds its own chords, in its own key context.
            e1_views::init(cx);
            // Its tokens are a second global of a different type, read from the
            // same palette, so both apps resolve a colour the same way. Only
            // Ginka's `apply` installs the toolkit's own theme.
            e1_ui::Tokens::install(e1_ui::Mode::Dark, cx);
        }

        cx.spawn(async move |cx| {
            // No title bar of any kind: the columns run to the top of the
            // window and carry their own controls (`docs/ui.md` §3.1). What is
            // left for the platform is the traffic lights, positioned to sit
            // on the same line as those controls.
            let mut options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: None,
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(13.), px(15.))),
                }),
                // Our own header strips move the window with
                // `start_window_move`, so AppKit must not also treat them as a
                // system drag region: it would handle double clicks itself and
                // delay every click while it disambiguated them.
                app_owns_titlebar_drag: true,
                ..Default::default()
            };
            options.window_min_size = Some(size(px(880.), px(560.)));
            // The glass surface of docs/ui.md §1: the window is translucent and
            // the desktop behind it is blurred. The theme's `bg.window` carries
            // the alpha, so painting it opaque anywhere would cancel this out.
            options.window_background = WindowBackgroundAppearance::Blurred;
            options.window_bounds = Some(cx.update(|cx| {
                WindowBounds::Windowed(Bounds::centered(None, size(px(1440.), px(920.)), cx))
            }));

            cx.open_window(options, |window, cx| {
                // The window opens with no rows and fills in as soon as the
                // daemon answers: starting one takes long enough that waiting
                // for it here would show a blank screen instead of a window.
                let shell = cx.new(|cx| shell::Shell::new(shell_paths, app_settings, window, cx));
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
