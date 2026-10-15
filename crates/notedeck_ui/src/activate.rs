//! Bring notedeck in front of the other apps on the desktop.

/// Bring the application to the front.
///
/// On macOS, egui's ViewportCommand::Focus focuses the window but doesn't
/// always activate the app (bring it in front of other apps). Stage Manager
/// single-window mode is particularly aggressive, so we use both
/// NSRunningApplication::activateWithOptions and orderFrontRegardless
/// on the key window.
pub fn activate_app(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);

    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSApplication, NSApplicationActivationOptions, NSRunningApplication};

        // Safety: UI update runs on the main thread
        if let Some(mtm) = MainThreadMarker::new() {
            let app = NSApplication::sharedApplication(mtm);

            // Activate via NSRunningApplication for per-process activation
            let current = NSRunningApplication::currentApplication();
            current.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);

            // Also force the key window to front regardless of Stage Manager
            if let Some(window) = app.keyWindow() {
                window.orderFrontRegardless();
            }
        }
    }
}
