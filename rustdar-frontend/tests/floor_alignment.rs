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
//!     `centre + Δλ · EARTH_RADIUS_KM · cos φ₀ · px_per_km`, which is a column
//!     per radian of longitude and nothing else;
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
//! | `SITE` | no | the identifier the volume's own radials carry | Radar ICAO. |
//! | `HALF_KM` | no | the app's default box | Box half-extent, km, on **both** axes. |
//! | `HALF_E_KM` | no | `HALF_KM` | Box half-extent east–west, km, overriding `HALF_KM` on that axis alone. |
//! | `HALF_N_KM` | no | `HALF_KM` | Box half-extent north–south, km. Give both axes, or `HALF_KM`, or neither. |
//! | `THRESH` | no | `15.0` | dBZ cut for the grid's echo mask. |
//! | `OUT` | no | — | Prefix; writes `_floor.ppm`, `_grid.pgm`, `_overlay.ppm`. |
//!
//! # The standing measurement
//!
//! `KDMX20250314_175512_V06`, default box, `THRESH=15`:
//!
//! | path | raster | IoU identity | best translation |
//! |---|---|---|---|
//! | the deleted `resample_floor` floor | ±230 km, 2048 px | 0.5815 | (0, 0) texels |
//! | the mirror read through `floor_colour` | ±230 km, 2048 px | 0.5777 | (0, 0) texels |
//! | the same, per-radial wedge widths | ±230 km, 2048 px | 0.5776 | (0, 0) texels |
//! | the same, extent from the sweep | ±458 km, 2048 px | 0.6789 | (−1, +2) texels |
//! | the same, side from the extent | ±458 km, 4096 px | 0.6883 | (−1, +2) texels |
//! | the same, gates on the ground | ±458 km, 4096 px | 0.6882 | (−1, +1) texels |
//! | the same, box square inside the reach | ±458 km, 4096 px | 0.6601 | (−1, +1) texels |
//! | the same, box circumscribing the ring | **±458 km, 4096 px** | **0.6572** | (0, +1) texels |
//!
//! The first two are the same measurement to within the mask criterion — the
//! old one asked "does this texel differ from the ground colour", this one asks
//! "did the mirror paint here". The third is the same picture again after each
//! radial started being painted at the width it declares rather than at the
//! distance to its neighbour: one part in six thousand, scattered wedge-edge
//! pixels, exactly as that change predicted.
//!
//! **The fourth is a different picture.** The raster is now projected at the
//! extent its own sweep reaches, and KDMX's 0.5° surveillance cut is 1832 gates
//! of 0.25 km — so the mirror covers ±458 km where it used to cover ±230, and
//! the box being scored occupies a quarter of it instead of all of it. Two
//! things follow, and they pull in opposite directions:
//!
//!   * **Coverage completes, and that is most of the gain.** The box's corners
//!     stand 325 km from the site. The grid samples the volume out there and
//!     found echo; the old raster stopped at 230 km and had nothing to show, so
//!     every corner texel was a miss the mapping could do nothing about. The
//!     far south-west square — where this day's storms were — goes 0.5009 →
//!     0.8635 on that alone.
//!   * **The mirror was half as fine, and the echo dilated.** 2.24 px/km
//!     instead of 4.45, so `render_gate`'s two extra samples per axis padded a
//!     gate's footprint by ~0.45 km rather than ~0.22 km: 38 208 texels painted
//!     against 29 263. The best translation drifts off zero by one and two
//!     texels (−0.90, +1.80 km) and buys 0.0041 of IoU, which is the dilation
//!     finding its own centre rather than a registration error — a
//!     mis-registration of that size would show as a centroid shift and does
//!     not (the centroid delta *shrank*, +24.8/−17.6 → +9.3/−13.3 texels).
//!
//! The fifth row restores the resolution: the raster's side now follows its
//! extent, so ±458 km is drawn on 4096 px at 4.45 px/km — the floor's own
//! scale. The dilation goes back with it (36 450 texels painted against
//! 38 208), the centroid delta closes a little further (+7.9/−11.0 texels) and
//! the identity IoU rises to 0.6883.
//!
//! **The sixth row is the two sides finally measuring the same geometry, and
//! it moves almost nothing.** The plan view now paints a gate at the ground
//! range under it, which is the `cos e` the grid has always sampled with — so
//! the asymmetry named in the residual paragraph below is gone. What it bought
//! on this volume: identity 0.6883 → 0.6882, painted 36 450 → 36 461, centroid
//! delta +7.9/−11.0 → +8.0/−11.0. The best translation is the one thing that
//! did move, (−1, +2) → (−1, +1) texels, i.e. the floor mask sits 0.9 km
//! closer to where the grid puts the same echo. That is the right direction
//! and it is *one texel*, so read it as consistent rather than as evidence.
//!
//! **The seventh row is the same picture measured through a coarser probe, and
//! it is not a registration change.** `box_half_width_km` now returns the
//! largest square that fits inside the volume's reach, so this volume's default
//! box is ±325.3 km where the six rows above were taken at ±230 km. The probe
//! lattice is a fixed [`PROBE_TEXELS`]² over whatever the box footprint is, so
//! a box 1.41× wider is sampled at 1.27 km/texel instead of 0.90, and both
//! masks lose about half their texels to it (floor 36 461 → 19 973, grid
//! 29 252 → 15 284). A mask's boundary is one texel wide either way, so a
//! coarser texel puts proportionally more of each mask on its own boundary and
//! IoU falls: 0.6882 → 0.6601. What did *not* move is the registration — the
//! best translation is (−1, +1) texels in both, the flips still fail by two
//! orders of magnitude (0.1296 / 0.0004 / 0.0004 against 0.6601) and the
//! centroid delta closes to +8.5/−4.6 texels. Read this row as the instrument
//! reporting on a wider box, not as the mapping having drifted.
//!
//! It is also the first row taken since the compiled-in site table was deleted,
//! and the six above it were the last measurement anyone could take: the run
//! panicked on `get_radar_site("KDMX")` from the moment the table went, so the
//! box-sizing change landed with nothing able to measure it. The site now comes
//! out of the volume — see `live_volume::site_of` — which is what makes a row
//! like this one reachable again for any radar.
//!
//! The reason it is so small is that this instrument is dominated by tilts
//! where `cos e` is nothing. KDMX's mask is mostly its 0.5° surveillance cut,
//! where the correction is 0.08 px; the tilts where it is worth 18 px paint a
//! small fraction of the texels. **A TDWR volume is where this row would
//! move**, and there is no TDWR in this instrument's corpus.
//!
//! **The eighth row is the box widening once more, and it is the seventh row's
//! arithmetic run a second time.** `box_half_width_km` returns the reach
//! itself now — the box circumscribes the data ring instead of standing
//! inscribed in it — so this volume's default box is ±460.1 km where the
//! seventh row was taken at ±325.3, the same √2 again. The probe pitch
//! coarsens 1.27 → 1.80 km/texel, both masks halve again (floor 19 973 →
//! 9 964, grid 15 284 → 7 680) and identity IoU falls 0.6601 → 0.6572.
//!
//! **What did not move is the registration, and the centroid says so in
//! kilometres.** +8.5/−4.6 texels at 1.27 km/texel is +10.80/−5.85 km; this
//! row's +5.8/−2.9 at 1.80 km/texel is +10.42/−5.21 km. The same offset to
//! within 0.4 and 0.6 km, which is a third of a texel on either probe. The
//! best translation reading (0, +1) texels where the seventh read (−1, +1) is
//! that same statement in the coarser unit: the east lane's −1.27 km is now
//! under half of a 1.80 km texel and rounds to zero, and the north lane's does
//! not. The flips still fail, by 5× for x and by three orders of magnitude for
//! the two that mirror v (0.1299 / 0.0006 / 0.0005 against 0.6572).
//!
//! **The corner columns say nothing at this box**, and that is a property of
//! the box rather than of the day. [`Region::far_corner`] is the outer quarter
//! of each axis, which now begins 230 km out, and this day's mass sits inside
//! that on every side: all four corners hold zero grid texels where the
//! ±325.3 km box put 380 of them in the south-west. Read the eighth row's
//! whole-box and centre columns only.
//!
//! **A fixed box re-read, and a residual this file cannot yet account for.**
//! `HALF_KM=230` on the same binary gives identity 0.6905 where the sixth row
//! recorded 0.6882, with the grid mask *identical* — 29 252 texels, the same
//! number — and the floor's up 13, 36 461 → 36 474. So the grid is the sixth
//! row's exactly and the mirror is not. Two candidates, neither confirmed
//! here: the site anchor (the sixth row was placed from the compiled-in table
//! and every row since is placed where the volume says, which moves the
//! raster's Mercator bounds while leaving a mask taken in box coordinates
//! alone), and the render's own rework since. The grid staying put under it is
//! consistent with either, because a 1.80 km cell and a 0.90 km probe texel
//! are both far coarser than a 0.22 km raster pixel. It is 0.0023 of IoU, it
//! is written down rather than explained, and nothing in the commit that
//! recorded it touched either side.
//!
//! **The mapping table still does not discriminate on this volume, and neither
//! the resolution nor the `cos e` asymmetry was why.** Whole-box on the ±325.3
//! km box: honest 0.6601, trapezoid 0.6649, linear-v 0.6603 — the two
//! deliberate perturbations score *above* the exact mapping, by +0.0048 and
//! +0.0002. They did the same at ±230 km on both raster sides: 0.6882 /
//! 0.6901 / 0.6933 at 4096 px, and +0.0008 / +0.0049 at 2048 px. Three
//! resolutions and two box sizes moved those margins by under 0.006 without
//! changing their sign, which disposes of the explanation the 2048 row
//! carried (that halving the uv rate sank two second-order errors under the
//! mask noise).
//!
//! **At the ±460.1 km box the sign finally turns over, and the turn is not the
//! finding.** Honest 0.6572 against trapezoid 0.6567 and linear-v 0.6548 — the
//! exact mapping leads, by 0.0005 and 0.0024. Three box sizes and two raster
//! sides have now put every one of these margins inside ±0.005 of zero in both
//! directions, which is what "the table does not discriminate here" looks like
//! when it is measured rather than assumed: a margin that changes sign under a
//! box change is a margin that was never carrying a signal. The rows that *do*
//! carry one are unmoved — `no cos(lat)` reads 0.3755 against 0.6572 on this
//! box, and the discrimination this file actually asserts is against the
//! synthetic fixture, not against this table. Measured directly on that
//! fixture — the same tree, the same field, only the side changed —
//! resolution barely moves this instrument at all: the corner falls are
//! 0.2366 / 0.1610 at 2048 px against 0.2406 / 0.1648 at 4096.
//!
//! What is left is the volume. This day's echo is one broad mass in the
//! south-west — 380 of the box's 15 284 grid texels are in that one eighth at
//! this probe pitch, and the other three corners hold 0, 0 and 4 — and the
//! honest mapping already carries a residual against it that has nothing to do
//! with the mapping: the grid's mask is a **column max** through the whole box
//! while the raster's is one tilt's plan view, so the two masks differ in area
//! by a third (19 973 against 15 284; 36 461 against 29 252 at ±230 km). That
//! was previously attributed in part to the raster omitting `cos e`; it applies
//! it now and the areas did not move, so the difference is the column max and
//! nothing else. A second-order perturbation of a few kilometres inside a
//! contiguous echo mass costs almost no overlap, and one that happens to nudge
//! the floor mask along that residual *buys* some. Only `no cos(lat)` — first
//! order, 1.34× at this latitude — still falls clear (0.3738, and 0.4634 at
//! ±230 km).
//!
//! So this table reports registration and coverage on a real volume, and the
//! discrimination that is *asserted* rather than reported lives in
//! [`a_broken_mapping_costs_iou_in_the_corner_even_where_the_centre_cannot_tell`],
//! whose field has structure at the perturbations' own scale everywhere in the
//! box. That is the fixture to change if the shader's mapping is ever
//! reworked; this one is for coverage and gross misregistration.
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
use rustdar_radar::types::KM_PER_DEGREE_LAT;

