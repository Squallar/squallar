//! The depicted instant is a **bound**, not just an origin: a flash stamped
//! later than it has not happened yet, and belongs on no canvas.

use super::*;

const WINDOW_SECS: f64 = 300.0;

fn as_of() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 24)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
}

/// 6 degrees of longitude across 256 texels, so a flash's longitude picks its
/// half of the canvas: -98.5 lands near x=64, -95.5 near x=192.
fn bounds() -> GeoBounds {
    GeoBounds {
        min_lat: 34.0,
        max_lat: 36.0,
        min_lon: -100.0,
        max_lon: -94.0,
    }
}

const WIDTH: u32 = 256;
const HEIGHT: u32 = 128;

fn flash(lon: f64, time: chrono::NaiveDateTime) -> FlashPaint {
    FlashPaint {
        lat: 35.0,
        lon,
        time,
        energy: Some(1e-14),
    }
}

fn render(flashes: Vec<FlashPaint>) -> RasterizeOutput {
    rasterize_glm_strikes(
        &GlmStrikesInput {
            flashes: std::sync::Arc::new(flashes),
            zoom: 8.0,
            is_dark: true,
            time_window_secs: WINDOW_SECS,
            now: as_of(),
            device_scale: 1.0,
        },
        &bounds(),
        WIDTH,
        HEIGHT,
    )
}

/// Painted-pixel counts either side of the vertical midline.
fn halves(out: &RasterizeOutput) -> (usize, usize) {
    let mut left = 0;
    let mut right = 0;
    for (i, px) in out.rgba.chunks_exact(4).enumerate() {
        if px[3] == 0 {
            continue;
        }
        if (i as u32 % WIDTH) < WIDTH / 2 {
            left += 1;
        } else {
            right += 1;
        }
    }
    (left, right)
}

/// The largest blue-over-alpha ratio any painted pixel carries.
///
/// **This, and not the alpha channel, is where the age lives.** The age ramp
/// returns a *constant* alpha (230 dark, 200 light) at every age and walks
/// white -> yellow -> orange -> red in RGB, so an alpha assertion cannot tell a
/// fresh strike from a stale one. Blue is the channel that collapses: age zero
/// is white (blue 255, ratio 1.0), and a strike half way down the ramp is
/// orange (blue 26, ratio ~0.10). The output is premultiplied and antialiased,
/// which scales blue and alpha by the same coverage, so the ratio survives
/// both.
fn peak_blue_ratio(out: &RasterizeOutput) -> f32 {
    out.rgba
        .chunks_exact(4)
        .filter(|px| px[3] >= 64)
        .map(|px| px[2] as f32 / px[3] as f32)
        .fold(0.0f32, f32::max)
}

/// Two flashes straddling the depicted instant. The one that has not happened
/// yet must not reach the canvas at all — and specifically must not reach it
/// at the ramp's peak, which is what a bare `.max(0)` on the age produced.
#[test]
fn a_flash_after_the_depicted_instant_is_not_drawn() {
    let out = render(vec![
        flash(-98.5, as_of() - chrono::TimeDelta::seconds(150)),
        flash(-95.5, as_of() + chrono::TimeDelta::seconds(30)),
    ]);

    let (left, right) = halves(&out);
    assert!(
        left > 0,
        "the fixture must actually draw the past strike, or every assertion \
         below is vacuous",
    );
    assert_eq!(
        right, 0,
        "the flash stamped 30 s after the depicted instant painted {right} \
         texels; a scrubbed pane must not show a strike that has not happened",
    );

    let peak = peak_blue_ratio(&out);
    assert!(
        peak < 0.5,
        "a painted texel carries blue/alpha {peak:.3}; the age-zero end of the \
         ramp is white at 1.00 and the 150 s strike is orange at about 0.10. \
         Anything near 1.00 means a future flash was drawn at FULL brightness, \
         which is the bug: age 0 is the brightest colour the ramp has, not the \
         dimmest.",
    );

    assert_eq!(
        out.hit_cells.as_ref().and_then(|c| c.max_id()),
        Some(0),
        "the hit map named a flash past index 0; a click must not be able to \
         select a strike that has not happened",
    );
}

/// The counterpart: nothing about the cull touches a flash at or before the
/// depicted instant, including one stamped on it exactly.
#[test]
fn a_flash_on_or_before_the_depicted_instant_still_draws() {
    let exactly_now = render(vec![flash(-98.5, as_of())]);
    let (left, right) = halves(&exactly_now);
    assert!(
        left > 0 && right == 0,
        "a flash stamped exactly on the depicted instant is age zero, not the \
         future: {left} left / {right} right",
    );
    let peak = peak_blue_ratio(&exactly_now);
    assert!(
        (peak - 1.0).abs() < 0.02,
        "age zero is the white end of the ramp (blue/alpha {peak:.3}); this is \
         the brightness the future flash was borrowing",
    );

    let past = render(vec![flash(
        -98.5,
        as_of() - chrono::TimeDelta::seconds(150),
    )]);
    assert!(halves(&past).0 > 0, "a 150 s strike is inside the window");
}
