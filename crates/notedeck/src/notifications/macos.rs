//! macOS notification backend using UNUserNotificationCenter.
//!
//! Provides native macOS notifications with profile picture support via
//! `UNNotificationAttachment`. This backend requires the app to run within
//! a proper `.app` bundle.
//!
//! # Architecture
//!
//! - **Linux**: Uses `notify-rust` (libnotify/DBus) - works well, supports images
//! - **macOS**: Uses `UNUserNotificationCenter` - requires `.app` bundle, supports images
//!
//! # Why not notify-rust on macOS?
//!
//! `notify-rust` uses `mac-notification-sys` which has a bug: it calls
//! `get id of application "use_default"` via AppleScript during initialization,
//! which can trigger a "Where is use_default?" dialog on macOS.
//!
//! Additionally, `mac-notification-sys` uses the deprecated `NSUserNotificationCenter`
//! API (deprecated since macOS 10.14).
//!
//! # Image Support
//!
//! macOS `UNNotificationAttachment` only accepts local file URLs. Remote images
//! must be downloaded and cached locally first. Use [`super::image_cache::NotificationImageCache`]
//! to handle this.
//!
//! # Bundle Requirement
//!
//! `UNUserNotificationCenter` requires a proper `.app` bundle with a valid
//! `Info.plist`. Running via `cargo run` will fail. For development, either:
//! - Build the `.app` bundle
//! - Skip notifications (this backend logs a warning and returns gracefully)
//!
//! # Thread Safety
//!
//! The notification delegate and permission request must happen on the main thread
//! (or a thread with a run loop). These are handled by [`initialize_on_main_thread`]
//! which should be called during app startup.
//!
//! The [`MacOSBackend`] itself can be constructed on any thread (including the
//! notification worker thread) since it only sends notifications - it doesn't
//! need to interact with the delegate after installation.

use super::backend::NotificationBackend;
use super::types::{safe_prefix, ExtractedEvent};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{class, msg_send, sel};
use std::path::Path;
use std::sync::OnceLock;
use tracing::{debug, error, info, warn};

/// Check if the current thread is the main thread.
///
/// On macOS, uses `NSThread.isMainThread` class method for accurate detection.
pub(super) fn is_main_thread() -> bool {
    // SAFETY: `+[NSThread isMainThread]` is a side-effect free class method that
    // returns a primitive bool. The Objective-C runtime is initialized by the
    // time this code runs in our app lifecycle.
    let result: bool = unsafe { msg_send![class!(NSThread), isMainThread] };
    result
}

/// Initialize macOS notification system on the main thread.
///
/// This must be called on the main thread (or a thread with a run loop) during
/// app startup. It:
/// 1. Installs the notification delegate for foreground presentation
/// 2. Requests notification permission from the user
///
/// Returns the delegate which must be kept alive for the app's lifetime.
/// Store this in `NotificationManager` to ensure proper cleanup.
///
/// Returns `None` if not running in a valid `.app` bundle.
///
/// # Thread Safety
///
/// This should be called on the main thread. If called from another thread,
/// a warning is logged and the permission dialog may not appear correctly.
pub fn initialize_on_main_thread() -> Option<Retained<AnyObject>> {
    // Warn if not on main thread - permission dialog may fail
    if !is_main_thread() {
        warn!(
            "macOS notification initialization called off main thread; \
             permission dialog may not appear correctly"
        );
    }

    if !is_valid_bundle() {
        warn!("macOS notifications: not running in .app bundle, skipping initialization");
        return None;
    }

    info!("Initializing macOS notifications on main thread");

    // Install delegate for foreground notifications
    let delegate = create_and_install_delegate();

    // Request permission - shows native macOS dialog
    request_permission();

    Some(delegate)
}

