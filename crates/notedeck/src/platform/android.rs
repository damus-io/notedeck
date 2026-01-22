use crate::platform::{file::emit_selected_file, SelectedMedia};
use jni::{
    objects::{JByteArray, JClass, JObject, JObjectArray, JString},
    JNIEnv,
};
use std::sync::atomic::{AtomicI32, Ordering};
use tracing::{debug, error, info};

pub fn get_jvm() -> jni::JavaVM {
    unsafe { jni::JavaVM::from_raw(ndk_context::android_context().vm().cast()) }.unwrap()
}

// Thread-safe static global
static KEYBOARD_HEIGHT: AtomicI32 = AtomicI32::new(0);

/// This function is called by our main notedeck android activity when the
/// keyboard height changes. You can use [`virtual_keyboard_height`] to access
/// this
#[no_mangle]
pub extern "C" fn Java_com_damus_notedeck_KeyboardHeightHelper_nativeKeyboardHeightChanged(
    _env: jni::JNIEnv,
    _class: jni::objects::JClass,
    height: jni::sys::jint,
) {
    debug!("updating virtual keyboard height {}", height);

    // Convert and store atomically
    KEYBOARD_HEIGHT.store(height.max(0), Ordering::SeqCst);
}

/// Gets the current Android virtual keyboard height. Useful for transforming
/// the view
pub fn virtual_keyboard_height() -> i32 {
    KEYBOARD_HEIGHT.load(Ordering::SeqCst)
}

#[no_mangle]
pub extern "C" fn Java_com_damus_notedeck_MainActivity_nativeOnFilePickedFailed(
    mut env: JNIEnv,
    _class: JClass,
    juri: JString,
    je: JString,
) {
    let _uri: String = env.get_string(&juri).unwrap().into();
    let _error: String = env.get_string(&je).unwrap().into();
}

#[no_mangle]
pub extern "C" fn Java_com_damus_notedeck_MainActivity_nativeOnFilePickedWithContent(
    mut env: JNIEnv,
    _class: JClass,
    // [display_name, size, mime_type]
    juri_info: JObjectArray,
    jcontent: JByteArray,
) {
    debug!("File picked with content");

    let display_name: Option<String> = {
        let obj = env.get_object_array_element(&juri_info, 0).unwrap();
        if obj.is_null() {
            None
        } else {
            Some(env.get_string(&JString::from(obj)).unwrap().into())
        }
    };

    if let Some(display_name) = display_name {
        let length = env.get_array_length(&jcontent).unwrap() as usize;
        let mut content: Vec<i8> = vec![0; length];
        env.get_byte_array_region(&jcontent, 0, &mut content)
            .unwrap();

        debug!("selected file: {display_name:?} ({length:?} bytes)",);

        emit_selected_file(SelectedMedia::from_bytes(
            display_name,
            content.into_iter().map(|b| b as u8).collect(),
        ));
    } else {
        error!("Received null file name");
    }
}

pub fn try_open_file_picker() {
    match open_file_picker() {
        Ok(()) => {
            info!("File picker opened successfully");
        }
        Err(e) => {
            error!("Failed to open file picker: {}", e);
        }
    }
}

pub fn open_file_picker() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Get the Java VM from AndroidApp
    let vm = get_jvm();

    // Attach current thread to get JNI environment
    let mut env = vm.attach_current_thread()?;

    let context = unsafe { JObject::from_raw(ndk_context::android_context().context().cast()) };
    // Call the openFilePicker method on the MainActivity
    env.call_method(
        context,
        "openFilePicker",
        "()V", // Method signature: no parameters, void return
        &[],   // No arguments
    )?;

    Ok(())
}

// =============================================================================
// Notification Control API
// =============================================================================
//
// Architecture Note: JNI Callback State
// --------------------------------------
// The statics below exist because JNI callbacks cannot receive Rust state as parameters.
// Java calls these callbacks with a fixed signature (JNIEnv, JClass, ...args), with no
// way to pass an AppContext reference. Options considered:
//
// 1. **Current: Module statics** - JNI writes to statics, Rust app reads via API functions.
//    Simple, works, but scattered state.
//
// 2. **Polling Java via JNI** - Rust calls Java to check permission status.
//    Adds JNI overhead on every check.
//
// 3. **Store pointer in Java** - Pass Rust state pointer to Java, have Java pass it back.
//    Complex lifecycle management, potential UB if mismanaged.
//
// 4. **Global PlatformState struct** - Consolidate statics into one struct in OnceCell.
//    Better organization but still fundamentally global. Consider for future refactor.
//
// For now, we use approach #1 with clear documentation of the JNI constraint.

