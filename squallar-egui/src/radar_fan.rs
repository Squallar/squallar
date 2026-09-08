//! A sweep's gates as codes, and the door that draws them.
//!
//! The plan-view radar layer has one picture per frame and two ways to hold
//! it. The **raster** is what shipped: the rasterizer walks every gate, resolves
//! a colour per gate and writes it into a square RGBA image sized to the range
//! the sweep reached — `side x side x 4` bytes, where `side` is a screen-scale
//! number and not a data-scale one. The **fan** is the sweep itself: one byte
//! per gate in the polar frame the radar measured in, plus a 256-entry colour
//! table the fragment stage indexes. Same gates, same colours, no resampling
//! step in between.
//!
//! The size difference is the whole reason this exists, and it is a difference
//! of *shape* rather than of compression: a raster is quadratic in a number
//! chosen for the screen, a plane is linear in the numbers the radar chose.
//! `docs/radar-polar-design.md` §6.1 carries the arithmetic.
//!
//! # What this module is, and is not
//!
//! It is the **UI-side vocabulary**: the payload one sweep hands across
//! ([`FanSweep`]), what one frame's view of it is ([`FanView`]), the whole of
//! one draw ([`FanDraw`]), and the trait the shell installs a renderer through
//! ([`RadarFanPainter`]). It is the same shape as
//! [`crate::tile_mesh::TileMeshPainter`] and for the same reason: this crate
//! describes a draw and receives an opaque `Arc<dyn Any>` back, and never names
//! `wgpu`.
//!
//! It is **not** the producer and not the renderer. The plane is built off the
//! frame thread from the codes the wire carried
//! (`squallar_radar::render::codes::CodePlane`), and the codes are baked into a
//! table by `codes::Lut` — neither is named here, because both name
//! `RadarProduct` and this crate names no radar product at all
//! (`arch_ratchets.rs`, `PRODUCT_IN_EGUI_MAX = 0`, asserted with `assert_eq!`).
//! What arrives here is bytes, scalars and a [`FieldId`].
//!
//! # Why the radii travel in the payload
//!
//! [`FanGeometry`] carries the two Earth radii the projection and the beam
//! bend are taken on rather than reading them from `squallar_geo` or
//! `squallar_radar::beam`. That is not indirection for its own sake: the
//! fragment stage has to recover a ground range from a screen position and get
//! the *same* number the readout's pick got, and a shader that spelled a radius
//! of its own would be a second definition of the sphere.
//! `squallar-radar/tests/geodesy_one_definition.rs` scans every `.rs` **and
//! `.wgsl`** in the workspace for a literal in the 6300-6400 band; carrying the
//! values as data is what keeps both this file and the shader out of that scan
//! without an exemption.

use std::any::Any;
use std::sync::Arc;

use squallar_source::product::FieldId;

/// Entries in the colour table one plane is painted through, and therefore
/// codes an R8 plane can address.
///
/// The producer's own constant is `squallar_radar::render::codes::LUT_ENTRIES`;
/// this is the consumer's, and [`FanSweep::is_well_formed`] is where the two
/// are held equal on every payload that arrives.
pub const LUT_ENTRIES: usize = 256;

/// Bytes one entry of that table occupies: straight — **not premultiplied** —
/// RGBA.
///
/// The fragment stage multiplies by the layer's opacity and premultiplies
/// there, exactly as the tile-mesh path does into egui's own blend state.
/// Premultiplying at the bake would apply the layer factor twice.
pub const LUT_ENTRY_BYTES: usize = 4;

/// Bytes a whole colour table occupies — the length
/// [`FanSweep::lut_rgba`] must have.
pub const LUT_BYTES: usize = LUT_ENTRIES * LUT_ENTRY_BYTES;