/// Check if running in a valid macOS `.app` bundle.
///
/// `UNUserNotificationCenter` requires a proper bundle with `Info.plist`.
pub fn is_valid_bundle() -> bool {
    use objc2_foundation::NSBundle;

    let bundle = NSBundle::mainBundle();
    let identifier = bundle.bundleIdentifier();

    match identifier {
        Some(id) => {
            let id_str = id.to_string();
            let valid =
                !id_str.is_empty() && !id_str.starts_with("com.apple.") && id_str != "cargo";
            debug!("Bundle identifier: {} (valid: {})", id_str, valid);
            valid
        }
        None => {
            debug!("No bundle identifier found");
            false
        }
    }
}

/// Create and install the notification center delegate for foreground notifications.
///
/// By default, macOS suppresses notification banners when the app is in the foreground.
/// This delegate implements `userNotificationCenter:willPresentNotification:withCompletionHandler:`
/// to return presentation options that force the banner to display.
///
/// Returns an owned reference to the delegate that must be kept alive.
fn create_and_install_delegate() -> Retained<AnyObject> {
    unsafe {
        // Get the notification center
        let center: *mut AnyObject =
            msg_send![class!(UNUserNotificationCenter), currentNotificationCenter];

        // Create the delegate class and instance
        let delegate_class = get_or_create_delegate_class();
        let delegate: Retained<AnyObject> = msg_send![delegate_class, new];

        // Set as delegate
        let _: () = msg_send![center, setDelegate: &*delegate];

        debug!("macOS notification delegate installed for foreground presentation");

        delegate
    }
}

/// Get or create the Objective-C delegate class.
///
/// Uses a static `OnceLock` because the Objective-C runtime only allows
/// registering a class once per process. This is an unavoidable requirement
/// of the Objective-C runtime, not application state.
fn get_or_create_delegate_class() -> &'static AnyClass {
    use objc2::runtime::ClassBuilder;

    static CLASS: OnceLock<&'static AnyClass> = OnceLock::new();

    CLASS.get_or_init(|| {
        let superclass = class!(NSObject);
        let mut builder = ClassBuilder::new(c"NotedeckNotificationDelegate", superclass)
            .expect("Failed to create delegate class");

        // Add the willPresentNotification delegate method
        unsafe {
            builder.add_method(
                sel!(userNotificationCenter:willPresentNotification:withCompletionHandler:),
                will_present_notification
                    as unsafe extern "C" fn(
                        *mut objc2::runtime::AnyObject,
                        Sel,
                        *mut objc2::runtime::AnyObject,
                        *mut objc2::runtime::AnyObject,
                        *mut objc2::runtime::AnyObject,
                    ),
            );
        }

        builder.register()
    })
}

/// Objective-C block layout for calling the completion handler
#[repr(C)]
struct BlockLiteral {
    isa: *const std::ffi::c_void,
    flags: i32,
    reserved: i32,
    invoke: unsafe extern "C" fn(*mut BlockLiteral, usize),
}

/// Delegate method: userNotificationCenter:willPresentNotification:withCompletionHandler:
///
/// This is called when a notification arrives while the app is in the foreground.
/// We return presentation options to show the banner, play sound, and add to list.
unsafe extern "C" fn will_present_notification(
    _this: *mut objc2::runtime::AnyObject,
    _sel: Sel,
    _center: *mut objc2::runtime::AnyObject,
    _notification: *mut objc2::runtime::AnyObject,
    completion_handler: *mut objc2::runtime::AnyObject,
) {
    // UNNotificationPresentationOptions:
    // .badge  = 1 << 0 = 1
    // .sound  = 1 << 1 = 2
    // .alert  = 1 << 2 = 4  (macOS 10.14+, deprecated in 11 but still works)
    // .list   = 1 << 3 = 8  (macOS 11+)
    // .banner = 1 << 4 = 16 (macOS 11+)
    //
    // Include .alert for macOS 10.15 compatibility (our minimum target).
    // On macOS 11+, .banner takes precedence; .alert is ignored.
    let options: usize = 4 | 2 | 16 | 8; // alert + sound + banner + list

    debug!(
        "willPresentNotification called, returning options: {}",
        options
    );

    // Call the completion handler block with our presentation options
    // The completion handler is an Objective-C block
    if !completion_handler.is_null() {
        let block = completion_handler as *mut BlockLiteral;
        ((*block).invoke)(block, options);
    }
}

