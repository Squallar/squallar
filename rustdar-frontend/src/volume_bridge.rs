//! The end of the 3D wire: what a `rustdar-egui` 3D pane asks for, and the wgpu
//! that answers it.
//!
//! Three things live here, and they are separate because they have three
//! different lifetimes.
//!
//! * [`VolumeStore`] — the built voxel grids, refcounted **by target** so two
//!   panes on one volume share one build. Lives as long as the `App`, survives a
//!   surface loss, and holds no GPU handle at all.
//! * [`VolumePainter`] — the object `Gui` is handed. Lives as long as a renderer:
//!   dropped by `clear_graphics_state` on suspend and on surface loss, which is
//!   what makes a stale GPU handle unreachable rather than merely unused.
//! * [`VolumeResources`] — the wgpu side, inside egui's `CallbackResources`.
//!   Lives as long as the `EguiRenderer` that owns the map.
//!
//! # The one hazard this module is written around
//!
//! `egui_wgpu` downcasts the `Arc<dyn Any>` in a `PaintCallback`. A payload of
//! the wrong type produces one `log::warn!` in `prepare` and a **silent
//! `continue`** in `paint`: a pane that draws nothing, with no panic, no error
//! on screen, and no failing test. Everything that can be tested without a GPU
//! therefore is, and the one thing that cannot — that the payload downcasts —
//! has its own test here, in the only crate that can name both types.
//!
//! # The transfer function, and why five of the six moments are refused
//!
//! `VoxelGrid::fade_band()` reports how many indices above the no-data index are
//! still fully transparent. Measured, it is **64 for reflectivity and 0 for
//! every other moment** — velocity, spectrum width, ZDR, ΦDP and ρHV — because
//! only reflectivity's palette has a transparency floor above the ramp's bottom.
//!
//! Zero is not merely "no fade", and the consequence is larger than it looks.
//! The volume texture is sampled `Linear`, so **every** boundary between a cell
//! with data and a cell without one interpolates across the whole bottom of the
//! ramp inside one voxel. Where that bottom is opaque, the interpolation paints.
//! A volume is mostly empty — a real KSRX velocity grid is 8% filled — so
//! "everywhere a ray passes near data" is very nearly the entire coverage cone,
//! and the accumulated result is a solid block of whatever colour sits low on
//! the ramp.
//!
//! That is not a prediction. It was rendered: at 80 km half-width on KSRX,
//! 2026-07-30 22:33Z, reflectivity resolved into individual convective cells
//! standing above a stratiform sheet, and velocity — the same volume, 677 933
//! cells with data — filled the pane with opaque green from edge to edge.
//!
//! **A short forced fade at the bottom of the table does not fix it, and the
//! first version of this module was wrong to say it did.** The artefact is the
//! whole sweep from index 0 to the neighbour's value, not its bottom; a band of
//! `n` indices hides `n/255` of it. Widening the band far enough to matter would
//! erase the measurements the band covers — and for velocity, ZDR and ΦDP the
//! bottom of the ramp is a real measurement, not a floor.
//!
//! So the decision is a **gate, not a repair**: a moment whose palette does not
//! fade at the bottom of its ramp is not rendered in 3D, and the pane says why.
//! Only reflectivity clears it today, which is also the moment GR2Analyst's 3D
//! view is built around.
//!
//! # The fade width is a proxy, and the property it stands in for is different
//!
//! Read [`MINIMUM_FADE_INDICES`] as a number to tune and you will tune the wrong
//! thing. Review measured what a wider forced fade actually does to velocity, at
//! the same 64 indices reflectivity has: **still unusable** — a speckled disc
//! rather than a solid one. At 128 structure finally appears, and only the
//! *outbound* half of it, because half the scale has been deleted.
//!
//! So the property that matters is not "how many transparent entries are
//! there". It is **whether the low end of the ramp is unimportant**. It is for
//! reflectivity, whose ramp bottom is sub-noise dBZ; it is not for velocity,
//! whose ramp bottom is the strongest inbound flow the radar can encode. The
//! fade width is a usable proxy *only because the six palettes measure 64 and
//! 0* — every threshold in `1..=64` selects the same single moment.
//!
//! A hypothetical palette with a 20-index fade over a meaningful low end would
//! clear this gate and paint a block anyway. If one appears, the fix is a
//! per-moment statement of which end of the scale carries no information — not
//! a bigger constant here.
//!
//! Two things this deliberately is *not*:
//!
//! * It is not a claim that the other five cannot be rendered. It is a claim
//!   that they cannot be rendered **through a palette designed for a plan view**,
//!   where opacity carries no meaning because nothing is behind anything. Giving
//!   each moment its own opacity profile — transparent near 0 m/s and opaque at
//!   the extremes for velocity, transparent near ρHV 1.0 and opaque below it —
//!   is a real design and a good one. It is also five separate presentation
//!   judgements with no oracle in this work package, and the campaign has an
//!   oracle: WP-K compares against GR. Guessing here would put five tuned
//!   constants in front of that comparison rather than behind it.
//! * It is not the encoding's fault alone. The clean fix for the *interpolation*
//!   half is a second channel saying "this cell has data", so the filter never
//!   crosses the boundary — a format change, not a transfer function.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui_wgpu::wgpu;
use rustdar_egui::pane::VolumeTarget;
use rustdar_egui::volume_view::{VolumeFrameState, VolumePaint, VolumePainter, view_for};
use rustdar_radar::voxel::VoxelGrid;

