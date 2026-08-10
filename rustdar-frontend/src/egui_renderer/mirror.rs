//! How large the pane mirror is drawn, and how often that is allowed to change.
//!
//! The mirror is the 2D pane's own render, copied into an offscreen texture the
//! raymarch samples for the 3D view's map floor. Nothing about that copy forces
//! it to be made at the *frame's* texel density: it is regenerated every frame
//! from egui's tessellated primitives, so the density is a free parameter, and
//! the only thing that ever wanted it pinned to the frame was that nobody had
//! asked for anything else.
//!
//! A 3D camera that is low and close magnifies the ground it samples. At the
//! user's reported framing — a 460 km box, the eye near the deck, a city name
//! filling a fifth of the pane — the floor is stretched several times over, and
//! a fixed-density mirror has no more detail to give. This module is the two
//! decisions that fixes: **which rung** the mirror is drawn at, and **when** the
//! rung is allowed to move.
//!
//! # The lever, and that it works upwards
//!
//! `egui_wgpu::Renderer::render` hardcodes `set_viewport(0, 0, size_in_pixels)`
//! and WebGPU validates the viewport against the attachment, so the mirror
//! cannot be scaled onto a differently-sized target by touching the viewport.
//! The one lever that works is to move `size_in_pixels` and `pixels_per_point`
//! **together**: egui's vertex shader divides by `screen_size_in_points`, which
//! is their quotient, so the geometry is untouched and only the sampling rate
//! moves.
//!
//! That argument is a statement about a quotient and is therefore direction-free
//! — it holds for a scale above 1 exactly as it does for the halving that
//! already shipped. Two consequences are worth naming because they are what a
//! reviewer should check rather than take on trust:
//!
//! * The floor's own uniform lanes are `pixels_per_point / size_in_pixels`
//!   (`volume_bridge::floor_lanes`), i.e. the reciprocal of the same quotient.
//!   Scaling both leaves every lane bit-identical, so **registration cannot
//!   move** — which is why `floor_alignment` still reports best translation
//!   `(0, 0)` with this landed, and would still do so at any rung.
//! * The attachment grows, so the device's `max_texture_dimension_2d` becomes a
//!   real bound rather than a formality. See [`MirrorLimits`].
//!
//! # What a rung buys, and what it does not
//!
//! A rung multiplies the texels the mirror has. It does **not** multiply the
//! detail the mirror is given: the basemap, the roads and the place labels all
//! arrive as raster tiles chosen for the 2D pane's own zoom, and drawing a
//! 256-texel tile into twice as many mirror texels interpolates rather than
//! reveals. So the rung is spent, and only spent, alongside a matching
//! **tile zoom bias** on the source pane — `log2` of the applied rung, fetched
//! one slippy level deeper and drawn at half the point footprint. The rung is
//! where the extra detail lands; the bias is where it comes from. Either alone
//! is wasted, which is why [`MirrorRungs::tile_zoom_bias`] is derived from the
//! rung that was *applied* rather than the one that was wanted.

/// The largest side the pane mirror is allowed when nothing better is known.
///
/// 2048 because that is the smallest `max_texture_dimension_2d` the targets this
/// application runs on may legitimately report: it is what
/// `wgpu::Limits::downlevel_webgl2_defaults()` guarantees, and the wasm arm is
/// held to that floor. A mirror at this cap allocates on every device the rest
/// of the application already runs on.
///
/// It is a **fallback, not the cap**. [`MirrorLimits::for_device`] raises it to
/// whatever the adapter actually reports, which on any desktop is 8192 or more
/// — and without that raise a 4K desktop frame would go on being mirrored at
/// half its own density, which is the reduction this constant used to force.
pub const MIRROR_MAX_SIDE: u32 = 2048;

