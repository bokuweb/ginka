//! The Dock tile's badge: how many sessions wait on the reader
//! (`ginka_ui::notify::badge`). GPUI has no Dock API, so macOS is reached
//! through AppKit; elsewhere there is no Dock and this does nothing.

/// Show `label` on the Dock tile, or clear it.
///
/// Called from the window's own updates, which run on the main thread; off
/// it, AppKit must not be touched and the call is dropped.
pub fn set_badge(label: Option<String>) {
    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::NSApplication;
        use objc2_foundation::NSString;
        let Some(main) = MainThreadMarker::new() else {
            return;
        };
        let tile = NSApplication::sharedApplication(main).dockTile();
        let label = label.map(|label| NSString::from_str(&label));
        tile.setBadgeLabel(label.as_deref());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = label;
    }
}