/// Where a sweep's gates are on the ground, and on what spheres.
///
/// Everything the vertex stage needs to place a gate and the fragment stage
/// needs to recover which gate it landed in. The scalars are `f64` here and
/// narrow at the uniform: the site's own screen position is computed on the
/// CPU in `f64` precisely because the difference of two Mercator ordinates
/// loses too much in `f32` at deep zoom (design §4.3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FanGeometry {
    pub site_lat: f64,
    pub site_lon: f64,
    /// Gate 0's centre **along the beam**, km.
    pub first_gate_slant_km: f64,
    /// One gate's depth **along the beam**, km.
    pub gate_interval_slant_km: f64,
    /// The elevation the sweep was flown at, degrees, or `None` where the two
    /// ranges above are **already ground ranges** and must not be converted at
    /// all. The same distinction `PolarGeometry::elevation_deg` draws, carried
    /// across unchanged because the fragment's gate solve is that function's
    /// expression.
    pub elevation_deg: Option<f64>,
    /// How many gates along a radial the render actually reached — the bound
    /// the readout's pick answers within, and so the bound the fragment must
    /// answer within too, whatever [`FanSweep::gates`] the plane is wide.
    pub reach_gates: u32,
    /// The **ground** range of the outer edge of the last reached gate, km:
    /// where the drawn disc ends. The mesh's outermost ring sits here.
    pub reach_km: f64,
    /// The sphere a ground range is taken on, km. See the module docs.
    pub earth_radius_km: f64,
    /// The effective (beam-bent) radius a slant range is converted on, km.
    /// Equal to [`Self::earth_radius_km`] would be a straight beam, which is
    /// not what any of this tree's geometry does.
    pub effective_radius_km: f64,
}

/// **One sweep's polar payload**: its codes, its colours, and the sky each
/// radial was drawn over.
///
/// Radial-major at every level. Level 0 is `radials * gates` bytes; each
/// further level ceil-halves both dimensions and holds the strongest code
/// beneath it, so a fragment covering many gates reads the strongest echo in
/// its footprint rather than whichever radial happened to be written last.
/// That is a deliberate change from the raster's arbitration and the design
/// records it as ruling (1).
///
/// **Residency is pointer identity.** The renderer keeps a `Weak<FanSweep>`
/// per resident GPU sweep and sweeps them once per pass — `tile_mesh`'s rule
/// verbatim — so a loop step that reuses a sweep uploads nothing, and a sweep
/// nothing holds any more is released without this crate saying so.
#[derive(Clone, Debug, PartialEq)]
pub struct FanSweep {
    /// The field these codes decode to. **A [`FieldId`] and never a radar
    /// product**: the table is already baked, so the only thing left to name
    /// is which legend and which readout the gates belong to.
    pub field: FieldId,
    /// Radials at level 0.
    pub radials: u32,
    /// Gates at level 0 — the stride of a level-0 row.
    pub gates: u32,
    /// Level 0 followed by every further level, concatenated in level order.
    pub codes: Vec<u8>,
    /// Byte offset into [`Self::codes`] at which each level begins, level 0
    /// first. Its length is the number of levels, which is `1` for a
    /// categorical field whose codes must never be reduced.
    pub level_offsets: Vec<u32>,
    /// The colour table, [`LUT_BYTES`] of straight RGBA.
    pub lut_rgba: Vec<u8>,
    /// The **drawn** sky of each radial, degrees clockwise from true north, as
    /// `(lo, hi)` — one entry per radial, in the render's own radial order.
    ///
    /// Drawn, not declared: a sweep's radials do not tile the circle evenly
    /// and the readout's pick and the picture must agree about the edges or a
    /// hover reads a gate the user is not looking at. One table, one producer.
    pub edges: Vec<[f32; 2]>,
    pub geometry: FanGeometry,
}

impl FanSweep {
    /// Levels in this sweep's chain, counting level 0.
    pub fn levels(&self) -> usize {
        self.level_offsets.len()
    }

    /// A level's dimensions, by repeated ceil-halving — the producer's own
    /// rule, restated on the consumer's side because the renderer has to size
    /// a texture level from it and the two must not disagree.
    pub fn level_shape(&self, level: usize) -> Option<(u32, u32)> {
        if level >= self.levels() {
            return None;
        }
        let (mut r, mut g) = (self.radials, self.gates);
        for _ in 0..level {
            r = r.div_ceil(2);
            g = g.div_ceil(2);
        }
        Some((r, g))
    }

    /// A level's bytes, or `None` past the chain or where the payload is
    /// short.
    pub fn level(&self, level: usize) -> Option<&[u8]> {
        let (r, g) = self.level_shape(level)?;
        let start = *self.level_offsets.get(level)? as usize;
        let len = (r as usize).checked_mul(g as usize)?;
        self.codes.get(start..start.checked_add(len)?)
    }

