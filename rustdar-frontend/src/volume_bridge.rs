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
//! # The transfer function for the five moments that do not fade
//!
//! `VoxelGrid::fade_band()` reports how many indices above the no-data index are
//! still fully transparent. It is 64 for reflectivity and **0** for velocity,
//! ZDR, ρHV, spectrum width and ΦDP, because only reflectivity's palette has a
//! transparency floor above the ramp's bottom.
//!
//! Zero is not merely "no fade". The volume texture is sampled `Linear`, so
//! every echo edge interpolates from the no-data index up to its neighbour's,
//! sweeping the *whole bottom of the ramp* inside one voxel. Where the ramp's
//! bottom is opaque, that sweep paints a shell around every echo in the palette's
//! most alarming colour: a ρHV ≈ 0.2 debris shell around every storm, a −63 m/s
//! inbound shell around every outbound edge. It is an artefact of the encoding
//! and it looks exactly like a finding.
//!
//! [`transfer_lut`] answers it with a short forced fade at the bottom of the
//! table — the same move `colormap_lut` already makes for index 0, extended by a
//! few indices. What it costs is stated per product in
//! `the_forced_fade_costs_only_the_saturated_end_of_each_palette`, and it is
//! **not free**: it is the correct call for four of the five and a real loss for
//! the fifth.
//!
//! * **Velocity, ZDR, ρHV** — the faded indices lie entirely below the palette's
//!   own first legend stop, where the colour has already saturated. Nothing
//!   distinguishable is lost; a −63 m/s cell is drawn in the same red as a
//!   −40 m/s one either way.
//! * **Spectrum width** — the band reaches ~1.75 m/s, which *is* inside the
//!   legend. Low spectrum width is the uninteresting end of that moment, so the
//!   trade is defensible, but it is a trade.
//! * **ΦDP** — a circular moment has no "bottom of the ramp"; 0° and 360° are
//!   the same measurement, so fading near 0° fades a real, common value (phase
//!   close to the radar) for no encoding reason. It is the worst-served moment
//!   by this format already — see `VoxelGrid::wraps` and the 4× quantisation
//!   loss — and this makes it slightly worse. It is done anyway, because the
//!   alternative on the same moment is a full-palette rainbow rim around every
//!   echo, which is louder.
//!
//! **The cure is not a wider band.** The artefact is the whole sweep, not its
//! bottom; a band of `n` hides `n/255` of it. The cure is a second channel
//! saying "this cell has data", so the filter never crosses the boundary at all
//! — which is a format change, not a transfer function, and is deliberately not
//! made here.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui_wgpu::wgpu;
use rustdar_egui::pane::VolumeTarget;
use rustdar_egui::volume_view::{VolumeFrameState, VolumePaint, VolumePainter, view_for};
use rustdar_radar::voxel::{LUT_LEN, VoxelGrid};

use crate::egui_renderer::AttachmentConfig;
use crate::volume::VolumeSupport;
use crate::volume::quality::VolumeQuality;
use crate::volume::raymarch::{OffscreenTarget, VolumePipelines, VolumeTextures};
use crate::volume::uniform::VolumeUniform;

/// How many indices at the bottom of the colour table are forced towards
/// transparent when the palette does not fade there on its own.
///
/// Short on purpose — 8 of 256 is 3.1% of the ramp. It is sized to cover the
/// saturated tail below each palette's first legend stop, not to hide the whole
/// interpolation artefact, which no finite band can. See the module doc for what
/// it costs per product.
pub const FORCED_FADE_INDICES: u8 = 8;

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

    /// Host bytes the store is holding. Reported rather than bounded: the bound
    /// is the pane count, which is what the store is keyed by.
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
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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

        let callback = VolumeCallback {
            pane_idx: frame.pane_idx,
            grid_id: found.id,
            lut: transfer_lut(&grid),
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
    [
        (x1 - x0) as f32,
        (y1 - y0) as f32,
        (z1 - z0) as f32,
    ]
}

/// The grid's colour table with at least [`FORCED_FADE_INDICES`] of fade at the
/// bottom, whatever the palette does on its own.
///
/// A **ramp**, not a cut: alpha is scaled linearly from zero at index 1 to full
/// at the top of the band, so an echo edge fades through the band instead of
/// stepping at the end of it. Colour is left alone — only alpha moves — because
/// the point is to stop the bottom of the ramp *contributing*, not to recolour
/// it.
///
/// A palette that already fades further than this is returned untouched, which
/// is the reflectivity case (64 indices) and the whole reason this is a floor
/// rather than a setting. See the module doc for what the floor costs each of
/// the five moments that need it.
pub fn transfer_lut(grid: &VoxelGrid) -> Arc<Vec<u8>> {
    Arc::new(faded_lut(grid.lut(), grid.fade_band(), FORCED_FADE_INDICES))
}

