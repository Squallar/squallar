//! Getting a raster onto the GPU without spending the frame on it.
//!
//! `queue.write_texture` is a blocking host write through the card's BAR window
//! (see [`crate::staging_ring`]). Measured here, RTX 3090 / Vulkan, median of
//! five: 2048² (17 MB) 7.79 ms, 4096² (67 MB) 31.30 ms, 7362² (217 MB)
//! 59.44 ms, 8192² (268 MB) 50.62 ms — and a WSR-88D surveillance cut asks for
//! 7362 px, once per distinct raster per volume, six panes at a time on a
//! resume.
//!
//! Two things answer it. **Bands**: a raster moves in row bands of at most
//! [`UPLOAD_BAND_BYTES`], which needs no adapter feature and so bounds the frame
//! on WebGL2 and GLES as well as Vulkan. **DMA**: where the device has a
//! [`crate::staging_ring`], a band is memcpy'd into cached host memory and
//! pulled across by the copy engine — per band at 7362², `write_texture`
//! 6.41 ms (13.7 MB) / 3.23 ms (6.9 MB) against staging+DMA 0.55 / 0.25 ms, i.e.
//! 2.1 GB/s against 24.7 GB/s.
//!
//! DMA without bands does not pay for itself: one un-banded DMA of a 7362²
//! raster is still 10.25 ms of frame thread, an un-banded ring slot is the whole
//! raster (437 MB resident pinned host memory at depth 2), and WebGL2 has no
//! `MAPPABLE_PRIMARY_BUFFERS` at all.
//!
//! A raster fills top-down over the frames its bands take (7 frames for 7362² on
//! a ring device) and this module cannot hold it back — egui mints a fresh
//! `TextureId` per `load_texture`. The drain must therefore keep asking for
//! frames: the app runs on `ControlFlow::Wait`, so "there will be another frame"
//! is not something it may assume.

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::sync::Arc;

use egui_wgpu::Renderer;
use egui_wgpu::wgpu;

use crate::staging_ring::{Ring, device_has_ring};

/// The most one band carries, and so the size of one ring slot.
///
/// 8 MiB: what `write_texture` moves in 4.0 ms at the measured 2.1 GB/s through
/// the BAR window, a quarter of a 16.7 ms frame. Every band costs at most that
/// much frame thread whichever route it takes, so the DMA path can fall back to
/// `write_texture` mid-raster. Two slots of 8 MiB is 16.9 MiB of pinned host
/// memory against the 437 MB an un-banded ring would need.
pub const UPLOAD_BAND_BYTES: usize = 8 << 20;

/// Bands one frame moves when the copy engine is doing it.
///
/// One per ring slot, derived rather than chosen: a slot claimed on this frame
/// cannot be handed back on this frame, so asking for more slots than the ring
/// has is asking to be declined. Measured at four bands against a ring of two, a
/// 7362² raster took 13 frames with a 6.59 ms worst frame (a third of the bands
/// ran out of [`DECLINE_PATIENCE`]); at `STAGING_RING_DEPTH`, 14 frames with a
/// 0.61 ms worst frame after the first.
pub const DMA_BANDS_PER_FRAME: usize = crate::staging_ring::STAGING_RING_DEPTH;

/// The largest delta that crosses whole through `Renderer::update_texture` on
/// a device of this capability, and the band size past it. On a ring device
/// both stay [`UPLOAD_BAND_BYTES`] — the ring's own measured shape. On a
/// ringless device — all of web — every byte is a blocking `write_texture` on
/// the frame thread, so both fall to
/// [`squallar_device_profile::constants::BLOCKING_BAND_BYTES`], whose sweep
/// note carries the dry-frame cost this choice was made against.
fn band_cap(capable: bool) -> usize {
    if capable {
        UPLOAD_BAND_BYTES
    } else {
        squallar_device_profile::constants::BLOCKING_BAND_BYTES
    }
}

/// Whether a delta of `bytes` for a texture this module does not own crosses
/// whole on this frame's queue rather than being filed as bands.
fn goes_whole(capable: bool, bytes: usize) -> bool {
    bytes <= band_cap(capable)
}

