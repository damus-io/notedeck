use crate::platform::{file::emit_selected_file, NotificationMode, SelectedMedia};
use enostr::FullKeypair;
use jni::{
    objects::{JByteArray, JClass, JObject, JObjectArray, JString, JValue},
    sys::jobject,
    JNIEnv,
};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::RwLock;
use tracing::{debug, error, info, warn};

// =============================================================================
// Static Globals for JNI Callbacks
// =============================================================================
//
// JNI callbacks have fixed signatures `(JNIEnv, JClass, ...)` determined by the
// Java Native Interface specification. They cannot receive custom Rust state as
// parameters. These statics are the minimal set required:

/// Stores the current FCM token for access from Rust code.
static FCM_TOKEN: RwLock<Option<String>> = RwLock::new(None);

/// Stores the active account's keypair for NIP-98 HTTP authentication signing.
static SIGNING_KEYPAIR: RwLock<Option<FullKeypair>> = RwLock::new(None);

/// Whether a permission request is currently pending (UI state tracking).
static NOTIFICATION_PERMISSION_PENDING: AtomicBool = AtomicBool::new(false);

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

// ============================================================================
// FCM / Notepush JNI Functions
// ============================================================================

/// Returns the current FCM token, if available
pub fn get_fcm_token() -> Option<String> {
    match FCM_TOKEN.read() {
        Ok(guard) => guard.clone(),
        Err(e) => {
            error!("Failed to read FCM token: lock poisoned: {}", e);
            None
        }
    }
}

/// Sets the active account's keypair for NIP-98 signing.
/// Call this when the active account changes.
pub fn set_signing_keypair(keypair: Option<FullKeypair>) {
    match SIGNING_KEYPAIR.write() {
        Ok(mut guard) => {
            *guard = keypair;
            info!("Signing keypair updated for FCM registration");
        }
        Err(e) => {
            error!("Failed to update signing keypair: lock poisoned: {}", e);
        }
    }
}

/// Gets the current signing keypair, if available
pub fn get_signing_keypair() -> Option<FullKeypair> {
    match SIGNING_KEYPAIR.read() {
        Ok(guard) => guard.clone(),
        Err(e) => {
            error!("Failed to read signing keypair: lock poisoned: {}", e);
            None
        }
    }
}

/// Called by NotedeckFirebaseMessagingService when FCM token is refreshed
#[no_mangle]
pub extern "C" fn Java_com_damus_notedeck_service_NotedeckFirebaseMessagingService_nativeOnFcmTokenRefreshed(
    mut env: JNIEnv,
    _class: JClass,
    jtoken: JString,
) {
    let token: String = match env.get_string(&jtoken) {
        Ok(s) => s.into(),
        Err(e) => {
            error!("Failed to get FCM token string: {}", e);
            return;
        }
    };

    info!("FCM token refreshed: {}", truncate_content(&token, 20));

    // Store token for later use
    match FCM_TOKEN.write() {
        Ok(mut guard) => {
            *guard = Some(token);
        }
        Err(e) => {
            error!("Failed to store FCM token: lock poisoned: {}", e);
        }
    }

    // TODO: Trigger re-registration with notepush server if user has notifications enabled
}

/// Called by NotedeckFirebaseMessagingService to process incoming Nostr events
/// Returns a NotificationResult object or null
#[no_mangle]
pub extern "C" fn Java_com_damus_notedeck_service_NotedeckFirebaseMessagingService_nativeProcessNostrEvent(
    mut env: JNIEnv,
    _class: JClass,
    jevent_json: JString,
) -> jobject {
    let event_json: String = match env.get_string(&jevent_json) {
        Ok(s) => s.into(),
        Err(e) => {
            error!("Failed to get event JSON string: {}", e);
            return std::ptr::null_mut();
        }
    };

    debug!(
        "Processing Nostr event from FCM: {}",
        truncate_content(&event_json, 100)
    );

    // Parse the Nostr event and extract notification details
    // For now, return a simple notification - full implementation needs nostr crate integration
    let (title, body, event_id) = match parse_nostr_event_for_notification(&event_json) {
        Some(result) => result,
        None => {
            warn!("Failed to parse Nostr event for notification");
            return std::ptr::null_mut();
        }
    };

    // Create NotificationResult object to return to Kotlin
    match create_notification_result(&mut env, &title, &body, event_id.as_deref()) {
        Ok(obj) => obj,
        Err(e) => {
            error!("Failed to create NotificationResult: {}", e);
            std::ptr::null_mut()
        }
    }
}

