//! **What a hit cell is decided by, at the edges of the texture.**
//!
//! Two arms of one defect. `HitCells::record` quarters an `f32` texel
//! coordinate with `as u32`, which *saturates*: every negative coordinate, and
//! NaN, becomes 0 — an index the bound test accepts as column or row zero.
//! Nothing should ever hand it one, and until the clamp below, the GLM strike
//! loop did: it stepped from `(px - r) as i32`, which truncates toward zero, so
//! a bolt within its hit radius of the west or north edge sampled outside the
//! texture and had those samples folded back by the cast.
//!
//! Both arms are tested, and so is the direction that would hurt more: a guard
//! that also rejected the real column 0 would delete the hits along two edges
//! of every hit-mapped layer, which is worse than the fold it replaces.

use super::*;

const WINDOW_SECS: f64 = 300.0;

fn as_of() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 24)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
}

#[test]
fn a_negative_or_nan_coordinate_records_nothing() {
    let mut cells = HitCells::new(64, 64);
    cells.record(-12.0, 8.0, 7);
    cells.record(8.0, -3.0, 7);
    cells.record(-1.0, -1.0, 7);
    cells.record(f32::NAN, 8.0, 7);
    cells.record(8.0, f32::NAN, 7);
    cells.record(f32::NEG_INFINITY, 8.0, 7);
    assert!(
        cells.cells.is_empty(),
        "a coordinate outside the texture on the low side saturated into cell \
         0 and recorded a hit there: {:?}",
        cells.cells,
    );
}

#[test]
fn the_real_cell_zero_still_records() {
    let mut cells = HitCells::new(64, 64);
    cells.record(0.0, 0.0, 7);
    cells.record(3.0, 3.0, 9);
    assert_eq!(
        cells.ids_at(0.5 / 16.0, 0.5 / 16.0),
        [7, 9],
        "the guard above must reject only what is outside the texture; texels \
         0..4 are cell (0, 0) and belong to it",
    );
}

/// 6 degrees of longitude across 256 texels.
fn bounds() -> squallar_geo::GeoBounds {
    squallar_geo::GeoBounds {
        min_lat: 34.0,
        max_lat: 36.0,
        min_lon: -100.0,
        max_lon: -94.0,
    }
}

fn strike_cells(lat: f64, lon: f64) -> Vec<u32> {
    let out = rasterize_glm_strikes(
        &GlmStrikesInput {
            flashes: std::sync::Arc::new(vec![FlashPaint {
                lat,
                lon,
                time: as_of(),
                // The mid-size bolt: `energy_size_scale(None)` is 0.5, so at
                // zoom 8 the base is 16 texels, the bolt 16 and the hit disc's
                // radius 9.6.
                energy: None,
            }]),
            zoom: 8.0,
            is_dark: true,
            time_window_secs: WINDOW_SECS,
            now: as_of(),
            device_scale: 1.0,
        },
        &bounds(),
        256,
        128,
    );
    let mut keys: Vec<u32> = out
        .hit_cells
        .expect("the GLM rasterizer always answers cells")
        .cells
        .keys()
        .copied()
        .collect();
    keys.sort_unstable();
    keys
}

/// **The symptom: half a bolt's click target, at the corner.**
///
/// A flash in the north-west corner projects to `px = 0`, `py ≈ 3`, with a hit
/// radius of 9.6. Stepping from `(0 - 9.6) as i32 = -9` sampled x = -9, -5, -1
/// outside the texture, and the saturating cast folded all three into column 0
/// — so the disc's real columns 1 and 2 were sampled at x = 3 and 7 only, and
/// four of its ten cells were never recorded at all. The cell index is
/// `qy * 64 + qx` on this 64x32 grid, and what the truncating loop found was
/// `[0, 1, 64, 65, 128]`: the left half of a bolt whose right half is drawn on
/// screen and answers nothing when clicked.
#[test]
fn a_bolt_at_the_corner_records_the_whole_disc_it_draws() {
    assert_eq!(
        strike_cells(35.95, -100.0),
        [0, 1, 2, 64, 65, 66, 128, 129, 130, 192],
        "the hit disc at the north-west corner spans columns 0..2 and rows \
         0..3; sampling from a truncated negative start reaches only half of \
         them and calls the rest column 0",
    );
}

/// The control: the same bolt well inside the texture is untouched by the
/// clamp, so the change above is about the edges and not about every flash.
/// Its centre is `px = 128`, `py ≈ 64`, radius 9.6, and both spellings step
/// from the same place because nothing is clamped.
#[test]
fn a_bolt_in_the_middle_is_unchanged() {
    assert_eq!(
        strike_cells(35.0, -97.0),
        [
            926, 927, 928, 929, 990, 991, 992, 993, 1054, 1055, 1056, 1057, 1118, 1119, 1120, 1121,
        ],
        "an interior bolt's cells must not move; the clamp only ever raises a \
         start that was below zero",
    );
}
