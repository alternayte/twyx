//! The Dock icon's badge on macOS: the count of unread inbox items.

/// Shows `unread` on the Dock icon, or no badge for none.
#[cfg(target_os = "macos")]
pub fn badge(unread: usize) {
    use objc2_app_kit::NSApplication;
    use objc2_foundation::{MainThreadMarker, NSString};
    let Some(main) = MainThreadMarker::new() else {
        return;
    };
    let label = (unread > 0).then(|| NSString::from_str(&unread.to_string()));
    NSApplication::sharedApplication(main)
        .dockTile()
        .setBadgeLabel(label.as_deref());
}

#[cfg(not(target_os = "macos"))]
pub fn badge(_unread: usize) {}
