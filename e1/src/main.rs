//! The e1 desktop app.
//!
//! Deliberately thin: it finds where settings live, decides the language,
//! finds a token, owns standalone-only lifecycle integration, opens a frameless
//! window and mounts [`e1_views::Shell`]. Everything that draws is in
//! `e1-views`, so a host can mount the same views without this file
//! (`AGENTS.md` rule 1).

rust_i18n::i18n!("locales", fallback = "en");

use anyhow::Result;
use e1_github::auth::Source;
use e1_github::{GitHub, HttpCache, RepoId, Rest, Scripted};
use e1_ui::settings::{self, AppSettings};
use e1_ui::{Mode, Paths};
use gpui::{
    App, AppContext as _, Bounds, TitlebarOptions, WindowBackgroundAppearance, WindowBounds,
    WindowOptions, point, px, size,
};
use gpui_component::Root;
use std::sync::Arc;

#[cfg(target_os = "macos")]
gpui::actions!(e1_app, [CheckForUpdates, Quit]);

/// The updater is application-owned state, deliberately separate from the
/// embeddable GitHub views.
#[cfg(target_os = "macos")]
struct UpdaterState(Option<e1_updater_macos::Updater>);

#[cfg(target_os = "macos")]
impl gpui::Global for UpdaterState {}

/// Install the macOS application menu, omitting update UI from unsupported
/// development layouts where Sparkle could not initialize.
#[cfg(target_os = "macos")]
fn set_app_menu(cx: &mut App, updater_available: bool) {
    use gpui::{Menu, MenuItem, SystemMenuType};

    let mut items = Vec::new();
    if updater_available {
        items.push(MenuItem::action(
            rust_i18n::t!("menu.check_for_updates").to_string(),
            CheckForUpdates,
        ));
        items.push(MenuItem::separator());
    }
    items.push(MenuItem::os_submenu(
        rust_i18n::t!("menu.services").to_string(),
        SystemMenuType::Services,
    ));
    items.push(MenuItem::separator());
    items.push(MenuItem::action(
        rust_i18n::t!("menu.quit").to_string(),
        Quit,
    ));
    cx.set_menus([Menu::new(rust_i18n::t!("app.name").to_string()).items(items)]);
}

/// Where the window's data comes from, and where its token came from.
///
/// `E1_DEMO=1` runs over scripted data with no network at all. Otherwise the
/// token is discovered; without one the window still opens, on the sign-in
/// screen — a window that refuses to open cannot tell the reader what to do
/// about it.
fn source(paths: &Paths) -> (Option<Arc<dyn GitHub>>, Option<Source>) {
    if std::env::var_os("E1_DEMO").is_some_and(|value| value == "1") {
        tracing::info!("running over scripted data");
        return (Some(Arc::new(Scripted::sample())), None);
    }
    match e1_github::auth::discover() {
        Some((token, source)) => {
            tracing::info!(?source, "token found");
            let cache = HttpCache::new(paths.http_cache());
            (
                Some(Arc::new(Rest::new(token).with_cache(cache))),
                Some(source),
            )
        }
        None => {
            tracing::warn!("no GitHub token was found; opening on the sign-in screen");
            (None, None)
        }
    }
}

/// `E1_DEMO_OPEN=owner/name#12:src/main.rs`: an item to open, on its files,
/// with one of them expanded, as soon as the window is up. The path is
/// optional. For screenshots; see `Shell::open_at_launch`.
fn open_at_launch() -> Option<((RepoId, u64), Option<String>)> {
    let value = std::env::var("E1_DEMO_OPEN").ok()?;
    let (item, file) = match value.split_once(':') {
        Some((item, file)) => (item, Some(file.to_string())),
        None => (value.as_str(), None),
    };
    let (repo, number) = item.split_once('#')?;
    Some(((RepoId::parse(repo)?, number.parse().ok()?), file))
}

