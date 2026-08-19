//! Getting a raster onto the GPU without spending the frame on it.
//!
//! # What this replaces
//!
//! `end_pass_and_upload` used to walk `FullOutput::textures_delta.set` straight
//! into `egui_wgpu::Renderer::update_texture`, which is `queue.write_texture`,
//! which is a blocking host write through the card's BAR window — see
//! [`crate::staging_ring`] for why that is slow and what it is slow *at*.
//! Nothing about that is web-specific or native-specific: it was on the frame
//! thread on every target.
//!
//! It only became a defect worth this module when the raster ceiling started
//! deriving per sweep. `budget::Budgets::raster_side_for_adapter` reports 8192
//! on this box and a WSR-88D surveillance cut asks for **7362 px** — 217 MB of
//! RGBA, once per distinct raster per volume. Measured here (RTX 3090, Vulkan,
//! release + LTO), median of five:
//!
//! | raster | bytes | one `write_texture` |
//! |-------:|------:|--------------------:|
//! |  2048² |  17 MB|             7.79 ms |
//! |  4096² |  67 MB|            31.30 ms |
//! |  7362² | 217 MB|            59.44 ms |
//! |  8192² | 268 MB|            50.62 ms |
//!
//! Three and a half dropped frames for one upload, and `restore_cached_render`
//! walks every pane, so a resume with six distinct rasters paid all of it six
//! times in one frame.
//!
//! # The shape of the answer: bands, and DMA where there is a copy engine
//!
//! Two things, and the second is an optimisation of the first.
//!
//! **Bands.** A raster is uploaded in row bands of at most
//! [`UPLOAD_BAND_BYTES`], and a frame moves as many as its budget allows. That
//! is the mechanism, and it is the only one that holds on **every** target: it
//! needs no adapter feature, so it bounds the frame on WebGL2 and on GLES
//! exactly as it does on Vulkan, and it is what makes the ring's slots small
//! enough to be affordable.
//!
//! **DMA.** Where the device has a [`crate::staging_ring`], a band is memcpy'd
//! into cached host memory and pulled across by the copy engine instead of being
//! pushed across by the frame thread. Measured on the same box, per band, at
//! 7362²:
//!
//! | route          | 13.7 MB band | 6.9 MB band |
//! |----------------|-------------:|------------:|
//! | `write_texture`|      6.41 ms |     3.23 ms |
//! | staging + DMA  |      0.55 ms |     0.25 ms |
//!
//! 2.1 GB/s against 24.7 GB/s, which is the BAR-versus-system-RAM difference
//! [`crate::staging_ring`] derives, reproduced two orders of magnitude above the
//! plane it was first measured on.
//!
//! # Why not one of the other two things it could have been
//!
//! **DMA alone, without bands.** It does not finish the job and it cannot pay
//! for itself. One un-banded DMA of a 7362² raster still costs **10.25 ms** of
//! frame thread — the memcpy is real work even at 24.7 GB/s — and 13.22 ms at
//! 8192². More decisively, an un-banded ring slot *is the whole raster*: 218 MB
//! × [`crate::staging_ring::STAGING_RING_DEPTH`] = **437 MB of permanently
//! resident pinned host memory**, to save 49 ms once per volume. And it is
//! nothing at all on WebGL2, which has no `MAPPABLE_PRIMARY_BUFFERS` — so the
//! target with no second thread to fall back on would have kept every
//! millisecond it has today.
//!
//! **Capping the raster side so one upload fits a frame.** A 16.7 ms frame at
//! `write_texture`'s measured 2.1 GB/s is 35 MB, i.e. a **2944 px** ceiling
//! against today's 8192. Every WSR-88D sweep currently lands at exactly 2.000
//! texels per gate; at 2944 px the widest surveillance cut gets 0.80, so four
//! gates in five stop having a texel of their own. That is giving back the
//! feature to avoid fixing the upload, and it does not even work — it is the
//! fallback only if the two above had failed.
//!
//! # A band is visible before its successors land, and who does something about it
//!
//! A raster fills top-down over the frames its bands take: 7 frames (117 ms) for
//! 7362² on a ring device, 2 for 2048² without one. **This module does not hold
//! it back, and cannot.** egui mints a fresh `TextureId` per
//! `Context::load_texture`, so "hold" at this layer means the id has no bind
//! group yet, `egui_wgpu::Renderer::render` skips the mesh with a warning, and
//! the pane draws no radar at all for the same 117 ms — strictly worse than the
//! strips. Worse still, the id is opaque here: nothing at this layer knows which
//! id is the *predecessor* of which, so it could not even choose what to hold
//! instead.
//!
//! The app knows both. A pane keeps the previous raster on screen — with the
//! bounds and the metadata that describe it — and swaps the whole set when the
//! next one is whole; see `rustdar_egui::overlay_cache::OverlayTextureCache`.
//! What this module owes that is one honest answer to one question, which is
//! [`TextureUploads::is_delivered`]: **has every texel egui handed over for this
//! id reached the GPU?**
//!
//! It is a query rather than an event, and level-triggered rather than
//! edge-triggered, for the reason `OverlayTextureCache`'s own doc gives about
//! staleness: a caller that misses one frame's notification is a caller wedged
//! forever, where a caller that misses one frame's *question* asks it again on
//! the next.
//!
//! Nothing samples a raster except the paint — verified against the hover path
//! as of 73fa4619, which now reads the gate under the pointer out of the volume
//! and never touches the texture or even the rect it was drawn into. So a
//! partially-filled texture is a partially-drawn picture and never a wrong
//! answer; what the app's hold buys is that a *whole* picture is never captioned
//! with another picture's product, tilt, fold limit or ground.
//!
//! # The frame that finishes an upload has to be asked for
//!
//! [`TextureUploads::apply`] returns whether anything is still pending, and
//! `end_pass_and_upload` turns that into a zero `repaint_delay`. Without it a
//! raster that arrived on the last frame before the app went idle would sit half
//! uploaded until an unrelated input woke the loop — the app runs on
//! `ControlFlow::Wait`, so "there will be another frame" is not something this
//! may assume.

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::sync::Arc;

