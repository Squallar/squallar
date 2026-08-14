use super::*;
use crate::glm::{GlmDataLevel, GlmFlash, GlmSatellite};

/// The ends of the clamp window.
const WEAKEST: f32 = 1e-16;
const STRONGEST: f32 = 1e-12;

/// Fails if an unreported energy renders as an extreme. A `0.0` sentinel
/// does: `0.0f32.log10()` is `-inf`, which clamps to the window floor.
#[test]
fn unknown_energy_draws_between_the_extremes() {
    let unknown = energy_size_scale(None);
    let weakest = energy_size_scale(Some(WEAKEST));
    let strongest = energy_size_scale(Some(STRONGEST));

    assert_eq!(weakest, 0.0, "the window floor should be the channel floor");
    assert_eq!(
        strongest, 1.0,
        "the window ceiling should be the channel ceiling"
    );
    assert!(
        unknown > weakest,
        "an unreported energy must not render as the weakest strike (got {unknown})"
    );
    assert!(
        unknown < strongest,
        "an unreported energy must not render as the strongest strike (got {unknown})"
    );
}

/// A reinstated `0.0` sentinel lands on the floor, indistinguishable from
/// the weakest real strike.
#[test]
fn zero_energy_would_clamp_to_the_floor() {
    assert_eq!(energy_size_scale(Some(0.0)), 0.0);
    assert_eq!(
        energy_size_scale(Some(0.0)),
        energy_size_scale(Some(WEAKEST))
    );
}

#[test]
fn energy_scale_is_monotonic_and_clamped() {
    assert!(energy_size_scale(Some(1e-14)) > energy_size_scale(Some(1e-15)));
    assert_eq!(energy_size_scale(Some(1e-20)), 0.0);
    assert_eq!(energy_size_scale(Some(1e-9)), 1.0);
}

fn render_one(energy: Option<f32>) -> usize {
    let flash = GlmFlash {
        lat: 35.0,
        lon: -97.0,
        energy,
        area: None,
        time: chrono::NaiveDate::from_ymd_opt(2026, 7, 24)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap(),
        satellite: GlmSatellite::GoesEast,
        level: GlmDataLevel::Flash,
    };
    let bounds = GeoBounds {
        min_lat: 34.0,
        max_lat: 36.0,
        min_lon: -98.0,
        max_lon: -96.0,
    };
    let out = rasterize_glm_strikes(
        std::slice::from_ref(&flash),
        &[],
        &bounds,
        128,
        128,
        &GlmRenderParams {
            device_scale: 1.0,
            zoom: 8.0,
            is_dark: true,
            time_window_secs: 300.0,
            now: flash.time,
        },
    );
    // Bolt size is the only thing varying, so painted-pixel count is a
    // proxy for the size the strike was drawn at.
    out.rgba.chunks_exact(4).filter(|px| px[3] > 0).count()
}

/// Pins the *wiring*, not just the mapping: an unreported energy has to
/// reach the canvas as a mid-size bolt.
#[test]
fn unknown_energy_renders_larger_than_the_weakest_strike() {
    let unknown = render_one(None);
    let weakest = render_one(Some(WEAKEST));
    let strongest = render_one(Some(STRONGEST));

    assert!(weakest > 0, "the fixture must actually draw something");
    assert!(
        unknown > weakest,
        "unknown energy drew {unknown} px, the weakest strike drew {weakest} px"
    );
    assert!(
        unknown < strongest,
        "unknown energy drew {unknown} px, the strongest strike drew {strongest} px"
    );
}

// ── Antimeridian ─────────────────────────────────────────────────────────

/// Draw one flash at `lon` into a viewport spanning `min_lon..max_lon`, and
/// return the painted-pixel count. Zero means the flash never reached the
/// canvas.
fn render_at(lon: f64, min_lon: f64, max_lon: f64) -> usize {
    let flash = GlmFlash {
        lat: 20.0,
        lon,
        energy: Some(1e-14),
        area: None,
        time: chrono::NaiveDate::from_ymd_opt(2026, 7, 24)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap(),
        satellite: GlmSatellite::GoesWest,
        level: GlmDataLevel::Flash,
    };
    let bounds = GeoBounds {
        min_lat: 10.0,
        max_lat: 30.0,
        min_lon,
        max_lon,
    };
    let out = rasterize_glm_strikes(
        std::slice::from_ref(&flash),
        &[],
        &bounds,
        128,
        128,
        &GlmRenderParams {
            device_scale: 1.0,
            zoom: 8.0,
            is_dark: true,
            time_window_secs: 300.0,
            now: flash.time,
        },
    );
    out.rgba.chunks_exact(4).filter(|px| px[3] > 0).count()
}

/// The defect. A viewport panned over the dateline arrives unfolded, as
/// `-195..-165`; GLM longitudes arrive folded, so a flash at +172.9 — a real
/// GOES-West detection, the fold is what put it there — is geographically
/// dead centre of that view. It used to fail `lon > max_lon` and never draw.
#[test]
fn a_goes_west_flash_past_the_antimeridian_draws_in_a_dateline_viewport() {
    // +172.9 is inside -195..-165 once shifted by 360 to -187.1.
    let drawn = render_at(172.9, -195.0, -165.0);
    assert!(
        drawn > 0,
        "a flash at +172.9 lies inside a -195..-165 viewport and must draw; \
         drew {drawn} px"
    );
    // The other side of the same dateline view still draws.
    assert!(
        render_at(-170.0, -195.0, -165.0) > 0,
        "the western half too"
    );
}

/// The fix must not make the cull permissive: a flash genuinely outside the
/// view still has to be dropped, or "it draws now" would be satisfied by
/// removing the test altogether.
#[test]
fn a_flash_outside_a_dateline_viewport_is_still_dropped() {
    // +100 shifts to -260, which is west of min_lon -195: outside.
    assert_eq!(
        render_at(100.0, -195.0, -165.0),
        0,
        "a flash 65 degrees outside the view must not draw"
    );
    // And an ordinary, non-straddling viewport keeps rejecting its outside.
    assert_eq!(render_at(-50.0, -98.0, -96.0), 0);
    assert!(render_at(-97.0, -98.0, -96.0) > 0, "inside still draws");
}

/// `wrap_lon` lands in `[min_lon, min_lon + 360)` whatever it is handed, which
/// is what lets the caller drop the `< min_lon` half of the range test.
#[test]
fn wrap_lon_lands_in_the_boxs_own_frame() {
    let mb = MercatorBounds::from_geo(&GeoBounds {
        min_lat: 10.0,
        max_lat: 30.0,
        min_lon: -195.0,
        max_lon: -165.0,
    });
    for lon in [-180.0, -179.9, 0.0, 172.9, 180.0, -195.0, 165.0, 359.0] {
        let w = mb.wrap_lon(lon);
        assert!(
            (-195.0..165.0).contains(&w),
            "{lon} wrapped to {w}, outside [min_lon, min_lon+360)"
        );
        // and it is the same meridian
        let diff = (w - lon).abs() % 360.0;
        assert!(diff < 1e-9 || (diff - 360.0).abs() < 1e-9, "{lon} -> {w}");
    }
    assert!((mb.wrap_lon(172.9) - (-187.1)).abs() < 1e-9);
    // A non-straddling box leaves an interior longitude untouched.
    let ordinary = MercatorBounds::from_geo(&GeoBounds {
        min_lat: 34.0,
        max_lat: 36.0,
        min_lon: -98.0,
        max_lon: -96.0,
    });
    assert!((ordinary.wrap_lon(-97.0) - (-97.0)).abs() < 1e-9);
}
