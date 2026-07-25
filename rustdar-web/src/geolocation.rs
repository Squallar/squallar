//! The browser's Geolocation API standing in for the serial GPS reader.
//!
//! The pipeline downstream of this is not serial-aware in any way: a source
//! obtains a `Sender<GpsFix>`, pushes fixes into it, and hands the matching
//! `Receiver` to [`PlatformBridge::set_gps_fix_receiver`]. `poll_gps_fix`,
//! `drain_latest` and `Gui::set_gps_fix` are shared by every target. Android
//! already does exactly this over JNI, so this is the second non-serial source,
//! not the first.
//!
//! [`PlatformBridge::set_gps_fix_receiver`]: rustdar_frontend::platform::PlatformBridge::set_gps_fix_receiver

use rustdar_gps::GpsFix;

/// Build a [`GpsFix`] from the fields a browser `GeolocationCoordinates` carries.
///
/// Separated from the `web_sys` call so the mapping is testable on the native
/// host — the conversion is where a mistake would be silent (a swapped
/// latitude and longitude puts the user in the wrong hemisphere and nothing
/// errors), whereas the `web_sys` call around it either works or throws.
///
/// The browser gives accuracy in metres and no satellite count or HDOP, so
/// those stay `None` rather than being invented. `heading` and `speed` are
/// `None` unless the device is actually moving, which is the API's own
/// behaviour and is preserved rather than defaulted to zero — a fabricated
/// heading of 0° would rotate the map north-up and look deliberate.
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
        // `from_lat_lon` is what sets `fix_quality` to `Gps`; going through it
        // rather than a struct literal keeps this in step with whatever that
        // helper decides a positional fix means.
        ..GpsFix::from_lat_lon(latitude, longitude)
    }
}

/// Start watching the browser's position, pushing every reading into `sender`.
///
/// Uses `watchPosition`, not `getCurrentPosition`: the fix channel is modelled
/// as *state* — `drain_latest` keeps only the newest value and discards the rest
/// — so a continuous stream is what it expects, and polling would just be a
/// worse version of the same thing.
///
/// The permission prompt is the browser's and appears on the first call. A
/// refusal arrives as an error callback, is logged once, and leaves the channel
/// empty forever, which the UI already handles: it is the same observable state
/// as a desktop machine with no GPS attached.
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
        // A closed receiver means the app is gone. There is nothing to clean up
        // — the watch dies with the page — so this is logged, not acted on.
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
    // The map wants a real position, and the user has already paid for the
    // permission prompt by the time this runs.
    options.set_enable_high_accuracy(true);

    if let Err(e) = geolocation.watch_position_with_error_callback_and_options(
        on_success.as_ref().unchecked_ref(),
        Some(on_error.as_ref().unchecked_ref()),
        &options,
    ) {
        log::warn!("failed to start watching position: {e:?}");
        return;
    }

    // The browser holds these callbacks for the lifetime of the watch, which is
    // the lifetime of the page. Dropping the `Closure`s would free the
    // JS-visible functions out from under it and the first callback would then
    // call into freed wasm memory. `forget` is the documented way to say "this
    // lives as long as the page does"; the watch is never cancelled, so the leak
    // is bounded at two closures for the whole session.
    on_success.forget();
    on_error.forget();
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdar_gps::FixQuality;

    /// Latitude and longitude must not be transposed. Nothing downstream would
    /// notice — both are plain `f64` and a transposed pair is a valid location,
    /// just the wrong one.
    #[test]
    fn latitude_and_longitude_keep_their_places() {
        let fix = fix_from_coords(35.25, -97.5, None, None, None);
        assert_eq!(fix.latitude, 35.25);
        assert_eq!(fix.longitude, -97.5);
    }

    /// A reading from the browser is a real positional fix, not the `Default`
    /// that `GpsFix::default()` would give — the map treats an `Invalid` quality
    /// as "no location" and would draw nothing.
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

    /// A stationary device reports no heading and no speed, and that has to stay
    /// absent. Substituting zero would be indistinguishable from "moving due
    /// north at rest" and would rotate a heading-up map to a fabricated bearing.
    #[test]
    fn absent_motion_stays_absent_rather_than_becoming_zero() {
        let fix = fix_from_coords(35.25, -97.5, None, None, None);
        assert_eq!(fix.speed_mps, None);
        assert_eq!(fix.heading_deg, None);
        assert_eq!(fix.altitude_m, None);
    }

    /// The browser supplies neither a satellite count nor an HDOP. They must
    /// stay `None` rather than being invented, since the UI shows them verbatim.
    #[test]
    fn fields_the_browser_does_not_supply_are_not_invented() {
        let fix = fix_from_coords(35.25, -97.5, Some(390.0), Some(12.5), Some(275.0));
        assert_eq!(fix.satellites, None);
        assert_eq!(fix.hdop, None);
    }
}
