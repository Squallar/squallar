//! The end of the 3D wire: what a `rustdar-egui` 3D pane asks for, and the wgpu
//! that answers it.
//!
//! `egui_wgpu` downcasts the `Arc<dyn Any>` in a `PaintCallback`. A payload of
//! the wrong type produces one `log::warn!` in `prepare` and a **silent
//! `continue`** in `paint`: a pane that draws nothing, with no panic and no
//! failing test. Everything testable without a GPU therefore is, and the one
//! thing that cannot be — that the payload downcasts — has its own test here.

use std::any::Any;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui_wgpu::wgpu;
use rustdar_egui::pane::VolumeTarget;
use rustdar_egui::volume_alpha::AlphaCurve;
use rustdar_egui::volume_view::{VolumeFrameState, VolumePaint, VolumePainter, view_for};
use rustdar_radar::voxel::VoxelGrid;

use crate::VolumeSupport;
use crate::raymarch::staging::VolumeStaging;
use crate::raymarch::{CoarseLevel, OffscreenTarget, VolumePipelines, VolumeTextures};
use crate::uniform::VolumeUniform;
use rustdar_device_profile::quality::VolumeQuality;
use rustdar_gpu::egui_renderer::AttachmentConfig;

/// The fewest see-through entries a grid's table may have, anywhere on its
/// ramp, before this renderer refuses to draw a volume through it.
pub const MINIMUM_FADE_INDICES: u8 = 16;

/// Width of the shader's opacity ramp, in its 0-1 index units: eight palette
/// indices.
pub const EDGE_SOFT_WIDTH: f32 = 8.0 / 255.0;

/// Cells one march step advances along the ray on the cloud rung.
pub const CLOUD_STEP_CELLS: f32 = 0.5;

/// The reconstruction level the cloud look marches the grid at, in mip units
/// — **the ceiling of the knob's travel, not what every box gets**. The level
/// a frame actually marches at is [`cloud_reconstruction_lod_for`], which
/// tapers this to zero as the grid's cells coarsen.
pub const CLOUD_RECONSTRUCTION_LOD: f32 = 1.0;

/// Cell size at or below which the cloud rung smooths at the full
/// [`CLOUD_RECONSTRUCTION_LOD`], in kilometres per cell.
pub const CLOUD_SMOOTHING_FULL_CELL_KM: f32 = 0.65;

/// Cell size at or above which the cloud rung smooths not at all, in
/// kilometres per cell.
pub const CLOUD_SMOOTHING_RAW_CELL_KM: f32 = 1.75;

/// The reconstruction level the cloud rung marches a grid of this cell size at:
/// [`CLOUD_RECONSTRUCTION_LOD`] at or below [`CLOUD_SMOOTHING_FULL_CELL_KM`],
/// zero at or above [`CLOUD_SMOOTHING_RAW_CELL_KM`], linear between.
/// `largest_cell_km` is the grid's coarsest axis — the kilometres one cell
/// spans, which on every shipped box is the horizontal.
pub fn cloud_reconstruction_lod_for(largest_cell_km: f32) -> f32 {
    let travel = CLOUD_SMOOTHING_RAW_CELL_KM - CLOUD_SMOOTHING_FULL_CELL_KM;
    let weight = ((CLOUD_SMOOTHING_RAW_CELL_KM - largest_cell_km) / travel).clamp(0.0, 1.0);
    CLOUD_RECONSTRUCTION_LOD * weight
}

/// Whether a grid uploaded on this device, at this cell size, will ever have
/// its coarse mip level sampled — and so whether the upload should allocate
/// one. See [`CoarseLevel`] for the cost, and the cross-reference behind the
/// answer.
fn coarse_level_for(gradient_shading: bool, largest_cell_km: f32) -> CoarseLevel {
    if gradient_shading && cloud_reconstruction_lod_for(largest_cell_km) > 0.0 {
        CoarseLevel::Built
    } else {
        CoarseLevel::Omitted
    }
}

/// The march's skip threshold for a palette whose
/// [`VoxelGrid::fade_band`](rustdar_radar::voxel::VoxelGrid::fade_band) is
/// `band`, in the shader's 0-1 index units.
pub fn empty_index_threshold_for(band: u8) -> f32 {
    (f32::from(band) + 0.5) / 255.0
}

/// The fade band the march should anchor on: the palette's own, unless the
/// user has drawn a Volume Alpha curve — then the **curve's**.
pub fn effective_fade_band(palette_band: u8, curve: Option<&AlphaCurve>) -> u8 {
    curve.map_or(palette_band, AlphaCurve::fade_band)
}

/// The colour table as the GPU should hold it: the grid's own bytes, with the
/// alpha channel replaced by the user's curve when one exists.
pub fn effective_lut<'a>(base: &'a [u8], curve: Option<&AlphaCurve>) -> Cow<'a, [u8]> {
    let Some(curve) = curve else {
        return Cow::Borrowed(base);
    };
    let mut out = base.to_vec();
    for (entry, alpha) in out.chunks_exact_mut(4).zip(curve.alphas()) {
        entry[3] = *alpha;
    }
    if let Some(no_data) = out.get_mut(3) {
        *no_data = 0;
    }
    Cow::Owned(out)
}

/// A voxel grid the store is holding, or the state of not holding one yet.
#[derive(Clone)]
pub enum VolumeEntry {
    /// A build is in flight for this target.
    Building,
    /// Built. The `Arc` is shared with every callback that draws it.
    Ready(Arc<VoxelGrid>),
    /// Not built, and why — in a sentence fit for the centre of a pane.
    Refused(String),
}

/// How a holder holds what it asks the store for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    /// One grid at a time. Attaching sheds everything else this holder has,
    /// keeping a same-scope resolved grid only while a build is in flight —
    /// the seamless swap.
    Single,
    /// One of a set. Attaching sheds nothing, and the holder is obliged to
    /// state the whole set through [`VolumeStore::retain_set`] on every pass
    /// so that a set it has stopped wanting is released rather than leaked.
    Set,
}

/// The built voxel grids, refcounted by target.
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
    /// Panes holding a *set* rather than one grid — see [`Hold::Set`].
    set_holders: Vec<usize>,
}