use crate::egui_renderer::AttachmentConfig;
use crate::volume::VolumeSupport;
use crate::volume::quality::VolumeQuality;
use crate::volume::raymarch::{OffscreenTarget, VolumePipelines, VolumeTextures};
use crate::volume::uniform::VolumeUniform;

/// The narrowest transparent run at the bottom of a palette that this renderer
/// will draw a volume through.
///
/// **A proxy for a property this number cannot express, not a tuning knob** —
/// the module doc says which property and why a wider value does not buy what it
/// looks like it buys. In short: every threshold in `1..=64` selects exactly
/// reflectivity, because the six palettes measure 64 and 0 and nothing between;
/// and a *forced* fade of 64 on velocity was measured and is still unusable.
///
/// 16 rather than 1 so that a palette with a token one- or two-entry floor
/// cannot clear a `> 0` test; 16 rather than 64 so that the value is not
/// mistaken for reflectivity's own band, which it has no relationship to.
///
/// It is a **bar**, not a repair — nothing here rewrites a colour table.
pub const MINIMUM_FADE_INDICES: u8 = 16;

/// Width of the shader's opacity ramp, in its 0-1 index units: eight palette
/// indices.
///
/// The ramp starts at the palette's own fade boundary —
/// [`empty_index_threshold_for`], half an index below the first entry whose
/// alpha is not zero — and reaches full palette alpha eight indices above it.
/// Without it the boundary is an alpha cliff one Nearest-sampled LUT step
/// wide: at an echo edge the interpolated index crosses that step inside a
/// single voxel, so every shelf and every echo top wears a hard rim
/// (the terraced shells of the 2026-08-09 report). Eight indices is half a
/// [`MINIMUM_FADE_INDICES`] and, on reflectivity's 0.5 dB-per-index ramp,
/// 4 dBZ of fade — chosen by rendering the KCRP 2017-08-26 (Harvey) volume at
/// 4, 8 and 16: at 4 the tilt shelves keep a faint rim, at 16 the shelf
/// structure the render is supposed to keep legible starts to blur together,
/// and 8 has neither. It softens *presentation* only: the palette, the field
/// and the skip threshold's position all stay the data's own.
///
/// One global constant, with no per-product plumbing behind it: the queued
/// products WP must make widening a per-product decision — a categorical
/// palette (HHC) must never be softened at its class boundaries — and today
/// the only guard is that HHC cannot reach this march at all, because
/// `rustdar_radar::sampler`'s samplable gate refuses every non-moment product
/// before a grid exists.
pub const EDGE_SOFT_WIDTH: f32 = 8.0 / 255.0;

/// The march's skip threshold for a palette whose
/// [`VoxelGrid::fade_band`](rustdar_radar::voxel::VoxelGrid::fade_band) is
/// `band`, in the shader's 0-1 index units.
///
/// `fade_band()` is a **count**: how many indices above the no-data index are
/// still fully transparent. The first entry whose alpha is not zero is
/// therefore `band + 1`, and a Nearest-sampled LUT fetch of an interpolated
/// grid index `i` (in 0-1 units) returns a visible entry exactly when
/// `i * 255 > band + 0.5` — the midpoint between the last transparent entry
/// and the first visible one. So `(band + 0.5) / 255` is the exact boundary:
/// below it the march can skip the sample — and its up-to-seven shading
/// fetches — without changing a pixel, and the [`EDGE_SOFT_WIDTH`] ramp rises
/// from the same boundary, so the first visible index fades in at about 1%
/// opacity instead of arriving as a cliff.
///
/// An earlier version anchored at `(band - 0.5) / 255`, one whole index low:
/// a one-index shell of guaranteed-transparent samples paid full fetch cost
/// for nothing, and the ramp's foot sat below the palette's own fade boundary
/// so the first visible index rendered at ~9% opacity. One function on
/// purpose — the march-cost and real-mask harnesses import it, so an anchor
/// change here cannot leave the instruments measuring a different threshold
/// than production ships.
pub fn empty_index_threshold_for(band: u8) -> f32 {
    (f32::from(band) + 0.5) / 255.0
}

/// A voxel grid the store is holding, or the reason it could not build one.
#[derive(Clone)]
pub enum VolumeEntry {
    /// Built. The `Arc` is shared with every callback that draws it.
    Ready(Arc<VoxelGrid>),
    /// Not built, and why — in a sentence fit for the centre of a pane.
    ///
    /// Kept rather than retried, because every reason `build_voxels` returns
    /// `None` is a property of the volume rather than of the moment: a scan with
    /// no coverage pattern (a volume joined mid-flight, before its VCP message
    /// lands) does not acquire one, and a product with no native moment never
    /// gains one. Retrying every frame would be a 100 ms resample per frame that
    /// fails identically each time.
    Refused(String),
}

/// The built voxel grids, refcounted by target.
///
/// # Why refcounting is by target and not by pane
///
/// Two 3D panes showing the same volume and moment — the ordinary way to compare
/// two camera angles — must share one 8 MiB build and one GPU upload. Keying the
/// store by pane would build it twice and upload it twice, and nothing on screen
/// would say so.
///
/// # Why a `Mutex` and not a `RefCell`
///
/// `VolumePainter` is `Send + Sync`, because egui's callback payloads are
/// required to be and the `Gui` holds the painter across frames. `RefCell` is
/// neither. The lock is uncontended in practice — every access is on the frame
/// thread — and the alternative is a bound that would have to be unpicked the
/// first time anything touches this from a worker.
pub struct VolumeStore {
    inner: Mutex<StoreInner>,
}

