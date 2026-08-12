//! Where a voxel grid's plane waits while it crosses PCIe.
//!
//! [`VolumePipelines::upload_volume_at`] used to hand the widened plane straight
//! to `queue.write_texture`, and on the desktop shape that was **~15.5 ms of
//! blocking CPU stores on the frame thread**, against a 16.7 ms budget. This
//! module is the other route: the plane is written into a host-memory buffer the
//! GPU can read, and a `copy_buffer_to_texture` lets the card's copy engine pull
//! it across by DMA while the frame goes on.
//!
//! [`VolumePipelines::upload_volume_at`]: super::VolumePipelines::upload_volume_at
//!
//! # Why `write_texture` is slow, which is not the reason it looks like
//!
//! It is not the bytes. This box's RTX 3090 sits on a **PCIe 4.0 x4** chipset
//! link — two cascaded switches, `LnkSta: Width x4`, ~7.88 GB/s raw — so 32 MiB
//! cannot cross it faster than about 5 ms by any route at all, and nothing here
//! claims otherwise.
//!
//! What changes is **who waits**. `write_texture` stages through a buffer wgpu
//! creates with `MAP_WRITE`, and `wgpu-hal`'s Vulkan backend maps that to
//! `MemoryLocation::CpuToGpu`, which `gpu-allocator` satisfies from
//! `HOST_VISIBLE | HOST_COHERENT | DEVICE_LOCAL` — the card's 246 MiB
//! host-visible **BAR** window. Every store the host makes there is a write
//! straight down the link, and the call does not return until the last one
//! lands. Measured on this machine: **32 MiB in 15.6 ms, 2.15 GB/s**, and
//! `/proc/self/smaps` says why — the region is `/dev/nvidia0` with `Rss: 0 kB`
//! and `VmFlags: ... pf io ...`, a `VM_IO | VM_PFNMAP` window with no page
//! frames behind it.
//!
//! The buffers here ask for `MAP_READ | COPY_SRC` instead. That is the one
//! usage pair `wgpu-hal` maps to `MemoryLocation::GpuToCpu`
//! (`wgpu-hal/src/vulkan/device.rs`, the `(true, false)` arm), which is ordinary
//! cached system RAM: `/dev/nvidiactl`, `Rss: 65536 kB`, and no `io` flag. The
//! same 32 MiB memcpy costs **1.7 ms at 20 GB/s** there. The DMA that follows
//! still pays the link — it takes about 5.3 ms — but it is the copy engine
//! paying it, asynchronously, and not `prepare`.
//!
//! # Why writing through a *read* mapping is allowed
//!
//! `MAP_READ | COPY_SRC` is normally rejected outright by `wgpu-core`
//! (`device/resource.rs`, the `read_mismatch` arm), and
//! [`STAGING_RING_FEATURE`] is exactly the flag that skips that check.
//! Past it, wgpu never looks at the map mode again: `Buffer::map_raw`'s
//! `BufferMapState::Active` arm destructures `{ ref mapping, ref range, .. }`
//! and ignores the `host` field, so `get_mapped_range_mut` hands back a writable
//! pointer to a read mapping.
//!
//! That is *permitted* rather than *contracted*, so it was checked rather than
//! assumed: under the Khronos validation layer, loaded for both instance and
//! device, the whole sequence — create, map, write through the read mapping,
//! unmap, copy, submit, read back — produced **no validation message of any
//! severity**.
//!
//! # The one unstated premise: the mapping is coherent
//!
//! Everything above rests on `mapping.is_coherent`, and this module cannot
//! assert it: `is_coherent` is a field of `wgpu-hal`'s `BufferMapping`, which no
//! public wgpu API exposes. So it is written down instead, with the reason it
//! holds and the shape of the failure if it ever stopped.
//!
//! It holds because `gpu-allocator` 0.28 satisfies `MemoryLocation::GpuToCpu`
//! from `HOST_VISIBLE | HOST_COHERENT | HOST_CACHED` and falls back to
//! `HOST_VISIBLE | HOST_COHERENT` — **both arms are coherent**, so the property
//! is a consequence of the allocator's preference list rather than of luck about
//! a particular driver's memory types.
//!
//! Two things depend on it and neither is loud. `Buffer::unmap`'s flush is gated
//! on `host == HostMap::Write && !is_coherent`, so an incoherent mapping written
//! through a *read* handle would never be flushed at all. Worse,
//! `wgpu-core/src/device/mod.rs` chooses between draining the zero-init tracker
//! and leaving it armed on the same coherence test: on the non-draining branch
//! the buffer stays `NeedsInitializedMemory`, and wgpu would **zero the staging
//! buffer between the write and the copy**. The symptom of that is not a crash
//! or a validation message — it is a silently all-zero volume, which reads as
//! "the radar returned nothing" rather than as a bug here.
//!
//! Unreachable on the three backends that can have a ring at all — Vulkan, DX12
//! and Metal all go through that same allocator arm — and GLES cannot reach this
//! module, having no [`STAGING_RING_FEATURE`]. Recorded because the failure is
//! silent, not because it is close.
//!
//! # Why the ring never blocks
//!
//! A slot cannot be written again until the copy reading it has finished, and
//! waiting for that on the frame thread would give back everything this bought.
//! So it is never waited for. [`VolumeStaging::write_plane`] drives wgpu's
//! callbacks with a **non-blocking** `PollType::Poll`, takes a slot that is
//! mapped, and — if none is — returns `false` and lets the caller take the
//! `write_texture` path it already has. The ring can therefore only ever make a
//! frame faster or leave it exactly as it was.
//!
//! That is also what makes this one code path rather than a native one and a web
//! one. `PollType::Poll` is expressible on every target — it is `PollType::Wait`
//! that a browser cannot honour — and the arm that would need waiting is the
//! arm that answers `false`. WebGL2 has no [`STAGING_RING_FEATURE`] and so never
//! builds a ring at all; were some future browser backend to offer the feature
//! and still resolve `map_async` only from the event loop, no slot would ever
//! report mapped and every upload would take the fallback. Slower than it could
//! be, and identical to today.
//!
//! [`STAGING_RING_DEPTH`] is 2 because that is what the measurement asks for:
//! `MAX_LOOP_VOLUME_BUILDS_PER_FRAME` is 1, and a slot handed back after a
//! 32 MiB copy remaps in **5.34 ms** — a third of a frame. A slot used on one
//! frame is ready on the next, and the second slot covers the frame in between.
//!
//! [`MAX_LOOP_VOLUME_BUILDS_PER_FRAME`]: crate::constants::MAX_LOOP_VOLUME_BUILDS_PER_FRAME

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use egui_wgpu::wgpu;

