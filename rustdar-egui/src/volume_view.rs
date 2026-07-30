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
//! # No vertical exaggeration
//!
//! `box_from_world` divides by the box's true extent in kilometres, so a
//! 240 × 240 × 18 km box is drawn as the pancake it is. That is a decision, not
//! an oversight: the volume's whole claim is that it shows real vertical
//! structure, and a silent 13× stretch would make every storm look like a tower.
//! An exaggeration control is a knob to add later, above this function, and it
//! would be a knob with a number on it.

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
    let half_diagonal = 0.5
        * (box_size_km[0] * box_size_km[0]
            + box_size_km[1] * box_size_km[1]
            + box_size_km[2] * box_size_km[2])
            .sqrt();
    let distance = camera.eye_distance() * half_diagonal;
    build_view(
        camera,
        box_size_km,
        aspect,
        NEAR_IN_HALF_DIAGONALS * half_diagonal,
        distance + FAR_MARGIN_IN_HALF_DIAGONALS * half_diagonal,
    )
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

    let half_diagonal = 0.5
        * (box_size_km[0] * box_size_km[0]
            + box_size_km[1] * box_size_km[1]
            + box_size_km[2] * box_size_km[2])
            .sqrt();
    let distance = camera.eye_distance() * half_diagonal;

    let eye_km = orbit_eye_km(camera, distance);

    // Looking at the box's centre, which world space puts at the origin.
    let forward = normalize([-eye_km[0], -eye_km[1], -eye_km[2]])?;
    let right = normalize(cross(forward, [0.0, 0.0, 1.0]))?;
    let up = cross(right, forward);

    let view_from_clip = inverse_perspective(FOV_Y_DEG, aspect, near, far)?;
    let world_from_view = camera_basis(right, up, forward, eye_km);
    let box_from_world = box_from_world(box_size_km);

    let box_from_clip = multiply(box_from_world, multiply(world_from_view, view_from_clip));

    Some(VolumeView {
        box_from_clip,
        eye_in_box: to_box(eye_km, box_size_km),
        eye_km,
    })
}

/// Where the eye sits in world kilometres, relative to the box's centre.
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
mod tests {
    use super::*;

    const BOX_KM: [f32; 3] = [240.0, 240.0, 18.0];

    fn camera(yaw: f32, pitch: f32, distance: f32) -> OrbitCamera {
        OrbitCamera::restore(yaw, pitch, distance).expect("finite camera")
    }

    /// Apply a column-major matrix to a homogeneous point and divide through,
    /// exactly as `unproject` in the shader does.
    fn unproject(m: Mat4, ndc: [f32; 3]) -> [f32; 3] {
        let p = [ndc[0], ndc[1], ndc[2], 1.0];
        let mut out = [0.0f32; 4];
        for (r, slot) in out.iter_mut().enumerate() {
            *slot = (0..4).map(|k| m[k][r] * p[k]).sum();
        }
        [out[0] / out[3], out[1] / out[3], out[2] / out[3]]
    }

    fn direction(view: &VolumeView, ndc: [f32; 2]) -> [f32; 3] {
        let far = unproject(view.box_from_clip, [ndc[0], ndc[1], 1.0]);
        normalize([
            far[0] - view.eye_in_box[0],
            far[1] - view.eye_in_box[1],
            far[2] - view.eye_in_box[2],
        ])
        .expect("a ray with a direction")
    }

    /// The centre of the screen looks at the centre of the box.
    ///
    /// The single strongest end-to-end check available without a GPU: it
    /// exercises all three factors and their multiplication order at once. A
    /// transposed factor, a swapped multiplication order or a sign error in the
    /// basis all move this ray off the centre, and none of them can be seen by
    /// reading the code.
    #[test]
    fn the_centre_of_the_screen_looks_at_the_centre_of_the_box() {
        for (yaw, pitch) in [(0.0, 0.0), (225.0, 25.0), (37.0, -80.0), (359.0, 89.0)] {
            let view = view_for(camera(yaw, pitch, 2.5), BOX_KM, 1.6).expect("a view");
            let ray = direction(&view, [0.0, 0.0]);
            // The centre of box space is (0.5, 0.5, 0.5); the eye is somewhere
            // outside. The ray from eye to centre is the one the middle pixel
            // must cast.
            let wanted = normalize([
                0.5 - view.eye_in_box[0],
                0.5 - view.eye_in_box[1],
                0.5 - view.eye_in_box[2],
            ])
            .expect("a direction to the centre");
            for axis in 0..3 {
                assert!(
                    (ray[axis] - wanted[axis]).abs() < 1e-4,
                    "yaw {yaw} pitch {pitch}: centre ray {ray:?} does not point at the box \
                     centre ({wanted:?})",
                );
            }
        }
    }