struct StoredVolume {
    id: u64,
    target: VolumeTarget,
    entry: VolumeEntry,
    /// Which panes are holding this. Empty is impossible: the entry is dropped
    /// when the last pane lets go.
    panes: Vec<usize>,
}

impl StoredVolume {
    /// GPU texture bytes this entry's upload occupies, or 0 while there is
    /// nothing uploaded.
    fn texture_bytes(&self) -> usize {
        let VolumeEntry::Ready(grid) = &self.entry else {
            return 0;
        };
        let shape = grid.shape();
        let Ok(cells) = [shape.nx, shape.ny, shape.nz]
            .iter()
            .map(|&n| u32::try_from(n))
            .collect::<Result<Vec<u32>, _>>()
            .map(|v| [v[0], v[1], v[2]])
        else {
            return 0;
        };
        crate::raymarch::resident_grid_bytes(cells).unwrap_or(0)
    }
}

/// What the store holds for one target, with the id its GPU upload is keyed by.
pub struct VolumeLookup {
    pub id: u64,
    pub entry: VolumeEntry,
    /// This is not the target's own entry — it is a grid the pane was already
    /// holding, standing in while the build for `target` is in flight.
    pub stood_in: bool,
}

impl VolumeStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(StoreInner::default()),
        }
    }

    /// Attach `pane_idx` to `target`'s entry if one exists — built, building
    /// or refused — and say whether it did.
    pub fn share(&self, pane_idx: usize, target: &VolumeTarget) -> bool {
        self.share_held(pane_idx, target, Hold::Single)
    }

    /// [`Self::share`], saying how the holder holds it. See [`Hold`].
    pub fn share_held(&self, pane_idx: usize, target: &VolumeTarget, hold: Hold) -> bool {
        let mut inner = self.lock();
        let Some(found) = inner.entries.iter().position(|e| &e.target == target) else {
            return false;
        };
        let keep_old = matches!(inner.entries[found].entry, VolumeEntry::Building);
        match hold {
            Hold::Single => {
                inner.set_holders.retain(|&p| p != pane_idx);
                inner.shed(pane_idx, target, keep_old);
            }
            Hold::Set => inner.mark_set_holder(pane_idx),
        }
        // Re-found after the shed, which prunes entries and moves positions.
        let Some(entry) = inner.entries.iter_mut().find(|e| &e.target == target) else {
            return false;
        };
        if !entry.panes.contains(&pane_idx) {
            entry.panes.push(pane_idx);
        }
        true
    }

    /// Open a `Building` entry for `target`, attached to `pane_idx` — the
    /// worker path's in-flight marker, opened at dispatch.
    pub fn begin_build(&self, pane_idx: usize, target: &VolumeTarget) {
        self.begin_build_held(pane_idx, target, Hold::Single);
    }

    /// [`Self::begin_build`], saying how the holder holds it. See [`Hold`].
    pub fn begin_build_held(&self, pane_idx: usize, target: &VolumeTarget, hold: Hold) {
        let mut inner = self.lock();
        match hold {
            Hold::Single => {
                inner.set_holders.retain(|&p| p != pane_idx);
                inner.shed(pane_idx, target, true);
            }
            Hold::Set => inner.mark_set_holder(pane_idx),
        }
        let id = inner.next_id;
        inner.next_id += 1;
        inner.entries.push(StoredVolume {
            id,
            target: target.clone(),
            entry: VolumeEntry::Building,
            panes: vec![pane_idx],
        });
    }

    /// Resolve `target`'s `Building` entry with what the build produced, and
    /// say whether anything was waiting for it.
    pub fn complete(&self, target: &VolumeTarget, entry: VolumeEntry) -> bool {
        let mut inner = self.lock();
        let Some(found) = inner
            .entries
            .iter()
            .position(|e| &e.target == target && matches!(e.entry, VolumeEntry::Building))
        else {
            return false;
        };
        inner.entries[found].entry = entry;
        let panes = inner.entries[found].panes.clone();
        for pane in panes {
            // A set holder is exempt, and this is the line that makes a 3D loop
            // possible at all: the seamless swap's rule is "the grid that just
            // landed supersedes the one this pane was painting through the
            // wait", which is right for one grid and destroys fourteen.
            if inner.set_holders.contains(&pane) {
                continue;
            }
            inner.shed(pane, target, false);
        }
        true
    }

    /// State the whole set `pane_idx` holds, detaching it from everything else
    /// and dropping whatever nobody is left holding. Returns how many entries
    /// were dropped outright.
    pub fn retain_set(&self, pane_idx: usize, keep: &[VolumeTarget]) -> usize {
        let mut inner = self.lock();
        inner.mark_set_holder(pane_idx);
        for entry in &mut inner.entries {
            if keep.contains(&entry.target) {
                continue;
            }
            entry.panes.retain(|&p| p != pane_idx);
        }
        let before = inner.entries.len();
        inner.entries.retain(|e| !e.panes.is_empty());
        before - inner.entries.len()
    }

    /// Whether `pane_idx` is holding a set rather than one grid. See [`Hold`].
    pub fn holds_set(&self, pane_idx: usize) -> bool {
        self.lock().set_holders.contains(&pane_idx)
    }

    /// Every pane index this store is still holding something for that the
    /// layout has stopped showing — at or past `visible_panes` — in ascending
    /// order.
    pub fn hidden_holders(&self, visible_panes: usize) -> Vec<usize> {
        let inner = self.lock();
        let mut hidden: Vec<usize> = inner
            .entries
            .iter()
            .flat_map(|e| e.panes.iter().copied())
            .chain(inner.set_holders.iter().copied())
            .filter(|&pane| pane >= visible_panes)
            .collect();
        hidden.sort_unstable();
        hidden.dedup();
        hidden
    }

    /// Release everything `pane_idx` holds **as a set**, and stop treating it
    /// as a set holder. Returns how many entries were dropped outright.
    pub fn release_set(&self, pane_idx: usize) -> usize {
        if !self.lock().set_holders.contains(&pane_idx) {
            return 0;
        }
        self.retain_set(pane_idx, &[])
    }

    /// Evict resolved grids, oldest first, until the store's GPU texture bytes
    /// fit `budget`. Returns how many were evicted.
    pub fn enforce_budget(&self, budget: usize) -> usize {
        let mut inner = self.lock();
        let mut evicted = 0;
        loop {
            let total: usize = inner.entries.iter().map(StoredVolume::texture_bytes).sum();
            if total <= budget {
                return evicted;
            }
            let Some(oldest) = inner
                .entries
                .iter()
                .filter(|e| matches!(e.entry, VolumeEntry::Ready(_)))
                .map(|e| e.id)
                .min()
            else {
                // Over budget with nothing resolved to give back. Reported by
                // returning what was actually evicted rather than looping: the
                // in-flight builds land, and the next pass reclaims.
                return evicted;
            };
            inner.entries.retain(|e| e.id != oldest);
            evicted += 1;
        }
    }

    /// GPU texture bytes the store's resolved grids occupy — what
    /// [`Self::enforce_budget`] measures.
    pub fn texture_bytes(&self) -> usize {
        self.lock()
            .entries
            .iter()
            .map(StoredVolume::texture_bytes)
            .sum()
    }

    /// Record a synchronously-known result. `pane_idx` is attached to it and
    /// holds nothing else afterwards — this is for answers that need no build,
    /// like a refusal decided at dispatch time.
    pub fn insert(&self, pane_idx: usize, target: VolumeTarget, entry: VolumeEntry) {
        self.insert_held(pane_idx, target, entry, Hold::Single);
    }

    /// [`Self::insert`], saying how the holder holds it. See [`Hold`].
    pub fn insert_held(
        &self,
        pane_idx: usize,
        target: VolumeTarget,
        entry: VolumeEntry,
        hold: Hold,
    ) {
        let mut inner = self.lock();
        match hold {
            Hold::Single => inner.detach(pane_idx),
            Hold::Set => inner.mark_set_holder(pane_idx),
        }
        let id = inner.next_id;
        inner.next_id += 1;
        inner.entries.push(StoredVolume {
            id,
            target,
            entry,
            panes: vec![pane_idx],
        });
    }

    /// This pane is holding nothing. Drops whatever it was holding if it was
    /// the last one.
    pub fn release(&self, pane_idx: usize) {
        self.lock().detach(pane_idx);
    }

    /// Drop every entry whose target names `product`.
    pub fn evict_product(&self, product: &rustdar_radar::fields::Id) {
        let mut inner = self.lock();
        inner.entries.retain(|e| e.target.product != *product);
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
                stood_in: false,
            })
    }

    /// What pane `pane_idx` should paint for `target`: the target's own entry
    /// when it is resolved, else the newest same-scope grid the pane still
    /// holds — the old picture, painted through a rebuild, and flagged
    /// [`VolumeLookup::stood_in`] so the caller draws it in the box that was
    /// asked for rather than in the one it was built over.
    pub fn lookup_for_pane(&self, pane_idx: usize, target: &VolumeTarget) -> Option<VolumeLookup> {
        let inner = self.lock();
        if let Some(found) = inner
            .entries
            .iter()
            .find(|e| &e.target == target && !matches!(e.entry, VolumeEntry::Building))
        {
            return Some(VolumeLookup {
                id: found.id,
                entry: found.entry.clone(),
                stood_in: false,
            });
        }
        // The `same_scope` clause is **belt and braces, and no test can see
        // it**: `share` and `begin_build` shed the pane's out-of-scope entries
        // before this can run, so under the public API there is never an
        // out-of-scope grid attached to fall back to — mutation testing
        // confirmed removing *this clause alone* changes nothing observable.
        inner
            .entries
            .iter()
            .filter(|e| {
                e.panes.contains(&pane_idx)
                    && same_scope(&e.target, target)
                    && matches!(e.entry, VolumeEntry::Ready(_))
            })
            .max_by_key(|e| e.id)
            .map(|e| VolumeLookup {
                id: e.id,
                entry: e.entry.clone(),
                stood_in: true,
            })
    }

    /// Every id the store is still holding. The GPU side keeps exactly these
    /// uploads and frees the rest.
    pub fn live_ids(&self) -> Vec<u64> {
        self.lock().entries.iter().map(|e| e.id).collect()
    }

    /// Host bytes the store is holding, and how many volumes that is.
    pub fn memory_bytes(&self) -> usize {
        self.lock()
            .entries
            .iter()
            .map(|e| match &e.entry {
                VolumeEntry::Building => 0,
                VolumeEntry::Ready(grid) => grid.memory_bytes(),
                VolumeEntry::Refused(why) => why.len(),
            })
            .sum()
    }

    /// A poisoned lock is recovered from rather than propagated.
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
    /// Detach `pane_idx` from whatever it holds, dropping entries nobody
    /// holds.
    fn detach(&mut self, pane_idx: usize) {
        self.set_holders.retain(|&p| p != pane_idx);
        for entry in &mut self.entries {
            entry.panes.retain(|&p| p != pane_idx);
        }
        self.entries.retain(|e| !e.panes.is_empty());
    }

    /// Record that `pane_idx` holds a set. Idempotent — every attach a set
    /// holder makes says so, and saying it twice must not double the list.
    fn mark_set_holder(&mut self, pane_idx: usize) {
        if !self.set_holders.contains(&pane_idx) {
            self.set_holders.push(pane_idx);
        }
    }

    /// Detach `pane_idx` from everything it can no longer show, given that it
    /// is now aimed at `target`.
    fn shed(&mut self, pane_idx: usize, target: &VolumeTarget, keep_old: bool) {
        for entry in &mut self.entries {
            if &entry.target == target {
                continue;
            }
            let keep = keep_old
                && same_scope(&entry.target, target)
                && !matches!(entry.entry, VolumeEntry::Building);
            if !keep {
                entry.panes.retain(|&p| p != pane_idx);
            }
        }
        self.entries.retain(|e| !e.panes.is_empty());
    }
}

