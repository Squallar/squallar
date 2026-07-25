//! Android theme detection for Rustdar
//!
//! This crate provides Android-specific theme detection functionality
//! using JNI to query the Android system's UI mode.

/// Wrap the process `JavaVM` that `ndk_context` holds.
///
/// jni 0.22 made `JavaVM::from_raw` infallible, but it *asserts* the pointer is
/// non-null where 0.21 returned a `Result`. Checking here keeps the pre-0.22
/// behaviour of falling back to a default instead of panicking if `ndk_context`
/// has not been initialised yet.
#[cfg(target_os = "android")]
fn android_vm() -> Option<jni::vm::JavaVM> {
    let vm = ndk_context::android_context().vm();
    if vm.is_null() {
        return None;
    }
    // SAFETY: non-null, and `ndk_context` guarantees it is the process JavaVM.
    Some(unsafe { jni::vm::JavaVM::from_raw(vm.cast()) })
}

/// Android theme detection using JNI
#[cfg(target_os = "android")]
pub fn detect_dark_theme() -> bool {
    use jni::objects::JObject;
    use jni::{jni_sig, jni_str};

    // Get the NDK context
    let ctx = ndk_context::android_context();
    let Some(vm) = android_vm() else { return false };
    let context = ctx.context();

    // jni 0.22: the environment is only ever handed out as a `&mut Env` borrowed
    // for the duration of a closure, so all JNI work happens inside here.
    let ui_mode = vm.attach_current_thread(|env| -> jni::errors::Result<i32> {
        // Get the Activity context
        let activity = unsafe { JObject::from_raw(env, context.cast()) };

        // Get Resources from the context
        let resources = env
            .call_method(
                &activity,
                jni_str!("getResources"),
                jni_sig!("()Landroid/content/res/Resources;"),
                &[],
            )?
            .l()?;

        // Get Configuration from Resources
        let configuration = env
            .call_method(
                &resources,
                jni_str!("getConfiguration"),
                jni_sig!("()Landroid/content/res/Configuration;"),
                &[],
            )?
            .l()?;

        // Get uiMode from Configuration
        env.get_field(&configuration, jni_str!("uiMode"), jni_sig!("I"))?
            .i()
    });

    let Ok(ui_mode) = ui_mode else { return false };

    // Check if UI_MODE_NIGHT_YES is set (0x20)
    const UI_MODE_NIGHT_MASK: i32 = 0x30;
    const UI_MODE_NIGHT_YES: i32 = 0x20;

    (ui_mode & UI_MODE_NIGHT_MASK) == UI_MODE_NIGHT_YES
}

/// Stub for non-Android platforms
#[cfg(not(target_os = "android"))]
pub fn detect_dark_theme() -> bool {
    false
}
