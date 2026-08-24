//! The OS dark-theme preference, read out of `Configuration.uiMode` over JNI.

use super::with_activity;

/// Read the OS dark-theme preference out of `Configuration.uiMode`.
///
/// Handed to the Android `PlatformBridge` by `entry::android_main` rather than
/// called from it, so the bridge stays JNI-free.
///
/// `dark-light` compiles for Android and returns a wrong answer there, which
/// is why this exists.
/// Every failure path answers `false` (light) rather than propagating, and the
/// fallback is reachable: [`with_activity`] yields `None` before
/// [`android_main`] has stashed the Activity.
pub fn detect_dark_theme() -> bool {
    use jni::{jni_sig, jni_str};

    // `getResources()` is declared on `Context`, but goes through the Activity
    // (see [`JAVA`]) because the Activity's `Resources` track the current
    // configuration, which is what is being read.
    let ui_mode = with_activity(|env, activity| -> jni::errors::Result<i32> {
        // Context.getResources().getConfiguration().uiMode.
        let resources = env
            .call_method(
                activity,
                jni_str!("getResources"),
                jni_sig!("()Landroid/content/res/Resources;"),
                &[],
            )?
            .l()?;
        let configuration = env
            .call_method(
                &resources,
                jni_str!("getConfiguration"),
                jni_sig!("()Landroid/content/res/Configuration;"),
                &[],
            )?
            .l()?;

        env.get_field(&configuration, jni_str!("uiMode"), jni_sig!("I"))?
            .i()
    });

    // `Option<Result<_>>`: the outer `None` is "no Activity yet, or the thread
    // would not attach", the inner `Err` is a JNI failure. Both mean light.
    let Some(Ok(ui_mode)) = ui_mode else {
        return false;
    };

    // Configuration.UI_MODE_NIGHT_MASK / UI_MODE_NIGHT_YES.
    const UI_MODE_NIGHT_MASK: i32 = 0x30;
    const UI_MODE_NIGHT_YES: i32 = 0x20;

    (ui_mode & UI_MODE_NIGHT_MASK) == UI_MODE_NIGHT_YES
}
