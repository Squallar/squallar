//! Where the sun is, and what colour its light is — the two halves of one
//! quantity, returned together.
//!
//! Solar *position* is the NOAA Solar Calculator's arithmetic (Meeus,
//! *Astronomical Algorithms*, ch. 25 and 28, in the abridged form NOAA GML
//! publishes): geometric mean longitude and anomaly, the equation of centre,
//! the nutation/aberration correction that turns true longitude into apparent
//! longitude, and the equation of time. It is a *low-accuracy* solar theory by
//! Meeus's own label. Measured against a DE421 apparent place over 947,376
//! site-instants — 10-minute steps through 2000 and 2026, nine sites including
//! both poles and the antimeridian — it is **within 0.0133°**, and half the
//! domain is over 0.0040°. [`solar::tests::ALMANAC_TOL_DEG`] carries that
//! envelope and the reasoning behind it.
//!
//! 0.0133° is 1/40 of the sun's own disc. Through a Lambert cosine it is about
//! two hundredths of an 8-bit level on flat ground, half a level on the
//! steepest segment of the tint ramp, and a few seconds of terminator timing.
//!
//! Solar *tint* is a 1-D ramp keyed on that same elevation, because the physics
//! that aims the light and the physics that colours it are one quantity: the
//! air mass the beam crossed. It is **two** ramps, and the split is load-bearing
//! rather than decorative — see [`sun_tint`] and [`sky_ambient`]. A single ramp
//! multiplied by `N·L` produces no light at all once the sun is down, which
//! would make every twilight and night colour in this module unreachable.
//!
//! **Every elevation here is geometric** — the unrefracted altitude of the
//! sun's *centre*. That is deliberate and it is what makes the almanac pins in
//! `solar/tests.rs` exact rather than approximate: sunrise, sunset and civil
//! twilight are *defined* at geometric centre altitudes (−0.8333° and −6.0°),
//! with standard refraction and the solar semidiameter already folded into the
//! −0.8333°. Adding a refraction model here would double-count them.

/// Julian Day of the Unix epoch, 1970-01-01T00:00:00Z.
const JD_UNIX_EPOCH: f64 = 2_440_587.5;

/// Julian Day of J2000.0, 2000-01-01T12:00:00 TT.
const JD_J2000: f64 = 2_451_545.0;

/// Days in a Julian century.
const DAYS_PER_JULIAN_CENTURY: f64 = 36_525.0;

/// How far from J2000 [`solar_position`] will answer, in Julian centuries —
/// roughly the years 1500 to 2500.
///
/// Not a taste judgement: every series here is a polynomial in this quantity,
/// and outside a bounded window they overflow to infinity and then to `NaN`.
/// `solar_position(39.74, -104.98, 1e100)` used to return a `NaN` elevation
/// beside a perfectly ordinary-looking night colour, which is the shape of
/// failure this workspace has named *silent partial success*. The window is far
/// wider than the theory's own accuracy claim (1900–2100) on purpose: refusing
/// is for inputs that are *wrong*, not for inputs that are merely imprecise.
const SUPPORTED_JULIAN_CENTURIES: f64 = 5.0;

/// Where the sun is, seen from one point on the ground at one instant.
///
/// Angles are degrees. Elevation and azimuth are a horizon-frame pair;
/// declination and hour angle are the equatorial pair they were built from, and
/// they are public because they are what an almanac publishes and therefore
/// what this module can be *checked* against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SolarPosition {
    /// Geometric altitude of the sun's centre above the astronomical horizon,
    /// `-90..=90`. Negative is below the horizon. **Unrefracted** — see the
    /// module note.
    pub elevation_deg: f64,
    /// True bearing of the sun, `0..360`, clockwise from north.
    pub azimuth_deg: f64,
    /// Apparent declination of the sun, `-90..=90`.
    pub declination_deg: f64,
    /// Local hour angle, `-180..180`, measured **westward** — negative before
    /// local solar noon, zero at it, positive after.
    pub hour_angle_deg: f64,
}

