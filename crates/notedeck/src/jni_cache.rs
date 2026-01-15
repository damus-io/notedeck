//! JNI ClassLoader cache for Android.
//!
//! On Android, when Rust code runs on a native thread and tries to call Java
//! via JNI's `FindClass`, it uses the thread's classloader which doesn't have
//! access to app classes. This module caches the app's ClassLoader when Java
//! passes it to us at startup.

#[cfg(target_os = "android")]
use jni::{
    objects::{GlobalRef, JClass, JObject, JString, JValue},
    sys::jobject,
    JNIEnv, JavaVM,
};

#[cfg(target_os = "android")]
use std::sync::OnceLock;

#[cfg(target_os = "android")]
static JNI_CACHE: OnceLock<JniCache> = OnceLock::new();

#[cfg(target_os = "android")]
struct JniCache {
    vm: JavaVM,
    class_loader: GlobalRef,
}

// JniCache is safe to share across threads because:
// - JavaVM is designed to be shared across threads
// - GlobalRef prevents the ClassLoader from being garbage collected
#[cfg(target_os = "android")]
unsafe impl Send for JniCache {}
#[cfg(target_os = "android")]
unsafe impl Sync for JniCache {}

/// JNI function called from Java to initialize the classloader cache.
///
/// This must be called from MainActivity.onCreate() or similar, passing
/// the Activity's class loader.
///
/// Java signature: `public static native void initClassLoader(ClassLoader loader);`
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_damus_notedeck_MainActivity_initClassLoader(
    mut env: JNIEnv,
    _class: JClass,
    class_loader: JObject,
) {
    if JNI_CACHE.get().is_some() {
        tracing::debug!("JNI cache already initialized");
        return;
    }

    if class_loader.is_null() {
        tracing::error!("initClassLoader called with null classloader");
        return;
    }

    // Get the JavaVM from the environment
    let vm = match env.get_java_vm() {
        Ok(vm) => vm,
        Err(e) => {
            tracing::error!("Failed to get JavaVM: {}", e);
            return;
        }
    };

    // Create a global reference so the ClassLoader isn't garbage collected
    let class_loader_global = match env.new_global_ref(class_loader) {
        Ok(global) => global,
        Err(e) => {
            tracing::error!("Failed to create global ref for class loader: {}", e);
            return;
        }
    };

    let cache = JniCache {
        vm,
        class_loader: class_loader_global,
    };

    if JNI_CACHE.set(cache).is_err() {
        tracing::warn!("JNI cache was already set by another thread");
    } else {
        tracing::info!("JNI classloader cache initialized successfully from Java");
    }
}

/// Initialize stub - actual initialization happens via JNI from Java.
#[cfg(target_os = "android")]
pub fn init() -> Result<(), String> {
    // This is now a no-op; initialization happens when Java calls initClassLoader
    Ok(())
}

/// Check if the JNI cache is initialized.
#[cfg(target_os = "android")]
pub fn is_initialized() -> bool {
    JNI_CACHE.get().is_some()
}

/// Find a class using the cached app classloader.
///
/// Unlike `JNIEnv::find_class`, this works from any thread because it uses
/// the app's classloader that was cached at startup.
#[cfg(target_os = "android")]
pub fn find_class<'a>(env: &mut JNIEnv<'a>, name: &str) -> Result<JClass<'a>, String> {
    let cache = JNI_CACHE.get().ok_or_else(|| {
        "JNI cache not initialized. Java must call initClassLoader() first.".to_string()
    })?;

    // Convert class name to Java format (com/foo/Bar -> com.foo.Bar)
    let java_name = name.replace('/', ".");

    let class_name: JString = env
        .new_string(&java_name)
        .map_err(|e| format!("Failed to create class name string: {e}"))?;

    // Clear any pending exception before calling loadClass
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }

    // Convert GlobalRef to a local reference for use in this thread/env
    let class_loader_local: JObject = env
        .new_local_ref(cache.class_loader.as_obj())
        .map_err(|e| format!("Failed to create local ref for class loader: {e}"))?;

    // Convert JString to JObject for the JValue argument
    let class_name_obj: JObject = class_name.into();

    let class_obj: JObject = env
        .call_method(
            &class_loader_local,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValue::Object(&class_name_obj)],
        )
        .map_err(|e| {
            // Clear exception to prevent crash on subsequent JNI calls
            let _ = env.exception_clear();
            format!("Failed to load class {}: {e}", java_name)
        })?
        .l()
        .map_err(|e| format!("Failed to convert class to object: {e}"))?;

    // SAFETY: We know this is a Class object because loadClass returns Class<?>
    Ok(class_obj.into())
}

/// Execute a closure with a JNIEnv attached to the current thread.
#[cfg(target_os = "android")]
pub fn with_jni<F, R>(f: F) -> Result<R, String>
where
    F: FnOnce(&mut JNIEnv) -> Result<R, String>,
{
    let cache = JNI_CACHE.get().ok_or_else(|| {
        "JNI cache not initialized. Java must call initClassLoader() first.".to_string()
    })?;

    let mut env = cache
        .vm
        .attach_current_thread()
        .map_err(|e| format!("Failed to attach thread: {e}"))?;

    f(&mut env)
}

// Stub implementations for non-Android platforms
#[cfg(not(target_os = "android"))]
pub fn init() -> Result<(), String> {
    Ok(())
}

#[cfg(not(target_os = "android"))]
pub fn is_initialized() -> bool {
    true
}
