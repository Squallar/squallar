//! Location fixes over JNI: `LocationManager` reads, the `LocationHelper`
//! subscription, and the 10 s poll thread that feeds the app.

use super::permissions::{has_location_permission, request_location_permission};
use super::{with_activity, with_env};

/// Prompt if Android needs prompting, and start delivering fixes. The `bool`
/// answers **did the call reach Java?**, never "did the user agree"; see
/// [`request_location_permission`](super::permissions::request_location_permission).
pub(super) fn request_location() -> bool {
    if !has_location_permission() {
        log::info!("Requesting ACCESS_FINE_LOCATION + ACCESS_COARSE_LOCATION permissions");
        return request_location_permission();
    }
    start_location_updates()
}

/// Stop delivering fixes. Cannot revoke the runtime permission, so this is an
/// off switch for the *stream*: the flag stops the poll, and
/// `LocationHelper.stop()` drops the subscription that keeps providers running.
pub(super) fn stop_location() {
    // Flag first: the poll thread may be mid-sleep.
    LOCATION_UPDATES_ACTIVE.store(false, std::sync::atomic::Ordering::Relaxed);
    stop_location_updates();
}

/// Whether Android is currently delivering fixes. A relaxed atomic load, not a
/// JNI call, because it is read on the frame path.
pub(super) fn location_active() -> bool {
    LOCATION_UPDATES_ACTIVE.load(std::sync::atomic::Ordering::Relaxed)
}

/// The device's last known location via `LocationManager`, or `None`. "Last
/// known" is whatever the providers last produced for *any* client.
fn get_last_known_location() -> Option<crate::Fix> {
    with_activity(last_known_location_with).flatten()
}

/// What a `LocationManager` provider name says about the fix it produced. Only
/// `"gps"` is a satellite fix; everything else is the network provider fusing
/// Wi-Fi scans and cell towers. `Device`, not `Estimated`, which is NMEA
/// quality 6 — dead reckoning — and the settings pane prints the label verbatim.
fn provider_fix_quality(provider: &str) -> crate::FixQuality {
    if provider == "gps" {
        crate::FixQuality::Gps
    } else {
        crate::FixQuality::Device
    }
}

/// Body of [`get_last_known_location`], split out so it can keep using `?` on
/// `Option` inside the `Env` closure jni 0.22's attachment API requires.
fn last_known_location_with(
    env: &mut jni::Env<'_>,
    activity: &jni::objects::JObject<'_>,
) -> Option<crate::Fix> {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

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

        if let Some(fix) = fix_from_location(env, &location, provider) {
            return Some(fix);
        }
    }
    None
}

/// Decode one `android.location.Location` into a [`Fix`](crate::Fix). The one
/// decoder: both reads — the cached fix `LocationHelper` was handed and the
/// passive `getLastKnownLocation` — come through here, so a field guarded on one
/// path cannot be unguarded on the other.
fn fix_from_location(
    env: &mut jni::Env<'_>,
    location: &jni::objects::JObject<'_>,
    provider: &str,
) -> Option<crate::Fix> {
    use jni::{jni_sig, jni_str};

    let lat = env
        .call_method(location, jni_str!("getLatitude"), jni_sig!("()D"), &[])
        .ok()?
        .d()
        .ok()?;
    let lon = env
        .call_method(location, jni_str!("getLongitude"), jni_sig!("()D"), &[])
        .ok()?
        .d()
        .ok()?;

    // A provider that has produced nothing answers (0, 0) rather than null on
    // some devices, and Null Island is not a fix.
    if lat.abs() < 0.001 && lon.abs() < 0.001 {
        return None;
    }

    let altitude_m = env
        .call_method(location, jni_str!("getAltitude"), jni_sig!("()D"), &[])
        .and_then(|v| v.d())
        .ok()
        .filter(|_| {
            env.call_method(location, jni_str!("hasAltitude"), jni_sig!("()Z"), &[])
                .and_then(|v| v.z())
                .unwrap_or(false)
        });

    let speed_mps = env
        .call_method(location, jni_str!("getSpeed"), jni_sig!("()F"), &[])
        .and_then(|v| v.f())
        .ok()
        .filter(|_| {
            env.call_method(location, jni_str!("hasSpeed"), jni_sig!("()Z"), &[])
                .and_then(|v| v.z())
                .unwrap_or(false)
        })
        .map(|s| s as f64);

    let heading_deg = env
        .call_method(location, jni_str!("getBearing"), jni_sig!("()F"), &[])
        .and_then(|v| v.f())
        .ok()
        .filter(|_| {
            env.call_method(location, jni_str!("hasBearing"), jni_sig!("()Z"), &[])
                .and_then(|v| v.z())
                .unwrap_or(false)
        })
        .map(|b| b as f64);

    // Guarded by `hasAccuracy()`: a `Location` without one returns 0.0, and
    // 0 m would read as a perfect fix rather than an absent field.
    let accuracy_m = env
        .call_method(location, jni_str!("getAccuracy"), jni_sig!("()F"), &[])
        .and_then(|v| v.f())
        .ok()
        .filter(|_| {
            env.call_method(location, jni_str!("hasAccuracy"), jni_sig!("()Z"), &[])
                .and_then(|v| v.z())
                .unwrap_or(false)
        })
        .map(|a| a as f64);

    Some(crate::Fix {
        point: rustdar_geo::GeoPoint { lat, lon },
        altitude_m,
        speed_mps,
        heading_deg,
        satellites: None, // Not available from getLastKnownLocation
        fix_quality: provider_fix_quality(provider),
        hdop: None,
        accuracy_m,
        timestamp: None,
    })
}