/// The sun as a light: where it is, which way it points, and what colour it is
/// — both the directional half and the half that does not care which way a
/// surface faces.
///
/// All four fields travel together on purpose. A ground surface and a
/// raymarched volume lit from two separate sources read as two composited
/// pictures rather than one scene, so there is exactly one place either can ask.
///
/// The intended consumer arithmetic, and the reason [`ambient`](Self::ambient)
/// exists at all:
///
/// ```text
/// lit = albedo * (colour * max(0, dot(normal, direction_enu)) + ambient)
/// ```
///
/// **`colour * N·L` alone is zero everywhere the sun is down.** Without the
/// ambient term every twilight and night colour this module defines would be
/// unreachable, and a scene at 2 a.m. would render as pure black rather than as
/// ground read by silhouette.
///
/// `colour + ambient` can exceed 1.0 on a surface facing a high sun — it peaks
/// near 1.29 at the zenith, since a clear sky really does add roughly a quarter
/// again on top of the beam. Exposure is the consumer's decision and this type
/// does not pre-empt it; `the_lit_sum_peaks_where_it_is_documented_to` pins the
/// figure so it cannot drift into a consumer unnoticed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SunLight {
    /// Where the sun is.
    pub position: SolarPosition,
    /// Unit vector **from the lit surface toward the sun**, in the local
    /// east-north-up frame: `[east, north, up]`.
    ///
    /// `up` carries the sign of [`SolarPosition::elevation_deg`], so at night
    /// this points below the horizon. A consumer that wants a headlight-safe
    /// vector clamps it; this type states the truth.
    pub direction_enu: [f64; 3],
    /// Linear-light RGB of the **direct beam**, from [`sun_tint`]. Applied
    /// through `N·L`, and identically zero once the sun has set.
    pub colour: [f64; 3],
    /// Linear-light RGB of the **sky**, from [`sky_ambient`]. Applied without a
    /// cosine, because scattered light arrives from the whole hemisphere.
    pub ambient: [f64; 3],
}

/// Julian Day for a Unix timestamp in seconds (UTC, leap seconds ignored as
/// Unix time itself ignores them).
pub fn julian_day(unix_seconds: f64) -> f64 {
    unix_seconds / 86_400.0 + JD_UNIX_EPOCH
}

