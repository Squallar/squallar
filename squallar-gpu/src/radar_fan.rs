//! One radar sweep drawn as a fan of per-radial sectors, coloured on the GPU
//! by a lookup into a 256-entry table.
//!
//! The renderer half of the polar representation `docs/radar-polar-design.md`
//! describes: the sweep reaches the card as the **codes the wire carried**, one
//! byte a gate, and the colour is resolved per fragment out of a
//! `256 x 1` RGBA table instead of per gate on the CPU into a plan-view raster.
//! A surveillance tilt is 1,758,832 B of code plane against 650,388,528 B of
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
//! # Nothing installs this
//!
//! No painter publishes a [`RadarFanCallback`], no pane issues one, and no
//! budget selects the polar price. This module and
//! `tests/radar_fan_gpu.rs` are its only callers. That is deliberate and it is
//! a safety property rather than staging convenience: the polar price and the
//! plan-view raster price differ by ~369x for the same tilt, so a scene priced
//! as polar while the renderer still produces a raster would be admitted at a
//! fraction of what it then allocates. The switch belongs in the lane that
//! makes the renderer produce polar frames, keyed on what it produced.
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
}

/// The scalars one sweep is drawn and decoded by.
///
/// Every one of them is a number the producing crate already holds; none is
/// derived here. `elevation_deg` is `None` for a sweep whose two range figures
/// are **already ground ranges** — `PolarGeometry::gate_at`'s own distinction,
/// and the reason this is an `Option` and not a NaN.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FanScalars {
    /// The GROUND range of the mesh's innermost ring, km.
    pub first_gate_km: f32,
    /// The GROUND range of the mesh's outermost ring, km.
    pub reach_km: f32,
    /// Gate 0's centre ALONG THE BEAM, km.
    pub first_gate_slant_km: f32,
    /// One gate's depth ALONG THE BEAM, km.
    pub gate_interval_slant_km: f32,
    /// One gate's GROUND depth, km — the mip selection's denominator, and a
    /// different number from the one above.
    pub gate_interval_km: f32,
    /// The tilt's elevation, degrees, or `None` where the ranges are ground
    /// ranges already.
    pub elevation_deg: Option<f32>,
    /// Gates the sweep reached, which bounds the gate index.
    pub reach_gates: u32,
    /// The 4/3 effective earth radius the beam bends over, km.
    ///
    /// **Handed in rather than named.** `squallar-gpu` may not depend on the
    /// crate that defines it, and the workspace has exactly one definition of
    /// each horizontal geodesy figure —
    /// `squallar-radar/tests/geodesy_one_definition.rs` scans every `.rs` and
    /// `.wgsl` for a second spelling.
    pub re_eff_km: f32,
}

/// One sweep's CPU side: the code plane and its chain, the baked colour table,
/// the drawn azimuth of every radial, and the scalars.
///
/// # Where this type belongs
///
/// The design sites it in `squallar-egui`, so that the UI layer can hold a
/// sweep without depending on the wgpu boundary. It is here for as long as
/// nothing outside this crate names it, which is for as long as nothing
/// installs the painter; the UI-seam lane moves it and this module keeps only
/// the store.
///
/// # Residency
///
/// Held behind an `Arc`, and the store keeps a `Weak` to it. **That handle is
/// the whole eviction rule**: when the owner drops the sweep the weak handle
/// goes dead and the next pass's sweep gives the GPU textures back. Nothing
/// here has a budget of its own to disagree with the owner's.
#[derive(Clone, Debug, PartialEq)]
pub struct FanSweep {
    id: u64,
    radials: usize,
    gates: usize,
    codes: Vec<u8>,
    mip_levels: usize,
    lut: Vec<u8>,
    edges: Vec<[f32; 2]>,
    scalars: FanScalars,
}

