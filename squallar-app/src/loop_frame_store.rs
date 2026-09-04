//! **One copy of every 2D loop frame, however many panes show it.**
//!
//! Two panes on the same site, product and tilt (and, for a section, the
//! same line and vector) at the same instant are looking at one picture.
//! Before this store each pane owned its own `LoopFrame` texture, and the
//! only way the second pane avoided rendering and uploading the picture again
//! was a sibling scan gated on the panes being layer-linked in one group — so
//! two unlinked panes held two textures, two hover sources and paid the render
//! twice. The user's ruling is that *the same data on both should deduplicate
//! both resident memory and the work done to render*, linked or not, and this
//! is where that is kept.
//!
//! The model is [`squallar_volumetric::bridge::VolumeStore`]: entries keyed by
//! what built them, refcounted by the panes holding them, dropped when the
//! last holder lets go. The difference is what a pane holds. A 3D pane holds a
//! grid *id* because the raymarch names its upload by id; a 2D pane's
//! [`LoopFrame`](squallar_egui::pane::LoopFrame) keeps a clone of the
//! [`LoopFrameImage`] itself, because that is what the painter in
//! `squallar-egui` reads and an `egui::TextureHandle` is already an id with a
//! retain count behind it — a clone shares the texture, and the GPU copy is
//! freed when the last handle drops. So the store's clone plus every pane's
//! clone are one texture, and what the store adds is the **identity** (a
//! lookup by key, so a pane owed a picture takes the finished one whatever its
//! links say), the **holders** (so a frame one pane scrubbed away from stays
//! while another pane's render set still names it), and the **count** the
//! telemetry says.
//!
//! **Holders are re-stated every pass**, the way [`Hold::Set`] holders state
//! their set through `retain_set`: the dispatch walk clears them, every pane
//! names the frames its render set wants and the ones it is still holding
//! under budget — filing any picture it holds that the store has never seen,
//! so every picture a pane holds is one every other pane may take, however
//! it got there — and an entry nobody named is dropped. Between passes the
//! arrival and clone paths add holders as they hand pictures out.
//!
//! [`Hold::Set`]: squallar_volumetric::bridge::Hold::Set

use chrono::NaiveDateTime;
use squallar_egui::pane::{LoopFrameImage, RenderTarget, SectionLoopKey};
use squallar_radar::types::RenderView;

/// **What one 2D loop frame is a picture of.** The plan-view half is the
/// [`RenderTarget`] — site, product, and the tilt where the view says it
/// selects the picture, compared by the same tenths bucket the render's own
/// identity is built on — plus the instant; a section adds the line and the
/// vector it was cut with. The view is part of the key because a plan-view
/// raster and a section cut of one target at one instant are two pictures.
#[derive(Clone, Debug)]
pub(crate) struct LoopFrameKey {
    pub target: RenderTarget,
    pub view: RenderView,
    pub timestamp: NaiveDateTime,
    /// The section half, `Some` exactly when `view` is a cross-section.
    pub section: Option<SectionLoopKey>,
}

impl LoopFrameKey {
    pub fn plan_view(target: RenderTarget, timestamp: NaiveDateTime) -> Self {
        Self {
            target,
            view: RenderView::PlanView,
            timestamp,
            section: None,
        }
    }

    pub fn section(target: RenderTarget, key: SectionLoopKey, timestamp: NaiveDateTime) -> Self {
        Self {
            target,
            view: RenderView::CrossSection,
            timestamp,
            section: Some(key),
        }
    }

    /// Whether this key names the picture `other` does. Cheapest term first:
    /// the store is scanned once per frame slot per pane per pass, and the
    /// instant rejects almost every entry before a string is compared.
    pub fn matches(&self, other: &Self) -> bool {
        self.matches_parts(
            &other.target,
            other.view,
            other.section.as_ref(),
            other.timestamp,
        )
    }

    fn matches_parts(
        &self,
        target: &RenderTarget,
        view: RenderView,
        section: Option<&SectionLoopKey>,
        timestamp: NaiveDateTime,
    ) -> bool {
        self.timestamp == timestamp
            && self.view == view
            && self.section.as_ref() == section
            && self.target.matches(target, view)
    }
}

struct StoredFrame {
    key: LoopFrameKey,
    image: LoopFrameImage,
    /// Which panes hold this, as of the last pass and every hand-out since.
    holders: Vec<usize>,
}

/// The finished 2D loop frames, refcounted by key. See the module note.
#[derive(Default)]
pub(crate) struct LoopFrameStore {
    entries: Vec<StoredFrame>,
}

