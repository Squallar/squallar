//! The seam between a 3D pane and whatever can actually draw one, plus every
//! matrix that turns an [`OrbitCamera`] into the two numbers the raymarch reads.
//!
//! Box space is the unit cube `[0,1]³` over the voxel grid; world space is
//! kilometres with `x` east, `y` north, `z` up and the origin at the box's
//! centre. [`view_for`] builds
//!
//! ```text
//! box_from_clip = box_from_world · world_from_view · view_from_clip
//! ```
//!
//! **compositionally**, never by inverting a general 4×4. Each factor has a
//! closed form: `box_from_world` is a scale and a translate, `world_from_view`
//! *is* the camera basis (the inverse of a look-at is built, not computed), and
//! `view_from_clip` is the analytic inverse of the perspective matrix.

use std::any::Any;
use std::sync::Arc;

use crate::pane::{OrbitCamera, VolumeTarget};

/// Vertical field of view of the volume camera, degrees.
const FOV_Y_DEG: f32 = 40.0;

/// Near plane, in multiples of the [`framing_radius_km`] the eye stands off in.
const NEAR_IN_FRAMING_RADII: f32 = 0.02;
/// Far plane beyond the eye, in multiples of the **stretched box's**
/// half-diagonal. See [`NEAR_IN_FRAMING_RADII`] for why the two planes are
/// measured in different units.
const FAR_MARGIN_IN_HALF_DIAGONALS: f32 = 2.0;

/// Shortest cross product the camera basis will accept before calling itself
/// degenerate. Reached only if pitch is at ±90°, which [`OrbitCamera`] does not
/// allow — so this is the guard for a caller who built a camera another way.
const MIN_BASIS_LENGTH: f32 = 1e-6;

/// A column-major 4×4, `m[column][row]` — WGSL's convention and std140's, so
/// the columns go out in order with no transpose.
pub type Mat4 = [[f32; 4]; 4];

/// Web Mercator's `y` for a latitude in radians: `ln(tan(π/4 + φ/2))` —
/// [`squallar_geo::lat_rad_to_mercator_y`], whose one definition is now the one
/// spelling workspace-wide, not just in this crate.
pub fn mercator_y(lat_rad: f64) -> f64 {
    squallar_geo::lat_rad_to_mercator_y(lat_rad)
}

/// Web Mercator's `y` for a latitude in **degrees**.
pub fn mercator_y_of_lat(lat_deg: f64) -> f64 {
    squallar_geo::lat_rad_to_mercator_y(lat_deg.to_radians())
}

/// How a map render maps geography onto egui's coordinate space — the affine a
/// 3D pane needs in order to find its ground inside a copy of that render.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapPaneGeo {
    /// The pane's rect in **points**, in the frame's own coordinate space —
    /// what the mirror pass clips this pane's primitives to.
    pub rect: egui::Rect,
    /// The anchor's latitude, degrees north.
    pub anchor_lat: f64,
    /// The anchor's longitude, degrees east.
    pub anchor_lon: f64,
    /// Where the anchor landed on the frame, in points.
    pub anchor: egui::Pos2,
    /// Points of screen `x` per degree of longitude east. Positive.
    pub points_per_degree_lon: f64,
    /// Points of screen `y` per unit of Mercator `y`. **Negative**: Mercator
    /// `y` increases north and screen `y` increases down.
    pub points_per_mercator_y: f64,
}

impl MapPaneGeo {
    /// Where `(lat, lon)` lands on the frame, in points, by this affine.
    pub fn project(&self, lat_deg: f64, lon_deg: f64) -> egui::Pos2 {
        let dx = (lon_deg - self.anchor_lon) * self.points_per_degree_lon;
        let dy = (mercator_y_of_lat(lat_deg) - mercator_y_of_lat(self.anchor_lat))
            * self.points_per_mercator_y;
        egui::pos2(self.anchor.x + dx as f32, self.anchor.y + dy as f32)
    }
}

/// **Which light a 3D pane's ground and volume are both lit by.**
///
/// One value, not two, and that is the point of C2: the terrain, the flat map
/// lid and the raymarched storm are lit off one uniform, so a warm sunset
/// ground under a neutral-white storm is not a defect to be found but an
/// arithmetic nothing can write. `squallar_volumetric::uniform::SurfaceLight`
/// is the shape this reaches the GPU as.
///
/// **Computed here, in `squallar-egui`, and carried through** — not in
/// `squallar-volumetric` where the rest of the uniform is built. That crate
/// has `squallar-geo` only as a dev-dependency and its own charter pins it
/// there, so putting the solar arithmetic beside the rest of the uniform would
/// mean promoting a band-0 crate into the volumetric graph to avoid passing
/// one struct.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VolumeLight {
    /// The fixed, always-above light this renderer has always used. Readable
    /// from any orientation of the box, and the light a pane falls back to.
    Headlight,
    /// The real sun over the box's own anchor at the volume's own collection
    /// time.
    Sun(SunOverBox),
}

