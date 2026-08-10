//! The floor↔volume alignment instrument.
//!
//! The 3D view's floor is no longer a picture built for it. It is the 2D map
//! pane's own already-rendered egui output — the **pane mirror** — and the
//! raymarch reaches into it per pixel: `volume.wgsl`'s `floor_colour` carries
//! the ray's landing point on the box's bottom face out to geography and back
//! into the mirror's texture coordinates. That single conversion is the whole
//! registration, and it is three lines of shader nobody can step through.
//!
//! This file is a **CPU model of those three lines**, run against the same
//! raster the mirror is made of, so the conversion can be scored without a
//! headless GPU:
//!
//!   * `rustdar_radar::voxel::build_voxels`, the grid the raymarch draws, and
//!   * `render_from`'s raster read through the shader's mapping, the ground
//!     that grid stands on —
//!
//! measured against one another on a common lattice over the box footprint.
//! The echo footprint of the grid's columns and the sampled mirror must sit on
//! top of one another; the transform that best maps one onto the other **is
//! the diagnosis**:
//!
//!   * best translation ≈ (0, 0) and identity beating every flip → registered;
//!   * a flip winning → a row-direction disagreement;
//!   * a half-box offset → an origin/sign disagreement;
//!   * shapes aligned but IoU near zero → the two paths read different data.
//!
//! # Why the mapping is modelled rather than called
//!
//! `floor_colour` is WGSL. The lanes it reads (`floor_uv`, `floor_geo`) are
//! built on the CPU, but the arithmetic between them and a texture coordinate
//! only exists in the shader, and running the shader means a device, a
//! swapchain-format decision and a readback — none of which a `cargo test` row
//! has. So [`mirror_uv`] restates `floor_colour`'s conversion in Rust, and the
//! restatement is made honest by the perturbations: every deliberate break of
//! the mapping listed in [`Mapping`] is scored alongside the true one, and a
//! break that does not cost IoU is a hole in the instrument, not a harmless
//! variant. See [`Mapping`] and [`Region`].
//!
//! # Why the raster stands in for the mirror
//!
//! The shipped mirror is the whole 2D pane: tiles, the radar raster, alerts,
//! labels, the lot, drawn a second time into an offscreen target. Everything
//! in it except the radar raster is egui geometry that only exists inside a
//! running frame, so a test cannot have it. What a test *can* have is the one
//! layer whose geography is a pure function of the site — the raster
//! `render_from` produces — and that layer is enough, because the mapping
//! being scored does not know or care which layers painted the pixel it lands
//! on.
//!
//! The raster's own grid convention, read out of `rustdar_radar::render`'s
//! `MercatorProjection::render_gate` (and matching `ui_map_pane`, which places
//! the texture at `ImageBounds`' north-west and south-east corners on the
//! walkers map):
//!
//!   * **columns are linear in longitude**, `min_lon` at column 0 and
//!     `max_lon` at the right edge — `render_gate` writes
//!     `centre + dx_km · (cos φ₀ / cos φ) · PIXELS_PER_KM`, and that
//!     `cos φ₀ / cos φ` factor is exactly what turns kilometres east into
//!     degrees of longitude and back;
//!   * **rows are linear in Web Mercator y**, not in latitude —
//!     `py = (mercator_y_max − mercator_y(φ)) · IMAGE_SIZE / (mercator_y_max −
//!     mercator_y_min)`, row 0 at `max_lat`.
//!
//! Which is why `floor_colour`'s v axis runs with Mercator y and its u axis
//! with longitude, and why [`Mapping::LinearLatitudeV`] is a perturbation and
//! not a simplification.
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
//!
//! # The standing measurement
//!
//! `KDMX20250314_175512_V06`, default box, `THRESH=15`:
//!
//! | path | IoU identity | best translation |
//! |---|---|---|
//! | the deleted `resample_floor` floor | 0.5815 | (0, 0) texels |
//! | the mirror read through `floor_colour` | 0.5777 | (0, 0) texels |
//!
//! The two are the same measurement to within the mask criterion — the old one
//! asked "does this texel differ from the ground colour", this one asks "did
//! the mirror paint here" — and the registration verdict is identical: no
//! translation improves on zero, every flip is crushed (0.14, 0.0005, 0.0003).
//! The reprojection is now *exact* where `resample_floor` was a scale and a
//! translate, so the small difference is not a regression in registration; the
//! places the old approximation was wrong (the box's corners, by 7.6 km across
//! and 3.7 km down) are outside this volume's echo, which stood in the south-
//! west and inside 230 km.
//!
//! # What this file inherited from the deleted `volume_floor/tests.rs`
//!
//! The CPU floor compositor and its unit tests went with the old design. Two
//! of the geometry contracts they pinned survive the move and are re-pinned
//! here against the mirror path:
//!
//!   * **site-centred mapping** — the box's own site position must land on the
//!     pixel the raster drew the site's own echo at
//!     ([`the_boxs_site_position_lands_on_the_mirrors_site_pixel`]);
//!   * **gate/pixel coincidence** — a gate at a known range and azimuth must
//!     land on the mirror pixel that renders it
//!     ([`a_gate_lands_on_the_mirror_pixel_that_renders_it`]).
//!
//! One did not survive, and is deliberately not faked: **layer stack order**.
//! `compose_floor` used to paint ground, basemap, radar and labels itself, in
//! that order, and a test could check that a label tile covered an echo. The
//! mirror has no compositor — the stacking is egui's own painting order inside
//! the source pane, established by the 2D pane's draw calls and reproduced by
//! replaying that pane's geometry. There is nothing in this crate left to
//! assert it against, and a test that rebuilt a stack here would be checking
//! its own fixture. It belongs to `rustdar-egui`'s pane, or to nothing.
#![cfg(not(target_arch = "wasm32"))]