/// Whether an entry built for `a` may stand in for `b` — the same radar and
/// the same moment, at another data time **or over another patch of ground**.
/// The seamless swap is licensed exactly this far, and nothing else may.
fn same_scope(a: &VolumeTarget, b: &VolumeTarget) -> bool {
    a.volume.site == b.volume.site && a.product == b.product
}

/// The painter a `Gui` is handed. Turns a pane's frame state into a payload
/// `egui_wgpu` can draw, or into a sentence saying why it cannot.
pub struct BridgeVolumePainter {
    store: Arc<VolumeStore>,
    /// The quality this adapter was classified into, from
    /// `AdapterInfo::device_type`. Fixed for the life of the renderer: a device
    /// does not change class, and the thing that *does* change per frame — the
    /// pane's size — is applied by `VolumeQuality::fit` below.
    quality: VolumeQuality,
    /// The resolved `Budgets::offscreen_bytes` this renderer fits every pane's
    /// raymarch target into. Handed in rather than read from a `cfg` constant,
    /// for the reason `quality` is: a budget read inline is a budget checkable
    /// on the one arm the test runner compiled.
    offscreen_bytes: usize,
    /// What the capability probe said when the renderer was built. Re-consulted
    /// through `volume::support` on every frame, so a device error latched
    /// halfway through a session degrades the pane rather than being remembered
    /// only until the next restart.
    probed: VolumeSupport,
    /// The largest floor magnification any pane reported since this was last
    /// taken — what the adaptive mirror rung is chosen from.
    floor_demand: std::sync::atomic::AtomicU32,
}

