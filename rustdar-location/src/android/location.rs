//! Location fixes over JNI: `LocationManager` reads, the `LocationHelper`
//! subscription, and the 10 s poll thread that feeds the app.

use super::permissions::{has_location_permission, request_location_permission};
use super::{with_activity, with_env};

/// Prompt if Android needs prompting, and start delivering fixes.
///
/// Backs the arm's `request` (the gate's `request_location`, driven in-crate
/// since WO-RL-4 — the `LocationHooks` fn-pointer hop died with the bridge
/// verbs). The two halves are one method because the gate has one question —
/// "make location happen" — and which of the two it means is a thing only the
/// platform knows.
///
/// The `bool` is the one this verb is honest about anywhere, and both
/// branches answer the same question: **did the call reach Java?** Neither is
/// "did the user agree", which arrives later and is read back through
/// [`location_permission_status`](super::permissions::location_permission_status).
/// See [`request_location_permission`] for the
/// threading note that makes a `false` a real and recoverable outcome rather
/// than a user's decision — that distinction is the whole reason the gate has a
/// second attempt.
///
/// [`request_location_permission`]: super::permissions::request_location_permission
pub(super) fn request_location() -> bool {
    if !has_location_permission() {
        log::info!("Requesting ACCESS_FINE_LOCATION + ACCESS_COARSE_LOCATION permissions");
        return request_location_permission();
    }
    start_location_updates()
}

/// Stop delivering fixes.
///
/// Backs the arm's `stop`. Cannot revoke the runtime
/// permission — Android offers an app no way to give one back — so this is an
/// off switch for the *stream*, and it is a real one: the flag stops
/// [`start_location_thread`]'s poll from reading the provider, and
/// `LocationHelper.stop()` drops the subscription that keeps the providers
/// producing at all.
pub(super) fn stop_location() {
    // Flag first: the poll thread is a different thread and may be mid-sleep,
    // so this is what stops the *next* pass regardless of what Java does.
    LOCATION_UPDATES_ACTIVE.store(false, std::sync::atomic::Ordering::Relaxed);
    stop_location_updates();
}