    /// **Bytes this payload holds** — the codes and the table, read off the
    /// vectors rather than recomputed from the shape, so it measures the
    /// object and not a second spelling of its price.
    ///
    /// The edge table is in it too: at 720 radials it is 5,760 B, which is
    /// small beside the plane and not small enough to round away when a pane
    /// is holding sixty of them.
    pub fn resident_bytes(&self) -> usize {
        self.codes.len() + self.lut_rgba.len() + self.edges.len() * size_of::<[f32; 2]>()
    }

    /// Whether this payload describes itself consistently.
    ///
    /// **Checked at the door rather than trusted**, because everything past
    /// this point is an index into a byte slice sized by a number that
    /// travelled beside it. A malformed payload here is a texture upload of
    /// the wrong length or a fragment reading another level's bytes; refusing
    /// it is a picture that does not draw, which is recoverable and visible.
    ///
    /// It is deliberately not a constructor guard. The producer already
    /// refuses at construction (`CodePlane::build`) and a second opinion on
    /// the same question is how two authorities drift apart; this is the
    /// *transport* check — that what arrived is what was built — and it is the
    /// one the renderer needs before it indexes anything.
    pub fn is_well_formed(&self) -> bool {
        if self.radials == 0 || self.gates == 0 || self.level_offsets.is_empty() {
            return false;
        }
        if self.edges.len() != self.radials as usize {
            return false;
        }
        if self.geometry.reach_gates == 0 || self.geometry.reach_gates > self.gates {
            return false;
        }
        if self.level_offsets[0] != 0 {
            return false;
        }
        // Every level must lie inside `codes`, and the levels must be laid out
        // in order with no gap and no overlap: the offset of level `l + 1` is
        // the end of level `l`.
        let mut want = 0usize;
        for level in 0..self.levels() {
            let Some((r, g)) = self.level_shape(level) else {
                return false;
            };
            if self.level_offsets[level] as usize != want {
                return false;
            }
            let Some(cells) = (r as usize).checked_mul(g as usize) else {
                return false;
            };
            let Some(next) = want.checked_add(cells) else {
                return false;
            };
            want = next;
        }
        self.codes.len() == want
    }
}

/// One frame's view of a fan, in the terms the vertex stage places it with.
///
/// Every field is read off the frame's own [`walkers::Projector`] and the
/// pane's rect, so the fan lands exactly where a projected point lands. The
/// site's screen position in particular is projected in `f64` here and enters
/// the uniform as a small `f32` offset, which is what keeps deep zoom exact
/// (design §4.3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FanView {
    /// The pane's map rect, **points**. This is the callback's own rect, which
    /// egui turns into the viewport, so the vertex stage emits clip space
    /// inside it and no `set_viewport` override is needed.
    pub rect: egui::Rect,
    /// The site's screen position, points.
    pub site_px: egui::Pos2,
    /// Points the whole Mercator world spans at this zoom —
    /// `Projector::world_pixels`, which despite the name is points.
    pub world_px: f64,
    /// Ground kilometres one screen point covers **at the site**: the level
    /// selector, and the only reason the chain exists.
    pub km_per_px: f64,
    /// Physical pixels per point, for the callback's own viewport arithmetic.
    pub pixels_per_point: f32,
}

/// The whole of one fan draw: which sweeps, seen how, at what opacity.
pub struct FanDraw<'a> {
    /// The sweeps to draw, in submission order. Cloned by the renderer to hold
    /// the payloads alive for the upload, and downgraded to the weak handles
    /// its residency is swept by.
    pub sweeps: &'a Arc<[Arc<FanSweep>]>,
    pub view: FanView,
    /// The layer's opacity for this frame, 0-1, which the callback must apply
    /// itself: a `Shape::Callback` is the one shape `Painter::add` cannot
    /// tint, so the layer walk's `set_opacity` reaches every CPU-drawn shape
    /// beside this one and reaches the fan only through here.
    pub opacity: f32,
    /// egui's cumulative pass number, so the renderer can tell one frame's
    /// draws from the next without a clock or a callback of its own.
    pub pass_nr: u64,
}

