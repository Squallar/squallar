//! Host memory the GPU's copy engine can read, so that crossing PCIe is not
//! something the frame thread stands and waits for.
//!
//! `volume::raymarch::staging::VolumeStaging` is where this was discovered and
//! for one release it was also where it lived. It is here instead because the
//! argument below for *why writing through a read mapping is sound* has nothing
//! to do with voxel grids, and is the kind of reasoning that rots the moment
//! there are two copies of it.
//!
//! # Why `write_texture` is slow, which is not the reason it looks like
//!
//! It is not the bytes. This project's RTX 3090 sits on a **PCIe 4.0 x4**
//! chipset link — two cascaded switches, `LnkSta: Width x4`, ~7.88 GB/s raw — so
//! 32 MiB cannot cross it faster than about 5 ms by any route at all, and
//! nothing here claims otherwise.
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
//! paying it, asynchronously, and not the frame.
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
//! or a validation message — it is silently blank content, which reads as "the
//! radar returned nothing" rather than as a bug here.
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
//! So it is never waited for. [`Ring::claim`] drives wgpu's callbacks with a
//! **non-blocking** `PollType::Poll`, hands back a slot that is mapped, and — if
//! none is — answers `None` and leaves the caller to say what a declined upload
//! means for it. The ring can therefore only ever make a frame faster or leave
//! it exactly as it was.
//!
//! That is also what makes this one code path rather than a native one and a web
//! one. `PollType::Poll` is expressible on every target — it is `PollType::Wait`
//! that a browser cannot honour — and the arm that would need waiting is the arm
//! that answers `None`. WebGL2 has no [`STAGING_RING_FEATURE`] and so never
//! builds a ring at all; were some future browser backend to offer the feature
//! and still resolve `map_async` only from the event loop, no slot would ever
//! report mapped and every upload would take its caller's fallback. Slower than
//! it could be, and identical to today.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use egui_wgpu::wgpu;

/// The one adapter feature a staging ring needs.
///
/// Requested at device creation by `AppState::new` when the adapter offers it,
/// and re-read off the **device** by each caller, because a device is not
/// obliged to enable everything its adapter can do. Absent it, `wgpu-core`
/// refuses to create the `MAP_READ | COPY_SRC` buffer at all and every upload
/// takes the `write_texture` path — which is what WebGL2 does, having neither
/// the feature nor a BAR window to be slow across.
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
/// combinations legal, and the only new ones anything asks for are the rings'.
pub const STAGING_RING_FEATURE: wgpu::Features = wgpu::Features::MAPPABLE_PRIMARY_BUFFERS;

/// How many buffers a ring holds.
///
/// 2, because that is what the measurement asks for. A slot handed back after a
/// 32 MiB copy remaps in **5.34 ms** — a third of a frame — and
/// [`MAX_LOOP_VOLUME_BUILDS_PER_FRAME`] is 1, so a slot used on one frame is
/// ready on the next and the second slot covers the frame in between.
///
/// [`MAX_LOOP_VOLUME_BUILDS_PER_FRAME`]: crate::constants::MAX_LOOP_VOLUME_BUILDS_PER_FRAME
pub const STAGING_RING_DEPTH: usize = 2;

/// Whether `device` can have a ring at all.
///
/// The runtime capability check every caller starts from, and the reason the
/// two upload paths need no `cfg` between native and web: a WebGL2 device simply
/// answers `false` here and takes the fallback the native path also has.
pub fn device_has_ring(device: &wgpu::Device) -> bool {
    device.features().contains(STAGING_RING_FEATURE)
}

/// [`STAGING_RING_DEPTH`] equally sized staging buffers, used round-robin.
pub struct Ring {
    slots: Vec<Slot>,
    /// Where the next claim starts looking, so the slots are used round-robin
    /// and each one gets the longest possible time to remap.
    next: usize,
    /// What each slot is sized to. Grow-only.
    bytes: wgpu::BufferAddress,
    /// Debug label prefix, so a capture says which caller a buffer belongs to.
    label: String,
}

