//! One radar sweep drawn as a fan of per-radial sectors, coloured on the GPU
//! by a lookup into a 256-entry table.
//!
//! The renderer half of the polar representation `docs/radar-polar-design.md`
//! describes: the sweep reaches the card as the **codes the wire carried**, one
//! byte a gate, and the colour is resolved per fragment out of a
//! `256 x 1` RGBA table instead of per gate on the CPU into a plan-view raster.
//! A surveillance tilt is 1,758,630 B of code plane against 650,388,528 B of
//! raster at the side one is actually rendered at.
//!
//! # Why this is a paint callback and not an egui texture
//!
//! egui's `ImageData` has one variant, `Color32` — four bytes a texel of
//! finished colour. There is no way to hand egui a single-channel plane of
//! indices and a palette to resolve them through, so a code plane cannot ride
//! the texture path at all. The plane and the table are therefore **this
//! callback's own bind group**, reached through `egui_wgpu::CallbackTrait`
//! exactly as [`crate::tile_mesh`] reaches its vertex buffers.
//!
//! Everything structural below is `tile_mesh`'s, copied rather than invented:
//! the store in `egui_wgpu`'s `CallbackResources` (keyed by type, so one slot
//! for the application and the per-sweep map inside it), the `Weak` handle that
//! is the whole eviction rule, the once-per-pass sweep under a `swept_pass`
//! guard, the dynamic-offset uniform ring claimed in `prepare` and read in
//! `paint`, and the batch that writes that ring once per pass rather than once
//! per draw.
//!
//! # What installs this
//!
//! [`RadarFanBridge`], published through `squallar_egui`'s
//! `GuiEvent::RadarFanPainter` by the shell that also puts a [`RadarFanStore`]
//! into the renderer's callback resources — the store first, so no frame can
//! dispatch a fan callback into an empty slot. The same switch is what makes
//! the *producer* build planes at all (`squallar_radar`'s `PlanSurface::Fan`),
//! so a build that cannot draw a fan never asks for one: a polar frame has no
//! fallback, because the raster it would fall back to is the allocation this
//! representation exists not to make.
//!
//! The scene's price follows what the renderer produced and not this module's
//! presence. A pane's loop frames are priced off the payloads the pane is
//! actually holding — `squallar_egui::radar_fan::FanSweep::resident_bytes`,
//! measured — so a frame that fell back to a raster is still priced as one.
//!
//! # What this module does NOT decide
//!
//! **Which products may ride an R8 plane.** `squallar_radar`'s `CodePlane::build`
//! refuses the computed gradient fields at construction, and a second opinion
//! here would be a second authority on the same question. This consumes what
//! that produces and checks only what it must in order to upload it: that the
//! shapes are inside the texture caps and that the buffers are the length the
//! shape declares.

use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use egui_wgpu::wgpu;
use squallar_device_profile::constants::{
    MAX_POLAR_GATES, MAX_POLAR_RADIALS, POLAR_LUT_BYTES, POLAR_LUT_ENTRIES, full_mip_levels,
};

pub mod chain_staging;

use chain_staging::{ChainStaging, ChainStagingTotals, Pending};

/// **The payload this module draws** — `squallar_egui`'s, not one of its own.
///
/// A type alias and never a second type. The plane is built off the frame
/// thread into that struct, the pane holds it, and the callback below carries
/// the same `Arc`: one allocation, one authority on what a sweep's shape is,
/// and no copy anywhere between the producer and the upload. The name is
/// shortened here only because it appears in every signature.
pub type UiSweep = squallar_egui::radar_fan::FanSweep;

/// Wedges in the canonical disk mesh — one per radial a sweep may declare.
///
/// [`MAX_POLAR_RADIALS`] and not a number of its own: the mesh is built once
/// and shared by every sweep, so it has to carry a sector for the widest
/// payload `CodePlane` will admit. A sweep with fewer radials leaves the
/// surplus sectors' drawn edges equal, which makes their triangles degenerate
/// and costs no fragments.
pub const SECTORS: usize = MAX_POLAR_RADIALS;

/// Range segments per sector.
///
/// A **silhouette parameter only**. The fragment stage solves for its own
/// ground range out of the interpolated Mercator offset, so this changes how
/// round the outer arc looks and nothing about which gate a pixel reads. At
/// 460 km the outer chord of a 0.5 degree sector departs from the true arc by
/// `r(1 - cos 0.25 deg)` = 4.4 m, and a 29 km radial segment's departure from
/// the Mercator curve is smaller still — both far under a 250 m gate.
pub const RINGS: usize = 16;

/// Vertices in the canonical mesh: two sides of a sector, at every ring
/// boundary.
pub const MESH_VERTICES: usize = SECTORS * (RINGS + 1) * 2;

/// Indices in the canonical mesh: two triangles a quad, one quad per sector per
/// ring.
pub const MESH_INDICES: usize = SECTORS * RINGS * 6;

/// **Indices a sweep of `radials` radials occupies in the canonical mesh** —
/// and therefore the whole of what a draw has to submit for it.
///
/// [`disk_mesh`] is sector-major: sector `s`'s six indices per ring are emitted
/// before sector `s + 1`'s, so a sweep's sectors are the mesh's FIRST
/// `radials`, contiguously, and `0..sector_indices(radials)` is exactly them.
/// `the_canonical_mesh_is_sector_major_and_a_sweep_owns_its_prefix` is what
/// holds that, decoded out of the mesh this function is about rather than
/// asserted of it.
///
/// The mesh carries a sector per radial [`MAX_POLAR_RADIALS`] admits — 1440 —
/// and both real WSR-88D shapes declare 720, so **half of every fan draw was a
/// vertex stage run over sectors whose two triangles are degenerate by
/// construction**. They were never pixels; they were `SECTORS - radials`
/// wedges' worth of transcendentals per pane per frame, and the fragment stage
/// could not see the difference.
///
/// The clamp is not a second opinion on [`admit`], which refuses a payload past
/// [`MAX_POLAR_RADIALS`] before a callback is ever built. It bounds an index
/// range handed straight to `draw_indexed`, which is a device error rather than
/// a wrong picture if it runs off the buffer — and `paint` has no way to refuse
/// a callback that reached it.
pub const fn sector_indices(radials: u32) -> u32 {
    let sectors = if radials < SECTORS as u32 {
        radials
    } else {
        SECTORS as u32
    };
    sectors * RINGS as u32 * 6
}

/// Bits the sector index occupies in a mesh vertex.
const SECTOR_BITS: u32 = 11;
/// Bits the ring index occupies, above the sector.
const RING_BITS: u32 = 5;
/// Where the side bit sits.
const SIDE_SHIFT: u32 = SECTOR_BITS + RING_BITS;

// **A build failure, not a test.** The whole mesh is one `u32` a vertex, and a
// field that no longer fits its bits would alias onto its neighbour silently —
// a sector drawn at another sector's azimuth, with every pixel in the picture
// looking plausible.
const _: () = assert!(
    SECTORS <= 1 << SECTOR_BITS,
    "the sector index no longer fits the mesh vertex's bit field"
);
const _: () = assert!(
    RINGS < 1 << RING_BITS,
    "the ring index no longer fits the mesh vertex's bit field"
);
// The edge table pairs two radials to a `vec4`, so an odd cap would leave the
// last radial's `(lo, hi)` in an element that is half off the end of the array.
const _: () = assert!(
    SECTORS.is_multiple_of(2),
    "the drawn-edge table pairs radials into vec4 lanes and cannot hold an odd count"
);

