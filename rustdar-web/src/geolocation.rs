//! The browser's Geolocation API standing in for the serial GPS reader.
//!
//! Nothing downstream is serial-aware: a source pushes into a `Sender<GpsFix>`
//! and hands the `Receiver` to [`PlatformBridge::set_gps_fix_receiver`]. Android
//! already does this over JNI.
//!
//! [`PlatformBridge::set_gps_fix_receiver`]: rustdar_frontend::platform::PlatformBridge::set_gps_fix_receiver

use rustdar_gps::GpsFix;

/// Build a [`GpsFix`] from a browser `GeolocationCoordinates`.
///
/// Separated from the `web_sys` call so the mapping is testable on the host: a
/// swapped latitude and longitude is silently valid, whereas the `web_sys` call
/// either works or throws.
///
/// Satellite count and HDOP stay `None` — the browser has neither. `heading` and
/// `speed` stay `None` when the device is stationary rather than defaulting to
/// zero, which would rotate a heading-up map to a fabricated bearing.
pub fn fix_from_coords(
    latitude: f64,
    longitude: f64,
    altitude_m: Option<f64>,
    speed_mps: Option<f64>,
    heading_deg: Option<f64>,
) -> GpsFix {
    GpsFix {
        altitude_m,
        speed_mps,
        heading_deg,
        // `from_lat_lon` is what sets `fix_quality` to `Gps`; a struct literal
        // would drift from whatever it decides a positional fix means.
        ..GpsFix::from_lat_lon(latitude, longitude)
    }
}

/// Start watching the browser's position, pushing every reading into `sender`.
///
/// `watchPosition`, not `getCurrentPosition`: the fix channel is state —
/// `drain_latest` keeps only the newest value — so a stream is what it expects.
///
/// A refused permission arrives as an error callback and leaves the channel
/// empty forever, which is the same observable state as a desktop with no GPS.
#[cfg(target_arch = "wasm32")]
pub fn start_watch(sender: std::sync::mpsc::Sender<GpsFix>) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::Closure;

    let Some(geolocation) = web_sys::window().and_then(|w| w.navigator().geolocation().ok()) else {
        log::info!("no Geolocation API on this browser; user location is unavailable");
        return;
    };

    let on_success = Closure::<dyn FnMut(web_sys::Position)>::new(move |position: web_sys::Position| {
        let coords = position.coords();
        let fix = fix_from_coords(
            coords.latitude(),
            coords.longitude(),
            coords.altitude(),
            coords.speed(),
            coords.heading(),
        );
        // A closed receiver means the app is gone; the watch dies with the page.
        if sender.send(fix).is_err() {
            log::debug!("GPS receiver dropped; geolocation updates have nowhere to go");
        }
    });

    let on_error = Closure::<dyn FnMut(web_sys::PositionError)>::new(
        move |error: web_sys::PositionError| {
            // Denial is the common case and is not an application error.
            log::info!(
                "geolocation unavailable (code {}): {}",
                error.code(),
                error.message()
            );
        },
    );

    let options = web_sys::PositionOptions::new();
    options.set_enable_high_accuracy(true);

    if let Err(e) = geolocation.watch_position_with_error_callback_and_options(
        on_success.as_ref().unchecked_ref(),
        Some(on_error.as_ref().unchecked_ref()),
        &options,
    ) {
        log::warn!("failed to start watching position: {e:?}");
        return;
    }

    // Dropping these `Closure`s would free the JS-visible functions out from
    // under a watch the browser holds for the life of the page, and the next
    // callback would call into freed wasm memory. The watch is never cancelled,
    // so the leak is two closures per session.
    on_success.forget();
    on_error.forget();
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdar_gps::FixQuality;

    /// Nothing downstream would notice a transposition: both are plain `f64` and
    /// a swapped pair is a valid location, just the wrong one.
    #[test]
    fn latitude_and_longitude_keep_their_places() {
        let fix = fix_from_coords(35.25, -97.5, None, None, None);
        assert_eq!(fix.latitude, 35.25);
        assert_eq!(fix.longitude, -97.5);
    }

    /// Not `GpsFix::default()`: the map treats an `Invalid` quality as "no
    /// location" and draws nothing.
    #[test]
    fn a_reading_counts_as_a_gps_fix() {
        let fix = fix_from_coords(35.25, -97.5, None, None, None);
        assert!(
            matches!(fix.fix_quality, FixQuality::Gps),
            "{:?}",
            fix.fix_quality
        );
    }

    /// The optional fields are carried through rather than dropped.
    #[test]
    fn altitude_speed_and_heading_are_carried_through() {
        let fix = fix_from_coords(35.25, -97.5, Some(390.0), Some(12.5), Some(275.0));
        assert_eq!(fix.altitude_m, Some(390.0));
        assert_eq!(fix.speed_mps, Some(12.5));
        assert_eq!(fix.heading_deg, Some(275.0));
    }

    /// Substituting zero for a stationary device's absent heading is
    /// indistinguishable from "moving due north" and rotates a heading-up map.
    #[test]
    fn absent_motion_stays_absent_rather_than_becoming_zero() {
        let fix = fix_from_coords(35.25, -97.5, None, None, None);
        assert_eq!(fix.speed_mps, None);
        assert_eq!(fix.heading_deg, None);
        assert_eq!(fix.altitude_m, None);
    }

    /// The browser supplies neither, and the UI shows them verbatim.
    #[test]
    fn fields_the_browser_does_not_supply_are_not_invented() {
        let fix = fix_from_coords(35.25, -97.5, Some(390.0), Some(12.5), Some(275.0));
        assert_eq!(fix.satellites, None);
        assert_eq!(fix.hdop, None);
    }
}
