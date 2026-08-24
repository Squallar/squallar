//! Display density (pixels per dp) over JNI.

use super::with_activity;

/// Get the display density (pixels per dp) for converting physical to logical pixels.
pub(super) fn get_display_density() -> f32 {
    use jni::{jni_sig, jni_str};

    // getResources is a Context method, so this worked on the Application too,
    // but the Activity's Resources track the current configuration.
    with_activity(|env, activity| -> jni::errors::Result<f32> {
        // activity.getResources().getDisplayMetrics().density
        let resources = env
            .call_method(
                activity,
                jni_str!("getResources"),
                jni_sig!("()Landroid/content/res/Resources;"),
                &[],
            )?
            .l()?;
        let metrics = env
            .call_method(
                &resources,
                jni_str!("getDisplayMetrics"),
                jni_sig!("()Landroid/util/DisplayMetrics;"),
                &[],
            )?
            .l()?;
        env.get_field(&metrics, jni_str!("density"), jni_sig!("F"))?
            .f()
    })
    .and_then(Result::ok)
    .unwrap_or(1.0)
}
