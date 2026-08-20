//! The location runtime permission: query, request, and the tri-state model.

use super::with_activity;

/// Check whether the app holds either location runtime permission. FINE **or**
/// COARSE: since Android 12 the dialog offers "approximate only", which grants
/// COARSE and denies FINE, and the network provider serves fixes under COARSE.
pub(super) fn has_location_permission() -> bool {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    // checkSelfPermission is a Context method, so the Activity serves fine.
    with_activity(|env, activity| -> jni::errors::Result<bool> {
        for permission in [
            "android.permission.ACCESS_FINE_LOCATION",
            "android.permission.ACCESS_COARSE_LOCATION",
        ] {
            let perm = env.new_string(permission)?;
            let granted = env
                .call_method(
                    activity,
                    jni_str!("checkSelfPermission"),
                    jni_sig!("(Ljava/lang/String;)I"),
                    &[JValue::from(&perm)],
                )?
                .i()?;
            if granted == 0 {
                return Ok(true); // PERMISSION_GRANTED == 0
            }
        }
        Ok(false)
    })
    .and_then(Result::ok)
    .unwrap_or(false)
}

/// Request the location runtime permissions (FINE together with COARSE).
///
/// Shows the system dialog; the result is asynchronous, so poll
/// [`has_location_permission`] afterwards. The `bool` is whether the JNI call
/// was made, so a failure to reach `Activity.requestPermissions` is not mistaken
/// for a dialog the user dismissed.
///
/// **This runs off the main thread, which is not a supported context.**
/// `Activity.requestPermissions` goes on to `startActivityForResult` and sets
/// `mHasCurrentPermissionsRequest` without synchronisation, and the framework
/// expects both on the UI thread. There is no `checkThread()` assertion, so it
/// does not throw — whether the dialog appears can depend on the Activity's
/// lifecycle state. That is why the caller retries; the bound lives in
/// [`crate::LocationGate`].
pub(super) fn request_location_permission() -> bool {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    // requestPermissions() is Activity-only.
    let result = with_activity(|env, activity| -> jni::errors::Result<()> {
        // FINE and COARSE together, and the pairing is load-bearing: from
        // Android 12 a request for ACCESS_FINE_LOCATION *alone* is silently
        // discarded — no dialog, no callback — because the user must be offered
        // the "approximate" downgrade alongside it. Both are in the manifest.
        let fine = env.new_string("android.permission.ACCESS_FINE_LOCATION")?;
        let coarse = env.new_string("android.permission.ACCESS_COARSE_LOCATION")?;
        let string_class = env.find_class(jni_str!("java/lang/String"))?;
        let perm_array = env.new_object_array(2, &string_class, &fine)?;
        perm_array.set_element(env, 1, &coarse)?;

        env.call_method(
            activity,
            jni_str!("requestPermissions"),
            jni_sig!("([Ljava/lang/String;I)V"),
            &[JValue::from(&perm_array), JValue::Int(1)],
        )?;
        Ok(())
    });

    match result {
        Some(Ok(())) => true,
        Some(Err(e)) => {
            log::warn!("requestPermissions failed: {e:?}");
            false
        }
        None => {
            log::warn!("requestPermissions: no Activity yet, or JNI attach failed");
            false
        }
    }
}

/// Whether Android would show the permission dialog if asked right now. FINE
/// **or** COARSE, matching [`has_location_permission`].
///
/// `None` means the question could not be put at all — no `Activity` stashed
/// yet, a failed JNI attach, or a throw — which is a *different answer* from
/// `Some(false)`.
fn should_show_permission_rationale() -> Option<bool> {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    // shouldShowRequestPermissionRationale is Activity-only.
    with_activity(|env, activity| -> jni::errors::Result<bool> {
        for permission in [
            "android.permission.ACCESS_FINE_LOCATION",
            "android.permission.ACCESS_COARSE_LOCATION",
        ] {
            let perm = env.new_string(permission)?;
            let show = env
                .call_method(
                    activity,
                    jni_str!("shouldShowRequestPermissionRationale"),
                    jni_sig!("(Ljava/lang/String;)Z"),
                    &[JValue::from(&perm)],
                )?
                .z()?;
            if show {
                return Ok(true);
            }
        }
        Ok(false)
    })
    .and_then(|result| {
        result
            .inspect_err(|e| log::warn!("shouldShowRequestPermissionRationale failed: {e:?}"))
            .ok()
    })
}