#[derive(Default)]
struct StoreInner {
    /// The next id to hand out. Ids identify an upload on the GPU side, where
    /// `VolumeTarget` cannot go: it holds a `NaiveDateTime` and a `String` and
    /// is not `Hash`, and making it so would put a hashing obligation on a UI
    /// type for the sake of a texture cache.
    next_id: u64,
    /// At most one per 3D pane, so a linear scan is the right structure —
    /// and it means `VolumeTarget`'s derived `PartialEq` is the only comparison
    /// needed, rather than a hand-written `Hash` that has to agree with it.
    entries: Vec<StoredVolume>,
}

struct StoredVolume {
    id: u64,
    target: VolumeTarget,
    entry: VolumeEntry,
    /// Which panes are holding this. Empty is impossible: the entry is dropped
    /// when the last pane lets go.
    panes: Vec<usize>,
}

/// What the store holds for one target, with the id its GPU upload is keyed by.
pub struct VolumeLookup {
    pub id: u64,
    pub entry: VolumeEntry,
}

impl VolumeStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(StoreInner::default()),
        }
    }

    /// Whether a build is needed for `target`, and record `pane_idx` as wanting
    /// it either way.
    ///
    /// The two halves are one call because they have to be atomic against each
    /// other: a second pane asking for a volume that is already in hand must
    /// attach without triggering a second 100 ms build.
    pub fn claim(&self, pane_idx: usize, target: &VolumeTarget) -> bool {
        let mut inner = self.lock();
        // A pane holds one volume at a time. Letting go of the old one here —
        // rather than waiting for a `ReleaseVolume` that only arrives on a kind
        // change — is what keeps a pane that is being scrubbed through time from
        // accumulating a grid per volume it passes.
        inner.detach(pane_idx);
        if let Some(found) = inner.entries.iter_mut().find(|e| &e.target == target) {
            found.panes.push(pane_idx);
            return false;
        }
        true
    }

    /// Record the result of a build. `pane_idx` is attached to it.
    ///
    /// The `detach` below is **belt and braces, and no test can see it**:
    /// production reaches here only through [`Self::claim`], which has already
    /// detached, so deleting this line survives mutation testing. It stays
    /// because `insert` is public and nothing in the type stops a future caller
    /// from reaching it directly — and the failure it would then have is a pane
    /// silently holding two volumes, 8 MiB each, released only when the pane
    /// stops being a 3D pane. Recorded here rather than left as an unexplained
    /// survivor in a report nobody reads next to the code.
    pub fn insert(&self, pane_idx: usize, target: VolumeTarget, entry: VolumeEntry) {
        let mut inner = self.lock();
        inner.detach(pane_idx);
        let id = inner.next_id;
        inner.next_id += 1;
        inner.entries.push(StoredVolume {
            id,
            target,
            entry,
            panes: vec![pane_idx],
        });
    }

    /// This pane is holding nothing. Drops whatever it was holding if it was the
    /// last one.
    pub fn release(&self, pane_idx: usize) {
        self.lock().detach(pane_idx);
    }

    /// What is in hand for `target`, if anything.
    pub fn lookup(&self, target: &VolumeTarget) -> Option<VolumeLookup> {
        let inner = self.lock();
        inner
            .entries
            .iter()
            .find(|e| &e.target == target)
            .map(|e| VolumeLookup {
                id: e.id,
                entry: e.entry.clone(),
            })
    }

    /// Every id the store is still holding. The GPU side keeps exactly these
    /// uploads and frees the rest.
    pub fn live_ids(&self) -> Vec<u64> {
        self.lock().entries.iter().map(|e| e.id).collect()
    }

    /// Host bytes the store is holding, and how many volumes that is.
    ///
    /// Reported rather than bounded, and logged on every build — because the
    /// bound is "one grid per 3D pane", and 8 MiB a pane is the kind of figure
    /// that wants to be visible in a log the day someone finds a path that
    /// keeps a grid a pane no longer needs. One such path is already known:
    /// reducing the pane count hides a 3D pane without converting it, and
    /// `ReleaseVolume` fires only on a *kind* change.
    pub fn memory_bytes(&self) -> usize {
        self.lock()
            .entries
            .iter()
            .map(|e| match &e.entry {
                VolumeEntry::Ready(grid) => grid.memory_bytes(),
                VolumeEntry::Refused(why) => why.len(),
            })
            .sum()
    }

    /// A poisoned lock is recovered from rather than propagated.
    ///
    /// The only thing that can poison it is a panic inside one of the six short
    /// methods above, none of which can panic on their own — so a poisoned lock
    /// means the process is already unwinding. Taking the guard anyway keeps a
    /// second panic out of the paint path, where on wasm a main-thread panic
    /// aborts the whole application.
    fn lock(&self) -> std::sync::MutexGuard<'_, StoreInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for VolumeStore {
    fn default() -> Self {
        Self::new()
    }
}

impl StoreInner {
    /// Detach `pane_idx` from whatever it holds, dropping entries nobody holds.
    fn detach(&mut self, pane_idx: usize) {
        for entry in &mut self.entries {
            entry.panes.retain(|&p| p != pane_idx);
        }
        self.entries.retain(|e| !e.panes.is_empty());
    }
}

/// The painter a `Gui` is handed. Turns a pane's frame state into a payload
/// `egui_wgpu` can draw, or into a sentence saying why it cannot.
pub struct BridgeVolumePainter {
    store: Arc<VolumeStore>,
    /// The quality this adapter was classified into, from
    /// `AdapterInfo::device_type`. Fixed for the life of the renderer: a device
    /// does not change class, and the thing that *does* change per frame — the
    /// pane's size — is applied by `fit_to_budget` below.
    quality: VolumeQuality,
    /// What the capability probe said when the renderer was built. Re-consulted
    /// through `volume::support` on every frame, so a device error latched
    /// halfway through a session degrades the pane rather than being remembered
    /// only until the next restart.
    probed: VolumeSupport,
}