/// The highest rung the mirror is ever asked for, as a multiple of the frame's
/// own texel density.
///
/// Two independent things stop at 2, and they agree, which is the only reason
/// to write a single number down:
///
/// * **The tile cache.** A rung is only worth having with a matching tile zoom
///   bias, and the bias is `log2(rung)`. Each level is four times the tiles,
///   against the single `tile_source::TILE_CACHE_ENTRIES` (256) LRU every pane
///   and every layer shares. A 900-point-square source pane drawing a basemap
///   and a label layer needs about 32 tiles at bias 0, 162 at bias 1 and 594 at
///   bias 2 — so bias 2 cannot fit however the window is arranged, while bias 1
///   fits some windows and not others.
/// * **Memory.** 4x the frame's texels is 16x its bytes — 126 MiB for a 1080p
///   frame — which no arm of [`crate::constants::VOLUME_MIRROR_BYTES_MAX`]
///   admits anyway.
///
/// So the byte budget would refuse rung 4 on its own; the cap is written down
/// separately because the tile-cache argument is the one that would still hold
/// on a target with memory to spare.
///
/// **"Fits some windows and not others" is not left to a cap.** Whether bias 1
/// actually fits is measured per frame against the real pane rects, by
/// `Gui::tile_zoom_bias_for_pane` through `tiles::tiles_resident_for`, and the
/// bias is dropped to 0 when it would not. This constant is the ceiling on what
/// may ever be asked for; that check is what decides whether the ask is taken.
pub const MIRROR_SCALE_MAX: f32 = 2.0;

/// How far past a rung boundary the camera must fall before the rung above it is
/// given up, as a multiple.
///
/// Rungs are powers of two, so the bare rule "use the smallest rung at least as
/// large as the magnification" has its switch points at exactly 1.0 and 2.0 —
/// and a camera drifting across one of those would re-render the mirror at a new
/// size, and re-fetch a whole tile pyramid at a new zoom, on alternate frames.
///
/// The band is measured against the boundary of the rung being dropped **to**,
/// not against the rung in force: giving up rung 2 needs a magnification below
/// `1.0 / 1.25 = 0.8`, because 1.0 is where rung 2 stops being necessary. Read
/// the other way round — as a fraction of the rung in force — the same rule is a
/// 40 % undershoot, and stating it against the boundary is what makes it one
/// number rather than one per rung.
///
/// 1.25 because walkers' own scroll-zoom step is about 1.21x per wheel notch
/// (`ui_map`'s note on the 0.55 frame-time change): a dead band wider than one
/// notch means no single notch can cross a rung boundary, so moving a rung takes
/// two deliberate notches in the same direction rather than one twitch back and
/// forth over a threshold.
pub const MIRROR_RUNG_HYSTERESIS: f32 = 1.25;

/// How many consecutive frames the camera must want a different rung before it
/// gets one.
///
/// 15 frames is a quarter-second at 60 Hz. The dead band above stops a rung
/// oscillating at a fixed camera; this stops it *sweeping* — a continuous drag
/// from far to near crosses a boundary once, and without a dwell it would issue
/// a tile-zoom change on the frame it happened to cross, mid-gesture, while the
/// user is still moving and the answer is still changing.
///
/// It applies in both directions on purpose. Deferring an upward change costs a
/// quarter-second of the softness the user already had; taking it immediately
/// costs a fetch storm at the exact moment the frame budget is most contended.
pub const MIRROR_RUNG_DWELL_FRAMES: u32 = 15;

/// What the device and the target's memory budget will let the mirror be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MirrorLimits {
    /// The adapter's `max_texture_dimension_2d`, never below
    /// [`MIRROR_MAX_SIDE`].
    pub max_side: u32,
    /// This target's arm of [`crate::constants::VOLUME_MIRROR_BYTES_MAX`].
    pub max_bytes: usize,
}

impl MirrorLimits {
    /// The limits for a device reporting `max_texture_dimension_2d`.
    ///
    /// Floored at [`MIRROR_MAX_SIDE`] rather than trusted outright so that a
    /// device — or a test double — reporting something absurdly small cannot
    /// drive the fit loop down to a one-texel mirror. The byte budget is the
    /// compiled target's, which is the half of this pair that is a *decision*
    /// rather than a measurement.
    pub fn for_device(max_texture_dimension_2d: u32) -> Self {
        Self {
            max_side: max_texture_dimension_2d.max(MIRROR_MAX_SIDE),
            max_bytes: crate::constants::VOLUME_MIRROR_BYTES_MAX,
        }
    }
}

