//! The floor↔volume alignment instrument.
//!
//! `volume_floor.rs`'s unit tests verify `resample_floor` against the forward
//! projection formulas — but they *restate* those formulas, so a drift that
//! moves both the resampler and the expectation together is invisible to them.
//! This file is the check they cannot be: it runs a **real Level II volume**
//! through both production paths that must agree —
//!
//!   * `rustdar_radar::voxel::build_voxels`, the grid the raymarch draws, and
//!   * `render_from` + `resample_floor`, the floor that grid stands on —
//!
//! and measures where the same weather landed in each, in the floor's own
//! texel space. The echo footprint of the grid's columns and the painted
//! texels of the floor must sit on top of one another; the transform that
//! best maps one onto the other **is the diagnosis**:
//!
//!   * best translation ≈ (0, 0) and identity beating every flip → registered;
//!   * a flip winning → a row-direction disagreement;
//!   * a half-box offset → an origin/sign disagreement;
//!   * shapes aligned but IoU near zero → the two paths read different data.
//!
//! The instrument test is `#[ignore]`d because it reads a volume from disk:
//!
//! ```text
//! VOL=/path/to/KDMX20250314_175512_V06 [THRESH=15] [OUT=/tmp/prefix] \
//! cargo test -p rustdar-frontend --release --test floor_alignment -- --ignored --nocapture
//! ```
//!
//! | variable | required | default | meaning |
//! |---|---|---|---|
//! | `VOL` | yes | — | Uncompressed NEXRAD Level II archive file. |
//! | `SITE` | no | first four characters of `VOL`'s name | Radar ICAO. |
//! | `HALF_KM` | no | the app's default box | Box half-width, km. |
//! | `THRESH` | no | `15.0` | dBZ cut for the grid's echo mask. |
//! | `OUT` | no | — | Prefix; writes `_floor.ppm`, `_grid.pgm`, `_overlay.ppm`. |
#![cfg(not(target_arch = "wasm32"))]

use rustdar_frontend::volume::floor::{FLOOR_GROUND_RGBA, FLOOR_TEXELS, resample_floor};
use rustdar_radar::types::RadarProduct;

// ── The volume (the recipe `volume_real_mask.rs` documents) ──────────────────

/// Decode a whole Level II archive file into a `Scan` through
/// `rustdar_radar::chunks::decode_chunk` — the only bytes-to-`Scan` route in
/// this crate's dependency set; see `volume_real_mask.rs` for why not
/// `nexrad_data::volume::File::scan`.
fn scan_from_archive(path: &std::path::Path) -> nexrad_model::data::Scan {
    let bytes =
        std::fs::read(path).unwrap_or_else(|e| panic!("reading VOL {}: {e}", path.display()));
    assert!(
        !bytes.starts_with(&[0x1f, 0x8b]),
        "{} is gzipped; gunzip it first (see volume_real_mask.rs)",
        path.display(),
    );
    let name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("volume");
    let contents = rustdar_radar::chunks::decode_chunk(name, &bytes)
        .unwrap_or_else(|e| panic!("decoding {}: {e}", path.display()));
    let coverage_pattern = contents
        .coverage_pattern
        .unwrap_or_else(|| panic!("{} carries no message 5", path.display()));
    let sweeps = nexrad_model::data::Sweep::from_radials(contents.radials);
    assert!(
        !sweeps.is_empty(),
        "{} decoded to no sweeps",
        path.display()
    );
    nexrad_model::data::Scan::new(coverage_pattern, sweeps)
}

// ── Masks ────────────────────────────────────────────────────────────────────

/// A binary mask over the floor's own texel lattice, row 0 the box footprint's
/// north edge — both sides of the comparison are expressed on this lattice.
struct Mask {
    side: usize,
    on: Vec<bool>,
}

impl Mask {
    fn count(&self) -> usize {
        self.on.iter().filter(|&&b| b).count()
    }

    fn at(&self, col: i64, row: i64) -> bool {
        if col < 0 || row < 0 || col >= self.side as i64 || row >= self.side as i64 {
            return false;
        }
        self.on[row as usize * self.side + col as usize]
    }