impl VolumeLight {
    /// Whether this is the real sun. The pane's control asks, so it can say
    /// which light the picture is under rather than which one was asked for.
    pub fn is_sun(&self) -> bool {
        matches!(self, Self::Sun(_))
    }
}

/// The sun as this renderer needs it: a direction in box space and the two
/// colours that go with it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SunOverBox {
    /// Unit vector from the ground toward the sun, in **box space**.
    ///
    /// `squallar_geo::solar` answers in the local east-north-up frame, and box
    /// space *is* that frame for this box — `volume.wgsl`'s `box_x_km` is
    /// kilometres east of the anchor and `box_y_km` kilometres north — so the
    /// three components carry across unchanged.
    ///
    /// **No exaggeration correction, deliberately.** Both surfaces take their
    /// normals against the *displayed* geometry, so a box stretched 3x is lit
    /// as the steep thing it is drawn as; correcting the light instead would
    /// make the shading disagree with the silhouette in the same picture.
    pub direction_box: [f32; 3],
    /// Linear-light RGB of the direct beam, `squallar_geo::solar::sun_tint`.
    /// Zero at and below the horizon.
    pub beam: [f32; 3],
    /// Linear-light RGB of the sky, `squallar_geo::solar::sky_ambient`. Never
    /// zero, which is what makes the night floor reach a pixel.
    pub sky: [f32; 3],
    /// The sun's geometric elevation, degrees. Carried for the pane's own
    /// readout.
    pub elevation_deg: f32,
}

/// The sun over `anchor` at `unix_seconds`, or `None` on exactly the inputs
/// `squallar_geo::solar::sun_light` refuses.
///
/// **One light for a whole box is an approximation, and a stated one.** The
/// sun's hour angle runs four minutes of solar time per degree of longitude,
/// so the two ends of the widest box this app can frame are about half an hour
/// apart. A per-fragment sun would be a second light, which is the one thing
/// this unit exists to prevent.
pub fn sun_over(anchor: squallar_geo::GeoPoint, unix_seconds: f64) -> Option<SunOverBox> {
    let sun = squallar_geo::solar::sun_light(anchor.lat, anchor.lon, unix_seconds)?;
    let [east, north, up] = sun.direction_enu;
    let white = zenith_white();
    let balance = |c: [f64; 3]| {
        let mut out = [0.0f32; 3];
        for (slot, (value, reference)) in out.iter_mut().zip(c.into_iter().zip(white)) {
            *slot = (value / reference) as f32;
        }
        out
    };
    Some(SunOverBox {
        direction_box: [east as f32, north as f32, up as f32],
        beam: balance(sun.colour),
        sky: balance(sun.ambient),
        elevation_deg: sun.position.elevation_deg as f32,
    })
}

/// **The light a level surface takes under a zenith sun**, per channel — the
/// white this renderer balances against, and the exposure `squallar_geo::solar`
/// deliberately leaves to its consumer.
///
/// Dividing by it makes the sun's light *exactly* neutral and *exactly* one at
/// the zenith, so a basemap at noon reads as the colour it was authored as and
/// every departure from that is a real move of the sun. Two things it fixes,
/// both measured on the shipped light style rather than argued:
///
/// * **Unbalanced, a noon basemap is blue.** `colour + ambient` at the zenith
///   is `[1.25, 1.29, 1.40]` — a clear sky really does add a quarter again, and
///   more of it in blue. The eye adapts to that outdoors and a screen does not,
///   so `www/styles/light.json`'s near-white palette came out 0.098 blue-minus-
///   red at a 60-degree sun, at every one of the eleven cameras.
/// * **Unbalanced, it also clips.** Those same figures put a 0.955-albedo
///   background at 1.19 and a 0.895 landcover at 1.12, which are both 1.0 on a
///   screen: the style's whole tonal separation between background, landcover
///   and water is inside 0.06 of albedo, and an unbalanced daylight flattens it
///   to one white.
///
/// Derived from C1's own two ramps rather than written down, so it cannot
/// disagree with them about what daylight is.
fn zenith_white() -> [f64; 3] {
    let beam = squallar_geo::solar::sun_tint(90.0);
    let sky = squallar_geo::solar::sky_ambient(90.0);
    let mut out = [0.0f64; 3];
    for (slot, (b, s)) in out.iter_mut().zip(beam.into_iter().zip(sky)) {
        // Never zero: `sun_tint(90)` is 1 in every channel by its own table's
        // last knot, and `sky_ambient` is documented never to be zero at all.
        *slot = b + s;
    }
    out
}