/// The size and scale to draw the pane mirror at, and what it cost to get there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MirrorPlan {
    /// The mirror texture's size in texels.
    pub size_in_pixels: [u32; 2],
    /// The `pixels_per_point` the mirror pass draws at. Moves with
    /// [`Self::size_in_pixels`] and never without it — see the module doc.
    pub pixels_per_point: f32,
    /// What [`Self::size_in_pixels`] is as a multiple of the frame's own pixel
    /// size. A power of two; below 1 means the frame did not fit.
    pub applied_scale: f32,
    /// The rung the camera asked for, before the device and the budget had their
    /// say. Equal to [`Self::applied_scale`] on a target that could afford it.
    pub wanted_scale: f32,
}

impl MirrorPlan {
    /// Whether this target could not afford the density the camera wanted.
    ///
    /// Not cosmetic: the tile zoom bias is taken from the *applied* scale, so a
    /// degraded plan must not go on fetching a slippy level the mirror has no
    /// texels to show. Exposed rather than left implicit so the degradation is
    /// something the code says out loud.
    pub fn is_degraded(&self) -> bool {
        self.applied_scale < self.wanted_scale
    }

    /// How many slippy zoom levels deeper the source pane should fetch, given
    /// what this plan actually got. `0` or `1`; see [`MIRROR_SCALE_MAX`].
    pub fn tile_zoom_bias(&self) -> u8 {
        if self.applied_scale >= 2.0 { 1 } else { 0 }
    }
}

/// The rung the camera's magnification asks for: the smallest power of two that
/// covers it, held between 1 and [`MIRROR_SCALE_MAX`].
///
/// Never below 1. A camera that is *minifying* the floor — zoomed far out, many
/// mirror texels to the screen pixel — would in principle be served by a smaller
/// mirror, but the mirror is one texture for the whole application and shrinking
/// it would blur the 2D panes' own floors under any other 3D pane. Reductions
/// below 1 exist only as the fit's answer to a frame that does not fit, which is
/// a different question with a different answer.
pub fn wanted_scale_for(magnification: f32) -> f32 {
    if !magnification.is_finite() || magnification <= 1.0 {
        return 1.0;
    }
    let mut scale = 1.0f32;
    while scale < magnification && scale < MIRROR_SCALE_MAX {
        scale *= 2.0;
    }
    scale.min(MIRROR_SCALE_MAX)
}

/// Plan the mirror for a frame of `size_in_pixels` at `pixels_per_point`, asked
/// for at `wanted_scale`.
///
/// Scales up first, then halves until the result fits both the device's side
/// limit and the target's byte budget — so the fit is the same loop that already
/// shipped, and a frame too large to mirror at all is still reduced rather than
/// refused. Both axes and the scale move together at every step, which is the
/// invariant the whole design rests on.
pub fn mirror_plan(
    size_in_pixels: [u32; 2],
    pixels_per_point: f32,
    wanted_scale: f32,
    limits: MirrorLimits,
) -> MirrorPlan {
    let wanted = wanted_scale_for(wanted_scale);
    let mut size = [
        (size_in_pixels[0].max(1) as f32 * wanted) as u32,
        (size_in_pixels[1].max(1) as f32 * wanted) as u32,
    ];
    let mut applied = wanted;
    let mut scale = pixels_per_point * wanted;
    while size[0].max(size[1]) > limits.max_side
        || (size[0] as usize) * (size[1] as usize) * 4 > limits.max_bytes
    {
        let halved = [(size[0] / 2).max(1), (size[1] / 2).max(1)];
        if halved == size {
            // Both axes are already 1: nothing left to halve, and looping
            // forever is worse than a mirror nothing can sample.
            break;
        }
        size = halved;
        applied *= 0.5;
        scale *= 0.5;
    }
    MirrorPlan {
        size_in_pixels: size,
        pixels_per_point: scale,
        applied_scale: applied,
        wanted_scale: wanted,
    }
}