use super::{GRID_BYTES_PER_CELL, label};

/// The one adapter feature a staging ring needs.
///
/// Requested at device creation by `AppState::new` when the adapter offers it,
/// and re-read off the **device** here, because a device is not obliged to
/// enable everything its adapter can do. Absent it, `wgpu-core` refuses to
/// create the `MAP_READ | COPY_SRC` buffer at all and every upload takes the
/// `write_texture` path — which is what WebGL2 does, having neither the feature
/// nor a BAR window to be slow across.
///
/// Enabling it costs nothing else, and that is a structural claim rather than a
/// survey of this crate's buffers. wgpu logs a warning that it is "a massive
/// performance footgun on a discrete GPU", which is about putting *primary*
/// buffers — vertex, index, uniform, storage — in host memory. But the feature
/// **only relaxes a validation check**: it appears in `wgpu-core`'s
/// `create_buffer` as the guard on the `read_mismatch`/`write_mismatch` arms and
/// nowhere in placement at all. Placement is decided per buffer from that
/// buffer's own `MAP_READ`/`MAP_WRITE` bits (`wgpu-hal/src/vulkan/device.rs`),
/// which the feature does not touch. So no buffer that was legal to create
/// before can change where it lands — the feature can only make *new*
/// combinations legal, and the only new one anything asks for is the ring's.
pub const STAGING_RING_FEATURE: wgpu::Features = wgpu::Features::MAPPABLE_PRIMARY_BUFFERS;

/// How many buffers the ring holds. See the module docs for why it is 2.
pub const STAGING_RING_DEPTH: usize = 2;

/// The host memory an upload borrows: a ring of GPU-readable staging buffers
/// where the device has them, and the plain widening buffer everywhere else.
///
/// One of these for the whole application, held by
/// `volume::bridge::VolumeResources`, for the reason that struct's field note
/// gives: uploads are serialised on the frame thread, so there is never a second
/// one in flight to need a second.
///
/// # What it costs to hold
///
/// **The ring is permanently resident host memory once anything has been
/// uploaded**, at [`STAGING_RING_DEPTH`] × the largest plane this process has
/// seen: **64.00 MiB** on `DESKTOP_VOLUME_GRID_CELLS`, 27.00 MiB on the mobile
/// rung, and **nothing at all** on wasm32, which never has the feature. Like the
/// widening buffer it replaces, it only ever grows and is not given back by
/// `release_pane` or `retain_uploads` — a session that closed its last 3D pane
/// is the one likeliest to open another, and the whole point is that the pages
/// are bought once.
///
/// A machine that never opens a 3D pane allocates neither half, because the ring
/// is built on first use rather than at construction.
///
/// **A ring-capable device holds 64.00 MiB in the steady state and 96.00 MiB in
/// the worst case**, and the difference is not a fault — it is
/// [`Self::write_plane`]'s never-blocks property working as designed. While
/// every upload finds a mapped slot, the plane is widened straight into that
/// slot, [`Self::widening`] is never touched, and it stays a zero-length `Vec`:
/// 64.00 MiB, which is **+32.00 MiB** on what the widening buffer alone used to
/// cost. But the ring is allowed to decline — that is the whole reason it never
/// waits — and the first upload it declines takes the `write_texture` fallback,
/// which allocates the widening buffer at the largest shape seen and, because
/// that `Vec` only ever grows, keeps it for the session. From then on the two
/// coexist: **96.00 MiB on desktop, +64.00 MiB**, and 40.50 MiB on the mobile
/// rung.
///
/// So the number to plan against on desktop is 96.00 MiB, not 64.00. Reaching it
/// takes one frame on which every slot was still feeding a copy — a frame the
/// GPU was behind on, which is exactly the frame the fallback exists for — and
/// it is permanent once reached. `the_worst_case_residency_is_the_ring_and_the_
/// widening_together` pins both figures.
///
/// It is host memory either way, so it is outside the GPU budget
/// `crate::constants::APP_TEXTURE_BUDGET_BYTES` states, exactly as the widening
/// buffer is.
pub struct VolumeStaging {
    /// The host-side buffer the fallback widens into. See
    /// [`super::coverage_premultiplied_into`] for why the caller owns it.
    widening: Vec<u8>,
    /// `None` until the first upload, and forever on a device without
    /// [`STAGING_RING_FEATURE`].
    ring: Option<Ring>,
    /// Whether this device could have a ring at all — read once, at
    /// construction, so the hot path is a `bool` and not a feature-set test.
    capable: bool,
}

