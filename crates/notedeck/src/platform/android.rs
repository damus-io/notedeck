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

pub fn vibrate(duration_ms: i64) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let vm = get_jvm();
    let mut env = vm.attach_current_thread()?;
    let context = unsafe { JObject::from_raw(ndk_context::android_context().context().cast()) };
    env.call_method(
        context,
        "vibrate",
        "(J)V",
        &[jni::objects::JValue::Long(duration_ms)],
    )?;
    Ok(())
}

pub fn try_vibrate() {
    match vibrate(200) {
        Ok(()) => {
            info!("Vibration triggered");
        }
        Err(e) => {
            error!("Failed to vibrate: {}", e);
        }
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
