//! Back-to-minimise: `Activity.moveTaskToBack(true)` over JNI.

use super::with_activity;

/// Minimize the app by calling `Activity.moveTaskToBack(true)` via JNI.
///
/// This keeps the app alive in recents with a proper thumbnail instead of
/// killing the process (which leaves a white box in recents).
///
/// This is where *every* back press that the UI did not want ends up, whichever
/// route it came in by: `KEYCODE_BACK` off the native input queue, or
/// `OnBackInvokedDispatcher` through [`Java_com_rustdar_BackHandler_nativeBackPressed`].
/// Both reach `App::resolve_back_press`, which asks the UI first and only then
/// calls `PlatformBridge::handle_back` — which is this.
///
/// `handle_back` reports `true` as soon as a handler is installed, so a no-op
/// here reads to the frontend as a handled press and the button does nothing at
/// all — which is what happened while this was reaching the Application instead
/// of the Activity. See [`JAVA`].
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