use std::sync::atomic::AtomicBool;

/// Thread-safe static for tracking notification permission request result.
///
/// NOTE: This global exists because JNI callbacks cannot receive Rust state as parameters.
/// Java's permission result callback stores the result here; Rust app code reads it via
/// `get_notification_permission_result()`. See architecture note above.
static NOTIFICATION_PERMISSION_GRANTED: AtomicBool = AtomicBool::new(false);

/// Tracks whether a permission request is currently in flight.
/// Set to true when `request_notification_permission()` is called,
/// cleared when `nativeOnNotificationPermissionResult` callback fires.
static NOTIFICATION_PERMISSION_PENDING: AtomicBool = AtomicBool::new(false);

/// Called from Java when notification permission request completes.
#[no_mangle]
pub extern "C" fn Java_com_damus_notedeck_MainActivity_nativeOnNotificationPermissionResult(
    _env: JNIEnv,
    _class: JClass,
    granted: jni::sys::jboolean,
) {
    let granted = granted != 0;
    debug!("Notification permission result: {}", granted);
    NOTIFICATION_PERMISSION_GRANTED.store(granted, Ordering::SeqCst);
    NOTIFICATION_PERMISSION_PENDING.store(false, Ordering::SeqCst);
}

/// Enable push notifications for the given pubkey and relay URLs.
/// Writes to SharedPreferences and starts the foreground service.
///
/// This is a convenience wrapper around `enable_notifications_multi` for single-account use.
pub fn enable_notifications(
    pubkey_hex: &str,
    relay_urls: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    enable_notifications_multi(&[pubkey_hex.to_string()], relay_urls)
}

/// Enable push notifications for multiple pubkeys and relay URLs.
/// Writes to SharedPreferences and starts the foreground service.
///
/// Supports multi-account notifications where events are attributed to the
/// appropriate account based on p-tag analysis.
pub fn enable_notifications_multi(
    pubkey_hexes: &[String],
    relay_urls: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let vm = get_jvm();
    let mut env = vm.attach_current_thread()?;
    let context = unsafe { JObject::from_raw(ndk_context::android_context().context().cast()) };

    // Serialize pubkeys as JSON array for multi-account support
    let pubkeys_json = serde_json::to_string(pubkey_hexes)?;
    let jpubkeys = env.new_string(&pubkeys_json)?;

    // Serialize relay URLs as JSON array
    let relays_json = serde_json::to_string(relay_urls)?;
    let jrelays = env.new_string(&relays_json)?;

    env.call_method(
        context,
        "enableNotificationsMulti",
        "(Ljava/lang/String;Ljava/lang/String;)V",
        &[
            jni::objects::JValue::Object(&jpubkeys.into()),
            jni::objects::JValue::Object(&jrelays.into()),
        ],
    )?;

    info!(
        "Notifications enabled for {} accounts with {} relays",
        pubkey_hexes.len(),
        relay_urls.len()
    );
    Ok(())
}

/// Disable push notifications.
/// Stops the foreground service and updates SharedPreferences.
pub fn disable_notifications() -> Result<(), Box<dyn std::error::Error>> {
    let vm = get_jvm();
    let mut env = vm.attach_current_thread()?;
    let context = unsafe { JObject::from_raw(ndk_context::android_context().context().cast()) };

    env.call_method(context, "disableNotifications", "()V", &[])?;

    info!("Notifications disabled");
    Ok(())
}

/// Check if notification permission is granted.
/// On Android 13+, requires POST_NOTIFICATIONS runtime permission.
pub fn is_notification_permission_granted() -> Result<bool, Box<dyn std::error::Error>> {
    let vm = get_jvm();
    let mut env = vm.attach_current_thread()?;
    let context = unsafe { JObject::from_raw(ndk_context::android_context().context().cast()) };

    let result = env.call_method(context, "isNotificationPermissionGranted", "()Z", &[])?;
    Ok(result.z()?)
}

/// Request notification permission from the user.
/// On Android 13+, shows system permission dialog.
/// Use `is_notification_permission_pending()` to check if request is in progress.
/// Use `get_notification_permission_result()` to get the result after completion.
pub fn request_notification_permission() -> Result<(), Box<dyn std::error::Error>> {
    NOTIFICATION_PERMISSION_PENDING.store(true, Ordering::SeqCst);

    let result = (|| {
        let vm = get_jvm();
        let mut env = vm.attach_current_thread()?;
        let context = unsafe { JObject::from_raw(ndk_context::android_context().context().cast()) };

        env.call_method(context, "requestNotificationPermission", "()V", &[])?;

        debug!("Notification permission requested");
        Ok(())
    })();

    // Reset pending flag if the JNI call failed
    if result.is_err() {
        NOTIFICATION_PERMISSION_PENDING.store(false, Ordering::SeqCst);
        error!("Failed to request notification permission, resetting pending flag");
    }

    result
}

