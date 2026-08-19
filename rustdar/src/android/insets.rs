//! System window insets (status bar, navigation bar, cutouts) over JNI.

use super::with_activity;

/// Query the system window insets (status bar, navigation bar) in physical pixels.
/// Returns (top, bottom, left, right) inset values.
///
/// # Called from the winit thread, not the UI thread
///
/// `View`/`ViewRootImpl` are main-thread-only by contract. `getWindowInsets`
/// happens not to `checkThread()`, so this does not throw — but what it returns
/// is whatever `ViewRootImpl` last computed, not a fresh measurement. Before
/// the first layout that is the null this function already guards for, which is
/// exactly why [`android_main`] defers the query to a callback fired on the
/// first `resumed()` rather than calling it at startup.
///
/// The practical consequence is a stale read after a configuration change until
/// the next layout, not a wrong-object read. Prior to the [`JAVA`] fix this
/// returned `(0,0,0,0)` unconditionally, so any real value is new behaviour
/// that has not been observed on a device.
pub fn get_system_insets() -> (f32, f32, f32, f32) {
    with_activity(system_insets_with).unwrap_or((0.0, 0.0, 0.0, 0.0))
}

/// Body of [`get_system_insets`], split out so it can `return` early from inside
/// the `Env` closure that jni 0.22's attachment API requires.
fn system_insets_with(
    env: &mut jni::Env<'_>,
    activity: &jni::objects::JObject<'_>,
) -> (f32, f32, f32, f32) {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    // `activity` is the real Activity -- see [`JAVA`]. This used to take
    // whatever `ndk_context` held and guard with `is_instance_of(_, Activity)`,
    // which on android-activity 0.6 is the Application and so failed every
    // single time: the insets returned here were unconditionally (0,0,0,0), on
    // every API level, and the map laid its excluded_rects out against a
    // full-bleed rect that ignored cutouts and the navigation bar.
    //
    // Activity.getWindow().getDecorView().getRootWindowInsets()
    let window = match env.call_method(
        activity,
        jni_str!("getWindow"),
        jni_sig!("()Landroid/view/Window;"),
        &[],
    ) {
        Ok(w) => match w.l() {
            Ok(w) => w,
            Err(_) => return (0.0, 0.0, 0.0, 0.0),
        },
        Err(_) => return (0.0, 0.0, 0.0, 0.0),
    };
    let decor = match env.call_method(
        &window,
        jni_str!("getDecorView"),
        jni_sig!("()Landroid/view/View;"),
        &[],
    ) {
        Ok(v) => match v.l() {
            Ok(v) => v,
            Err(_) => return (0.0, 0.0, 0.0, 0.0),
        },
        Err(_) => return (0.0, 0.0, 0.0, 0.0),
    };
    let insets_obj = match env.call_method(
        &decor,
        jni_str!("getRootWindowInsets"),
        jni_sig!("()Landroid/view/WindowInsets;"),
        &[],
    ) {
        Ok(i) => match i.l() {
            Ok(i) if !i.is_null() => i,
            _ => return (0.0, 0.0, 0.0, 0.0),
        },
        Err(_) => return (0.0, 0.0, 0.0, 0.0),
    };

    // On API 30+, use getInsets(WindowInsets.Type.systemBars())
    // On older APIs, use getSystemWindowInset*()
    let (top, bottom, left, right) = if android_api_level(env) >= 30 {
        // WindowInsets.Type.systemBars() returns a bitmask
        let type_class = match env.find_class(jni_str!("android/view/WindowInsets$Type")) {
            Ok(c) => c,
            Err(_) => return get_legacy_insets(env, &insets_obj),
        };
        let type_mask =
            match env.call_static_method(&type_class, jni_str!("systemBars"), jni_sig!("()I"), &[])
            {
                Ok(v) => match v.i() {
                    Ok(v) => v,
                    Err(_) => return get_legacy_insets(env, &insets_obj),
                },
                Err(_) => return get_legacy_insets(env, &insets_obj),
            };
        let insets_result = env.call_method(
            &insets_obj,
            jni_str!("getInsets"),
            jni_sig!("(I)Landroid/graphics/Insets;"),
            &[JValue::Int(type_mask)],
        );
        match insets_result {
            Ok(val) => {
                let insets = match val.l() {
                    Ok(i) if !i.is_null() => i,
                    _ => return get_legacy_insets(env, &insets_obj),
                };
                let t = env
                    .get_field(&insets, jni_str!("top"), jni_sig!("I"))
                    .map(|v| v.i().unwrap_or(0))
                    .unwrap_or(0);
                let b = env
                    .get_field(&insets, jni_str!("bottom"), jni_sig!("I"))
                    .map(|v| v.i().unwrap_or(0))
                    .unwrap_or(0);
                let l = env
                    .get_field(&insets, jni_str!("left"), jni_sig!("I"))
                    .map(|v| v.i().unwrap_or(0))
                    .unwrap_or(0);
                let r = env
                    .get_field(&insets, jni_str!("right"), jni_sig!("I"))
                    .map(|v| v.i().unwrap_or(0))
                    .unwrap_or(0);
                (t as f32, b as f32, l as f32, r as f32)
            }
            Err(_) => get_legacy_insets(env, &insets_obj),
        }
    } else {
        get_legacy_insets(env, &insets_obj)
    };

    (top, bottom, left, right)
}

/// Fallback for Android < API 30: use deprecated getSystemWindowInset*() methods.
fn get_legacy_insets(
    env: &mut jni::Env<'_>,
    insets_obj: &jni::objects::JObject<'_>,
) -> (f32, f32, f32, f32) {
    use jni::{jni_sig, jni_str};

    let top = env
        .call_method(
            insets_obj,
            jni_str!("getSystemWindowInsetTop"),
            jni_sig!("()I"),
            &[],
        )
        .map(|v| v.i().unwrap_or(0))
        .unwrap_or(0);
    let bottom = env
        .call_method(
            insets_obj,
            jni_str!("getSystemWindowInsetBottom"),
            jni_sig!("()I"),
            &[],
        )
        .map(|v| v.i().unwrap_or(0))
        .unwrap_or(0);
    let left = env
        .call_method(
            insets_obj,
            jni_str!("getSystemWindowInsetLeft"),
            jni_sig!("()I"),
            &[],
        )
        .map(|v| v.i().unwrap_or(0))
        .unwrap_or(0);
    let right = env
        .call_method(
            insets_obj,
            jni_str!("getSystemWindowInsetRight"),
            jni_sig!("()I"),
            &[],
        )
        .map(|v| v.i().unwrap_or(0))
        .unwrap_or(0);
    (top as f32, bottom as f32, left as f32, right as f32)
}

/// Get the Android API level.
///
/// Takes the caller's `Env` rather than attaching its own: jni 0.22 attachments
/// push a JNI stack frame, and nesting one inside `system_insets_with` would put
/// the local references it is holding out of the top frame.
fn android_api_level(env: &mut jni::Env<'_>) -> i32 {
    use jni::{jni_sig, jni_str};

    let Ok(build_class) = env.find_class(jni_str!("android/os/Build$VERSION")) else {
        return 0;
    };
    env.get_static_field(&build_class, jni_str!("SDK_INT"), jni_sig!("I"))
        .map(|v| v.i().unwrap_or(0))
        .unwrap_or(0)
}
