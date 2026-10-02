//! Standalone AWS client entry point.
//!
//! Views and SQS behavior live in libraries so another GPUI host can mount
//! them while this binary only owns the window lifecycle.

use ginka_core::{Paths, settings};
use gpui::{
    App, AppContext as _, Bounds, TitlebarOptions, WindowBackgroundAppearance, WindowBounds,
    WindowOptions, point, px, size,
};
use gpui_component::Root;

fn main() -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let app_settings: settings::AppSettings = settings::load(&paths.app_settings());
    let aws_settings_path = paths.root().join("aws.json");
    ginka_core::i18n::init(app_settings.locale.as_deref());
    gpui_platform::application()
        .with_assets(ginka_ui::Assets)
        .run(|cx: &mut App| {
            gpui_component::init(cx);
            ginka_ui::theme::apply(ginka_ui::Mode::Dark, cx);
            cx.spawn(async move |cx| {
                let options = WindowOptions {
                    titlebar: Some(TitlebarOptions {
                        title: None,
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(13.), px(15.))),
                    }),
                    app_owns_titlebar_drag: true,
                    window_background: WindowBackgroundAppearance::Blurred,
                    window_min_size: Some(size(px(1100.), px(560.))),
                    window_bounds: Some(cx.update(|cx| {
                        WindowBounds::Windowed(Bounds::centered(
                            None,
                            size(px(1360.), px(860.)),
                            cx,
                        ))
                    })),
                    ..Default::default()
                };
                cx.open_window(options, |window, cx| {
                    let view = cx.new(|cx| {
                        aws_views::SqsView::with_settings_file(
                            window,
                            cx,
                            aws_settings_path.clone(),
                        )
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                })
                .expect("failed to open AWS client window");
                cx.update(|cx| cx.activate(true));
            })
            .detach();
        });
    Ok(())
}