/// **The one place a refused instant is decided**, and it decides on the
/// headlight.
///
/// `sun_light` answers `None` rather than a half-answer for a non-finite
/// argument, a latitude that is not a place, or an instant further from J2000
/// than about five centuries. Three answers were available and two of them are
/// wrong. `unwrap_or(<night>)` is the worst: it is exactly the silent-night
/// defect the `Option` was introduced to remove, and it paints a dark pane
/// that looks like a correct 3 a.m. picture and is a refusal. Refusing to draw
/// at all is worse than it sounds too — the instant reaching here is the
/// volume's own collection time, and a pane that blanks because a timestamp is
/// odd has traded the whole picture for a light.
///
/// So a refused instant takes **the readable light**: a complete, legible,
/// correct picture of the volume rather than a degraded one, and the same
/// picture the toggle's other position draws. Nothing is half-done, so this is
/// not the silent-partial-success shape. What keeps it from being *silent* is
/// that the pane's own control calls this same function and reports which
/// light came back — see `ui_map::render_volume_controls`.
pub fn volume_light(
    accurate: bool,
    anchor: Option<squallar_geo::GeoPoint>,
    unix_seconds: f64,
) -> VolumeLight {
    if !accurate {
        return VolumeLight::Headlight;
    }
    match anchor.and_then(|anchor| sun_over(anchor, unix_seconds)) {
        Some(sun) => VolumeLight::Sun(sun),
        None => VolumeLight::Headlight,
    }
}

/// A volume's collection time as Unix seconds.
///
/// The conversion lives at the call site because `squallar-geo`'s charter
/// forbids it a chrono dependency, so its solar API takes a plain `f64`. One
/// spelling, here, rather than one per caller.
pub fn unix_seconds_of(collected: chrono::NaiveDateTime) -> f64 {
    collected.and_utc().timestamp() as f64
}

/// Everything the painter is told about one 3D pane on one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeFrameState {
    /// Which pane is asking. The renderer's offscreen targets are per-pane —
    /// two 3D panes at different sizes need two — and `egui_wgpu`'s
    /// `CallbackResources` is keyed by **type**, so this index is the only
    /// thing that can tell them apart.
    pub pane_idx: usize,
    /// Which volume and moment the pane wants drawn.
    pub target: VolumeTarget,
    /// Where the eye is, **after** this frame's drag.
    pub camera: OrbitCamera,
    /// The pane's size in physical pixels, before any quality rung is applied.
    pub size_px: [u32; 2],
    /// The scale [`Self::size_px`] was measured at, so the pane's size in
    /// *points* is recoverable.
    pub pixels_per_point: f32,
    /// Whether this pane wants the map floor drawn under the volume.
    pub floor: bool,
    /// The Mercator affine of the map this pane drew into its own off-screen
    /// floor strip, and the strip it drew it into.
    pub source: Option<MapPaneGeo>,
    /// How much of egui's coordinate space the pane mirror covers, in points —
    /// `Gui::mirror_size_points`.
    pub mirror_size_points: [f32; 2],
    /// The user's Volume Alpha curve for this pane's product, or `None` for
    /// an untouched editor.
    pub alpha: Option<crate::volume_alpha::AlphaCurve>,
    /// How the pane draws its volume: the lit accumulation or an isosurface.
    pub view_mode: crate::pane::VolumeViewMode,
    /// The isosurface threshold for this pane's product, in the product's own
    /// units ([`squallar_radar::voxel::iso_shape`] says what the number
    /// means). Read only in isosurface mode; the renderer translates it into
    /// index space against the grid's own ramp.
    pub iso_threshold: f32,
    /// **The light this pane's ground and volume are both lit by.** See
    /// [`VolumeLight`].
    pub light: VolumeLight,
    /// The terrain this pane's ground should be drawn as, or `None` while no
    /// height field has landed for it — in which case the pane draws the flat
    /// map floor at box `z = 0`, exactly as it always has.
    pub heights: Option<Arc<GroundHeightField>>,
    /// The extruded buildings standing on that terrain, or `None` for a pane
    /// drawing bare ground.
    ///
    /// **Read only alongside [`Self::heights`]**, and that is a property of
    /// the picture rather than a convenience: a prism is authored as a height
    /// *above the ground*, so a building with no height field under it has
    /// nowhere to stand. The renderer drops it rather than putting it at sea
    /// level.
    pub buildings: Option<Arc<BuildingPrismMesh>>,
}