/// `E1_DEMO_LOG=2`: with `E1_DEMO_OPEN`, an Actions job whose log to open
/// on top of the item. For screenshots of the log screen.
fn log_at_launch() -> Option<u64> {
    std::env::var("E1_DEMO_LOG").ok()?.parse().ok()
}

/// `E1_DEMO_HISTORY=owner/name`: a repository's history to open, with its
/// newest commit read. For screenshots.
fn history_at_launch() -> Option<RepoId> {
    RepoId::parse(&std::env::var("E1_DEMO_HISTORY").ok()?)
}

/// `E1_DEMO_FILES=owner/name:src/main.rs`: a repository's finder to open,
/// with a file read. The path is optional. For screenshots.
fn browse_at_launch() -> Option<(RepoId, Option<String>)> {
    let value = std::env::var("E1_DEMO_FILES").ok()?;
    let (repo, file) = match value.split_once(':') {
        Some((repo, file)) => (repo, Some(file.to_string())),
        None => (value.as_str(), None),
    };
    Some((RepoId::parse(repo)?, file))
}

fn main() -> Result<()> {
    let paths = Paths::from_env()?;
    paths.ensure()?;
    let _log_guard = e1_ui::logging::init(&paths)?;
    let app_settings: AppSettings = settings::load(&paths.app_settings());
    let locale = e1_ui::i18n::init(app_settings.locale.as_deref());
    tracing::info!(%locale, "language");
    let (github, token_source) = source(&paths);
    let shell_paths = paths.clone();

    let application = gpui_platform::application().with_assets(e1_ui::Assets);

    application.run(move |cx: &mut App| {
        gpui_component::init(cx);
        e1_views::init(cx);
        let mode = Mode::resolve(app_settings.appearance, cx.window_appearance());
        e1_ui::theme::apply(mode, cx);

        #[cfg(target_os = "macos")]
        {
            use gpui::KeyBinding;

            let updater = e1_updater_macos::Updater::init();
            let updater_available = updater.is_some();
            cx.set_global(UpdaterState(updater));
            cx.on_action(|_: &CheckForUpdates, cx| {
                if let Some(updater) = &cx.global::<UpdaterState>().0 {
                    updater.check_for_updates();
                }
            });
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
            set_app_menu(cx, updater_available);
        }

        cx.spawn(async move |cx| {
            // No title bar of any kind: the columns run to the top of the
            // window and carry their own controls (`docs/ui.md` §3.1). What
            // is left for the platform is the traffic lights, positioned to
            // sit on the same line as those controls.
            let mut options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: None,
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(13.), px(15.))),
                }),
                // Our own header strips move the window, so AppKit must not
                // also treat them as a system drag region.
                app_owns_titlebar_drag: true,
                ..Default::default()
            };
            options.window_min_size = Some(size(px(880.), px(560.)));
            // The glass surface of docs/ui.md §1: the window is translucent
            // and the desktop behind it is blurred.
            options.window_background = WindowBackgroundAppearance::Blurred;
            options.window_bounds = Some(cx.update(|cx| {
                WindowBounds::Windowed(Bounds::centered(None, size(px(1440.), px(920.)), cx))
            }));

            let open_at_launch = open_at_launch();
            let log_at_launch = log_at_launch();
            let browse_at_launch = browse_at_launch();
            let history_at_launch = history_at_launch();
            cx.open_window(options, |window, cx| {
                let shell = cx.new(|cx| {
                    e1_views::Shell::new(
                        github,
                        token_source,
                        shell_paths,
                        app_settings,
                        window,
                        cx,
                    )
                });
                if let Some((key, file)) = open_at_launch {
                    shell.update(cx, |shell, cx| {
                        shell.open_at_launch(key, file, log_at_launch, cx)
                    });
                }
                if let Some((repo, file)) = browse_at_launch {
                    shell.update(cx, |shell, cx| shell.browse_at_launch(repo, file, cx));
                }
                if let Some(repo) = history_at_launch {
                    shell.update(cx, |shell, cx| shell.history_at_launch(repo, cx));
                }
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