/// macOS notification backend using UNUserNotificationCenter.
///
/// Displays native system notifications on macOS with support for profile
/// pictures via `UNNotificationAttachment`.
///
/// # Requirements
///
/// - Must run within a `.app` bundle (not `cargo run`)
/// - Images must be local file paths (use `NotificationImageCache` to download)
/// - [`initialize_on_main_thread`] must be called during app startup
///
/// # Thread Safety
///
/// This struct can be constructed on any thread. The delegate installation and
/// permission request happen separately via [`initialize_on_main_thread`].
pub struct MacOSBackend {
    /// Whether we're running in a valid bundle
    is_bundle_valid: bool,
}

impl MacOSBackend {
    /// Create a new macOS notification backend.
    ///
    /// Note: [`initialize_on_main_thread`] should be called during app startup
    /// before using this backend. This constructor just checks bundle validity.
    pub fn new() -> Self {
        let is_bundle_valid = is_valid_bundle();

        if is_bundle_valid {
            info!("macOS notification backend initialized (bundle valid)");
        } else {
            warn!(
                "macOS notification backend: not running in .app bundle, notifications will be skipped"
            );
        }

        Self { is_bundle_valid }
    }
}

/// Request notification permission from the user.
///
/// This shows a native macOS dialog: "Notedeck would like to send you notifications"
/// with Allow/Don't Allow buttons. The result is logged asynchronously.
///
/// Called automatically by [`initialize_on_main_thread`].
fn request_permission() {
    use objc2::runtime::Bool;
    use objc2_foundation::NSError;
    use objc2_user_notifications::{UNAuthorizationOptions, UNUserNotificationCenter};

    let center = UNUserNotificationCenter::currentNotificationCenter();

    // Request alert + sound + badge permissions
    let options = UNAuthorizationOptions(1 | 2 | 4); // Alert + Sound + Badge

    let completion_handler = block2::RcBlock::new(move |granted: Bool, error: *mut NSError| {
        if granted.as_bool() {
            info!("macOS notification permission GRANTED");
        } else if !error.is_null() {
            // SAFETY: error is non-null, we can dereference it
            let err = unsafe { &*error };
            warn!(
                "macOS notification permission result: granted={}, error: {} (code={})",
                granted.as_bool(),
                err.localizedDescription(),
                err.code()
            );
        } else {
            warn!("macOS notification permission DENIED by user");
        }
    });

    center.requestAuthorizationWithOptions_completionHandler(options, &completion_handler);

    info!("Requested macOS notification permission");
}

impl MacOSBackend {
    /// Send a notification using UNUserNotificationCenter.
    ///
    /// # Arguments
    /// * `title` - Notification title
    /// * `body` - Notification body text
    /// * `image_path` - Optional path to local image file for attachment
    /// * `notification_id` - Unique identifier for the notification
    fn send_native_notification(
        &self,
        title: &str,
        body: &str,
        image_path: Option<&Path>,
        notification_id: &str,
    ) {
        use objc2_foundation::NSString;
        use objc2_user_notifications::{
            UNMutableNotificationContent, UNNotificationRequest, UNTimeIntervalNotificationTrigger,
            UNUserNotificationCenter,
        };

        // Get the notification center
        let center = UNUserNotificationCenter::currentNotificationCenter();

        // Create notification content
        let content = UNMutableNotificationContent::new();

        // Set title and body
        let ns_title = NSString::from_str(title);
        let ns_body = NSString::from_str(body);
        content.setTitle(&ns_title);
        content.setBody(&ns_body);

        // Add image attachment if provided (shows profile picture)
        if let Some(path) = image_path {
            if let Some(attachment) = self.create_attachment(path, notification_id) {
                let attachments = objc2_foundation::NSArray::from_retained_slice(&[attachment]);
                content.setAttachments(&attachments);
                debug!("Added profile picture attachment: {:?}", path);
            }
        }

        // Create a trigger with minimal delay.
        // Apple requires time interval triggers to be >= 1 second (shorter may be silently dropped).
        let trigger =
            UNTimeIntervalNotificationTrigger::triggerWithTimeInterval_repeats(1.0, false);

        // Create request
        let ns_id = NSString::from_str(notification_id);
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &ns_id,
            &content,
            Some(&trigger),
        );