/// Parses a Nostr event JSON and extracts notification title, body, and event ID.
///
/// Handles the following Nostr event kinds:
/// - Kind 1: Text note (mention)
/// - Kind 4: Encrypted direct message
/// - Kind 6: Repost
/// - Kind 7: Reaction (like, dislike, custom emoji)
/// - Kind 9735: Zap receipt
///
/// Returns `None` if the JSON is malformed or missing required fields.
fn parse_nostr_event_for_notification(
    event_json: &str,
) -> Option<(String, String, Option<String>)> {
    // Parse JSON to extract kind, content, and id
    let value: serde_json::Value = serde_json::from_str(event_json).ok()?;

    let kind = value.get("kind")?.as_u64()?;
    let content = value.get("content")?.as_str().unwrap_or("");
    let event_id = value.get("id")?.as_str().map(|s| s.to_string());

    let (title, body) = match kind {
        1 => ("New mention".to_string(), truncate_content(content, 100)),
        4 => (
            "New direct message".to_string(),
            "Contents are encrypted".to_string(),
        ),
        6 => (
            "Someone reposted".to_string(),
            truncate_content(content, 100),
        ),
        7 => {
            let reaction = match content {
                "" | "+" => "❤️",
                "-" => "👎",
                _ => content,
            };
            ("New reaction".to_string(), reaction.to_string())
        }
        9735 => ("Someone zapped you".to_string(), "".to_string()),
        _ => ("New activity".to_string(), truncate_content(content, 100)),
    };

    Some((title, body, event_id))
}

/// Truncates a string to a maximum number of characters (not bytes).
/// Appends "…" if truncation occurred. Safe for all UTF-8 strings.
fn truncate_content(content: &str, max_chars: usize) -> String {
    let char_count = content.chars().count();
    if char_count <= max_chars {
        content.to_string()
    } else {
        let truncated: String = content.chars().take(max_chars).collect();
        format!("{}…", truncated)
    }
}

/// Create a Kotlin NotificationResult object
fn create_notification_result(
    env: &mut JNIEnv,
    title: &str,
    body: &str,
    event_id: Option<&str>,
) -> Result<jobject, jni::errors::Error> {
    let class = env.find_class(
        "com/damus/notedeck/service/NotedeckFirebaseMessagingService$NotificationResult",
    )?;

    let jtitle = env.new_string(title)?;
    let jbody = env.new_string(body)?;
    let jevent_id = match event_id {
        Some(id) => JObject::from(env.new_string(id)?),
        None => JObject::null(),
    };

    let obj = env.new_object(
        class,
        "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
        &[
            JValue::Object(&jtitle.into()),
            JValue::Object(&jbody.into()),
            JValue::Object(&jevent_id),
        ],
    )?;

    Ok(obj.into_raw())
}

/// Called by NotepushClient to sign a NIP-98 auth header
/// Returns base64-encoded signed event, or null on error
#[no_mangle]
pub extern "C" fn Java_com_damus_notedeck_service_NotepushClient_nativeSignNip98Auth(
    mut env: JNIEnv,
    _class: JClass,
    jurl: JString,
    jmethod: JString,
    jbody: JString,
) -> jobject {
    let url: String = match env.get_string(&jurl) {
        Ok(s) => s.into(),
        Err(e) => {
            error!("Failed to get URL string: {}", e);
            return std::ptr::null_mut();
        }
    };

    let method: String = match env.get_string(&jmethod) {
        Ok(s) => s.into(),
        Err(e) => {
            error!("Failed to get method string: {}", e);
            return std::ptr::null_mut();
        }
    };

    let body: Option<String> = if jbody.is_null() {
        None
    } else {
        match env.get_string(&jbody) {
            Ok(s) => Some(s.into()),
            Err(e) => {
                warn!("Failed to get body string, treating as empty: {}", e);
                None
            }
        }
    };

    debug!("Signing NIP-98 auth for {} {}", method, url);

    // Get the signing keypair
    let keypair = match get_signing_keypair() {
        Some(kp) => kp,
        None => {
            error!("No signing keypair available for NIP-98 auth");
            return std::ptr::null_mut();
        }
    };

    // Sign the NIP-98 event
    match sign_nip98_event(&keypair, &url, &method, body.as_deref()) {
        Ok(base64_event) => match env.new_string(&base64_event) {
            Ok(jstr) => jstr.into_raw(),
            Err(e) => {
                error!("Failed to create Java string: {}", e);
                std::ptr::null_mut()
            }
        },
        Err(e) => {
            error!("Failed to sign NIP-98 event: {}", e);
            std::ptr::null_mut()
        }
    }
}