/// The bit pattern [`BridgeVolumePainter::floor_demand`] holds when no pane
/// asked for a floor.
const NO_FLOOR_DEMAND: u32 = u32::MAX;

impl BridgeVolumePainter {
    pub fn new(
        store: Arc<VolumeStore>,
        quality: VolumeQuality,
        offscreen_bytes: usize,
        probed: VolumeSupport,
    ) -> Self {
        Self {
            store,
            quality,
            offscreen_bytes,
            probed,
            floor_demand: std::sync::atomic::AtomicU32::new(NO_FLOOR_DEMAND),
        }
    }

    /// The frame's floor magnification demand, clearing it for the next frame.
    pub fn take_floor_demand(&self) -> Option<f32> {
        let bits = self
            .floor_demand
            .swap(NO_FLOOR_DEMAND, std::sync::atomic::Ordering::Relaxed);
        (bits != NO_FLOOR_DEMAND).then(|| f32::from_bits(bits))
    }

    /// Fold one pane's magnification into the frame's demand, keeping the
    /// largest.
    fn record_floor_demand(&self, magnification: f32) {
        use std::sync::atomic::Ordering::Relaxed;
        let seen = self.floor_demand.load(Relaxed);
        let folded = if seen == NO_FLOOR_DEMAND {
            magnification
        } else {
            f32::from_bits(seen).max(magnification)
        };
        self.floor_demand.store(folded.to_bits(), Relaxed);
    }
}

impl VolumePainter for BridgeVolumePainter {
    fn paint(&self, frame: &VolumeFrameState) -> VolumePaint {
        // Re-asked every frame rather than cached: `volume::support` folds in
        // the process-global latch that `install_error_latch` and the
        // two-strike surface-loss counter write, and neither of those had
        // happened when this painter was built.
        if let Some(why) = crate::support(&self.probed).reason() {
            return VolumePaint::Empty(why.to_owned());
        }

        // Through the pane-scoped lookup, which is the seamless swap: while a
        // rebuild for this target is in flight, the pane's previous grid of the
        // same site and product answers, so a live volume updating every sealed
        // sweep — or a box the user has just zoomed — repaints rather than
        // flashing "Building…".
        let Some(found) = self.store.lookup_for_pane(frame.pane_idx, &frame.target) else {
            // Nothing paintable at all — the very first build, or a hard
            // retarget with nothing old worth showing.
            return VolumePaint::Empty(format!(
                "Building the {} volume...",
                field_code(&frame.target.product),
            ));
        };
        let grid = match &found.entry {
            VolumeEntry::Ready(grid) => Arc::clone(grid),
            // Unreachable through `lookup_for_pane`, which never answers with
            // a `Building` entry — but the enum says it can, so the honest
            // fallback is the same first-build message.
            VolumeEntry::Building => {
                return VolumePaint::Empty(format!(
                    "Building the {} volume...",
                    field_code(&frame.target.product),
                ));
            }
            VolumeEntry::Refused(why) => return VolumePaint::Empty(why.clone()),
        };

        // On the tilt *count*, never on "the index plane is all no-data".
        if grid.tilt_count() == 1 {
            return VolumePaint::Empty(
                "This volume has a single tilt, so there is no vertical structure to render. \
                 Wait for a full scan."
                    .to_owned(),
            );
        }

        // After the grid is built rather than before, deliberately: the answer
        // is a property of the table that travels *inside* the grid, and
        // reading it from a second copy of the palette would be a second copy
        // to keep in step.
        if let Some(why) = palette_refusal(&grid) {
            return VolumePaint::Empty(why);
        }

        let fitted = self.quality.fit(frame.size_px, self.offscreen_bytes);
        // The box the pane asked for, which is the grid's own whenever the
        // build for it has landed.
        let Some(drawn) = DrawnBox::for_lookup(&found, &frame.target, &grid) else {
            // A stand-in whose target's box cannot be placed — see
            // `DrawnBox::for_target`. Blank rather than draw a picture over
            // ground it is not over.
            return VolumePaint::Empty(format!(
                "Building the {} volume...",
                field_code(&frame.target.product),
            ));
        };
        let box_size_km = drawn.size_km();
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
        // Where the drawn box's unit cube sits in the grid. The identity while
        // nothing is pending, which is a multiply by one and an add of zero.
        uniform.grid_from_box_scale = drawn.scale;
        uniform.grid_from_box_offset = drawn.offset;
        uniform.grid_bounded = drawn.bounded;
        // The stretch the pane is drawn at, for the shading's normals only —
        // `OrbitCamera` floors it at 1, which is what licenses the shader to
        // divide by it unguarded.
        uniform.vertical_exaggeration = frame.camera.vertical_exaggeration();
        // The rung this pane actually got, not the one the adapter was offered:
        // The fit can step the resolution down, and shading rides the same
        // struct.
        uniform.gradient_shading = fitted.quality.shading.is_on();
        if fitted.quality.shading.is_on() {
            uniform.reconstruction_lod = cloud_reconstruction_lod_for(largest_cell_km(&uniform));
            uniform.step_cells = CLOUD_STEP_CELLS;
        }
        // The march's transfer edge, anchored at the **effective** fade
        // boundary: the palette's own unless a Volume Alpha curve is applied,
        // then the curve's — [`effective_fade_band`] holds the whole decision
        // and its reasoning.
        uniform.empty_index_threshold =
            empty_index_threshold_for(effective_fade_band(grid.fade_band(), frame.alpha.as_ref()));
        uniform.edge_soft_width = EDGE_SOFT_WIDTH;

        // The view mode. In isosurface mode the two formerly-reserved lanes
        // carry the crossing parameters, translated against this grid's own
        // ramp so the surface sits exactly where the ramp puts the value.
        if frame.view_mode == rustdar_egui::pane::VolumeViewMode::Isosurface {
            let (centre, threshold) = grid.iso_uniform_params(frame.iso_threshold);
            uniform.iso_centre = centre;
            uniform.iso_threshold = threshold;
            uniform.empty_index_threshold = empty_index_threshold_for(0);
            uniform.reconstruction_lod = 0.0;
        }

        // The floor: drawn only when the pane wants it AND the map it was
        // dragged on has told us where it is.
        let floor = frame.floor.then_some(frame.source).flatten().map(|geo| {
            let (site_lat, site_lon) = grid.site();
            let site_points = geo.project(site_lat, site_lon);
            FloorSource {
                site_points: [site_points.x, site_points.y],
                points_per_degree_lon: geo.points_per_degree_lon as f32,
                points_per_mercator_y: geo.points_per_mercator_y as f32,
                site_lat: site_lat as f32,
                // The box's west and south edges as kilometres east and north
                // of the site — its *position*, which `box_size_km` does not
                // carry.
                west_km: drawn.x_km.0 as f32,
                south_km: drawn.y_km.0 as f32,
                mirror_size_points: frame.mirror_size_points,
            }
        });
        uniform.map_floor = floor.is_some();

        // What the adaptive mirror rung is chosen from. Recorded only when a
        // floor is actually resolved, so a pane with the floor hidden — or one
        // whose source map has not said where it is — asks for no texels.
        if let Some(geo) = floor.is_some().then_some(frame.source).flatten() {
            let (site_lat, _) = grid.site();
            if let Some(magnification) = rustdar_egui::volume_view::floor_magnification(
                frame.camera,
                uniform.box_size_km,
                frame.size_px[1] as f32 / frame.pixels_per_point.max(f32::MIN_POSITIVE),
                geo.points_per_degree_lon,
                site_lat,
            ) {
                self.record_floor_demand(magnification);
            }
        }

        // What the caption is allowed to claim. The box it names is the drawn
        // one and is therefore true either way; what it must not do is report
        // the *requested* box's cell size while a coarser grid is on screen.
        let showing = rustdar_egui::volume_view::Showing {
            cell_km: cell_km(&grid),
            stale: found.stood_in,
            partial: drawn.bounded,
        };

        let callback = VolumeCallback {
            pane_idx: frame.pane_idx,
            grid_id: found.id,
            grid,
            floor,
            // The Volume Alpha curve rides to `prepare`, which owns the LUT
            // upload — the one seam the curve is applied at.
            alpha: frame.alpha.clone(),
            uniform,
            offscreen_px: fitted.size,
            live_ids: self.store.live_ids(),
        };

        VolumePaint::Callback {
            payload: paint_payload(callback),
            showing,
        }
    }