use egui_wgpu::Renderer;
use egui_wgpu::wgpu;

use crate::staging_ring::{Ring, device_has_ring};

/// The most one band carries, and so the size of one ring slot.
///
/// 8 MiB, because that is what `queue.write_texture` moves in **4.0 ms** at the
/// 2.1 GB/s measured through the BAR window — a quarter of a 16.7 ms frame, on
/// the route that has no copy engine to hand the work to. Every band costs at
/// most this much frame thread whichever route it takes, which is the property
/// that lets the DMA path fall back to `write_texture` mid-raster without a
/// frame suddenly costing what this module exists to remove.
///
/// It is also what makes the ring affordable: two slots of 8 MiB is **16.9 MiB**
/// of pinned host memory at the widest raster this build makes, against the
/// 437 MB an un-banded ring would need.
pub const UPLOAD_BAND_BYTES: usize = 8 << 20;

/// Bands one frame moves when the copy engine is doing it.
///
/// **One per ring slot, and that is a derivation rather than a choice.** A slot
/// claimed on this frame cannot be handed back on this frame — the copy reading
/// it has microseconds of wall clock to drain, against the ~2 ms an 8 MiB DMA
/// takes — so a frame that asks for more slots than the ring has is a frame that
/// gets declined, and a declined band either waits (slower) or falls through to
/// `write_texture` (the thing being removed). Measured, while this was four
/// bands against a ring of two: a 7362² raster took **13 frames with a 6.59 ms
/// worst frame**, because a third of the bands ran out of [`DECLINE_PATIENCE`]
/// and were pushed across by hand. At [`STAGING_RING_DEPTH`] it is 14 frames
/// with a **0.61 ms** worst frame after the first.
///
/// The frames a slot then gets to remap in are real frames, which is the
/// assumption `staging_ring`'s depth was chosen under.
///
/// [`STAGING_RING_DEPTH`]: crate::staging_ring::STAGING_RING_DEPTH
pub const DMA_BANDS_PER_FRAME: usize = crate::staging_ring::STAGING_RING_DEPTH;

