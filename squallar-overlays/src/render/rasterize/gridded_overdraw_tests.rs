//! **What the gridded scatter writes, against what the picture holds.**
//!
//! [`rasterize_gridded`] paints one axis-aligned rect per grid point, sized from
//! the point's neighbour spacing, and the sizing has a `0.5` px floor on each
//! half-extent. Once a cell's spacing goes sub-pixel that floor is what decides
//! the rect: **two pixels by two, whatever the cell's real size is.** At the
//! zoomed-out viewport a global grid samples the picture at about 1:1 — fifteen
//! million GMGSI cells over a ten and a half million pixel picture — and the
//! scatter wrote sixty million pixels into it, four per cell, 5.61 times the
//! picture. MRMS's twenty-four and a half million cells wrote ninety-eight
//! million, 9.17 times.
//!
//! **The floor was not protecting against holes.** A rect is built from
//! `x0..=x1` inclusive, so it is at least one pixel wide however small the
//! half-extent is, and consecutive cells' real intervals overlap by `0.1` of
//! their spacing whatever the scale — the union of a run is contiguous with the
//! floor and without it. What the floor was protecting is the *seam between two
//! cells at multi-pixel spacing*, where rounding two independent centres can
//! part two rects; that case is untouched, because nothing here changes any
//! rect's size. The overlap is given up instead of the extent:
//! [`CellRect::clip_x`] hands a cell's right columns and bottom rows to the
//! cells drawn after it, which were overwriting them anyway.
//!
//! The viewport is the one `gmgsi_seam_probe_tests` froze off a user's laptop,
//! reached through that module's own transcriptions of
//! `walkers::Projector::unproject` and
//! `squallar_egui::overlay_cache::OverlayTexturePlan::coverage` — one spelling
//! of the reconstruction, not two, so a row here and a row there describe the
//! same picture.
//!
//! **Every figure names its denominator.** `written` is pixel *stores*, counted
//! once per store by [`gridded_ledger`]; `picture` is `width * height`;
//! `painted` is output pixels left non-transparent, which is *coverage* and is
//! what a hole would take away. `written` and `painted` are never added and
//! never compared without saying which is which: a change that cut `written` by
//! cutting `painted` would have stopped drawing data, which is the failure this
//! module exists to tell apart from the win.

use super::gmgsi_seam_probe_tests as viewport;
use super::gridded_ledger;
use super::{CellRect, GeoBounds, GriddedInput, rasterize_gridded};
use crate::render::gridded::{GridValues, ResidentGrid, field_paint};
use squallar_source::product::FieldId;
use std::sync::Arc;

/// Output pixels that would change the frame they were drawn on. **Straight
/// alpha** — [`rasterize_gridded`] writes unmultiplied bytes — so the test is
/// the alpha byte and nothing else.
fn painted(rgba: &[u8]) -> u64 {
    rgba.chunks_exact(4).filter(|px| px[3] != 0).count() as u64
}

/// One picture's reading.
struct Reading {
    label: String,
    width: u32,
    height: u32,
    cells: u64,
    written: u64,
    painted: u64,
}