/// Consecutive frames the ring may decline a band before it is pushed across by
/// `write_texture` regardless.
///
/// Waiting a frame is the design, but a slot whose `map_async` errored never
/// comes back, and at depth 2 two such slots are a permanent refusal. Running
/// out of patience costs one band's [`UPLOAD_BAND_BYTES`] on the frame thread
/// rather than a pane that never draws.
const DECLINE_PATIENCE: u32 = 4;

/// What this renderer's texture uploads have actually moved.
///
/// **Product telemetry, not a campaign instrument.** It is always on, it has no
/// feature gate and no debug arm, and every field is a `u64` add on a path that
/// was already touching the same cache line. The renderer owns one, so the
/// numbers are scoped to a device and an adapter rather than to a process — the
/// scope every other figure this module quotes is already in.
///
/// # Denominator
///
/// **Every texture delta egui hands this renderer**, not only the overlay
/// rasters: the font atlas, the basemap tiles, the legend ramps and the cross
/// sections are all in here. That is deliberate — this is the sink, and what it
/// counts is what the device actually paid for. The overlay-attributable slice
/// is a different instrument with a different denominator; see
/// `squallar_egui::overlay_cache::ledger`.
///
/// # Why a zero here is readable
///
/// [`Self::deltas`] is the non-vacuity floor. Every byte figure below is zero
/// on a renderer that has been shown nothing, and zero is also what an upload
/// path that had silently stopped moving bytes would read; the two are only
/// distinguishable because a delta that was *filed* is counted whatever route
/// it then took. `deltas == 0` is "egui handed this renderer nothing";
/// `deltas > 0 && bytes() == 0` is a sink that stopped working. A gate that
/// reads the byte total without reading this one cannot tell them apart —
/// the lesson `worker_port::account` records for the reply transport.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UploadTotals {
    /// Texture deltas filed, by any route. See the type's own note: this is
    /// what makes a zero byte count readable.
    pub deltas: u64,
    /// Bytes handed whole to `Renderer::update_texture` — every delta at or
    /// under [`UPLOAD_BAND_BYTES`] for an id this module does not own. **A
    /// routing figure and a subset of [`Self::blocking_bytes`], never added to
    /// it**: `update_texture` is `write_texture` on the frame's own queue, so
    /// these bytes are blocking too, whatever the device.
    pub whole_bytes: u64,
    /// Bands [`TextureUploads::drain`] moved. The non-vacuity partner of the
    /// two banded byte figures the way [`Self::deltas`] is of all of them: a
    /// raster filed as bands but never drained shows `bands == 0` with the
    /// delta counted.
    pub bands: u64,
    /// Bytes the copy engine pulled out of a staging slot (see
    /// [`crate::staging_ring`]). Measured 24.7 GB/s against the BAR window's
    /// 2.1 GB/s, and they cost the frame a memcpy rather than a blocking host
    /// write. Always banded.
    pub staged_bytes: u64,
    /// Every byte `write_texture` pushed through the BAR window on the frame
    /// thread — whole deltas through `Renderer::update_texture` and bands on
    /// a device with no ring or past [`DECLINE_PATIENCE`] alike. **This is
    /// the figure that is frame time on every device.** It is classified by
    /// the path the bytes took, never by whether their delta straddled
    /// [`UPLOAD_BAND_BYTES`]: spike B (2026-08-30) measured Firefox's
    /// ~8.51 MB pictures banded and counted ~13 GB blocking while Chromium's
    /// ~7.57 MB pictures went whole and counted ~0.1 GB — the same ringless
    /// traffic, opposite readings, flipped by 32 px of canvas width.
    pub blocking_bytes: u64,
}

impl UploadTotals {
    /// Every byte this renderer has put on the GPU, by any route. The two
    /// terms are disjoint: a byte either blocked the frame thread or was
    /// pulled by the copy engine.
    pub fn bytes(&self) -> u64 {
        self.staged_bytes + self.blocking_bytes
    }

    /// Bytes that crossed as bands rather than whole.
    pub fn banded_bytes(&self) -> u64 {
        self.bytes() - self.whole_bytes
    }