impl BridgeVolumePainter {
    pub fn new(store: Arc<VolumeStore>, quality: VolumeQuality, probed: VolumeSupport) -> Self {
        Self {
            store,
            quality,
            probed,
        }
    }
}

impl VolumePainter for BridgeVolumePainter {
    fn paint(&self, frame: &VolumeFrameState) -> VolumePaint {
        // Re-asked every frame rather than cached: `volume::support` folds in
        // the process-global latch that `install_error_latch` and the two-strike
        // surface-loss counter write, and neither of those had happened when
        // this painter was built.
        if let Some(why) = crate::volume::support(&self.probed).reason() {
            return VolumePaint::Empty(why.to_owned());
        }

        let Some(found) = self.store.lookup(&frame.target) else {
            return VolumePaint::Empty(format!(
                "Building the {} volume…",
                frame.target.product.code(),
            ));
        };
        let grid = match found.entry {
            VolumeEntry::Ready(grid) => grid,
            VolumeEntry::Refused(why) => return VolumePaint::Empty(why),
        };

        // On the tilt *count*, never on "the index plane is all no-data".
        //
        // A single-tilt volume does yield an empty grid, but that emptiness is
        // measure-zero rather than an invariant: a cell centre landing
        // bit-exactly on the beam's height paints, so the "all empty" test is
        // right almost always and silently wrong the rest of the time. And the
        // user is owed the reason, not an empty box.
        if grid.tilt_count() == 1 {
            return VolumePaint::Empty(
                "This volume has a single tilt, so there is no vertical structure to render. \
                 Wait for a full scan."
                    .to_owned(),
            );
        }

        // After the grid is built rather than before, deliberately: the answer
        // is a property of the table that travels *inside* the grid, and reading
        // it from a second copy of the palette would be a second copy to keep in
        // step. The build is not wasted either — the store keeps it, so
        // switching back to a moment that renders costs nothing.
        if let Some(why) = palette_refusal(&grid) {
            return VolumePaint::Empty(why);
        }

        let fitted = self.quality.fit_to_budget(frame.size_px);
        let box_size_km = box_size_km(&grid);
        let aspect = fitted.size[0] as f32 / fitted.size[1] as f32;
        let Some(view) = view_for(frame.camera, box_size_km, aspect) else {
            // Reached by a pane collapsed to nothing by a divider drag, and by a
            // grid whose box has a zero axis. Both are transient or impossible;
            // neither may hand the GPU a matrix of NaN.
            return VolumePaint::Empty("This pane is too small to draw a volume in.".to_owned());
        };

        let shape = grid.shape();
        let mut uniform = VolumeUniform::new(
            box_size_km,
            [shape.nx as u32, shape.ny as u32, shape.nz as u32],
        );
        uniform.box_from_clip = view.box_from_clip;
        uniform.eye_in_box = view.eye_in_box;
        // The rung this pane actually got, not the one the adapter was offered:
        // `fit_to_budget` can step the resolution down, and shading rides the
        // same struct.
        uniform.gradient_shading = fitted.quality.shading.is_on();
        // The march's transfer edge, anchored at this palette's own fade
        // boundary. `fade_band()` counts the fully transparent indices above
        // the no-data index, so the first visible entry is `band + 1` and
        // [`empty_index_threshold_for`] — `(band + 0.5) / 255` — is exactly
        // where a Nearest LUT fetch starts returning visible entries: below
        // it the march can skip the sample — and its up-to-seven shading
        // fetches — without changing a pixel. The ramp then dissolves the
        // alpha cliff at that same boundary over [`EDGE_SOFT_WIDTH`].
        uniform.empty_index_threshold = empty_index_threshold_for(grid.fade_band());
        uniform.edge_soft_width = EDGE_SOFT_WIDTH;

        let callback = VolumeCallback {
            pane_idx: frame.pane_idx,
            grid_id: found.id,
            grid,
            uniform,
            offscreen_px: fitted.size,
            live_ids: self.store.live_ids(),
        };

        VolumePaint::Callback(paint_payload(callback))
    }
}

/// Wrap a callback in whatever `egui_wgpu` downcasts to.
///
/// `egui_wgpu::Callback`'s field is private and its only constructor hands back
/// a whole `epaint::PaintCallback`, so the payload can only be obtained by
/// building one and taking its `callback` field. The rect passed in is
/// **discarded**, and that is exact rather than approximate: `new_paint_callback`
/// stores the rect on the `PaintCallback` it returns and puts nothing but the
/// boxed trait object inside the `Arc`. `rustdar-egui` supplies the real rect
/// when it constructs its own `PaintCallback`.
///
/// Generic over the callback so the test below can exercise the wrapper without
/// a `VoxelGrid` — which has no constructor outside `build_voxels` and would
/// need a synthetic `Scan` to obtain. That `VolumeCallback` itself satisfies
/// `CallbackTrait` is proven by this function's one production call site
/// compiling; what needs a *test* is that the wrapper still produces the type
/// `egui_wgpu` downcasts to, which is exactly what would change if someone
/// simplified this to `Arc::new(callback)`.
fn paint_payload(callback: impl egui_wgpu::CallbackTrait + 'static) -> Arc<dyn Any + Send + Sync> {
    egui_wgpu::Callback::new_paint_callback(egui::Rect::ZERO, callback).callback
}