    /// Mask centroid as (col, row), or `None` when empty.
    fn centroid(&self) -> Option<(f64, f64)> {
        let mut n = 0usize;
        let (mut sx, mut sy) = (0.0f64, 0.0f64);
        for row in 0..self.side {
            for col in 0..self.side {
                if self.on[row * self.side + col] {
                    n += 1;
                    sx += col as f64;
                    sy += row as f64;
                }
            }
        }
        (n > 0).then(|| (sx / n as f64, sy / n as f64))
    }
}

/// Intersection-over-union of `a` against `b` **transformed**: texel
/// `(c, r)` of `a` is compared with `b` at `(c', r')` where each axis is
/// optionally mirrored and then shifted.
fn iou(a: &Mask, b: &Mask, flip_x: bool, flip_y: bool, dx: i64, dy: i64) -> f64 {
    let side = a.side as i64;
    let mut inter = 0usize;
    let mut union = 0usize;
    for row in 0..side {
        for col in 0..side {
            let av = a.at(col, row);
            let (mut bc, mut br) = (col, row);
            if flip_x {
                bc = side - 1 - bc;
            }
            if flip_y {
                br = side - 1 - br;
            }
            let bv = b.at(bc + dx, br + dy);
            if av && bv {
                inter += 1;
            }
            if av || bv {
                union += 1;
            }
        }
    }
    if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    }
}

/// The translation in `±reach` (coarse step then a ±(step) refine) that
/// maximises IoU with no flip, and that IoU.
fn best_translation(a: &Mask, b: &Mask, reach: i64, step: i64) -> ((i64, i64), f64) {
    let mut best = ((0i64, 0i64), -1.0f64);
    let consider = |dx: i64, dy: i64, best: &mut ((i64, i64), f64)| {
        let v = iou(a, b, false, false, dx, dy);
        if v > best.1 {
            *best = ((dx, dy), v);
        }
    };
    let mut dy = -reach;
    while dy <= reach {
        let mut dx = -reach;
        while dx <= reach {
            consider(dx, dy, &mut best);
            dx += step;
        }
        dy += step;
    }
    let (cx, cy) = best.0;
    for dy in (cy - step)..=(cy + step) {
        for dx in (cx - step)..=(cx + step) {
            consider(dx, dy, &mut best);
        }
    }
    best
}

// ── Output ───────────────────────────────────────────────────────────────────

fn write_ppm_rgba(path: &str, side: usize, rgba: &[u8]) {
    let mut out = format!("P6\n{side} {side}\n255\n").into_bytes();
    for px in rgba.chunks_exact(4) {
        out.extend_from_slice(&px[..3]);
    }
    std::fs::write(path, out).unwrap_or_else(|e| panic!("writing {path}: {e}"));
}

fn write_pgm_mask(path: &str, mask: &Mask) {
    let mut out = format!("P5\n{} {}\n255\n", mask.side, mask.side).into_bytes();
    out.extend(mask.on.iter().map(|&b| if b { 255u8 } else { 0 }));
    std::fs::write(path, out).unwrap_or_else(|e| panic!("writing {path}: {e}"));
}

/// Red = grid only, green = floor only, yellow = both.
fn write_overlay(path: &str, grid: &Mask, floor: &Mask) {
    let side = grid.side;
    let mut out = format!("P6\n{side} {side}\n255\n").into_bytes();
    for row in 0..side {
        for col in 0..side {
            let g = grid.on[row * side + col];
            let f = floor.on[row * side + col];
            out.extend_from_slice(&[if g { 255 } else { 0 }, if f { 255 } else { 0 }, 0]);
        }
    }
    std::fs::write(path, out).unwrap_or_else(|e| panic!("writing {path}: {e}"));
}

// ── The instrument ───────────────────────────────────────────────────────────