    /// Count `bytes` handed whole to `Renderer::update_texture`: one
    /// `write_texture` on the frame's own queue, so whole AND blocking.
    /// The one ledger arithmetic for that route — [`TextureUploads::file`]
    /// and the host-test seam both call it, so they cannot drift apart.
    fn count_whole_write(&mut self, bytes: u64) {
        self.whole_bytes += bytes;
        self.blocking_bytes += bytes;
    }

    /// Count one band of `bytes`, `staged` naming the route that moved it.
    /// The one ledger arithmetic for the banded routes, shared the way
    /// [`Self::count_whole_write`] is.
    fn count_band(&mut self, bytes: u64, staged: bool) {
        self.bands += 1;
        if staged {
            self.staged_bytes += bytes;
        } else {
            self.blocking_bytes += bytes;
        }
    }

    /// How far along this ledger is, as one number, so a caller can tell
    /// "nothing has happened since I last looked" in a single compare.
    fn progress(&self) -> u64 {
        self.deltas + self.bands
    }
}

/// egui's texture deltas, moved across in bounded bands.
pub struct TextureUploads {
    /// Built **eagerly**, at construction, on a device that can have one: the
    /// first upload through a cold ring measured 10.04 ms against 1.4 ms for
    /// later ones (two buffer creations, two `map_async`es and 16.9 MiB of
    /// first-touch page faults).
    ring: Option<Ring>,
    /// Whether this device could have a ring at all. See
    /// [`crate::staging_ring::device_has_ring`].
    capable: bool,
    /// Textures this module allocated and therefore owns. Ownership is sticky:
    /// egui holds a 1×1 stand-in under the same id, so routing a later delta for
    /// an owned id back to `Renderer::update_texture` would copy a full-size
    /// image into a 1×1 texture. Every arm decides on `owned.contains_key`.
    owned: HashMap<egui::TextureId, wgpu::Texture>,
    /// Bands still to move, oldest first, so a raster that arrived earlier
    /// completes before one that arrived later starts.
    pending: VecDeque<Band>,
    /// Ids every texel of whose latest delta has reached the GPU — **every** id
    /// this module is shown, not only the banded ones, or "no, because I never
    /// banded it" would be a hold that never ends. [`Self::free`] takes an id
    /// out as egui retires it, so this holds one key per live texture.
    delivered: HashSet<egui::TextureId>,
    /// What this renderer has actually moved. See [`UploadTotals`].
    totals: UploadTotals,
    /// [`UploadTotals::progress`] at the last line [`Self::report`] logged, so
    /// a frame that moved nothing costs one `u64` compare and says nothing.
    reported: u64,
}

/// What is left of one texture's upload.
struct Band {
    id: egui::TextureId,
    /// The pixels. An `Arc` egui is already holding, so carrying it across
    /// frames costs a refcount rather than a copy.
    image: Arc<egui::ColorImage>,
    /// Where row 0 of `image` goes in the destination texture.
    origin: [u32; 2],
    /// Rows of `image` already moved.
    done: u32,
    /// Consecutive frames the ring has declined this. See [`DECLINE_PATIENCE`].
    declined: u32,
    /// Set while this raster's texture has not been created yet, holding the
    /// sampler its `load_texture` asked for. See [`TextureUploads::allocate`].
    allocate: Option<egui::TextureOptions>,
}

impl TextureUploads {
    /// Uploads for `device`, with a ring if it can have one.
    pub fn new(device: &wgpu::Device) -> Self {
        let capable = device_has_ring(device);
        Self {
            ring: capable.then(|| {
                Ring::new(
                    device,
                    UPLOAD_BAND_BYTES as wgpu::BufferAddress,
                    "squallar.raster.staging",
                )
            }),
            capable,
            owned: HashMap::new(),
            pending: VecDeque::new(),
            delivered: HashSet::new(),
            totals: UploadTotals::default(),
            reported: 0,
        }
    }

