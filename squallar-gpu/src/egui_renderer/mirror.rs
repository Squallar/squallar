//! How large the pane mirror is drawn, and how often that is allowed to change.
//!
//! The mirror is a pane's own map render, copied into an offscreen texture the
//! raymarch samples for the 3D floor. It is regenerated every frame from egui's
//! primitives, so its density is a free parameter: which rung it is drawn at,
//! and when the rung may move.
//!
//! `egui_wgpu::Renderer::render` hardcodes `set_viewport(0, 0, size_in_pixels)`,
//! so the only lever is to move `size_in_pixels` and `pixels_per_point`
//! **together** — egui's vertex shader divides by their quotient, so geometry is
//! untouched and only the sampling rate moves. The floor's uniform lanes are the
//! reciprocal of that same quotient (`volume_bridge::floor_lanes`), so scaling
//! both leaves registration bit-identical at any rung. The attachment does grow,
//! which makes `max_texture_dimension_2d` a real bound — see [`MirrorLimits`].
//!
//! A rung multiplies the mirror's texels, not the detail it is given: tiles
//! arrive at the 2D pane's own zoom, so a rung is only worth having alongside a
//! matching tile zoom bias of `log2(applied rung)` on the pane drawing it.

// The side cap is device-class policy and lives on the floor since WO-RD;
// `MirrorLimits::for_device` here is what spends it.
use squallar_device_profile::constants::MIRROR_MAX_SIDE;

/// The highest rung the mirror is ever asked for, as a multiple of the frame's
/// own texel density.
///
/// Two independent things stop at 2. The tile cache: bias `log2(rung)` roughly
/// quadruples the tiles per level against the source's byte allowance
/// (`Budgets::tile_styled_bytes`, 114 city-core entries at the desktop
/// floor) — a 900-point floor strip drawing base and labels needs 72
/// tiles at bias 0, 242 at 1 and 882 at 2, so bias 2 could not be held whatever
/// the camera asked for. Those are the figures
/// `squallar_egui::tiles::tiles_resident_for` reports, which are the worst case
/// over the whole zoom range rather than at a whole zoom: between two zoom
/// steps a tile is drawn smaller and more of them fit, so a cache sized on the
/// whole-zoom count evicts tiles that are still on the glass.
///
/// Memory: 4× the frame's texels is 16× its bytes, 126 MiB for a 1080p frame,
/// which no arm of `VOLUME_MIRROR_BYTES_MAX` admits.
///
/// The mirror covers the frame plus the off-screen floor strips, which one
/// uniform translation keeps under twice the frame — at most one extra halving
/// on any target, since the fit halves both axes at once
/// ([`the_strip_costs_at_most_one_rung_on_any_target`]). In practice that costs
/// the top rung at 1440p on desktop and half the floor's density on a wasm frame
/// over ~1024 points or a phone frame over ~2000 px.
pub const MIRROR_SCALE_MAX: f32 = 2.0;

/// How far past a rung boundary the camera must fall before the rung above it is
/// given up, as a multiple.
///
/// Measured against the boundary of the rung being dropped **to**, not the rung
/// in force: giving up rung 2 needs a magnification below `1.0 / 1.25 = 0.8`.
/// 1.25 because walkers' scroll-zoom step is ~1.21× per wheel notch, so no
/// single notch can cross a rung boundary.
pub const MIRROR_RUNG_HYSTERESIS: f32 = 1.25;

/// How many consecutive frames the camera must want a different rung before it
/// gets one.
///
/// 15 frames, a quarter-second at 60 Hz. The dead band stops a rung oscillating
/// at a fixed camera; this stops it sweeping mid-gesture. Both directions on
/// purpose: taking an upward change immediately costs a fetch storm at the
/// moment the frame budget is most contended.
pub const MIRROR_RUNG_DWELL_FRAMES: u32 = 15;

