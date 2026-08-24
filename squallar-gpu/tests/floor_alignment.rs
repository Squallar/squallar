//! The floor↔volume alignment instrument.
//!
//! The 3D view's floor is the 2D pane's own rendered output — the pane mirror —
//! and `volume.wgsl`'s `floor_colour` carries the ray's landing point on the
//! box's bottom face out to geography and back into the mirror's texture
//! coordinates. This file is a CPU model of those three lines ([`mirror_uv`]),
//! scored against `build_voxels`' grid on a common lattice over the box
//! footprint. The transform that best maps one mask onto the other is the
//! diagnosis: translation ≈ (0, 0) with identity beating every flip →
//! registered; a flip winning → a row-direction disagreement; a half-box offset
//! → an origin/sign disagreement; shapes aligned but IoU ≈ 0 → the two paths
//! read different data. Every deliberate break in [`Mapping`] is scored
//! alongside the true one, so a break that costs no IoU is a hole in the
//! instrument.
//!
//! The raster's grid convention, from `MercatorProjection::render_gate`:
//! columns are linear in longitude (`centre + Δλ · EARTH_RADIUS_KM · cos φ₀ ·
//! px_per_km`, `min_lon` at column 0), rows are linear in Web Mercator y
//! (`py = (mercator_y_max − mercator_y(φ)) · IMAGE_SIZE / (mercator_y_max −
//! mercator_y_min)`, row 0 at `max_lat`) — not in latitude, which is why
//! [`Mapping::LinearLatitudeV`] is a perturbation and not a simplification.
//!
//! `#[ignore]`d because it reads a volume from disk:
//!
//! ```text
//! VOL=/path/to/KDMX20250314_175512_V06 [THRESH=15] [OUT=/tmp/prefix] \
//! cargo test -p squallar-gpu --release --test floor_alignment -- --ignored --nocapture
//! ```
//!
//! `VOL` is required. `SITE` defaults to the volume's own identifier, `HALF_KM`
//! (or `HALF_E_KM`/`HALF_N_KM`) to the app's box, `THRESH` to 15 dBZ; `OUT` is a
//! prefix for `_floor.ppm`, `_grid.pgm`, `_overlay.ppm`.
#![cfg(not(target_arch = "wasm32"))]

use squallar_radar::types::{ImageBounds, RadarProduct};

// ── The volume, and the radar it places ──────────────────────────────────────

/// The volume reader and the site it learns, shared with the other two live
/// instruments in this directory. See `live_volume/mod.rs` for why the site
/// comes out of the volume rather than out of a lookup.
mod live_volume;
use live_volume::{scan_from_archive, site_of};

// ── The mirror, and the shader's own conversion into it ──────────────────────

/// Kilometres per degree of latitude: `ImageBounds`' conversion and the
/// shader's `KM_PER_DEGREE_LAT`, which are now one figure derived from
/// `EARTH_RADIUS_KM` — the same sphere `render_gate` walks north on. This
/// instrument imports it rather than copying it, so that a future divergence
/// shows up as a *measurement* here instead of being mirrored into the model
/// and cancelling itself out.
use squallar_geo::KM_PER_DEGREE_LAT;

/// Side of the lattice both masks are expressed on, in texels.
const PROBE_TEXELS: usize = 512;

/// The 3D texture limit these fixtures build their grids against.
const GRID_DEVICE_AXIS: usize = 256;

/// The background the PPM dump draws unpainted probe texels on: the deleted
/// `volume_floor.rs`'s `FLOOR_GROUND_RGBA`. It is a *dump* convention only —
/// the shipped floor has no ground colour, and `floor_colour` returns
/// transparent where the mirror has nothing.
const DUMP_GROUND_RGBA: [u8; 4] = [16, 18, 22, 255];

/// Alpha at or above which a mirror texel counts as painted. The raster leaves
/// unpainted pixels at `[0, 0, 0, 0]`, so any positive alpha is real ink; the
/// small threshold keeps palette edges that fade to nothing out of the mask,
/// the same role the old "differs from the ground colour by more than 6" cut
/// had. This is deliberately what the *shader* can see — a colour and an alpha
/// — and not the `f32` value grid `render_from` also returns, which would be a
/// sharper mask of something the floor does not have.
const PAINTED_ALPHA: u8 = 8;

/// Web Mercator's y: `ln(tan(π/4 + φ/2))`. The shader's `mercator_y`, and a
/// deliberate test-side mirror of `squallar_geo::lat_rad_to_mercator_y` — a
/// drift detector, not a convergence miss.
fn mercator_y(lat_rad: f64) -> f64 {
    (std::f64::consts::FRAC_PI_4 + lat_rad / 2.0).tan().ln()
}

/// The pane mirror as this instrument can have it: a raster, plus the four
/// numbers `VolumeUniform::floor_uv` carries — where the site sits in it and
/// how fast its texture coordinates run with geography.
struct Mirror {
    side: usize,
    /// Kilometres of ground across one texel: `2 · extent / side`.
    km_per_px: f64,
    rgba: Vec<u8>,
    site_lat_deg: f64,
    /// `floor_uv.x`
    u_at_site: f64,
    /// `floor_uv.y`
    v_at_site: f64,
    /// `floor_uv.z`
    u_per_degree_east: f64,
    /// `floor_uv.w`
    v_per_mercator_y: f64,
}

impl Mirror {
    /// Build the affine from `ImageBounds`, which is where the raster's own
    /// geography comes from — `render_from` projects through
    /// `MercatorProjection::from_bounds(lat, &ImageBounds::from_radar_site(..))`
    /// and `ui_map_pane` places the finished texture between the same bounds'
    /// north-west and south-east corners.
    fn from_pane_raster(
        rgba: Vec<u8>,
        side: usize,
        site_lat: f64,
        site_lon: f64,
        extent_km: f64,
    ) -> Self {
        let bounds = ImageBounds::from_radar_site(site_lat, site_lon, extent_km);
        let km_per_px = 2.0 * extent_km / side as f64;
        let lon_span = bounds.max_lon - bounds.min_lon;
        let merc_span = bounds.mercator_y_max - bounds.mercator_y_min;
        let site_merc = mercator_y(site_lat.to_radians());
        Mirror {
            side,
            km_per_px,
            rgba,
            site_lat_deg: site_lat,
            u_at_site: (site_lon - bounds.min_lon) / lon_span,
            v_at_site: (bounds.mercator_y_max - site_merc) / merc_span,
            u_per_degree_east: 1.0 / lon_span,
            v_per_mercator_y: -1.0 / merc_span,
        }
    }