/// The sun's position over `(lat_deg, lon_deg)` at `unix_seconds`, longitude
/// **east-positive**.
///
/// `None` — never a half-answer — when the request cannot be honoured:
///
/// * any argument is `NaN` or infinite;
/// * `lat_deg` is outside `-90..=90`, which is not a place;
/// * `unix_seconds` is further from J2000 than [`SUPPORTED_JULIAN_CENTURIES`].
///
/// Longitude is *not* range-checked: values outside ±180 wrap correctly and a
/// caller holding an unwrapped longitude is not making a mistake.
pub fn solar_position(lat_deg: f64, lon_deg: f64, unix_seconds: f64) -> Option<SolarPosition> {
    if !lat_deg.is_finite() || !lon_deg.is_finite() || !unix_seconds.is_finite() {
        return None;
    }
    if !(-90.0..=90.0).contains(&lat_deg) {
        return None;
    }
    let jc = (julian_day(unix_seconds) - JD_J2000) / DAYS_PER_JULIAN_CENTURY;
    if !(-SUPPORTED_JULIAN_CENTURIES..=SUPPORTED_JULIAN_CENTURIES).contains(&jc) {
        return None;
    }

    // Geometric mean longitude and mean anomaly of the sun, and the orbit's
    // eccentricity — Meeus 25.2, 25.3 and 25.4.
    let mean_lon_deg = (280.466_46 + jc * (36_000.769_83 + jc * 0.000_303_2)).rem_euclid(360.0);
    let mean_anomaly_deg = 357.529_11 + jc * (35_999.050_29 - jc * 0.000_153_7);
    let eccentricity = 0.016_708_634 - jc * (0.000_042_037 + jc * 0.000_000_126_7);

    // Equation of centre: the difference between the true and mean anomaly of
    // an elliptical orbit, to three harmonics.
    let m = mean_anomaly_deg.to_radians();
    let centre_deg = m.sin() * (1.914_602 - jc * (0.004_817 + jc * 0.000_014))
        + (2.0 * m).sin() * (0.019_993 - jc * 0.000_101)
        + (3.0 * m).sin() * 0.000_289;

    // Apparent longitude: true longitude corrected for nutation in longitude
    // and for the aberration of light. `omega` is the longitude of the moon's
    // ascending node, the dominant nutation term.
    let omega = (125.04 - 1_934.136 * jc).to_radians();
    let apparent_lon = (mean_lon_deg + centre_deg - 0.005_69 - 0.004_78 * omega.sin()).to_radians();

    // Mean obliquity of the ecliptic (Meeus 22.2) plus the nutation in
    // obliquity, giving the true obliquity the apparent place is referred to.
    let mean_obliquity_deg =
        23.0 + (26.0 + (21.448 - jc * (46.815 + jc * (0.000_59 - jc * 0.001_813))) / 60.0) / 60.0;
    let obliquity = (mean_obliquity_deg + 0.002_56 * omega.cos()).to_radians();

    // Clamped for the reason `site_bearing_range_km` clamps: the product can
    // round a hair past ±1 at a solstice and `asin` is then `NaN`.
    let declination = (obliquity.sin() * apparent_lon.sin())
        .clamp(-1.0, 1.0)
        .asin();

    // Equation of time in minutes — apparent solar time minus mean solar time
    // (Meeus 28.3), the correction that turns clock time into the sun's own.
    let y = (obliquity / 2.0).tan().powi(2);
    let l0 = mean_lon_deg.to_radians();
    let eq_time_min = 4.0
        * (y * (2.0 * l0).sin() - 2.0 * eccentricity * m.sin()
            + 4.0 * eccentricity * y * m.sin() * (2.0 * l0).cos()
            - 0.5 * y * y * (4.0 * l0).sin()
            - 1.25 * eccentricity * eccentricity * (2.0 * m).sin())
        .to_degrees();

    // True solar time at the meridian of `lon_deg`, in minutes past its own
    // midnight. 4 minutes per degree is one rotation, 1440 min over 360°.
    let utc_minutes = unix_seconds.rem_euclid(86_400.0) / 60.0;
    let true_solar_minutes = (utc_minutes + eq_time_min + 4.0 * lon_deg).rem_euclid(1_440.0);
    let hour_angle_deg = true_solar_minutes / 4.0 - 180.0;

    let (sin_ha, cos_ha) = hour_angle_deg.to_radians().sin_cos();
    let (sin_lat, cos_lat) = lat_deg.to_radians().sin_cos();
    let (sin_dec, cos_dec) = declination.sin_cos();

    let sin_elevation = (sin_lat * sin_dec + cos_lat * cos_dec * cos_ha).clamp(-1.0, 1.0);

    // `atan2` rather than the NOAA spreadsheet's `acos` form: that one divides
    // by `cos(lat)·sin(zenith)`, which vanishes at a pole and at the zenith and
    // needs a hand-written quadrant fix-up afterwards. This spelling is the
    // same angle with neither hazard.
    let azimuth = (-cos_dec * sin_ha).atan2(sin_dec * cos_lat - cos_dec * sin_lat * cos_ha);

    Some(SolarPosition {
        elevation_deg: sin_elevation.asin().to_degrees(),
        azimuth_deg: azimuth.to_degrees().rem_euclid(360.0),
        declination_deg: declination.to_degrees(),
        hour_angle_deg,
    })
}

/// The sun as a light over `(lat_deg, lon_deg)` at `unix_seconds` — position,
/// direction, beam colour and sky colour from one call, so no caller can hold
/// half of it.
///
/// `None` on exactly the inputs [`solar_position`] refuses, and for the same
/// reason: a direction of `[NaN, NaN, NaN]` beside a valid-looking night colour
/// is worse than no answer.
pub fn sun_light(lat_deg: f64, lon_deg: f64, unix_seconds: f64) -> Option<SunLight> {
    let position = solar_position(lat_deg, lon_deg, unix_seconds)?;
    let (sin_el, cos_el) = position.elevation_deg.to_radians().sin_cos();
    let (sin_az, cos_az) = position.azimuth_deg.to_radians().sin_cos();
    Some(SunLight {
        direction_enu: [cos_el * sin_az, cos_el * cos_az, sin_el],
        colour: sun_tint(position.elevation_deg),
        ambient: sky_ambient(position.elevation_deg),
        position,
    })
}

