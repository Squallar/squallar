//! The location runtime permission: query, request, and the tri-state model.

use super::with_activity;

/// Check whether the app holds either location runtime permission.
///
/// FINE **or** COARSE: since Android 12 the permission dialog offers
/// "approximate only", which grants COARSE and denies FINE. That is still a
/// usable grant -- the network provider serves fixes under COARSE alone (see
/// the per-provider fallback in [`last_known_location_with`]) -- so treating
/// FINE as the only "yes" would read a user who already answered as
/// unpermissioned and burn the bounded re-requests in
/// [`location_permission_status`] against them.
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
/// Shows the system permission dialog. The result is asynchronous; poll
/// [`has_location_permission`] afterwards to check the outcome.
///
/// Returns whether the JNI call was actually made. That is not the same as
/// "the user granted it" — it is the caller's cue that the request happened at
/// all, so a failure to reach `Activity.requestPermissions` is not mistaken for
/// a dialog the user dismissed. See [`start_location_thread`].
///
/// # This is called off the main thread, and that is not a supported context
///
/// `Activity.requestPermissions` goes on to `startActivityForResult` and sets
/// `mHasCurrentPermissionsRequest` without synchronisation. The framework
/// expects both on the UI thread; this runs on the winit event-loop thread,
/// which under `android-activity` is the dedicated native thread `android_main`
/// was started on and is not the UI thread either. It is
/// not a `checkThread()` assertion, so it does not throw — it is simply outside
/// what the framework guarantees, and whether the dialog appears can depend on
/// where the Activity is in its lifecycle when the call lands.
///
/// **That is why the caller retries, and why the retry must not be
/// "simplified" to a single attempt.** A `false` here is a request that did not
/// happen; treating it as a request the user declined is exactly the bug this
/// replaced. See the bounded retry in [`crate::LocationGate`],
/// which is what the return value below feeds.
///
/// (Only two nouns in the paragraph above have moved since it was written: the
/// thread the call comes in on, and where the retry lives. Every claim about
/// the framework, and the whole of why the `bool` exists, is unchanged — the
/// caller used to be [`start_location_thread`]'s bounded loop and is now the
/// permission gate, and *neither* is the UI thread.)
pub(super) fn request_location_permission() -> bool {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    // requestPermissions() is Activity-only -- see [`JAVA`] for why this used to
    // be reached through `ndk_context` and therefore never ran.
    let result = with_activity(|env, activity| -> jni::errors::Result<()> {
        // FINE and COARSE together, and the pairing is load-bearing: from
        // Android 12 -- which a targetSdk 34 build is squarely under -- a
        // request for ACCESS_FINE_LOCATION *alone* is silently discarded by
        // the framework: no dialog, no callback, because the user must be
        // offered the "approximate" downgrade alongside it. Each discarded
        // call still counted against the caller's bounded attempts, so both
        // were burned with the user never once asked. Both permissions are
        // declared in the manifest.
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

/// Whether Android would show the permission dialog if asked right now.
///
/// `Activity.shouldShowRequestPermissionRationale` is API 23+ and minSdk here is
/// 28, so it is always present. FINE **or** COARSE, matching
/// [`has_location_permission`]: either one still showing means a dialog would
/// appear.
///
/// `None` means the question could not be put at all — no `Activity` stashed
/// yet, a failed JNI attach, or a throw — and that is a *different answer* from
/// `Some(false)`, which is why this is not a `bool`. See
/// [`location_permission_status`].
///
/// It is a binder round trip to the package manager rather than a `View` call,
/// so calling it off the UI thread is ordinary; the hazard documented on
/// [`request_location_permission`] does not extend to it.
fn should_show_permission_rationale() -> Option<bool> {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    // shouldShowRequestPermissionRationale is Activity-only, like
    // requestPermissions -- see [`JAVA`].
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
///
/// Backs the arm's `permission` (the gate's `location_permission`, driven
/// in-crate since WO-RL-4 — the `LocationHooks` fn-pointer hop died with the
/// bridge verbs). `attempts` is what the gate has said about how many times
/// this *install* has asked; see the tri-state below for why it is needed and
/// the gate seam's `set_location_attempts` for where it comes from.
///
/// # Android's states are three, and its API names two of them
///
/// `checkSelfPermission` says granted or not. `shouldShowRequestPermissionRationale`
/// then splits "not" in two, and the split is counter-intuitive — it is `true`
/// in the *middle* case and `false` at both ends:
///
/// | asked before | rationale | means | reported |
/// |---|---|---|---|
/// | no  | `false` | nobody has been asked | `Prompt` |
/// | yes | `true`  | declined once; **the dialog will still show** | `Prompt` |
/// | yes | `false` | declined twice, or "don't ask again" | `Denied` |
///
/// Rows 1 and 3 are indistinguishable from inside the framework, which is the
/// entire reason `attempts` is a parameter. Collapsing them onto `Denied` would
/// render a fresh install with no button and no way in; collapsing them onto
/// `Prompt` would offer a button that raises a dialog Android silently refuses
/// to show, which reads as a broken app.
///
/// # `Unknown` is not `Unavailable`, and this is where that matters most
///
/// `None` from [`should_show_permission_rationale`] means the question could not
/// be put: no `Activity` in [`JAVA`] yet, or a JNI attach that failed. That is
/// the state of *every* Android launch for the first frames — `android_main`
/// stashes the context before `run_app`, but a resumed second Activity passes
/// through it too — and it must mean "wait", not "this device has no location
/// service". `Unavailable` is terminal: the gate stops polling, the settings
/// pane says location is not available on this platform, and the feature is
/// gone for the life of the process on a phone that has it.
///
pub(super) fn location_permission_status(attempts: u8) -> crate::LocationPermission {
    if has_location_permission() {
        return crate::LocationPermission::Granted;
    }
    // `has_location_permission` folds "no" and "could not ask" into the same
    // `false`, so the rationale call below doubles as the reachability probe. It
    // has to: answering `Denied` from a missing Activity would be a permanent
    // refusal recorded against a user who was never asked.
    //
    // Called only on the way past a `false`, so a granted permission costs one
    // binder round trip per poll rather than two.
    permission_from_rationale(should_show_permission_rationale(), attempts)
}

/// The decision half of [`location_permission_status`], over plain values.
///
/// Split out for the same reason [`provider_fix_quality`] is: everything above
/// it is JNI and everything in it is a table, and the table is where the three
/// mistakes live. See [`location_permission_status`] for what each row means and
/// why `None` is [`Unknown`](crate::LocationPermission::Unknown).
fn permission_from_rationale(rationale: Option<bool>, attempts: u8) -> crate::LocationPermission {
    use crate::LocationPermission;

    match (rationale, attempts) {
        // Could not ask. First frames of every launch; means "wait".
        (None, _) => LocationPermission::Unknown,
        // Declined once. Android will still show the dialog, so this is a
        // `Prompt` with a working button -- reporting it as `Denied` would be a
        // regression against what this app did before it modelled permissions
        // at all.
        (Some(true), _) => LocationPermission::Prompt,
        // No rationale and this install has asked: declined twice, or "don't
        // ask again". Android will not show the dialog, so neither will we.
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

    // Everything here runs on the *host*, under the `jni-typecheck` feature that
    // widens this crate's own `cfg` (see the top of the file), so it needs
    // `cargo test --all-features` -- a plain `cargo test --workspace` compiles
    // none of this crate. There is no JVM, so `JAVA` is empty and every helper
    // degrades to its documented default, which is not a limitation for two of
    // these tests but the point of them: "no Activity yet" is a real Android
    // state and the host reproduces it exactly.
    //
    // What cannot be tested here is anything that reaches Java: the permission
    // dialog, `shouldShowRequestPermissionRationale`'s real answers, and
    // `LocationHelper.start()`/`stop()`. Those need a device.

    // ── The permission tri-state ────────────────────────────────────────

    /// A fresh install has been asked nothing, and Android's API says exactly
    /// the same thing about a permanently-denied one:
    /// `shouldShowRequestPermissionRationale` is `false` at *both* ends. Only
    /// the app's own attempt count separates them, which is why it is a
    /// parameter and not a static somewhere.
    #[test]
    fn an_install_that_has_never_asked_is_offered_the_prompt() {
        assert_eq!(
            permission_from_rationale(Some(false), 0),
            LocationPermission::Prompt
        );
    }

    /// The middle row, and the one a `bool` would have lost. A user who
    /// declined once still gets the dialog from Android, so reporting `Denied`
    /// here would render the settings pane with no button and no way back — a
    /// regression against what this app did before it modelled permissions at
    /// all.
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

    /// No rationale *and* this install has asked: declined twice, or "don't ask
    /// again". Android silently auto-denies from here, so a button would raise
    /// a dialog that never appears — which reads to the user as a broken app.
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
    /// be `Unavailable`. `android_main` stashes the Activity before `run_app`,
    /// but a JNI attach can fail and a second Activity passes through the same
    /// window — and `Unavailable` is terminal: the gate stops polling, the
    /// settings pane says this platform has no location service, and the
    /// feature is gone for the life of the process on a phone that has it.
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

    /// The composed path, on a host with no JVM at all — which is the same
    /// shape as an Android process before `android_main` has stashed its
    /// Activity: `checkSelfPermission` cannot be reached, so it reports "not
    /// granted", and the rationale call cannot be reached either, so it reports
    /// nothing. Any answer but `Unknown` here is a permission decision invented
    /// out of a failed JNI attach.
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