/// Consecutive frames the ring may decline a band before it is pushed across by
/// `write_texture` regardless.
///
/// A declined claim is the ring saying the GPU has not drained the slot yet, and
/// the right answer is almost always to wait one frame — that is the whole
/// design (`crate::staging_ring`, "why the ring never blocks"). But *almost*
/// always is not a liveness argument, and the failure it leaves open is a raster
/// that never finishes rather than one that finishes late: a slot whose
/// `map_async` errored never comes back, and with a ring of depth 2 two such
/// slots are a permanent refusal. So patience is bounded, and running out of it
/// costs one band's [`UPLOAD_BAND_BYTES`] on the frame thread — the same 4 ms a
/// ringless device pays every frame — rather than a pane that never draws.
const DECLINE_PATIENCE: u32 = 4;

/// egui's texture deltas, moved across in bounded bands.
///
/// One per [`super::EguiRenderer`], built from the device so that whether there
/// is a ring is a runtime capability answered once rather than a `cfg`.
pub struct TextureUploads {
    /// Built **eagerly**, at construction, on a device that can have one.
    ///
    /// `VolumeStaging` builds its ring lazily because a session may never open a
    /// 3D pane. Every session draws a raster, so laziness here buys nothing and
    /// costs a frame: measured, the first upload through a cold ring took
    /// **10.04 ms** where the second and later ones took 1.4 — two buffer
    /// creations, two `map_async`es and 16.9 MiB of first-touch page faults, all
    /// on whichever frame the first raster happened to land on. Doing it in
    /// `EguiRenderer::new` moves that to start-up, where there is no frame to
    /// drop.
    ring: Option<Ring>,
    /// Whether this device could have a ring at all. See
    /// [`crate::staging_ring::device_has_ring`].
    capable: bool,
    /// Textures this module allocated and therefore owns.
    ///
    /// **Ownership is sticky, and that is load-bearing.** egui's renderer holds
    /// a 1×1 stand-in under the same id (see [`Self::adopt`]), so routing a
    /// later delta for an owned id back to `Renderer::update_texture` would copy
    /// a full-size image into a 1×1 texture. Every arm below therefore decides
    /// on `owned.contains_key` and never on the delta's shape.
    owned: HashMap<egui::TextureId, wgpu::Texture>,
    /// Bands still to move, oldest first, so a raster that arrived earlier
    /// completes before one that arrived later starts.
    pending: VecDeque<Band>,
    /// Ids every texel of whose latest delta has reached the GPU. See
    /// [`Self::is_delivered`].
    ///
    /// **Every** id this module is shown, not only the banded ones: a delta that
    /// went straight to `Renderer::update_texture` is delivered by the time this
    /// frame's queue is submitted, and an answer of "no, because I never banded
    /// it" would be a hold that never ends. The set is what makes the query
    /// total.
    ///
    /// It does not grow without bound. [`Self::free`] takes an id out as egui
    /// retires it, and egui retires an id when the last `TextureHandle` for it
    /// drops — so this holds one `u64`-sized key per *live* texture, which is
    /// the font atlas, the map tiles' LRU and one raster per pane.
    delivered: HashSet<egui::TextureId>,
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
    ///
    /// **16.9 MiB of pinned host memory on a ring-capable device, and nothing at
    /// all without one.** Fixed for the life of the renderer, and independent of
    /// how large a raster ever gets: [`BandPlan::of`] sizes a band against the
    /// padded stride, so a slot of [`UPLOAD_BAND_BYTES`] holds any band of any
    /// raster this build can make. `Ring::grow` is still reachable, but only by
    /// an image whose single row is wider than a whole band — 2 M px, against a
    /// 32768 texture limit.
    pub fn new(device: &wgpu::Device) -> Self {
        let capable = device_has_ring(device);
        Self {
            ring: capable.then(|| {
                Ring::new(
                    device,
                    UPLOAD_BAND_BYTES as wgpu::BufferAddress,
                    "rustdar.raster.staging",
                )
            }),
            capable,
            owned: HashMap::new(),
            pending: VecDeque::new(),
            delivered: HashSet::new(),
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
        }
    }

    /// Whether this device can stage through host memory at all.
    ///
    /// Exposed so a test can say which arm it is on rather than inferring it
    /// from a timing.
    pub fn has_ring(&self) -> bool {
        self.capable
    }

    /// Bands this may move in one frame.
    ///
    /// One without a ring, because a band is then `write_texture` and costs the
    /// frame thread its whole 4 ms. [`DMA_BANDS_PER_FRAME`] with one, because
    /// that is how many slots there are to claim.
    fn bands_per_frame(&self) -> usize {
        if self.capable { DMA_BANDS_PER_FRAME } else { 1 }
    }