/// `vec4` lanes in the WGSL `Sweep`'s drawn-edge table: two radials each.
pub const EDGE_VEC4S: usize = SECTORS / 2;

/// The shader's own source, so a test translates the module the pipeline is
/// built from rather than a second copy of it.
pub const RADAR_FAN_WGSL: &str = include_str!("radar_fan.wgsl");

/// Callbacks that reached `prepare` or `paint` with no [`RadarFanStore`] in the
/// callback resources, since the process started.
///
/// **Process-wide and always on**, like the rest of this workspace's ledgers.
/// It is the only evidence of the one wiring mistake that leaves an
/// ordinary-looking map with no radar on it: the store is keyed by type in
/// `egui_wgpu`'s `CallbackResources`, so an install-order slip produces
/// callbacks that decline silently and a picture nobody can tell from "no data
/// yet". A `#[cfg(test)]` counter could not report one from a running app.
static STORELESS_PREPARES: AtomicU64 = AtomicU64::new(0);

/// The store-less tally since the process started.
pub fn storeless_callbacks() -> u64 {
    STORELESS_PREPARES.load(Ordering::Relaxed)
}

/// Bytes one [`Locals`](radar_fan.wgsl) block occupies: two `vec2` and eight
/// `f32`.
const LOCALS_BYTES: u64 = 48;

/// Bytes the WGSL `Sweep` block occupies: the drawn-edge table, then twelve
/// scalar lanes.
pub const SWEEP_UNIFORM_BYTES: u64 = (EDGE_VEC4S as u64) * 16 + 48;

/// Uniform slots one frame may claim before the ring wraps onto one the same
/// frame is still going to read.
///
/// One slot per **callback**, not per sweep: a callback carries one view and
/// every sweep it draws shares it, which is why a loop step swaps a bind group
/// and writes nothing.
const RING_SLOTS: u32 = 64;

/// The desktop layout's pane cap. See [`RING_SLOTS`].
const PANES: u32 = 6;
/// Fan draws one pane may issue in a frame. One radar surface per pane is what
/// the app draws today; four is the slack.
const FANS_PER_PANE: u32 = 4;

// **A build failure, not a test**, for the reason `tile_mesh`'s equivalent is:
// a ring that wraps inside one frame overwrites a slot that frame is still
// going to read, and draws one pane's fan at another pane's site.
const _: () = assert!(
    PANES * FANS_PER_PANE <= RING_SLOTS,
    "a frame's worst case of fan draws no longer fits the uniform ring"
);

/// One mesh vertex, packed.
///
/// Public because `tests/radar_fan_gpu.rs` asserts the mesh against it rather
/// than against a second spelling of the same arithmetic.
pub const fn pack_vertex(sector: u32, ring: u32, side: u32) -> u32 {
    sector | (ring << SECTOR_BITS) | (side << SIDE_SHIFT)
}
/// Why a payload could not become a resident sweep.
///
/// Every arm is a refusal and none is a truncation. **None of them is a
/// fidelity judgement**: whether a product's gates survive eight bits is
/// `squallar_radar`'s `CodePlane::build`'s question and is settled before a
/// payload gets here. These are the things this module cannot upload.
///
/// **Nor is any of them the payload's own self-description.** Whether a
/// [`UiSweep`]'s level offsets, code length, table length and edge count agree
/// with the shape it declares is [`UiSweep::is_well_formed`]'s question, asked
/// at the draw fork one crate up and counted there as its own refusal. These
/// are the further things a *texture upload* needs and that question does not
/// cover: the resolution caps, a usable gate depth, and one site per callback.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FanRefusal {
    /// Zero radials, zero gates, or past [`MAX_POLAR_RADIALS`] /
    /// [`MAX_POLAR_GATES`].
    Shape { radials: usize, gates: usize },
    /// The code buffer is not the length the shape's full chain declares.
    CodeBytes { got: usize, want: usize },
    /// The table is not [`POLAR_LUT_BYTES`].
    LutBytes { got: usize, want: usize },
    /// The drawn-edge table does not carry one entry per radial.
    EdgeCount { got: usize, want: usize },
    /// A gate depth of zero or less, which would make the gate index a divide
    /// by zero.
    GateInterval(f32),
    /// Two sweeps of one callback disagree about where the radar is or about
    /// which sphere a ground range is taken on.
    ///
    /// **A callback's `Locals` block holds one site and one radius**, shared by
    /// every sweep it draws, because a pane's fan is one radar's cuts. A span
    /// that disagreed would draw the second sweep's gates at the first sweep's
    /// site — a plausible picture of the wrong place — so it is refused where
    /// the disagreement can be seen rather than resolved by taking whichever
    /// sweep happens to be first.
    Site,
}

/// **Whether this module can upload one payload**, and nothing about whether it
/// should be drawn.
///
/// Refuses, never truncates and never panics. The caller is
/// [`RadarFanBridge::payload`], which declines the whole draw on the first
/// refusal — the egui side counts that as
/// `squallar_egui::radar_fan::FanRefusal::PainterDeclined`, so a refusal here
/// is a hole in the picture the always-on ledger can report from a running app
/// rather than a silent empty pane.
pub fn admit(sweep: &UiSweep) -> Result<(), FanRefusal> {
    let radials = sweep.radials as usize;
    let gates = sweep.gates as usize;
    if radials == 0 || gates == 0 || radials > MAX_POLAR_RADIALS || gates > MAX_POLAR_GATES {
        return Err(FanRefusal::Shape { radials, gates });
    }
    // Clamped the way the upload loop clamps it: a payload declaring more
    // levels than the shape has is uploaded to the depth that exists, so the
    // length this checks is the length the loop will read.
    let levels = sweep.levels().clamp(1, full_mip_levels(radials, gates));
    let want = chain_bytes(radials, gates, levels);
    if sweep.codes.len() != want {
        return Err(FanRefusal::CodeBytes {
            got: sweep.codes.len(),
            want,
        });
    }
    if sweep.lut_rgba.len() != POLAR_LUT_BYTES {
        return Err(FanRefusal::LutBytes {
            got: sweep.lut_rgba.len(),
            want: POLAR_LUT_BYTES,
        });
    }
    if sweep.edges.len() != radials {
        return Err(FanRefusal::EdgeCount {
            got: sweep.edges.len(),
            want: radials,
        });
    }
    let scalars = sweep_scalars(sweep);
    let unusable = |v: f32| !v.is_finite() || v <= 0.0;
    if unusable(scalars.gate_interval_slant_km) {
        return Err(FanRefusal::GateInterval(scalars.gate_interval_slant_km));
    }
    if unusable(scalars.gate_interval_km) {
        return Err(FanRefusal::GateInterval(scalars.gate_interval_km));
    }
    Ok(())
}