    /// Yaw is a compass bearing of the *eye*, so the default camera is to the
    /// south-west of the box exactly as [`OrbitCamera::default`] promises.
    ///
    /// Pins the convention rather than the arithmetic: reversing the sine and
    /// cosine, or negating one of them, still produces a working orbit that
    /// simply spins the wrong way — a defect nobody notices until they compare
    /// against a map.
    #[test]
    fn yaw_is_the_compass_bearing_of_the_eye_from_the_box() {
        let view = view_for(OrbitCamera::default(), BOX_KM, 1.0).expect("a view");
        assert!(
            view.eye_km[0] < 0.0 && view.eye_km[1] < 0.0,
            "the default camera should sit south-west of the box, not at {:?}",
            view.eye_km,
        );
        assert!(view.eye_km[2] > 0.0, "a positive pitch is above the box");

        for (yaw, axis, sign) in [
            (0.0, 1, 1.0),
            (90.0, 0, 1.0),
            (180.0, 1, -1.0),
            (270.0, 0, -1.0),
        ] {
            let view = view_for(camera(yaw, 0.0, 2.0), BOX_KM, 1.0).expect("a view");
            assert!(
                view.eye_km[axis] * sign > 0.0,
                "yaw {yaw} should put the eye on axis {axis} sign {sign}, got {:?}",
                view.eye_km,
            );
        }
    }

    /// The box is *not* stretched to a cube: a 240 x 240 x 18 km box keeps its
    /// proportions.
    ///
    /// Measured through the geometry rather than asserted about the matrix, so
    /// it fails if anyone "fixes" the pancake by normalising the axes. Looking
    /// straight down the y axis from level, the box's horizontal half-extent
    /// subtends a much larger angle than its vertical one, in the ratio of the
    /// physical extents.
    #[test]
    fn the_box_keeps_its_true_proportions() {
        let view = view_for(camera(180.0, 0.0, 2.0), BOX_KM, 1.0).expect("a view");
        // Box space is the unit cube whatever the physical extent, so the proof
        // has to be in world kilometres: the eye distance is set from the
        // half-diagonal of the *physical* box, which a normalised cube would
        // not have.
        let distance = (view.eye_km[0] * view.eye_km[0]
            + view.eye_km[1] * view.eye_km[1]
            + view.eye_km[2] * view.eye_km[2])
            .sqrt();
        let half_diagonal = 0.5 * (240.0f32 * 240.0 + 240.0 * 240.0 + 18.0 * 18.0).sqrt();
        assert!(
            (distance - 2.0 * half_diagonal).abs() < 1e-2,
            "eye at {distance} km is not 2.0 half-diagonals ({half_diagonal} km) out",
        );
        // And the eye in box space is *not* on a sphere: the z axis is 13x
        // shorter, so two half-diagonals of z is far more of the box's height
        // than of its width.
        let dz = (view.eye_in_box[2] - 0.5).abs();
        let dy = (view.eye_in_box[1] - 0.5).abs();
        assert!(
            dy > dz,
            "a level camera should be displaced in y, not z: {:?}",
            view.eye_in_box,
        );
    }

    /// The near and far planes do not move a ray.
    ///
    /// They look load-bearing and are not — the shader unprojects only at
    /// `depth = 1.0`, where the analytic inverse gives exactly the far plane, and
    /// the normalisation divides the distance out. Pinned because the tempting
    /// "fix" for a rendering problem is to tune them, and this says in advance
    /// that it will do nothing.
    ///
    /// **The depth range is not free of consequences, only of geometry.** The
    /// homogeneous `w` at `depth = 1.0` is `near/(near − far)`, so a range
    /// spanning many orders of magnitude cancels most of an `f32`'s digits away
    /// before the divide. That is why this asserts over sane ranges and why the
    /// production values are a couple of hundred apart rather than a million.
    #[test]
    fn the_frustum_depth_range_does_not_move_a_ray() {
        let camera = camera(225.0, 25.0, 2.5);
        let shallow = build_view(camera, BOX_KM, 1.6, 1.0, 3_000.0).expect("a view");
        let deep = build_view(camera, BOX_KM, 1.6, 20.0, 60_000.0).expect("a view");
        assert_ne!(
            shallow.box_from_clip, deep.box_from_clip,
            "precondition: the two frustums must actually differ",
        );
        for ndc in [[0.0, 0.0], [-1.0, -1.0], [0.9, -0.3]] {
            let want = direction(&shallow, ndc);
            let got = direction(&deep, ndc);
            for axis in 0..3 {
                assert!(
                    (got[axis] - want[axis]).abs() < 1e-3,
                    "ndc {ndc:?}: a 20x deeper frustum moved the ray from {want:?} to {got:?}",
                );
            }
        }
    }