    /// File this frame's deltas and move what the budget allows.
    ///
    /// Returns whether anything is still pending, which the caller owes a
    /// repaint — see the module note.
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
        // A queued band counts as ownership even though `owned` has no texture
        // for it yet: between a raster being filed and the drain allocating it,
        // the renderer holds only the 1×1 stand-in, and a partial delta routed
        // by texture rather than by id would write a full-size image into that.
        let mine = self.owned.contains_key(&id) || self.pending.iter().any(|band| band.id == id);

        // Small and not already ours: exactly the call that shipped. The font
        // atlas and every overlay under a band go through here untouched, which
        // is why nothing about glyph UVs or atlas growth changes.
        if !mine && image.as_raw().len() <= UPLOAD_BAND_BYTES {
            renderer.update_texture(device, queue, id, delta);
            // Whole, on this frame's queue, before anything can draw it — so it
            // is delivered by the same test the banded path is held to. See
            // [`Self::delivered`] for why the cheap path has to answer at all.
            self.delivered.insert(id);
            return;
        }

        let mut allocate = None;
        if delta.pos.is_none() {
            // A whole image: ours to allocate, at exactly the size egui asked
            // for — but **not now**. Creating the texture is VRAM allocation,
            // and `restore_cached_render` hands over every pane's raster in one
            // pass: six 217 MB creations measured **27.26 ms** on the frame that
            // filed them, which is the defect again wearing a different hat. The
            // drain does it instead, on the frame that first has budget to move
            // a band into it, so the allocations spread exactly as the bands do.
            //
            // What does happen now is the 1×1 seed, so the id is never
            // unbound: `Renderer::render` silently skips a mesh whose id it does
            // not hold, and a pane that vanished for a frame would be a worse
            // bug than the one being fixed.
            self.seed(device, queue, renderer, id, delta.options);
            self.pending.retain(|band| band.id != id);
            // The texture this replaces goes now. egui's bind group still holds
            // a view of it, so wgpu keeps it alive until the drain rebinds.
            self.owned.remove(&id);
            allocate = Some(delta.options);
        } else if !mine {
            // A large *partial* into a texture egui allocated. Take the texture
            // over rather than pay for the write: the bind group already points
            // at it, so nothing has to be rebound.
            let Some(existing) = renderer.texture(&id).and_then(|held| held.texture.clone()) else {
                // egui has no texture under this id, so there is nothing to
                // write into and nothing this module can do about it. Hand it
                // back and let `update_texture`'s own panic name the fault.
                renderer.update_texture(device, queue, id, delta);
                return;
            };
            self.owned.insert(id, existing);
        }