use rustdar_radar::types::{ImageBounds, RadarProduct};

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

// ── The mirror, and the shader's own conversion into it ──────────────────────

/// Kilometres per degree of latitude. `ImageBounds`' constant and the shader's
/// `KM_PER_DEGREE_LAT`; both spell it out rather than deriving it from an
/// Earth radius, so this does too. (It implies a 6378 km sphere, where
/// `render_gate` walks north on `EARTH_RADIUS_KM = 6371` — a 0.12 % disagreement
/// that the pins below budget for explicitly rather than paper over.)
const KM_PER_DEGREE_LAT: f64 = 111.32;

/// Side of the lattice both masks are expressed on, in texels.
///
/// Nothing in the shipped path has a floor lattice any more — the mirror is
/// frame-sized and the march samples it per pixel. 512 is the deleted
/// `volume_floor.rs`'s `FLOOR_TEXELS`, kept so this instrument's numbers stay
/// comparable with the ones the old path was measured at.
const PROBE_TEXELS: usize = 512;

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

/// Web Mercator's y: `ln(tan(π/4 + φ/2))`. The shader's `mercator_y`, and
/// `rustdar_radar::types::lat_rad_to_mercator_y`, which is private.
fn mercator_y(lat_rad: f64) -> f64 {
    (std::f64::consts::FRAC_PI_4 + lat_rad / 2.0).tan().ln()
}