/// [`transfer_lut`]'s arithmetic, over plain bytes so it is testable without a
/// `VoxelGrid`.
fn faded_lut(lut: &[u8], natural_band: u8, minimum_band: u8) -> Vec<u8> {
    let mut out = lut.to_vec();
    if natural_band >= minimum_band || out.len() != LUT_LEN {
        return out;
    }
    // Index 0 is the no-data entry and is already fully transparent; the ramp
    // therefore runs across indices 1..=minimum_band, reaching full weight one
    // step past the band.
    let band = f32::from(minimum_band);
    for index in 1..=usize::from(minimum_band) {
        let weight = index as f32 / (band + 1.0);
        let alpha = &mut out[index * 4 + 3];
        // Rounded, and `min` against the original: a weight of 1 must not be
        // able to *raise* an alpha the palette had already lowered.
        *alpha = ((f32::from(*alpha) * weight).round() as u8).min(*alpha);
    }
    out
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
    pub fn new(device: &wgpu::Device, egui_attachments: AttachmentConfig, queue: &wgpu::Queue) -> Self {
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
    lut: Arc<Vec<u8>>,
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

        if !uploads.contains_key(&self.grid_id) {
            let shape = self.grid.shape();
            let Some(textures) = pipelines.upload_volume(
                device,
                queue,
                [shape.nx as u32, shape.ny as u32, shape.nz as u32],
                self.grid.indices(),
                &self.lut,
            ) else {
                // `upload_volume` has already logged which invariant it refused
                // on. Nothing to add, and nothing to draw.
                return Vec::new();
            };
            uploads.insert(self.grid_id, textures);
        }
        let Some(textures) = uploads.get(&self.grid_id) else {
            return Vec::new();
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

    /// A palette that already fades further than the floor is left exactly as it
    /// is — reflectivity's 64-index band is not narrowed to 8.
    #[test]
    fn a_palette_that_already_fades_is_untouched() {
        let mut lut = vec![255u8; LUT_LEN];
        for entry in 0..=64 {
            lut[entry * 4 + 3] = 0;
        }
        assert_eq!(
            faded_lut(&lut, 64, FORCED_FADE_INDICES),
            lut,
            "a 64-index natural band must not be replaced by an 8-index forced one",
        );
    }

    /// The forced band fades rather than cuts, never raises an alpha, and stops
    /// where it says it does.
    #[test]
    fn the_forced_band_ramps_and_stops() {
        let mut lut = vec![255u8; LUT_LEN];
        lut[3] = 0; // index 0 is the no-data entry
        let faded = faded_lut(&lut, 0, FORCED_FADE_INDICES);

        assert_eq!(faded[3], 0, "the no-data entry stays transparent");
        let alphas: Vec<u8> = (1..=usize::from(FORCED_FADE_INDICES) + 1)
            .map(|i| faded[i * 4 + 3])
            .collect();
        assert!(
            alphas.windows(2).all(|w| w[0] < w[1]),
            "the band must ramp monotonically, not step: {alphas:?}",
        );
        assert!(alphas[0] < 255, "index 1 must be faded");
        assert_eq!(
            *alphas.last().expect("a band"),
            255,
            "the first index past the band must be at full alpha",
        );
        for index in usize::from(FORCED_FADE_INDICES) + 1..256 {
            assert_eq!(
                faded[index * 4 + 3], 255,
                "index {index} is past the band and must be untouched",
            );
        }
        for index in 0..256 {
            assert!(
                faded[index * 4 + 3] <= lut[index * 4 + 3],
                "the fade must never raise an alpha (index {index})",
            );
        }
    }

    /// Only alpha moves. Recolouring the bottom of a ramp would be a different
    /// and much larger decision than making it contribute less.
    #[test]
    fn the_forced_band_changes_only_alpha() {
        let lut: Vec<u8> = (0..LUT_LEN).map(|i| (i % 251) as u8).collect();
        let faded = faded_lut(&lut, 0, FORCED_FADE_INDICES);
        for index in 0..256 {
            assert_eq!(
                faded[index * 4..index * 4 + 3],
                lut[index * 4..index * 4 + 3],
                "index {index}'s colour changed",
            );
        }
    }

    /// A table that is not the size the format promises is passed through
    /// untouched rather than indexed into.
    #[test]
    fn a_table_of_the_wrong_length_is_not_touched() {
        let lut = vec![255u8; 16];
        assert_eq!(faded_lut(&lut, 0, FORCED_FADE_INDICES), lut);
    }
}