#[test]
#[ignore = "reads a Level II volume from VOL; run with --ignored --nocapture"]
fn measure_floor_against_grid_on_a_real_volume() {
    let vol = std::path::PathBuf::from(std::env::var("VOL").expect("set VOL"));
    let icao = std::env::var("SITE").unwrap_or_else(|_| {
        vol.file_name()
            .and_then(std::ffi::OsStr::to_str)
            .map(|n| n.chars().take(4).collect())
            .expect("SITE, or a VOL file name starting with the ICAO")
    });
    let site = rustdar_radar::sites::get_radar_site(&icao)
        .unwrap_or_else(|| panic!("{icao} is not a site this build knows"));
    let half_km: f64 = std::env::var("HALF_KM")
        .ok()
        .map(|s| s.parse().expect("HALF_KM must be a number"))
        .unwrap_or(rustdar_egui::pane::DEFAULT_HALF_WIDTH_KM);
    let thresh: f32 = std::env::var("THRESH")
        .ok()
        .map(|s| s.parse().expect("THRESH must be a number"))
        .unwrap_or(15.0);

    let scan = scan_from_archive(&vol);

    // The grid, exactly as `handle_prepare_volume` requests the default box.
    let request = rustdar_radar::voxel::VoxelRequest {
        centre: (site.lat, site.lon),
        half_width_km: half_km,
        base_km_msl: rustdar_radar::voxel::DEFAULT_BASE_KM_MSL,
        top_km_msl: rustdar_radar::voxel::DEFAULT_TOP_KM_MSL,
        product: RadarProduct::Reflectivity,
        shape: rustdar_radar::voxel::default_shape(),
        values_wanted: false,
    };
    let grid = rustdar_radar::voxel::build_voxels(&scan, &request, site.lat, site.lon)
        .expect("a buildable grid");

    // The floor, exactly as `maybe_spawn_floor_render` + the job render it.
    let elevation =
        rustdar_radar::render::find_closest_elevation(&scan, RadarProduct::Reflectivity, 0.0)
            .expect("a reflectivity tilt");
    let input = rustdar_radar::render_input::RenderInput::extract(
        &scan,
        elevation,
        RadarProduct::Reflectivity,
        site.lat,
        site.lon,
        None,
        None,
    )
    .expect("a renderable base tilt");
    let (image, _data_reach_km, _values) =
        rustdar_radar::render::render_from(&input).expect("a rendered base tilt");
    let floor_image = resample_floor(&image, site.lat, grid.x_range_km(), grid.y_range_km())
        .expect("a resampled floor");

    let side = FLOOR_TEXELS as usize;
    assert_eq!(floor_image.size, [FLOOR_TEXELS; 2]);

    // Floor mask: any texel visibly painted above the ground colour.
    let floor_mask = Mask {
        side,
        on: floor_image
            .rgba
            .chunks_exact(4)
            .map(|px| {
                px[..3]
                    .iter()
                    .zip(&FLOOR_GROUND_RGBA[..3])
                    .any(|(&c, &g)| c.abs_diff(g) > 6)
            })
            .collect(),
    };

    // Grid mask: the column-max echo, on the same lattice. Texel (col, row)
    // maps to km exactly as `resample_floor`'s output is documented: row 0 is
    // the footprint's north edge.
    let shape = grid.shape();
    let cut = grid.value_to_index(thresh);
    let mut column_max = vec![0u8; shape.nx * shape.ny];
    for iz in 0..shape.nz {
        for iy in 0..shape.ny {
            for ix in 0..shape.nx {
                let v = grid.index_at(ix, iy, iz).unwrap();
                let slot = &mut column_max[iy * shape.nx + ix];
                *slot = (*slot).max(v);
            }
        }
    }
    let (x0, x1) = grid.x_range_km();
    let (y0, y1) = grid.y_range_km();
    let mut grid_on = vec![false; side * side];
    for row in 0..side {
        let y_km = y1 - (row as f64 + 0.5) / side as f64 * (y1 - y0);
        let iy = ((y_km - y0) / (y1 - y0) * shape.ny as f64) as usize;
        let iy = iy.min(shape.ny - 1);
        for col in 0..side {
            let x_km = x0 + (col as f64 + 0.5) / side as f64 * (x1 - x0);
            let ix = ((x_km - x0) / (x1 - x0) * shape.nx as f64) as usize;
            let ix = ix.min(shape.nx - 1);
            grid_on[row * side + col] = column_max[iy * shape.nx + ix] >= cut && cut > 0;
        }
    }
    let grid_mask = Mask { side, on: grid_on };

    // ── The numbers ──────────────────────────────────────────────────────
    let km_per_texel = (x1 - x0) / side as f64;
    println!("volume: {}", vol.display());
    println!(
        "box: x {:.1}..{:.1} km, y {:.1}..{:.1} km ({:.3} km/texel), grid {}x{}x{}",
        x0, x1, y0, y1, km_per_texel, shape.nx, shape.ny, shape.nz,
    );
    println!(
        "masks: floor {} texels painted, grid {} texels ≥ {thresh} dBZ (column max)",
        floor_mask.count(),
        grid_mask.count(),
    );
    let identity = iou(&grid_mask, &floor_mask, false, false, 0, 0);
    println!("IoU identity: {identity:.4}");
    println!(
        "IoU flip x:   {:.4}",
        iou(&grid_mask, &floor_mask, true, false, 0, 0)
    );
    println!(
        "IoU flip y:   {:.4}",
        iou(&grid_mask, &floor_mask, false, true, 0, 0)
    );
    println!(
        "IoU flip xy:  {:.4}",
        iou(&grid_mask, &floor_mask, true, true, 0, 0)
    );
    let ((dx, dy), at_best) = best_translation(&grid_mask, &floor_mask, 96, 4);
    println!(
        "best translation: ({dx}, {dy}) texels = ({:.2}, {:.2}) km east/south, IoU {at_best:.4}",
        dx as f64 * km_per_texel,
        dy as f64 * km_per_texel,
    );
    if let (Some(gc), Some(fc)) = (grid_mask.centroid(), floor_mask.centroid()) {
        println!(
            "centroids: grid ({:.1}, {:.1}), floor ({:.1}, {:.1}), delta ({:+.1}, {:+.1}) texels",
            gc.0,
            gc.1,
            fc.0,
            fc.1,
            fc.0 - gc.0,
            fc.1 - gc.1,
        );
    }

    if let Ok(prefix) = std::env::var("OUT") {
        write_ppm_rgba(&format!("{prefix}_floor.ppm"), side, &floor_image.rgba);
        write_pgm_mask(&format!("{prefix}_grid.pgm"), &grid_mask);
        write_overlay(&format!("{prefix}_overlay.ppm"), &grid_mask, &floor_mask);
        println!("wrote {prefix}_floor.ppm, {prefix}_grid.pgm, {prefix}_overlay.ppm");
    }
}