/// The **direct beam**, as `(elevation_deg, linear RGB)` knots in ascending
/// elevation. Consumed through `N·L`.
///
/// - **+90° to +30°** — neutral. The beam crosses about one air mass; there is
///   nothing to redden it.
/// - **+30° to +2°** — warming. Air mass grows as `1/sin(elevation)`, so Rayleigh
///   scattering strips blue out of the direct beam faster and faster.
/// - **+2° to 0°** — amber to sunset red, ~38 air masses at the horizon.
/// - **−0.8333° and below** — nothing. There is no direct beam once the sun has
///   set, and −0.8333° is where it sets: the geometric centre altitude at which
///   standard refraction still lifts the upper limb onto the horizon, the same
///   figure the sunrise/sunset pins use. Everything visible after that moment is
///   scattered light and belongs to [`sky_ambient`].
const BEAM_STOPS: [(f64, [f64; 3]); 7] = [
    (-90.0, [0.000, 0.000, 0.000]),
    (-0.8333, [0.000, 0.000, 0.000]),
    (0.0, [0.950, 0.470, 0.260]),
    (2.0, [1.000, 0.620, 0.360]),
    (10.0, [1.000, 0.880, 0.720]),
    (30.0, [1.000, 0.980, 0.950]),
    (90.0, [1.000, 1.000, 1.000]),
];

/// The **sky**, as `(elevation_deg, linear RGB)` knots in ascending elevation.
/// Consumed without a cosine.
///
/// This ramp is where the night lives, and it is the half a naive design loses.
///
/// - **+90° to +10°** — a blue daylight sky, roughly a quarter of the beam.
/// - **+2° to −1°** — the sunset sky, warm, and by now the *only* light on
///   ground facing away from the sun.
/// - **−1° to −6°** — the afterglow fading through violet to blue. Red drops out
///   of the scattered light too, and the crossover where blue overtakes it sits
///   near −1.3°.
/// - **−6° to −18°** — civil twilight out to astronomical, settling onto a floor.
/// - **below −18°** — the floor, held flat. It is deliberately *not* black:
///   ground has to read by silhouette at 2 a.m., and a notice that the scene is
///   dark is no use to someone trying to see the terrain under a storm.
const SKY_STOPS: [(f64, [f64; 3]); 11] = [
    (-90.0, [0.035, 0.045, 0.075]),
    (-18.0, [0.035, 0.045, 0.075]),
    (-12.0, [0.045, 0.055, 0.090]),
    (-6.0, [0.100, 0.120, 0.200]),
    (-3.0, [0.130, 0.125, 0.185]),
    (-1.0, [0.205, 0.170, 0.195]),
    (0.0, [0.235, 0.185, 0.210]),
    (2.0, [0.250, 0.220, 0.255]),
    (10.0, [0.270, 0.280, 0.340]),
    (30.0, [0.255, 0.285, 0.375]),
    (90.0, [0.250, 0.290, 0.400]),
];

/// Piecewise-linear scan over a knot table, clamped flat outside it.
///
/// A `NaN` elevation returns the first knot: every comparison against `NaN` is
/// false, so the scan finds no segment and falls through — and the first knot is
/// the darkest in both tables, which is the safe way to be wrong about how
/// bright a scene is.
fn ramp(stops: &[(f64, [f64; 3])], elevation_deg: f64) -> [f64; 3] {
    for pair in stops.windows(2) {
        let (lo_deg, lo) = pair[0];
        let (hi_deg, hi) = pair[1];
        if elevation_deg >= lo_deg && elevation_deg <= hi_deg {
            let t = (elevation_deg - lo_deg) / (hi_deg - lo_deg);
            return [
                lo[0] + (hi[0] - lo[0]) * t,
                lo[1] + (hi[1] - lo[1]) * t,
                lo[2] + (hi[2] - lo[2]) * t,
            ];
        }
    }
    let last = stops[stops.len() - 1];
    if elevation_deg > last.0 {
        last.1
    } else {
        stops[0].1
    }
}

/// Linear-light RGB of the sun's **direct beam** at `elevation_deg`, through
/// [`BEAM_STOPS`]. Zero at and below −0.8333°.
///
/// Linear light, not sRGB, and brightness is folded in: multiplying an albedo
/// by this and by `N·L` gives hue and dimming in one operation.
pub fn sun_tint(elevation_deg: f64) -> [f64; 3] {
    ramp(&BEAM_STOPS, elevation_deg)
}

/// Linear-light RGB of the **sky** at `elevation_deg`, through [`SKY_STOPS`].
/// Never zero — see [`SKY_STOPS`] on why the floor is not black.
///
/// Applied without a cosine, so this is the term that actually reaches a pixel
/// at night.
pub fn sky_ambient(elevation_deg: f64) -> [f64; 3] {
    ramp(&SKY_STOPS, elevation_deg)
}

#[cfg(test)]
mod tests;
