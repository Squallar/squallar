//! What a platform-supplied number has to look like to be a reading.
//!
//! Every location service this crate reads has a way of saying "I do not know",
//! and each arm decodes its own: the XDG portal spells an unknown altitude
//! `-G_MAXDOUBLE` (`-f64::MAX`, i.e. [`f64::MIN`]), CoreLocation marks the
//! component invalid with a negative *accuracy*, Android answers
//! `hasAltitude() == false`, WinRT and the browser return a null reference.
//!
//! This module is the floor under all of them, for the values that get through
//! anyway — one did: `-f64::MAX` reached the map tooltip and rendered as a
//! 309-digit altitude, six lines tall.
//!
//! A plausibility band rather than a table of sentinels, because the sentinel
//! that leaks is by definition the one whose platform did not document it. What
//! these say is only "nothing carrying a receiver is here"; every arm keeps its
//! own decoding on top, and a stricter arm rule outranks these.

/// The lowest altitude that is a reading. The lowest dry land on earth is the
/// Dead Sea shore at about -430 m, so -500 m sits under every reading and over
/// every sentinel — including the -10 000 m Chromium spells `kBadAltitude`.
/// Firefox's GeoClue provider draws its line in the same place for the same
/// reason (`kGCMinAlt`, `dom/system/linux/GeoclueLocationProvider.cpp`).
const MIN_ALTITUDE_M: f64 = -500.0;

/// The highest. The Kármán line: a balloon at 40 km is well under it and
/// nothing above it is holding a GPS receiver this app should believe.
const MAX_ALTITUDE_M: f64 = 100_000.0;

/// The fastest ground speed that is a reading, ~Mach 3 at sea level. Airliners
/// cruise at ~250 m/s; anything past this is a receiver glitching, not a user
/// moving.
const MAX_SPEED_MPS: f64 = 1_000.0;

/// Altitude in metres above sea level, or `None` if nothing could be there.
///
/// A range test and not an equality against the known sentinels: `contains`
/// also rejects NaN and both infinities, which is the same answer for the same
/// reason.
pub fn altitude_m(raw: f64) -> Option<f64> {
    (MIN_ALTITUDE_M..=MAX_ALTITUDE_M)
        .contains(&raw)
        .then_some(raw)
}

/// Ground speed in metres per second, or `None` if it is not a speed.
///
/// Zero passes: a stationary receiver reporting 0 m/s is reporting. The arms
/// that want "stationary" to read as *absent* — the portal's, because
/// `HeadingSource::Auto` reads speed to decide whether a bearing is
/// trustworthy — say so themselves.
pub fn speed_mps(raw: f64) -> Option<f64> {
    (0.0..=MAX_SPEED_MPS).contains(&raw).then_some(raw)
}

/// True course in degrees, or `None` if it is off the compass. Zero is *inside*
/// the range: due north is the one bearing a truthiness test would delete.
pub fn heading_deg(raw: f64) -> Option<f64> {
    (0.0..=360.0).contains(&raw).then_some(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The number that was on the glass: the portal's and geoclue's
    /// `-G_MAXDOUBLE`, which `f64::MIN` spells exactly.
    #[test]
    fn the_unknown_altitude_sentinel_is_not_an_altitude() {
        assert_eq!(f64::MIN, -f64::MAX);
        assert_eq!(altitude_m(f64::MIN), None);
        assert_eq!(altitude_m(f64::MAX), None);
    }

    /// Chromium's `kBadAltitude`, which is small enough to look like a place.
    #[test]
    fn a_sentinel_wearing_a_plausible_magnitude_is_still_not_an_altitude() {
        assert_eq!(altitude_m(-10_000.0), None);
    }

    #[test]
    fn a_number_that_is_not_a_number_is_not_an_altitude() {
        assert_eq!(altitude_m(f64::NAN), None);
        assert_eq!(altitude_m(f64::INFINITY), None);
        assert_eq!(altitude_m(f64::NEG_INFINITY), None);
    }

    /// The band has to admit the places people actually are, including the ones
    /// below sea level and the ones in an aircraft.
    #[test]
    fn the_readings_a_receiver_really_produces_survive() {
        assert_eq!(altitude_m(-430.0), Some(-430.0)); // Dead Sea shore
        assert_eq!(altitude_m(0.0), Some(0.0));
        assert_eq!(altitude_m(357.0), Some(357.0));
        assert_eq!(altitude_m(8_849.0), Some(8_849.0)); // Everest
        assert_eq!(altitude_m(11_000.0), Some(11_000.0)); // cruising
    }

    #[test]
    fn a_reversing_or_impossible_speed_is_not_a_speed() {
        assert_eq!(speed_mps(-1.0), None); // geoclue's sentinel
        assert_eq!(speed_mps(f64::NAN), None);
        assert_eq!(speed_mps(f64::MAX), None);
        assert_eq!(speed_mps(0.0), Some(0.0));
        assert_eq!(speed_mps(12.5), Some(12.5));
    }

    #[test]
    fn a_bearing_off_the_compass_is_not_a_bearing() {
        assert_eq!(heading_deg(-1.0), None); // geoclue's sentinel
        assert_eq!(heading_deg(361.0), None);
        assert_eq!(heading_deg(f64::NAN), None);
        assert_eq!(heading_deg(0.0), Some(0.0));
        assert_eq!(heading_deg(271.0), Some(271.0));
    }
}