/// An extruded building mesh, as the renderer needs it.
///
/// **A plain carrier, not `squallar_buildings::BuildingMesh`**, for the reason
/// [`GroundHeightField`] is not `squallar_elevation::HeightField`: neither this
/// crate nor `squallar-volumetric` declares `squallar-buildings`, and the
/// latter's charter pins the normal dependency set by name. The buildings crate
/// links neither egui nor wgpu on purpose — all of it runs inside the offload
/// worker — so what crosses is the numbers.
#[derive(Clone, Debug, PartialEq)]
pub struct BuildingPrismMesh {
    /// Distinguishes one mesh from the next on the GPU, where the box's `f64`
    /// ranges cannot go. Equality is what says "this is the mesh already
    /// uploaded".
    pub id: u64,
    /// The site the mesh's kilometres are measured from, degrees north and
    /// east — the same anchor the volume grid carries.
    ///
    /// **Checked rather than assumed by the renderer.** The prisms are
    /// site-relative, which is what lets one upload survive a box change; the
    /// one thing that must not change under them is the site, and a mesh built
    /// around a different one would put every building somewhere plausible and
    /// wrong.
    pub site: (f64, f64),
    /// Box kilometres: east, north, and **height above the ground beneath**,
    /// which is the frame `squallar_buildings::prism` authors in.
    pub positions: Vec<[f32; 3]>,
    /// One unit outward normal per position.
    pub normals: Vec<[f32; 3]>,
    /// Triangles, counter-clockwise seen from outside the solid.
    pub indices: Vec<u32>,
}

/// A resampled height field, as the renderer needs it.
///
/// **A plain carrier, not `squallar_elevation::HeightField`**, and the reason
/// is a dependency rather than a preference: this crate does not declare
/// `squallar-elevation` and `squallar-volumetric`'s charter forbids it
/// declaring one (`squallar-volumetric/tests/charter.rs` pins the normal
/// dependency set by name). The encoding travels **as data** — `base_m` and
/// `quantum_m` ride here rather than being re-spelled — so there is still
/// exactly one definition of what a `u16` sample means, and it is the
/// elevation crate's. Whoever fills this in is where that is asserted.
#[derive(Clone, Debug, PartialEq)]
pub struct GroundHeightField {
    /// Distinguishes one field from the next on the GPU, where the box's
    /// `f64` ranges cannot go. Monotonic per producer; equality is what says
    /// "this is the field already uploaded".
    pub id: u64,
    /// The site the box's kilometres are measured from, degrees north and
    /// east — the same anchor the volume grid carries.
    pub site: (f64, f64),
    /// The field's own footprint, kilometres east of the site.
    pub x_km: (f64, f64),
    /// The field's own footprint, kilometres north of the site.
    pub y_km: (f64, f64),
    /// Posts along each axis. `samples.len()` is their product.
    pub posts: [u32; 2],
    /// One `u16` per post, row-major from the south-west post:
    /// `height_m = base_m + sample * quantum_m`.
    pub samples: Arc<Vec<u16>>,
    /// The encoding's zero, metres.
    pub base_m: f64,
    /// The encoding's step, metres.
    pub quantum_m: f64,
    /// The lowest and highest metres in `samples`, computed **where the field
    /// was built** rather than per frame: the maximum is what the box's ceiling
    /// lane wants, and folding a quarter of a million samples on the frame
    /// thread to find it is exactly the kind of "it runs rarely" this
    /// repository does not accept.
    pub range_m: (f64, f64),
}

/// What the painter answered.
pub enum VolumePaint {
    /// Draw this. The payload is opaque here on purpose — see the module doc —
    /// but what it is a picture *of* is not, because the caption has to say so.
    Callback {
        payload: Arc<dyn Any + Send + Sync>,
        showing: Showing,
    },
    /// Nothing to draw, and why not, in a sentence fit for the pane's centre.
    Empty(String),
}

/// What a [`VolumePaint::Callback`] is a picture of — the two facts a caption
/// cannot get right from the pane's own state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Showing {
    /// Kilometres across one cell of the grid actually on screen, east–west
    /// and north–south, or `None` when the grid has no horizontal extent to
    /// divide (impossible for a built grid; answered rather than unwrapped
    /// because a caption is not a place to panic).
    pub cell_km: Option<(f32, f32)>,
    /// The grid on screen was built for a different box, and a build for the
    /// box the pane asked for has not landed yet.
    pub stale: bool,
    /// The grid does not reach the edges of the drawn box: real data in the
    /// middle and nothing outside it. Only ever true alongside `stale`, and
    /// only when the zoom was *outwards*.
    pub partial: bool,
}