/// **What identifies one payload to the store**, and therefore what residency
/// is keyed by: the address of the `Arc`'s own allocation.
///
/// **Exact rather than merely likely, and a [`Weak`] is what makes it so.** The
/// store holds a weak handle beside every resident sweep, and a `Weak` keeps
/// the allocation alive after the payload inside it is dropped — so no later
/// `Arc<UiSweep>` can be handed an address a resident entry still names, and
/// the reuse that would draw one sweep's codes under another sweep's key
/// cannot occur. `squallar_egui::radar_fan::FanSweep`'s own doc states
/// residency as pointer identity for this reason; `squallar_egui::pane`'s
/// `RadarSurface::key` identifies the same object the same way.
fn sweep_key(sweep: &Arc<UiSweep>) -> usize {
    Arc::as_ptr(sweep) as usize
}

/// The scalars one sweep is drawn and decoded by, in the lanes the WGSL
/// `Sweep` block declares.
///
/// **Every geodesy figure is read off the payload and none is derived here.**
/// This crate may not depend on the one that defines the spheres, and the
/// workspace has exactly one definition of each horizontal geodesy figure —
/// `squallar-radar/tests/geodesy_one_definition.rs` scans every `.rs` and
/// `.wgsl` for a second spelling. The two ground radii and the elevation
/// travel in [`squallar_egui::radar_fan::FanGeometry`]; what happens below is
/// arithmetic over them and never a conversion of its own.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FanScalars {
    first_gate_km: f32,
    reach_km: f32,
    first_gate_slant_km: f32,
    gate_interval_slant_km: f32,
    /// One gate's GROUND depth, km — the mip selection's denominator, and a
    /// different number from the slant one above. The disc's own two ground
    /// radii over the gates between them, which is the only reading of it that
    /// cannot disagree with where the mesh's rings were placed.
    gate_interval_km: f32,
    elev_rad: f32,
    has_elevation: u32,
    reach_gates: u32,
    re_eff_km: f32,
}

fn sweep_scalars(sweep: &UiSweep) -> FanScalars {
    let g = sweep.geometry;
    // Zero gates would be a divide, and `admit` refuses `reach_gates == 0`
    // through the well-formedness the draw fork already asked — but this
    // function is what `admit` reads the answer out of, so it runs first. A
    // zero here yields a non-finite depth, which is exactly what the
    // `GateInterval` arm refuses.
    let gate_interval_km = (g.reach_km - g.first_gate_km) / f64::from(g.reach_gates);
    FanScalars {
        first_gate_km: g.first_gate_km as f32,
        reach_km: g.reach_km as f32,
        first_gate_slant_km: g.first_gate_slant_km as f32,
        gate_interval_slant_km: g.gate_interval_slant_km as f32,
        gate_interval_km: gate_interval_km as f32,
        // Zero and never a NaN for the sweep whose ranges are ground ranges
        // already: a uniform lane carrying a NaN is a value every arithmetic
        // path has to be checked against. `has_elevation` is what the shader
        // reads to know the lane is meaningless.
        elev_rad: g.elevation_deg.unwrap_or(0.0).to_radians() as f32,
        has_elevation: u32::from(g.elevation_deg.is_some()),
        reach_gates: g.reach_gates,
        re_eff_km: g.effective_radius_km as f32,
    }
}

/// Bytes a `radials x gates` chain of `levels` levels occupies at one byte a
/// code.
///
/// The producer's own arithmetic, restated here because the store has to slice
/// a buffer by it. `the_chain_arithmetic_is_the_producers` in
/// `tests/radar_fan_gpu.rs` holds it against `CodePlane`.
pub fn chain_bytes(radials: usize, gates: usize, levels: usize) -> usize {
    let mut total = 0usize;
    for level in 0..levels {
        let shift = u32::try_from(level).unwrap_or(u32::MAX);
        let r = radials.checked_shr(shift).unwrap_or(0).max(1);
        let g = gates.checked_shr(shift).unwrap_or(0).max(1);
        total += r * g;
    }
    total
}

/// Where one pane is looking, on the frame a fan is drawn.
///
/// **In points, and converted to pixels in `prepare`.** egui turns the
/// callback's rect into the render pass's viewport by a rounding of its own
/// ([`ViewportInPixels::from_points`], in `epaint`), and clip space maps onto
/// whatever that rounding produced. A caller narrowing to pixels here would be
/// a second rounding, off by up to a pixel from the one egui applied; so this
/// carries the points and [`prepare_locals`] reproduces egui's arithmetic from
/// the renderer's own [`egui_wgpu::ScreenDescriptor`].
///
/// **Every geodesy figure is handed in**, for the reason [`admit`] names: this
/// crate may not name the sphere.
///
/// [`ViewportInPixels::from_points`]: egui::epaint::ViewportInPixels::from_points
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FanView {
    /// The callback's own rect, in points — the pane's map rect, which is what
    /// egui turns into the viewport.
    pub rect: egui::Rect,
    /// The site's screen position, in points, in the frame [`Self::rect`] is
    /// in. Projected in `f64` on the CPU and arriving already reduced, so the
    /// shader never forms a difference of two `O(1)` projected coordinates.
    pub site_pt: egui::Pos2,
    /// Points the whole Mercator world spans at this zoom.
    pub world_pt: f32,
    /// The site's latitude, degrees. Reduced to its sine, cosine and Mercator
    /// y in `f64` below, so the shader receives three numbers it only ever
    /// differences against.
    pub site_lat_deg: f64,
    /// Ground kilometres one screen **point** covers. Divided by the frame's
    /// pixels per point below, because the level selection is a question about
    /// pixels and a points figure would pick a level too fine by that factor
    /// on every high-DPI display.
    pub km_per_pt: f32,
    /// The sphere ground range is measured on, km.
    pub earth_radius_km: f32,
    /// Paint-time layer opacity, 0-1.
    pub opacity: f32,
}

