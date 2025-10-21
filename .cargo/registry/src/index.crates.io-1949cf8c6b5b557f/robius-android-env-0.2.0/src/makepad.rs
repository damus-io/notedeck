use makepad_android_state::{get_activity, get_java_vm};

pub fn with_activity_inner<F, R>(f: F) -> crate::Result<R>
where
    F: for<'a, 'b, 'c, 'd> FnOnce(&'a mut crate::JNIEnv<'b>, &'c crate::JObject<'d>) -> R,
{
    let jvm = unsafe { crate::JavaVM::from_raw(get_java_vm().cast()) }?;
    let mut jni_env = jvm.attach_current_thread_permanently()?;

    let activity = unsafe { crate::JObject::from_raw(get_activity().cast()) };
    Ok(f(&mut jni_env, &activity))
}