// ── The pin: a synthetic storm, both production paths, no file, no GPU ───────
//
// The instrument above needs a volume on disk; this is the same comparison as
// a test the gauntlet runs every time. A 55 dBZ disc is planted at a known
// offset from the site and pushed through **both** production paths — the
// voxel build the raymarch draws, and the real 2D rasterizer + `resample_floor`
// the floor ships through. Neither expectation restates a projection formula:
// the oracle is the planted disc's own position, and the assertion is that the
// two paths put it in the same place.
//
// What it closes that the `volume_floor.rs` unit tests cannot:
//
//  * the 2026-08-09 2× floor zoom — the raster's *data reach* fed to the
//    resampler as its half-extent. The fixture's low tilt reaches 177 km, not
//    the raster's 230, precisely so that confusing the two numbers again
//    moves the disc's floor footprint ~33 km outward and fails the bound;
//  * coordinated drift between `resample_floor` and `MercatorProjection` —
//    the raster here comes from the real renderer, not from a restated
//    formula;
//  * axis flips, which mirror the off-centre, off-diagonal disc across the
//    box and miss by hundreds of kilometres.

/// One reflectivity sweep over `field(azimuth_deg, slant_km) -> Option<dBZ>`,
/// on the operational super-res gate layout (centre of gate 0 at 2.125 km,
/// 250 m gates — the same numbers `rustdar-radar`'s own fixtures fly).
fn refl_sweep(
    elevation_number: u8,
    elevation_deg: f32,
    radial_count: usize,
    n_gates: usize,
    field: &dyn Fn(f64, f64) -> Option<f64>,
) -> nexrad_model::data::Sweep {
    use nexrad_model::data::{MomentData, Radial, RadialStatus};
    const FIRST_GATE_M: u16 = 2125;
    const GATE_M: u16 = 250;
    let spacing = 360.0 / radial_count as f32;
    let radials = (0..radial_count)
        .map(|i| {
            let az = i as f32 * spacing;
            let bytes: Vec<u8> = (0..n_gates)
                .map(|j| {
                    let slant_km =
                        f64::from(FIRST_GATE_M) / 1000.0 + j as f64 * f64::from(GATE_M) / 1000.0;
                    match field(f64::from(az), slant_km) {
                        None => 0,
                        Some(dbz) => ((dbz * 2.0 + 66.0).round() as i64).clamp(2, 255) as u8,
                    }
                })
                .collect();
            Radial::new(
                0,
                i as u16,
                az,
                spacing,
                RadialStatus::IntermediateRadialData,
                elevation_number,
                elevation_deg,
                Some(MomentData::from_fixed_point(
                    bytes.len() as u16,
                    FIRST_GATE_M,
                    GATE_M,
                    8,
                    2.0,
                    66.0,
                    bytes,
                )),
                None,
                None,
                None,
                None,
                None,
                None,
            )
        })
        .collect();
    nexrad_model::data::Sweep::new(elevation_number, radials)
}