/// One [`FanView`] against one frame's screen, in the byte layout the WGSL
/// `Locals` block declares.
///
/// Assembled field by field rather than cast from a `repr(C)` struct: this
/// crate forbids `unsafe`, and forty-eight bytes once per draw is not where a
/// frame is spent.
///
/// The viewport arithmetic is `epaint`'s `ViewportInPixels::from_points`, term
/// for term: round each edge to whole physical pixels, clamp to the screen,
/// and take the difference. Not an approximation of it — the same expression,
/// because the pass's viewport is set from that function and the shader's clip
/// space maps onto whatever it produced.
fn prepare_locals(
    view: &FanView,
    screen: &egui_wgpu::ScreenDescriptor,
) -> [u8; LOCALS_BYTES as usize] {
    let ppp = screen.pixels_per_point;
    let [screen_w, screen_h] = screen.size_in_pixels;
    let (screen_w, screen_h) = (screen_w as f32, screen_h as f32);
    let left = (ppp * view.rect.min.x).round().clamp(0.0, screen_w);
    let right = (ppp * view.rect.max.x).round().clamp(left, screen_w);
    let top = (ppp * view.rect.min.y).round().clamp(0.0, screen_h);
    let bottom = (ppp * view.rect.max.y).round().clamp(top, screen_h);

    let (sin_lat0, cos_lat0) = view.site_lat_deg.to_radians().sin_cos();
    // Web Mercator's y is `atanh(sin lat)`. Formed in f64 and handed over,
    // because the fragment stage adds a small offset to it and inverts.
    let merc_y = sin_lat0.clamp(-1.0, 1.0).atanh();
    // A zero-scale frame is one nothing can be placed on; the LOD it selects
    // is level 0, which is the finest and never the wrong picture.
    let km_per_px = if ppp.is_finite() && ppp > 0.0 {
        view.km_per_pt / ppp
    } else {
        0.0
    };
    let lanes: [[u8; 4]; 12] = [
        (ppp * view.site_pt.x - left).to_ne_bytes(),
        (ppp * view.site_pt.y - top).to_ne_bytes(),
        (right - left).to_ne_bytes(),
        (bottom - top).to_ne_bytes(),
        (ppp * view.world_pt).to_ne_bytes(),
        view.opacity.to_ne_bytes(),
        km_per_px.to_ne_bytes(),
        (sin_lat0 as f32).to_ne_bytes(),
        (cos_lat0 as f32).to_ne_bytes(),
        (merc_y as f32).to_ne_bytes(),
        view.earth_radius_km.to_ne_bytes(),
        (1.0f32 / view.earth_radius_km).to_ne_bytes(),
    ];
    let mut out = [0u8; LOCALS_BYTES as usize];
    for (lane, bytes) in lanes.iter().enumerate() {
        out[lane * 4..lane * 4 + 4].copy_from_slice(bytes);
    }
    out
}

/// One sweep's uniform block: the drawn-edge table in radians, then the
/// scalars.
///
/// Surplus sectors are left at `(0, 0)`, which is `lo == hi` and therefore two
/// degenerate triangles — the mechanism that lets one canonical mesh serve
/// every sweep shape.
fn sweep_bytes(sweep: &UiSweep, levels: usize) -> Vec<u8> {
    let mut out = vec![0u8; SWEEP_UNIFORM_BYTES as usize];
    for (radial, [lo, hi]) in sweep.edges.iter().enumerate() {
        let at = radial * 8;
        out[at..at + 4].copy_from_slice(&lo.to_radians().to_ne_bytes());
        out[at + 4..at + 8].copy_from_slice(&hi.to_radians().to_ne_bytes());
    }
    let s = sweep_scalars(sweep);
    let tail = EDGE_VEC4S * 16;
    let lanes: [[u8; 4]; 12] = [
        s.first_gate_km.to_ne_bytes(),
        s.reach_km.to_ne_bytes(),
        s.first_gate_slant_km.to_ne_bytes(),
        s.gate_interval_slant_km.to_ne_bytes(),
        s.elev_rad.to_ne_bytes(),
        s.re_eff_km.to_ne_bytes(),
        (1.0f32 / s.re_eff_km).to_ne_bytes(),
        s.gate_interval_km.to_ne_bytes(),
        s.has_elevation.to_ne_bytes(),
        sweep.radials.to_ne_bytes(),
        s.reach_gates.to_ne_bytes(),
        (levels as u32).to_ne_bytes(),
    ];
    for (lane, bytes) in lanes.iter().enumerate() {
        let at = tail + lane * 4;
        out[at..at + 4].copy_from_slice(bytes);
    }
    out
}

/// One pass's views, gathered so the ring is written once per pass rather than
/// once per draw. `tile_mesh::PlacementBatch`'s shape and its reason:
/// `queue.write_buffer` is a staging allocation and a deferred destroy, not a
/// memcpy.
struct ViewBatch {
    stride: u32,
    cursor: u32,
    first: u32,
    bytes: Vec<u8>,
}

impl ViewBatch {
    fn new(stride: u32) -> Self {
        Self {
            stride,
            cursor: 0,
            first: 0,
            bytes: Vec::new(),
        }
    }

    fn push(&mut self, locals: [u8; LOCALS_BYTES as usize], write: impl FnOnce(u64, &[u8])) -> u32 {
        let slot = self.cursor;
        if self.bytes.is_empty() {
            self.first = slot;
        }
        self.bytes.extend_from_slice(&locals);
        let padded = self.bytes.len() + (self.stride as usize - LOCALS_BYTES as usize);
        self.bytes.resize(padded, 0);
        self.cursor = (self.cursor + 1) % RING_SLOTS;
        if self.cursor == 0 {
            self.flush(write);
        }
        slot
    }

    fn flush(&mut self, write: impl FnOnce(u64, &[u8])) -> bool {
        if self.bytes.is_empty() {
            return false;
        }
        write(u64::from(self.first) * u64::from(self.stride), &self.bytes);
        self.bytes.clear();
        true
    }
}

/// One sweep, resident on the GPU.
struct Resident {
    /// The code plane, the table and the sweep's own uniform, in one group.
    bind_group: wgpu::BindGroup,
    /// Held so the textures outlive the bind group.
    _codes: wgpu::Texture,
    _lut: wgpu::Texture,
    _uniform: wgpu::Buffer,
    bytes: u64,
    /// The owner's handle, seen from here. Dead means the sweep is gone and so
    /// are these textures, next sweep — and, while it is held, no later
    /// payload can be allocated at the address this entry is keyed by. See
    /// [`sweep_key`].
    alive: Weak<UiSweep>,
}

/// What the fan draws need across frames: the pipeline, the canonical mesh, the
/// uniform ring, and the sweeps that are resident.
pub struct RadarFanStore {
    pipeline: wgpu::RenderPipeline,
    sweep_layout: wgpu::BindGroupLayout,
    frame_bind_group: wgpu::BindGroup,
    ring: wgpu::Buffer,
    /// Bytes between two ring slots — the adapter's uniform offset alignment,
    /// never smaller than one `Locals`.
    stride: u32,
    batch: ViewBatch,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    resident: HashMap<usize, Resident>,
    /// The sweeps whose textures exist and whose bytes have not crossed yet,
    /// and the ring they cross through. See [`chain_staging`].
    staging: ChainStaging,
    resident_bytes: u64,
    uploads: u64,
    upload_bytes: u64,
    views: u64,
    ring_writes: u64,
    /// Render-pass calls `paint` recorded, and the paints that recorded them.
    /// Always on: the six-call claim is the reason this path exists rather than
    /// a callback per radial, and a claim nothing counts is prose.
    ///
    /// Atomics because `paint` is handed the store through a shared reference,
    /// which is also why they are the only counters here that are.
    recorded: AtomicU64,
    paints: AtomicU64,
    /// Mesh indices those draws submitted.
    ///
    /// Always on, and for the same reason as the two above. "A draw submits
    /// the sweep's own sectors and not the whole canonical mesh" is a claim
    /// **the picture cannot report**: a surplus sector's triangles are
    /// degenerate whether or not they are submitted, so a range that went back
    /// to the whole mesh would draw an identical frame and only this number
    /// would move.
    drawn: AtomicU64,
    /// Callbacks that found no store — the one wiring mistake that produces an
    /// ordinary-looking map with no radar in it. Counted rather than logged:
    /// this crate declares no `log` dependency.
    store_missing: AtomicU64,
    swept_pass: Option<u64>,
}

