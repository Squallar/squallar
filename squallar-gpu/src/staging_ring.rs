//! Host memory the GPU's copy engine can read, so that crossing PCIe is not
//! something the frame thread stands and waits for.
//!
//! `write_texture` stages through a `MAP_WRITE` buffer, which `wgpu-hal`'s
//! Vulkan backend places in the card's host-visible BAR window: every host store
//! is a write down the link and the call blocks until the last lands (measured
//! on this box: 32 MiB in 15.6 ms, 2.15 GB/s). `MAP_READ | COPY_SRC` is the one
//! usage pair `wgpu-hal` maps to `MemoryLocation::GpuToCpu` — ordinary cached
//! system RAM — where the same memcpy costs 1.7 ms at 20 GB/s and the copy
//! engine pays the ~5.3 ms link cost asynchronously.
//!
//! Writing through a *read* mapping: `wgpu-core` normally rejects
//! `MAP_READ | COPY_SRC` (`device/resource.rs`, `read_mismatch`), and
//! [`STAGING_RING_FEATURE`] skips that check; past it `get_mapped_range_mut`
//! hands back a writable pointer. Checked under the Khronos validation layer:
//! no message of any severity.
//!
//! Unstated premise: the mapping is coherent. `is_coherent` is a `wgpu-hal`
//! field no public API exposes, but `gpu-allocator` 0.28 satisfies `GpuToCpu`
//! from two arms that are both `HOST_COHERENT`. If it ever stopped holding,
//! `unmap`'s flush would be skipped and wgpu would leave the zero-init tracker
//! armed and **zero the staging buffer between the write and the copy** —
//! silently blank content, no crash and no validation message.
//!
//! The ring never blocks: [`Ring::claim`] polls with a non-blocking
//! `PollType::Poll` and answers `None` when no slot is mapped, leaving the
//! caller its fallback. `PollType::Wait` is not expressible on wasm.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use egui_wgpu::wgpu;

/// The one adapter feature a staging ring needs.
///
/// The feature only relaxes a validation check — it is the guard on
/// `create_buffer`'s `read_mismatch`/`write_mismatch` arms and appears nowhere
/// in placement, which is decided per buffer from its own `MAP_READ`/`MAP_WRITE`
/// bits. So wgpu's "footgun on a discrete GPU" warning does not apply: no buffer
/// that was legal before can change where it lands.
pub const STAGING_RING_FEATURE: wgpu::Features = wgpu::Features::MAPPABLE_PRIMARY_BUFFERS;

/// How many buffers a ring holds.
///
/// 2: a slot handed back after a 32 MiB copy remaps in 5.34 ms, and
/// `MAX_LOOP_VOLUME_BUILDS_PER_FRAME` is 1, so the second slot covers the frame
/// in between.
pub const STAGING_RING_DEPTH: usize = 2;

/// Whether `device` can have a ring at all.
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
    mapped: Arc<AtomicBool>,
}

impl Ring {
    /// [`STAGING_RING_DEPTH`] buffers of `bytes`, each already asked to map.
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
    pub fn grow(&mut self, device: &wgpu::Device, bytes: wgpu::BufferAddress) {
        if self.bytes >= bytes {
            return;
        }
        *self = Self::new(device, bytes, &self.label.clone());
    }

    /// A slot the host may write, or `None` when every one is still in flight.
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
            // a buffer without `MAP_WRITE` takes `wgpu-core`'s
            // `BufferMapState::Init` arm, which allocates a separate
            // `MAP_WRITE` staging buffer — in the BAR — and copies through it.
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
        assert_eq!(pick(&[], 7), None);
    }
}