/// A code plane and its chain, as one argument.
///
/// The four fields describe one object and are only ever handed over together;
/// `squallar_radar`'s `CodePlane` is where they come from and it holds them the
/// same way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FanPlane {
    pub radials: usize,
    pub gates: usize,
    /// The whole chain concatenated, level 0 first. Level `l` is
    /// `ceil(radials / 2^l) x ceil(gates / 2^l)` bytes, radial-major.
    pub codes: Vec<u8>,
    /// Levels in `codes`, counting level 0. `1` for a plane whose product
    /// reduces by nothing.
    pub mip_levels: usize,
}

impl FanSweep {
    /// Take a sweep, or refuse it.
    ///
    /// Refuses, never truncates and never panics.
    pub fn new(
        id: u64,
        plane: FanPlane,
        lut: Vec<u8>,
        edges: Vec<[f32; 2]>,
        scalars: FanScalars,
    ) -> Result<Self, FanRefusal> {
        let FanPlane {
            radials,
            gates,
            codes,
            mip_levels,
        } = plane;
        if radials == 0 || gates == 0 || radials > MAX_POLAR_RADIALS || gates > MAX_POLAR_GATES {
            return Err(FanRefusal::Shape { radials, gates });
        }
        let levels = mip_levels.clamp(1, full_mip_levels(radials, gates));
        let want = chain_bytes(radials, gates, levels);
        if codes.len() != want {
            return Err(FanRefusal::CodeBytes {
                got: codes.len(),
                want,
            });
        }
        if lut.len() != POLAR_LUT_BYTES {
            return Err(FanRefusal::LutBytes {
                got: lut.len(),
                want: POLAR_LUT_BYTES,
            });
        }
        if edges.len() != radials {
            return Err(FanRefusal::EdgeCount {
                got: edges.len(),
                want: radials,
            });
        }
        let unusable = |v: f32| !v.is_finite() || v <= 0.0;
        if unusable(scalars.gate_interval_slant_km) {
            return Err(FanRefusal::GateInterval(scalars.gate_interval_slant_km));
        }
        if unusable(scalars.gate_interval_km) {
            return Err(FanRefusal::GateInterval(scalars.gate_interval_km));
        }
        Ok(Self {
            id,
            radials,
            gates,
            codes,
            mip_levels: levels,
            lut,
            edges,
            scalars,
        })
    }

    /// What the store keys residency by.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Level 0's shape, as `(radials, gates)`.
    pub fn shape(&self) -> (usize, usize) {
        (self.radials, self.gates)
    }

    /// Levels in the chain, counting level 0.
    pub fn mip_levels(&self) -> usize {
        self.mip_levels
    }

    /// The scalars this sweep is drawn and decoded by.
    pub fn scalars(&self) -> FanScalars {
        self.scalars
    }

    /// Bytes this sweep costs the GPU: the whole chain, plus the table.
    pub fn bytes(&self) -> u64 {
        (self.codes.len() + self.lut.len()) as u64
    }

    /// One level's bytes and its `(radials, gates)`, or `None` past the chain.
    pub fn level(&self, level: usize) -> Option<(&[u8], usize, usize)> {
        if level >= self.mip_levels {
            return None;
        }
        let mut at = 0usize;
        let (mut r, mut g) = (self.radials, self.gates);
        for _ in 0..level {
            at += r * g;
            r = r.div_ceil(2);
            g = g.div_ceil(2);
        }
        Some((&self.codes[at..at + r * g], r, g))
    }
}

/// Bytes a `radials x gates` chain of `levels` levels occupies at one byte a
/// code.
///
/// The producer's own arithmetic, restated here because the store has to slice
/// a buffer by it. `the_chain_arithmetic_is_the_producers` in
/// `tests/radar_fan_gpu.rs` holds it against `CodePlane`.
pub fn chain_bytes(radials: usize, gates: usize, levels: usize) -> usize {
    let (mut r, mut g) = (radials, gates);
    let mut total = 0usize;
    for _ in 0..levels {
        total += r * g;
        r = r.div_ceil(2);
        g = g.div_ceil(2);
    }
    total
}