    /// The grid's own colour table, for the Volume Alpha editor's palette
    /// strip and default curve — through the same pane-scoped lookup `paint`
    /// draws by, so the editor always shows the table the pane is actually
    /// rendering through, stand-in grid and all.
    fn palette(&self, pane_idx: usize, target: &VolumeTarget) -> Option<Vec<u8>> {
        match self.store.lookup_for_pane(pane_idx, target)?.entry {
            VolumeEntry::Ready(grid) => Some(grid.lut().to_vec()),
            VolumeEntry::Building | VolumeEntry::Refused(_) => None,
        }
    }

    /// Through the same pane-scoped lookup [`Self::paint`] uses, and through
    /// the same [`DrawnBox`] it hands the uniform — so the box the pan gesture
    /// is scaled against, the box the caption names and the box the shader
    /// marches are one derivation, not three that agree by inspection.
    fn box_size_km(&self, pane_idx: usize, target: &VolumeTarget) -> Option<[f32; 3]> {
        let found = self.store.lookup_for_pane(pane_idx, target)?;
        let VolumeEntry::Ready(grid) = &found.entry else {
            return None;
        };
        Some(DrawnBox::for_lookup(&found, target, grid)?.size_km())
    }

    /// The **held grid's** horizontal cell count, not the drawn box's.
    fn grid_cells_across(&self, pane_idx: usize, target: &VolumeTarget) -> Option<usize> {
        let found = self.store.lookup_for_pane(pane_idx, target)?;
        let VolumeEntry::Ready(grid) = &found.entry else {
            return None;
        };
        Some(grid.shape().nx)
    }
}

/// Wrap a callback in whatever `egui_wgpu` downcasts to.
fn paint_payload(callback: impl egui_wgpu::CallbackTrait + 'static) -> Arc<dyn Any + Send + Sync> {
    egui_wgpu::Callback::new_paint_callback(egui::Rect::ZERO, callback).callback
}

/// The floor's two uniform `vec4`s: the geography `paint` resolved, normalised
/// against the mirror it will be sampled from.
fn floor_lanes(
    source: &FloorSource,
    mirror_size_points: [f32; 2],
    gamma_encoded: bool,
) -> ([f32; 4], [f32; 4]) {
    let per_point_u = 1.0 / mirror_size_points[0].max(f32::MIN_POSITIVE);
    let per_point_v = 1.0 / mirror_size_points[1].max(f32::MIN_POSITIVE);
    (
        [
            source.site_points[0] * per_point_u,
            source.site_points[1] * per_point_v,
            source.points_per_degree_lon * per_point_u,
            source.points_per_mercator_y * per_point_v,
        ],
        [
            source.site_lat,
            source.west_km,
            source.south_km,
            if gamma_encoded { 1.0 } else { 0.0 },
        ],
    )
}