/// The ring itself: [`STAGING_RING_DEPTH`] buffers, all the same size.
struct Ring {
    slots: Vec<Slot>,
    /// Where the next claim starts looking, so the slots are used round-robin
    /// and each one gets the longest possible time to remap.
    next: usize,
    /// What each slot is sized to. Grow-only, like the widening buffer.
    bytes: wgpu::BufferAddress,
}

/// One staging buffer and whether the host may write it right now.
struct Slot {
    buffer: wgpu::Buffer,
    /// `true` exactly while the buffer is mapped and idle.
    ///
    /// Set by `map_async`'s callback, which is why it is an `Arc<AtomicBool>`
    /// and not a plain field: the callback outlives the call that installed it
    /// and must be `'static`. Cleared the instant a slot is claimed, so a slot
    /// whose copy is still in flight can never be handed out twice.
    mapped: Arc<AtomicBool>,
}

impl Default for VolumeStaging {
    /// Host memory only — no ring, ever.
    ///
    /// What a caller with no device to ask hands in, and what the fallback path
    /// is exercised through. Production always goes through [`Self::new`].
    fn default() -> Self {
        Self {
            widening: Vec::new(),
            ring: None,
            capable: false,
        }
    }
}

impl VolumeStaging {
    /// Staging for `device`, with a ring if it can have one.
    ///
    /// Allocates nothing: the ring is built on the first upload, sized to that
    /// upload's plane, so a session that never opens a 3D pane pays for neither
    /// half.
    pub fn new(device: &wgpu::Device) -> Self {
        let capable = device.features().contains(STAGING_RING_FEATURE);
        Self {
            widening: Vec::new(),
            ring: None,
            capable,
        }
    }

    /// Whether this device can stage through host memory at all.
    ///
    /// The runtime capability check, exposed so a test can say which arm it is
    /// on rather than inferring it from a timing.
    pub fn has_ring(&self) -> bool {
        self.capable
    }

    /// Host bytes this is holding: the ring's slots plus the widening buffer.
    ///
    /// The figure the residency note above quotes, as something a test can
    /// assert rather than a number in prose.
    pub fn host_bytes(&self) -> usize {
        let ring = self.ring.as_ref().map_or(0, |ring| {
            usize::try_from(ring.bytes).unwrap_or(usize::MAX) * ring.slots.len()
        });
        ring.saturating_add(self.widening.len())
    }

    /// The widening buffer, for the `write_texture` fallback.
    pub(super) fn widening(&mut self) -> &mut Vec<u8> {
        &mut self.widening
    }

    /// Widen `indices` into a staging slot and start the copy into `grid`'s
    /// mip 0, or say `false` and leave the caller to `write_texture`.
    ///
    /// `false` is a completely ordinary answer and not an error: this device may
    /// have no ring, the plane may not be expressible as a buffer copy, or every
    /// slot may still be feeding a copy that has not drained. All three take the
    /// path that was there before, which is why nothing downstream has to know
    /// which route a given upload took.
    ///
    /// # Why the caller may draw immediately afterwards
    ///
    /// The copy is recorded and **submitted here**, before this returns, so the
    /// grid is complete as far as anything ordered after it on the queue is
    /// concerned — and every read of it is: `prepare` runs before egui submits
    /// the encoder the raymarch was recorded into. There is no
    /// partially-uploaded grid for the uploads cache or the store's eviction to
    /// trip over, which is the whole reason for submitting here rather than
    /// borrowing the caller's encoder. Handing the copy to an encoder that had
    /// not been submitted yet would also make the `map_async` below race it:
    /// wgpu would resolve the map against the *previous* submission, hand the
    /// slot back while a recorded-but-unsubmitted copy still wanted it, and
    /// panic at submission time on a mapped buffer.
    pub(super) fn write_plane(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        grid: &wgpu::Texture,
        cells: [u32; 3],
        indices: &[u8],
    ) -> bool {
        if !self.capable {
            return false;
        }
        let Some(layout) = PlaneLayout::of(cells) else {
            return false;
        };
        if indices.len() != layout.cells {
            return false;
        }

        let ring = self
            .ring
            .get_or_insert_with(|| Ring::new(device, layout.bytes));
        ring.grow(device, layout.bytes);
        let Some(slot) = ring.claim(device) else {
            return false;
        };

        widen_into_mapping(&slot.buffer, &layout, indices);
        slot.buffer.unmap();

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some(&label("grid.staging")),
        });
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: &slot.buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    // The padded stride, not the plane's own: unlike
                    // `write_texture`, which repacks internally, a buffer copy
                    // is held to `COPY_BYTES_PER_ROW_ALIGNMENT`. Every shipped
                    // rung is already a multiple of it and pads by zero;
                    // `PlaneLayout` is what makes the ones that are not work
                    // anyway.
                    bytes_per_row: Some(layout.padded_row),
                    rows_per_image: Some(cells[1]),
                },
            },
            grid.as_image_copy(),
            wgpu::Extent3d {
                width: cells[0],
                height: cells[1],
                depth_or_array_layers: cells[2],
            },
        );
        queue.submit(Some(encoder.finish()));

        // Ask for it back. The callback fires on a later poll, once the copy
        // above has actually drained — which is the whole of the "still in
        // flight" bookkeeping, delegated to the one component that knows.
        let mapped = Arc::clone(&slot.mapped);
        slot.buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                if result.is_ok() {
                    mapped.store(true, Ordering::Release);
                }
                // On error the slot simply never comes back and the ring runs one
                // slot shallower — degraded, never wrong. Nothing is logged from a
                // wgpu callback, which may run on a thread with no context to say
                // it in; `VolumeStaging::has_ring` is what a caller asks instead.
            });
        true
    }
}