    /// v per degree of latitude, taken at the site — the slope a
    /// linear-in-latitude v axis would run at if it were tangent to the true
    /// Mercator one where the site is. `d(mercator_y)/dφ = sec φ`, so this is
    /// the honest linearisation, which is what makes
    /// [`Mapping::LinearLatitudeV`] the *plausible* wrong answer rather than a
    /// straw man: it agrees with the truth at the site exactly and parts from
    /// it as the square of the distance north or south.
    fn v_per_degree_lat(&self) -> f64 {
        self.v_per_mercator_y / self.site_lat_deg.to_radians().cos() * std::f64::consts::PI / 180.0
    }

    /// The texel at `(u, v)`, nearest-neighbour, or `None` off the mirror —
    /// which `floor_colour` returns transparent for rather than clamping,
    /// because off-mirror is ground the source pane is not showing.
    fn sample(&self, uv: (f64, f64)) -> Option<[u8; 4]> {
        if !(0.0..=1.0).contains(&uv.0) || !(0.0..=1.0).contains(&uv.1) {
            return None;
        }
        let col = ((uv.0 * self.side as f64) as usize).min(self.side - 1);
        let row = ((uv.1 * self.side as f64) as usize).min(self.side - 1);
        let at = (row * self.side + col) * 4;
        Some([
            self.rgba[at],
            self.rgba[at + 1],
            self.rgba[at + 2],
            self.rgba[at + 3],
        ])
    }
}

/// The box's bottom face in the terms `floor_geo` and `box_size_km` carry it:
#[derive(Clone, Copy)]
struct BoxGeo {
    west_km: f64,
    south_km: f64,
    size_x_km: f64,
    size_y_km: f64,
}

impl BoxGeo {
    fn from_grid(grid: &squallar_radar::voxel::VolumeGrid) -> Self {
        let (x0, x1) = grid.x_range_km();
        let (y0, y1) = grid.y_range_km();
        BoxGeo {
            west_km: x0,
            south_km: y0,
            size_x_km: x1 - x0,
            size_y_km: y1 - y0,
        }
    }

    /// The `hit.xy` a ray landing `x_km` east and `y_km` north of the site
    /// would carry — the inverse of the first two lines of `floor_colour`,
    /// used by the pins below to ask the mapping about a named place.
    fn hit_at_km(&self, x_km: f64, y_km: f64) -> (f64, f64) {
        (
            (x_km - self.west_km) / self.size_x_km,
            (y_km - self.south_km) / self.size_y_km,
        )
    }
}

/// Which arithmetic [`mirror_uv`] runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mapping {
    /// The shader's own conversion.
    Honest,
    /// Drop the `cos φ` term: `d_lon = x_km / KM_PER_DEGREE_LAT`, as though a degree of
    /// longitude were a degree of latitude. Stretches the sampled ground
    /// east-west by `1 / cos φ` about the site's meridian — nothing at
    /// `x = 0`, tens of kilometres at the box's east and west edges.
    NoCosLat,
    /// The **equirectangular** mapping: `φ = φ₀ + y/K`, `Δλ = x/(K·cos φ)`.
    Equirectangular,
    /// Run v linear in latitude instead of in Mercator y, at the slope that
    /// makes the two agree at the site. Zero at the site, growing as the
    /// square of the distance north or south.
    LinearLatitudeV,
}

impl Mapping {
    /// Every mapping the instrument scores, the honest one first.
    const ALL: [Mapping; 4] = [
        Mapping::Honest,
        Mapping::NoCosLat,
        Mapping::Equirectangular,
        Mapping::LinearLatitudeV,
    ];

    fn label(self) -> &'static str {
        match self {
            Mapping::Honest => "honest (the shader)",
            Mapping::NoCosLat => "no cos(lat)",
            Mapping::Equirectangular => "equirectangular (the deleted mapping)",
            Mapping::LinearLatitudeV => "v linear in latitude",
        }
    }
}

/// `volume.wgsl`'s `floor_colour`, in Rust, up to the texture fetch.
fn mirror_uv(
    mirror: &Mirror,
    geo: &BoxGeo,
    hit: (f64, f64),
    mapping: Mapping,
) -> Option<(f64, f64)> {
    let x_km = geo.west_km + hit.0 * geo.size_x_km;
    let y_km = geo.south_km + hit.1 * geo.size_y_km;

    let site_lat_rad = mirror.site_lat_deg.to_radians();

    let (lat_deg, d_lon_deg) = match mapping {
        // The two equirectangular variants keep their own latitude, because
        // the latitude *is* part of what they get wrong.
        Mapping::NoCosLat | Mapping::Equirectangular => {
            let lat_deg = mirror.site_lat_deg + y_km / KM_PER_DEGREE_LAT;
            let cos_lat = match mapping {
                Mapping::NoCosLat => 1.0,
                _ => lat_deg.to_radians().cos(),
            };
            if cos_lat.abs() < 1e-6 {
                return None;
            }
            (lat_deg, x_km / (KM_PER_DEGREE_LAT * cos_lat))
        }
        _ => {
            // `squallar_geo::great_circle_destination` about the site, which is what
            // the box's kilometres mean. Called rather than restated: the
            // point of this instrument is to score the *shader's* arithmetic,
            // and the placement it has to agree with is the radar crate's.
            let range_km = x_km.hypot(y_km);
            let bearing_deg = x_km.atan2(y_km).to_degrees();
            let (lat, lon) = squallar_geo::great_circle_destination(
                mirror.site_lat_deg,
                0.0,
                bearing_deg,
                range_km,
            );
            (lat, lon)
        }
    };
    let lat_rad = lat_deg.to_radians();
    let u = mirror.u_at_site + d_lon_deg * mirror.u_per_degree_east;

    let v = match mapping {
        Mapping::LinearLatitudeV => {
            mirror.v_at_site + (lat_deg - mirror.site_lat_deg) * mirror.v_per_degree_lat()
        }
        _ => {
            let d_merc = mercator_y(lat_rad) - mercator_y(site_lat_rad);
            mirror.v_at_site + d_merc * mirror.v_per_mercator_y
        }
    };

    if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
        return None;
    }
    Some((u, v))
}