/// The box a pane is drawing, and where that box's unit cube sits inside the
/// grid being drawn through it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct DrawnBox {
    x_km: (f64, f64),
    y_km: (f64, f64),
    z_km_msl: (f64, f64),
    /// Grid texture coordinate from unit-cube position: `t = scale · p +
    /// offset`, per axis.
    scale: [f32; 3],
    offset: [f32; 3],
    /// The box reaches outside the grid on some axis, so the march has to
    /// answer air out there rather than let the sampler clamp.
    bounded: bool,
}

impl DrawnBox {
    fn size_km(&self) -> [f32; 3] {
        [
            (self.x_km.1 - self.x_km.0) as f32,
            (self.y_km.1 - self.y_km.0) as f32,
            (self.z_km_msl.1 - self.z_km_msl.0) as f32,
        ]
    }

    /// The grid drawn in its own box — the settled case, and the one every
    /// mask instrument in this repository measures.
    fn settled(grid: &VoxelGrid) -> Self {
        Self {
            x_km: grid.x_range_km(),
            y_km: grid.y_range_km(),
            z_km_msl: grid.z_range_km_msl(),
            scale: crate::uniform::IDENTITY_GRID_FROM_BOX.0,
            offset: crate::uniform::IDENTITY_GRID_FROM_BOX.1,
            bounded: false,
        }
    }

    /// The box `target` asks for, drawn through `grid`.
    ///
    /// Such a target's width is the volume's own reach
    /// (`rustdar_radar::voxel::box_half_width_km` over `volume_reach_km`) — a
    /// fact about a scan the renderer never sees, so there is no rectangle here
    /// to crop into. The held grid's own box is the honest answer rather than a
    /// guess, because `same_scope` has already pinned the stand-in to the same
    /// **site** and the same **product**, and the reach is a function of exactly
    /// those two: across 150 archive volumes from 53 sites every WSR-88D
    /// reports the same 460.1 km reflectivity reach, and each moment follows its
    /// own cut. So the box the pending build is about to produce is the box the
    /// held grid was already built in, and drawing it there is not a
    /// registration error — it is the same ground.
    ///
    /// The one case the two can disagree is a volume still filling: the reach is
    /// a maximum over the sweeps present, so a half-arrived volume could report
    /// a shorter one. In practice it cannot move after the first sealed sweep,
    /// because every moment's longest cut is at the bottom of the ladder — a
    /// WSR-88D's split cuts put the 460 km surveillance sweep and the 300 km
    /// Doppler sweep at 0.5°, first and second in the volume. If it ever did
    /// move, this is a stand-in behaving as a stand-in: one pop when the real
    /// grid lands, where the alternative is a blank pane every volume.
    fn for_target(target: &VolumeTarget, grid: &VoxelGrid) -> Option<Self> {
        let Some(region) = target.region else {
            return Some(Self::settled(grid));
        };
        let (site_lat, site_lon) = grid.site();
        // `clamped` is the resampler's own, not a copy of its bounds:
        // `horizontal_ranges_km` gives the arithmetic that needs the two to
        // agree bit for bit.
        let (x_km, y_km) = rustdar_radar::voxel::horizontal_ranges_km(
            (region.centre().lat, region.centre().lon),
            region.half_extent_km().clamped(),
            site_lat,
            site_lon,
        );
        let settled = Self::settled(grid);
        if (x_km, y_km) == (settled.x_km, settled.y_km) {
            return Some(settled);
        }
        // A grid with a zero horizontal axis. Impossible for anything
        // `build_voxels` produced, and a division that returned infinities here
        // would reach the GPU as a NaN matrix.
        let (scale, offset) = crop_into(grid, x_km, y_km)?;
        Some(Self {
            x_km,
            y_km,
            z_km_msl: settled.z_km_msl,
            scale,
            offset,
            // An affine that stays within `[0, 1]` on every axis needs no
            // bounds test at all, and the zoom-*in* case — the common one — is
            // exactly that.
            bounded: (0..3).any(|axis| offset[axis] < 0.0 || offset[axis] + scale[axis] > 1.0),
        })
    }

    /// The box a pane holding `lookup` for `target` is drawing.
    fn for_lookup(lookup: &VolumeLookup, target: &VolumeTarget, grid: &VoxelGrid) -> Option<Self> {
        if lookup.stood_in {
            Self::for_target(target, grid)
        } else {
            Some(Self::settled(grid))
        }
    }
}

/// Kilometres across one horizontal cell of `grid`, east–west and north–south
/// — the resolution the picture on screen really has, which is not the
/// requested region's while a stand-in is up. `None` for a grid with no cells
/// across either axis, which `build_voxels` does not produce.
fn cell_km(grid: &VoxelGrid) -> Option<(f32, f32)> {
    let axis = |(a, b): (f64, f64), cells: usize| {
        let cells = u32::try_from(cells).ok()?;
        (cells > 0).then(|| ((b - a) / f64::from(cells)) as f32)
    };
    Some((
        axis(grid.x_range_km(), grid.shape().nx)?,
        axis(grid.y_range_km(), grid.shape().ny)?,
    ))
}

/// `(scale, offset)` taking the unit cube of the box `x_km × y_km` (with the
/// grid's own vertical) to a coordinate in `grid`'s texture. `None` if the
/// grid has a zero horizontal axis.
///
/// ```text
/// world = box_min + p · box_size          (what the shader marches)
/// t     = (world − grid_min) / grid_size  (where the texture has it)
/// ```
fn crop_into(grid: &VoxelGrid, x_km: (f64, f64), y_km: (f64, f64)) -> Option<([f32; 3], [f32; 3])> {
    let axes = [
        (x_km, grid.x_range_km()),
        (y_km, grid.y_range_km()),
        // The vertical is the grid's own, so it is the identity by
        // construction rather than by arithmetic that happens to cancel.
        (grid.z_range_km_msl(), grid.z_range_km_msl()),
    ];
    let mut scale = [0.0f32; 3];
    let mut offset = [0.0f32; 3];
    for (axis, (drawn, held)) in axes.into_iter().enumerate() {
        let held_size = held.1 - held.0;
        if !(held_size.is_finite() && held_size > 0.0) {
            return None;
        }
        scale[axis] = ((drawn.1 - drawn.0) / held_size) as f32;
        offset[axis] = ((drawn.0 - held.0) / held_size) as f32;
    }
    Some((scale, offset))
}