impl Showing {
    /// The grid on screen is the one the pane asked for.
    pub const SETTLED: Self = Self {
        cell_km: None,
        stale: false,
        partial: false,
    };
}

/// Something that can turn a 3D pane's state into a paint callback.
pub trait VolumePainter: Send + Sync {
    /// Produce this frame's payload for one pane, or say why there is none.
    fn paint(&self, frame: &VolumeFrameState) -> VolumePaint;

    /// The palette the pane's grid carries — 1024 bytes of straight RGBA, one
    /// entry per index — or `None` while no grid is in hand.
    fn palette(&self, _pane_idx: usize, _target: &VolumeTarget) -> Option<Vec<u8>> {
        None
    }

    /// The full extent in kilometres, each axis, of the box the pane's grid was
    /// actually resampled over — or `None` while no grid is in hand.
    fn box_size_km(&self, _pane_idx: usize, _target: &VolumeTarget) -> Option<[f32; 3]> {
        None
    }

    /// Cells along the grid's horizontal axes, for the caption's km-per-cell —
    /// or `None` while no grid is in hand.
    fn grid_cells_across(&self, _pane_idx: usize, _target: &VolumeTarget) -> Option<usize> {
        None
    }
}

/// The two things the raymarch's uniform block needs from the camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeView {
    /// Clip space to box space, column-major.
    pub box_from_clip: Mat4,
    /// Box space to clip space — the direction geometry is drawn through,
    /// built **forward** as `clip_from_view · view_from_world ·
    /// world_from_box`, never by inverting [`Self::box_from_clip`].
    ///
    /// Every factor has a closed form, so no general 4x4 inverse is taken
    /// anywhere. What is measured here rather than argued is the pair's own
    /// agreement, by `the_forward_and_backward_cameras_are_exact_inverses`:
    /// a box point survives the trip out through one and back through the
    /// other to under 1e-3 box units, and the two matrices' product is the
    /// identity to what `f32` allows over factors this size. The residual of
    /// the inverted alternative is **not** measured here — the plan's 2e-5
    /// figure for it comes from a spike, not from this tree.
    pub clip_from_box: Mat4,
    /// The perspective eye, in box space.
    pub eye_in_box: [f32; 3],
    /// Where the eye is in world kilometres, relative to the box centre. Not
    /// read by the shader; returned because it is the one intermediate a test
    /// or a readout would otherwise have to re-derive.
    pub eye_km: [f32; 3],
}

/// Build this frame's view, or `None` for a box or a viewport that cannot be
/// looked at.
pub fn view_for(camera: OrbitCamera, box_size_km: [f32; 3], aspect: f32) -> Option<VolumeView> {
    let radius = framing_radius_km(box_size_km);
    let depth = half_diagonal(exaggerated_box_km(camera, box_size_km));
    let distance = camera.eye_distance() * radius;
    build_view(
        camera,
        box_size_km,
        aspect,
        NEAR_IN_FRAMING_RADII * radius,
        distance + FAR_MARGIN_IN_HALF_DIAGONALS * depth,
    )
}

/// The box as the camera sees it: the true extent with the vertical axis
/// stretched by [`OrbitCamera::vertical_exaggeration`].
pub fn exaggerated_box_km(camera: OrbitCamera, box_size_km: [f32; 3]) -> [f32; 3] {
    [
        box_size_km[0],
        box_size_km[1],
        box_size_km[2] * camera.vertical_exaggeration(),
    ]
}

/// The length [`OrbitCamera::eye_distance`] is a multiple of: half the diagonal
/// of the **square of side `north`** the box stands on, `north / √2`.
fn framing_radius_km(box_size_km: [f32; 3]) -> f32 {
    half_diagonal([box_size_km[1], box_size_km[1], 0.0])
}

/// The standoff at which a 3D pane draws the ground at exactly the scale its
/// plan pane draws it at — [`OrbitCamera`]'s default, derived here rather than
/// chosen.
/// A plan pane `H` points tall shows `N` kilometres of north–south ground over
/// that height, because the box is cut from the pane's own rectangle: `H / N`
/// points to the kilometre.
/// ```text
/// (H/2) / (d · tan(fov/2)) = H / N   ⇒   d = N / (2 · tan(fov/2))
/// ```
pub fn eye_distance_for_plan_scale() -> f32 {
    std::f32::consts::SQRT_2 / (2.0 * (0.5 * FOV_Y_DEG.to_radians()).tan())
}