/// The mirror pixel a point `x_km` east / `y_km` north of the site maps to,
/// in fractional pixel coordinates — the mapping run forward and turned back
/// into the raster's own units, which is what the coincidence pins compare
/// against the raster's painted centroid.
fn mirror_pixel_for_km(
    mirror: &Mirror,
    geo: &BoxGeo,
    x_km: f64,
    y_km: f64,
    mapping: Mapping,
) -> Option<(f64, f64)> {
    let uv = mirror_uv(mirror, geo, geo.hit_at_km(x_km, y_km), mapping)?;
    Some((uv.0 * mirror.side as f64, uv.1 * mirror.side as f64))
}

// ── Masks ────────────────────────────────────────────────────────────────────

/// A binary mask over the probe lattice, row 0 the box footprint's north edge
/// — both sides of the comparison are expressed on this lattice.
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

/// A rectangle of the probe lattice to score inside.
#[derive(Clone, Copy)]
struct Region {
    label: &'static str,
    col0: usize,
    col1: usize,
    row0: usize,
    row1: usize,
}

impl Region {
    fn whole(side: usize) -> Self {
        Region {
            label: "whole box",
            col0: 0,
            col1: side,
            row0: 0,
            row1: side,
        }
    }

    /// The middle quarter of the side — ±⅛ of the box about its centre, so
    /// roughly ±57 km on the shipped 460 km box. Everything the projection can
    /// get wrong is nearly zero here.
    fn centre(side: usize) -> Self {
        Region {
            label: "centre ⅛",
            col0: side * 3 / 8,
            col1: side * 5 / 8,
            row0: side * 3 / 8,
            row1: side * 5 / 8,
        }
    }

    /// One far corner — the outer quarter of the box's side on each axis, so
    /// roughly 115..230 km out along both on the default box. Both the
    /// trapezoid error and the Mercator one are at their largest here. Whether
    /// the radar reaches all of it depends on the volume: a corner stands
    /// 325 km from the site, so a sweep that stops at 230 km cuts this square
    /// on the diagonal while a 458 km surveillance cut fills it.
    fn far_corner(side: usize, east: bool, north: bool) -> Self {
        let (col0, col1) = if east {
            (side * 3 / 4, side)
        } else {
            (0, side / 4)
        };
        // Row 0 is the footprint's north edge.
        let (row0, row1) = if north {
            (0, side / 4)
        } else {
            (side * 3 / 4, side)
        };
        Region {
            label: match (east, north) {
                (true, true) => "far NE",
                (true, false) => "far SE",
                (false, true) => "far NW",
                (false, false) => "far SW",
            },
            col0,
            col1,
            row0,
            row1,
        }
    }

    /// The far north-east corner. Named because the synthetic fixture's
    /// assertions live there — its field covers the whole box, so any corner
    /// would do, and one of them has to be written down.
    fn far_north_east(side: usize) -> Self {
        Self::far_corner(side, true, true)
    }
}

