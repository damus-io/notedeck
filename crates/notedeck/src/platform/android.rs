use std::sync::atomic::{AtomicI32, Ordering};
use tracing::debug;

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
    KEYBOARD_HEIGHT.store(height, Ordering::SeqCst);
}

/// Gets the current Android virtual keyboard height. Useful for transforming
/// the view
pub fn virtual_keyboard_height() -> i32 {
    KEYBOARD_HEIGHT.load(Ordering::SeqCst)
}
