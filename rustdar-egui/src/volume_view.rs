//! The seam between a 3D pane and whatever can actually draw one, plus every
//! matrix that turns an [`OrbitCamera`] into the two numbers the raymarch reads.
//!
//! # Why the bridge is `Arc<dyn Any + Send + Sync>`
//!
//! This crate must gain no wgpu dependency — that is what keeps the whole UI
//! headless-testable, and it is a hard constraint of the work package rather
//! than a preference. So the value a 3D pane hands to egui cannot be typed here:
//! it is whatever `egui_wgpu` wants inside an `epaint::PaintCallback`, and only
//! the frontend can build one.
//!
//! `epaint::PaintCallback` has two **public** fields, `rect` and `callback:
//! Arc<dyn Any + Send + Sync>`, so this crate can construct one directly. That
//! is the route, and it is not the obvious one: `egui_wgpu::Callback`'s own
//! field is private and its only constructor
//! (`Callback::new_paint_callback(rect, cb)`) hands back a finished
//! `PaintCallback`. A crate that cannot name `egui_wgpu` therefore cannot make
//! the payload — it can only be given one, which is exactly what
//! [`VolumePainter`] is for.
//!
//! # Why the painter is asked *during* the UI pass
//!
//! [`VolumePainter::paint`] is called from inside the pane loop, with the camera
//! as it stands after this frame's drag has been applied. Building the payload
//! before `Gui::ui` runs would be simpler and would put the orbit **one frame
//! behind the pointer** — which does not read as a bug, it reads as input lag,
//! and it gets "fixed" by tuning drag sensitivity instead of by fixing the
//! order. The painter object is long-lived; the payload is not.
//!
//! # Why a wrong payload is the dangerous case
//!
//! `egui_wgpu`'s renderer downcasts the `Arc<dyn Any>` it is given. A payload of
//! the wrong type is one `log::warn!` in `prepare` and a **silent `continue`**
//! in `paint` — a pane that draws nothing, with no error on screen and no
//! failing test. That is why the frontend owns a test that its own payload
//! downcasts, and why [`StubVolumePainter`] is documented as exercising
//! everything *except* that.
//!
//! # The camera math
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
//! `view_from_clip` is the analytic inverse of the perspective matrix. A general
//! inverse would be forty lines of arithmetic whose failure mode is a
//! plausible-looking picture.
//!
//! # Vertical exaggeration, and where it is and is not applied
//!
//! At true proportions the default box is 460 km wide by 18 km tall — **25.6:1**
//! — and even a tight 40 km one is 2.2:1: either reads as a sheet of paper. So [`OrbitCamera::vertical_exaggeration`] stretches
//! it, and it is a knob with a number on it rather than a silent constant.
//!
//! It is applied in exactly one place: [`exaggerated_box_km`], which every
//! function here routes its box through. Scaling the box's `z` **extent** rather
//! than the geometry inside it is what makes the stretch a pure change of the
//! camera's world:
//!
//! * `box_from_world` divides `z` by `size_z · ex`, so a cell that sat at box
//!   `z = 0.4` still sits at box `z = 0.4`. The volume texture is untouched and
//!   the raymarch is unaware the knob exists.
//! * The eye, the half-diagonal, the near and far planes and the pivot are all
//!   measured against the same stretched box, so the framing is unchanged as the
//!   knob turns: a box at `eye_distance = 2.5` fills the same fraction of the
//!   pane at 1× and at 12×.
//!
//! **Nothing the pane reports about height goes through it.** The stretch is
//! geometry; the readout reads `VoxelGrid::z_range_km_msl` and is in real kft
//! MSL at every exaggeration. That separation is the whole reason the knob is
//! defensible — an exaggerated view is a drawing convention, an exaggerated
//! *number* would be a fabricated measurement.
//!
//! # The pivot, and why panning is scaled to depth
//!
//! [`OrbitCamera::pivot`] is the point the orbit turns about, and
//! [`pan_for_drag`] is what a drag on the pane does to it. The scaling there is
//! the whole of whether panning feels right: the pivot is moved by the world
//! distance one screen point spans **at the pivot's own depth**, so the point of
//! the box under the pointer stays under the pointer. Any fixed rate instead —
//! a constant fraction of the box per point, say — attaches the box to the mouse
//! rather than to the ground, and it goes wrong in opposite directions at the two
//! ends of the zoom: sluggish when zoomed in, and flying off the pane when zoomed
//! out.