/// The pane mirror as this instrument can have it: a raster, plus the four
/// numbers `VolumeUniform::floor_uv` carries — where the site sits in it and
/// how fast its texture coordinates run with geography.
struct Mirror {
    side: usize,
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
    ///
    /// u is linear in longitude and v in Mercator y, both anchored at the site
    /// the way the uniform anchors them. `v_per_mercator_y` is **negative**:
    /// Mercator y grows north and rows grow south.
    fn from_pane_raster(rgba: Vec<u8>, side: usize, site_lat: f64, site_lon: f64) -> Self {
        let bounds = ImageBounds::from_radar_site(site_lat, site_lon);
        let lon_span = bounds.max_lon - bounds.min_lon;
        let merc_span = bounds.mercator_y_max - bounds.mercator_y_min;
        let site_merc = mercator_y(site_lat.to_radians());
        Mirror {
            side,
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
/// its west and south edges as kilometres east and north **of the site**, and
/// its extent. Position and extent are separate because the uniform keeps them
/// separate.
#[derive(Clone, Copy)]
struct BoxGeo {
    west_km: f64,
    south_km: f64,
    size_x_km: f64,
    size_y_km: f64,
}

impl BoxGeo {
    fn from_grid(grid: &rustdar_radar::voxel::VoxelGrid) -> Self {
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
///
/// [`Mapping::Honest`] is `floor_colour`, line for line. The rest are the
/// mistakes that mapping is one edit away from, kept as first-class variants so
/// the instrument can be shown to *fail* — a scoring rig nobody has watched go
/// red is a number, not a check. Each is scored beside the honest one, and
/// each one's damage is concentrated somewhere different, which is the reason
/// [`Region`] exists.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mapping {
    /// The shader's own conversion.
    Honest,
    /// Drop the `cos φ` term: `d_lon = x_km / 111.32`, as though a degree of
    /// longitude were a degree of latitude. Stretches the sampled ground
    /// east-west by `1 / cos φ` about the site's meridian — nothing at
    /// `x = 0`, tens of kilometres at the box's east and west edges.
    NoCosLat,
    /// Take `cos φ` at the **site** instead of at the pixel. This is the
    /// trapezoid error: the box's footprint really is wider along its north
    /// edge than its south, and a site-latitude cosine collapses it to a
    /// rectangle. Exactly zero on the site's own parallel and at `x = 0`,
    /// a few kilometres at the box's far corners — the error a centred score
    /// cannot see.
    CosAtSite,
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
        Mapping::CosAtSite,
        Mapping::LinearLatitudeV,
    ];

    fn label(self) -> &'static str {
        match self {
            Mapping::Honest => "honest (the shader)",
            Mapping::NoCosLat => "no cos(lat)",
            Mapping::CosAtSite => "cos at site (trapezoid)",
            Mapping::LinearLatitudeV => "v linear in latitude",
        }
    }
}

/// `volume.wgsl`'s `floor_colour`, in Rust, up to the texture fetch.
///
/// ```text
/// x_km = floor_geo.y + hit.x · box_size_km.x
/// y_km = floor_geo.z + hit.y · box_size_km.y
/// φ    = φ₀ + y_km / 111.32
/// Δλ   = x_km / (111.32 · cos φ)          ← cos at THIS point's latitude
/// u    = floor_uv.x + Δλ · floor_uv.z
/// v    = floor_uv.y + (mercᵧ(φ) − mercᵧ(φ₀)) · floor_uv.w
/// ```
///
/// `None` where the shader returns transparent: off the mirror, and at a
/// latitude whose cosine has collapsed.
fn mirror_uv(
    mirror: &Mirror,
    geo: &BoxGeo,
    hit: (f64, f64),
    mapping: Mapping,
) -> Option<(f64, f64)> {
    let x_km = geo.west_km + hit.0 * geo.size_x_km;
    let y_km = geo.south_km + hit.1 * geo.size_y_km;

    let site_lat_rad = mirror.site_lat_deg.to_radians();
    let lat_deg = mirror.site_lat_deg + y_km / KM_PER_DEGREE_LAT;
    let lat_rad = lat_deg.to_radians();

    let cos_lat = match mapping {
        Mapping::NoCosLat => 1.0,
        Mapping::CosAtSite => site_lat_rad.cos(),
        _ => lat_rad.cos(),
    };
    if cos_lat.abs() < 1e-6 {
        return None;
    }
    let d_lon_deg = x_km / (KM_PER_DEGREE_LAT * cos_lat);
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
///
/// The instrument's predecessor scored one centred box and nothing else, and
/// that is precisely where the projection errors it was built to catch are
/// smallest: `cos φ` is symmetric about the site's parallel and its error
/// vanishes on the box's own centre lines, so a centred-only score is blind to
/// the trapezoid. Scoring a corner as well is the fix, and the *contrast*
/// between the two — reported side by side for every [`Mapping`] — is what
/// makes the number mean something.
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

    /// One far corner — the outer quarter of the side on each axis, so roughly
    /// 115..230 km out along both. Both the trapezoid error and the Mercator
    /// one are at their largest here, and the radar still reaches part of it
    /// (the raster stops at `MAX_RANGE_KM`, which cuts this square on the
    /// diagonal).
    ///
    /// All four are worth scoring on a real volume, because a real volume's
    /// echo is wherever the weather was: the 2025-03-14 KDMX case this
    /// instrument was calibrated on has its storms in the **south-west**, and a
    /// north-east-only corner probe would have scored an empty square and
    /// reported a confident zero.
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
///
/// `hit.y` runs **north** — `floor_colour` adds it to the box's *south* edge —
/// so the row-to-`hit.y` line is where a v flip would live, and it is written
/// once, here.
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
fn sample_grid(grid: &rustdar_radar::voxel::VoxelGrid, thresh: f32) -> Mask {
    let side = PROBE_TEXELS;
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

    // The mirror's stand-in: the 2D pane's own raster, rendered the way the
    // pane renders it. In the app this raster is one layer of the mirror, drawn
    // by egui into a frame-sized target; here it is the whole of it.
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
    let raster_side = rustdar_radar::types::IMAGE_SIZE;
    let mirror = Mirror::from_pane_raster(image, raster_side, site.lat, site.lon);
    let geo = BoxGeo::from_grid(&grid);

    let side = PROBE_TEXELS;
    let floor = sample_floor(&mirror, &geo, Mapping::Honest);
    let grid_mask = sample_grid(&grid, thresh);

    // ── The numbers ──────────────────────────────────────────────────────
    let (x0, x1) = grid.x_range_km();
    let (y0, y1) = grid.y_range_km();
    let shape = grid.shape();
    let km_per_texel = (x1 - x0) / side as f64;
    println!("volume: {}", vol.display());
    println!(
        "box: x {:.1}..{:.1} km, y {:.1}..{:.1} km ({:.3} km/texel), grid {}x{}x{}",
        x0, x1, y0, y1, km_per_texel, shape.nx, shape.ny, shape.nz,
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
        dx as f64 * km_per_texel,
        dy as f64 * km_per_texel,
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
    //
    // Read the corner columns with care on a real volume. IoU inside a
    // sub-region is only a fair comparison when both masks fill it comparably:
    // where the grid's echo saturates a corner and the floor's does not,
    // `no cos(lat)` — which stretches the sampled ground outward by `1/cos φ`,
    // 1.34× at KDMX — drags *more* echo into the square and can score **above**
    // the honest mapping there while losing badly over the whole box. Measured
    // on KDMX 2025-03-14: whole box 0.5777 honest against 0.4989 broken, far SW
    // corner 0.5009 honest against 0.6326 broken. That is a property of scoring
    // a lopsided sub-region, not a defect in the mapping, and it is why the
    // discrimination is *asserted* against the synthetic fixture in
    // `a_broken_mapping_costs_iou_in_the_corner_even_where_the_centre_cannot_tell`,
    // whose field fills the box evenly, and only *reported* here.
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
        rustdar_radar::render::find_closest_elevation(&scan, RadarProduct::Reflectivity, 0.0)
            .expect("a reflectivity tilt");
    let input = rustdar_radar::render_input::RenderInput::extract(
        &scan,
        elevation,
        RadarProduct::Reflectivity,
        site_lat,
        site_lon,
        None,
        None,
    )
    .expect("a renderable base tilt");
    let (image, _reach, _values) =
        rustdar_radar::render::render_from(&input).expect("a rendered base tilt");
    Mirror::from_pane_raster(image, rustdar_radar::types::IMAGE_SIZE, site_lat, site_lon)
}

/// The default box, as a [`BoxGeo`] — `±DEFAULT_HALF_WIDTH_KM` about the site,
/// which is what `build_voxels` produces for the app's own request.
fn default_box() -> BoxGeo {
    let half = rustdar_egui::pane::DEFAULT_HALF_WIDTH_KM;
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
    // 940 gates reach 237 km — just past `MAX_RANGE_KM`, where the rasterizer
    // stops anyway, so every probe inside the radar's range is reachable and
    // nothing is computed that could never be drawn. A probe further out than
    // that would find an empty raster and trip the assertion below, which is
    // the right failure for asking about ground the radar cannot see.
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
///
/// The old contract was that the site's echo landed at the *centre texel* of a
/// site-centred floor. There is no floor texture any more, so the contract
/// moves with the mapping: the box's own site position — `hit` = (0.5, 0.5) on
/// a `±half` box — must map to the mirror pixel the raster drew the site's own
/// echo at.
///
/// That pixel is **not** the raster's centre, and saying so is the point. The
/// raster's columns are linear in longitude and symmetric, so the site is on
/// the middle column; its rows are linear in Mercator y between `min_lat` and
/// `max_lat`, and Mercator is not linear in latitude, so the site sits a few
/// pixels **below** the middle row. A mapping that assumed the site was at
/// v = 0.5 would pass a centre-of-image check and fail this one. KMPX at
/// 44.8°N is the site because that offset grows with latitude — about 18 px of
/// 2048 there — and this pin wants it plainly non-zero.
#[test]
fn the_boxs_site_position_lands_on_the_mirrors_site_pixel() {
    let site = rustdar_radar::sites::get_radar_site("KMPX").expect("KMPX is a known site");
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
    // raster's middle by Mercator's own curvature over 230 km. If this ever
    // reads zero the raster has stopped being a Mercator picture and the v
    // axis of the mapping is no longer the right shape for it.
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
///
/// The old contract had two independent forward routes to the same ground — a
/// radar gate and a slippy tile — and asserted they met on one floor texel.
/// The tile route is gone with the compositor; what remains, and is the thing
/// the shader can actually get wrong, is the **inverse**: a gate planted at a
/// known range and azimuth must be found again by running the mapping from the
/// box position that names the same ground. The rasterizer is the oracle and
/// the mapping is under test, which is the right way round.
///
/// Three probes, because the plausible wrong answers die at *different* ones
/// and each leaves the others green — the same reason the deleted test carried
/// a corner probe:
///
///   * `(150, 160)` — well east and well north, where taking `cos φ` at the
///     site instead of at the point costs 16 px and a latitude-linear v axis 11;
///   * `(60, 215)` — nearly due north, where the `cos` errors nearly vanish
///     (8 px) and the v axis is at its worst (18 px);
///   * `(-190, -100)` — the opposite quadrant, which catches a sign as well as
///     a scale, and where the v error is down to 3 px.
///
/// Probes at KMPX, 44.8°N, for the reason
/// [`a_broken_mapping_costs_iou_in_the_corner_even_where_the_centre_cannot_tell`]
/// gives: both second-order errors scale with `tan φ₀`, and this pin wants them
/// clear of the honest mapping's own budget rather than merely above it.
///
/// That budget is 4 px, against a measured worst probe of 2.3. Most of it is
/// one known, deliberate disagreement: `render_gate` walks north on
/// `EARTH_RADIUS_KM` (6371 km) and `floor_colour` on `KM_PER_DEGREE_LAT`
/// (111.32 km/°, a 6378 km sphere), which is 0.12 % — about a pixel at 200 km.
/// The rest is the blob's own discretisation.
#[test]
fn a_gate_lands_on_the_mirror_pixel_that_renders_it() {
    const HONEST_BUDGET_PX: f64 = 4.0;
    const MUST_MISS_PX: f64 = 10.0;

    let site = rustdar_radar::sites::get_radar_site("KMPX").expect("KMPX is a known site");
    let geo = default_box();
    let mirror = mirror_from_field(site.lat, site.lon, 720, 940, &|_, _| None);

    let probes = [(150.0, 160.0), (60.0, 215.0), (-190.0, -100.0)];
    let mut worst_miss = [0.0f64; Mapping::ALL.len()];
    for (dx_km, dy_km) in probes {
        let drawn = beacon_pixel(site.lat, site.lon, dx_km, dy_km);
        for (slot, mapping) in worst_miss.iter_mut().zip(Mapping::ALL) {
            let mapped = mirror_pixel_for_km(&mirror, &geo, dx_km, dy_km, mapping)
                .unwrap_or_else(|| panic!("({dx_km}, {dy_km}) km fell off the mirror"));
            let apart = (mapped.0 - drawn.0).hypot(mapped.1 - drawn.1);
            println!(
                "({dx_km:>6.0}, {dy_km:>6.0}) km  {:<26} {apart:>8.2} px",
                mapping.label(),
            );
            if mapping == Mapping::Honest {
                assert!(
                    apart < HONEST_BUDGET_PX,
                    "a gate at ({dx_km}, {dy_km}) km was drawn at raster pixel \
                     ({:.1}, {:.1}) and the mapping put it at ({:.1}, {:.1}) — \
                     {apart:.1} px apart, over the {HONEST_BUDGET_PX} px budget",
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
            *miss > MUST_MISS_PX,
            "{} — a mapping this file calls broken — landed within {miss:.1} px of \
             the drawn gate at every probe, so no probe here would notice it. The \
             probe set has gone blind, not the shader.",
            mapping.label(),
        );
    }
}

// ── The pin: a synthetic storm, both production paths, no file, no GPU ───────
//
// The instrument above needs a volume on disk; this is the same comparison as
// a test the gauntlet runs every time. A 55 dBZ disc is planted at a known
// offset from the site and pushed through **both** production paths — the
// voxel build the raymarch draws, and the real 2D rasterizer read through the
// shader's mapping. Neither expectation restates a projection formula: the
// oracle is the planted disc's own position, and the assertion is that the two
// paths put it in the same place.
//
// What it closes:
//
//  * coordinated drift between `floor_colour` and `MercatorProjection` — the
//    raster here comes from the real renderer, not from a restated formula, so
//    a change to how the rasterizer projects moves this whether or not the
//    mapping moved with it;
//  * axis flips, which mirror the off-centre, off-diagonal disc across the box
//    and miss by hundreds of kilometres;
//  * the historical 2026-08-09 2× floor zoom, in the only form it can still
//    take. That bug was the raster's *data reach* fed to the old resampler as
//    its half-extent; the mirror has no half-extent to confuse, because its
//    geography comes from `ImageBounds::from_radar_site` and nothing else. The
//    fixture keeps its short low tilt (700 gates, 177 km, deliberately not the
//    raster's 230 km bounds) anyway: it costs nothing, and it means the pin is
//    still standing over the ground where that bug lived.

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
    // 700 gates: data reach 2.125 + 700·0.25 ≈ 177 km, short of the raster's
    // 230 km bounds on purpose (see the note above).
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

    // Path two: the real 2D rasterizer, read through the shader's mapping as
    // the march reads the mirror.
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
    let mirror =
        Mirror::from_pane_raster(image, rustdar_radar::types::IMAGE_SIZE, site.lat, site.lon);
    let geo = BoxGeo::from_grid(&grid);
    let floor = sample_floor(&mirror, &geo, Mapping::Honest);

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
///
/// 8 km is a compromise with two hard edges: the voxel grid's own cells are
/// 460/256 ≈ 1.8 km, so a block has to be several cells across to survive the
/// build at all; and every block edge is where a misregistered mapping shows
/// up, so bigger blocks mean a blunter instrument. Eight is about four and a
/// half cells and nine probe texels — measured, it roughly doubles what the
/// smallest perturbation costs against a 16 km block while leaving the honest
/// mapping's own score comfortably above the bounds below.
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
///
/// The predecessor of this file scored one centred box and nothing else. Its
/// author left the warning verbatim: *"the instrument as it stands scores a
/// single centred box where the `cos φ` term is near-symmetric, so it would NOT
/// have caught the trapezoid error. A centred-only probe is the fixture-
/// blindness failure this codebase keeps finding."*
///
/// So this test takes a field with structure everywhere, runs it through both
/// production paths, and scores every [`Mapping`] three times — whole box,
/// centre eighth, far north-east corner. What the measurements say:
///
///   * [`Mapping::NoCosLat`] is **first order** in `x_km`: it stretches the
///     sampled ground by `1/cos φ` about the site's meridian, which is tens of
///     kilometres at the box edge and several at the centre eighth's own edge.
///     It is fatal everywhere, corner included, and needs no corner to catch.
///     It also *saturates* — IoU has a floor — so its corner and centre falls
///     are of similar size and this test does not ask them to be ordered.
///   * [`Mapping::CosAtSite`] (the trapezoid) and [`Mapping::LinearLatitudeV`]
///     are **second order**: both are exactly right at the site and grow with
///     the square of the distance from its parallel. These are the errors the
///     warning is about. Measured on this fixture they cost 0.001 and 0.009 of
///     IoU at the centre — noise — and 0.12 and 0.09 in the corner. A
///     centred-only instrument would have called both of them clean.
///
/// The site is **KMPX**, at 44.8°N, and not the KTLX the other fixtures fly:
/// both second-order errors scale with `tan φ₀`, so a northern site is where
/// this fixture has the most to say. The mapping is not site-specific and
/// nothing here depends on which site it is beyond that.
#[test]
fn a_broken_mapping_costs_iou_in_the_corner_even_where_the_centre_cannot_tell() {
    let site = rustdar_radar::sites::get_radar_site("KMPX").expect("KMPX is a known site");
    let field = |az_deg: f64, slant_km: f64| -> Option<f64> {
        let az = az_deg.to_radians();
        let (x, y) = (slant_km * az.sin(), slant_km * az.cos());
        block_is_lit((x / BLOCK_KM).floor() as i64, (y / BLOCK_KM).floor() as i64).then_some(55.0)
    };
    // 940 gates reach 237 km: past `MAX_RANGE_KM`, so the raster and the grid
    // both stop where the radar does and neither runs out of fixture first.
    let scan = nexrad_model::data::Scan::new(
        two_tilt_vcp(),
        vec![
            refl_sweep(1, 0.53, 720, 940, &field),
            refl_sweep(2, 4.47, 360, 940, &field),
        ],
    );

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
    let grid_mask = sample_grid(&grid, 15.0);

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
    let (image, _reach, _values) =
        rustdar_radar::render::render_from(&input).expect("a rendered base tilt");
    let mirror =
        Mirror::from_pane_raster(image, rustdar_radar::types::IMAGE_SIZE, site.lat, site.lon);
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
        .filter(|(m, _)| matches!(m, Mapping::CosAtSite | Mapping::LinearLatitudeV))
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
