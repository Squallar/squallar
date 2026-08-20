//! Back-to-minimise: `Activity.moveTaskToBack(true)` over JNI.

use super::with_activity;

/// Minimize the app by calling `Activity.moveTaskToBack(true)` via JNI.
///
/// This keeps the app alive in recents with a proper thumbnail instead of
/// killing the process.
/// This is where *every* back press the UI did not want ends up. `handle_back`
/// reports `true` as soon as a handler is installed, so a no-op here reads to
/// the frontend as a handled press and the button does nothing at all.
pub fn move_task_to_back() {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    let called = with_activity(|env, activity| {
        match env.call_method(
            activity,
            jni_str!("moveTaskToBack"),
            jni_sig!("(Z)Z"),
            &[JValue::Bool(true)],
        ) {
            Ok(_) => log::info!("App moved to background"),
            Err(e) => log::warn!("moveTaskToBack failed: {e:?}"),
        }
    });
    if called.is_none() {
        log::warn!("moveTaskToBack: no Activity yet, or JNI attach failed");
    }
}