        // Filed as bands, so texels egui has handed over are not on the GPU yet.
        // Withdrawn here rather than only on the whole-image arm above, because
        // a *partial* into an adopted texture unfinishes it just as thoroughly:
        // the id was delivered a moment ago and the rows this delta names are
        // not there.
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
    ///
    /// # The 1×1 stand-in
    ///
    /// `Renderer::update_egui_texture_from_wgpu_texture_with_sampler_options` is
    /// the only way to bind a texture this module owns to an id egui minted, and
    /// it `expect`s the id to be in the renderer's map already. There is no
    /// public way to put one there without also writing a full image — which is
    /// the write being removed. So a **1×1** whole-image delta goes in first: it
    /// creates the map entry, a 4-byte texture and a bind group, and the next
    /// line replaces the bind group with one pointing at the real texture. The
    /// 1×1 is never sampled and is destroyed by `Renderer::free_texture` with
    /// everything else the id owns.
    ///
    /// That stand-in is the reason ownership is sticky; see [`Self::owned`].
    fn allocate(
        &mut self,
        device: &wgpu::Device,
        renderer: &mut Renderer,
        id: egui::TextureId,
        size: [usize; 2],
        options: egui::TextureOptions,
    ) -> wgpu::Texture {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rustdar.raster"),
            size: wgpu::Extent3d {
                width: size[0] as u32,
                height: size[1] as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // The format and the first two usages must match what
            // `Renderer::update_texture` would have created, because egui's
            // pipeline samples this through egui's own bind group layout.
            // `COPY_DST` is what `copy_buffer_to_texture` needs and what
            // `write_texture` needed before it.
            format: wgpu::TextureFormat::Rgba8Unorm,
            // `COPY_SRC` is for the tests, and worth the one word, exactly as it
            // is on `VolumePipelines`' offscreen: it is the only way
            // `tests/raster_upload_gpu.rs` can read a band back and say it
            // landed at the right row through the right stride, and both of
            // those fail silently — a wrong stride shears the picture and
            // produces no error anywhere.
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
    ///
    /// Four bytes of VRAM and a bind group. It is never sampled for longer than
    /// the frames between a raster being filed and its first band moving, and it
    /// is destroyed by `Renderer::free_texture` with everything else the id owns.
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
            let Some(plan) =
                BandPlan::of(band.image.width(), band.image.height() as u32, band.done)
            else {
                // A finished or degenerate band, dropped rather than requeued —
                // and *delivered*, because there is nothing left to move. A
                // zero-width image is not a size `plan_view_image` will make,
                // but "no rows will ever land" answering "not yet" would be a
                // pane holding its previous picture for the rest of the
                // session, and liveness is not something to spend on a shape
                // that cannot occur.
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
                band.done += plan.rows;
                band.declined = 0;
                let done = band.done >= plan.height;
                if done {
                    // The last band of this delta. The copy is on the queue this
                    // frame submits, and the frame that reads the answer is a
                    // later one, so there is no window in which this says "whole"
                    // while the picture is not.
                    self.delivered.insert(band.id);
                } else {
                    self.pending.push_front(band);
                }
                if allocated {
                    // Creating the texture is the expensive half of this frame,
                    // and it is not measured in bytes: **4.82 ms** for a 7362²
                    // raster on this box, against the ~0.7 ms a band costs by
                    // DMA. So a frame that allocated one has spent its frame,
                    // and the rest of the budget goes to the next one. Without
                    // this, a resume filing six rasters put an allocation and a
                    // full band budget on the same frames.
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
    ///
    /// `false` is an ordinary answer, not an error — see
    /// [`crate::staging_ring::Ring::claim`].
    fn stage_band(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        band: &Band,
        plan: &BandPlan,
    ) -> bool {
        let ring = self.ring.get_or_insert_with(|| {
            Ring::new(device, plan.staged_bytes(), "rustdar.raster.staging")
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
                // The padding past the real row is left as it was. The copy
                // reads none of it, and wgpu zero-initialised the allocation
                // once at creation, so there is no uninitialised memory here.
                this.into_slice(..plan.row_bytes)
                    .copy_from_slice(plan.source_row(band, row));
                rest = next;
            }
        }
        slot.buffer().unmap();

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rustdar.raster.staging"),
        });
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: slot.buffer(),
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    // The padded stride, not the row's own: unlike
                    // `write_texture`, which repacks internally, a buffer copy
                    // is held to `COPY_BYTES_PER_ROW_ALIGNMENT`. A 7362 px
                    // raster is 29448 bytes a row against a 29696-byte stride,
                    // so this pads on the shape that matters most.
                    bytes_per_row: Some(plan.padded_row),
                    rows_per_image: Some(plan.rows),
                },
            },
            plan.destination(texture, band),
            plan.extent(),
        );
        // Submitted here rather than borrowed from the frame's encoder, for the
        // reason `Slot::remap` gives: a map asked for against an unsubmitted
        // copy resolves early and panics at submission on a mapped buffer.
        queue.submit(Some(encoder.finish()));
        slot.remap();
        true
    }

    /// Forget everything egui retired this frame.
    ///
    /// Called from `EguiRenderer::free_textures`, after the submit, alongside
    /// `Renderer::free_texture`. Dropping the `wgpu::Texture` is what actually
    /// releases the VRAM this module allocated; wgpu keeps it alive until any
    /// submission still referencing it retires.
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
    ///
    /// The question a pane asks before it swaps the picture it is showing for
    /// the one behind this id; see the module note. `false` for an id this
    /// module has never been shown, which is the honest answer while a
    /// `load_texture` from *this* frame is still sitting in `TextureManager`
    /// waiting for `end_pass` to hand its delta over.
    ///
    /// **An id from a renderer that no longer exists answers `false` forever**,
    /// because a rebuilt `EguiRenderer` starts with an empty set. That is the
    /// right answer to the question asked — those texels are on no GPU — and it
    /// is why `App::restore_cached_render` lets go of every held raster before
    /// it re-uploads: a hold whose id died with its context is the one hold
    /// nothing else would ever end.
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
    ///
    /// Only [`Self::free`]'s side of the bookkeeping can be reached without a
    /// GPU — filing and draining both take a `wgpu::Device` — and that side is
    /// the one that keeps the set the size of the live texture list rather than
    /// the size of the session. The delivering half is checked for real in
    /// `tests/raster_upload_gpu.rs`.
    #[cfg(test)]
    pub fn mark_delivered_for_test(&mut self, id: egui::TextureId) {
        self.delivered.insert(id);
    }

    /// The texture this module allocated for `id`, if it owns one.
    ///
    /// The counterpart of `egui_wgpu::Renderer::texture`, and the **only** way
    /// to reach a raster's real pixels: the renderer's own entry for an owned id
    /// holds the 1×1 stand-in [`Self::adopt`] seeded it with, because
    /// `update_egui_texture_from_wgpu_texture_with_sampler_options` replaces the
    /// bind group and leaves the texture beside it alone. Nothing in the app
    /// asks either of them — egui paints through the bind group — so this exists
    /// for `raster_upload_gpu` to read a band back and say it landed.
    pub fn texture(&self, id: egui::TextureId) -> Option<&wgpu::Texture> {
        self.owned.get(&id)
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
    /// across, or `None` when there is nothing left to move.
    ///
    /// One band, always — the frame's budget is a *count* of these, because the
    /// thing being rationed is ring slots (see [`DMA_BANDS_PER_FRAME`]) and not
    /// bytes. Rationing bytes instead left a remainder at the end of every
    /// frame, which planned a third band of five rows that the two-slot ring had
    /// nothing to put it in.
    ///
    /// Over the dimensions rather than over a [`Band`] so that the shapes that
    /// matter can be checked without allocating them: a 7362² raster is 217 MB,
    /// which is not something a unit test should be building to ask how many
    /// rows fit in 8 MiB.
    ///
    /// **At least one row, always**, even when a single row is wider than the
    /// whole budget. A plan of zero rows would be requeued unchanged forever,
    /// and forever is a pane that never finishes drawing rather than a frame
    /// that costs too much. No raster this build makes comes close — one row at
    /// the 8192 ceiling is 32 KB against an 8 MiB band — but the arithmetic is
    /// what guarantees progress and it should not depend on that.
    fn of(width: usize, height: u32, done: u32) -> Option<Self> {
        if width == 0 || done >= height {
            return None;
        }
        let row_bytes = width * 4;
        let padded_row = u32::try_from(row_bytes)
            .ok()?
            .next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        // Against the **padded** stride, not the row's own, so that a band never
        // needs a staging slot larger than [`UPLOAD_BAND_BYTES`]. That is what
        // lets the ring be built once, at a fixed size, before any raster is
        // known — see [`TextureUploads::new`]. The frame's own cost is still the
        // real bytes, [`Self::bytes`]; the padding is not work it does.
        let capped = (UPLOAD_BAND_BYTES / padded_row as usize).max(1);
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
///
/// The route every target has: no adapter feature, no staging buffer, and no
/// row padding — `write_texture` repacks internally where a buffer copy may not.
/// It is what WebGL2 and GLES use for every band, and what a ring device uses
/// for a band it has run out of [`DECLINE_PATIENCE`] for.
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
///
/// `egui_wgpu::renderer::create_sampler` is private, and
/// `update_egui_texture_from_wgpu_texture_with_sampler_options` is the only
/// entry point that takes a texture this module owns — so the mapping from
/// `TextureOptions` to a `wgpu::SamplerDescriptor` has to be restated here.
/// Restated *exactly*: a raster loaded `NEAREST` that came back `LINEAR` would
/// blur a plan view's gate edges into a gradient and paint the impression that
/// the data was measured continuously, which is the one thing the plan view's
/// sampling choice exists to refuse.
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
        label: Some("rustdar.raster.sampler"),
        mag_filter: filter(options.magnification),
        min_filter: filter(options.minification),
        address_mode_u: address,
        address_mode_v: address,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests;