use std::any::Any;
use std::sync::Arc;

use crate::pane::{OrbitCamera, VolumeTarget};

/// Vertical field of view of the volume camera, degrees.
///
/// Narrower than a first-person 60–90°: the subject is a box being inspected
/// from outside, and a wide lens on a 240 km box bends the storm's edges away
/// from the viewer in a way that reads as a fisheye rather than as perspective.
const FOV_Y_DEG: f32 = 40.0;

/// Near plane, in multiples of the box's half-diagonal.
///
/// Both planes are **cosmetic here** and that is worth saying, because it looks
/// as though they should matter. The shader only ever unprojects at `depth =
/// 1.0` and uses the result for a *direction*; the far distance cancels in the
/// normalisation and the near distance cancels out of the analytic inverse at
/// that depth (`B/(A+1) = far` exactly). They are chosen to be sane rather than
/// tuned, and a test pins that changing them does not move a ray.
const NEAR_IN_HALF_DIAGONALS: f32 = 0.02;
/// Far plane, in multiples of the box's half-diagonal, beyond the eye. See
/// [`NEAR_IN_HALF_DIAGONALS`].
const FAR_MARGIN_IN_HALF_DIAGONALS: f32 = 2.0;

/// Shortest cross product the camera basis will accept before calling itself
/// degenerate. Reached only if pitch is at ±90°, which [`OrbitCamera`] does not
/// allow — so this is the guard for a caller who built a camera another way.
const MIN_BASIS_LENGTH: f32 = 1e-6;

/// A column-major 4×4, `m[column][row]` — WGSL's convention and std140's, so
/// the columns go out in order with no transpose.
pub type Mat4 = [[f32; 4]; 4];

/// Web Mercator's `y` for a latitude in radians: `ln(tan(π/4 + φ/2))`.
fn mercator_y(lat_rad: f64) -> f64 {
    (std::f64::consts::FRAC_PI_4 + lat_rad * 0.5).tan().ln()
}

/// Web Mercator's `y` for a latitude in **degrees**.
///
/// Public because the renderer on the other side of this seam has to evaluate
/// exactly this function to turn a [`MapPaneGeo`] into texture coordinates,
/// and a second spelling of it there is precisely the drift this seam exists
/// to prevent.
pub fn mercator_y_of_lat(lat_deg: f64) -> f64 {
    mercator_y(lat_deg.to_radians())
}

/// How a 2D map pane's own render maps geography onto the frame — the affine a
/// 3D pane needs in order to find its ground inside a copy of that render.
///
/// # What this is for
///
/// The 3D view's map floor is not a picture built for the floor. It is the
/// **source pane's own render**, copied into an offscreen "mirror" texture and
/// sampled by the raymarch. That makes the floor Web Mercator — whatever the
/// 2D pane draws, in whatever projection the 2D pane draws it in — while the
/// voxel box stays a tangent plane in kilometres east and north of the site,
/// because beam geometry is kilometres and Mercator's scale factor varies
/// ~6.6% across a 460 km box at mid-latitude, which would stretch storms.
///
/// So *something* has to carry the one conversion, and this is it: the pane's
/// projection, reduced to the four numbers a linear reprojection needs. It is
/// four numbers rather than a `walkers::Projector` because this seam's whole
/// point is that the renderer gains no dependency on the map — and because the
/// reduction is exact, not an approximation: Web Mercator's screen `x` is
/// linear in longitude and its screen `y` is linear in Mercator `y`, so an
/// affine in those two variables reproduces the projector everywhere on the
/// pane, not merely near the anchor.
///
/// # Why an anchor rather than an origin
///
/// The affine is measured from the pane's own centre rather than from
/// longitude 0 and the equator, so nothing downstream has to cancel a number
/// near −93° against another one to land on a texture coordinate near 0.5. The
/// quantities that reach `f32` stay small.
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
    ///
    /// Exact rather than a local linearisation — see the type doc.
    pub fn project(&self, lat_deg: f64, lon_deg: f64) -> egui::Pos2 {
        let dx = (lon_deg - self.anchor_lon) * self.points_per_degree_lon;
        let dy = (mercator_y_of_lat(lat_deg) - mercator_y_of_lat(self.anchor_lat))
            * self.points_per_mercator_y;
        egui::pos2(self.anchor.x + dx as f32, self.anchor.y + dy as f32)
    }
}

