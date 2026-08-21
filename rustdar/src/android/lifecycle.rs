//! Whether the Activity under us is going away for good.
//!
//! Android delivers no event that distinguishes "the user backgrounded us"
//! from "this Activity is finishing" — winit reports `Suspended` for both,
//! because both destroy the window. `Activity.isFinishing()` is the
//! difference, and reading it is what lets the event loop end itself on the
//! second one and stay running on the first.

use super::with_activity;

/// `true` once `finish()` has been called on our Activity — which predictive
/// back's unclaimed press does — and `false` for an ordinary backgrounding.
///
/// **Why the app has to ask at all.** `NativeActivity.onDestroy()` reaches
/// android-activity's `notify_destroyed()`, which blocks the Java **UI
/// thread** in a condvar until the Rust `android_main` thread reports
/// `Stopped`. winit 0.30's Android backend does not act on
/// `MainEvent::Destroy` (it is an upstream `TODO` that logs and returns), so
/// `run_app` never returns, `android_main` never returns, and the UI thread
/// stays blocked until ActivityTaskManager gives up with an "Activity destroy
/// timeout". A second Activity created in that process then has a deadlocked
/// main thread behind it and never draws — measured, 2026-08-21, and it is
/// why the predictive-back opt-in was struck at WO-RP-3.
///
/// Reading this at `Suspended` gets ahead of that: the window is destroyed
/// before `onDestroy` runs, so the loop can wind itself down while the UI
/// thread is still free.
///
/// Every failure path answers `false`, and the fallback is reachable:
/// [`with_activity`] yields `None` before `android_main` has stashed the
/// Activity and after it has cleared it. `false` is the safe answer — it
/// means "treat this as a backgrounding", which is what the app did before
/// this probe existed.
pub fn activity_is_finishing() -> bool {
    use jni::{jni_sig, jni_str};

    let finishing = with_activity(|env, activity| -> jni::errors::Result<bool> {
        env.call_method(activity, jni_str!("isFinishing"), jni_sig!("()Z"), &[])?
            .z()
    });

    // `Option<Result<_>>`: the outer `None` is "no Activity, or the thread
    // would not attach", the inner `Err` is a JNI failure. Both mean "assume
    // this is a backgrounding", because exiting a loop we did not have to
    // exit loses the user's session.
    matches!(finishing, Some(Ok(true)))
}