/// The box's physical extent in kilometres, along each axis.
fn box_size_km(grid: &VoxelGrid) -> [f32; 3] {
    let (x0, x1) = grid.x_range_km();
    let (y0, y1) = grid.y_range_km();
    let (z0, z1) = grid.z_range_km_msl();
    [(x1 - x0) as f32, (y1 - y0) as f32, (z1 - z0) as f32]
}

/// Why this moment cannot be drawn as a volume, or `None` if it can.
///
/// The whole transfer-function decision, in one predicate over one measured
/// number. See the module doc for what was rendered to arrive at it.
fn palette_refusal(grid: &VoxelGrid) -> Option<String> {
    palette_refusal_for(grid.fade_band(), grid.product().name())
}

/// [`palette_refusal`] over the two things it actually reads, so the decision is
/// testable without a `VoxelGrid` — which has no constructor outside
/// `build_voxels` and would need a synthetic `Scan` to obtain.
fn palette_refusal_for(band: u8, moment: &str) -> Option<String> {
    if band >= MINIMUM_FADE_INDICES {
        return None;
    }
    Some(format!(
        "{moment} cannot be drawn as a volume yet.\n\nIts colour table is opaque at the bottom of its \
         scale, so every boundary between measured and unmeasured air paints — and a volume is \
         mostly unmeasured air. The result is a solid block, not a picture. Reflectivity's table \
         fades over the lowest quarter of its scale, which is why it renders.\n\nGiving each \
         moment its own opacity profile is the fix, and it is a presentation decision that wants \
         comparing against GR2Analyst rather than guessing.",
    ))
}

/// The wgpu side, held in egui's `CallbackResources`.
///
/// One inserted type is one slot for the **whole application** — `CallbackResources`
/// is a `TypeMap` keyed by type, not by pane or by callback — so the per-pane
/// split has to live inside this struct rather than beside it. Two 3D panes at
/// different sizes need two offscreen targets, and there is no second slot to
/// put the other one in.
pub struct VolumeResources {
    pipelines: VolumePipelines,
    /// One offscreen per pane, sized to that pane. `Option` because
    /// `VolumePipelines::ensure_offscreen` takes the slot and decides whether to
    /// reallocate, which is what keeps a pane-sized texture from being churned
    /// at the frame rate.
    targets: HashMap<usize, Option<OffscreenTarget>>,
    /// One upload per grid, keyed by the store's id. Two panes on one volume
    /// share the entry, which is the GPU half of the store's refcounting.
    uploads: HashMap<u64, VolumeTextures>,
}

impl VolumeResources {
    /// Build the pipelines for the pass egui draws into.
    pub fn new(
        device: &wgpu::Device,
        egui_attachments: AttachmentConfig,
        queue: &wgpu::Queue,
    ) -> Self {
        let pipelines = VolumePipelines::new(device, egui_attachments);
        pipelines.upload_quad(queue);
        Self {
            pipelines,
            targets: HashMap::new(),
            uploads: HashMap::new(),
        }
    }

    /// Free everything `pane_idx` was the only user of.
    ///
    /// This is what makes `GuiAction::ReleaseVolume` actually give memory back:
    /// a pane-sized `Rgba8Unorm` target (~3 MiB at 900²) and, when the last pane
    /// on a volume lets go, the 3D texture and its table. Dropping the handles
    /// is the free — wgpu reference-counts them and the allocation goes when the
    /// last reference does.
    pub fn release_pane(&mut self, pane_idx: usize, live_ids: &[u64]) {
        self.targets.remove(&pane_idx);
        self.uploads.retain(|id, _| live_ids.contains(id));
    }
}

/// One 3D pane's draw, for one frame.
///
/// Carries the grid rather than a handle to it because the upload may not have
/// happened yet: `prepare` is the first place a `wgpu::Device` exists, so the
/// bytes have to travel this far. The `Arc` makes that a refcount bump.
struct VolumeCallback {
    pane_idx: usize,
    grid_id: u64,
    grid: Arc<VoxelGrid>,
    uniform: VolumeUniform,
    offscreen_px: [u32; 2],
    /// Every grid the store still holds, so `prepare` can free the uploads for
    /// the ones it does not. Carried on the callback rather than read from the
    /// store because `prepare` runs with no access to anything but its
    /// arguments.
    live_ids: Vec<u64>,
}