/// What the device and the target's memory budget will let the mirror be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MirrorLimits {
    /// The adapter's `max_texture_dimension_2d`, never below
    /// [`MIRROR_MAX_SIDE`].
    pub max_side: u32,
    /// The resolved `Budgets::mirror_bytes` — this build's arm of
    /// [`squallar_device_profile::constants::VOLUME_MIRROR_BYTES_MAX`].
    pub max_bytes: usize,
}

impl MirrorLimits {
    /// The limits for a device reporting `max_texture_dimension_2d`.
    pub fn for_device(max_texture_dimension_2d: u32, max_bytes: usize) -> Self {
        Self {
            max_side: max_texture_dimension_2d.max(MIRROR_MAX_SIDE),
            max_bytes,
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
    /// How much of egui's own coordinate space the mirror covers, in points:
    pub size_in_points: [f32; 2],
    /// What [`Self::size_in_pixels`] is as a multiple of the frame's own pixel
    /// density. A power of two; below 1 means the request did not fit.
    pub applied_scale: f32,
    /// The rung the camera asked for, before the device and the budget had their
    /// say. Equal to [`Self::applied_scale`] on a target that could afford it.
    pub wanted_scale: f32,
}

impl MirrorPlan {
    /// Whether this target could not afford the density the camera wanted.
    pub fn is_degraded(&self) -> bool {
        self.applied_scale < self.wanted_scale
    }

    /// How many slippy zoom levels deeper a pane drawing a floor strip should
    /// fetch, given what this plan actually got. `0` or `1`; see
    /// [`MIRROR_SCALE_MAX`].
    pub fn tile_zoom_bias(&self) -> u8 {
        if self.applied_scale >= 2.0 { 1 } else { 0 }
    }
}

/// The rung the camera's magnification asks for: the smallest power of two that
/// covers it, held between 1 and [`MIRROR_SCALE_MAX`].
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

/// Plan the mirror for a region of egui's coordinate space `size_in_points`
/// across, drawn at a frame density of `pixels_per_point`, asked for at
/// `wanted_scale`.
pub fn mirror_plan(
    size_in_points: [f32; 2],
    pixels_per_point: f32,
    wanted_scale: f32,
    limits: MirrorLimits,
) -> MirrorPlan {
    let wanted = wanted_scale_for(wanted_scale);
    let points = [
        size_in_points[0].max(f32::MIN_POSITIVE),
        size_in_points[1].max(f32::MIN_POSITIVE),
    ];
    let texels = |axis: usize| {
        let px = (points[axis] * pixels_per_point * wanted).round();
        if px.is_finite() {
            px.max(1.0) as u32
        } else {
            1
        }
    };
    let mut size = [texels(0), texels(1)];
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
        size_in_points: points,
        applied_scale: applied,
        wanted_scale: wanted,
    }
}

/// The mirror's size and the scale to draw it at, for a region
/// `size_in_points` across at `pixels_per_point`, with no camera asking for
/// more.
pub fn mirror_size_for(size_in_points: [f32; 2], pixels_per_point: f32) -> ([u32; 2], f32) {
    let plan = mirror_plan(
        size_in_points,
        pixels_per_point,
        1.0,
        MirrorLimits::for_device(
            MIRROR_MAX_SIDE,
            squallar_device_profile::constants::VOLUME_MIRROR_BYTES_MAX,
        ),
    );
    (plan.size_in_pixels, plan.pixels_per_point)
}

/// The rung the mirror is currently drawn at, and how long the camera has
/// disagreed with it.
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
    pub fn observe(
        &mut self,
        magnification: Option<f32>,
        size_in_points: [f32; 2],
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
        let plan = mirror_plan(size_in_points, pixels_per_point, self.scale, limits);
        self.last = Some(plan);
        plan
    }

    /// The rung this magnification argues for, given the one in force.
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

    /// How many slippy zoom levels deeper a pane drawing a floor should fetch on
    /// the next frame, from the last plan the mirror was actually sized to.
    pub fn tile_zoom_bias(&self) -> u8 {
        self.last.map_or(0, |plan| plan.tile_zoom_bias())
    }
}

#[cfg(test)]
mod tests;