/// Signs a NIP-98 HTTP Auth event (kind 27235) for notepush API authentication.
///
/// Creates a Nostr event with:
/// - Kind 27235 (HTTP Auth)
/// - `u` tag: the request URL
/// - `method` tag: HTTP method (GET, POST, PUT, DELETE)
/// - `payload` tag: SHA-256 hash of request body (if present)
///
/// Returns the event as a base64-encoded JSON string, suitable for the
/// `Authorization: Nostr <base64>` header.
///
/// See: <https://github.com/nostr-protocol/nips/blob/master/98.md>
fn sign_nip98_event(
    keypair: &FullKeypair,
    url: &str,
    method: &str,
    body: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use nostr::{EventBuilder, Kind, Tag, TagKind};
    use sha2::{Digest, Sha256};

    let keys = nostr::Keys::new(keypair.secret_key.clone());

    // Build tags
    let mut tags = vec![
        Tag::custom(
            TagKind::SingleLetter(nostr::SingleLetterTag::lowercase(nostr::Alphabet::U)),
            vec![url.to_string()],
        ),
        Tag::custom(TagKind::Method, vec![method.to_string()]),
    ];

    // Add payload hash if body is present
    if let Some(body_content) = body {
        let mut hasher = Sha256::new();
        hasher.update(body_content.as_bytes());
        let hash = hasher.finalize();
        let hash_hex = hex::encode(hash);
        tags.push(Tag::custom(TagKind::Payload, vec![hash_hex]));
    }

    // Create and sign the event
    let event = EventBuilder::new(Kind::HttpAuth, "")
        .tags(tags)
        .sign_with_keys(&keys)?;

    // Serialize to JSON and base64 encode
    let event_json = serde_json::to_string(&event)?;
    let base64_encoded = STANDARD.encode(event_json.as_bytes());

    debug!("Signed NIP-98 event for {}", url);
    Ok(base64_encoded)
}

// =============================================================================
// Notification Control API
// =============================================================================

/// Get the current notification mode from Android SharedPreferences.
///
/// Queries the Kotlin side via JNI to get the persisted notification mode.
/// Returns `Disabled` if the JNI call fails.
#[profiling::function]
pub fn get_notification_mode() -> NotificationMode {
    load_notification_mode_from_prefs().unwrap_or(NotificationMode::Disabled)
}

/// Set the notification mode with mutual exclusivity handling.
///
/// This function ensures only one notification method is active at a time:
/// 1. Disables the current mode (if any)
/// 2. Enables the new mode
/// 3. Persists to SharedPreferences
///
/// # Arguments
/// * `mode` - The new notification mode to set
/// * `pubkey_hex` - The user's public key in hex format
/// * `relay_urls` - List of relay URLs for native mode
///
/// # Errors
/// Returns an error if JNI calls fail or if native mode is requested without relay URLs.
#[profiling::function]
pub fn set_notification_mode(
    mode: NotificationMode,
    pubkey_hex: &str,
    relay_urls: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let current = get_notification_mode();

    if current == mode {
        return Ok(());
    }

    // Disable current mode first (mutual exclusivity)
    match current {
        NotificationMode::Fcm => disable_fcm_notifications()?,
        NotificationMode::Native => disable_native_notifications()?,
        NotificationMode::Disabled => {}
    }

    // Enable new mode — if it fails, persist Disabled so state stays consistent
    match mode {
        NotificationMode::Fcm => {
            if let Err(e) = enable_fcm_notifications(pubkey_hex) {
                save_notification_mode_to_prefs(NotificationMode::Disabled)?;
                return Err(e);
            }
        }
        NotificationMode::Native => {
            if let Err(e) = enable_native_notifications(pubkey_hex, relay_urls) {
                save_notification_mode_to_prefs(NotificationMode::Disabled)?;
                return Err(e);
            }
        }
        NotificationMode::Disabled => {}
    }

    // Persist to SharedPreferences
    save_notification_mode_to_prefs(mode)?;

    info!("Notification mode changed from {:?} to {:?}", current, mode);
    Ok(())
}

/// Load notification mode from Android SharedPreferences
fn load_notification_mode_from_prefs() -> Result<NotificationMode, Box<dyn std::error::Error>> {
    let vm = get_jvm();
    let mut env = vm.attach_current_thread()?;
    let context = unsafe { JObject::from_raw(ndk_context::android_context().context().cast()) };

    let result = env.call_method(context, "getNotificationMode", "()I", &[])?;
    let mode_int = result.i()?;

    Ok(NotificationMode::from_index(mode_int as usize))
}