    /// Uploads with no device to ask, for a host test: bands, never DMA.
    #[cfg(test)]
    pub fn without_device() -> Self {
        Self {
            ring: None,
            capable: false,
            owned: HashMap::new(),
            pending: VecDeque::new(),
            delivered: HashSet::new(),
            totals: UploadTotals::default(),
            reported: 0,
        }
    }

    /// Whether this device can stage through host memory at all.
    pub fn has_ring(&self) -> bool {
        self.capable
    }

    /// Bands this may move in one frame.
    fn bands_per_frame(&self) -> usize {
        if self.capable { DMA_BANDS_PER_FRAME } else { 1 }
    }

    /// File this frame's deltas and move what the budget allows.
    pub fn apply(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut Renderer,
        set: &[(egui::TextureId, egui::epaint::ImageDelta)],
    ) -> bool {
        for (id, delta) in set {
            self.file(device, queue, renderer, *id, delta);
        }
        self.drain(device, queue, renderer);
        !self.pending.is_empty()
    }

    /// Route one delta: egui's own path, or a queue of bands.
    fn file(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut Renderer,
        id: egui::TextureId,
        delta: &egui::epaint::ImageDelta,
    ) {
        let egui::epaint::ImageData::Color(image) = &delta.image;
        // Counted here rather than on either arm below, so that it counts the
        // same thing whatever route the delta then takes. See [`UploadTotals`]:
        // this is what makes a zero byte total readable.
        self.totals.deltas += 1;
        // A queued band counts as ownership even before the drain allocates:
        // until then the renderer holds only the 1×1 stand-in.
        let mine = self.owned.contains_key(&id) || self.pending.iter().any(|band| band.id == id);

        // Small and not already ours: the font atlas and every overlay under
        // this device's whole-delta limit go through `update_texture`
        // untouched. On a ringless device the limit is the blocking band, so
        // a web-picture-sized raster spreads over frames instead of spending
        // one frame whole.
        if !mine && goes_whole(self.capable, image.as_raw().len()) {
            renderer.update_texture(device, queue, id, delta);
            self.totals.count_whole_write(image.as_raw().len() as u64);
            // Whole, on this frame's queue, before anything can draw it.
            self.delivered.insert(id);
            return;
        }

        let mut allocate = None;
        if delta.pos.is_none() {
            // A whole image: ours to allocate, but not now — six 217 MB
            // creations measured 27.26 ms on one frame. The drain allocates on
            // the frame that first has budget for a band, so they spread.
            self.seed(device, queue, renderer, id, delta.options);
            self.pending.retain(|band| band.id != id);
            // The texture this replaces goes now. egui's bind group still holds
            // a view of it, so wgpu keeps it alive until the drain rebinds.
            self.owned.remove(&id);
            allocate = Some(delta.options);
        } else if !mine {
            // A large *partial* into a texture egui allocated: take the texture
            // over, so the bind group needs no rebind.
            let Some(existing) = renderer.texture(&id).and_then(|held| held.texture.clone()) else {
                // No texture under this id: hand it back and let
                // `update_texture`'s own panic name the fault.
                renderer.update_texture(device, queue, id, delta);
                return;
            };
            self.owned.insert(id, existing);
        }

        // Filed as bands, so texels egui has handed over are not on the GPU yet.
        self.delivered.remove(&id);

        let origin = delta.pos.unwrap_or([0, 0]);
        self.pending.push_back(Band {
            id,
            image: Arc::clone(image),
            origin: [origin[0] as u32, origin[1] as u32],
            done: 0,
            declined: 0,
            allocate,
        });
    }

    /// Allocate a texture of `size` and make egui's renderer paint `id` with it.
    fn allocate(
        &mut self,
        device: &wgpu::Device,
        renderer: &mut Renderer,
        id: egui::TextureId,
        size: [usize; 2],
        options: egui::TextureOptions,
    ) -> wgpu::Texture {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("squallar.raster"),
            size: wgpu::Extent3d {
                width: size[0] as u32,
                height: size[1] as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Format and the first two usages must match what
            // `Renderer::update_texture` would have created.
            format: wgpu::TextureFormat::Rgba8Unorm,
            // `COPY_SRC` is the only way `tests/raster_upload_gpu.rs` can read
            // a band back; a wrong stride shears the picture silently.
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
        });