/// Something that can draw a sweep's code plane from the GPU.
///
/// Installed by the shell through [`GuiEvent::RadarFanPainter`]; absent, a fan
/// surface cannot be drawn at all and the pane says so rather than painting
/// nothing quietly — see [`FanRefusal`].
///
/// [`GuiEvent::RadarFanPainter`]: crate::shell_api::GuiEvent::RadarFanPainter
pub trait RadarFanPainter: Send + Sync {
    /// This frame's payload for one pane's sweeps, or `None` when the renderer
    /// cannot draw them.
    fn payload(&self, draw: FanDraw<'_>) -> Option<Arc<dyn Any + Send + Sync>>;
}

/// Why a fan surface did not become a callback this frame.
///
/// **A fan that cannot be drawn has no fallback**, which is what makes this an
/// enum and not a `bool`: the raster it would fall back to is exactly the
/// object the polar representation exists not to allocate, so there is nothing
/// on the pane to draw instead. Every arm is therefore a hole in the picture,
/// counted by [`ledger`] and never silent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FanRefusal {
    /// No renderer is installed. A build with no wgpu renderer, and every unit
    /// test in this crate.
    NoPainter,
    /// The payload does not describe itself consistently —
    /// [`FanSweep::is_well_formed`] said no.
    Malformed,
    /// This pass is a 3D pane's off-screen floor strip, whose primitives are
    /// copied into the mirror with every callback swapped for an empty mesh.
    /// A fan issued here would reach the floor as nothing at all, so it is
    /// refused where the swap can still be seen instead of drawn where it
    /// cannot.
    FloorStrip,
    /// The renderer was asked and declined.
    PainterDeclined,
}

/// What a fan surface's draw decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FanOutcome {
    Painted,
    Refused(FanRefusal),
}

/// **What the fan path has done since the process started.**
///
/// Always on, and for the reason every other ledger in this tree is: a hole in
/// the picture that only a `cfg(test)` counter can see is a hole nobody can
/// report from a running app. The figures are process-global running totals
/// and name their own denominator — `draws` is every fan surface that reached
/// the draw fork, and `painted + refused` is exactly that number.
pub mod ledger {
    use std::sync::atomic::{AtomicU64, Ordering};

    static DRAWS: AtomicU64 = AtomicU64::new(0);
    static PAINTED: AtomicU64 = AtomicU64::new(0);
    static NO_PAINTER: AtomicU64 = AtomicU64::new(0);
    static MALFORMED: AtomicU64 = AtomicU64::new(0);
    static FLOOR_STRIP: AtomicU64 = AtomicU64::new(0);
    static DECLINED: AtomicU64 = AtomicU64::new(0);

    /// The counters, read together.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Totals {
        /// Fan surfaces that reached the draw fork.
        pub draws: u64,
        /// Of those, the ones that became a paint callback.
        pub painted: u64,
        pub no_painter: u64,
        pub malformed: u64,
        pub floor_strip: u64,
        pub declined: u64,
    }

    impl Totals {
        /// Every arm of [`super::FanRefusal`], summed. `painted + refused ==
        /// draws` is the identity these figures are only meaningful under.
        pub fn refused(&self) -> u64 {
            self.no_painter + self.malformed + self.floor_strip + self.declined
        }
    }

    /// Record one fan surface's outcome.
    pub(crate) fn note(outcome: super::FanOutcome) {
        DRAWS.fetch_add(1, Ordering::Relaxed);
        let counter = match outcome {
            super::FanOutcome::Painted => &PAINTED,
            super::FanOutcome::Refused(super::FanRefusal::NoPainter) => &NO_PAINTER,
            super::FanOutcome::Refused(super::FanRefusal::Malformed) => &MALFORMED,
            super::FanOutcome::Refused(super::FanRefusal::FloorStrip) => &FLOOR_STRIP,
            super::FanOutcome::Refused(super::FanRefusal::PainterDeclined) => &DECLINED,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// The running totals.
    pub fn totals() -> Totals {
        Totals {
            draws: DRAWS.load(Ordering::Relaxed),
            painted: PAINTED.load(Ordering::Relaxed),
            no_painter: NO_PAINTER.load(Ordering::Relaxed),
            malformed: MALFORMED.load(Ordering::Relaxed),
            floor_strip: FLOOR_STRIP.load(Ordering::Relaxed),
            declined: DECLINED.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
#[path = "radar_fan/tests.rs"]
mod tests;