impl RadarFanStore {
    /// Build the pipeline and the canonical mesh for a pass with these
    /// attachments.
    ///
    /// The chain upload route is the device's own answer —
    /// [`chain_staging::available`] — so a build takes the ring where the
    /// adapter has one and the window where it does not, with no `cfg` between
    /// them.
    pub fn new(device: &wgpu::Device, attachments: crate::egui_renderer::AttachmentConfig) -> Self {
        Self::with_staging(device, attachments, chain_staging::available(device))
    }

    /// [`Self::new`] with the chain upload route named rather than asked for.
    ///
    /// **Both values are production routes**: `false` is what every device
    /// without [`crate::staging_ring::STAGING_RING_FEATURE`] takes, which is
    /// all of the web. It is spelled out here so a suite on a device that has
    /// a ring can draw the same sweep both ways and hold the two pictures
    /// against each other, which is the only place the web arm's route can be
    /// checked on this hardware.
    pub fn with_staging(
        device: &wgpu::Device,
        attachments: crate::egui_renderer::AttachmentConfig,
        staged: bool,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("radar fan"),
            source: wgpu::ShaderSource::Wgsl(RADAR_FAN_WGSL.into()),
        });

        let frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("radar fan locals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(LOCALS_BYTES),
                },
                count: None,
            }],
        });
        let sweep_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("radar fan sweep"),
            entries: &[
                // The code plane. `Uint` and not `Float`: a code is an index
                // and nothing may interpolate between two of them.
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Uint,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // The table. No sampler accompanies it: every fetch is a
                // `textureLoad` at an explicit level.
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(SWEEP_UNIFORM_BYTES),
                    },
                    count: None,
                },
            ],
        });

        let stride = align_up(
            LOCALS_BYTES as u32,
            device.limits().min_uniform_buffer_offset_alignment,
        );
        let ring = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("radar fan locals ring"),
            size: u64::from(stride) * u64::from(RING_SLOTS),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let frame_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("radar fan locals"),
            layout: &frame_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &ring,
                    offset: 0,
                    size: wgpu::BufferSize::new(LOCALS_BYTES),
                }),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("radar fan"),
            bind_group_layouts: &[Some(&frame_layout), Some(&sweep_layout)],
            immediate_size: 0,
        });

        // egui picks its fragment entry point off the target's sRGB-ness and
        // this must pick the same one, or every radar pixel is gamma-shifted
        // against the map beneath it.
        let fragment_entry = if attachments.color_format.is_srgb() {
            "fs_main_linear_framebuffer"
        } else {
            "fs_main_gamma_framebuffer"
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("radar fan"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: 4,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Uint32,
                        offset: 0,
                        shader_location: 0,
                    }],
                }],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: attachments
                .depth_format
                .map(|format| wgpu::DepthStencilState {
                    format,
                    // egui's own pipeline writes no depth and compares Always;
                    // this draws in the same pass and must not start.
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::Always),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
            multisample: wgpu::MultisampleState {
                count: attachments.msaa_samples.max(1),
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(fragment_entry),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                // egui's blend state, copied rather than chosen.
                targets: &[Some(wgpu::ColorTargetState {
                    format: attachments.color_format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::OneMinusDstAlpha,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        // Written at creation rather than through the queue: the mesh is a
        // pure function of the packing, it never changes with the data, and a
        // mapped-at-creation buffer needs no queue and no first-frame branch.
        let (vertex_bytes, index_bytes) = disk_mesh();
        let vertices = mapped_buffer(device, wgpu::BufferUsages::VERTEX, &vertex_bytes);
        let indices = mapped_buffer(device, wgpu::BufferUsages::INDEX, &index_bytes);

        Self {
            pipeline,
            sweep_layout,
            frame_bind_group,
            ring,
            stride,
            batch: ViewBatch::new(stride),
            vertices,
            indices,
            resident: HashMap::new(),
            staging: ChainStaging::new(staged),
            resident_bytes: 0,
            uploads: 0,
            upload_bytes: 0,
            views: 0,
            ring_writes: 0,
            recorded: AtomicU64::new(0),
            paints: AtomicU64::new(0),
            drawn: AtomicU64::new(0),
            store_missing: AtomicU64::new(0),
            swept_pass: None,
        }
    }

    /// Give back every sweep the owner has let go of. Once per pass.
    fn sweep(&mut self, pass_nr: u64) {
        if self.swept_pass == Some(pass_nr) {
            return;
        }
        self.swept_pass = Some(pass_nr);
        let mut bytes = 0u64;
        self.resident.retain(|_, entry| {
            if entry.alive.strong_count() > 0 {
                return true;
            }
            bytes += entry.bytes;
            false
        });
        self.resident_bytes -= bytes;
    }

    /// Make one sweep resident, uploading it if this is the first frame it has
    /// been drawn on.
    ///
    /// **Nothing is uploaded twice and nothing is copied.** The payload is the
    /// one `squallar_egui` built off the frame thread, held here through the
    /// callback's `Arc`; a loop step that returns to a sweep already resident
    /// finds its key and writes no bytes at all.
    fn ensure(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, sweep: &Arc<UiSweep>) {
        let key = sweep_key(sweep);
        if self.resident.contains_key(&key) {
            return;
        }
        let (radials, gates) = (sweep.radials as usize, sweep.gates as usize);
        // The payload was admitted by the bridge that built this callback, but
        // the levels are clamped again rather than trusted: this is the number
        // the texture is created with, and a descriptor that promised more
        // levels than the loop below writes would leave a level of the chain
        // undefined for the shader to read.
        //
        // **`full_mip_levels` is wgpu's `Extent3d::max_mips` and each level's
        // shape is its `mip_level_size`** — the same arithmetic, not a
        // conservative bound on it, which is what
        // `the_chain_arithmetic_is_the_producers` holds against wgpu itself.
        // It was not, until 2026-09-08: the producer ceil-halved, so a
        // 720 × 1832 sweep asked for twelve levels of a texture that admits
        // eleven, `create_texture` refused it, and every frame after that
        // recorded a `set_bind_group` against the invalid bind group the
        // refusal left behind.
        let levels = sweep.levels().clamp(1, full_mip_levels(radials, gates));
        let codes = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("radar fan codes"),
            size: wgpu::Extent3d {
                width: gates as u32,
                height: radials as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: levels as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let lut = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("radar fan lut"),
            size: wgpu::Extent3d {
                width: POLAR_LUT_ENTRIES as u32,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Not `Rgba8UnormSrgb`: the table holds the bytes
            // `get_color_for_value` produced, which are gamma-space sRGB, and
            // the fragment applies whichever gamma convention the target's own
            // format selects. Letting the sampler decode would apply it twice
            // on one of the two arms.
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        // **The chain and the table do not cross here.** Both textures are
        // filed and the whole pass's filings move together out of one staging
        // slot in `finish_prepare`, which runs after every callback's prepare
        // and before the render pass. See [`chain_staging`] for what that
        // buys and why one slot serves the pass rather than one serving a
        // sweep.
        self.staging.file(Pending {
            sweep: Arc::clone(sweep),
            codes: codes.clone(),
            lut: lut.clone(),
            levels,
        });

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("radar fan sweep"),
            size: SWEEP_UNIFORM_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uniform, 0, &sweep_bytes(sweep, levels));

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("radar fan sweep"),
            layout: &self.sweep_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &codes.create_view(&wgpu::TextureViewDescriptor::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(
                        &lut.create_view(&wgpu::TextureViewDescriptor::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });

        // **What the card is holding for this sweep**, off the payload's own
        // vectors rather than recomputed from its shape: the whole chain, the
        // table and the drawn-edge lanes, which is what
        // `squallar_egui::radar_fan::FanSweep::resident_bytes` measures.
        let bytes = sweep.resident_bytes() as u64;
        self.resident_bytes += bytes;
        self.uploads += 1;
        self.upload_bytes += bytes;
        self.resident.insert(
            key,
            Resident {
                bind_group,
                _codes: codes,
                _lut: lut,
                _uniform: uniform,
                bytes,
                alive: Arc::downgrade(sweep),
            },
        );
    }

    /// Lay one draw's view into the pass's batch and answer its ring slot.
    fn slot(
        &mut self,
        queue: &wgpu::Queue,
        view: &FanView,
        screen: &egui_wgpu::ScreenDescriptor,
    ) -> u32 {
        self.views += 1;
        let ring = &self.ring;
        let ring_writes = &mut self.ring_writes;
        self.batch
            .push(prepare_locals(view, screen), |offset, bytes| {
                queue.write_buffer(ring, offset, bytes);
                *ring_writes += 1;
            })
    }

    /// Write the pass's gathered views into the ring as one `write_buffer`.
    fn flush(&mut self, queue: &wgpu::Queue) {
        let ring = &self.ring;
        let ring_writes = &mut self.ring_writes;
        self.batch.flush(|offset, bytes| {
            queue.write_buffer(ring, offset, bytes);
            *ring_writes += 1;
        });
    }

    /// Views laid into the ring, and the `write_buffer` calls that carried
    /// them — one per pass with fan draws in it, not one per draw.
    pub fn view_writes(&self) -> (u64, u64) {
        (self.views, self.ring_writes)
    }

    /// Render-pass calls recorded, and the paints that recorded them. Six for a
    /// one-sweep pane, and two more per extra sweep.
    pub fn recorded_calls(&self) -> (u64, u64) {
        (
            self.recorded.load(Ordering::Relaxed),
            self.paints.load(Ordering::Relaxed),
        )
    }

    /// Mesh indices this store's draws have submitted, since the process
    /// started. See the field.
    pub fn indices_drawn(&self) -> u64 {
        self.drawn.load(Ordering::Relaxed)
    }

    /// Callbacks that found no store in the callback resources.
    pub fn store_missing(&self) -> u64 {
        self.store_missing.load(Ordering::Relaxed)
    }

    /// Fold the process-wide store-less tally into this store, so a test that
    /// holds one store reads the same number the ledger does.
    pub fn note_storeless(&self) {
        self.store_missing.store(
            STORELESS_PREPARES.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
    }

    /// Bytes this store is holding for sweeps.
    pub fn resident_bytes(&self) -> u64 {
        self.resident_bytes
    }

    /// Sweeps this store is holding.
    pub fn resident_sweeps(&self) -> usize {
        self.resident.len()
    }

    /// Texture and buffer writes this store has made, and their bytes — one set
    /// per sweep lifetime, never per frame.
    pub fn uploads(&self) -> (u64, u64) {
        (self.uploads, self.upload_bytes)
    }

    /// Which route this store's chain uploads took. See
    /// [`chain_staging::ChainStagingTotals`] for the denominator.
    pub fn chain_staging(&self) -> ChainStagingTotals {
        self.staging.totals()
    }

    /// Pinned host memory this store's staging ring is holding — zero until it
    /// has uploaded a sweep, and zero for the life of a store on a device with
    /// no ring.
    pub fn staging_host_bytes(&self) -> usize {
        self.staging.host_bytes()
    }

    /// **Move every sweep filed by this pass's prepares.** Once a pass, from
    /// `finish_prepare`.
    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        self.staging.drain(device, queue);
    }
}

/// The canonical disk mesh, as its two buffers' bytes.
///
/// Built from the packing and nothing else: a vertex carries `(sector, ring,
/// side)` and the shader derives every coordinate from the sweep's own edge
/// table, so this never changes with the data and is written once per store.
fn disk_mesh() -> (Vec<u8>, Vec<u8>) {
    let mut vertices = Vec::with_capacity(MESH_VERTICES * 4);
    for sector in 0..SECTORS as u32 {
        for ring in 0..=RINGS as u32 {
            for side in 0..2u32 {
                vertices.extend_from_slice(&pack_vertex(sector, ring, side).to_ne_bytes());
            }
        }
    }
    let mut indices = Vec::with_capacity(MESH_INDICES * 4);
    for sector in 0..SECTORS as u32 {
        let base = sector * (RINGS as u32 + 1) * 2;
        for ring in 0..RINGS as u32 {
            let near = base + ring * 2;
            let far = near + 2;
            for corner in [near, far, near + 1, near + 1, far, far + 1] {
                indices.extend_from_slice(&corner.to_ne_bytes());
            }
        }
    }
    (vertices, indices)
}

/// A buffer created with its contents already in it.
///
/// `mapped_at_creation` rather than `queue.write_buffer`, because the mesh is
/// written exactly once per store and this needs no queue — which is what lets
/// [`RadarFanStore::new`] take a device alone.
fn mapped_buffer(
    device: &wgpu::Device,
    usage: wgpu::BufferUsages,
    contents: &[u8],
) -> wgpu::Buffer {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("radar fan mesh"),
        size: contents.len() as u64,
        usage,
        mapped_at_creation: true,
    });
    buffer
        .slice(..)
        .get_mapped_range_mut()
        .copy_from_slice(contents);
    buffer.unmap();
    buffer
}

/// Round `value` up to a multiple of `alignment`.
fn align_up(value: u32, alignment: u32) -> u32 {
    let alignment = alignment.max(1);
    value.div_ceil(alignment) * alignment
}

/// One pane's fan, on one frame.
///
/// **A span of sweeps, not one sweep.** Every sweep of a pane draws at the same
/// view, through the same pipeline and the same mesh, so one callback draws all
/// of them — and a callback is a primitive boundary in the egui stream whatever
/// it goes on to record. A loop step then swaps a bind group and writes
/// nothing.
pub struct RadarFanCallback {
    /// The sweeps to draw, front to back. Keeps them alive until `prepare` has
    /// read them, and is what the store's weak handles are taken from.
    ///
    /// **The `squallar_egui` payload itself, shared and never copied.** The
    /// plane was built off the frame thread and the pane is holding it; this
    /// carries the same allocation, so a fan costs the process one plane and
    /// not two.
    pub sweeps: Vec<Arc<UiSweep>>,
    pub view: FanView,
    pub pass_nr: u64,
    /// Written by `prepare`, read by `paint`. Every prepare of a frame runs
    /// before any paint of it, so one `AtomicU32` carries the slot across
    /// without the store having to hold it.
    slot: AtomicU32,
}

impl RadarFanCallback {
    /// One pane's fan. `None` for an empty span, which would be a callback that
    /// records a bind group and draws nothing — the primitive boundary this
    /// path exists to spend only on pixels.
    pub fn new(sweeps: Vec<Arc<UiSweep>>, view: FanView, pass_nr: u64) -> Option<Self> {
        if sweeps.is_empty() {
            return None;
        }
        Some(Self {
            sweeps,
            view,
            pass_nr,
            slot: AtomicU32::new(0),
        })
    }

    /// The payload `egui_wgpu` downcasts. [`RadarFanBridge`] is the seam that
    /// publishes it; the rect it is given here is `ZERO` and unread, because
    /// the rect that matters is the one the pane's `Shape::Callback` carries
    /// and that is what egui turns into the viewport.
    pub fn payload(self) -> Arc<dyn Any + Send + Sync> {
        egui_wgpu::Callback::new_paint_callback(egui::Rect::ZERO, self).callback
    }
}

impl egui_wgpu::CallbackTrait for RadarFanCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(store) = callback_resources.get_mut::<RadarFanStore>() else {
            // Counted rather than said: this crate declares no `log`
            // dependency, and a store-less callback is the one wiring mistake
            // that leaves an ordinary-looking map with no radar on it.
            STORELESS_PREPARES.fetch_add(1, Ordering::Relaxed);
            return Vec::new();
        };
        store.sweep(self.pass_nr);
        for sweep in &self.sweeps {
            store.ensure(device, queue, sweep);
        }
        let slot = store.slot(queue, &self.view, screen_descriptor);
        self.slot.store(slot, Ordering::Relaxed);
        Vec::new()
    }

    fn finish_prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        // Every callback of the pass is asked; the first finds the batch full
        // and the filings waiting, the rest find both empty.
        //
        // **Before any `paint`, which is what makes the deferral safe**:
        // `egui_wgpu` runs every callback's `finish_prepare` after every
        // `prepare` and before the render pass, so no sweep is ever drawn from
        // a texture whose bytes are still on this side of the queue.
        if let Some(store) = callback_resources.get_mut::<RadarFanStore>() {
            store.upload(device, queue);
            store.flush(queue);
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &egui_wgpu::CallbackResources,
    ) {
        let Some(store) = callback_resources.get::<RadarFanStore>() else {
            STORELESS_PREPARES.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let mut recorded = 0u64;
        let mut drawn = 0u64;
        // **No `set_viewport`.** `tile_mesh` overrides egui's courtesy viewport
        // because its geometry is already placed in whole-screen points; the
        // fan does not, because the callback's rect IS the pane's map rect and
        // the vertex stage emits clip space inside it.
        render_pass.set_pipeline(&store.pipeline);
        render_pass.set_bind_group(
            0,
            &store.frame_bind_group,
            &[self.slot.load(Ordering::Relaxed) * store.stride],
        );
        render_pass.set_vertex_buffer(0, store.vertices.slice(..));
        render_pass.set_index_buffer(store.indices.slice(..), wgpu::IndexFormat::Uint32);
        recorded += 4;
        for sweep in &self.sweeps {
            // `continue`, not `return`: a pane can be missing one sweep's
            // residency and hold the others, and they still draw.
            let Some(resident) = store.resident.get(&sweep_key(sweep)) else {
                continue;
            };
            render_pass.set_bind_group(1, &resident.bind_group, &[]);
            // **The sweep's own sectors, not the whole canonical mesh.** The
            // mesh carries one per radial `MAX_POLAR_RADIALS` admits and a
            // real sweep declares half that; the rest are degenerate by
            // construction and were being vertex-shaded anyway. See
            // [`sector_indices`], which is also why this range is a prefix and
            // needs no second index buffer, no rebuild and no cache.
            let range = sector_indices(sweep.radials);
            render_pass.draw_indexed(0..range, 0, 0..1);
            recorded += 2;
            drawn += u64::from(range);
        }
        store.recorded.fetch_add(recorded, Ordering::Relaxed);
        store.paints.fetch_add(1, Ordering::Relaxed);
        store.drawn.fetch_add(drawn, Ordering::Relaxed);
    }
}

/// **The seam's renderer side**: turns one pane's fan draw into the payload
/// `egui_wgpu` downcasts, or declines it.
///
/// Holds nothing, exactly as [`crate::tile_mesh::TileMeshBridge`] holds
/// nothing: the store is in the callback resources and the textures are in the
/// store, so this is installed once and never has to be replaced when a sweep
/// arrives or goes.
///
/// **Declining is visible.** `squallar_egui`'s draw fork counts a `None` from
/// here as `FanRefusal::PainterDeclined` in its always-on ledger, so every
/// refusal below is a hole in the picture a running app can report rather than
/// a pane that quietly draws nothing.
#[derive(Default)]
pub struct RadarFanBridge;

impl squallar_egui::radar_fan::RadarFanPainter for RadarFanBridge {
    fn payload(
        &self,
        draw: squallar_egui::radar_fan::FanDraw<'_>,
    ) -> Option<Arc<dyn Any + Send + Sync>> {
        // An empty span would be a callback that records a bind group and
        // draws nothing — a primitive boundary bought for no pixels, which is
        // the cost this whole path exists to remove.
        let first = draw.sweeps.first()?;
        for sweep in draw.sweeps.iter() {
            admit(sweep).ok()?;
            // One `Locals` block per callback carries one site and one sphere.
            // See `FanRefusal::Site`.
            let (a, b) = (sweep.geometry, first.geometry);
            if a.site_lat != b.site_lat
                || a.site_lon != b.site_lon
                || a.earth_radius_km != b.earth_radius_km
            {
                return None;
            }
        }
        let view = FanView {
            rect: draw.view.rect,
            site_pt: draw.view.site_px,
            world_pt: draw.view.world_px as f32,
            site_lat_deg: first.geometry.site_lat,
            km_per_pt: draw.view.km_per_px as f32,
            earth_radius_km: first.geometry.earth_radius_km as f32,
            opacity: draw.opacity,
        };
        RadarFanCallback::new(draw.sweeps.to_vec(), view, draw.pass_nr)
            .map(RadarFanCallback::payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A sweep's sectors are the canonical mesh's first `radials`, and
    /// `sector_indices` names exactly them.**
    ///
    /// The whole of why a fan needs no second index buffer and no rebuild to
    /// stop shading sectors it has no radial for. Decoded out of
    /// [`disk_mesh`]'s own bytes through [`pack_vertex`]'s own fields, so this
    /// is the mesh the pipeline is fed and not a second description of it.
    ///
    /// **The property an input needs to reach the defect**: a radial count
    /// BELOW [`SECTORS`]. At `radials == SECTORS` the fitted range is the whole
    /// mesh and every assertion below holds for a function that ignored its
    /// argument. Both real WSR-88D shapes declare 720 against a 1440-sector
    /// mesh, and the premise is asserted rather than assumed.
    ///
    /// The picture cannot report this: a surplus sector's drawn edges are
    /// equal, so its triangles are degenerate whether they are submitted or
    /// not, and a range that went back to the whole mesh would draw the same
    /// frame. That is why `RadarFanStore::indices_drawn` exists and why this
    /// suite reads the mesh rather than a readback.
    ///
    /// TAMPER: emit the mesh ring-major, or drop the `RINGS * 6` from
    /// [`sector_indices`], and the prefix rows go red.
    #[test]
    fn the_canonical_mesh_is_sector_major_and_a_sweep_owns_its_prefix() {
        let (vertex_bytes, index_bytes) = disk_mesh();
        let vertices: Vec<u32> = vertex_bytes
            .chunks_exact(4)
            .map(|b| u32::from_ne_bytes(b.try_into().unwrap()))
            .collect();
        let indices: Vec<u32> = index_bytes
            .chunks_exact(4)
            .map(|b| u32::from_ne_bytes(b.try_into().unwrap()))
            .collect();
        assert_eq!(vertices.len(), MESH_VERTICES);
        assert_eq!(indices.len(), MESH_INDICES);
        assert_eq!(sector_indices(SECTORS as u32) as usize, MESH_INDICES);

        // The two real WSR-88D radial counts and a coarse legacy one.
        let mut checked = 0;
        for radials in [720u32, 360, 180] {
            assert!(
                (radials as usize) < SECTORS,
                "a sweep filling the mesh cannot show that the range fits it"
            );
            let range = sector_indices(radials) as usize;
            assert!(range < MESH_INDICES, "the fitted range is the whole mesh");
            // Every index inside the range names a vertex of a sector the
            // sweep has a radial for...
            for &index in &indices[..range] {
                let sector = vertices[index as usize] & ((1 << SECTOR_BITS) - 1);
                assert!(
                    sector < radials,
                    "index {index} inside a {radials}-radial sweep's range                      names sector {sector}, which the sweep has no radial for"
                );
                checked += 1;
            }
            // ...and every index outside it names one the sweep does not, so
            // the range is exactly the sweep's sectors and never a prefix of
            // them.
            for &index in &indices[range..] {
                let sector = vertices[index as usize] & ((1 << SECTOR_BITS) - 1);
                assert!(
                    sector >= radials,
                    "index {index} outside a {radials}-radial sweep's range                      names sector {sector}, which the sweep DOES carry — the                      range drops geometry the picture needs"
                );
                checked += 1;
            }
        }
        assert_eq!(checked, 3 * MESH_INDICES, "the loop compared nothing");

        // And the saving is the one the shapes make it: both real sweeps
        // declare half the sectors the mesh carries.
        assert_eq!(
            sector_indices(720) as usize * 2,
            MESH_INDICES,
            "a 720-radial sweep no longer submits half the mesh"
        );
    }

    /// **The viewport this pass places a fan in is egui's own, at every scale
    /// factor** — not an approximation of it.
    ///
    /// The pass's viewport is set by `egui_wgpu` from
    /// `PaintCallbackInfo::viewport_in_pixels`, and the vertex stage emits clip
    /// space against the size [`prepare_locals`] wrote into `Locals`. If the
    /// two roundings differ the whole fan is offset and scaled by the
    /// difference — a picture that looks like a radar image and is drawn at the
    /// wrong place, worst at the fractional scale factors a real desktop uses.
    ///
    /// Driven at `pixels_per_point` values the hardware suite cannot reach: it
    /// renders a 256 px canvas at 1.0, where points and pixels are the same
    /// number and every conversion below is the identity.
    ///
    /// The radius comes from `squallar_geo` even though nothing here reads it:
    /// a fixture spelling one would be the second definition of the sphere
    /// that `squallar-radar/tests/geodesy_one_definition.rs` scans every `.rs`
    /// in this workspace for, and it caught this file when it did.
    ///
    /// TAMPER: use `rect.size() * ppp` for the viewport, or drop either clamp,
    /// and the fractional or off-screen rows go red.
    #[test]
    fn the_viewport_is_epaints_own_at_every_scale_factor() {
        let rect = egui::Rect::from_min_max(egui::pos2(10.5, 20.25), egui::pos2(310.5, 220.75));
        let screen = [1024u32, 768u32];
        let mut checked = 0;
        for ppp in [1.0f32, 1.25, 1.5, 2.0, 2.4, 3.0] {
            let want = egui::epaint::ViewportInPixels::from_points(&rect, ppp, screen);
            let bytes = prepare_locals(
                &FanView {
                    rect,
                    site_pt: rect.center(),
                    world_pt: 4096.0,
                    site_lat_deg: 35.0,
                    km_per_pt: 2.0,
                    earth_radius_km: squallar_geo::EARTH_RADIUS_KM as f32,
                    opacity: 1.0,
                },
                &egui_wgpu::ScreenDescriptor {
                    size_in_pixels: screen,
                    pixels_per_point: ppp,
                },
            );
            let lane = |i: usize| f32::from_ne_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());

            assert_eq!(
                [lane(2), lane(3)],
                [want.width_px as f32, want.height_px as f32],
                "the viewport size disagrees with egui's at {ppp}"
            );
            // And the site sits at the same offset inside it that egui's own
            // left/top edges put it at.
            assert_eq!(
                [lane(0), lane(1)],
                [
                    ppp * rect.center().x - want.left_px as f32,
                    ppp * rect.center().y - want.top_px as f32
                ],
                "the site's position inside the viewport disagrees at {ppp}"
            );
            // The level selection is per PIXEL, so it falls as the scale
            // factor rises. A points figure would select a level too fine by
            // exactly this factor on every high-DPI display.
            assert_eq!(lane(6), 2.0 / ppp, "km per pixel at {ppp}");
            // World span scales with it too, or the fan would be placed at one
            // zoom and sized at another.
            assert_eq!(lane(4), 4096.0 * ppp, "world pixels at {ppp}");
            checked += 1;
        }
        assert_eq!(checked, 6, "the loop compared nothing");

        // A rect running off the screen is clamped the way egui clamps it,
        // rather than emitting a viewport wider than the target.
        let off = egui::Rect::from_min_max(egui::pos2(-40.0, -10.0), egui::pos2(2000.0, 900.0));
        let want = egui::epaint::ViewportInPixels::from_points(&off, 2.0, screen);
        let bytes = prepare_locals(
            &FanView {
                rect: off,
                site_pt: egui::pos2(0.0, 0.0),
                world_pt: 4096.0,
                site_lat_deg: 35.0,
                km_per_pt: 2.0,
                earth_radius_km: squallar_geo::EARTH_RADIUS_KM as f32,
                opacity: 1.0,
            },
            &egui_wgpu::ScreenDescriptor {
                size_in_pixels: screen,
                pixels_per_point: 2.0,
            },
        );
        let lane = |i: usize| f32::from_ne_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
        assert_eq!(
            [lane(2), lane(3)],
            [want.width_px as f32, want.height_px as f32]
        );
        assert!(
            lane(2) <= screen[0] as f32 && lane(3) <= screen[1] as f32,
            "an off-screen rect produced a viewport larger than the target"
        );
        // The premise the clamp row rests on: this rect really does run off,
        // so the assertion above is the clamp firing and not an identity.
        assert!(2.0 * off.width() > screen[0] as f32);
    }
}