        renderer.update_egui_texture_from_wgpu_texture_with_sampler_options(
            device,
            &texture.create_view(&wgpu::TextureViewDescriptor::default()),
            sampler_descriptor(options),
            id,
        );
        self.owned.insert(id, texture.clone());
        texture
    }

    /// Put a 1×1 stand-in under `id`, so egui has something to paint it with
    /// until [`Self::allocate`] runs.
    fn seed(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut Renderer,
        id: egui::TextureId,
        options: egui::TextureOptions,
    ) {
        if renderer.texture(&id).is_some() {
            return;
        }
        let seed = egui::epaint::ImageDelta::full(
            egui::ColorImage::filled([1, 1], egui::Color32::TRANSPARENT),
            options,
        );
        renderer.update_texture(device, queue, id, &seed);
    }

    /// Move as many bands as the frame's budget allows.
    fn drain(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, renderer: &mut Renderer) {
        for _ in 0..self.bands_per_frame() {
            let Some(mut band) = self.pending.pop_front() else {
                break;
            };
            let mut allocated = false;
            let texture = match band.allocate.take() {
                // The raster's own texture, created on the frame that first has
                // budget for a band of it rather than on the frame it arrived.
                Some(options) => {
                    allocated = true;
                    let size = band.image.size;
                    self.allocate(device, renderer, band.id, size, options)
                }
                None => {
                    let Some(texture) = self.owned.get(&band.id).cloned() else {
                        // The id was freed while its bands waited. Dropping them
                        // is the whole of the cleanup: the texture went with the
                        // free.
                        continue;
                    };
                    texture
                }
            };
            let Some(plan) = BandPlan::of(
                band.image.width(),
                band.image.height() as u32,
                band.done,
                band_cap(self.capable),
            ) else {
                // A finished or degenerate band, dropped rather than requeued —
                // and *delivered*: "no rows will ever land" answering "not yet"
                // would hold the pane's previous picture for the session.
                self.delivered.insert(band.id);
                continue;
            };

            let staged = self.capable && self.stage_band(device, queue, &texture, &band, &plan);
            let moved = if staged {
                true
            } else if !self.capable || band.declined + 1 >= DECLINE_PATIENCE {
                write_band(queue, &texture, &band, &plan);
                true
            } else {
                false
            };

            if moved {
                // Counted where the bytes move, and split by the route that
                // moved them: `staged` cost the frame a memcpy, the other arm
                // cost it a blocking host write.
                self.totals.count_band(plan.bytes() as u64, staged);
                band.done += plan.rows;
                band.declined = 0;
                let done = band.done >= plan.height;
                if done {
                    // The last band. Its copy is on the queue this frame submits
                    // and the answer is read on a later frame.
                    self.delivered.insert(band.id);
                } else {
                    self.pending.push_front(band);
                }
                if allocated {
                    // Creating the texture is 4.82 ms for a 7362² raster against
                    // ~0.7 ms for a band by DMA, so a frame that allocated one
                    // has spent its budget.
                    break;
                }
            } else {
                // The ring is behind. Put it back and stop: every other band
                // this frame would be told the same thing by the same ring.
                band.declined += 1;
                self.pending.push_front(band);
                break;
            }
        }
    }

    /// Memcpy one band into a staging slot and start its copy, or say `false`.
    fn stage_band(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        band: &Band,
        plan: &BandPlan,
    ) -> bool {
        let ring = self.ring.get_or_insert_with(|| {
            Ring::new(device, plan.staged_bytes(), "squallar.raster.staging")
        });
        // Only an image with a row wider than a whole band can ask for this; see
        // [`Self::new`].
        ring.grow(device, plan.staged_bytes());
        let Some(slot) = ring.claim(device) else {
            return false;
        };

        {
            let mut view = slot.buffer().get_mapped_range_mut(..plan.staged_bytes());
            let mut rest = view.slice(..);
            for row in 0..plan.rows {
                let (this, next) = rest.split_at(plan.padded_row as usize);
                // The padding past the real row is left as it was; the copy
                // reads none of it and wgpu zero-initialised the allocation.
                this.into_slice(..plan.row_bytes)
                    .copy_from_slice(plan.source_row(band, row));
                rest = next;
            }
        }
        slot.buffer().unmap();

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("squallar.raster.staging"),
        });
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: slot.buffer(),
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    // The padded stride, not the row's own: a buffer copy is
                    // held to `COPY_BYTES_PER_ROW_ALIGNMENT` (a 7362 px raster
                    // is 29448 bytes a row against a 29696-byte stride).
                    bytes_per_row: Some(plan.padded_row),
                    rows_per_image: Some(plan.rows),
                },
            },
            plan.destination(texture, band),
            plan.extent(),
        );
        // Submitted here, not on the frame's encoder: a map asked for against
        // an unsubmitted copy resolves early and panics at submission.
        queue.submit(Some(encoder.finish()));
        slot.remap();
        true
    }

    /// Forget everything egui retired this frame.
    pub fn free(&mut self, ids: &[egui::TextureId]) {
        for id in ids {
            self.owned.remove(id);
            // What keeps [`Self::delivered`] the size of the live texture set
            // rather than the size of the session.
            self.delivered.remove(id);
        }
        self.pending.retain(|band| !ids.contains(&band.id));
    }

    /// Whether every texel egui has handed over for `id` has reached the GPU.
    pub fn is_delivered(&self, id: egui::TextureId) -> bool {
        self.delivered.contains(&id)
    }

    /// Bands not yet moved, for a test that wants to say how far along an upload
    /// is without a GPU to ask.
    #[cfg(test)]
    pub fn pending_bands(&self) -> usize {
        self.pending.len()
    }

    /// Put `id` in the delivered set without a device to deliver it with.
    #[cfg(test)]
    pub fn mark_delivered_for_test(&mut self, id: egui::TextureId) {
        self.delivered.insert(id);
    }

    /// Record a moved band the way [`Self::drain`] does, with no device to
    /// move it with — for the host test of the report's cadence. Calls the
    /// same [`UploadTotals::count_band`] the drain calls, so the seam and the
    /// real path share one arithmetic; the real arithmetic on a real adapter
    /// is `the_upload_ledger_counts_every_byte_of_a_banded_raster_once`.
    #[cfg(test)]
    pub fn note_band_for_test(&mut self, bytes: u64, staged: bool) {
        self.totals.count_band(bytes, staged);
    }

    /// Record one whole delta the way [`Self::file`]'s `update_texture` arm
    /// does, with no device to move it with — the host-test seam for the
    /// classification. Calls the same [`UploadTotals::count_whole_write`] the
    /// real arm calls; the real path on a real adapter is
    /// `a_ringless_byte_is_called_blocking_on_both_sides_of_the_band_straddle`,
    /// which is `#[ignore]`d because it needs an adapter -- run it with
    /// `cargo test -p squallar-gpu --test raster_upload_gpu -- --ignored`.
    #[cfg(test)]
    pub fn note_whole_delta_for_test(&mut self, bytes: u64) {
        self.totals.deltas += 1;
        self.totals.count_whole_write(bytes);
    }

    /// The texture this module allocated for `id`, if it owns one.
    pub fn texture(&self, id: egui::TextureId) -> Option<&wgpu::Texture> {
        self.owned.get(&id)
    }

    /// What this renderer has moved since it was built. See [`UploadTotals`].
    pub fn totals(&self) -> UploadTotals {
        self.totals
    }

    /// [`Self::totals`], but only when something has moved since the last time
    /// this was asked — so a caller can report the line on a frame that
    /// uploaded something and stay silent on one that did not.
    ///
    /// **This crate cannot report the line itself**: `squallar-gpu` declares no
    /// `log` dependency and has never held a `log::` call. The counters
    /// therefore live where the bytes move and the sentence lives where a
    /// logger exists, which is the same split
    /// `squallar_volumetric::degrade::note_surface_loss_with_volume` already
    /// uses for the surface-loss count.
    ///
    /// An idle frame costs one `u64` add and one compare.
    pub fn totals_if_moved(&mut self) -> Option<UploadTotals> {
        let progress = self.totals.progress();
        if progress == self.reported {
            return None;
        }
        self.reported = progress;
        Some(self.totals)
    }
}