/// Side of the lattice both masks are expressed on, in texels.
///
/// Nothing in the shipped path has a floor lattice any more — the mirror is
/// frame-sized and the march samples it per pixel. 512 is the deleted
/// `volume_floor.rs`'s `FLOOR_TEXELS`, kept so this instrument's numbers stay
/// comparable with the ones the old path was measured at.
const PROBE_TEXELS: usize = 512;

/// The 3D texture limit these fixtures build their grids against.
///
/// The grid's shape is a runtime answer now — `voxel::shape_for_budget` spends
/// the tier's cell budget over the axes a device says it can hold — so a test
/// that wants a grid has to name a device. This one names the least capable
/// conforming device, the WebGL2 guarantee, for two reasons: what is being
/// measured here is a **mapping**, which is a property of the box rather than
/// of the cell count, and a fixture that moved with the adapter would be an IoU
/// threshold nobody could reproduce. It is also the cheapest, which matters for
/// a test that resamples a synthetic volume in the gate.
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
    /// Kilometres of ground across one texel: `2 · extent / side`.
    ///
    /// Here because a budget written in *pixels* is a budget that changes
    /// meaning when the raster's size does, and it does now — the same 237 km
    /// fixture is 2048 texels on a device that cannot take the long-range
    /// raster and 4096 on one that can. What the mapping can be wrong by is a
    /// distance on the ground, so that is the unit the pins below state.
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
    ///
    /// u is linear in longitude and v in Mercator y, both anchored at the site
    /// the way the uniform anchors them. `v_per_mercator_y` is **negative**:
    /// Mercator y grows north and rows grow south.
    ///
    /// `extent_km` is the **render's own second return value**, not a
    /// constant. A raster is projected at `plan_view_extent_km` of its sweep's
    /// reach, so the fixtures here span three different amounts of ground —
    /// 230 km where the tilt stops inside the floor, 237 km where it does not,
    /// 458 km on a real super-res volume — and a mirror built at any other
    /// number would score a correct mapping as broken.
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
    /// Drop the `cos φ` term: `d_lon = x_km / KM_PER_DEGREE_LAT`, as though a degree of
    /// longitude were a degree of latitude. Stretches the sampled ground
    /// east-west by `1 / cos φ` about the site's meridian — nothing at
    /// `x = 0`, tens of kilometres at the box's east and west edges.
    NoCosLat,
    /// The **equirectangular** mapping this reprojection used to run:
    /// `φ = φ₀ + y/K`, `Δλ = x/(K·cos φ)`. Not an invented mistake — it is
    /// what shipped, and it registered only because `render_gate` placed the
    /// raster's gates by the same approximation. Zero on the cardinals through
    /// the site and outward on the diagonals, growing with range and with
    /// latitude: ~15 km at the default box's corners at 41.7 °N, which is twice
    /// the trapezoid error this reprojection was introduced to remove.
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
///
/// ```text
/// x_km  = floor_geo.y + hit.x · box_size_km.x
/// y_km  = floor_geo.z + hit.y · box_size_km.y
/// δ     = hypot(x_km, y_km) / EARTH_RADIUS_KM      ← the box is polar…
/// sin φ = sin φ₀·cos δ + cos φ₀·sin δ·cos az       ← …so this is spherical
/// Δλ    = atan2(sin az·sin δ·cos φ₀, cos δ − sin φ₀·sin φ)
/// u     = floor_uv.x + Δλ° · floor_uv.z
/// v     = floor_uv.y + (mercᵧ(φ) − mercᵧ(φ₀)) · floor_uv.w
/// ```
///
/// `(sin az, cos az)` is `(x_km, y_km)/range`, which is what makes the box's
/// own coordinates the bearing and the range with no trigonometry in between —
/// see `rustdar_radar::voxel`'s "Geometry" section.
///
/// `None` where the shader returns transparent: off the mirror, and within a
/// millionth of a pole.
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
            // `beam::great_circle_destination` about the site, which is what
            // the box's kilometres mean. Called rather than restated: the
            // point of this instrument is to score the *shader's* arithmetic,
            // and the placement it has to agree with is the radar crate's.
            let range_km = x_km.hypot(y_km);
            let bearing_deg = x_km.atan2(y_km).to_degrees();
            let (lat, lon) = rustdar_radar::beam::great_circle_destination(
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

    /// One far corner — the outer quarter of the box's side on each axis, so
    /// roughly 115..230 km out along both on the default box. Both the
    /// trapezoid error and the Mercator one are at their largest here. Whether
    /// the radar reaches all of it depends on the volume: a corner stands
    /// 325 km from the site, so a sweep that stops at 230 km cuts this square
    /// on the diagonal while a 458 km surveillance cut fills it.
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

/// The box's half-extent from the environment, or `None` for the box
/// `build_voxels` sizes from the volume's own reach.
///
/// `None` is what a pane with no picked region sends, and it stays the default
/// so the standing measurement is still one `VOL=` away.
///
/// **Two axes rather than one**, because a 3D pane's box is the rectangle of
/// ground its viewport shows and no longer the square inscribed in it. An
/// instrument that could only ask for a square could not score the shape the
/// application actually renders — and the floor covering the box is precisely
/// the invariant the inscribed square used to protect for free, so a rectangle
/// is the case now worth measuring.
///
/// One axis alone is refused rather than paired with an invented number: the
/// other axis's honest partner is the default box, which is not known until
/// `build_voxels` has seen the scan, and quietly substituting a stand-in would
/// report a shape nobody asked for.
fn half_extent_from_env() -> Option<rustdar_radar::voxel::HalfExtentKm> {
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
            Some(rustdar_radar::voxel::HalfExtentKm { east_km, north_km })
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
    let request = rustdar_radar::voxel::VoxelRequest {
        centre: (site_lat, site_lon),
        half_extent_km: half_extent,
        base_km_msl: rustdar_radar::voxel::DEFAULT_BASE_KM_MSL,
        top_km_msl: rustdar_radar::voxel::DEFAULT_TOP_KM_MSL,
        product: RadarProduct::Reflectivity,
        shape: rustdar_radar::voxel::default_shape(GRID_DEVICE_AXIS),
        values_wanted: false,
    };
    let grid = rustdar_radar::voxel::build_voxels(&scan, &request, site_lat, site_lon)
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
    let shape = grid.shape();
    // **Per axis.** The probe lattice is a fixed `PROBE_TEXELS`² over whatever
    // the box footprint is, so on a rectangular box a texel is not square on
    // the ground and one number cannot convert both lanes of a translation.
    // Reading the east rate onto the north lane is how a rectangle would report
    // a registration error it does not have.
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
    //
    // Read the corner columns with care on a real volume. IoU inside a
    // sub-region is only a fair comparison when both masks fill it comparably:
    // where the grid's echo saturates a corner and the floor's does not,
    // `no cos(lat)` — which stretches the sampled ground outward by `1/cos φ`,
    // 1.34× at KDMX — drags *more* echo into the square and can score **above**
    // the honest mapping there while losing badly over the whole box. Measured
    // on KDMX 2025-03-14 at the ±230 km box: whole box 0.5777 honest against
    // 0.4989 broken, far SW corner 0.5009 honest against 0.6326 broken. It does
    // not reproduce at the ±325.3 km box the same volume gets now — 0.7204
    // honest against 0.1683 there, because the wider square no longer saturates
    // — which is the point: whether the corner column is fair depends on the
    // volume and the box, so it is never the thing to conclude from. That is a
    // property of scoring
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
    let (image, side, extent_km) = pane_raster(&input).expect("a rendered base tilt");
    Mirror::from_pane_raster(image, side, site_lat, site_lon, extent_km)
}

/// The raster a **static desktop pane** would produce for `input`, its side and
/// its extent — the picture the shipped mirror is made of.
///
/// `render_from_sized` at this build's own long-range ceiling, not
/// `render_from`: a pane render is dispatched at the ceiling its device
/// reported, so a sweep reaching past the 230 km floor is drawn onto at least
/// 4096 px rather than 2048, and
/// an instrument that modelled the mirror at the base size would be scoring a
/// picture no pane ever shows.
///
/// The side is read back off the buffer for the reason every consumer in the
/// app reads it back off the buffer: it is a function of how far the sweep
/// reached, which is a property of the volume on disk and not of this file.
fn pane_raster(input: &rustdar_radar::render_input::RenderInput) -> Option<(Vec<u8>, usize, f64)> {
    let rustdar_radar::render::SweepRender {
        image,
        max_range_km: extent_km,
        ..
    } = rustdar_radar::render::render_from_sized(
        input,
        rustdar_frontend::constants::LONG_RANGE_IMAGE_SIZE,
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
///
/// `rustdar-radar` carries no list of the network — see
/// [`SiteTable`](rustdar_radar::sites::SiteTable) — so a test binary that
/// decodes no volume and fetches no catalogue has an empty table, and
/// `get_radar_site` answers `None` for every identifier. The application
/// resolves its own table before its first frame; this is the same step, for a
/// process that has no application in it.
///
/// Its own copy rather than the library's `crate::test_sites`, because an
/// integration test is a separate crate and cannot reach a `#[cfg(test)]`
/// module inside the one it is testing.
///
/// **Not what places the site for
/// [`measure_floor_against_grid_on_a_real_volume`]** — which is `#[ignore]`d,
/// since it wants both an adapter and a volume on disk. That one holds a volume,
/// and a volume says where its own radar is — so it learns `VOL`'s site rather
/// than looking it up here, and works on any radar's volume instead of on the
/// two written down below. See `live_volume::site_of`. A fixture test has no
/// volume to learn from and a synthetic grid it builds itself, which is what
/// this list is for.
///
/// The positions are load-bearing here in a way they are not elsewhere: the
/// point of `the_boxs_site_position_lands_on_the_mirrors_site_pixel` is that
/// the Mercator offset grows with latitude, so `KMPX` has to actually be at
/// 44.8°N for the offset it pins to be the ~18 px the note describes.
fn install_radars() {
    use rustdar_radar::site_position::SitePosition;
    use rustdar_radar::sites::SiteFix;

    /// `(ICAO, latitude, longitude, site_height_m, tower_height_m)`, the
    /// position and heights each radar's own Level II volume reports.
    const SITES: [(&str, i32, i32, i32, i32); 2] = [
        ("KMPX", 44_849_000, -93_566_000, 288, 30),
        ("KTLX", 35_333_060, -97_277_500, 370, 19),
    ];

    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        rustdar_radar::sites::resolve(SITES.map(
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
///
/// A **fixture size**, deliberately, and no longer "what the app requests":
/// the box a sourceless pane gets now follows its volume's own reach
/// (`rustdar_radar::voxel::box_half_width_km`), and the two tests below are
/// about the *shape* of the box→mirror mapping rather than about which box it
/// is handed. Pinning it here keeps their probes — one of them 190 km west of
/// the site — inside the box whatever the extent policy does next, and the two
/// tests that do care about the shipped geometry read it off a real grid
/// (`BoxGeo::from_grid`) instead.
fn default_box() -> BoxGeo {
    let half = rustdar_radar::voxel::BASE_HALF_WIDTH_KM;
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
    install_radars();
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
    // raster's middle by Mercator's own curvature over the frame's half-width.
    // If this ever reads zero the raster has stopped being a Mercator picture
    // and the v axis of the mapping is no longer the right shape for it.
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
///     site instead of at the point costs 3.67 km and a latitude-linear v axis
///     2.47;
///   * `(60, 215)` — nearly due north, where the `cos` errors nearly vanish
///     (1.93 km) and the v axis is at its worst (4.05 km);
///   * `(-190, -100)` — the opposite quadrant, which catches a sign as well as
///     a scale, and where the v error is down to 0.65 km.
///
/// Probes at KMPX, 44.8°N, for the reason
/// [`a_broken_mapping_costs_iou_in_the_corner_even_where_the_centre_cannot_tell`]
/// gives: both second-order errors scale with `tan φ₀`, and this pin wants them
/// clear of the honest mapping's own budget rather than merely above it.
///
/// **The figures are kilometres of ground, not texels**, and that is the
/// change the adaptive raster forced: this fixture reaches 237 km, so it is
/// drawn on 2048 texels where the device cannot take a long-range raster and
/// on 4096 where it can, and every texel figure would double between the two
/// while nothing about the mapping moved. What a mapping can be wrong by is a
/// distance.
///
/// The honest budget is 0.9 km. When it was set, most of it was one known
/// disagreement — `render_gate` walked north on `EARTH_RADIUS_KM` (6371 km)
/// and `floor_colour` on a `KM_PER_DEGREE_LAT` of 111.32 (a 6378 km sphere),
/// 0.12 % apart, about 0.24 km at 200 km — with a measured worst probe of
/// 0.48 km against it. The two spheres are one sphere now, so what is left is
/// the blob's own discretisation and the worst probe can only have come down.
///
/// The budget is deliberately *not* tightened to the smaller residual: it is a
/// ceiling separating an honest mapping from the broken ones at `MUST_MISS_KM`,
/// and those still miss by more than twice it for reasons — an
/// equirectangular reprojection, a latitude-linear v axis — that no
/// unification touches.
#[test]
fn a_gate_lands_on_the_mirror_pixel_that_renders_it() {
    const HONEST_BUDGET_KM: f64 = 0.9;
    const MUST_MISS_KM: f64 = 2.3;

    install_radars();
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
///
/// [`default_box`] is square, and on a square box `BoxGeo`'s two sizes are
/// interchangeable — so is `floor_colour`'s pair of reprojection lines, which
/// this module restates. A box built with the east extent on both axes, or
/// with the two exchanged, maps every `hit` exactly where the honest one does.
///
/// So this pin runs the mapping **forward from a box position** rather than
/// from a place on the ground. `mirror_pixel_for_km` cannot see any of this:
/// it goes km → `hit` → km through [`BoxGeo::hit_at_km`] and back through
/// [`mirror_uv`], and those two cancel exactly whatever the extents are. What
/// the shader actually does is start from `hit` — the ray's crossing of the
/// box's bottom face, in the box's own `0..1` coordinates — and the box is the
/// only thing that says what ground that is.
///
/// 460 × 230 km, centred on the site: the 2:1 footprint a wide 3D pane frames,
/// with both halves inside the fixture's 237 km raster.
///
/// `MUST_MISS_KM` is 50 rather than the 2.3 the mapping pins use, and
/// deliberately so: these two are not projection subtleties competing with the
/// honest mapping's own budget. They read a box coordinate against the wrong
/// axis's extent, which at these probes is 60 to 105 km of ground. A bound
/// that made them look marginal would be describing them wrongly.
#[test]
fn a_rectangular_boxs_two_extents_each_stay_on_their_own_axis() {
    const HONEST_BUDGET_KM: f64 = 0.9;
    const MUST_MISS_KM: f64 = 50.0;

    install_radars();
    let site = rustdar_radar::sites::get_radar_site("KMPX").expect("KMPX is a known site");
    let mirror = mirror_from_field(site.lat, site.lon, 720, 940, &|_, _| None);

    let honest = BoxGeo {
        west_km: -230.0,
        south_km: -115.0,
        size_x_km: 460.0,
        size_y_km: 230.0,
    };
    // The box the pane used to send, and the box a transposition would build.
    // Both centred on the site, like the honest one, so the only thing wrong
    // with either is which extent went on which axis.
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
//    and miss by more than a hundred kilometres;
//  * the historical 2026-08-09 2× floor zoom: the raster's *data reach* fed to
//    the old resampler as its half-extent. The reach and the extent are once
//    again two different numbers — a raster is projected at
//    `plan_view_extent_km` of the reach, which holds it at 230 km until the
//    data passes that — so the fixture's short low tilt (700 gates, 177 km
//    against a 230 km frame) is not a historical curiosity but the live
//    discriminator: a mirror built from 177 would be 1.3× zoomed and this pin
//    would fail. The **box** is now a third number again — ±125.2 km, the
//    reach over √2 (`voxel::box_half_width_km`) — so reach, frame and box are
//    three distinct scales here and a mapping that confused any two of them
//    cannot pass.

#[test]
fn a_planted_storm_lands_on_the_floor_exactly_under_its_own_voxels() {
    // A 55 dBZ disc, radius 20 km, centred 60 km east / 85 km north of the
    // site — off-centre on both axes and off the diagonal, so every flip and
    // the site-centred control disagree with it, by 120 km or more.
    //
    // Inside the box on every side with room to spare, which is now a fixture
    // constraint rather than a free choice: the box this volume earns is
    // ±125.2 km (177.1 km of reach over √2), and a disc clipped by the box
    // edge would move the grid's centroid without moving the floor's and fail
    // the alignment pin for a reason that is not a misalignment.
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
    let site = rustdar_radar::sites::get_radar_site("KTLX").expect("KTLX is a known site");

    // Path one: the voxel build, at the app's own default request.
    let request = rustdar_radar::voxel::VoxelRequest {
        centre: (site.lat, site.lon),
        half_extent_km: None,
        base_km_msl: rustdar_radar::voxel::DEFAULT_BASE_KM_MSL,
        top_km_msl: rustdar_radar::voxel::DEFAULT_TOP_KM_MSL,
        product: RadarProduct::Reflectivity,
        shape: rustdar_radar::voxel::default_shape(GRID_DEVICE_AXIS),
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
    let (image, side, extent_km) = pane_raster(&input).expect("a rendered base tilt");
    let mirror = Mirror::from_pane_raster(image, side, site.lat, site.lon, extent_km);
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
///   * [`Mapping::Equirectangular`] (the mapping this reprojection used to run)
///     and [`Mapping::LinearLatitudeV`] are **second order**: both are exactly
///     right at the site and grow with the square of the distance from it.
///     These are the errors the warning is about, and the first of them is not
///     hypothetical — it is what shipped, invisible because the raster it was
///     scored against carried the same approximation. A centred-only
///     instrument would have called both of them clean.
///
/// The site is **KMPX**, at 44.8°N, and not the KTLX the other fixtures fly:
/// both second-order errors scale with `tan φ₀`, so a northern site is where
/// this fixture has the most to say. The mapping is not site-specific and
/// nothing here depends on which site it is beyond that.
#[test]
fn a_broken_mapping_costs_iou_in_the_corner_even_where_the_centre_cannot_tell() {
    install_radars();
    let site = rustdar_radar::sites::get_radar_site("KMPX").expect("KMPX is a known site");
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

    let request = rustdar_radar::voxel::VoxelRequest {
        centre: (site.lat, site.lon),
        half_extent_km: None,
        base_km_msl: rustdar_radar::voxel::DEFAULT_BASE_KM_MSL,
        top_km_msl: rustdar_radar::voxel::DEFAULT_TOP_KM_MSL,
        product: RadarProduct::Reflectivity,
        shape: rustdar_radar::voxel::default_shape(GRID_DEVICE_AXIS),
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