/// Intersection-over-union of `a` against `b` **transformed** and restricted
/// to `region`: texel `(c, r)` of `a` is compared with `b` at `(c', r')` where
/// each axis is optionally mirrored and then shifted.
fn iou_in(a: &Mask, b: &Mask, region: Region, flip: (bool, bool), dx: i64, dy: i64) -> f64 {
    let side = a.side as i64;
    let mut inter = 0usize;
    let mut union = 0usize;
    for row in region.row0 as i64..region.row1 as i64 {
        for col in region.col0 as i64..region.col1 as i64 {
            let av = a.at(col, row);
            let (mut bc, mut br) = (col, row);
            if flip.0 {
                bc = side - 1 - bc;
            }
            if flip.1 {
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

/// [`iou_in`] over the whole lattice.
fn iou(a: &Mask, b: &Mask, flip_x: bool, flip_y: bool, dx: i64, dy: i64) -> f64 {
    iou_in(a, b, Region::whole(a.side), (flip_x, flip_y), dx, dy)
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

// ── Building the two masks ───────────────────────────────────────────────────

/// The floor as the march would draw it: the mirror sampled through `mapping`
/// at the centre of every probe texel, with row 0 the footprint's north edge.
struct FloorSample {
    mask: Mask,
    /// The sampled colours, for the `OUT` dump. Unpainted texels get
    /// [`DUMP_GROUND_RGBA`] so the PPM is readable; nothing in the shipped
    /// path paints a ground colour.
    rgba: Vec<u8>,
}

fn sample_floor(mirror: &Mirror, geo: &BoxGeo, mapping: Mapping) -> FloorSample {
    let side = PROBE_TEXELS;
    let mut on = vec![false; side * side];
    let mut rgba = Vec::with_capacity(side * side * 4);
    for row in 0..side {
        let hit_y = 1.0 - (row as f64 + 0.5) / side as f64;
        for col in 0..side {
            let hit_x = (col as f64 + 0.5) / side as f64;
            let texel = mirror_uv(mirror, geo, (hit_x, hit_y), mapping)
                .and_then(|uv| mirror.sample(uv))
                .filter(|px| px[3] >= PAINTED_ALPHA);
            match texel {
                Some(px) => {
                    on[row * side + col] = true;
                    rgba.extend_from_slice(&px);
                }
                None => rgba.extend_from_slice(&DUMP_GROUND_RGBA),
            }
        }
    }
    FloorSample {
        mask: Mask { side, on },
        rgba,
    }
}

/// The grid's echo footprint on the same lattice: the column maximum of the
/// voxel grid, thresholded, nearest-sampled into probe texels.
fn sample_grid(grid: &squallar_radar::voxel::VolumeGrid, thresh: f32) -> Mask {
    let side = PROBE_TEXELS;
    let shape = grid.dims();
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
    let mut on = vec![false; side * side];
    for row in 0..side {
        let y_km = y1 - (row as f64 + 0.5) / side as f64 * (y1 - y0);
        let iy = (((y_km - y0) / (y1 - y0) * shape.ny as f64) as usize).min(shape.ny - 1);
        for col in 0..side {
            let x_km = x0 + (col as f64 + 0.5) / side as f64 * (x1 - x0);
            let ix = (((x_km - x0) / (x1 - x0) * shape.nx as f64) as usize).min(shape.nx - 1);
            on[row * side + col] = column_max[iy * shape.nx + ix] >= cut && cut > 0;
        }
    }
    Mask { side, on }
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

/// How many of `mask`'s texels are lit inside `region`. Printed beside the
/// table so an IoU of zero can be read as "the mapping is wrong here" or as
/// "no weather stood here" — on a real volume the second is common, and the
/// two are indistinguishable from the ratio alone.
fn count_in(mask: &Mask, region: Region) -> usize {
    let mut n = 0;
    for row in region.row0..region.row1 {
        for col in region.col0..region.col1 {
            if mask.on[row * mask.side + col] {
                n += 1;
            }
        }
    }
    n
}

/// The mapping × region table: one row per [`Mapping`], one column per
/// [`Region`]. The honest row is the measurement; the rest are the proof that
/// the measurement can move.
fn print_mapping_table(mirror: &Mirror, geo: &BoxGeo, grid_mask: &Mask, regions: &[Region]) {
    print!("{:<26}", "mapping");
    for region in regions {
        print!(" {:>10}", region.label);
    }
    println!("  {:>10}", "painted");
    print!("{:<26}", "grid texels in region");
    for region in regions {
        print!(" {:>10}", count_in(grid_mask, *region));
    }
    println!("  {:>10}", grid_mask.count());
    for mapping in Mapping::ALL {
        let floor = sample_floor(mirror, geo, mapping);
        print!("{:<26}", mapping.label());
        for region in regions {
            print!(
                " {:>10.4}",
                iou_in(grid_mask, &floor.mask, *region, (false, false), 0, 0)
            );
        }
        println!("  {:>10}", floor.mask.count());
    }
}

// ── The instrument ───────────────────────────────────────────────────────────

/// The box's half-extent from the environment, or `None` for the box
/// `build_voxels` sizes from the volume's own reach.
fn half_extent_from_env() -> Option<squallar_radar::voxel::HalfExtentKm> {
    let parsed = |name: &str| -> Option<f64> {
        std::env::var(name).ok().map(|raw| {
            raw.trim()
                .parse()
                .unwrap_or_else(|e| panic!("{name}={raw:?} does not parse: {e}"))
        })
    };
    let both = parsed("HALF_KM");
    match (parsed("HALF_E_KM").or(both), parsed("HALF_N_KM").or(both)) {
        (None, None) => None,
        (Some(east_km), Some(north_km)) => {
            Some(squallar_radar::voxel::HalfExtentKm { east_km, north_km })
        }
        _ => panic!(
            "set both HALF_E_KM and HALF_N_KM, or HALF_KM for a square, or \
             neither for the volume's own default box",
        ),
    }
}

#[test]
#[ignore = "reads a Level II volume from VOL; run with --ignored --nocapture"]
fn measure_floor_against_grid_on_a_real_volume() {
    let vol = std::path::PathBuf::from(std::env::var("VOL").expect("set VOL"));
    let half_extent = half_extent_from_env();
    let thresh: f32 = std::env::var("THRESH")
        .ok()
        .map(|s| s.parse().expect("THRESH must be a number"))
        .unwrap_or(15.0);

    // Decoded before the site is asked for, and that order is the point: the
    // volume states where its own radar is, so this instrument places the
    // radar it is about to measure instead of looking it up in a table nothing
    // filled. `install_radars` below is for the fixture tests only.
    let scan = scan_from_archive(&vol);
    let (icao, site_lat, site_lon) = site_of(&scan, &vol);

    // The grid, exactly as `handle_prepare_volume` requests the default box.
    let request = squallar_radar::voxel::VoxelRequest {
        centre: (site_lat, site_lon),
        half_extent_km: half_extent,
        base_km_msl: squallar_radar::voxel::DEFAULT_BASE_KM_MSL,
        top_km_msl: squallar_radar::voxel::DEFAULT_TOP_KM_MSL,
        product: RadarProduct::Reflectivity,
        shape: squallar_radar::voxel::default_shape(GRID_DEVICE_AXIS),
        values_wanted: false,
    };
    let grid = squallar_radar::voxel::build_voxels(&scan, &request, site_lat, site_lon)
        .expect("a buildable grid");

    // The mirror's stand-in: the 2D pane's own raster, rendered the way the
    // pane renders it. In the app this raster is one layer of the mirror, drawn
    // by egui into a frame-sized target; here it is the whole of it.
    let elevation =
        squallar_radar::render::find_closest_elevation(&scan, RadarProduct::Reflectivity, 0.0)
            .expect("a reflectivity tilt");
    let input = squallar_radar::render_input::RenderInput::extract(
        &scan,
        elevation,
        RadarProduct::Reflectivity,
        site_lat,
        site_lon,
        None,
        None,
    )
    .expect("a renderable base tilt");
    let (image, raster_side, extent_km) = pane_raster(&input).expect("a rendered base tilt");
    let mirror = Mirror::from_pane_raster(image, raster_side, site_lat, site_lon, extent_km);
    let geo = BoxGeo::from_grid(&grid);

    let side = PROBE_TEXELS;
    let floor = sample_floor(&mirror, &geo, Mapping::Honest);
    let grid_mask = sample_grid(&grid, thresh);

    // ── The numbers ──────────────────────────────────────────────────────
    let (x0, x1) = grid.x_range_km();
    let (y0, y1) = grid.y_range_km();
    let shape = grid.dims();
    // **Per axis.** The probe lattice is a fixed `PROBE_TEXELS`² over whatever
    // the box footprint is, so on a rectangular box a texel is not square on
    // the ground and one number cannot convert both lanes of a translation.
    let km_per_texel_x = (x1 - x0) / side as f64;
    let km_per_texel_y = (y1 - y0) / side as f64;
    println!("volume: {}", vol.display());
    // The position is the volume's own, not a table's — see `live_volume`.
    println!("site: {icao} at {site_lat:.5}, {site_lon:.5}, as this volume states it");
    println!(
        "box: x {:.1}..{:.1} km, y {:.1}..{:.1} km ({:.3} x {:.3} km/texel, \
         {:.4}:1 east:north), grid {}x{}x{}",
        x0,
        x1,
        y0,
        y1,
        km_per_texel_x,
        km_per_texel_y,
        (x1 - x0) / (y1 - y0),
        shape.nx,
        shape.ny,
        shape.nz,
    );
    println!(
        "mirror: {raster_side}x{raster_side} px, site at u {:.4} v {:.4}, \
         {:.2} u/deg east, {:.2} v/mercator-y",
        mirror.u_at_site, mirror.v_at_site, mirror.u_per_degree_east, mirror.v_per_mercator_y,
    );
    println!(
        "masks: floor {} texels painted, grid {} texels ≥ {thresh} dBZ (column max)",
        floor.mask.count(),
        grid_mask.count(),
    );
    let identity = iou(&grid_mask, &floor.mask, false, false, 0, 0);
    println!("IoU identity: {identity:.4}");
    println!(
        "IoU flip x:   {:.4}",
        iou(&grid_mask, &floor.mask, true, false, 0, 0)
    );
    println!(
        "IoU flip y:   {:.4}",
        iou(&grid_mask, &floor.mask, false, true, 0, 0)
    );
    println!(
        "IoU flip xy:  {:.4}",
        iou(&grid_mask, &floor.mask, true, true, 0, 0)
    );
    let ((dx, dy), at_best) = best_translation(&grid_mask, &floor.mask, 96, 4);
    println!(
        "best translation: ({dx}, {dy}) texels = ({:.2}, {:.2}) km east/south, IoU {at_best:.4}",
        dx as f64 * km_per_texel_x,
        dy as f64 * km_per_texel_y,
    );
    if let (Some(gc), Some(fc)) = (grid_mask.centroid(), floor.mask.centroid()) {
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

    // The instrument's own proof of life: every deliberately broken mapping,
    // scored whole, in a centred region and in each far corner. The honest row
    // must lead; the broken rows must fall furthest in whichever corner the
    // day's weather actually stood in, because that is where the errors they
    // introduce live. Corners with no grid texels in them score zero for every
    // mapping and mean nothing — the count row above is how to tell.
    println!();
    print_mapping_table(
        &mirror,
        &geo,
        &grid_mask,
        &[
            Region::whole(side),
            Region::centre(side),
            Region::far_corner(side, true, true),
            Region::far_corner(side, true, false),
            Region::far_corner(side, false, false),
            Region::far_corner(side, false, true),
        ],
    );

    if let Ok(prefix) = std::env::var("OUT") {
        write_ppm_rgba(&format!("{prefix}_floor.ppm"), side, &floor.rgba);
        write_pgm_mask(&format!("{prefix}_grid.pgm"), &grid_mask);
        write_overlay(&format!("{prefix}_overlay.ppm"), &grid_mask, &floor.mask);
        println!("wrote {prefix}_floor.ppm, {prefix}_grid.pgm, {prefix}_overlay.ppm");
    }
}

// ── Fixtures: synthetic sweeps through the real production paths ─────────────

/// One reflectivity sweep over `field(azimuth_deg, slant_km) -> Option<dBZ>`,
/// on the operational super-res gate layout (centre of gate 0 at 2.125 km,
/// 250 m gates — the same numbers `squallar-radar`'s own fixtures fly).
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
/// cuts, all other knobs at the fixture defaults `squallar-radar`'s voxel
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

/// Push a field through the real rasterizer and hand back the pane raster as a
/// [`Mirror`] — the same three calls the app's 2D pane makes, so nothing here
/// restates the raster's projection.
fn mirror_from_field(
    site_lat: f64,
    site_lon: f64,
    radial_count: usize,
    n_gates: usize,
    field: &dyn Fn(f64, f64) -> Option<f64>,
) -> Mirror {
    let scan = nexrad_model::data::Scan::new(
        two_tilt_vcp(),
        vec![
            refl_sweep(1, 0.53, radial_count, n_gates, field),
            refl_sweep(2, 4.47, radial_count.min(360), n_gates, field),
        ],
    );
    let elevation =
        squallar_radar::render::find_closest_elevation(&scan, RadarProduct::Reflectivity, 0.0)
            .expect("a reflectivity tilt");
    let input = squallar_radar::render_input::RenderInput::extract(
        &scan,
        elevation,
        RadarProduct::Reflectivity,
        site_lat,
        site_lon,
        None,
        None,
    )
    .expect("a renderable base tilt");
    let (image, side, extent_km) = pane_raster(&input).expect("a rendered base tilt");
    Mirror::from_pane_raster(image, side, site_lat, site_lon, extent_km)
}

/// The raster a **static desktop pane** would produce for `input`, its side and
/// its extent — the picture the shipped mirror is made of.
fn pane_raster(input: &squallar_radar::render_input::RenderInput) -> Option<(Vec<u8>, usize, f64)> {
    let squallar_radar::render::SweepRender {
        image,
        max_range_km: extent_km,
        ..
    } = squallar_radar::render::render_from_sized(
        input,
        squallar_device_profile::constants::LONG_RANGE_IMAGE_SIZE,
    )?;
    let side = (image.len() / 4).isqrt();
    assert_eq!(
        side * side * 4,
        image.len(),
        "a plan-view raster is square RGBA",
    );
    Some((image, side, extent_km))
}

/// The two radars **the fixture tests** in this file measure against, placed
/// once.
fn install_radars() {
    use squallar_radar::site_position::SitePosition;
    use squallar_radar::sites::SiteFix;

    /// `(ICAO, latitude, longitude, site_height_m, tower_height_m)`, the
    /// position and heights each radar's own Level II volume reports.
    const SITES: [(&str, i32, i32, i32, i32); 2] = [
        ("KMPX", 44_849_000, -93_566_000, 288, 30),
        ("KTLX", 35_333_060, -97_277_500, 370, 19),
    ];

    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        squallar_radar::sites::resolve(SITES.map(
            |(name, lat_udeg, lon_udeg, site_height_m, tower_height_m)| {
                (
                    name,
                    SiteFix::Learned(SitePosition {
                        lat_udeg,
                        lon_udeg,
                        site_height_m,
                        tower_height_m,
                    }),
                )
            },
        ));
    });
}

/// A fixed `±230 km` box about the site, as a [`BoxGeo`].
fn default_box() -> BoxGeo {
    let half = squallar_radar::voxel::BASE_HALF_WIDTH_KM;
    BoxGeo {
        west_km: -half,
        south_km: -half,
        size_x_km: 2.0 * half,
        size_y_km: 2.0 * half,
    }
}

/// Where a blob of echo planted `dx_km` east / `dy_km` north of the site
/// actually landed in the raster, as a fractional pixel — the renderer's own
/// forward projection, measured rather than restated.
fn beacon_pixel(site_lat: f64, site_lon: f64, dx_km: f64, dy_km: f64) -> (f64, f64) {
    // 5 km: several gates across at every range this is used at, so the blob
    // is a resolved shape whose centroid is stable, and small enough that
    // Mercator's own row compression across it is far under a pixel.
    const BLOB_KM: f64 = 5.0;
    let field = |az_deg: f64, slant_km: f64| -> Option<f64> {
        let az = az_deg.to_radians();
        let (x, y) = (slant_km * az.sin(), slant_km * az.cos());
        ((x - dx_km).hypot(y - dy_km) <= BLOB_KM).then_some(55.0)
    };
    // 940 gates reach 237 km, so the raster is projected at 237 km — just past
    // the floor, which keeps this fixture's geometry within 3 % of the ±230 km
    // frame it was calibrated on while still exercising an extent the sweep
    // chose. Every probe inside the radar's range is reachable and nothing is
    // computed that could never be drawn; a probe further out would find an
    // empty raster and trip the assertion below, which is the right failure for
    // asking about ground the radar cannot see.
    let mirror = mirror_from_field(site_lat, site_lon, 720, 940, &field);
    let side = mirror.side;
    let (mut n, mut sx, mut sy) = (0usize, 0.0f64, 0.0f64);
    for row in 0..side {
        for col in 0..side {
            if mirror.rgba[(row * side + col) * 4 + 3] >= PAINTED_ALPHA {
                n += 1;
                sx += col as f64 + 0.5;
                sy += row as f64 + 0.5;
            }
        }
    }
    assert!(
        n > 0,
        "the beacon at ({dx_km}, {dy_km}) km never reached the raster — a broken fixture",
    );
    (sx / n as f64, sy / n as f64)
}

// ── The pins ─────────────────────────────────────────────────────────────────

/// **Site-centred mapping**, re-pinned from the deleted
/// `volume_floor/tests.rs`'s `the_sites_pixel_lands_in_the_middle_of_a_site_
/// centred_floor`.
#[test]
fn the_boxs_site_position_lands_on_the_mirrors_site_pixel() {
    install_radars();
    let site = squallar_radar::sites::get_radar_site("KMPX").expect("KMPX is a known site");
    let geo = default_box();
    let drawn = beacon_pixel(site.lat, site.lon, 0.0, 0.0);
    let mirror = mirror_from_field(site.lat, site.lon, 720, 940, &|_, _| None);

    let mapped = mirror_pixel_for_km(&mirror, &geo, 0.0, 0.0, Mapping::Honest)
        .expect("the site is on the mirror");
    let apart = (mapped.0 - drawn.0).hypot(mapped.1 - drawn.1);
    println!(
        "site: mapped to ({:.2}, {:.2}) px, drawn at ({:.2}, {:.2}) px, {apart:.2} px apart; \
         raster middle {:.1}",
        mapped.0,
        mapped.1,
        drawn.0,
        drawn.1,
        mirror.side as f64 / 2.0,
    );
    assert!(
        apart < 3.0,
        "the box's site position mapped to mirror pixel ({:.1}, {:.1}); the raster \
         drew the site's own echo at ({:.1}, {:.1}), {apart:.1} px away",
        mapped.0,
        mapped.1,
        drawn.0,
        drawn.1,
    );

    // And the asymmetry the mapping is carrying: the site's row is off the
    // raster's middle by Mercator's own curvature over the frame's half-width.
    let middle = mirror.side as f64 / 2.0;
    assert!(
        (mapped.0 - middle).abs() < 1.0,
        "the site must sit on the raster's middle column, not at {:.1} of {middle}",
        mapped.0,
    );
    assert!(
        mapped.1 - middle > 2.0,
        "the site must sit below the raster's middle row — Mercator's rows are \
         denser to the south — but it mapped to row {:.1} of {middle}",
        mapped.1,
    );
}

/// **Gate/pixel coincidence**, re-pinned from the deleted
/// `volume_floor/tests.rs`'s `a_tile_pixel_and_a_radar_gate_at_the_same_ground_
/// land_on_the_same_texel`.
#[test]
fn a_gate_lands_on_the_mirror_pixel_that_renders_it() {
    const HONEST_BUDGET_KM: f64 = 0.9;
    const MUST_MISS_KM: f64 = 2.3;

    install_radars();
    let site = squallar_radar::sites::get_radar_site("KMPX").expect("KMPX is a known site");
    let geo = default_box();
    let mirror = mirror_from_field(site.lat, site.lon, 720, 940, &|_, _| None);

    let probes = [(150.0, 160.0), (60.0, 215.0), (-190.0, -100.0)];
    let mut worst_miss = [0.0f64; Mapping::ALL.len()];
    for (dx_km, dy_km) in probes {
        let drawn = beacon_pixel(site.lat, site.lon, dx_km, dy_km);
        for (slot, mapping) in worst_miss.iter_mut().zip(Mapping::ALL) {
            let mapped = mirror_pixel_for_km(&mirror, &geo, dx_km, dy_km, mapping)
                .unwrap_or_else(|| panic!("({dx_km}, {dy_km}) km fell off the mirror"));
            let apart = (mapped.0 - drawn.0).hypot(mapped.1 - drawn.1) * mirror.km_per_px;
            println!(
                "({dx_km:>6.0}, {dy_km:>6.0}) km  {:<26} {apart:>8.3} km",
                mapping.label(),
            );
            if mapping == Mapping::Honest {
                assert!(
                    apart < HONEST_BUDGET_KM,
                    "a gate at ({dx_km}, {dy_km}) km was drawn at raster pixel \
                     ({:.1}, {:.1}) and the mapping put it at ({:.1}, {:.1}) — \
                     {apart:.3} km apart, over the {HONEST_BUDGET_KM} km budget",
                    drawn.0,
                    drawn.1,
                    mapped.0,
                    mapped.1,
                );
            }
            *slot = slot.max(apart);
        }
    }

    // Every break must be caught by at least one probe. Without this the
    // paragraph above is a claim; with it, it is checked.
    for (miss, mapping) in worst_miss.iter().zip(Mapping::ALL) {
        if mapping == Mapping::Honest {
            continue;
        }
        assert!(
            *miss > MUST_MISS_KM,
            "{} — a mapping this file calls broken — landed within {miss:.3} km of \
             the drawn gate at every probe, so no probe here would notice it. The \
             probe set has gone blind, not the shader.",
            mapping.label(),
        );
    }
}

/// **A box whose two horizontal extents differ**, and the two mistakes that
/// are invisible until one does.
#[test]
fn a_rectangular_boxs_two_extents_each_stay_on_their_own_axis() {
    const HONEST_BUDGET_KM: f64 = 0.9;
    const MUST_MISS_KM: f64 = 50.0;

    install_radars();
    let site = squallar_radar::sites::get_radar_site("KMPX").expect("KMPX is a known site");
    let mirror = mirror_from_field(site.lat, site.lon, 720, 940, &|_, _| None);

    let honest = BoxGeo {
        west_km: -230.0,
        south_km: -115.0,
        size_x_km: 460.0,
        size_y_km: 230.0,
    };
    // The box the pane used to send, and the box a transposition would build.
    let squared = BoxGeo {
        south_km: -230.0,
        size_y_km: 460.0,
        ..honest
    };
    let swapped = BoxGeo {
        west_km: -115.0,
        south_km: -230.0,
        size_x_km: 230.0,
        size_y_km: 460.0,
    };

    let pixel_at = |geo: &BoxGeo, hit: (f64, f64)| -> Option<(f64, f64)> {
        let uv = mirror_uv(&mirror, geo, hit, Mapping::Honest)?;
        Some((uv.0 * mirror.side as f64, uv.1 * mirror.side as f64))
    };

    let mut worst_wrong = [0.0f64; 2];
    for (dx_km, dy_km) in [(150.0, 100.0), (60.0, 105.0), (-190.0, -60.0)] {
        let drawn = beacon_pixel(site.lat, site.lon, dx_km, dy_km);
        // The box position the shader would march to for this ground.
        let hit = honest.hit_at_km(dx_km, dy_km);
        assert!(
            (0.0..=1.0).contains(&hit.0) && (0.0..=1.0).contains(&hit.1),
            "({dx_km}, {dy_km}) km is outside the box this pin frames: {hit:?}",
        );

        let mapped = pixel_at(&honest, hit).expect("the probe is on the mirror");
        let apart = (mapped.0 - drawn.0).hypot(mapped.1 - drawn.1) * mirror.km_per_px;
        println!("({dx_km:>6.0}, {dy_km:>6.0}) km  hit {hit:?}  honest {apart:>8.3} km");
        assert!(
            apart < HONEST_BUDGET_KM,
            "a gate at ({dx_km}, {dy_km}) km was drawn at raster pixel \
             ({:.1}, {:.1}) and the rectangular box put it at ({:.1}, {:.1}) — \
             {apart:.3} km apart, over the {HONEST_BUDGET_KM} km budget",
            drawn.0,
            drawn.1,
            mapped.0,
            mapped.1,
        );

        for (slot, (wrong, label)) in worst_wrong.iter_mut().zip([
            (&squared, "east extent on both axes"),
            (&swapped, "the two extents exchanged"),
        ]) {
            let mapped = pixel_at(wrong, hit)
                .unwrap_or_else(|| panic!("{label} fell off the mirror at ({dx_km}, {dy_km})"));
            let apart = (mapped.0 - drawn.0).hypot(mapped.1 - drawn.1) * mirror.km_per_px;
            println!("({dx_km:>6.0}, {dy_km:>6.0}) km  {label:<26} {apart:>8.3} km");
            *slot = slot.max(apart);
        }
    }

    for (miss, label) in worst_wrong
        .iter()
        .zip(["east extent on both axes", "the two extents exchanged"])
    {
        assert!(
            *miss > MUST_MISS_KM,
            "{label} landed within {miss:.3} km of the drawn gate at every \
             probe, so no probe here would notice it. The probe set has gone \
             blind, not the mapping.",
        );
    }
}

// ── The pin: a synthetic storm, both production paths, no file, no GPU ───────

#[test]
fn a_planted_storm_lands_on_the_floor_exactly_under_its_own_voxels() {
    // A 55 dBZ disc, radius 20 km, centred 60 km east / 85 km north of the
    // site — off-centre on both axes and off the diagonal, so every flip and
    // the site-centred control disagree with it, by 120 km or more.
    const DISC_KM: (f64, f64) = (60.0, 85.0);
    const DISC_RADIUS_KM: f64 = 20.0;
    let field = |az_deg: f64, slant_km: f64| -> Option<f64> {
        let az = az_deg.to_radians();
        let (dx, dy) = (slant_km * az.sin(), slant_km * az.cos());
        ((dx - DISC_KM.0).hypot(dy - DISC_KM.1) <= DISC_RADIUS_KM).then_some(55.0)
    };
    // 700 gates: data reach 2.125 + 700·0.25 ≈ 177 km, inside the floor on
    // purpose, so the render's extent is 230 km and the two numbers differ
    // (see the note above).
    let scan = nexrad_model::data::Scan::new(
        two_tilt_vcp(),
        vec![
            refl_sweep(1, 0.53, 720, 700, &field),
            refl_sweep(2, 4.47, 360, 700, &field),
        ],
    );

    install_radars();
    let site = squallar_radar::sites::get_radar_site("KTLX").expect("KTLX is a known site");

    // Path one: the voxel build, at the app's own default request.
    let request = squallar_radar::voxel::VoxelRequest {
        centre: (site.lat, site.lon),
        half_extent_km: None,
        base_km_msl: squallar_radar::voxel::DEFAULT_BASE_KM_MSL,
        top_km_msl: squallar_radar::voxel::DEFAULT_TOP_KM_MSL,
        product: RadarProduct::Reflectivity,
        shape: squallar_radar::voxel::default_shape(GRID_DEVICE_AXIS),
        values_wanted: false,
    };
    let grid = squallar_radar::voxel::build_voxels(&scan, &request, site.lat, site.lon)
        .expect("a buildable grid");

    // Path two: the real 2D rasterizer, read through the shader's mapping as
    // the march reads the mirror.
    let elevation =
        squallar_radar::render::find_closest_elevation(&scan, RadarProduct::Reflectivity, 0.0)
            .expect("a reflectivity tilt");
    let input = squallar_radar::render_input::RenderInput::extract(
        &scan,
        elevation,
        RadarProduct::Reflectivity,
        site.lat,
        site.lon,
        None,
        None,
    )
    .expect("a renderable base tilt");
    let (image, side, extent_km) = pane_raster(&input).expect("a rendered base tilt");
    let mirror = Mirror::from_pane_raster(image, side, site.lat, site.lon, extent_km);
    let geo = BoxGeo::from_grid(&grid);
    let floor = sample_floor(&mirror, &geo, Mapping::Honest);

    // Where each path put the disc, in kilometres east/north of the site.
    let (x0, x1) = grid.x_range_km();
    let (y0, y1) = grid.y_range_km();
    let shape = grid.dims();
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

    let side = PROBE_TEXELS;
    let (mut fnum, mut fx, mut fy) = (0usize, 0.0f64, 0.0f64);
    for row in 0..side {
        for col in 0..side {
            if floor.mask.on[row * side + col] {
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
    // The alignment pin itself: the two paths agree with each other.
    let dx = floor_centroid.0 - grid_centroid.0;
    let dy = floor_centroid.1 - grid_centroid.1;
    assert!(
        dx.hypot(dy) < 4.0,
        "floor and grid disagree by ({dx:.1}, {dy:.1}) km about where the same \
         disc stands",
    );
}

// ── The pin that makes the instrument's numbers mean something ───────────────

/// Kilometres across one block of the perturbation fixture's field.
const BLOCK_KM: f64 = 8.0;

/// Whether the block at `(ix, iy)` is lit. A hash rather than a checkerboard,
/// because a checkerboard is periodic and a translation of exactly one period
/// would score as well as no translation at all — which is the one thing this
/// fixture must not do.
fn block_is_lit(ix: i64, iy: i64) -> bool {
    // splitmix64's finaliser over the two indices. The constants are the
    // published ones; nothing here depends on which hash it is, only that it
    // decorrelates neighbours.
    let mut h = (ix as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (iy as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 31;
    h & 1 == 0
}

/// The acceptance bar: **a broken mapping must cost IoU, and the errors a
/// centred score cannot see must cost it in the corner.**
#[test]
fn a_broken_mapping_costs_iou_in_the_corner_even_where_the_centre_cannot_tell() {
    install_radars();
    let site = squallar_radar::sites::get_radar_site("KMPX").expect("KMPX is a known site");
    let field = |az_deg: f64, slant_km: f64| -> Option<f64> {
        let az = az_deg.to_radians();
        let (x, y) = (slant_km * az.sin(), slant_km * az.cos());
        block_is_lit((x / BLOCK_KM).floor() as i64, (y / BLOCK_KM).floor() as i64).then_some(55.0)
    };
    // 940 gates reach 237 km: past the 230 km floor, so the raster is projected
    // at 237 and both it and the grid stop where the radar does rather than
    // either running out of fixture first.
    let scan = nexrad_model::data::Scan::new(
        two_tilt_vcp(),
        vec![
            refl_sweep(1, 0.53, 720, 940, &field),
            refl_sweep(2, 4.47, 360, 940, &field),
        ],
    );

    let request = squallar_radar::voxel::VoxelRequest {
        centre: (site.lat, site.lon),
        half_extent_km: None,
        base_km_msl: squallar_radar::voxel::DEFAULT_BASE_KM_MSL,
        top_km_msl: squallar_radar::voxel::DEFAULT_TOP_KM_MSL,
        product: RadarProduct::Reflectivity,
        shape: squallar_radar::voxel::default_shape(GRID_DEVICE_AXIS),
        values_wanted: false,
    };
    let grid = squallar_radar::voxel::build_voxels(&scan, &request, site.lat, site.lon)
        .expect("a buildable grid");
    let grid_mask = sample_grid(&grid, 15.0);

    let elevation =
        squallar_radar::render::find_closest_elevation(&scan, RadarProduct::Reflectivity, 0.0)
            .expect("a reflectivity tilt");
    let input = squallar_radar::render_input::RenderInput::extract(
        &scan,
        elevation,
        RadarProduct::Reflectivity,
        site.lat,
        site.lon,
        None,
        None,
    )
    .expect("a renderable base tilt");
    let (image, side, extent_km) = pane_raster(&input).expect("a rendered base tilt");
    let mirror = Mirror::from_pane_raster(image, side, site.lat, site.lon, extent_km);
    let geo = BoxGeo::from_grid(&grid);

    let whole = Region::whole(PROBE_TEXELS);
    let centre = Region::centre(PROBE_TEXELS);
    let corner = Region::far_north_east(PROBE_TEXELS);
    let score = |mapping: Mapping| {
        let floor = sample_floor(&mirror, &geo, mapping);
        [whole, centre, corner]
            .map(|region| iou_in(&grid_mask, &floor.mask, region, (false, false), 0, 0))
    };

    let honest = score(Mapping::Honest);
    println!("grid mask: {} texels", grid_mask.count());
    println!(
        "{:<26} {:>10} {:>10} {:>10}",
        "mapping", whole.label, centre.label, corner.label
    );
    println!(
        "{:<26} {:>10.4} {:>10.4} {:>10.4}",
        Mapping::Honest.label(),
        honest[0],
        honest[1],
        honest[2],
    );
    // The floor of the whole exercise: the honest mapping registers. Both
    // scored regions, because a corner score of zero would make every "the
    // corner fell" assertion below vacuous.
    assert!(
        honest[1] > 0.6,
        "the honest mapping scored {:.4} at the box centre — the fixture or the \
         mapping is broken before any perturbation is applied",
        honest[1],
    );
    assert!(
        honest[2] > 0.5,
        "the honest mapping scored {:.4} in the far NE corner — nothing below \
         can be read as a fall from that",
        honest[2],
    );

    let mut falls = Vec::new();
    for mapping in Mapping::ALL {
        if mapping == Mapping::Honest {
            continue;
        }
        let broken = score(mapping);
        let fall = [0, 1, 2].map(|i| honest[i] - broken[i]);
        println!(
            "{:<26} {:>10.4} {:>10.4} {:>10.4}   falls {:+.4} {:+.4} {:+.4}",
            mapping.label(),
            broken[0],
            broken[1],
            broken[2],
            fall[0],
            fall[1],
            fall[2],
        );
        // Proof of life: every break this file names must move the number in
        // the corner. Nothing weaker is asked of `NoCosLat`, whose damage is
        // first order and saturates IoU everywhere at once.
        assert!(
            fall[2] > 0.05,
            "{} cost only {:.4} of IoU in the far NE corner ({:.4} → {:.4}). A \
             mapping this file calls broken has to move the number, or the \
             number is not measuring the mapping.",
            mapping.label(),
            fall[2],
            honest[2],
            broken[2],
        );
        falls.push((mapping, fall));
    }

    // The centred-blindness argument itself. Both second-order errors are
    // exactly zero at the site and grow as the square of the distance from its
    // parallel, so a centred score barely moves for either — this asserts that
    // it barely moves, which is what makes the corner's fall the *only*
    // evidence that catches them, and hence what makes a centred-only
    // instrument demonstrably blind.
    for (mapping, fall) in falls
        .iter()
        .filter(|(m, _)| matches!(m, Mapping::Equirectangular | Mapping::LinearLatitudeV))
    {
        assert!(
            fall[1] < 0.05,
            "{} cost {:.4} at the box centre. It is a second-order error and is \
             supposed to be invisible there; if it is not, this test has stopped \
             demonstrating what a centred-only probe misses",
            mapping.label(),
            fall[1],
        );
        // 0.01 is the floor under the ratio: without it, a centre fall that
        // happened to land at zero would make any corner fall pass.
        assert!(
            fall[2] > 3.0 * fall[1].max(0.01),
            "{} cost {:.4} at the centre and {:.4} in the corner — not the \
             contrast the centred-only blindness argument rests on",
            mapping.label(),
            fall[1],
            fall[2],
        );
    }
}