/// What Android currently says about this app's access to the user's location.
/// `attempts` is what the gate has said about how many times this *install* has
/// asked.
///
/// Android's states are three and its API names two. `checkSelfPermission` says
/// granted or not; `shouldShowRequestPermissionRationale` splits "not" in two,
/// and is `true` in the *middle* case and `false` at both ends:
///
/// | asked before | rationale | means | reported |
/// |---|---|---|---|
/// | no  | `false` | nobody has been asked | `Prompt` |
/// | yes | `true`  | declined once; **the dialog will still show** | `Prompt` |
/// | yes | `false` | declined twice, or "don't ask again" | `Denied` |
///
/// Rows 1 and 3 are indistinguishable from inside the framework, which is why
/// `attempts` is a parameter. `None` from [`should_show_permission_rationale`]
/// is `Unknown` and never `Unavailable`, which is terminal.
pub(super) fn location_permission_status(attempts: u8) -> crate::LocationPermission {
    if has_location_permission() {
        return crate::LocationPermission::Granted;
    }
    // `has_location_permission` folds "no" and "could not ask" into the same
    // `false`, so the rationale call doubles as the reachability probe. Called
    // only past a `false`, so a granted permission costs one round trip, not two.
    permission_from_rationale(should_show_permission_rationale(), attempts)
}

/// The decision half of [`location_permission_status`], over plain values: above
/// it is JNI, in it is a table, and the table is where the mistakes live.
fn permission_from_rationale(rationale: Option<bool>, attempts: u8) -> crate::LocationPermission {
    use crate::LocationPermission;

    match (rationale, attempts) {
        // Could not ask. First frames of every launch; means "wait".
        (None, _) => LocationPermission::Unknown,
        // Declined once: Android still shows the dialog, so a working button.
        (Some(true), _) => LocationPermission::Prompt,
        // Declined twice, or "don't ask again": Android will not show it.
        (Some(false), 1..) => LocationPermission::Denied,
        // No rationale and never asked. A fresh install.
        (Some(false), 0) => LocationPermission::Prompt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocationPermission;
    use crate::android::java_context;

    // Host-run under `jni-typecheck`, so `cargo test --all-features`. There is no
    // JVM, so `JAVA` is empty and every helper degrades to its documented default
    // — which is the point: "no Activity yet" is a real Android state.

    /// `shouldShowRequestPermissionRationale` is `false` at *both* ends, so only
    /// the attempt count separates a fresh install from a permanently denied one.
    #[test]
    fn an_install_that_has_never_asked_is_offered_the_prompt() {
        assert_eq!(
            permission_from_rationale(Some(false), 0),
            LocationPermission::Prompt
        );
    }

    /// The middle row, and the one a `bool` would have lost.
    #[test]
    fn a_user_who_declined_once_can_still_be_asked() {
        for attempts in [0, 1, 2, u8::MAX] {
            assert_eq!(
                permission_from_rationale(Some(true), attempts),
                LocationPermission::Prompt,
                "rationale is Android saying the dialog will still show, and it \
                 outranks anything this app remembers"
            );
        }
    }

    /// Android silently auto-denies from here, so a button would raise a dialog
    /// that never appears.
    #[test]
    fn a_permanently_denied_install_is_reported_as_denied() {
        assert_eq!(
            permission_from_rationale(Some(false), 1),
            LocationPermission::Denied
        );
        assert_eq!(
            permission_from_rationale(Some(false), 2),
            LocationPermission::Denied
        );
    }

    /// **The state of every launch's first frames**, and the one that must never
    /// be `Unavailable` — which is terminal.
    #[test]
    fn a_device_with_no_activity_yet_is_unknown_rather_than_unavailable() {
        for attempts in [0, 1, 2] {
            assert_eq!(
                permission_from_rationale(None, attempts),
                LocationPermission::Unknown,
                "a question that could not be put was answered anyway"
            );
        }
    }

    /// The composed path on a host with no JVM, the same shape as an Android
    /// process before `android_main` has stashed its Activity.
    #[test]
    fn the_permission_query_waits_rather_than_guessing_when_java_is_unreachable() {
        assert!(
            java_context().is_none(),
            "the fixture has a JVM, so this proves nothing"
        );
        assert_eq!(location_permission_status(0), LocationPermission::Unknown);
        assert_eq!(location_permission_status(2), LocationPermission::Unknown);
    }
}