/// JClass for com.rustdar.LocationHelper, loaded once via the app class loader.
/// A `OnceLock`: one resolved through the app ClassLoader serves every Activity.
pub(super) static LOCATION_CLASS: std::sync::OnceLock<
    jni::objects::Global<jni::objects::JClass<'static>>,
> = std::sync::OnceLock::new();

/// Ask LocationHelper to begin real location updates (`LocationHelper.start()`).
///
/// `getLastKnownLocation` is passive: on a device where no other app requests
/// location it stays null forever, permission or not. `start()` makes this app
/// that client — LocationHelper subscribes a do-nothing listener on the main
/// looper, which is what switches the providers on. Java holds the subscription
/// because `requestLocationUpdates(String, long, float, LocationListener,
/// Looper)` is undeprecated across minSdk 28 to targetSdk 34 and a
/// `LocationListener` needs a DEX class.
///
/// Returns whether the call reached Java, so the caller retries a miss;
/// `start()` is idempotent. The flag is set from the *return value*, or a start
/// that never reached Java would leave nothing trying again.
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
/// The poll reads it before touching the provider, so a stopped stream stops
/// producing fixes even while the permission is granted.
static LOCATION_UPDATES_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Ask LocationHelper to drop the subscription (`LocationHelper.stop()`).
/// Idempotent. A miss costs a subscription left open until the Activity pauses.
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

/// The freshest fix `LocationHelper`'s subscription has been handed, or `None`
/// before the first delivery.
///
/// The helper caches what its listener receives; this reads the cache. It is not
/// a replacement for [`get_last_known_location`] but the layer above it: the
/// cache is empty until a provider has delivered at least once, which on a cold
/// start is minutes, and the passive last-known read is what the app opens on.
/// The provider name is read alongside rather than off the `Location`, so a fix
/// whose provider field is absent still maps to a quality.
fn cached_fix() -> Option<crate::Fix> {
    use jni::objects::JClass;
    use jni::{jni_sig, jni_str};

    let global_ref = LOCATION_CLASS.get()?;

    with_env(|env| {
        let cls: &JClass<'static> = global_ref;
        let location = env
            .call_static_method(
                cls,
                jni_str!("cachedFix"),
                jni_sig!("()Landroid/location/Location;"),
                &[],
            )
            .and_then(|v| v.l())
            .inspect_err(|e| log::warn!("LocationHelper.cachedFix() failed: {e:?}"))
            .ok()?;
        if location.is_null() {
            return None;
        }

        let provider = env
            .call_static_method(
                cls,
                jni_str!("cachedFixProvider"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )
            .and_then(|v| v.l())
            .ok()
            .filter(|s| !s.is_null())
            .and_then(|s| env.cast_local::<jni::objects::JString>(s).ok())
            .and_then(|s| s.try_to_string(env).ok())
            .unwrap_or_default();

        fix_from_location(env, &location, &provider)
    })
    .flatten()
}

/// Start a background thread that polls GPS location and sends updates through
/// the provided channel. It asks for nothing: prompting is
/// [`crate::LocationGate`]'s alone.
///
/// The 10 s poll stays, and it is a poll rather than a push on purpose: a push
/// would deliver on the UI thread and need JNI plumbing in the other direction
/// for a reader that already exists. Each turn reads the helper's cached fix
/// first and falls back to the passive `getLastKnownLocation` — the cache is
/// empty until a provider has delivered once, and the passive read is what a
/// cold start opens on.
///
/// `wake` is called after each fix reaches the channel — the app drains it only
/// while rendering, on `ControlFlow::Wait`. **Not** the shell's
/// `EVENT_LOOP_PROXY`: a proxy wake surfaces as
/// `ApplicationHandler::user_event`, which `App` does not override, so it
/// produces an *iteration* and not a *frame*. `Window::request_redraw` is also
/// the stronger wake here — it sets `redraw_flag` before `waker.wake()`.
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
                // Both terms, and neither is redundant: [`location_active`] is
                // the user's switch and the gate's revocation stop, while the
                // permission check keeps a withdrawn grant from throwing.
                if location_active()
                    && has_location_permission()
                    && let Some(fix) = cached_fix().or_else(get_last_known_location)
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

    // Host-run under `jni-typecheck`: no JVM, so every helper degrades to its
    // documented default.

    /// A bridge answering `true` here would have the gate believe delivery was
    /// already running and never call `request_location`.
    #[test]
    fn location_is_not_active_until_updates_have_been_started() {
        assert!(!location_active());
    }

    #[test]
    fn a_gps_provider_fix_still_claims_gps() {
        assert_eq!(provider_fix_quality("gps"), FixQuality::Gps);
    }

    /// `Estimated` — what this used to report — means dead reckoning in NMEA,
    /// and the settings pane prints the label verbatim.
    #[test]
    fn a_network_provider_fix_is_a_device_fix_rather_than_dead_reckoning() {
        assert_eq!(provider_fix_quality("network"), FixQuality::Device);
        assert_eq!(provider_fix_quality("passive"), FixQuality::Device);
        assert_eq!(provider_fix_quality("fused"), FixQuality::Device);
    }

    /// See `App::upgrade_provisional_site`.
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