/// Where one pane is looking, on the frame a fan is drawn.
///
/// **Every geodesy figure is handed in**, for the reason [`FanScalars`] names.
/// So is the site's screen position, computed in `f64` on the CPU and arriving
/// as a small `f32`: the shader never forms a difference of two `O(1)`
/// projected coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FanView {
    /// The site's position inside the callback's viewport, in pixels from its
    /// top-left corner.
    ///
    /// **The callback's viewport, not the frame's.** egui sets a viewport from
    /// the callback's rect before `paint`, so clip space maps onto that rect;
    /// a caller computing this must mirror `PaintCallbackInfo::viewport_in_pixels`'s
    /// rounding rather than its own.
    pub site_px: [f32; 2],
    /// That viewport's size, in pixels.
    pub viewport_px: [f32; 2],
    /// Pixels the whole world spans at this zoom.
    pub world_px: f32,
    /// The site's latitude, degrees. Reduced to its sine, cosine and Mercator
    /// y in `f64` here, so the shader receives three numbers it only ever
    /// differences against.
    pub site_lat_deg: f64,
    /// Ground kilometres one screen pixel covers, for the mip selection.
    pub km_per_px: f32,
    /// The sphere ground range is measured on, km.
    pub earth_radius_km: f32,
    /// Paint-time layer opacity, 0-1.
    pub opacity: f32,
}