impl Ring {
    /// [`STAGING_RING_DEPTH`] buffers of `bytes`, each already asked to map.
    ///
    /// The map is requested and **not** waited for. It does not have to be: the
    /// buffers have no GPU work behind them, so the single non-blocking
    /// `PollType::Poll` in [`Self::claim`] resolves every one of them on the
    /// spot — measured, on the first call, from cold.
    fn new(device: &wgpu::Device, bytes: wgpu::BufferAddress) -> Self {
        Self {
            slots: (0..STAGING_RING_DEPTH)
                .map(|index| Slot::new(device, bytes, index))
                .collect(),
            next: 0,
            bytes,
        }
    }

    /// Resize to hold `bytes`, if it does not already.
    ///
    /// Grow-only, for the reason [`super::coverage_premultiplied_into`] gives
    /// about the widening buffer: shrinking would give the pages back and buy
    /// them again on the next larger grid. Replacing the slots outright rather
    /// than resizing them is the only option wgpu offers, and it is safe with a
    /// copy in flight — a dropped `wgpu::Buffer` is kept alive until the
    /// submission using it retires.
    fn grow(&mut self, device: &wgpu::Device, bytes: wgpu::BufferAddress) {
        if self.bytes >= bytes {
            return;
        }
        *self = Self::new(device, bytes);
    }

    /// A slot the host may write, or `None` when every one is still in flight.
    ///
    /// The poll is `PollType::Poll` — a single non-blocking sweep of wgpu's
    /// callbacks. `PollType::Wait` here would be a stall on the frame thread of
    /// exactly the kind this module exists to remove, and on wasm it is not even
    /// expressible; the `None` arm is what stands in for it, and it costs a
    /// slower upload rather than a wrong one.
    fn claim(&mut self, device: &wgpu::Device) -> Option<&Slot> {
        let _ = device.poll(wgpu::PollType::Poll);
        let ready: Vec<bool> = self
            .slots
            .iter()
            .map(|slot| slot.mapped.load(Ordering::Acquire))
            .collect();
        let index = pick(&ready, self.next)?;
        self.next = (index + 1) % self.slots.len();
        // Before the caller writes a byte, so that a second claim in the same
        // frame cannot be handed the same slot.
        self.slots[index].mapped.store(false, Ordering::Release);
        Some(&self.slots[index])
    }
}

impl Slot {
    fn new(device: &wgpu::Device, bytes: wgpu::BufferAddress, index: usize) -> Self {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&label(&format!("grid.staging.{index}"))),
            size: bytes,
            // The pair, and only this pair. `MAP_READ` is what puts it in system
            // RAM rather than the BAR window; `COPY_SRC` is what lets the copy
            // engine read it. Adding `MAP_WRITE` — the usage this is nominally
            // for — would put it straight back in the BAR and undo the change.
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_SRC,
            // **Not** `true`, and this is load-bearing: `mapped_at_creation` on
            // a buffer without `MAP_WRITE` takes `wgpu-core`'s `BufferMapState::
            // Init` arm, which allocates a separate `MAP_WRITE` staging buffer —
            // in the BAR — and copies through it on unmap. The `map_async`
            // below is what actually maps this buffer's own memory.
            mapped_at_creation: false,
        });
        let mapped = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&mapped);
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                if result.is_ok() {
                    flag.store(true, Ordering::Release);
                }
            });
        Self { buffer, mapped }
    }
}

/// The first slot at or after `next`, wrapping, that is mapped.
///
/// Split out from [`Ring::claim`] because it is the one rule in this module that
/// can be wrong without a GPU to notice: hand back a slot whose copy has not
/// drained and the next `get_mapped_range_mut` panics on the frame thread. As a
/// function over a list of `bool` it is pinned by an ordinary unit test on every
/// target, including the ones that never build a ring.
fn pick(ready: &[bool], next: usize) -> Option<usize> {
    if ready.is_empty() {
        return None;
    }
    (0..ready.len())
        .map(|offset| (next + offset) % ready.len())
        .find(|&index| ready[index])
}