/// One staging buffer and whether the host may write it right now.
pub struct Slot {
    buffer: wgpu::Buffer,
    /// `true` exactly while the buffer is mapped and idle.
    ///
    /// Set by `map_async`'s callback, which is why it is an `Arc<AtomicBool>`
    /// and not a plain field: the callback outlives the call that installed it
    /// and must be `'static`. Cleared the instant a slot is claimed, so a slot
    /// whose copy is still in flight can never be handed out twice.
    mapped: Arc<AtomicBool>,
}

impl Ring {
    /// [`STAGING_RING_DEPTH`] buffers of `bytes`, each already asked to map.
    ///
    /// The map is requested and **not** waited for. It does not have to be: the
    /// buffers have no GPU work behind them, so the single non-blocking
    /// `PollType::Poll` in [`Self::claim`] resolves every one of them on the
    /// spot — measured, on the first call, from cold.
    pub fn new(device: &wgpu::Device, bytes: wgpu::BufferAddress, label: &str) -> Self {
        Self {
            slots: (0..STAGING_RING_DEPTH)
                .map(|index| Slot::new(device, bytes, label, index))
                .collect(),
            next: 0,
            bytes,
            label: label.to_owned(),
        }
    }

    /// Resize to hold `bytes`, if it does not already.
    ///
    /// Grow-only, because shrinking would give the pages back and buy them again
    /// on the next larger upload. Replacing the slots outright rather than
    /// resizing them is the only option wgpu offers, and it is safe with a copy
    /// in flight — a dropped `wgpu::Buffer` is kept alive until the submission
    /// using it retires.
    pub fn grow(&mut self, device: &wgpu::Device, bytes: wgpu::BufferAddress) {
        if self.bytes >= bytes {
            return;
        }
        *self = Self::new(device, bytes, &self.label.clone());
    }

    /// A slot the host may write, or `None` when every one is still in flight.
    ///
    /// The poll is `PollType::Poll` — a single non-blocking sweep of wgpu's
    /// callbacks. `PollType::Wait` here would be a stall on the frame thread of
    /// exactly the kind this module exists to remove, and on wasm it is not even
    /// expressible; the `None` arm is what stands in for it, and it costs a
    /// slower upload rather than a wrong one.
    pub fn claim(&mut self, device: &wgpu::Device) -> Option<&Slot> {
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

    /// Host bytes this ring is holding: every slot, whether idle or in flight.
    pub fn host_bytes(&self) -> usize {
        usize::try_from(self.bytes).unwrap_or(usize::MAX) * self.slots.len()
    }
}

impl Slot {
    fn new(device: &wgpu::Device, bytes: wgpu::BufferAddress, label: &str, index: usize) -> Self {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("{label}.{index}")),
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
        let slot = Self {
            buffer,
            mapped: Arc::new(AtomicBool::new(false)),
        };
        slot.remap();
        slot
    }

    /// The buffer to write through and to copy from.
    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    /// Ask for the mapping back, once the copy reading this slot has drained.
    ///
    /// **Call this only after the copy has been submitted.** Handing the copy to
    /// an encoder that had not been submitted yet would make this race it: wgpu
    /// would resolve the map against the *previous* submission, hand the slot
    /// back while a recorded-but-unsubmitted copy still wanted it, and panic at
    /// submission time on a mapped buffer.
    ///
    /// The callback fires on a later poll, once the copy has actually drained —
    /// which is the whole of the "still in flight" bookkeeping, delegated to the
    /// one component that knows.
    pub fn remap(&self) {
        let mapped = Arc::clone(&self.mapped);
        self.buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                if result.is_ok() {
                    mapped.store(true, Ordering::Release);
                }
                // On error the slot simply never comes back and the ring runs one
                // slot shallower — degraded, never wrong. Nothing is logged from a
                // wgpu callback, which may run on a thread with no context to say
                // it in; the caller's own capability accessor is what to ask
                // instead.
            });
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
                         the frame pays a stall it did not have to",
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
    /// is a ring of depth 1 wearing a two-slot coat.
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
    ///
    /// Not a shape anything builds — [`Ring::new`] always makes
    /// [`STAGING_RING_DEPTH`] slots — but [`pick`] is arithmetic over a slice it
    /// is handed, and `next % 0` is a panic rather than a wrong answer.
    #[test]
    fn a_ring_with_no_slots_refuses_instead_of_panicking() {
        assert_eq!(pick(&[], 0), None);
        assert_eq!(pick(&[], 7), None);
    }
}
