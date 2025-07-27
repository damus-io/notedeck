use crossbeam_channel::{unbounded, Receiver, Sender};
use jni::{
    objects::{JByteArray, JClass, JObject, JObjectArray, JString, JValue},
    JNIEnv,
};
use once_cell::sync::Lazy;

use std::sync::atomic::{AtomicI32, Ordering};
use tracing::{debug, error, info};

pub fn get_jvm() -> jni::JavaVM {
    unsafe { jni::JavaVM::from_raw(ndk_context::android_context().vm().cast()) }.unwrap()
}

pub struct SelectedFileChannel {
    sender: Sender<(String, Vec<i8>)>,
    receiver: Receiver<(String, Vec<i8>)>,
}

impl SelectedFileChannel {
    pub fn new() -> Self {
        let (sender, receiver) = unbounded();
        Self { sender, receiver }
    }

    pub fn new_selected_file(&self, uri: String, content: Vec<i8>) {
        let _ = self.sender.send((uri, content));
    }

    pub fn try_receive(&self) -> Option<(String, Vec<i8>)> {
        self.receiver.try_recv().ok()
    }

    pub fn receive(&self) -> Option<(String, Vec<i8>)> {
        self.receiver.recv().ok()
    }
}

pub static SELECTED_FILE_CHANNEL: Lazy<SelectedFileChannel> =
    Lazy::new(|| SelectedFileChannel::new());

pub fn emit_selected_file(uri: String, content: Vec<i8>) {
    SELECTED_FILE_CHANNEL.new_selected_file(uri, content);
}

pub fn get_next_selected_file() -> Option<(String, Vec<i8>)> {
    SELECTED_FILE_CHANNEL.try_receive()
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
    KEYBOARD_HEIGHT.store(height as i32, Ordering::SeqCst);
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
        let size: Option<i64> = {
            let obj = env.get_object_array_element(&juri_info, 1).unwrap();
            if obj.is_null() {
                None
            } else {
                Some(
                    JValue::Object(&env.get_object_array_element(&juri_info, 0).unwrap())
                        .j()
                        .unwrap_or(0),
                )
            }
        };

        let length = env.get_array_length(&jcontent).unwrap() as usize;
        let mut content: Vec<i8> = vec![0; length];
        env.get_byte_array_region(&jcontent, 0, &mut content)
            .unwrap();

        debug!(
            "selected file: {:?} ({} bytes)",
            display_name,
            size.unwrap_or(0)
        );

        emit_selected_file(display_name, content.clone());
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