/// How a grid's plane sits inside a buffer a copy can read.
///
/// The one thing this expresses that `write_texture` did not have to:
/// `copy_buffer_to_texture` requires each row to start on a
/// [`wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`] boundary, where `write_texture`
/// repacks internally to whatever the backend wants. Every shipped rung is
/// already aligned — `DESKTOP_VOLUME_GRID_CELLS` is 256 cells wide, so 1024
/// bytes; mobile 768; wasm32 512 — and pads by nothing at all. The padding
/// exists for the odd extents `upload_volume_at` accepts from a test, so that
/// they take the same path rather than a second one nobody runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlaneLayout {
    /// Bytes of real texels in a row: `cells[0] * GRID_BYTES_PER_CELL`.
    row: u32,
    /// That, rounded up to the copy alignment.
    padded_row: u32,
    /// Rows in the whole plane — `cells[1] * cells[2]`, not `cells[1]`.
    rows: usize,
    /// Cells in the whole plane, which is the index count this expects.
    cells: usize,
    /// Bytes a buffer must be to hold it.
    bytes: wgpu::BufferAddress,
}

impl PlaneLayout {
    /// `None` for a shape whose buffer would not fit in the address space, which
    /// is a shape `upload_refusal` will have turned away in any case.
    fn of(cells: [u32; 3]) -> Option<Self> {
        let row = cells[0].checked_mul(GRID_BYTES_PER_CELL)?;
        let padded_row = row
            .checked_next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            .filter(|_| row > 0)?;
        let rows = (cells[1] as usize).checked_mul(cells[2] as usize)?;
        let count = (cells[0] as usize).checked_mul(rows)?;
        let bytes = (padded_row as u64).checked_mul(rows as u64)?;
        Some(Self {
            row,
            padded_row,
            rows,
            cells: count,
            bytes,
        })
    }
}