impl egui_wgpu::CallbackTrait for VolumeCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(resources) = callback_resources.get_mut::<VolumeResources>() else {
            // The renderer was built without volume support, or the resources
            // were never inserted. Logged rather than silent because this is the
            // one wiring mistake that produces an ordinary-looking empty pane.
            log::warn!("3D volume view: no VolumeResources in the callback map; nothing to draw");
            return Vec::new();
        };
        // Destructured so the borrow checker can see that the pipelines are read
        // while the two maps are written.
        let VolumeResources {
            pipelines,
            targets,
            uploads,
        } = resources;

        uploads.retain(|id, _| self.live_ids.contains(id));

        let slot = targets.entry(self.pane_idx).or_default();
        pipelines.ensure_offscreen(device, slot, self.offscreen_px);
        let Some(target) = slot.as_ref() else {
            return Vec::new();
        };

        // Through the entry API rather than `contains_key` + `insert`, which is
        // one hash lookup instead of two — and the upload is refusable, so this
        // is a `match` on the entry rather than `or_insert_with`.
        let textures = match uploads.entry(self.grid_id) {
            std::collections::hash_map::Entry::Occupied(occupied) => occupied.into_mut(),
            std::collections::hash_map::Entry::Vacant(vacant) => {
                let shape = self.grid.shape();
                let Some(textures) = pipelines.upload_volume(
                    device,
                    queue,
                    [shape.nx as u32, shape.ny as u32, shape.nz as u32],
                    self.grid.indices(),
                    // Straight from the grid: the table travels inside it, and
                    // nothing here rewrites it. See the module doc.
                    self.grid.lut(),
                ) else {
                    // `upload_volume` has already logged which invariant it
                    // refused on. Nothing to add, and nothing to draw.
                    return Vec::new();
                };
                vacant.insert(textures)
            }
        };

        textures.write_uniform(queue, &self.uniform);
        // Into egui's own encoder, which egui submits before its own commands —
        // so the offscreen is written before the blit reads it. The other order
        // paints the previous frame's volume, which reads as input lag.
        pipelines.encode_raymarch(egui_encoder, target, textures);

        Vec::new()
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &egui_wgpu::CallbackResources,
    ) {
        let Some(resources) = callback_resources.get::<VolumeResources>() else {
            return;
        };
        let Some(Some(target)) = resources.targets.get(&self.pane_idx) else {
            return;
        };
        // Nothing was uploaded, so the offscreen holds whatever the last draw
        // left. Better an empty pane than another pane's volume.
        if !resources.uploads.contains_key(&self.grid_id) {
            return;
        }

        let viewport = info.viewport_in_pixels();
        if viewport.width_px <= 0 || viewport.height_px <= 0 {
            return;
        }
        // The quad covers all of clip space, so the viewport is what places it
        // over the pane. egui re-binds pipeline, scissor and viewport after
        // every callback, so nothing here has to be put back.
        render_pass.set_viewport(
            viewport.left_px as f32,
            viewport.top_px as f32,
            viewport.width_px as f32,
            viewport.height_px as f32,
            0.0,
            1.0,
        );
        resources.pipelines.paint_blit(render_pass, target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use rustdar_egui::pane::VolumeStamp;
    use rustdar_radar::types::RadarProduct;

    fn target(product: RadarProduct, minute: u32) -> VolumeTarget {
        VolumeTarget {
            region: None,
            volume: VolumeStamp {
                site: "KTLX".to_owned(),
                collected: NaiveDate::from_ymd_opt(2024, 5, 6)
                    .unwrap()
                    .and_hms_opt(22, minute, 0)
                    .unwrap(),
            },
            product,
        }
    }

    /// The payload the painter hands `rustdar-egui` is one `egui_wgpu` can
    /// actually draw.
    ///
    /// **This is the test the stub painter cannot be.** A wrong-typed payload is
    /// one `log::warn!` in `prepare` and a silent `continue` in `paint`, so
    /// every headless test in `rustdar-egui` — which can only ever see an
    /// `Arc<dyn Any>` — would pass against a payload that never draws a pixel.
    /// This crate is the only one that can name both ends, so the downcast is
    /// asserted here.
    #[test]
    fn the_payload_the_painter_hands_over_is_one_egui_wgpu_can_draw() {
        struct Nothing;
        impl egui_wgpu::CallbackTrait for Nothing {
            fn paint(
                &self,
                _info: egui::PaintCallbackInfo,
                _render_pass: &mut wgpu::RenderPass<'static>,
                _callback_resources: &egui_wgpu::CallbackResources,
            ) {
            }
        }

        let payload = paint_payload(Nothing);
        assert!(
            payload.downcast_ref::<egui_wgpu::Callback>().is_some(),
            "egui_wgpu downcasts the payload to its own `Callback`; anything else is one \
             log line and a silent `continue`, which looks exactly like a pane with no data",
        );
    }

    /// Refcounting is by target: two panes on one volume share one entry, and it
    /// survives until the second lets go.
    #[test]
    fn two_panes_on_one_volume_share_one_build() {
        let store = VolumeStore::new();
        let t = target(RadarProduct::Reflectivity, 0);

        assert!(store.claim(0, &t), "the first pane must trigger a build");
        store.insert(0, t.clone(), VolumeEntry::Refused("stub".to_owned()));

        assert!(
            !store.claim(1, &t),
            "a second pane on the same volume must not trigger a second build",
        );
        assert_eq!(store.live_ids().len(), 1, "one target, one entry");

        store.release(0);
        assert_eq!(
            store.live_ids().len(),
            1,
            "the entry must survive while the second pane still holds it",
        );
        store.release(1);
        assert!(
            store.live_ids().is_empty(),
            "the last pane letting go must drop the entry",
        );
    }

    /// A pane moving to a volume **another pane already built** lets go of the
    /// one it was holding.
    ///
    /// The path where `claim` is the only thing that can let go: it returns
    /// `false`, so no `insert` follows and `insert`'s own detach never runs.
    /// Without this, `claim` and `insert` each look redundant against the other
    /// and both can be deleted one at a time with every test still green —
    /// which is how mutation testing found this gap.
    #[test]
    fn a_pane_joining_a_volume_someone_else_built_drops_what_it_held() {
        let store = VolumeStore::new();
        let held = target(RadarProduct::Reflectivity, 0);
        let shared = target(RadarProduct::Velocity, 6);

        // Pane 0 builds and holds one volume; pane 1 builds another.
        store.claim(0, &held);
        store.insert(0, held.clone(), VolumeEntry::Refused("held".to_owned()));
        store.claim(1, &shared);
        store.insert(1, shared.clone(), VolumeEntry::Refused("shared".to_owned()));
        assert_eq!(
            store.live_ids().len(),
            2,
            "precondition: two volumes in hand"
        );

        // Pane 0 now wants the volume pane 1 already has. No build follows.
        assert!(
            !store.claim(0, &shared),
            "the build is shared, not repeated"
        );
        assert!(
            store.lookup(&held).is_none(),
            "the volume pane 0 was holding is nobody's now and must be gone",
        );
        assert_eq!(store.live_ids().len(), 1);
    }

    /// A pane that moves to another volume lets go of the old one at once.
    ///
    /// Without this, scrubbing a 3D pane through time accumulates one grid per
    /// volume passed — 8 MiB each — and nothing frees them until the pane stops
    /// being a 3D pane, which is not something a user does to reclaim memory.
    #[test]
    fn a_pane_holds_one_volume_at_a_time() {
        let store = VolumeStore::new();
        let first = target(RadarProduct::Reflectivity, 0);
        let second = target(RadarProduct::Reflectivity, 6);

        store.claim(0, &first);
        store.insert(0, first.clone(), VolumeEntry::Refused("stub".to_owned()));
        store.claim(0, &second);
        store.insert(0, second.clone(), VolumeEntry::Refused("stub".to_owned()));

        assert_eq!(store.live_ids().len(), 1, "the first volume must be gone");
        assert!(store.lookup(&first).is_none());
        assert!(store.lookup(&second).is_some());
    }

    /// Ids are never reused, so a stale callback cannot address a new upload.
    ///
    /// A callback built on the frame a volume rolled is still in egui's shape
    /// list when `prepare` runs. If the store had reused the id, that callback
    /// would draw the *new* volume through the old one's uniform — a picture
    /// that is wrong and looks right.
    #[test]
    fn a_released_id_is_never_handed_out_again() {
        let store = VolumeStore::new();
        let first = target(RadarProduct::Reflectivity, 0);
        store.claim(0, &first);
        store.insert(0, first.clone(), VolumeEntry::Refused("a".to_owned()));
        let first_id = store.lookup(&first).expect("stored").id;
        store.release(0);

        let second = target(RadarProduct::Velocity, 0);
        store.claim(0, &second);
        store.insert(0, second.clone(), VolumeEntry::Refused("b".to_owned()));
        assert_ne!(
            store.lookup(&second).expect("stored").id,
            first_id,
            "ids must not be reused",
        );
    }

    /// Exactly one of the six samplable moments clears the fade bar today, and
    /// the bands here are `rustdar_radar::voxel`'s own measurements.
    ///
    /// Written as literals rather than by rebuilding six grids, and that is the
    /// point: `the_fade_band_is_measured_per_product` upstream pins what the
    /// palettes produce, and this pins what this renderer *does* about it. If a
    /// palette gains a transparency floor, the upstream test changes and this one
    /// stays green — which is correct, because the moment would then start
    /// rendering, and that is a decision someone should make on purpose rather
    /// than discover.
    #[test]
    fn only_reflectivity_clears_the_fade_bar() {
        let measured = [
            ("Reflectivity", 64u8),
            ("Velocity", 0),
            ("Spectrum Width", 0),
            ("Differential Reflectivity", 0),
            ("Differential Phase", 0),
            ("Correlation Coefficient", 0),
        ];
        let drawable: Vec<&str> = measured
            .iter()
            .filter(|(moment, band)| palette_refusal_for(*band, moment).is_none())
            .map(|(moment, _)| *moment)
            .collect();
        assert_eq!(
            drawable,
            vec!["Reflectivity"],
            "the set of moments this renderer will draw as a volume changed",
        );
    }

    /// A refusal names the moment and says what would have to change.
    ///
    /// The pane paints this text and nothing else, so a bare "unavailable" here
    /// is a user staring at an empty box with no idea whether to wait, switch
    /// product, or file a bug.
    #[test]
    fn a_refusal_names_the_moment_and_says_why() {
        let why = palette_refusal_for(0, "Velocity").expect("an opaque palette is refused");
        assert!(
            why.starts_with("Velocity"),
            "the moment must be named: {why}"
        );
        assert!(
            why.contains("opaque"),
            "the reason must name the property that caused it: {why}",
        );
        assert!(
            why.contains("Reflectivity"),
            "the message must say which moment does work: {why}",
        );
    }

    /// The two guards inside `paint` that no headless test can reach are still
    /// in it, and the single-tilt one is still on the **count**.
    ///
    /// # Why this is a source scan and not a behavioural test
    ///
    /// Both guards read a `VoxelGrid`, and a `VoxelGrid` has no constructor
    /// outside `build_voxels` — which needs a synthetic `nexrad_model` `Scan`.
    /// So the only behavioural test would be an integration test carrying a
    /// scan builder, and until one exists these two guards can be deleted with
    /// every test in the workspace still green. Mutation testing found exactly
    /// that: removing the palette gate, and rewriting the tilt check as "the
    /// index plane is all no-data", both survived.
    ///
    /// The second of those is the one that matters. A single-tilt volume *does*
    /// yield an empty grid, so the emptiness test is right almost always — and
    /// wrong without warning when a cell centre lands bit-exactly on the beam's
    /// height, which is measure-zero rather than impossible. It also loses the
    /// reason: the user gets an empty box instead of "wait for a full scan".
    ///
    /// A scan is a weak test and is named as one. It is here because a guard
    /// nothing can fail is worse.
    #[test]
    fn the_guards_paint_cannot_be_tested_through_are_still_in_it() {
        let source = include_str!("volume_bridge.rs");
        let start = source
            .find("impl VolumePainter for BridgeVolumePainter {")
            .expect("the painter impl is no longer where this test looks for it");
        let body = &source[start..];
        let end = body
            .find("\n}\n")
            .expect("the painter impl has no closing brace");
        let body = &body[..end];

        assert!(
            body.contains("grid.tilt_count() == 1"),
            "`paint` no longer branches on the tilt count",
        );
        assert!(
            !body.contains("all(|&i|") && !body.contains("iter().all("),
            "`paint` looks like it tests the index plane for emptiness; \
             a single-tilt volume must be recognised by its tilt count, because \
             emptiness is measure-zero rather than an invariant",
        );
        assert!(
            body.contains("palette_refusal(&grid)"),
            "`paint` no longer consults the palette gate, so a moment whose colour \
             table is opaque at the bottom of its ramp would render as a solid block",
        );

        // The soft-edge mechanism's two production lines. Mutation testing
        // proved that deleting both left the entire workspace suite green:
        // the uniform's defaults (index-0 threshold, hard edge) are a
        // renderable configuration, so nothing downstream fails — the user
        // simply gets the hard shelf rims and the wasted marching the
        // 2026-08-09 work exists to remove. No behavioural test can reach
        // `paint` with a `Ready` grid (`VoxelGrid` has no constructor outside
        // `build_voxels`), so the lines are pinned here; the *values* they
        // assign are behaviourally pinned by
        // `the_skip_threshold_separates_the_last_transparent_entry_from_the_first_visible_one`
        // and `the_soft_width_is_eight_indices_half_the_fade_bar` below, and
        // the GPU mask instrument in `tests/volume_silhouette.rs` observes
        // the default width staying hard.
        assert!(
            body.contains(
                "uniform.empty_index_threshold = empty_index_threshold_for(grid.fade_band())"
            ),
            "`paint` no longer anchors the skip threshold at the palette's fade \
             boundary, so the march reverts to skipping only index 0 — every \
             transparent cell under the band pays full sample cost again",
        );
        assert!(
            body.contains("uniform.edge_soft_width = EDGE_SOFT_WIDTH"),
            "`paint` no longer widens the opacity ramp, so every shelf and echo \
             top reverts to the hard one-LUT-step rim the soft edge dissolves",
        );
    }

    /// [`empty_index_threshold_for`] sits strictly between the last fully
    /// transparent palette entry and the first visible one, for every band.
    ///
    /// The behavioural half of the anchor: a Nearest-sampled LUT fetch of
    /// entry `n` sees the index value `n / 255`, entries `0..=band` are
    /// transparent and `band + 1` is the first visible one — so the shader's
    /// `index > threshold` test must be false at `band / 255` and true at
    /// `(band + 1) / 255`. The shipped off-by-one (`(band - 0.5) / 255`)
    /// fails the first half of this: entry `band` itself cleared the
    /// threshold, so a one-index shell of zero-alpha samples paid up to seven
    /// fetches per step for nothing and the ramp's foot sat one index below
    /// the palette's own fade boundary.
    #[test]
    fn the_skip_threshold_separates_the_last_transparent_entry_from_the_first_visible_one() {
        for band in 0..=u8::MAX {
            let threshold = empty_index_threshold_for(band);
            assert!(
                f32::from(band) / 255.0 <= threshold,
                "at band {band} the last transparent entry clears the skip \
                 threshold, so the march samples — and shades — cells whose \
                 fetch is guaranteed invisible",
            );
            if band < u8::MAX {
                assert!(
                    f32::from(band) / 255.0 + 1.0 / 255.0 > threshold,
                    "at band {band} the first visible entry is under the skip \
                     threshold, so the march erases the bottom of the ramp",
                );
            }
        }
        // And the anchor is the midpoint, not merely inside the gap: the
        // EDGE_SOFT_WIDTH ramp rises from it, so where it sits inside the gap
        // decides the opacity the first visible index fades in at (~1% at the
        // midpoint, ~9% one index lower).
        assert_eq!(empty_index_threshold_for(64), 64.5 / 255.0);
    }

    /// The production ramp is eight indices wide — half the fade bar, and not
    /// the uniform's hard-edged default.
    ///
    /// Flipping [`EDGE_SOFT_WIDTH`] to 0 is the one-character revert of the
    /// user-visible half of the soft edge, and before this test nothing in
    /// the workspace could see it.
    #[test]
    fn the_soft_width_is_eight_indices_half_the_fade_bar() {
        // Pinning the value pins it away from zero too: a zero production
        // width is the hard alpha cliff the soft edge exists to dissolve.
        assert_eq!(
            EDGE_SOFT_WIDTH,
            f32::from(MINIMUM_FADE_INDICES) / 2.0 / 255.0,
            "EDGE_SOFT_WIDTH is no longer eight palette indices; the 4/8/16 \
             Harvey comparison behind the number is in its doc comment",
        );
    }

    /// The bar is inclusive, and a palette one index short of it is refused.
    ///
    /// Both halves matter. Written as `>` the whole set would flip on a palette
    /// sitting exactly at 16; written as `>=` on the wrong side, a 15-index
    /// token floor would pass and paint the block this gate exists to stop.
    #[test]
    fn the_fade_bar_is_inclusive_and_bites_one_index_below_it() {
        assert!(palette_refusal_for(MINIMUM_FADE_INDICES, "x").is_none());
        assert!(palette_refusal_for(MINIMUM_FADE_INDICES - 1, "x").is_some());
    }
}