/// One [`FanView`], in the byte layout the WGSL `Locals` block declares.
///
/// Assembled field by field rather than cast from a `repr(C)` struct: this
/// crate forbids `unsafe`, and forty-eight bytes once per draw is not where a
/// frame is spent.
fn locals_bytes(view: &FanView) -> [u8; LOCALS_BYTES as usize] {
    let (sin_lat0, cos_lat0) = view.site_lat_deg.to_radians().sin_cos();
    // Web Mercator's y is `atanh(sin lat)`. Formed in f64 and handed over,
    // because the fragment stage adds a small offset to it and inverts.
    let merc_y = sin_lat0.clamp(-1.0, 1.0).atanh();
    let lanes: [[u8; 4]; 12] = [
        view.site_px[0].to_ne_bytes(),
        view.site_px[1].to_ne_bytes(),
        view.viewport_px[0].to_ne_bytes(),
        view.viewport_px[1].to_ne_bytes(),
        view.world_px.to_ne_bytes(),
        view.opacity.to_ne_bytes(),
        view.km_per_px.to_ne_bytes(),
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
fn sweep_bytes(sweep: &FanSweep) -> Vec<u8> {
    let mut out = vec![0u8; SWEEP_UNIFORM_BYTES as usize];
    for (radial, [lo, hi]) in sweep.edges.iter().enumerate() {
        let at = radial * 8;
        out[at..at + 4].copy_from_slice(&lo.to_radians().to_ne_bytes());
        out[at + 4..at + 8].copy_from_slice(&hi.to_radians().to_ne_bytes());
    }
    let s = sweep.scalars;
    let tail = EDGE_VEC4S * 16;
    let lanes: [[u8; 4]; 12] = [
        s.first_gate_km.to_ne_bytes(),
        s.reach_km.to_ne_bytes(),
        s.first_gate_slant_km.to_ne_bytes(),
        s.gate_interval_slant_km.to_ne_bytes(),
        s.elevation_deg.unwrap_or(0.0).to_radians().to_ne_bytes(),
        s.re_eff_km.to_ne_bytes(),
        (1.0f32 / s.re_eff_km).to_ne_bytes(),
        s.gate_interval_km.to_ne_bytes(),
        u32::from(s.elevation_deg.is_some()).to_ne_bytes(),
        (sweep.radials as u32).to_ne_bytes(),
        s.reach_gates.to_ne_bytes(),
        (sweep.mip_levels as u32).to_ne_bytes(),
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
    /// are these textures, next sweep.
    alive: Weak<FanSweep>,
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
    resident: HashMap<u64, Resident>,
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
    /// Callbacks that found no store — the one wiring mistake that produces an
    /// ordinary-looking map with no radar in it. Counted rather than logged:
    /// this crate declares no `log` dependency.
    store_missing: AtomicU64,
    swept_pass: Option<u64>,
}

impl RadarFanStore {
    /// Build the pipeline and the canonical mesh for a pass with these
    /// attachments.
    pub fn new(device: &wgpu::Device, attachments: crate::egui_renderer::AttachmentConfig) -> Self {
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
            resident_bytes: 0,
            uploads: 0,
            upload_bytes: 0,
            views: 0,
            ring_writes: 0,
            recorded: AtomicU64::new(0),
            paints: AtomicU64::new(0),
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
    fn ensure(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, sweep: &Arc<FanSweep>) {
        if self.resident.contains_key(&sweep.id) {
            return;
        }
        let (radials, gates) = sweep.shape();
        let codes = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("radar fan codes"),
            size: wgpu::Extent3d {
                width: gates as u32,
                height: radials as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: sweep.mip_levels as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for level in 0..sweep.mip_levels {
            let (bytes, r, g) = sweep
                .level(level)
                .expect("a level inside the chain the sweep declares");
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &codes,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    // Packed, not padded: `write_texture` repacks internally.
                    bytes_per_row: Some(g as u32),
                    rows_per_image: Some(r as u32),
                },
                wgpu::Extent3d {
                    width: g as u32,
                    height: r as u32,
                    depth_or_array_layers: 1,
                },
            );
        }

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
        queue.write_texture(
            lut.as_image_copy(),
            &sweep.lut,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(POLAR_LUT_BYTES as u32),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: POLAR_LUT_ENTRIES as u32,
                height: 1,
                depth_or_array_layers: 1,
            },
        );

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("radar fan sweep"),
            size: SWEEP_UNIFORM_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uniform, 0, &sweep_bytes(sweep));

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

        let bytes = sweep.bytes();
        self.resident_bytes += bytes;
        self.uploads += 1;
        self.upload_bytes += bytes;
        self.resident.insert(
            sweep.id,
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
    fn slot(&mut self, queue: &wgpu::Queue, view: &FanView) -> u32 {
        self.views += 1;
        let ring = &self.ring;
        let ring_writes = &mut self.ring_writes;
        self.batch.push(locals_bytes(view), |offset, bytes| {
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
    pub sweeps: Vec<Arc<FanSweep>>,
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
    pub fn new(sweeps: Vec<Arc<FanSweep>>, view: FanView, pass_nr: u64) -> Option<Self> {
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

    /// The payload `egui_wgpu` downcasts, for a caller that has a painter seam
    /// to publish it through. **Nothing calls this yet**, which is what keeps
    /// the pass dark.
    pub fn payload(self) -> Arc<dyn Any + Send + Sync> {
        egui_wgpu::Callback::new_paint_callback(egui::Rect::ZERO, self).callback
    }
}

impl egui_wgpu::CallbackTrait for RadarFanCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
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
        let slot = store.slot(queue, &self.view);
        self.slot.store(slot, Ordering::Relaxed);
        Vec::new()
    }

    fn finish_prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        // Every callback of the pass is asked; the first finds the batch full
        // and writes it, the rest find it empty.
        if let Some(store) = callback_resources.get_mut::<RadarFanStore>() {
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
            let Some(resident) = store.resident.get(&sweep.id) else {
                continue;
            };
            render_pass.set_bind_group(1, &resident.bind_group, &[]);
            render_pass.draw_indexed(0..MESH_INDICES as u32, 0, 0..1);
            recorded += 2;
        }
        store.recorded.fetch_add(recorded, Ordering::Relaxed);
        store.paints.fetch_add(1, Ordering::Relaxed);
    }
}