impl LoopFrameStore {
    /// File `image` under `key`, held by `holder`. An entry already under the
    /// key is replaced — a section re-cut against a moved ladder — and the
    /// picture it held is handed back for the caller to free; `None` when the
    /// key was new.
    pub fn insert(
        &mut self,
        key: LoopFrameKey,
        image: LoopFrameImage,
        holder: usize,
    ) -> Option<LoopFrameImage> {
        let replaced = self
            .entries
            .iter()
            .position(|e| e.key.matches(&key))
            .map(|at| self.entries.swap_remove(at).image);
        self.entries.push(StoredFrame {
            key,
            image,
            holders: vec![holder],
        });
        replaced
    }

    /// The picture filed under `key`, if any pane has finished it.
    pub fn get(&self, key: &LoopFrameKey) -> Option<&LoopFrameImage> {
        self.entries
            .iter()
            .find(|e| e.key.matches(key))
            .map(|e| &e.image)
    }

    /// Record that `holder` is showing or wants `key`'s picture. False when
    /// nothing is filed under the key, which is not an error: a render set
    /// names frames nobody has finished yet.
    pub fn hold(&mut self, holder: usize, key: &LoopFrameKey) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|e| e.key.matches(key)) else {
            return false;
        };
        if !entry.holders.contains(&holder) {
            entry.holders.push(holder);
        }
        true
    }

    /// [`Self::hold`] for every one of `frames` under one target — the pane
    /// walk's spelling, which builds no key for a stamp already filed. A
    /// stamp the pane holds a picture for that the store has never seen is
    /// **filed** here, under the pane as its holder.
    pub fn hold_frames<'a>(
        &mut self,
        holder: usize,
        target: &RenderTarget,
        view: RenderView,
        section: Option<&SectionLoopKey>,
        frames: impl IntoIterator<Item = (NaiveDateTime, Option<&'a LoopFrameImage>)>,
    ) {
        for (stamp, held) in frames {
            if let Some(entry) = self
                .entries
                .iter_mut()
                .find(|e| e.key.matches_parts(target, view, section, stamp))
            {
                if !entry.holders.contains(&holder) {
                    entry.holders.push(holder);
                }
            } else if let Some(image) = held {
                self.entries.push(StoredFrame {
                    key: LoopFrameKey {
                        target: target.clone(),
                        view,
                        timestamp: stamp,
                        section: section.cloned(),
                    },
                    image: image.clone(),
                    holders: vec![holder],
                });
            }
        }
    }

    /// Forget every holder, ahead of a pass in which each pane re-states what
    /// it holds.
    pub fn begin_pass(&mut self) {
        for entry in &mut self.entries {
            entry.holders.clear();
        }
    }

    /// Drop every entry no pane re-stated since [`Self::begin_pass`], handing
    /// the pictures back for the caller to free.
    pub fn end_pass(&mut self) -> Vec<LoopFrameImage> {
        let (kept, dropped): (Vec<StoredFrame>, Vec<StoredFrame>) =
            std::mem::take(&mut self.entries)
                .into_iter()
                .partition(|entry| !entry.holders.is_empty());
        self.entries = kept;
        dropped.into_iter().map(|entry| entry.image).collect()
    }

    /// Everything, for a device that is going away: the handles are the dead
    /// device's and every pane's copies are being cleared in the same breath.
    pub fn clear(&mut self) -> Vec<LoopFrameImage> {
        self.entries.drain(..).map(|e| e.image).collect()
    }

    /// How many panes hold `key`'s picture.
    #[cfg(test)]
    pub fn holders(&self, key: &LoopFrameKey) -> usize {
        self.entries
            .iter()
            .find(|e| e.key.matches(key))
            .map_or(0, |e| e.holders.len())
    }

    /// Frames filed.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// **Frames more than one pane holds** — the pictures this store exists
    /// to keep once. The `loop state:` line's `shared`.
    pub fn shared(&self) -> usize {
        self.entries.iter().filter(|e| e.holders.len() > 1).count()
    }

    /// **What the stored frames hold on the HOST heap**, which is far less
    /// than what they hold.
    ///
    /// A frame's pixels are the GPU's: `PlanView` and `Section` carry an
    /// `egui::TextureHandle`, and `Volume` carries only a store id. What is
    /// on this heap and countable here is each plan-view frame's resident
    /// hover field and each section's tilt vectors.
    ///
    /// **The decoded volumes a plan-view frame pins are NOT in this figure**,
    /// deliberately: they are `Arc<Scan>`s the loop download cache may hold
    /// too, and folding them in here would put radar bytes inside a family
    /// whose name says they are elsewhere. [`Self::pinned_volume_bytes`] is
    /// that figure, and it is published into the census beside the other
    /// decoded-volume families, where shared ownership is already declared.
    ///
    /// **One thing this still does not count**: a `Volume` frame's grid is
    /// the volume store's, priced there.
    ///
    /// An `Overlay` frame used to be named here as carrying an unpriced hit
    /// map. It does not: `App::place_overlay_loop_frame` sets `hit_map: None`
    /// on every overlay loop frame (`app_render.rs`), because a frame is a
    /// picture and hovers are answered from the live layer state. The
    /// unpriced hit maps are real but they are the **live pane textures'** and
    /// the in-flight dispatch closures', neither of which this store holds.
    ///
    /// O(entries), O(1) apiece.
    pub fn resident_host_bytes(&self) -> u64 {
        self.entries.iter().fold(0u64, |sum, entry| {
            let bytes = match &entry.image {
                squallar_egui::pane::LoopFrameImage::PlanView(image) => {
                    image.hover.field_bytes() as u64
                }
                squallar_egui::pane::LoopFrameImage::Section(image) => {
                    (image.tilt_elevations_deg.len() * size_of::<f64>()
                        + image.tilt_collected_ms.len() * size_of::<i64>())
                        as u64
                }
                squallar_egui::pane::LoopFrameImage::Volume(_)
                | squallar_egui::pane::LoopFrameImage::Overlay(_) => 0,
            };
            sum.saturating_add(bytes)
        })
    }

    /// **Decoded Level II volumes the stored plan-view frames are keeping
    /// alive**, summed.
    ///
    /// A loop frame's `HoverSource` is built by `HoverSource::from_volume`
    /// and holds the `Arc<Scan>` the frame was drawn from, so the readout
    /// can decode a gate on demand. Until this figure existed nothing
    /// reported it: `HoverSource::resident_bytes` returned the polar field
    /// alone, so a frame pinning tens of MB of decoded radar priced at the
    /// 5.8 KiB of its geometry, invisible to the census that gates this
    /// campaign's decisions.
    ///
    /// **This overlaps the loop download cache on purpose.** The `Arc` is
    /// cloned out of that cache, so while the entry lives both figures name
    /// the same bytes — the census's stated convention for the decoded-volume
    /// families, each reporting what it would free if it alone let go. The
    /// case that makes this figure load-bearing rather than redundant is the
    /// other one: `App::retain_loop_volumes` evicts by the timestamps panes
    /// currently list, while a stored frame is kept by whether a pane still
    /// holds it, so **a frame can outlive its cache entry** — and those bytes
    /// were priced by nothing at all.
    ///
    /// Two frames drawn from the same volume count it twice; the census's
    /// radar families are an upper bound, not a partition.
    ///
    /// O(entries), O(1) apiece — every volume was priced once at arrival.
    pub fn pinned_volume_bytes(&self) -> u64 {
        self.entries.iter().fold(0u64, |sum, entry| {
            let bytes = match &entry.image {
                squallar_egui::pane::LoopFrameImage::PlanView(image) => {
                    image.hover.pinned_volume_bytes() as u64
                }
                squallar_egui::pane::LoopFrameImage::Section(_)
                | squallar_egui::pane::LoopFrameImage::Volume(_)
                | squallar_egui::pane::LoopFrameImage::Overlay(_) => 0,
            };
            sum.saturating_add(bytes)
        })
    }
}

/// Free pictures the store let go of.
///
/// The texture handles drop **here, on the frame thread, on purpose**: a
/// handle's drop is the retain-count decrement that frees the GPU texture,
/// the pool's free lane is detached and its drops may never run, and the
/// decrement itself is a lock and an integer. The hover sources go to the
/// free lane as one payload: each is a polar field and, for a frame drawn
/// from a volume, the sweep it reads back from — the part that is memory
/// rather than a resource whose teardown matters.
pub(crate) fn discard(images: Vec<LoopFrameImage>) {
    let mut hovers = Vec::with_capacity(images.len());
    for image in images {
        match image {
            LoopFrameImage::PlanView(data) => hovers.push(data.hover),
            LoopFrameImage::Section(_) | LoopFrameImage::Volume(_) | LoopFrameImage::Overlay(_) => {
            }
        }
    }
    if !hovers.is_empty() {
        squallar_worker::offload::discard("loop-frame-store", hovers);
    }
}

#[cfg(test)]
#[path = "loop_frame_store/tests.rs"]
mod tests;