/// The grid's coarsest cell in kilometres — the axis extent over that axis'
/// cell count, maximised over the three axes. This is what
/// [`cloud_reconstruction_lod_for`] scales the smoothing by; on every shipped
/// box the horizontal axes are the coarse ones (the vertical is ~0.14 km).
fn largest_cell_km(uniform: &VolumeUniform) -> f32 {
    (0..3)
        .map(|axis| uniform.box_size_km[axis] / uniform.grid_dims[axis].max(1) as f32)
        .fold(0.0f32, f32::max)
}

/// Why this moment cannot be drawn as a volume, or `None` if it can.
fn palette_refusal(grid: &VoxelGrid) -> Option<String> {
    palette_refusal_for(grid.see_through_indices(), grid.product().name())
}

/// [`palette_refusal`] over the two things it actually reads, so the decision is
/// testable without a `VoxelGrid` — which has no constructor outside
/// `build_voxels` and would need a synthetic `Scan` to obtain.
fn palette_refusal_for(see_through: u16, moment: &str) -> Option<String> {
    if see_through >= u16::from(MINIMUM_FADE_INDICES) {
        return None;
    }
    Some(format!(
        "{moment} cannot be drawn as a volume.\n\nIts colour table is opaque across its whole \
         scale, so every measured cell would paint at full strength and the render would be a \
         solid block, not a picture. A volume needs a see-through part of its scale - its \
         product's transparency profile is missing or has regressed.",
    ))
}

/// The wgpu side, held in egui's `CallbackResources`.
pub struct VolumeResources {
    pipelines: VolumePipelines,
    /// One offscreen per pane, sized to that pane. `Option` because
    /// `VolumePipelines::ensure_offscreen` takes the slot and decides whether to
    /// reallocate, which is what keeps a pane-sized texture from being churned
    /// at the frame rate.
    targets: HashMap<usize, Option<OffscreenTarget>>,
    /// One upload per grid, keyed by the store's id. Two panes on one volume
    /// share the entry, which is the GPU half of the store's refcounting.
    uploads: HashMap<u64, VolumeUpload>,
    /// The pane mirror: one frame-sized copy of the 2D panes' own render,
    /// shared by every 3D pane.
    mirror: Option<crate::raymarch::PaneMirror>,
    /// The host memory every grid upload widens its index plane into, held
    /// across uploads instead of allocated inside each one.
    staging: VolumeStaging,
}

/// One grid's GPU upload, and which Volume Alpha curve its colour table was
/// written through.
struct VolumeUpload {
    textures: VolumeTextures,
    /// The curve the uploaded table reflects — `None` for the grid's own
    /// palette, which is the bit-exact untouched-editor state.
    applied_alpha: Option<AlphaCurve>,
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
            mirror: None,
            // Empty, not pre-sized: the shape is not known until the first
            // upload, and a machine that never opens a 3D pane must pay nothing
            // — the same rule the mirror above follows.
            staging: VolumeStaging::new(device),
        }
    }

    /// Free everything `pane_idx` was the only user of.
    pub fn release_pane(&mut self, pane_idx: usize, live_ids: &[u64]) {
        self.targets.remove(&pane_idx);
        self.retain_uploads(live_ids);
    }

    /// Keep the uploads `live_ids` names and drop the rest.
    pub fn retain_uploads(&mut self, live_ids: &[u64]) {
        self.uploads.retain(|id, _| live_ids.contains(id));
    }

    /// Give `pane_idx` an offscreen of `size_px`, creating or resizing one only
    /// if it has to, and say whether one is in hand afterwards.
    pub fn ensure_pane_offscreen(
        &mut self,
        device: &wgpu::Device,
        pane_idx: usize,
        size_px: [u32; 2],
    ) -> bool {
        let slot = self.targets.entry(pane_idx).or_default();
        self.pipelines.ensure_offscreen(device, slot, size_px);
        slot.is_some()
    }

    /// Make `grid_id`'s upload resident — the texels once, the colour table
    /// whenever the effective one changed — and say whether it is.
    #[allow(clippy::too_many_arguments)]
    pub fn ensure_upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        grid_id: u64,
        cells: [u32; 3],
        indices: &[u8],
        palette: &[u8],
        alpha: Option<&AlphaCurve>,
        coarse: CoarseLevel,
    ) -> bool {
        // Through the entry API rather than `contains_key` + `insert`, which is
        // one hash lookup instead of two — and the upload is refusable, so this
        // is a `match` on the entry rather than `or_insert_with`.
        match self.uploads.entry(grid_id) {
            std::collections::hash_map::Entry::Occupied(occupied) => {
                let upload = occupied.into_mut();
                // The Volume Alpha seam's steady state: rewrite the 1 KiB table
                // only when the curve actually changed — a pointer comparison
                // almost every frame — and leave the 16 MiB grid untouched
                // always.
                if upload.applied_alpha.as_ref() != alpha {
                    upload
                        .textures
                        .write_lut(queue, &effective_lut(palette, alpha));
                    upload.applied_alpha = alpha.cloned();
                }
                true
            }
            std::collections::hash_map::Entry::Vacant(vacant) => {
                let Some(textures) = self.pipelines.upload_volume_at(
                    device,
                    queue,
                    cells,
                    indices,
                    &effective_lut(palette, alpha),
                    coarse,
                    &mut self.staging,
                ) else {
                    // `upload_volume` has already logged which invariant it
                    // refused on. Nothing to add, and nothing to draw.
                    return false;
                };
                vacant.insert(VolumeUpload {
                    textures,
                    applied_alpha: alpha.cloned(),
                });
                true
            }
        }
    }

    /// GPU texture bytes this is holding in the two maps
    /// [`Self::release_pane`] gives back: the panes' offscreens, and the grid
    /// uploads with their colour tables.
    pub fn resident_bytes(&self) -> usize {
        let offscreens: usize = self
            .targets
            .values()
            .flatten()
            .map(|target| rustdar_device_profile::quality::offscreen_bytes(target.size()))
            .sum();
        let uploads: usize = self
            .uploads
            .values()
            .map(|upload| upload.textures.texture_bytes())
            .sum();
        offscreens.saturating_add(uploads)
    }

    /// Give the pane mirror back, for a frame on which nothing wants a floor.
    pub fn release_mirror(&mut self) {
        self.mirror = None;
    }

    /// The mirror this frame's pass should draw into, sized to the frame and
    /// created or resized if it has to be.
    pub fn ensure_mirror(
        &mut self,
        device: &wgpu::Device,
        size: [u32; 2],
        format: wgpu::TextureFormat,
    ) -> wgpu::TextureView {
        self.pipelines
            .ensure_mirror(device, &mut self.mirror, size, format);
        // Cannot be `None`: `ensure_mirror` either kept a mirror or made one.
        // Answered rather than unwrapped because a panic here would be on the
        // frame path, where on wasm it aborts the whole application.
        self.mirror
            .as_ref()
            .map(|mirror| mirror.view().clone())
            .unwrap_or_else(|| {
                // Unreachable; a fresh 1×1 view is a cheaper failure than a
                // dead application, and the pass that draws into it is
                // harmless.
                device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some("volume.mirror.fallback"),
                        size: wgpu::Extent3d {
                            width: 1,
                            height: 1,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                        view_formats: &[],
                    })
                    .create_view(&wgpu::TextureViewDescriptor::default())
            })
    }
}