/// Everything the painter is told about one 3D pane on one frame.
///
/// Deliberately a record with no methods: it is the whole of the contract
/// between a pane and a renderer, so anything it does not carry is something
/// the renderer must not depend on.
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
    /// Whether this pane wants the map floor drawn under the volume.
    ///
    /// The positive form of `VolumePane::hide_floor`, resolved at the one
    /// place the pane's state is read. The renderer may still draw no floor —
    /// none may be in hand yet — but it must never draw one against this.
    pub floor: bool,
    /// The Mercator affine of the 2D pane this pane's region was dragged on,
    /// as that pane last drew itself.
    ///
    /// This is the whole of the floor's registration. `None` — no
    /// `source_pane`, or a source pane that is not a map — means the renderer
    /// has nothing to reproject through and must draw no floor, whatever
    /// [`Self::floor`] says.
    ///
    /// **"As that pane last drew itself"** is exact rather than loose: panes
    /// render in index order, so a 3D pane sitting *before* its source in that
    /// order reads the previous frame's affine. The mirror it samples is
    /// always this frame's picture, so during a pan of the source map the
    /// floor can trail the pane it mirrors by one frame's pan delta. That is
    /// bounded by one frame; the alternative is a second layout pass over every
    /// pane purely to hoist four numbers, which is a large cost for an artefact
    /// nobody can see at 60 Hz.
    ///
    /// Self-correction needs a *next frame*, though, and the app returns to
    /// `ControlFlow::Wait` when idle. A **discontinuous** jump — a site switch,
    /// jump-to-live, a layout change — puts a whole-continent offset on that
    /// frame rather than a pan delta, and if it is the last frame requested the
    /// misregistration is what stays on screen. So `render_panes` asks for a
    /// repaint whenever a map pane's freshly recorded affine differs from the
    /// one an earlier-indexed 3D pane consumed. A steady map asks for nothing.
    pub source: Option<MapPaneGeo>,
    /// The user's Volume Alpha curve for this pane's product, or `None` for
    /// an untouched editor.
    ///
    /// `None` is a contract, not a shorthand: it obliges the renderer to
    /// upload the grid's own LUT **bit-exactly**, so a user who never opens
    /// the editor renders exactly what the palette says. `Some` obliges it to
    /// replace the LUT's alpha channel with the curve — colours stay the
    /// palette's — and to re-anchor the march's skip threshold at the curve's
    /// own fade boundary rather than the palette's.
    pub alpha: Option<crate::volume_alpha::AlphaCurve>,
    /// How the pane draws its volume: the lit accumulation or an isosurface.
    ///
    /// A *drawing* property, which is why it rides the frame and not the
    /// [`VolumeTarget`]: the target keys what is sampled, and toggling the
    /// mode must not rebuild an 8 MiB grid — the same doctrine that keeps the
    /// camera off the target.
    pub view_mode: crate::pane::VolumeViewMode,
    /// The isosurface threshold for this pane's product, in the product's own
    /// units ([`rustdar_radar::voxel::iso_shape`] says what the number
    /// means). Read only in isosurface mode; the renderer translates it into
    /// index space against the grid's own ramp.
    pub iso_threshold: f32,
}

/// What the painter answered.
///
/// The empty arm carries its reason as a `String` rather than being a bare
/// `None`, because every way this can be empty is a different thing for the
/// user to do: wait for a volume, pick a different moment, use a different
/// machine. A 3D pane that draws an empty box says nothing; one that says *why*
/// the box is empty is the difference between a feature and a bug report.
pub enum VolumePaint {
    /// Draw this. Opaque here on purpose — see the module doc.
    Callback(Arc<dyn Any + Send + Sync>),
    /// Nothing to draw, and why not, in a sentence fit for the pane's centre.
    Empty(String),
}