/// The smallest coverage pattern `VolumeSampler` accepts: two reflectivity
/// cuts, all other knobs at the fixture defaults `rustdar-radar`'s voxel
/// tests use.
fn two_tilt_vcp() -> nexrad_model::data::VolumeCoveragePattern {
    use nexrad_model::data::{
        ChannelConfiguration, ElevationCut, PulseWidth, VolumeCoveragePattern, WaveformType,
    };
    let cut = |angle_deg: f64| {
        ElevationCut::new(
            angle_deg,
            ChannelConfiguration::ConstantPhase,
            WaveformType::CS,
            20.0,
            true,
            true,
            false,
            false,
            1,
            20,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            false,
            0,
            false,
            0,
            false,
            false,
        )
    };
    VolumeCoveragePattern::new(
        212,
        0,
        0.5,
        PulseWidth::Short,
        false,
        0,
        false,
        0,
        false,
        false,
        0,
        false,
        false,
        vec![cut(0.5), cut(4.5)],
    )
}

#[test]
fn a_planted_storm_lands_on_the_floor_exactly_under_its_own_voxels() {
    // A 55 dBZ disc, radius 20 km, centred 80 km east / 120 km north of the
    // site — off-centre on both axes and off the diagonal, so every flip and
    // the site-centred control disagree with it.
    const DISC_KM: (f64, f64) = (80.0, 120.0);
    const DISC_RADIUS_KM: f64 = 20.0;
    let field = |az_deg: f64, slant_km: f64| -> Option<f64> {
        let az = az_deg.to_radians();
        let (dx, dy) = (slant_km * az.sin(), slant_km * az.cos());
        ((dx - DISC_KM.0).hypot(dy - DISC_KM.1) <= DISC_RADIUS_KM).then_some(55.0)
    };
    // 700 gates: data reach 2.125 + 700·0.25 ≈ 177 km. Deliberately not the
    // raster's 230 km half-extent — the discriminator described above.
    let scan = nexrad_model::data::Scan::new(
        two_tilt_vcp(),
        vec![
            refl_sweep(1, 0.53, 720, 700, &field),
            refl_sweep(2, 4.47, 360, 700, &field),
        ],
    );

    let site = rustdar_radar::sites::get_radar_site("KTLX").expect("KTLX is a known site");

    // Path one: the voxel build, at the app's own default request.
    let request = rustdar_radar::voxel::VoxelRequest {
        centre: (site.lat, site.lon),
        half_width_km: rustdar_egui::pane::DEFAULT_HALF_WIDTH_KM,
        base_km_msl: rustdar_radar::voxel::DEFAULT_BASE_KM_MSL,
        top_km_msl: rustdar_radar::voxel::DEFAULT_TOP_KM_MSL,
        product: RadarProduct::Reflectivity,
        shape: rustdar_radar::voxel::default_shape(),
        values_wanted: false,
    };
    let grid = rustdar_radar::voxel::build_voxels(&scan, &request, site.lat, site.lon)
        .expect("a buildable grid");

    // Path two: the real 2D rasterizer and the resample, as the floor job
    // runs them.
    let elevation =
        rustdar_radar::render::find_closest_elevation(&scan, RadarProduct::Reflectivity, 0.0)
            .expect("a reflectivity tilt");
    let input = rustdar_radar::render_input::RenderInput::extract(
        &scan,
        elevation,
        RadarProduct::Reflectivity,
        site.lat,
        site.lon,
        None,
        None,
    )
    .expect("a renderable base tilt");
    let (image, _data_reach_km, _) =
        rustdar_radar::render::render_from(&input).expect("a rendered base tilt");
    let floor = resample_floor(&image, site.lat, grid.x_range_km(), grid.y_range_km())
        .expect("a resampled floor");

    // Where each path put the disc, in kilometres east/north of the site.
    let (x0, x1) = grid.x_range_km();
    let (y0, y1) = grid.y_range_km();
    let shape = grid.shape();
    let cut = grid.value_to_index(30.0);
    let (mut gn, mut gx, mut gy) = (0usize, 0.0f64, 0.0f64);
    for iy in 0..shape.ny {
        for ix in 0..shape.nx {
            let hit = (0..shape.nz).any(|iz| grid.index_at(ix, iy, iz).unwrap() >= cut.max(1));
            if hit {
                let (cx, cy, _) = grid.cell_centre_km(ix, iy, 0).expect("an in-grid cell");
                gn += 1;
                gx += cx;
                gy += cy;
            }
        }
    }
    assert!(gn > 0, "the disc never reached the grid — a broken fixture");
    let grid_centroid = (gx / gn as f64, gy / gn as f64);

    let side = FLOOR_TEXELS as usize;
    let (mut fnum, mut fx, mut fy) = (0usize, 0.0f64, 0.0f64);
    for row in 0..side {
        for col in 0..side {
            let at = (row * side + col) * 4;
            let painted = floor.rgba[at..at + 3]
                .iter()
                .zip(&FLOOR_GROUND_RGBA[..3])
                .any(|(&c, &g)| c.abs_diff(g) > 6);
            if painted {
                fnum += 1;
                fx += x0 + (col as f64 + 0.5) / side as f64 * (x1 - x0);
                fy += y1 - (row as f64 + 0.5) / side as f64 * (y1 - y0);
            }
        }
    }
    assert!(
        fnum > 0,
        "the disc never reached the floor — a broken fixture"
    );
    let floor_centroid = (fx / fnum as f64, fy / fnum as f64);

    // The fixture sanity bound: each path found the disc where it was
    // planted. 6 km against a 20 km radius — half-cell effects, beam
    // geometry and palette edges all fit inside it; a flip, a zoom or an
    // origin error does not.
    for (name, (cx, cy)) in [("grid", grid_centroid), ("floor", floor_centroid)] {
        let err = (cx - DISC_KM.0).hypot(cy - DISC_KM.1);
        assert!(
            err < 6.0,
            "the {name} put the disc at ({cx:.1}, {cy:.1}) km, {err:.1} km from \
             where it was planted {DISC_KM:?}",
        );
    }
    // The alignment pin itself: the two paths agree with each other. Feeding
    // the render's data reach (177 km here) back in as the raster half-extent
    // moves the floor's disc ~33 km outward and fails here, exactly as the
    // 2026-08-09 screenshot's cores stood twice as far from the site as their
    // volume did.
    let dx = floor_centroid.0 - grid_centroid.0;
    let dy = floor_centroid.1 - grid_centroid.1;
    assert!(
        dx.hypot(dy) < 4.0,
        "floor and grid disagree by ({dx:.1}, {dy:.1}) km about where the same \
         disc stands",
    );
}