/// One frame's worth of one band: which rows, and how they sit in a buffer.
struct BandPlan {
    /// Rows to move now.
    rows: u32,
    /// Rows in the whole image, so the caller can tell "done" from "more".
    height: u32,
    /// Bytes of real texels in a row.
    row_bytes: usize,
    /// That, rounded up to [`wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`].
    padded_row: u32,
}

impl BandPlan {
    /// What to move of a `width` × `height` image with `done` rows already
    /// across at a band budget of `cap` bytes, or `None` when there is
    /// nothing left to move.
    fn of(width: usize, height: u32, done: u32, cap: usize) -> Option<Self> {
        if width == 0 || done >= height {
            return None;
        }
        let row_bytes = width * 4;
        let padded_row = u32::try_from(row_bytes)
            .ok()?
            .next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        // Against the **padded** stride, so a band never needs a slot larger
        // than its cap and the ring can be built at a fixed size before any
        // raster is known.
        let capped = (cap / padded_row as usize).max(1);
        let rows = u32::try_from(capped).unwrap_or(u32::MAX).min(height - done);
        Some(Self {
            rows,
            height,
            row_bytes,
            padded_row,
        })
    }

    /// Bytes of real texels this plan moves.
    fn bytes(&self) -> usize {
        self.row_bytes * self.rows as usize
    }