        // Submit notification with completion handler to log errors
        let notification_id_owned = notification_id.to_string();
        let completion_handler =
            block2::RcBlock::new(move |error: *mut objc2_foundation::NSError| {
                if error.is_null() {
                    info!(
                        "macOS notification delivered successfully: {}",
                        &notification_id_owned[..notification_id_owned.len().min(8)]
                    );
                } else {
                    // SAFETY: error is non-null, we can dereference it
                    let err = unsafe { &*error };
                    error!(
                        "macOS notification FAILED: {} - {} (code={})",
                        &notification_id_owned[..notification_id_owned.len().min(8)],
                        err.localizedDescription(),
                        err.code()
                    );
                }
            });

        center.addNotificationRequest_withCompletionHandler(&request, Some(&completion_handler));

        debug!("macOS notification submitted: {}", notification_id);
    }

    /// Create a notification attachment from a local image file.
    ///
    /// Note: macOS MOVES the file to a system-managed location when creating an attachment.
    /// To preserve the cached file, we copy it to a temp location first.
    fn create_attachment(
        &self,
        path: &Path,
        identifier: &str,
    ) -> Option<Retained<objc2_user_notifications::UNNotificationAttachment>> {
        use objc2_foundation::{NSString, NSURL};
        use objc2_user_notifications::UNNotificationAttachment;

        // macOS deletes the original file when creating an attachment, so copy to temp
        let temp_path = std::env::temp_dir().join(format!(
            "notedeck_notif_{}.{}",
            identifier.chars().take(16).collect::<String>(),
            path.extension().and_then(|e| e.to_str()).unwrap_or("png")
        ));

        if let Err(e) = std::fs::copy(path, &temp_path) {
            error!(
                "Failed to copy image to temp for notification: {} - {:?}",
                e, path
            );
            return None;
        }

        // Convert path to file URL
        let path_str = temp_path.to_string_lossy();
        let ns_path = NSString::from_str(&path_str);
        let url = NSURL::fileURLWithPath(&ns_path);

        let ns_id = NSString::from_str(identifier);

        // Create attachment
        match unsafe {
            UNNotificationAttachment::attachmentWithIdentifier_URL_options_error(&ns_id, &url, None)
        } {
            Ok(attachment) => {
                debug!(
                    "Created notification attachment from {:?} (via temp {:?})",
                    path, temp_path
                );
                Some(attachment)
            }
            Err(e) => {
                error!("Failed to create notification attachment: {:?}", e);
                // Clean up temp file on error
                let _ = std::fs::remove_file(&temp_path);
                None
            }
        }
    }
}

impl Default for MacOSBackend {
    fn default() -> Self {
        Self::new()
    }
}

// No Drop impl needed - Retained<AnyObject> handles cleanup automatically

impl NotificationBackend for MacOSBackend {
    fn send_notification(
        &self,
        title: &str,
        body: &str,
        event: &ExtractedEvent,
        target_account: &str,
        picture_path: Option<&str>,
    ) {
        if !self.is_bundle_valid {
            debug!(
                "Skipping notification (no bundle): kind={} id={}",
                event.kind,
                safe_prefix(&event.id, 8),
            );
            return;
        }

        info!(
            "Sending macOS notification: kind={} id={} target={} picture={:?}",
            event.kind,
            safe_prefix(&event.id, 8),
            safe_prefix(target_account, 8),
            picture_path.map(|s| safe_prefix(s, 50)),
        );

        let image_path = picture_path.map(std::path::Path::new);
        self.send_native_notification(title, body, image_path, &event.id);
    }

    fn on_relay_status_changed(&self, connected_count: i32) {
        debug!("macOS backend: {} relays connected", connected_count);
    }
}