/// Half the length of the box's space diagonal — the depth the far plane is
/// measured in, and the framing radius' own arithmetic.
fn half_diagonal(box_size_km: [f32; 3]) -> f32 {
    0.5 * (box_size_km[0] * box_size_km[0]
        + box_size_km[1] * box_size_km[1]
        + box_size_km[2] * box_size_km[2])
        .sqrt()
}

/// Kilometres per degree of longitude at the equator.
const KM_PER_DEGREE_LON_AT_EQUATOR: f64 = squallar_geo::KM_PER_DEGREE_LAT;

/// How much the 3D view magnifies the ground it samples out of the pane mirror,
/// at the pivot's own depth. Dimensionless; 1.0 means one mirror texel per
/// screen pixel and nothing is being stretched.
pub fn floor_magnification(
    camera: OrbitCamera,
    box_size_km: [f32; 3],
    pane_height_points: f32,
    points_per_degree_lon: f64,
    site_lat_deg: f64,
) -> Option<f32> {
    if pane_height_points <= 0.0
        || !pane_height_points.is_finite()
        || !points_per_degree_lon.is_finite()
    {
        return None;
    }
    let distance_km = camera.eye_distance() * framing_radius_km(box_size_km);
    if distance_km <= 0.0 || !distance_km.is_finite() {
        return None;
    }
    let km_per_point_3d =
        2.0 * distance_km * (0.5 * FOV_Y_DEG.to_radians()).tan() / pane_height_points;
    if km_per_point_3d <= 0.0 || !km_per_point_3d.is_finite() {
        return None;
    }
    let km_per_degree_lon = KM_PER_DEGREE_LON_AT_EQUATOR * site_lat_deg.to_radians().cos();
    let points_per_km_2d = points_per_degree_lon.abs() / km_per_degree_lon;
    if points_per_km_2d <= 0.0 || !points_per_km_2d.is_finite() {
        return None;
    }
    let magnification = 1.0 / (km_per_point_3d * points_per_km_2d as f32);
    magnification.is_finite().then_some(magnification)
}

/// Where the camera is aimed, in world kilometres relative to the box's centre.
fn pivot_km(camera: OrbitCamera, box_size_km: [f32; 3]) -> [f32; 3] {
    let stretched = exaggerated_box_km(camera, box_size_km);
    let pivot = camera.pivot();
    [
        pivot[0] * 0.5 * stretched[0],
        pivot[1] * 0.5 * stretched[1],
        pivot[2] * 0.5 * stretched[2],
    ]
}

/// What a drag of `drag_points` screen points should add to
/// [`OrbitCamera::pivot`], in the box-fraction units the pivot is stored in.
pub fn pan_for_drag(
    camera: OrbitCamera,
    box_size_km: [f32; 3],
    viewport_height_points: f32,
    drag_points: [f32; 2],
) -> Option<[f32; 3]> {
    if !box_size_km.iter().all(|s| s.is_finite() && *s > 0.0) {
        return None;
    }
    if !viewport_height_points.is_finite() || viewport_height_points <= 0.0 {
        return None;
    }
    if !drag_points.iter().all(|d| d.is_finite()) {
        return None;
    }

    let stretched = exaggerated_box_km(camera, box_size_km);
    let distance = camera.eye_distance() * framing_radius_km(box_size_km);

    let eye = orbit_eye_km(camera, distance);
    let forward = normalize([-eye[0], -eye[1], -eye[2]])?;
    let right = normalize(cross(forward, [0.0, 0.0, 1.0]))?;
    let up = cross(right, forward);

    let km_per_point =
        2.0 * distance * (0.5 * FOV_Y_DEG.to_radians()).tan() / viewport_height_points;

    let along_right = -drag_points[0] * km_per_point;
    let along_up = drag_points[1] * km_per_point;

    let mut pan = [0.0f32; 3];
    for (axis, slot) in pan.iter_mut().enumerate() {
        let world = right[axis] * along_right + up[axis] * along_up;
        *slot = world / (0.5 * stretched[axis]);
    }
    pan.iter().all(|p| p.is_finite()).then_some(pan)
}

