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
