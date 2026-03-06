//! Android theme detection for Rustdar
//! 
//! This crate provides Android-specific theme detection functionality
//! using JNI to query the Android system's UI mode.

/// Android theme detection using JNI
#[cfg(target_os = "android")]
pub fn detect_dark_theme() -> bool {
    use jni::objects::JObject;
    use jni::JavaVM;
    
    // Get the NDK context
    let ctx = ndk_context::android_context();
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.unwrap();
    let mut env = vm.attach_current_thread().unwrap();
    
    // Get the Activity context
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    
    // Get Resources from the context
    let resources = env.call_method(
        &activity,
        "getResources",
        "()Landroid/content/res/Resources;",
        &[]
    ).unwrap().l().unwrap();
    
    // Get Configuration from Resources
    let configuration = env.call_method(
        &resources,
        "getConfiguration",
        "()Landroid/content/res/Configuration;",
        &[]
    ).unwrap().l().unwrap();
    
    // Get uiMode from Configuration
    let ui_mode = env.get_field(
        &configuration,
        "uiMode",
        "I"
    ).unwrap().i().unwrap();
    
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