/// [`view_for`] with the frustum's depth range supplied rather than derived.
fn build_view(
    camera: OrbitCamera,
    box_size_km: [f32; 3],
    aspect: f32,
    near: f32,
    far: f32,
) -> Option<VolumeView> {
    if !box_size_km.iter().all(|s| s.is_finite() && *s > 0.0) {
        return None;
    }
    if !aspect.is_finite() || aspect <= 0.0 {
        return None;
    }

    let stretched = exaggerated_box_km(camera, box_size_km);
    if !stretched.iter().all(|s| s.is_finite() && *s > 0.0) {
        return None;
    }
    let distance = camera.eye_distance() * framing_radius_km(box_size_km);

    let orbit_offset = orbit_eye_km(camera, distance);
    let pivot = pivot_km(camera, box_size_km);
    let eye_km = [
        pivot[0] + orbit_offset[0],
        pivot[1] + orbit_offset[1],
        pivot[2] + orbit_offset[2],
    ];

    let forward = normalize([-orbit_offset[0], -orbit_offset[1], -orbit_offset[2]])?;
    let right = normalize(cross(forward, [0.0, 0.0, 1.0]))?;
    let up = cross(right, forward);

    let view_from_clip = inverse_perspective(FOV_Y_DEG, aspect, near, far)?;
    let world_from_view = camera_basis(right, up, forward, eye_km);
    let box_from_world = box_from_world(stretched);

    let box_from_clip = multiply(box_from_world, multiply(world_from_view, view_from_clip));

    // The same three stages the other way round, each built rather than
    // inverted: `perspective` is the analytic twin of `inverse_perspective`,
    // `view_from_world` is the camera basis transposed with the eye subtracted
    // in that basis, and `world_from_box` is the scale and translate
    // `box_from_world` undoes. Composed forward, so the ground mesh and the
    // march cannot disagree about the camera by a residual.
    let clip_from_view = perspective(FOV_Y_DEG, aspect, near, far)?;
    let view_from_world = camera_basis_inverse(right, up, forward, eye_km);
    let world_from_box = world_from_box(stretched);

    let clip_from_box = multiply(clip_from_view, multiply(view_from_world, world_from_box));

    Some(VolumeView {
        box_from_clip,
        clip_from_box,
        eye_in_box: to_box(eye_km, stretched),
        eye_km,
    })
}

/// The orbit's offset in world kilometres: where the eye sits **relative to the
/// pivot**, which is the box's centre until the view is panned.
pub fn orbit_eye_km(camera: OrbitCamera, distance: f32) -> [f32; 3] {
    let yaw = camera.yaw_deg().to_radians();
    let pitch = camera.pitch_deg().to_radians();
    [
        distance * pitch.cos() * yaw.sin(),
        distance * pitch.cos() * yaw.cos(),
        distance * pitch.sin(),
    ]
}

/// A point in world kilometres as a point in box space.
fn to_box(p_km: [f32; 3], box_size_km: [f32; 3]) -> [f32; 3] {
    [
        p_km[0] / box_size_km[0] + 0.5,
        p_km[1] / box_size_km[1] + 0.5,
        p_km[2] / box_size_km[2] + 0.5,
    ]
}

/// Scale by the box's extent and shift its centre to `(0.5, 0.5, 0.5)`.
fn box_from_world(box_size_km: [f32; 3]) -> Mat4 {
    [
        [1.0 / box_size_km[0], 0.0, 0.0, 0.0],
        [0.0, 1.0 / box_size_km[1], 0.0, 0.0],
        [0.0, 0.0, 1.0 / box_size_km[2], 0.0],
        [0.5, 0.5, 0.5, 1.0],
    ]
}

/// [`box_from_world`] undone in closed form: shift the unit cube's centre to
/// the origin, then scale by the box's extent.
fn world_from_box(box_size_km: [f32; 3]) -> Mat4 {
    [
        [box_size_km[0], 0.0, 0.0, 0.0],
        [0.0, box_size_km[1], 0.0, 0.0],
        [0.0, 0.0, box_size_km[2], 0.0],
        [
            -0.5 * box_size_km[0],
            -0.5 * box_size_km[1],
            -0.5 * box_size_km[2],
            1.0,
        ],
    ]
}

/// The camera-to-world matrix, built rather than inverted.
fn camera_basis(right: [f32; 3], up: [f32; 3], forward: [f32; 3], eye: [f32; 3]) -> Mat4 {
    [
        [right[0], right[1], right[2], 0.0],
        [up[0], up[1], up[2], 0.0],
        [-forward[0], -forward[1], -forward[2], 0.0],
        [eye[0], eye[1], eye[2], 1.0],
    ]
}

/// The camera-to-world matrix undone in closed form: the rotation is
/// orthonormal, so its inverse is its transpose, and the translation is the eye
/// resolved in that same basis and negated. No general inverse is taken.
fn camera_basis_inverse(right: [f32; 3], up: [f32; 3], forward: [f32; 3], eye: [f32; 3]) -> Mat4 {
    let back = [-forward[0], -forward[1], -forward[2]];
    let dot = |a: [f32; 3]| a[0] * eye[0] + a[1] * eye[1] + a[2] * eye[2];
    [
        [right[0], up[0], back[0], 0.0],
        [right[1], up[1], back[1], 0.0],
        [right[2], up[2], back[2], 0.0],
        [-dot(right), -dot(up), -dot(back), 1.0],
    ]
}