impl Reading {
    fn picture_px(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// **The ceiling every leg is held to, derived rather than recorded.**
    ///
    /// A scatter that paints each pixel once writes at most what the picture
    /// holds. A scatter over a grid *finer* than the picture cannot need more
    /// stores than it has cells. Neither term is a tuned number — one is the
    /// texture's own size and the other is the grid's own count — and the
    /// larger of the two is the honest bound wherever the sampling ratio sits.
    ///
    /// The 1 % is what a coverage *boundary* costs: a cell with no later
    /// neighbour to hand its overlap to keeps the rect it was sized to, so the
    /// perimeter of every painted region writes a little twice. Measured at
    /// 0.03 % over the ten legs below, and 5.4 % of a much smaller number on
    /// the deliberately hole-riddled MRMS arm, where the perimeter is most of
    /// the region.
    fn ceiling(&self) -> u64 {
        self.cells.max(self.picture_px()) * 101 / 100
    }

    fn print(&self) {
        println!(
            "{:<32} {:>5}x{:<5} cells {:>10}  written {:>11}  \
             x picture {:>6.2}  per cell {:>8.2}  painted {:>10} ({:>5.1} %)",
            self.label,
            self.width,
            self.height,
            self.cells,
            self.written,
            self.written as f64 / self.picture_px() as f64,
            if self.cells > 0 {
                self.written as f64 / self.cells as f64
            } else {
                0.0
            },
            self.painted,
            100.0 * self.painted as f64 / self.picture_px() as f64,
        );
    }
}

/// Render one picture and read the ledger's delta over it.
///
/// The delta is taken off [`gridded_ledger::thread_totals`], which is this
/// thread's own: libtest runs a thread per test, so a process-wide delta would
/// count whatever another suite rastered in between.
fn read(label: &str, input: &GriddedInput, cov: &GeoBounds, w: u32, h: u32) -> (Reading, Vec<u8>) {
    let before = gridded_ledger::thread_totals();
    let out = rasterize_gridded(input, cov, w, h);
    let delta = gridded_ledger::thread_totals().since(&before);
    assert_eq!(
        delta.pictures, 1,
        "{label}: one raster must post exactly one picture to the ledger"
    );
    assert_eq!(
        delta.picture_px,
        u64::from(w) * u64::from(h),
        "{label}: the ledger's denominator must be the picture it drew"
    );
    let reading = Reading {
        label: label.to_string(),
        width: w,
        height: h,
        cells: delta.cells,
        written: delta.written_px,
        painted: painted(&out.rgba),
    };
    (reading, out.rgba.into_bytes())
}

// ── The grids, at their real shapes ───────────────────────────────────────

/// A value per cell that **differs from its neighbours in both axes**, so a
/// picture is sensitive to *which* cell won a pixel and not merely to whether a
/// pixel was painted. A constant field would look the same however the cells
/// were assigned, and that is exactly the property under test.
fn varying(ni: usize, nj: usize, lo: f32, hi: f32) -> Vec<f32> {
    let span = hi - lo;
    let mut v = Vec::with_capacity(ni * nj);
    for j in 0..nj {
        for i in 0..ni {
            // Two mutually prime strides: no two of the four neighbours of a
            // cell share its value, and the pattern does not repeat inside a
            // rect any cell can paint.
            let t = ((i * 7 + j * 13) % 251) as f32 / 251.0;
            v.push(lo + t * span);
        }
    }
    v
}

/// The same field with rectangular no-data blocks cut out of it, which is what
/// a real mosaic looks like: MRMS is transparent over every ocean and every gap
/// between radar umbrellas. **This is the arm a hole can appear in** — a cell
/// whose neighbour is absent has no later cell to hand its overlap to, and the
/// perimeter it keeps is the whole of the 1 % the ceiling allows.
fn with_gaps(mut v: Vec<f32>, ni: usize, nj: usize) -> Vec<f32> {
    for j in 0..nj {
        for i in 0..ni {
            if (i / 97 + j / 89) % 3 == 0 {
                v[j * ni + i] = f32::NAN;
            }
        }
    }
    v
}

const GMGSI_FIELD: &str = "GmgsiLongwaveIr";
const MRMS_FIELD: &str = "mrms_reflectivity";
/// Brightness temperatures in kelvin, the unit GMGSI's longwave band carries.
const GMGSI_VALUES: (f32, f32) = (180.0, 300.0);
/// dBZ, above the mosaic bar's 5 dBZ floor so every cell paints.
const MRMS_VALUES: (f32, f32) = (5.0, 75.0);

/// **The fixtures' own premise, asserted rather than assumed.** Every value
/// below must paint, or `painted` stops describing coverage and the no-hole
/// halves of the gates below go vacuous. A ramp edited past these ends fails
/// here, where the reason is legible, instead of in a count nobody can read.
fn every_value_paints() {
    for (field, (lo, hi)) in [(GMGSI_FIELD, GMGSI_VALUES), (MRMS_FIELD, MRMS_VALUES)] {
        let paint = field_paint(&FieldId::from_static(field))
            .unwrap_or_else(|| panic!("{field} must be a registered field"));
        for k in 0..=20u32 {
            let v = lo + (hi - lo) * k as f32 / 20.0;
            assert!(
                paint.paints(v),
                "{field}: this module's fixture spans {lo}..{hi} and the bar \
                 paints nothing at {v}, so `painted` no longer measures coverage"
            );
        }
    }
}

fn gmgsi(values: Vec<f32>) -> GriddedInput {
    viewport::grid(viewport::wrapping_lon_axis(), GridValues::F32(values))
}

const MRMS_NI: usize = 7000;
const MRMS_NJ: usize = 3500;

/// The CONUS mosaic as `mrms::decode` builds it: grid definition template 3.0,
/// 7000 x 3500 at 0.01 degrees, scanning from the north-west corner.
fn mrms(values: Vec<f32>) -> GriddedInput {
    GriddedInput::Resident(Arc::new(ResidentGrid {
        field: FieldId::from_static(MRMS_FIELD),
        ni: MRMS_NI,
        nj: MRMS_NJ,
        coords: crate::hrrr::GridCoords::Regular {
            lat0: 54.995,
            lon0: -129.995,
            dlat: -0.01,
            dlon: 0.01,
            ni: MRMS_NI,
            nj: MRMS_NJ,
            scan_mode: 0,
        },
        values: GridValues::F32(values),
    }))
}

/// HRRR at its real 1799 x 1059, on its own Lambert cone.
///
/// **The conservative arm.** A Lambert row is not a parallel, so two cells
/// beside each other can sit a pixel apart in `y`; where they do,
/// [`CellRect::clip_x`]'s span condition fails and the cell keeps its overlap.
/// The saving is therefore smaller here than on a separable grid, and the
/// picture is right for the same reason it is right there.
fn hrrr() -> GriddedInput {
    GriddedInput::Whole(Arc::new(super::lambert_fixture::lambert_grid(
        1799,
        1059,
        0b0100_0000,
    )))
}

/// The frozen pane's picture size at the shipped 150 % oversample: 4317 x 2476
/// = 10,688,892 px, the denominator every `x picture` is taken against.
fn picture() -> (u32, u32) {
    (
        (viewport::PANE_W * (1.0 + 2.0 * viewport::OVERDRAW)) as u32,
        (viewport::PANE_H * (1.0 + 2.0 * viewport::OVERDRAW)) as u32,
    )
}

fn box_at(zoom: f64) -> GeoBounds {
    viewport::coverage(&viewport::viewport_bounds(zoom), viewport::OVERDRAW)
}

/// The zoom ladder, frozen zoom first.
const ZOOMS: [f64; 5] = [viewport::FROZEN_ZOOM, 5.0, 7.0, 9.0, 11.0];

// ── The gates ─────────────────────────────────────────────────────────────

/// **A gridded raster writes no more than the larger of its two sizes.**
///
/// The frozen sub-pixel viewport and a zoom at which the grid covers the whole
/// picture, on both a wrapping global grid and a regional mosaic. Held to
/// [`Reading::ceiling`], which is derived from the picture and the grid and
/// carries no recorded figure.
///
/// **Red on the unfixed scatter at every leg**, by factors of 5.6, 15.7, 9.2
/// and 4.2 respectively.
#[test]
fn a_gridded_raster_writes_no_more_pixels_than_it_has_cells_or_the_picture_has() {
    every_value_paints();
    let (w, h) = picture();
    let mut legs = Vec::new();

    let g = gmgsi(varying(
        viewport::NI,
        viewport::NJ,
        GMGSI_VALUES.0,
        GMGSI_VALUES.1,
    ));
    for z in [viewport::FROZEN_ZOOM, 9.0] {
        legs.push(read(&format!("GMGSI zoom {z:.2}"), &g, &box_at(z), w, h).0);
    }
    drop(g);

    let m = mrms(varying(MRMS_NI, MRMS_NJ, MRMS_VALUES.0, MRMS_VALUES.1));
    for z in [viewport::FROZEN_ZOOM, 7.0] {
        legs.push(read(&format!("MRMS zoom {z:.2}"), &m, &box_at(z), w, h).0);
    }
    drop(m);

    for leg in &legs {
        leg.print();
    }
    for leg in &legs {
        assert!(
            leg.written <= leg.ceiling(),
            "{}: the scatter stored {} pixels into a {} px picture over {} cells \
             ({:.2} x the picture, {:.2} per cell); the ceiling is \
             max(cells, picture) + 1 % = {}",
            leg.label,
            leg.written,
            leg.picture_px(),
            leg.cells,
            leg.written as f64 / leg.picture_px() as f64,
            leg.written as f64 / leg.cells as f64,
            leg.ceiling(),
        );
    }
}

/// **The other half: the picture is still full.**
///
/// The companion the ceiling above needs, and the one that tells the win from
/// the failure it is confusable with. A rasterizer that stopped drawing data
/// would pass the ceiling trivially — `written` would collapse and so would its
/// denominator — so a zoom at which the grid demonstrably covers the whole
/// texture is held to **exact** full coverage. No slack and no recorded count:
/// the claim is `painted == width * height`, which is the picture's own size.
///
/// Green with and without the fix, which is what it is for: it is the healthy
/// input that most resembles the defect, and it is what a clip that gave up a
/// pixel nobody else painted would fail.
#[test]
fn a_grid_that_covers_the_texture_leaves_no_pixel_unpainted() {
    every_value_paints();
    let (w, h) = picture();
    let expect = u64::from(w) * u64::from(h);

    let g = gmgsi(varying(
        viewport::NI,
        viewport::NJ,
        GMGSI_VALUES.0,
        GMGSI_VALUES.1,
    ));
    let (gmgsi_leg, _) = read("GMGSI zoom 9.00", &g, &box_at(9.0), w, h);
    drop(g);
    let m = mrms(varying(MRMS_NI, MRMS_NJ, MRMS_VALUES.0, MRMS_VALUES.1));
    let (mrms_leg, _) = read("MRMS zoom 7.00", &m, &box_at(7.0), w, h);
    drop(m);

    for leg in [&gmgsi_leg, &mrms_leg] {
        leg.print();
        assert_eq!(
            leg.painted,
            expect,
            "{}: the grid covers this whole viewport, so every one of the \
             {expect} pixels must carry colour; {} do, leaving {} transparent",
            leg.label,
            leg.painted,
            expect - leg.painted,
        );
    }
}

/// **The clipping algebra, exhaustively.**
///
/// [`CellRect::clip_x`] and [`CellRect::clip_y`] are the whole of the change,
/// and their one obligation is that what they take away, the later cell puts
/// back: a pixel given up must be a pixel `l` covers. Everything else — how
/// much is given up, and whether anything is — is economy.
///
/// Every ordered pair of rects inside a 4 x 4 box: 10 000 pairs, both
/// directions, checked as sets of pixels rather than as coordinates. This is a
/// proof over the algebra, not a sample of it, and it needs no fixture and no
/// recorded value.
#[test]
fn a_clip_gives_up_only_pixels_the_later_cell_covers() {
    fn rects() -> Vec<CellRect> {
        let mut out = Vec::new();
        for x0 in 0..4i32 {
            for x1 in x0..4 {
                for y0 in 0..4i32 {
                    for y1 in y0..4 {
                        out.push(CellRect {
                            x0,
                            y0,
                            x1,
                            y1,
                            color: ecolor::Color32::from_rgba_premultiplied(1, 2, 3, 4),
                        });
                    }
                }
            }
        }
        out
    }
    fn pixels(r: &CellRect) -> Vec<(i32, i32)> {
        if r.is_empty() {
            return Vec::new();
        }
        (r.y0..=r.y1)
            .flat_map(|y| (r.x0..=r.x1).map(move |x| (x, y)))
            .collect()
    }

    let all = rects();
    let mut clipped_any = 0usize;
    for e in &all {
        for l in &all {
            for (axis, apply) in [
                ("clip_x", CellRect::clip_x as fn(&mut CellRect, &CellRect)),
                ("clip_y", CellRect::clip_y as fn(&mut CellRect, &CellRect)),
            ] {
                let mut c = *e;
                apply(&mut c, l);
                let before = pixels(e);
                let after = pixels(&c);
                let covered = pixels(l);
                for p in &after {
                    assert!(
                        before.contains(p),
                        "{axis}: {e:?} against {l:?} kept {p:?}, which it never had"
                    );
                }
                for p in &before {
                    assert!(
                        after.contains(p) || covered.contains(p),
                        "{axis}: {e:?} against {l:?} gave up {p:?}, which {l:?} \
                         does not cover — that pixel loses its colour"
                    );
                }
                if after.len() < before.len() {
                    clipped_any += 1;
                }
            }
        }
    }
    assert!(
        clipped_any > all.len(),
        "only {clipped_any} of {} pairs clipped anything, so this proves \
         nothing about a clip that fires",
        all.len() * all.len() * 2
    );
}

/// **The measurement.** Both zoomed-out grids across the whole zoom ladder,
/// the deliberately gapped MRMS arm beside the all-finite one, and HRRR's
/// Lambert cone — which is the conservative case. Not a gate: it is minutes
/// long and it is what the ratchets above were cut from.
#[test]
#[ignore = "measurement: cargo test --release -- --ignored gridded_scatter --nocapture"]
fn gridded_scatter_cost_across_the_zoom_ladder() {
    every_value_paints();
    let (w, h) = picture();
    println!(
        "\npicture {w} x {h} = {} px   (denominator of every `x picture` below)\n",
        u64::from(w) * u64::from(h)
    );

    let g = gmgsi(varying(
        viewport::NI,
        viewport::NJ,
        GMGSI_VALUES.0,
        GMGSI_VALUES.1,
    ));
    for z in ZOOMS {
        read(&format!("GMGSI z{z:.2}"), &g, &box_at(z), w, h)
            .0
            .print();
    }
    drop(g);

    let solid = varying(MRMS_NI, MRMS_NJ, MRMS_VALUES.0, MRMS_VALUES.1);
    let gapped = mrms(with_gaps(solid.clone(), MRMS_NI, MRMS_NJ));
    let m = mrms(solid);
    for z in ZOOMS {
        read(&format!("MRMS z{z:.2} all-finite"), &m, &box_at(z), w, h)
            .0
            .print();
    }
    drop(m);
    for z in ZOOMS {
        read(
            &format!("MRMS z{z:.2} with gaps"),
            &gapped,
            &box_at(z),
            w,
            h,
        )
        .0
        .print();
    }
    drop(gapped);

    let hrrr = hrrr();
    for z in ZOOMS {
        read(&format!("HRRR z{z:.2} lambert"), &hrrr, &box_at(z), w, h)
            .0
            .print();
    }
}