/// Something that can turn a 3D pane's state into a paint callback.
///
/// `Send + Sync` because the `Gui` holds one and egui's own callback payloads
/// are required to be, and because the implementation on the other side of this
/// trait owns GPU handles that a browser cannot share across threads — a bound
/// that is trivially satisfiable today and would be a silent rewrite to add
/// later.
pub trait VolumePainter: Send + Sync {
    /// Produce this frame's payload for one pane, or say why there is none.
    fn paint(&self, frame: &VolumeFrameState) -> VolumePaint;

    /// The palette the pane's grid carries — 1024 bytes of straight RGBA, one
    /// entry per index — or `None` while no grid is in hand.
    ///
    /// This is the Volume Alpha editor's one window into the renderer: the
    /// palette strip it draws, and the alpha channel it seeds an untouched
    /// curve from, are the **grid's own table**, read through the same
    /// pane-scoped lookup `paint` uses. Reading it anywhere else — a second
    /// copy of the colour tables in the UI crate — would be a copy to keep in
    /// step, and the day they disagreed the editor would show a curve over one
    /// palette while the volume rendered through another.
    ///
    /// Defaulted to `None` so a painter that cannot answer (the test stub, a
    /// future headless painter) is an editor that says "waiting for the
    /// volume" rather than a build break.
    fn palette(&self, _pane_idx: usize, _target: &VolumeTarget) -> Option<Vec<u8>> {
        None
    }
}

/// The two things the raymarch's uniform block needs from the camera.
///
/// Returned as plain arrays rather than as the frontend's `VolumeUniform`
/// because this crate cannot name that type — and should not: the rest of that
/// block is transfer-function state that has nothing to do with where the eye
/// is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeView {
    /// Clip space to box space, column-major.
    pub box_from_clip: Mat4,
    /// The perspective eye, in box space.
    ///
    /// A *perspective* eye specifically: rays are cast from this point, which is
    /// what lets the shader clamp the slab entry to zero and behave when the
    /// camera is inside the box. An orthographic camera has no such point and
    /// would need a different derivation throughout, not a different value here.
    pub eye_in_box: [f32; 3],
    /// Where the eye is in world kilometres, relative to the box centre. Not
    /// read by the shader; returned because it is the one intermediate a test
    /// or a readout would otherwise have to re-derive.
    pub eye_km: [f32; 3],
}

/// Build this frame's view, or `None` for a box or a viewport that cannot be
/// looked at.
///
/// Refuses rather than clamps, for the reason [`OrbitCamera::nudge`] gives at
/// length: every quantity here reaches a `1.0 / x`, and a clamp on the way in
/// would launder a non-finite input into a matrix full of `NaN` that the GPU
/// accepts, renders as an empty pane, and reports nowhere.
///
/// * `box_size_km` — the box's full extent along each axis. Every component
///   must be finite and strictly positive; a zero axis divides by zero in
///   `box_from_world` and a negative one mirrors the volume.
/// * `aspect` — width over height of the target being rendered into, finite and
///   strictly positive. A pane one frame wide during a divider drag is the
///   realistic way this arrives as zero.
pub fn view_for(camera: OrbitCamera, box_size_km: [f32; 3], aspect: f32) -> Option<VolumeView> {
    // No validation here on purpose: every check lives in `build_view`, which
    // this delegates to. A copy of the box check here would be unreachable —
    // mutation testing found exactly that, by deleting it and seeing nothing
    // fail — and an unreachable guard is one that can rot into disagreement with
    // the reachable one.
    //
    // The half-diagonal is taken from the *stretched* box, which is what keeps
    // the framing fixed as the exaggeration turns: `eye_distance` is in
    // half-diagonals, so a taller box is looked at from proportionally further
    // out and fills the same fraction of the pane.
    let half_diagonal = half_diagonal(exaggerated_box_km(camera, box_size_km));
    let distance = camera.eye_distance() * half_diagonal;
    build_view(
        camera,
        box_size_km,
        aspect,
        NEAR_IN_HALF_DIAGONALS * half_diagonal,
        distance + FAR_MARGIN_IN_HALF_DIAGONALS * half_diagonal,
    )
}