/// Save notification mode to Android SharedPreferences
fn save_notification_mode_to_prefs(
    mode: NotificationMode,
) -> Result<(), Box<dyn std::error::Error>> {
    let vm = get_jvm();
    let mut env = vm.attach_current_thread()?;
    let context = unsafe { JObject::from_raw(ndk_context::android_context().context().cast()) };

    env.call_method(
        context,
        "setNotificationMode",
        "(I)V",
        &[jni::objects::JValue::Int(mode.to_index() as i32)],
    )?;

    Ok(())
}

/// Enable FCM (Firebase Cloud Messaging) notifications
fn enable_fcm_notifications(pubkey_hex: &str) -> Result<(), Box<dyn std::error::Error>> {
    let vm = get_jvm();
    let mut env = vm.attach_current_thread()?;
    let context = unsafe { JObject::from_raw(ndk_context::android_context().context().cast()) };

    let jpubkey = env.new_string(pubkey_hex)?;

    env.call_method(
        context,
        "enableFcmNotifications",
        "(Ljava/lang/String;)V",
        &[jni::objects::JValue::Object(&jpubkey.into())],
    )?;

    info!(
        "FCM notifications enabled for {}",
        &pubkey_hex[..8.min(pubkey_hex.len())]
    );
    Ok(())
}

/// Disable FCM notifications
fn disable_fcm_notifications() -> Result<(), Box<dyn std::error::Error>> {
    let vm = get_jvm();
    let mut env = vm.attach_current_thread()?;
    let context = unsafe { JObject::from_raw(ndk_context::android_context().context().cast()) };

    env.call_method(context, "disableFcmNotifications", "()V", &[])?;

    info!("FCM notifications disabled");
    Ok(())
}

/// Enable native (direct relay) notifications
fn enable_native_notifications(
    pubkey_hex: &str,
    relay_urls: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    if relay_urls.is_empty() {
        warn!("Cannot enable native notifications: no relay URLs configured");
        return Err("No relay URLs configured".into());
    }

    let vm = get_jvm();
    let mut env = vm.attach_current_thread()?;
    let context = unsafe { JObject::from_raw(ndk_context::android_context().context().cast()) };

    let jpubkey = env.new_string(pubkey_hex)?;
    let relays_json = serde_json::to_string(relay_urls)?;
    let jrelays = env.new_string(&relays_json)?;

    env.call_method(
        context,
        "enableNativeNotifications",
        "(Ljava/lang/String;Ljava/lang/String;)V",
        &[
            jni::objects::JValue::Object(&jpubkey.into()),
            jni::objects::JValue::Object(&jrelays.into()),
        ],
    )?;

    info!(
        "Native notifications enabled for {} with {} relays",
        &pubkey_hex[..8.min(pubkey_hex.len())],
        relay_urls.len()
    );
    Ok(())
}

/// Disable native notifications
fn disable_native_notifications() -> Result<(), Box<dyn std::error::Error>> {
    let vm = get_jvm();
    let mut env = vm.attach_current_thread()?;
    let context = unsafe { JObject::from_raw(ndk_context::android_context().context().cast()) };

    env.call_method(context, "disableNativeNotifications", "()V", &[])?;

    info!("Native notifications disabled");
    Ok(())
}

/// Check if notification permission is granted
pub fn is_notification_permission_granted() -> Result<bool, Box<dyn std::error::Error>> {
    let vm = get_jvm();
    let mut env = vm.attach_current_thread()?;
    let context = unsafe { JObject::from_raw(ndk_context::android_context().context().cast()) };

    let result = env.call_method(context, "isNotificationPermissionGranted", "()Z", &[])?;
    Ok(result.z()?)
}

/// Request notification permission from the user
pub fn request_notification_permission() -> Result<(), Box<dyn std::error::Error>> {
    NOTIFICATION_PERMISSION_PENDING.store(true, Ordering::SeqCst);

    let vm = get_jvm();
    let mut env = vm.attach_current_thread()?;
    let context = unsafe { JObject::from_raw(ndk_context::android_context().context().cast()) };

    env.call_method(context, "requestNotificationPermission", "()V", &[])?;

    debug!("Notification permission requested");
    Ok(())
}

/// Check if a notification permission request is pending
pub fn is_notification_permission_pending() -> bool {
    NOTIFICATION_PERMISSION_PENDING.load(Ordering::SeqCst)
}

/// Called from Java when notification permission request completes
#[no_mangle]
pub extern "C" fn Java_com_damus_notedeck_MainActivity_nativeOnNotificationPermissionResult(
    _env: JNIEnv,
    _class: JClass,
    granted: jni::sys::jboolean,
) {
    let granted = granted != 0;
    debug!("Notification permission result: {}", granted);
    NOTIFICATION_PERMISSION_PENDING.store(false, Ordering::SeqCst);
}