    /// Bytes a staging buffer must be to hold it, padding included.
    fn staged_bytes(&self) -> wgpu::BufferAddress {
        u64::from(self.padded_row) * u64::from(self.rows)
    }

    /// Row `row` of this plan, as bytes of `band`'s image.
    fn source_row<'a>(&self, band: &'a Band, row: u32) -> &'a [u8] {
        let start = (band.done + row) as usize * self.row_bytes;
        &band.image.as_raw()[start..start + self.row_bytes]
    }

    /// Where in `texture` this plan's first row lands.
    fn destination<'a>(
        &self,
        texture: &'a wgpu::Texture,
        band: &Band,
    ) -> wgpu::TexelCopyTextureInfo<'a> {
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: band.origin[0],
                y: band.origin[1] + band.done,
                z: 0,
            },
            aspect: wgpu::TextureAspect::All,
        }
    }

    fn extent(&self) -> wgpu::Extent3d {
        wgpu::Extent3d {
            width: (self.row_bytes / 4) as u32,
            height: self.rows,
            depth_or_array_layers: 1,
        }
    }
}

/// Push one band across with `queue.write_texture`.
fn write_band(queue: &wgpu::Queue, texture: &wgpu::Texture, band: &Band, plan: &BandPlan) {
    let start = band.done as usize * plan.row_bytes;
    let bytes = &band.image.as_raw()[start..start + plan.bytes()];
    queue.write_texture(
        plan.destination(texture, band),
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            // Packed, not padded: `write_texture` repacks internally.
            bytes_per_row: Some(plan.row_bytes as u32),
            rows_per_image: Some(plan.rows),
        },
        plan.extent(),
    );
}

/// egui's own sampler, rebuilt.
fn sampler_descriptor(options: egui::TextureOptions) -> wgpu::SamplerDescriptor<'static> {
    let filter = |f: egui::TextureFilter| match f {
        egui::TextureFilter::Nearest => wgpu::FilterMode::Nearest,
        egui::TextureFilter::Linear => wgpu::FilterMode::Linear,
    };
    let address = match options.wrap_mode {
        egui::TextureWrapMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
        egui::TextureWrapMode::Repeat => wgpu::AddressMode::Repeat,
        egui::TextureWrapMode::MirroredRepeat => wgpu::AddressMode::MirrorRepeat,
    };
    wgpu::SamplerDescriptor {
        label: Some("squallar.raster.sampler"),
        mag_filter: filter(options.magnification),
        min_filter: filter(options.minification),
        address_mode_u: address,
        address_mode_v: address,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests;
