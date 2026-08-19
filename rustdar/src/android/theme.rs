//! The OS dark-theme preference, read out of `Configuration.uiMode` over JNI.

use super::with_activity;

/// Read the OS dark-theme preference out of `Configuration.uiMode`.
///
/// Handed to the Android `PlatformBridge` by `entry::android_main` rather
/// than called from it. The bridge stays JNI-free by the injection rule in
/// this module tree's doc (`android/mod.rs`): injecting a `fn()` is the same
/// inversion `set_insets_querier` and `set_back_handler` use.
///
/// `dark-light` answers this on every other platform. It compiles for Android
/// and returns a wrong answer there, which is why this exists at all.
///
/// Every failure path answers `false` (light) rather than propagating, and here
/// that fallback is reachable rather than decorative: [`with_activity`] yields
/// `None` before [`android_main`] has stashed the Activity, and the theme poll
/// thread can call this at any time. This used to read `ndk_context`, where the
/// equivalent guard could never fire — `ndk_context::android_context()` panics
/// on its own `expect` if the context was never initialised, one line before
/// anything got to check a pointer.
pub fn detect_dark_theme() -> bool {
    use jni::{jni_sig, jni_str};

    // `getResources()` is declared on `Context`, so this resolved off the
    // Application that `ndk_context` hands out and was the one bridge in this
    // file that had never been broken by it. It goes through the Activity now
    // for the same reason the others do (see [`JAVA`]) -- one answer to "which
    // object is this" -- and with a small gain: the Activity's `Resources`
    // track the current configuration, which is exactly what is being read.
    let ui_mode = with_activity(|env, activity| -> jni::errors::Result<i32> {
        // Context.getResources().getConfiguration().uiMode
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