    /// A wider viewport spreads the rays horizontally and leaves the vertical
    /// field of view alone. That is what `aspect` means, and dividing by it
    /// instead of multiplying is the mistake that squashes a 3D pane in a split
    /// layout while looking perfect in a square one.
    #[test]
    fn aspect_widens_the_horizontal_field_of_view_only() {
        let camera = camera(0.0, 0.0, 3.0);
        let square = view_for(camera, BOX_KM, 1.0).expect("a view");
        let wide = view_for(camera, BOX_KM, 2.0).expect("a view");

        let horizontal = |v: &VolumeView| {
            let centre = direction(v, [0.0, 0.0]);
            let edge = direction(v, [1.0, 0.0]);
            dot(centre, edge)
        };
        let vertical = |v: &VolumeView| {
            let centre = direction(v, [0.0, 0.0]);
            let edge = direction(v, [0.0, 1.0]);
            dot(centre, edge)
        };

        assert!(
            horizontal(&wide) < horizontal(&square),
            "doubling the aspect should widen the horizontal field of view",
        );
        assert!(
            (vertical(&wide) - vertical(&square)).abs() < 1e-6,
            "the vertical field of view must not depend on the aspect",
        );
    }

    fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }

    /// Every degenerate input is refused, not clamped.
    ///
    /// Each of these reaches a division. `f32::clamp` propagates `NaN`, so a
    /// clamp here would hand back a matrix of `NaN` that the GPU accepts and
    /// draws as an empty pane — a failure with no error anywhere.
    #[test]
    fn a_box_or_a_viewport_that_cannot_be_looked_at_is_refused() {
        let camera = OrbitCamera::default();
        for bad in [
            [0.0, 240.0, 18.0],
            [240.0, 0.0, 18.0],
            [240.0, 240.0, 0.0],
            [-240.0, 240.0, 18.0],
            [f32::NAN, 240.0, 18.0],
            [f32::INFINITY, 240.0, 18.0],
        ] {
            assert!(
                view_for(camera, bad, 1.0).is_none(),
                "box {bad:?} should have no view",
            );
        }
        for bad in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(
                view_for(camera, BOX_KM, bad).is_none(),
                "aspect {bad} should have no view",
            );
        }
    }

    /// The multiplication is column-major and in that order.
    ///
    /// Written against a hand-computed product rather than against another
    /// call to `multiply`, which is the version that cannot see a transpose.
    #[test]
    fn the_matrix_product_is_column_major() {
        // A pure translate by (1,2,3) and a pure scale by 2.
        let translate: Mat4 = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [1.0, 2.0, 3.0, 1.0],
        ];
        let scale: Mat4 = [
            [2.0, 0.0, 0.0, 0.0],
            [0.0, 2.0, 0.0, 0.0],
            [0.0, 0.0, 2.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        // scale · translate scales the translation; translate · scale does not.
        assert_eq!(multiply(scale, translate)[3], [2.0, 4.0, 6.0, 1.0]);
        assert_eq!(multiply(translate, scale)[3], [1.0, 2.0, 3.0, 1.0]);
    }

    /// The stub painter is not a substitute for the frontend's downcast test.
    ///
    /// This is the test that says so out loud. It asserts the stub's payload is
    /// exactly what `egui_wgpu` would silently discard, so nobody can read the
    /// stub-based suite as evidence that a real pane draws.
    #[test]
    fn the_stub_payload_is_the_kind_egui_wgpu_discards_in_silence() {
        let painter = StubVolumePainter::painting();
        let frame = VolumeFrameState {
            pane_idx: 0,
            target: VolumeTarget {
                volume: crate::pane::VolumeStamp {
                    site: "KTLX".to_owned(),
                    collected: chrono::NaiveDate::from_ymd_opt(2024, 5, 6)
                        .unwrap()
                        .and_hms_opt(22, 0, 0)
                        .unwrap(),
                },
                product: rustdar_radar::types::RadarProduct::Reflectivity,
            },
            camera: OrbitCamera::default(),
            size_px: [800, 600],
        };
        let VolumePaint::Callback(payload) = painter.paint(&frame) else {
            panic!("the painting stub must paint");
        };
        assert!(
            payload.downcast_ref::<StubPayload>().is_some(),
            "the stub's payload is its own type, which nothing in egui_wgpu can draw — \
             the real payload's downcast is pinned in rustdar-frontend by \
             `the_payload_the_painter_hands_over_is_one_egui_wgpu_can_draw`",
        );
        assert_eq!(painter.seen.lock().unwrap().len(), 1);
    }
}