/// Widen `indices` into `buffer`'s mapping, one padded row at a time.
///
/// The pass [`super::coverage_premultiplied_into`] does, writing to the mapped
/// range instead of to a `Vec` — so the ring path has **no intermediate copy at
/// all**, where the `write_texture` path has two (host buffer, then wgpu's own
/// staging). Same table, same 256 four-byte answers, so the bytes are the same
/// bytes; `the_two_routes_write_the_same_plane` is what says
/// so against a real readback.
///
/// `WriteOnly` rather than a `&mut [u8]` because that is all wgpu 29 will hand
/// out for a mapped buffer: mapped memory may be write-combining, where a read
/// costs a full cache-line fill and Rust's ordinary reference semantics would
/// invite one. Nothing here reads what it wrote, so the restriction is free.
fn widen_into_mapping(buffer: &wgpu::Buffer, layout: &PlaneLayout, indices: &[u8]) {
    const STRIDE: usize = GRID_BYTES_PER_CELL as usize;

    let texels = super::coverage_texels();
    let row = layout.row as usize;
    let width = row / STRIDE;

    let mut view = buffer.get_mapped_range_mut(..layout.bytes);
    let mut rest = view.slice(..);
    for index in 0..layout.rows {
        let (this, next) = rest.split_at(layout.padded_row as usize);
        // The padding past `row` is left exactly as it was. A buffer copy reads
        // none of it, and wgpu zero-initialised the whole allocation once at
        // creation, so there is no uninitialised memory here for the tail to be.
        let (out, _padding) = this.into_slice(..row).into_chunks::<STRIDE>();
        let source = &indices[index * width..(index + 1) * width];
        out.write_iter(source.iter().map(|&byte| texels[byte as usize]));
        rest = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ring never hands out a slot whose copy is still in flight.
    ///
    /// The one invariant here that a passing render cannot vouch for. Breaking
    /// it does not corrupt a texture — it panics inside `get_mapped_range_mut`,
    /// on the frame thread, in `prepare`, and only on the frames where the GPU
    /// happened to be behind. So it is pinned as arithmetic instead, over every
    /// readiness pattern a ring of the shipped depth can be in, from every
    /// starting position.
    #[test]
    fn a_slot_whose_copy_is_still_in_flight_is_never_handed_out() {
        for pattern in 0..(1usize << STAGING_RING_DEPTH) {
            let ready: Vec<bool> = (0..STAGING_RING_DEPTH)
                .map(|slot| pattern & (1 << slot) != 0)
                .collect();
            for next in 0..STAGING_RING_DEPTH {
                match pick(&ready, next) {
                    Some(index) => assert!(
                        ready[index],
                        "a ring at {ready:?} starting from {next} handed out slot \
                         {index}, which is still feeding a copy — the write into \
                         it will panic on the frame thread",
                    ),
                    None => assert!(
                        !ready.iter().any(|&r| r),
                        "a ring at {ready:?} starting from {next} refused a slot \
                         it had, so the upload falls back to `write_texture` and \
                         the frame pays ~15.5 ms it did not have to",
                    ),
                }
            }
        }
    }

    /// Round-robin, so a slot gets the whole ring's worth of frames to remap.
    ///
    /// Taking the *lowest* ready slot every time would pass the test above and
    /// still starve the ring: with depth 2 it would use slot 0, then slot 0
    /// again the moment it came back, and slot 1 would never be reached — which
    /// is a ring of depth 1 wearing a 64 MiB coat.
    #[test]
    fn the_ring_walks_forward_rather_than_always_taking_the_first_slot() {
        let all_ready = vec![true; STAGING_RING_DEPTH];
        let mut next = 0;
        let mut seen = Vec::new();
        for _ in 0..STAGING_RING_DEPTH {
            let index = pick(&all_ready, next).expect("every slot is ready");
            seen.push(index);
            next = (index + 1) % STAGING_RING_DEPTH;
        }
        let mut sorted = seen.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            STAGING_RING_DEPTH,
            "{STAGING_RING_DEPTH} consecutive claims took {seen:?} — a slot was \
             reused before every other one had a turn, so the ring is shallower \
             than the memory it is holding",
        );
    }

    /// An empty ring refuses rather than dividing by its own length.
    #[test]
    fn a_ring_with_no_slots_refuses_instead_of_panicking() {
        assert_eq!(pick(&[], 0), None);
    }

    /// Every shipped rung's rows are already aligned, so the ring pads nothing.
    ///
    /// Not decoration: if a shipped shape needed padding, the staging buffer
    /// would be larger than the plane and the residency figure this module
    /// quotes would be wrong. The odd shapes beside them are the ones the
    /// padding exists for, and they are checked to actually exercise it.
    #[test]
    fn the_shipped_grid_shapes_need_no_row_padding_and_the_odd_ones_do() {
        for cells in [
            crate::constants::WASM_VOLUME_GRID_CELLS,
            crate::constants::MOBILE_VOLUME_GRID_CELLS,
            crate::constants::DESKTOP_VOLUME_GRID_CELLS,
        ] {
            let layout = PlaneLayout::of(cells).expect("a shipped rung is expressible");
            assert_eq!(
                layout.row,
                layout.padded_row,
                "the {cells:?} rung's {}-byte row is no longer a multiple of \
                 {}, so its staging buffer is now bigger than its plane",
                layout.row,
                wgpu::COPY_BYTES_PER_ROW_ALIGNMENT,
            );
            assert_eq!(
                layout.bytes,
                super::super::grid_bytes_at(cells, super::super::CoarseLevel::Omitted)
                    .expect("a shipped rung fits") as u64,
                "the {cells:?} rung's staging buffer is not its mip-0 plane",
            );
        }

        for cells in [[7u32, 5, 3], [65, 3, 2], [1, 1, 1]] {
            let layout = PlaneLayout::of(cells).expect("an odd shape is expressible");
            assert!(
                layout.padded_row > layout.row,
                "{cells:?} was picked to exercise the row padding and does not",
            );
            assert_eq!(layout.padded_row % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 0);
        }
    }

    /// A shape whose buffer cannot be addressed is refused, not wrapped.
    #[test]
    fn a_plane_too_large_to_address_has_no_layout() {
        assert_eq!(PlaneLayout::of([u32::MAX, 2, 2]), None);
        assert_eq!(PlaneLayout::of([0, 4, 4]), None);
    }

    /// Host-only staging holds nothing and never claims a ring.
    ///
    /// The fallback's own precondition: a `VolumeStaging` built without a device
    /// must report no ring, so an upload through it takes the `write_texture`
    /// path. `the_two_routes_write_the_same_plane` on a real device is the
    /// other half.
    #[test]
    fn staging_with_no_device_has_no_ring_and_holds_nothing() {
        let staging = VolumeStaging::default();
        assert!(!staging.has_ring());
        assert_eq!(staging.host_bytes(), 0);
    }

    /// The ring puts **the same bytes in the same texels** as `write_texture`.
    ///
    /// The whole correctness case for this module, and the one claim no amount
    /// of host reasoning settles: the two routes disagree about row stride
    /// (`write_texture` repacks internally; a buffer copy is held to
    /// `COPY_BYTES_PER_ROW_ALIGNMENT`), about how many copies the bytes make on
    /// the way, and about which memory they pass through. So both are run
    /// against a real device, on textures this test owns, and the texels are
    /// read back and compared.
    ///
    /// The shapes are chosen to make the stride disagreement bite:
    /// `[128, 128, 64]` is the wasm32 rung and pads by nothing, `[7, 5, 3]` pads
    /// 28 bytes out to 256, `[65, 3, 2]` pads 260 out to 512 — which is the case
    /// that catches a `next_multiple_of` written as a `div_ceil` on the wrong
    /// operand — and `[1, 1, 1]` is the smallest extent the upload accepts. A
    /// stride mistake shears the volume by a row per slice, which looks like a
    /// camera bug rather than an upload one.
    ///
    /// # Why the order interleaves grows and shrinks
    ///
    /// [`Ring::grow`] is production-reachable — a session that moves from a
    /// narrow region box to a wide one uploads a larger plane than the ring was
    /// built for — and a walk that started at its widest shape would never run
    /// its body at all, because the ring only ever grows. So the order climbs
    /// and falls: it starts at `[1, 1, 1]`, **grows** to the wasm32 rung, shrinks
    /// twice onto that 4.00 MiB tail, **grows again** to the mobile rung, and
    /// falls back. The two grows are asserted to have actually happened, against
    /// [`VolumeStaging::host_bytes`], so a version that silently stopped
    /// resizing fails here rather than panicking inside
    /// `get_mapped_range_mut` on the frame thread.
    ///
    /// The shrinks matter for a different reason and are not filler: a smaller
    /// plane leaves the previous, larger one in the slot's tail, and the copy
    /// must read the prefix rather than the buffer.
    ///
    /// The plane content is deliberately not uniform: every one of the 256 index
    /// values appears, offset per shape, so a copy that landed at the wrong
    /// offset produces different bytes rather than the same byte.
    ///
    /// On an adapter without [`STAGING_RING_FEATURE`] there is no ring to
    /// compare, and the assertion becomes that `write_plane` **said so** rather
    /// than quietly writing nothing — which is the fallback's contract and the
    /// only thing that can be checked there.
    ///
    /// ```text
    /// cargo test -p rustdar-frontend --lib \
    ///     volume::raymarch::staging::tests::the_two_routes_write_the_same_plane \
    ///     -- --ignored --exact --nocapture
    /// ```
    #[test]
    #[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
    #[cfg(not(target_arch = "wasm32"))]
    fn the_two_routes_write_the_same_plane() {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .expect("no wgpu adapter; this test is ignored by default for that reason");
        eprintln!("wgpu adapter: {:?}", adapter.get_info());

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("rustdar.volume.staging.test"),
            required_features: adapter.features() & STAGING_RING_FEATURE,
            required_limits: adapter.limits(),
            memory_hints: Default::default(),
            experimental_features: Default::default(),
            trace: Default::default(),
        }))
        .expect("could not create a device on an adapter that was found");

        let mut staging = VolumeStaging::new(&device);
        eprintln!("staging ring available: {}", staging.has_ring());

        // Two turns of each shape, so the ring wraps and every slot is both
        // written and handed back at least once. A ring that never came back
        // would pass a single-shot test and starve on the second turn.
        //
        // `grows` counts the turn-0 resizes; turn 1 replays the same walk with
        // the ring already at its high-water mark, which is the early-return
        // half of `Ring::grow` and must add none.
        let mut high_water = 0;
        let mut grows = 0;
        for turn in 0..2 {
            for cells in [
                [1u32, 1, 1],   // build the ring at its smallest
                [128, 128, 64], // grow — the wasm32 rung, 4.00 MiB a slot
                [7, 5, 3],      // shrink onto that tail
                [65, 3, 2],     // shrink again, at the awkward stride
                [192, 192, 96], // grow — the mobile rung, 13.50 MiB a slot
                [128, 128, 64], // and fall back, which must not shrink it
            ] {
                let count = (cells[0] as usize) * (cells[1] as usize) * (cells[2] as usize);
                let indices: Vec<u8> = (0..count)
                    .map(|i| (i.wrapping_add(cells[2] as usize + turn)) as u8)
                    .collect();

                let through_write_texture = texture(&device, cells);
                let plane = super::super::coverage_premultiplied_into(
                    VolumeStaging::default().widening(),
                    &indices,
                )
                .to_vec();
                queue.write_texture(
                    through_write_texture.as_image_copy(),
                    &plane,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(cells[0] * GRID_BYTES_PER_CELL),
                        rows_per_image: Some(cells[1]),
                    },
                    extent(cells),
                );

                let through_ring = texture(&device, cells);
                let took_the_ring =
                    staging.write_plane(&device, &queue, &through_ring, cells, &indices);
                assert_eq!(
                    took_the_ring,
                    staging.has_ring(),
                    "turn {turn}, {cells:?}: the ring's answer disagrees with \
                     whether this device has one — either a capable device \
                     starved its own ring on a walk that uploads one plane at a \
                     time, or an incapable one claimed to have written a plane \
                     it cannot",
                );
                if !took_the_ring {
                    continue;
                }

                // The ring only ever grows, and it grew exactly when this shape
                // was the widest yet.
                let held = staging.host_bytes();
                if held > high_water {
                    grows += 1;
                    assert_eq!(
                        turn, 0,
                        "turn {turn}, {cells:?}: the ring resized on a replay of \
                         a walk it has already been through, so it is not \
                         grow-only after all and the pages this module says are \
                         bought once are being bought again",
                    );
                    high_water = held;
                }
                assert_eq!(
                    held, high_water,
                    "turn {turn}, {cells:?}: the ring shrank to fit a smaller \
                     plane, so the next larger grid pays for its pages again",
                );

                let expected = read_back(&device, &queue, &through_write_texture, cells);
                let got = read_back(&device, &queue, &through_ring, cells);
                assert_eq!(
                    expected.len(),
                    count * GRID_BYTES_PER_CELL as usize,
                    "turn {turn}, {cells:?}: the readback is not the plane's own \
                     length, so the comparison below is over the wrong bytes",
                );
                assert!(
                    expected.iter().any(|&b| b != 0),
                    "turn {turn}, {cells:?}: the reference plane is all zeroes, \
                     so an upload that wrote nothing at all would pass",
                );
                assert_eq!(
                    got, expected,
                    "turn {turn}, {cells:?}: the staging ring wrote different \
                     texels from `write_texture` — the grid the raymarch samples \
                     now depends on which route its plane took",
                );
            }
        }

        if staging.has_ring() {
            assert_eq!(
                grows, 3,
                "the walk above resized the ring {grows} times, not the three \
                 the shape order was built to force (build, then the wasm32 \
                 rung, then the mobile one) — so `Ring::grow`'s body is going \
                 unchecked and a session that widens its region box is relying \
                 on code no test runs",
            );
            assert_eq!(
                staging.host_bytes(),
                usize::try_from(
                    PlaneLayout::of([192, 192, 96])
                        .expect("the mobile rung")
                        .bytes
                )
                .expect("the mobile rung fits")
                    * STAGING_RING_DEPTH,
                "the ring did not settle at the widest shape it was given",
            );
        }
    }

    /// What a ring-capable device holds once it has also taken the fallback.
    ///
    /// The residency figure `VolumeStaging` quotes has two values, and the
    /// smaller one is the easy claim to leave standing by accident. While every
    /// upload finds a free slot the widening `Vec` is never touched and the cost
    /// is the ring alone. But declining an upload is a thing the ring is
    /// *designed* to do — see [`Ring::claim`] — and the fallback that catches it
    /// allocates the widening buffer for good.
    ///
    /// So both figures are pinned here, on the desktop rung, in the units the
    /// docs quote. The fallback is driven through the very call
    /// `upload_volume_at` makes when `write_plane` says no, rather than by
    /// trying to starve the ring on a timer — which would be a race, and a
    /// flaky test is worse than a prose claim. That the starve is *reachable* is
    /// `a_slot_whose_copy_is_still_in_flight_is_never_handed_out`'s `None` arm;
    /// what it costs when it happens is this.
    ///
    /// ```text
    /// cargo test -p rustdar-frontend --lib \
    ///     volume::raymarch::staging::tests::the_worst_case_residency_is_the_ring_and_the_widening_together \
    ///     -- --ignored --exact --nocapture
    /// ```
    #[test]
    #[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
    #[cfg(not(target_arch = "wasm32"))]
    fn the_worst_case_residency_is_the_ring_and_the_widening_together() {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .expect("no wgpu adapter; this test is ignored by default for that reason");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("rustdar.volume.staging.residency"),
            required_features: adapter.features() & STAGING_RING_FEATURE,
            required_limits: adapter.limits(),
            memory_hints: Default::default(),
            experimental_features: Default::default(),
            trace: Default::default(),
        }))
        .expect("could not create a device on an adapter that was found");

        let mut staging = VolumeStaging::new(&device);
        if !staging.has_ring() {
            eprintln!(
                "no staging ring on this adapter; the fallback holds one plane and that is all"
            );
            return;
        }

        let cells = crate::constants::DESKTOP_VOLUME_GRID_CELLS;
        let plane = PlaneLayout::of(cells).expect("the desktop rung").bytes as usize;
        let count = (cells[0] as usize) * (cells[1] as usize) * (cells[2] as usize);
        let indices = vec![7u8; count];

        assert_eq!(staging.host_bytes(), 0, "a fresh staging holds nothing");

        let grid = texture(&device, cells);
        assert!(staging.write_plane(&device, &queue, &grid, cells, &indices));
        let steady = staging.host_bytes();
        assert_eq!(
            steady,
            plane * STAGING_RING_DEPTH,
            "the steady state on the desktop rung is not the {STAGING_RING_DEPTH} \
             × 32.00 MiB the docs quote",
        );
        assert_eq!(steady, 64 << 20, "…and that is 64.00 MiB");

        // Now the fallback, exactly as `upload_volume_at` takes it.
        super::super::coverage_premultiplied_into(staging.widening(), &indices);
        let worst = staging.host_bytes();
        assert_eq!(
            worst,
            steady + plane,
            "one starved upload did not add a whole widening buffer, so the \
             worst case the docs quote is not what the code reaches",
        );
        assert_eq!(
            worst,
            96 << 20,
            "the worst-case desktop residency is not the 96.00 MiB \
             `VolumeStaging` and `MAX_LOOP_VOLUME_BUILDS_PER_FRAME` both state",
        );

        // And it is permanent: a later ring upload does not give it back.
        assert!(staging.write_plane(&device, &queue, &grid, cells, &indices));
        assert_eq!(
            staging.host_bytes(),
            worst,
            "the widening buffer was released once the ring recovered — which \
             would be a smaller worst case than the docs promise, but the `Vec` \
             only grows, so this failing means the accounting is wrong",
        );
    }

    /// A 3D grid texture a test can read back out of.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn texture(device: &wgpu::Device, cells: [u32; 3]) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rustdar.volume.staging.test.grid"),
            size: extent(cells),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: crate::volume::VOLUME_TEXTURE_FORMAT,
            // `COPY_SRC` beside the production pair, and only here: the shipped
            // grid does not carry it, so this is a texture of the test's own
            // rather than one borrowed from `upload_volume_at`. What is being
            // compared is the two write paths, which take the same arguments
            // wherever they are called from.
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn extent(cells: [u32; 3]) -> wgpu::Extent3d {
        wgpu::Extent3d {
            width: cells[0],
            height: cells[1],
            depth_or_array_layers: cells[2],
        }
    }

    /// The texels of a 3D grid, row padding stripped, in the plane's own order.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn read_back(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        cells: [u32; 3],
    ) -> Vec<u8> {
        let layout = PlaneLayout::of(cells).expect("a test shape is expressible");
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rustdar.volume.staging.test.readback"),
            size: layout.bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(layout.padded_row),
                    rows_per_image: Some(cells[1]),
                },
            },
            extent(cells),
        );
        queue.submit(Some(encoder.finish()));
        readback.slice(..).map_async(wgpu::MapMode::Read, |result| {
            result.expect("mapping the readback buffer failed");
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("polling the device failed");

        let mapped = readback.slice(..).get_mapped_range();
        let row = layout.row as usize;
        let mut plane = Vec::with_capacity(row * layout.rows);
        for index in 0..layout.rows {
            let at = index * layout.padded_row as usize;
            plane.extend_from_slice(&mapped[at..at + row]);
        }
        plane
    }
}