/// The box as the camera sees it: the true extent with the vertical axis
/// stretched by [`OrbitCamera::vertical_exaggeration`].
///
/// The single place the knob is applied. Everything else here — the eye, the
/// pivot, the frustum, `box_from_world` — reads this rather than the true box, so
/// there is exactly one line to be wrong and every consumer is wrong or right
/// together.
///
/// The horizontal axes are passed through untouched, which is the definition of
/// a *vertical* exaggeration and worth stating: scaling all three would be a zoom,
/// and a zoom is what `eye_distance` already is.
pub fn exaggerated_box_km(camera: OrbitCamera, box_size_km: [f32; 3]) -> [f32; 3] {
    [
        box_size_km[0],
        box_size_km[1],
        box_size_km[2] * camera.vertical_exaggeration(),
    ]
}

/// Half the length of the box's space diagonal — the unit `eye_distance` and the
/// two frustum planes are measured in.
fn half_diagonal(box_size_km: [f32; 3]) -> f32 {
    0.5 * (box_size_km[0] * box_size_km[0]
        + box_size_km[1] * box_size_km[1]
        + box_size_km[2] * box_size_km[2])
        .sqrt()
}

/// Where the camera is aimed, in world kilometres relative to the box's centre.
///
/// The pivot is stored as a fraction of the box's half-extent, so this is the one
/// multiplication that turns it back into a place. Against the *stretched* box,
/// so that a pivot on the top face stays on the top face as the exaggeration
/// turns.
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
///
/// # The scaling is the feel
///
/// A drag of N points moves the pivot by the world distance N points span **at
/// the pivot's depth** — so the piece of the box under the pointer stays under
/// the pointer, and the box reads as an object being pushed around rather than as
/// a picture being scrubbed. With a perspective camera that distance is
/// `2 · distance · tan(fov/2)` across the viewport's height, which is why this
/// needs the viewport as well as the camera.
///
/// # Signs
///
/// The content follows the pointer, so the *pivot* moves the other way: dragging
/// right carries the box right, which means aiming further left. Both signs are
/// convention rather than arithmetic — a sign error here pans perfectly well and
/// merely feels inverted — so both are pinned by a test.
///
/// `None` for anything that would divide by zero or produce a non-finite offset:
/// a pane with no height, a degenerate box, or a non-finite drag. Refused rather
/// than clamped for the reason [`OrbitCamera::nudge`] gives — though `nudge`
/// re-checks anyway, because this is not the only thing that could ever build a
/// pan.
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
    let distance = camera.eye_distance() * half_diagonal(stretched);

    // The camera basis, from the same eye direction `build_view` uses — so a pan
    // is along the axes the user sees, at every yaw and pitch.
    let eye = orbit_eye_km(camera, distance);
    let forward = normalize([-eye[0], -eye[1], -eye[2]])?;
    let right = normalize(cross(forward, [0.0, 0.0, 1.0]))?;
    let up = cross(right, forward);

    // World kilometres spanned by one screen point at the pivot's depth. The
    // vertical field of view is the one that is fixed, so the height is what this
    // is derived from and the horizontal follows from the same number — which is
    // correct, because screen points are square.
    let km_per_point =
        2.0 * distance * (0.5 * FOV_Y_DEG.to_radians()).tan() / viewport_height_points;

    // Screen y runs down, so a downward drag is a *negative* move along `up`;
    // the content-follows-pointer inversion then makes it positive. The two
    // negations are written out rather than cancelled so the reasoning survives.
    let along_right = -drag_points[0] * km_per_point;
    let along_up = drag_points[1] * km_per_point;

    let mut pan = [0.0f32; 3];
    for (axis, slot) in pan.iter_mut().enumerate() {
        let world = right[axis] * along_right + up[axis] * along_up;
        // Back into fractions of the box's half-extent, which is what the pivot
        // is stored in. The stretched box on every axis, matching `pivot_km`.
        *slot = world / (0.5 * stretched[axis]);
    }
    pan.iter().all(|p| p.is_finite()).then_some(pan)
}