/// Check if a notification permission request is currently pending.
pub fn is_notification_permission_pending() -> bool {
    NOTIFICATION_PERMISSION_PENDING.load(Ordering::SeqCst)
}

/// Get the result of the last notification permission request.
pub fn get_notification_permission_result() -> bool {
    NOTIFICATION_PERMISSION_GRANTED.load(Ordering::SeqCst)
}

/// Check if notifications are currently enabled in preferences.
pub fn are_notifications_enabled() -> Result<bool, Box<dyn std::error::Error>> {
    let vm = get_jvm();
    let mut env = vm.attach_current_thread()?;
    let context = unsafe { JObject::from_raw(ndk_context::android_context().context().cast()) };

    let result = env.call_method(context, "areNotificationsEnabled", "()Z", &[])?;
    Ok(result.z()?)
}

/// Check if the notification service is currently running.
pub fn is_notification_service_running() -> Result<bool, Box<dyn std::error::Error>> {
    let vm = get_jvm();
    let mut env = vm.attach_current_thread()?;
    let context = unsafe { JObject::from_raw(ndk_context::android_context().context().cast()) };

    let result = env.call_method(context, "isNotificationServiceRunning", "()Z", &[])?;
    Ok(result.z()?)
}

// =============================================================================
// Deep Link Handling
// =============================================================================
//
// See "Architecture Note: JNI Callback State" in Notification Control API section above.

use std::sync::RwLock;

/// Information about a deep link from a notification tap.
#[derive(Debug, Clone)]
pub struct DeepLinkInfo {
    pub event_id: String,
    pub event_kind: i32,
    pub author_pubkey: Option<String>,
}

/// Thread-safe storage for pending deep link.
///
/// Uses RwLock for read-heavy access pattern (UI polls frequently, writes only on notification tap).
/// Only one deep link can be pending at a time (latest wins).
///
/// NOTE: This global is required for JNI architecture - the callback from Java has no way
/// to receive Rust state as a parameter.
static PENDING_DEEP_LINK: RwLock<Option<DeepLinkInfo>> = RwLock::new(None);

/// Called from Java when user taps a notification.
/// Stores the deep link info for the main app to poll.
#[no_mangle]
pub extern "C" fn Java_com_damus_notedeck_MainActivity_nativeOnDeepLink(
    mut env: JNIEnv,
    _class: JClass,
    event_id: JString,
    event_kind: jni::sys::jint,
    author_pubkey: JString,
) {
    let event_id: String = match env.get_string(&event_id) {
        Ok(s) => s.into(),
        Err(e) => {
            error!("Failed to get event_id string: {}", e);
            return;
        }
    };

    let author_pubkey: Option<String> = {
        let s: String = env
            .get_string(&author_pubkey)
            .map(|s| s.into())
            .unwrap_or_default();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    };

    info!(
        "Deep link received: event_id={}, kind={}, author={}",
        &event_id[..8.min(event_id.len())],
        event_kind,
        author_pubkey
            .as_deref()
            .map(|p| &p[..8.min(p.len())])
            .unwrap_or("none")
    );

    let deep_link = DeepLinkInfo {
        event_id,
        event_kind,
        author_pubkey,
    };

    if let Ok(mut pending) = PENDING_DEEP_LINK.write() {
        *pending = Some(deep_link);
    } else {
        error!("Failed to acquire deep link write lock");
    }
}

/// Check if there's a pending deep link and consume it.
///
/// Returns `Some(DeepLinkInfo)` if a notification was tapped, `None` otherwise.
/// The deep link is cleared after this call.
///
/// Uses non-blocking `try_write()` to avoid blocking the render loop when the
/// JNI thread holds the lock. Returns `None` if lock is unavailable.
pub fn take_pending_deep_link() -> Option<DeepLinkInfo> {
    PENDING_DEEP_LINK
        .try_write()
        .ok()
        .and_then(|mut pending| pending.take())
}

/// Check if there's a pending deep link without consuming it.
///
/// Uses non-blocking `try_read()` to avoid blocking the render loop when the
/// JNI thread holds the lock. Returns `false` if lock is unavailable.
pub fn has_pending_deep_link() -> bool {
    PENDING_DEEP_LINK
        .try_read()
        .ok()
        .map(|pending| pending.is_some())
        .unwrap_or(false)
}