/// wgpu's right-handed perspective, whose clip `z` runs `0..1` — the forward
/// map [`inverse_perspective`] undoes.
fn perspective(fov_y_deg: f32, aspect: f32, near: f32, far: f32) -> Option<Mat4> {
    if !(near.is_finite() && far.is_finite() && near > 0.0 && far > near) {
        return None;
    }
    let f = 1.0 / (0.5 * fov_y_deg.to_radians()).tan();
    if !f.is_finite() || f <= 0.0 {
        return None;
    }
    let mut m = [[0.0f32; 4]; 4];
    m[0][0] = f / aspect;
    m[1][1] = f;
    m[2][2] = far / (near - far);
    m[2][3] = -1.0;
    m[3][2] = far * near / (near - far);
    Some(m)
}

/// The analytic inverse of wgpu's right-handed perspective, whose clip `z` runs
/// `0..1`.
fn inverse_perspective(fov_y_deg: f32, aspect: f32, near: f32, far: f32) -> Option<Mat4> {
    if !(near.is_finite() && far.is_finite() && near > 0.0 && far > near) {
        return None;
    }
    let f = 1.0 / (0.5 * fov_y_deg.to_radians()).tan();
    if !f.is_finite() || f <= 0.0 {
        return None;
    }
    let mut m = [[0.0f32; 4]; 4];
    m[0][0] = aspect / f;
    m[1][1] = 1.0 / f;
    m[3][2] = -1.0;
    m[2][3] = 1.0 / far - 1.0 / near;
    m[3][3] = 1.0 / near;
    Some(m)
}

/// `a · b`, column-major throughout: `(a·b)[c][r] = Σ a[k][r] · b[c][k]`.
fn multiply(a: Mat4, b: Mat4) -> Mat4 {
    let mut out = [[0.0f32; 4]; 4];
    for (c, column) in out.iter_mut().enumerate() {
        for (r, slot) in column.iter_mut().enumerate() {
            *slot = (0..4).map(|k| a[k][r] * b[c][k]).sum();
        }
    }
    out
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// `v` scaled to unit length, or `None` if it is too short to have a direction.
fn normalize(v: [f32; 3]) -> Option<[f32; 3]> {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    (length.is_finite() && length > MIN_BASIS_LENGTH)
        .then(|| [v[0] / length, v[1] / length, v[2] / length])
}

/// A painter that answers every frame with a payload of a type nothing can
/// draw, for tests that need the paint *path* without a GPU.
#[cfg(test)]
pub(crate) struct StubVolumePainter {
    /// What every call answers with.
    pub(crate) answer_empty: Option<String>,
    /// What every painting call claims to be a picture of.
    pub(crate) answer_showing: Showing,
    /// Every frame this painter has been asked about, in call order.
    pub(crate) seen: std::sync::Mutex<Vec<VolumeFrameState>>,
}

#[cfg(test)]
impl StubVolumePainter {
    pub(crate) fn painting() -> Self {
        Self {
            answer_empty: None,
            answer_showing: Showing::SETTLED,
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Painting, but standing in for a build that has not landed — the state a
    /// pane is in for the frames just after a zoom.
    pub(crate) fn standing_in(showing: Showing) -> Self {
        Self {
            answer_empty: None,
            answer_showing: showing,
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn empty(why: &str) -> Self {
        Self {
            answer_empty: Some(why.to_owned()),
            answer_showing: Showing::SETTLED,
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[cfg(test)]
impl VolumePainter for StubVolumePainter {
    fn paint(&self, frame: &VolumeFrameState) -> VolumePaint {
        self.seen
            .lock()
            .expect("stub painter mutex")
            .push(frame.clone());
        match &self.answer_empty {
            Some(why) => VolumePaint::Empty(why.clone()),
            None => VolumePaint::Callback {
                payload: Arc::new(StubPayload),
                showing: self.answer_showing,
            },
        }
    }

    /// A painting stub has a grid, so it answers with one's cell count.
    fn grid_cells_across(&self, _pane_idx: usize, _target: &VolumeTarget) -> Option<usize> {
        self.answer_empty.is_none().then(|| {
            squallar_radar::voxel::shape_for_budget(squallar_radar::voxel::DESKTOP_SHAPE, 2048).nx
        })
    }
}

/// The stub's payload type. Nothing downcasts to it, which is the point.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct StubPayload;

#[cfg(test)]
mod tests;