/// One 3D pane's draw, for one frame.
struct VolumeCallback {
    pane_idx: usize,
    grid_id: u64,
    grid: Arc<VoxelGrid>,
    /// Where this box's ground is inside the pane mirror, when the pane wants
    /// a floor and its source map has said where it is. `uniform.map_floor` is
    /// true exactly when this is `Some`.
    floor: Option<FloorSource>,
    /// The Volume Alpha curve the LUT must be uploaded through, or `None` for
    /// the grid's own table, bit-exactly. `prepare` compares this against
    /// what the upload cache holds and rewrites the 1 KiB table only on
    /// change — never per unchanged frame.
    alpha: Option<AlphaCurve>,
    uniform: VolumeUniform,
    offscreen_px: [u32; 2],
    /// Every grid the store still holds, so `prepare` can free the uploads for
    /// the ones it does not. Carried on the callback rather than read from the
    /// store because `prepare` runs with no access to anything but its
    /// arguments.
    live_ids: Vec<u64>,
}

/// Everything the floor's uniform lanes need that does not depend on the
/// frame's pixel size.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FloorSource {
    /// Where the radar site lands on the frame, in points.
    site_points: [f32; 2],
    /// Points of frame x per degree of longitude east.
    points_per_degree_lon: f32,
    /// Points of frame y per unit of Mercator y. Negative.
    points_per_mercator_y: f32,
    /// The site's latitude, degrees north — the origin the shader's
    /// reprojection measures from.
    site_lat: f32,
    /// The box's west edge, km east of the site.
    west_km: f32,
    /// The box's south edge, km north of the site.
    south_km: f32,
    /// The mirror's extent in points, which the positions above are normalised
    /// against. See [`floor_lanes`] and
    /// `rustdar_egui::volume_view::VolumeFrameState::mirror_size_points`.
    mirror_size_points: [f32; 2],
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
        // Everything the store has let go of, before this frame's own is made
        // resident beside it. The same line `release_pane` runs, and the reason
        // both exist is written there.
        resources.retain_uploads(&self.live_ids);

        if !resources.ensure_pane_offscreen(device, self.pane_idx, self.offscreen_px) {
            return Vec::new();
        }
        let shape = self.grid.shape();
        if !resources.ensure_upload(
            device,
            queue,
            self.grid_id,
            [shape.nx as u32, shape.ny as u32, shape.nz as u32],
            self.grid.indices(),
            // The grid's own table, through the one seam a user curve may
            // rewrite its alpha at — `ensure_upload` resolves the effective
            // bytes. See the module doc.
            self.grid.lut(),
            self.alpha.as_ref(),
            // Off the uniform, because the uniform is where the two facts
            // already agree: `gradient_shading` is the adapter's shading rung,
            // which is fixed for the renderer's life, and the cell size comes
            // from the same extents and dims the shader is handed.
            coarse_level_for(
                self.uniform.gradient_shading,
                largest_cell_km(&self.uniform),
            ),
        ) {
            return Vec::new();
        }

        // Destructured so the borrow checker can see that the pipelines are read
        // while the two maps are read beside them.
        let VolumeResources {
            pipelines,
            targets,
            uploads,
            mirror,
            // The upload above is the only reader, and it has already run —
            // including the staging ring's own submit, so nothing here is
            // waiting on a plane that has not been handed to the queue.
            staging: _,
        } = resources;
        // Both are known present — the two calls above answered `true` — and
        // both are answered rather than unwrapped because this runs on the frame
        // thread, where on wasm a panic aborts the whole application.
        let (Some(Some(target)), Some(upload)) =
            (targets.get(&self.pane_idx), uploads.get(&self.grid_id))
        else {
            return Vec::new();
        };
        let textures = &upload.textures;

        // The floor.
        let mut uniform = self.uniform;
        let floor_texture = match (self.floor.as_ref(), mirror.as_ref()) {
            (Some(source), Some(mirror)) => {
                let (uv, geo) =
                    floor_lanes(source, source.mirror_size_points, mirror.is_gamma_encoded());
                uniform.floor_uv = uv;
                uniform.floor_geo = geo;
                Some(mirror)
            }
            _ => {
                uniform.map_floor = false;
                None
            }
        };

        textures.write_uniform(queue, &uniform);
        // Into egui's own encoder, which egui submits before its own commands —
        // so the offscreen is written before the blit reads it. The other order
        // paints the previous frame's volume, which reads as input lag.
        pipelines.encode_raymarch_with_floor(egui_encoder, target, textures, floor_texture);

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

/// `pub(crate)` for one item alone: `tests::ready_grid`, the crate's only
/// real `VoxelGrid` — `build_voxels` is the sole constructor, and a second
/// copy of its fixture in another test module would be a second thing to keep
/// in step with the resampler. Everything else in here is a `#[test]`.
#[path = "volume_bridge/tests.rs"]
#[cfg(test)]
pub(crate) mod tests;

/// A field's short code, for the log lines that name what a slot is holding.
fn field_code(id: &rustdar_radar::fields::Id) -> &str {
    rustdar_radar::fields::spec_for(id).map_or_else(|| id.as_str(), |spec| spec.code)
}