/// The mirror's size and the scale to draw it at, for a frame of
/// `size_in_pixels` at `pixels_per_point`, with no camera asking for more.
///
/// The pre-adaptive behaviour, kept as its own name because it is what every
/// frame with no 3D pane still does and what the budget prose is written
/// against.
pub fn mirror_size_for(size_in_pixels: [u32; 2], pixels_per_point: f32) -> ([u32; 2], f32) {
    let plan = mirror_plan(
        size_in_pixels,
        pixels_per_point,
        1.0,
        MirrorLimits::for_device(MIRROR_MAX_SIDE),
    );
    (plan.size_in_pixels, plan.pixels_per_point)
}

/// The rung the mirror is currently drawn at, and how long the camera has
/// disagreed with it.
///
/// One per application, because the mirror is one texture for the whole
/// application. See [`MIRROR_RUNG_HYSTERESIS`] and [`MIRROR_RUNG_DWELL_FRAMES`]
/// for the two rules this holds the state for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MirrorRungs {
    scale: f32,
    /// The scale [`Self::observe`] has been asked for on every frame since it
    /// last disagreed with [`Self::scale`], and how many frames that has been.
    pending: Option<(f32, u32)>,
    /// The last plan [`Self::observe`] produced, so the tile bias a frame is
    /// drawn with is the one the mirror was actually sized to.
    last: Option<MirrorPlan>,
}

impl Default for MirrorRungs {
    fn default() -> Self {
        Self {
            scale: 1.0,
            pending: None,
            last: None,
        }
    }
}

impl MirrorRungs {
    /// Fold one frame's magnification demand into the rung, and plan the mirror.
    ///
    /// `magnification` is the largest any 3D pane on the frame reported (see
    /// `rustdar_egui::volume_view::floor_magnification`), or `None` when no pane
    /// wants a floor — which holds the rung where it is rather than resetting
    /// it, so hiding a floor for a moment does not cost the tile pyramid.
    pub fn observe(
        &mut self,
        magnification: Option<f32>,
        size_in_pixels: [u32; 2],
        pixels_per_point: f32,
        limits: MirrorLimits,
    ) -> MirrorPlan {
        if let Some(magnification) = magnification {
            let want = self.want_for(magnification);
            self.pending = match self.pending {
                Some((pending, frames)) if pending == want => Some((want, frames + 1)),
                _ if want != self.scale => Some((want, 1)),
                _ => None,
            };
            if let Some((want, frames)) = self.pending
                && frames >= MIRROR_RUNG_DWELL_FRAMES
            {
                self.scale = want;
                self.pending = None;
            }
        }
        let plan = mirror_plan(size_in_pixels, pixels_per_point, self.scale, limits);
        self.last = Some(plan);
        plan
    }

    /// The rung this magnification argues for, given the one in force.
    ///
    /// Upwards on the bare rule, downwards only once the magnification is a
    /// clear [`MIRROR_RUNG_HYSTERESIS`] below the boundary it would be dropping
    /// past — which is `bare` itself, the top of the rung being dropped to.
    fn want_for(&self, magnification: f32) -> f32 {
        let bare = wanted_scale_for(magnification);
        if bare >= self.scale {
            return bare;
        }
        if magnification * MIRROR_RUNG_HYSTERESIS < bare {
            bare
        } else {
            self.scale
        }
    }

    /// How many slippy zoom levels deeper a floor-source pane should fetch on
    /// the next frame, from the last plan the mirror was actually sized to.
    ///
    /// Last frame's, necessarily: tiles are drawn while the egui pass is open
    /// and the mirror is sized after it closes. A rung that has just moved
    /// therefore reaches the tiles one frame later, which is invisible beside
    /// the quarter-second dwell that let it move at all.
    pub fn tile_zoom_bias(&self) -> u8 {
        self.last.map_or(0, |plan| plan.tile_zoom_bias())
    }
}

#[cfg(test)]
mod tests;