/// Whether Android is currently delivering fixes.
///
/// Backs the arm's `active`. Read on the frame path, so it is a
/// relaxed atomic load rather than a JNI call: the flag is set only when
/// `LocationHelper.start()` has actually reached Java, so "active" here means
/// the subscription was established, not merely that it was asked for.
pub(super) fn location_active() -> bool {
    LOCATION_UPDATES_ACTIVE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Try to retrieve the device's last known GPS location via `LocationManager`.
/// Returns a [`crate::Fix`] on success or `None` if unavailable.
///
/// "Last known" is whatever the providers last produced for *any* client;
/// LocationHelper's subscription (see [`start_location_updates`]) is what
/// keeps them producing once permission is granted, and this poll doubles as
/// the fallback when that subscription could not be established.
fn get_last_known_location() -> Option<crate::Fix> {
    with_activity(last_known_location_with).flatten()
}

/// What a `LocationManager` provider name says about the fix it produced.
///
/// Only `"gps"` is a satellite fix. Everything else on this platform is the
/// network provider, which fuses Wi-Fi scans and cell towers.
///
/// `Device`, not `Estimated`, and the correction is about honesty rather than
/// behaviour. `Estimated` is NMEA quality 6 -- *dead reckoning*, a receiver
/// extrapolating from its last real fix -- which is a claim about a receiver
/// this device does not have. `FixQuality::can_relocate` admits both, so the
/// site upgrade is unchanged; what changes is the two words the settings pane
/// prints beside the position, which used to be wrong.
///
/// Split out of [`last_known_location_with`] for the reason `fix_from_coords` is
/// split out on the web: it is a decision, the rest of that function is nine JNI
/// calls, and only one of the two can be checked without a device.
fn provider_fix_quality(provider: &str) -> crate::FixQuality {
    if provider == "gps" {
        crate::FixQuality::Gps
    } else {
        crate::FixQuality::Device
    }
}

/// Body of [`get_last_known_location`], split out so it can keep using `?` on
/// `Option` inside the `Env` closure that jni 0.22's attachment API requires.
fn last_known_location_with(
    env: &mut jni::Env<'_>,
    activity: &jni::objects::JObject<'_>,
) -> Option<crate::Fix> {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    // LocationManager lm = context.getSystemService("location");
    // getSystemService is a Context method, so the Activity works here.
    let service_name = env.new_string("location").ok()?;
    let lm = env
        .call_method(
            activity,
            jni_str!("getSystemService"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
            &[JValue::from(&service_name)],
        )
        .ok()?
        .l()
        .ok()?;
    if lm.is_null() {
        return None;
    }

    // Try GPS first, then network provider as fallback
    for provider in &["gps", "network"] {
        let provider_str = env.new_string(provider).ok()?;
        let location = env.call_method(
            &lm,
            jni_str!("getLastKnownLocation"),
            jni_sig!("(Ljava/lang/String;)Landroid/location/Location;"),
            &[JValue::from(&provider_str)],
        );
        // getLastKnownLocation throws SecurityException without permission
        let location = match location {
            Ok(val) => val.l().ok()?,
            Err(_) => continue,
        };
        if location.is_null() {
            continue;
        }

        let lat = env
            .call_method(&location, jni_str!("getLatitude"), jni_sig!("()D"), &[])
            .ok()?
            .d()
            .ok()?;
        let lon = env
            .call_method(&location, jni_str!("getLongitude"), jni_sig!("()D"), &[])
            .ok()?
            .d()
            .ok()?;

        // Sanity check – (0, 0) is almost certainly a default/invalid value
        if lat.abs() < 0.001 && lon.abs() < 0.001 {
            continue;
        }

        // Extract extended fix data
        let altitude_m = env
            .call_method(&location, jni_str!("getAltitude"), jni_sig!("()D"), &[])
            .and_then(|v| v.d())
            .ok()
            .filter(|_| {
                env.call_method(&location, jni_str!("hasAltitude"), jni_sig!("()Z"), &[])
                    .and_then(|v| v.z())
                    .unwrap_or(false)
            });

        let speed_mps = env
            .call_method(&location, jni_str!("getSpeed"), jni_sig!("()F"), &[])
            .and_then(|v| v.f())
            .ok()
            .filter(|_| {
                env.call_method(&location, jni_str!("hasSpeed"), jni_sig!("()Z"), &[])
                    .and_then(|v| v.z())
                    .unwrap_or(false)
            })
            .map(|s| s as f64);

        let heading_deg = env
            .call_method(&location, jni_str!("getBearing"), jni_sig!("()F"), &[])
            .and_then(|v| v.f())
            .ok()
            .filter(|_| {
                env.call_method(&location, jni_str!("hasBearing"), jni_sig!("()Z"), &[])
                    .and_then(|v| v.z())
                    .unwrap_or(false)
            })
            .map(|b| b as f64);

        // The 68% confidence radius in metres, which is what `Fix` wants and
        // what `Location.getAccuracy()` documents itself as. Guarded by
        // `hasAccuracy()` like every other optional above: a `Location` without
        // one returns 0.0, and 0 m would read as a perfect fix rather than an
        // absent field.
        let accuracy_m = env
            .call_method(&location, jni_str!("getAccuracy"), jni_sig!("()F"), &[])
            .and_then(|v| v.f())
            .ok()
            .filter(|_| {
                env.call_method(&location, jni_str!("hasAccuracy"), jni_sig!("()Z"), &[])
                    .and_then(|v| v.z())
                    .unwrap_or(false)
            })
            .map(|a| a as f64);

        return Some(crate::Fix {
            point: rustdar_geo::GeoPoint { lat, lon },
            altitude_m,
            speed_mps,
            heading_deg,
            satellites: None, // Not available from getLastKnownLocation
            fix_quality: provider_fix_quality(provider),
            hdop: None,
            accuracy_m,
            timestamp: None,
        });
    }
    None
}

/// JClass for com.rustdar.LocationHelper, loaded once via the app class loader.
///
/// A `OnceLock`, unlike [`JAVA`](super::java_context): this is a *class*
/// resolved through the process-wide app ClassLoader, so the same object
/// serves every Activity instance and there is nothing to replace. Same shape
/// as the shell's `COMPASS_CLASS` (rustdar/src/android/compass.rs).
pub(super) static LOCATION_CLASS: std::sync::OnceLock<
    jni::objects::Global<jni::objects::JClass<'static>>,
> = std::sync::OnceLock::new();

/// Ask LocationHelper to begin real location updates (`LocationHelper.start()`).
///
/// `getLastKnownLocation` is passive: it reports the fix some location client
/// caused a provider to produce, and on a device where no other app happens to
/// be requesting location it stays null forever -- permission or not.
/// `start()` makes this app that client: LocationHelper subscribes a
/// do-nothing listener on the main looper, which is what switches the
/// providers on, and the existing [`get_last_known_location`] poll reads the
/// fixes they then produce. That split -- Java holds the subscription, Rust
/// keeps all the fix extraction -- is the simplest mechanism that covers
/// minSdk 28 through targetSdk 34: `requestLocationUpdates(String, long,
/// float, LocationListener, Looper)` exists and is undeprecated across the
/// whole range (`getCurrentLocation` is API 30+), and a `LocationListener` is
/// a Java interface Rust cannot implement without a DEX class anyway, so the
/// listener lives in LocationHelper.java beside the CompassHelper it mirrors.
///
/// Returns whether the call reached Java, so the caller retries a miss --
/// helper class not registered, JNI attach failure -- on its next pass instead
/// of giving live updates up for the process. Safe to deliver more than once:
/// `start()` is idempotent.
///
/// The retry is now the gate's rather than the poll thread's: the `Granted` arm
/// calls [`request_location`] on every pass while [`location_active`] is still
/// `false`, at ~1.3 Hz instead of every 10 s. Which is why the flag below is
/// set from the *return value* and not unconditionally -- a start that never
/// reached Java must leave `location_active` saying so, or nothing ever tries
/// again.
fn start_location_updates() -> bool {
    use jni::objects::JClass;
    use jni::{jni_sig, jni_str};

    let Some(global_ref) = LOCATION_CLASS.get() else {
        return false;
    };

    let started = with_env(|env| {
        let cls: &JClass<'static> = global_ref;
        env.call_static_method(cls, jni_str!("start"), jni_sig!("()V"), &[])
            .inspect_err(|e| log::warn!("LocationHelper.start() failed: {e:?}"))
            .is_ok()
    })
    .unwrap_or(false);
    if started {
        LOCATION_UPDATES_ACTIVE.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    started
}

/// Whether `LocationHelper.start()` has been delivered and not since stopped.
///
/// The one piece of state the location hooks share, and it is process-wide for
/// the same reason [`LOCATION_CLASS`] is: the hooks are bare `fn` pointers with
/// nothing to capture, and the poll thread reads it from a third thread again.
///
/// Not merely a cache of what Java thinks. It is what makes the settings pane's
/// **Turn off** button mean something: the poll below reads it before touching
/// the provider, so a stopped stream stops producing fixes even while the
/// runtime permission is still granted and `getLastKnownLocation` would happily
/// keep answering.
static LOCATION_UPDATES_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Ask LocationHelper to drop the subscription (`LocationHelper.stop()`).
///
/// The mirror of [`start_location_updates`], and equally idempotent. A miss is
/// logged and otherwise ignored: [`LOCATION_UPDATES_ACTIVE`] has already been
/// cleared by the time this runs, so the fix stream is stopped either way and
/// what a failure costs is a subscription left open until the Activity pauses --
/// battery, not correctness.
fn stop_location_updates() {
    use jni::objects::JClass;
    use jni::{jni_sig, jni_str};

    let Some(global_ref) = LOCATION_CLASS.get() else {
        return;
    };

    with_env(|env| {
        let cls: &JClass<'static> = global_ref;
        let _ = env
            .call_static_method(cls, jni_str!("stop"), jni_sig!("()V"), &[])
            .inspect_err(|e| log::warn!("LocationHelper.stop() failed: {e:?}"));
    });
}

/// Start a background thread that polls GPS location and sends updates
/// through the provided channel.
///
/// # It no longer asks for anything
///
/// This thread used to own the permission too: a bounded `requestPermissions`
/// loop with its own counter, firing three seconds after launch on whatever
/// state the Activity happened to be in. All of that has moved to
/// [`crate::LocationGate`], which is the one place in the app
/// that may prompt, and which knows three things this thread could not -- what
/// the OS currently says, whether this *install* has ever asked, and whether the
/// user has turned location off. What is left here is a reader.
///
/// The 10 s poll stays. `getLastKnownLocation` is what actually produces every
/// fix on this platform (`LocationHelper`'s listener is deliberately empty --
/// its subscription existing is the point), so removing the poll would remove
/// location, not just its permission half.
///
/// # `wake`
///
/// Called after each fix reaches the channel. The app drains that channel
/// from its platform poll, which runs only while rendering a frame,
/// and the loop sits on `ControlFlow::Wait` -- so without this a fix pushed
/// from here is invisible until something unrelated happens to draw one. It is
/// the facade's [`Wake`](crate::Wake) (the app's redraw waker) in production.
///
/// **Not** the shell's `EVENT_LOOP_PROXY` (its android back module), and the
/// difference is not that the proxy would
/// fail to wake the loop -- it would. It is that a proxy wake surfaces as
/// `ApplicationHandler::user_event`, which `App` does not override, so it
/// produces an *iteration* and not a *frame*; the back press below is collected
/// in `about_to_wait` and is happy with an iteration, while a GPS fix is drained
/// on the frame and is not. `Window::request_redraw` -- which is what the waker
/// ends in -- is also the stronger wake on this platform specifically: it sets
/// `redraw_flag` before `waker.wake()`, and the backend drops a bare
/// `PollEvent::Wake` unless a redraw or a user event is already outstanding.
///
/// A bare `impl Fn()` rather than the concrete waker type, so this module
/// stays a plain JNI reader with no coupling to the app's waker type: the
/// caller ([`AndroidBackend::set_wake`](super::AndroidBackend)) owns the
/// wiring, and the host `jni-typecheck` builds type-check this signature
/// without naming it.
pub(super) fn start_location_thread(
    sender: std::sync::mpsc::Sender<crate::Fix>,
    wake: impl Fn() + Send + 'static,
) {
    std::thread::Builder::new()
        .name("gps-location".into())
        .spawn(move || {
            // Let the app fully initialise before doing JNI work
            std::thread::sleep(std::time::Duration::from_secs(3));

            loop {
                // Both terms, and neither is redundant. [`location_active`] is
                // the user's switch and the gate's revocation stop -- without
                // it, "Turn off" would leave this thread reading the provider
                // every 10 s and the dot would keep moving. The permission check
                // is the framework's: a grant withdrawn in system settings makes
                // `getLastKnownLocation` throw, and there is no reason to make
                // it throw once per poll while the gate catches up.
                if location_active()
                    && has_location_permission()
                    && let Some(fix) = get_last_known_location()
                {
                    if sender.send(fix).is_err() {
                        break; // channel closed
                    }
                    wake();
                }

                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        })
        .expect("failed to spawn gps-location thread");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FixQuality;

    // Host-run under `jni-typecheck`, like the tri-state tests in
    // `permissions.rs` (the full preamble lives there): no JVM, so every
    // helper degrades to its documented default.

    /// Nothing is delivering until something has started it, and this flag is
    /// what the settings pane and the 10 s poll both read. A bridge that
    /// answered `true` here would have the gate believe delivery was already
    /// running and never call `request_location` at all.
    #[test]
    fn location_is_not_active_until_updates_have_been_started() {
        assert!(!location_active());
    }

    // ── The fix ─────────────────────────────────────────────────────────

    /// `"gps"` is the one provider that really is satellites.
    #[test]
    fn a_gps_provider_fix_still_claims_gps() {
        assert_eq!(provider_fix_quality("gps"), FixQuality::Gps);
    }

    /// Everything else is the network provider fusing Wi-Fi and cell towers.
    /// `Estimated` — what this used to report — means dead reckoning in NMEA,
    /// which is a claim about a receiver this device does not have, and the
    /// settings pane prints the label verbatim.
    #[test]
    fn a_network_provider_fix_is_a_device_fix_rather_than_dead_reckoning() {
        assert_eq!(provider_fix_quality("network"), FixQuality::Device);
        assert_eq!(provider_fix_quality("passive"), FixQuality::Device);
        assert_eq!(provider_fix_quality("fused"), FixQuality::Device);
    }

    /// Both qualities may refine the opening site, which is what makes the
    /// accuracy this file now reads the thing that decides it rather than a
    /// field nobody fills in. See `App::upgrade_provisional_site`.
    #[test]
    fn every_provider_this_app_reads_may_refine_the_opening_site() {
        for provider in ["gps", "network"] {
            assert!(
                provider_fix_quality(provider).can_relocate(),
                "{provider} fixes stopped being allowed to choose the radar site"
            );
        }
    }
}