/// [`view_for`] with the frustum's depth range supplied rather than derived.
///
/// Split out for exactly one reason: it is what lets a test build the same view
/// twice at wildly different near and far planes and assert the rays are
/// identical. Doing that by scaling the box instead would change the geometry as
/// well as the frustum, which is a test that cannot see what it is named for.
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

    // Every length below is against the stretched box. See `exaggerated_box_km`:
    // the grid's own coordinates are unchanged, so this is a change to the
    // camera's world and not to the data in it.
    let stretched = exaggerated_box_km(camera, box_size_km);
    if !stretched.iter().all(|s| s.is_finite() && *s > 0.0) {
        return None;
    }
    let distance = camera.eye_distance() * half_diagonal(stretched);

    // The orbit is about the pivot, not about the origin — so the eye is the
    // pivot plus the orbit offset, and the forward direction is still just the
    // orbit offset reversed. That the two stay in step is what keeps the pivot
    // exactly in the middle of the pane at every yaw and pitch.
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

    Some(VolumeView {
        box_from_clip,
        eye_in_box: to_box(eye_km, stretched),
        eye_km,
    })
}

/// The orbit's offset in world kilometres: where the eye sits **relative to the
/// pivot**, which is the box's centre until the view is panned.
///
/// Yaw is a **compass bearing of the eye from the centre**: 0° puts the camera
/// due north of the box looking south, 90° due east. That is what makes
/// [`OrbitCamera`]'s default of 225° the south-west view its documentation
/// claims, and it is the same sense as every other azimuth in this codebase
/// (`beam::site_bearing_range_km`, the sampler's `azimuth_deg`), which is worth
/// more than the alternative convention's slightly tidier trigonometry.
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

/// The camera-to-world matrix, built rather than inverted.
///
/// A look-at matrix is an orthonormal rotation followed by a translation, so its
/// inverse is the basis itself with the eye in the translation column. Writing
/// that down is exact and free; inverting the look-at would be neither.
///
/// The third column is `-forward` because a view space looks down its own `-z`,
/// which is the convention [`inverse_perspective`] is written against.
fn camera_basis(right: [f32; 3], up: [f32; 3], forward: [f32; 3], eye: [f32; 3]) -> Mat4 {
    [
        [right[0], right[1], right[2], 0.0],
        [up[0], up[1], up[2], 0.0],
        [-forward[0], -forward[1], -forward[2], 0.0],
        [eye[0], eye[1], eye[2], 1.0],
    ]
}

/// The analytic inverse of wgpu's right-handed perspective, whose clip `z` runs
/// `0..1`.
///
/// Derived rather than inverted. With `f = 1/tan(fovy/2)`, the forward matrix
/// sends a view point to `(f/aspect · x, f · y, A·z + B, −z)` where
/// `A = far/(near−far)` and `B = near·far/(near−far)`. Solving that back gives
/// four non-zero entries, two of which simplify all the way:
/// `A/B = 1/near` and `1/B = 1/far − 1/near`.
///
/// `None` for a degenerate frustum — a zero or inverted depth range, or a field
/// of view at the limit where `tan` blows up.
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
///
/// **It cannot catch the failure it most looks like it should.** A payload of
/// the wrong type is precisely what `egui_wgpu` swallows — one `log::warn!` in
/// `prepare` and a silent `continue` in `paint` — so a suite built only on this
/// stub proves the callback was pushed and proves nothing about whether it
/// would ever draw. The test that closes that gap lives in `rustdar-frontend`,
/// where the real payload's type is nameable, and it is named in this crate's
/// tests so the pairing is findable from either end.
#[cfg(test)]
pub(crate) struct StubVolumePainter {
    /// What every call answers with.
    pub(crate) answer_empty: Option<String>,
    /// Every frame this painter has been asked about, in call order.
    pub(crate) seen: std::sync::Mutex<Vec<VolumeFrameState>>,
}

#[cfg(test)]
impl StubVolumePainter {
    pub(crate) fn painting() -> Self {
        Self {
            answer_empty: None,
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn empty(why: &str) -> Self {
        Self {
            answer_empty: Some(why.to_owned()),
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
            None => VolumePaint::Callback(Arc::new(StubPayload)),
        }
    }
}

/// The stub's payload type. Nothing downcasts to it, which is the point.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct StubPayload;

#[cfg(test)]
mod tests;
