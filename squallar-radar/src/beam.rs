//! Beam geometry: the one place the crate turns a radar's polar coordinates
//! into height, ground range and geography, and back.

/// Effective earth radius under the standard 4/3 refraction model, km.
pub const RE_EFF_KM: f64 = 6371.0 * 4.0 / 3.0;

/// Half-power beamwidth of the WSR-88D antenna, degrees. A tilt's beam bottom
/// and top sit half of this below and above its centre elevation.
pub const WSR88D_HALF_POWER_BEAMWIDTH_DEG: f64 = 0.95;

/// Half-power beamwidth of the TDWR antenna, degrees.
pub const TDWR_HALF_POWER_BEAMWIDTH_DEG: f64 = 0.55;

/// The half-power beamwidth, degrees, of whichever network the radar nearest
/// `lat`/`lon` belongs to.
pub fn half_power_beamwidth_deg_near(lat: f64, lon: f64) -> f64 {
    match crate::sites::nearest_radar_site(lat, lon) {
        Some((site, _)) if site.is_tdwr() => TDWR_HALF_POWER_BEAMWIDTH_DEG,
        _ => WSR88D_HALF_POWER_BEAMWIDTH_DEG,
    }
}

/// Beam-centre height above the radar, km, at a slant range and elevation.
#[inline]
pub fn height_km(slant_range_km: f64, elev_deg: f64) -> f64 {
    let range_km = slant_range_km;
    let el = elev_deg.to_radians();
    range_km * el.sin() + range_km * range_km / (2.0 * RE_EFF_KM)
}

/// The slant range at which a tilt's beam centre reaches `height_km` above the
/// radar — the exact algebraic inverse of [`height_km`].
#[inline]
pub fn slant_range_for_height_km(height_km: f64, elev_deg: f64) -> f64 {
    let s = elev_deg.to_radians().sin();
    RE_EFF_KM * ((s * s + 2.0 * height_km / RE_EFF_KM).sqrt() - s)
}

/// Beam-centre height above the radar, km, on the **exact** spherical model —
/// `√(r² + Rₑ² + 2·r·Rₑ·sin e) − Rₑ`, taking the elevation in radians.
#[inline]
fn spherical_height_km(slant_range_km: f64, elev_rad: f64) -> f64 {
    let r = slant_range_km;
    (r * r + RE_EFF_KM * RE_EFF_KM + 2.0 * r * RE_EFF_KM * elev_rad.sin()).sqrt() - RE_EFF_KM
}

/// Ground range, km: the arc along the earth from the site to the point under
/// a gate at `slant_range_km` on a tilt of `elev_deg`.
#[inline]
pub fn ground_range_km(slant_range_km: f64, elev_deg: f64) -> f64 {
    let el = elev_deg.to_radians();
    let h = spherical_height_km(slant_range_km, el);
    RE_EFF_KM * (slant_range_km * el.cos() / (RE_EFF_KM + h)).asin()
}

/// The slant range whose gate sits over `ground_range_km` — the exact inverse
/// of [`ground_range_km`].
#[inline]
pub fn slant_range_for_ground_km(ground_range_km: f64, elev_deg: f64) -> f64 {
    let el = elev_deg.to_radians();
    let theta = ground_range_km / RE_EFF_KM;
    RE_EFF_KM * theta.sin() / (el + theta).cos()
}

/// Beam-centre height above the radar, km, over a point at `ground_range_km`
/// from the site on a tilt of `elev_deg`.
#[inline]
pub fn height_at_ground_km(ground_range_km: f64, elev_deg: f64) -> f64 {
    height_km(
        slant_range_for_ground_km(ground_range_km, elev_deg),
        elev_deg,
    )
}

#[cfg(test)]
mod tests;
