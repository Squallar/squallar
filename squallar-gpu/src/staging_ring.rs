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

/// **How much larger than recent demand a ring has to be before it gives the
/// difference back**, and **for how many consecutive stagings**.
///
/// The pair is [`squallar_device_profile`]'s `LOOP_POOL_HYSTERESIS` /
/// `LOOP_POOL_DWELL_FRAMES` shape, at values this path's own cost sets. **One
/// resize costs a frame**: it rebuilds every slot, and a rebuilt slot is not
/// mapped when the next `claim` asks (measured on this box, the BAR route,
/// 7.66–7.80 ms for a 13.75 MB frame). So the dead band is wide — nothing is
/// given back until the ring is at least **twice** the largest demand seen —
/// and the dwell is long: 240 consecutive stagings inside that band, which is
/// four seconds at 60 Hz and about one at the 250 Hz this application aims at.
///
/// The ratio is an integer multiply rather than a float, so the comparison
/// cannot drift with rounding, and at the geometry path's 4 MiB granularity
/// "twice" is always at least one whole granule.
pub const STAGING_RING_SHRINK_HYSTERESIS: u64 = 2;

/// See [`STAGING_RING_SHRINK_HYSTERESIS`].
pub const STAGING_RING_SHRINK_DWELL: u32 = 240;

/// **Whether a ring should be rebuilt, and at what size.**
///
/// Split out of [`Ring`] with no `wgpu` in it so the policy can be exercised on
/// a host with no adapter — the decision is the part that can be wrong, and a
/// device-backed suite would only ever run where one is present.
#[derive(Debug, Default)]
struct ShrinkWatch {
    /// Consecutive stagings whose demand stayed inside the dead band.
    dwell: u32,
    /// The largest demand seen across that run — what a shrink resizes to.
    peak: wgpu::BufferAddress,
}

impl ShrinkWatch {
    /// Fold one staging of `demand` bytes against a ring currently holding
    /// `held`, and answer the size the ring should be rebuilt at, or `None` to
    /// leave it alone.
    ///
    /// Growth is immediate and unconditional — a frame that does not fit is a
    /// frame that falls back — and resets the watch, so a ring that has just
    /// grown is never a candidate to shrink on the next staging.
    fn observe(
        &mut self,
        held: wgpu::BufferAddress,
        demand: wgpu::BufferAddress,
    ) -> Option<wgpu::BufferAddress> {
        if demand > held {
            self.dwell = 0;
            self.peak = 0;
            return Some(demand);
        }
        // Joined to the run so far, would the ring still be more than the
        // ratio too big? If so this staging extends the run and raises its
        // peak; if not the run ends here — and this staging may still be the
        // first of a new one, which is the case a plain reset gets wrong: it
        // would throw away a staging that qualifies on its own and make every
        // busy frame cost a dwell of `DWELL + 1`.
        let joined = self.peak.max(demand);
        if held >= joined.saturating_mul(STAGING_RING_SHRINK_HYSTERESIS) {
            self.peak = joined;
            self.dwell += 1;
        } else {
            self.peak = demand;
            self.dwell = u32::from(held >= demand.saturating_mul(STAGING_RING_SHRINK_HYSTERESIS));
        }
        if self.dwell < STAGING_RING_SHRINK_DWELL {
            return None;
        }
        // The run's own peak, which every staging in it fitted inside — not the
        // last staging, which would refuse a size the run had already served.
        let shrink_to = self.peak;
        self.dwell = 0;
        self.peak = 0;
        (shrink_to > 0 && shrink_to < held).then_some(shrink_to)
    }
}

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
    /// What each slot is sized to.
    bytes: wgpu::BufferAddress,
    /// Debug label prefix, so a capture says which caller a buffer belongs to.
    label: String,
    /// When this ring may give a past peak back. See [`ShrinkWatch`].
    shrink: ShrinkWatch,
    /// Slots written since the last [`Ring::remap_submitted`], newest last.
    ///
    /// A slot's mapping is asked back **after** the copy reading it is on the
    /// queue, and the copy is now recorded on the caller's own encoder rather
    /// than on one this ring submits itself. So a written slot waits here
    /// until the caller says that encoder went. See [`Ring::remap_submitted`].
    written: Vec<usize>,
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
            shrink: ShrinkWatch::default(),
            written: Vec::with_capacity(STAGING_RING_DEPTH),
        }
    }

    /// **Size this ring to what it is being asked to stage**, growing at once
    /// and shrinking only after a dwell inside a dead band.
    ///
    /// This was `grow`, and monotone: a ring that reached a past peak held it
    /// for the life of the process. On native scene A the geometry path's
    /// per-staging total climbs to 32.6 MB, so its slots settle at 36 MiB
    /// apiece and stay there through every trivial frame that follows — and a
    /// ring holds [`STAGING_RING_DEPTH`] of them. [`ShrinkWatch`] is the policy;
    /// [`STAGING_RING_SHRINK_HYSTERESIS`] carries what a resize costs and why
    /// the band is as wide as it is.
    /// **A resize drops every slot**, [`Ring::remap_submitted`]'s waiting list
    /// with them. That is safe where a caller stages once per frame and tells
    /// the ring its encoder went before the next staging — the copies of a
    /// dropped slot are already on the queue, and wgpu keeps the buffer alive
    /// until they drain. A caller that resized with an unsubmitted copy
    /// standing would lose it.
    pub fn fit(&mut self, device: &wgpu::Device, bytes: wgpu::BufferAddress) {
        let Some(size) = self.shrink.observe(self.bytes, bytes) else {
            return;
        };
        *self = Self::new(device, size, &self.label.clone());
    }

    /// A slot the host may write, or `None` when every one is still in flight.
    ///
    /// The slot is recorded as written before it is handed over: the caller's
    /// copy goes on an encoder this ring does not submit, so the mapping is
    /// asked back at [`Ring::remap_submitted`] and not here.
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
        self.written.push(index);
        Some(&self.slots[index])
    }

    /// **The encoder every claimed slot's copy was recorded into has been
    /// submitted**: ask for those mappings back.
    ///
    /// Idempotent and cheap on a frame that claimed nothing. A ring that is
    /// never told runs out of mapped slots and [`Ring::claim`] answers `None`
    /// from then on, which is the caller's own fallback route — degraded,
    /// never wrong.
    pub fn remap_submitted(&mut self) {
        for index in self.written.drain(..) {
            self.slots[index].remap();
        }
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

    // -- The sizing policy ---------------------------------------------------

    /// The geometry path's settled slot on native scene A: 32.6 MB rounded up
    /// to its 4 MiB granularity.
    const SETTLED: wgpu::BufferAddress = 36 << 20;
    /// A trivial frame's slot at the same granularity.
    const TRIVIAL: wgpu::BufferAddress = 4 << 20;

    /// **A ring that has grown to a past peak gives it back**, once demand has
    /// stayed inside the dead band for the whole dwell.
    ///
    /// Floor — make `ShrinkWatch::observe` return `None` whenever
    /// `demand <= held`, which is what `Ring::grow` did: the walk below never
    /// resizes and `held` stays at 36 MiB for every one of the 1000 stagings.
    #[test]
    fn a_ring_gives_a_past_peak_back_after_the_dwell() {
        let mut watch = ShrinkWatch::default();
        let mut held = SETTLED;
        let mut resizes = Vec::new();
        for staging in 0..1000u32 {
            if let Some(size) = watch.observe(held, TRIVIAL) {
                resizes.push((staging, size));
                held = size;
            }
        }
        assert_eq!(
            resizes,
            vec![(STAGING_RING_SHRINK_DWELL - 1, TRIVIAL)],
            "a ring settled at {SETTLED} B a slot and asked for {TRIVIAL} B a              thousand times running resized {:?}. Exactly one shrink is right:              the first at the dwell, and none after, because {TRIVIAL} is not              twice anything smaller that was asked for.",
            resizes,
        );
        assert_eq!(held, TRIVIAL);
        let given_back = (SETTLED - TRIVIAL) * STAGING_RING_DEPTH as u64;
        assert_eq!(
            given_back,
            64 << 20,
            "the walk above hands back {given_back} B across the ring's              {STAGING_RING_DEPTH} slots",
        );
    }

    /// **Nothing is given back before the dwell is up**, and a single frame
    /// back at the old size restarts it — the anti-thrash half.
    #[test]
    fn one_busy_staging_restarts_the_dwell() {
        let mut watch = ShrinkWatch::default();
        for _ in 0..(STAGING_RING_SHRINK_DWELL - 1) {
            assert_eq!(
                watch.observe(SETTLED, TRIVIAL),
                None,
                "a ring must not resize inside its dwell",
            );
        }
        // One staging that needs the room, one short of the dwell.
        assert_eq!(watch.observe(SETTLED, SETTLED), None, "it already fits");
        for _ in 0..(STAGING_RING_SHRINK_DWELL - 1) {
            assert_eq!(
                watch.observe(SETTLED, TRIVIAL),
                None,
                "the dwell must start again from that staging, not resume",
            );
        }
        assert_eq!(
            watch.observe(SETTLED, TRIVIAL),
            Some(TRIVIAL),
            "and the full dwell after it does resize",
        );
    }

    /// **The dead band is what it says**: a ring less than
    /// [`STAGING_RING_SHRINK_HYSTERESIS`]x the demand never shrinks, however
    /// long it is asked.
    #[test]
    fn a_ring_inside_the_dead_band_never_shrinks() {
        let mut watch = ShrinkWatch::default();
        // Exactly at the ratio's edge, from the inside.
        let demand = SETTLED / STAGING_RING_SHRINK_HYSTERESIS + 1;
        for _ in 0..(STAGING_RING_SHRINK_DWELL * 4) {
            assert_eq!(
                watch.observe(SETTLED, demand),
                None,
                "a ring holding {SETTLED} B asked for {demand} B shrank;                  re-growing costs a frame, and the next staging over the ratio                  would ask for it back",
            );
        }
        // And one byte the other side of the edge does shrink, so the assertion
        // above is about the ratio and not about the walk being inert.
        let mut watch = ShrinkWatch::default();
        let inside = SETTLED / STAGING_RING_SHRINK_HYSTERESIS;
        let mut resized = None;
        for _ in 0..STAGING_RING_SHRINK_DWELL {
            resized = resized.or(watch.observe(SETTLED, inside));
        }
        assert_eq!(resized, Some(inside));
    }

    /// **A shrink resizes to the run's own peak, not to the last staging.**
    /// Every frame inside the run fitted; the ring must still fit them all.
    #[test]
    fn a_shrink_lands_on_the_largest_staging_of_the_run_it_ends() {
        let mut watch = ShrinkWatch::default();
        let peak = SETTLED / 4;
        let mut resized = None;
        for staging in 0..STAGING_RING_SHRINK_DWELL {
            // One staging in the middle is four times the rest, and is still
            // well inside the dead band.
            let demand = if staging == 7 { peak } else { peak / 4 };
            resized = resized.or(watch.observe(SETTLED, demand));
        }
        assert_eq!(
            resized,
            Some(peak),
            "the ring resized to something other than the largest staging of              the run, so a staging the run already served would now be refused",
        );
    }

    /// **The real ring, on a real device, gives the pages back** — and its own
    /// always-on figure says so.
    ///
    /// The policy suites above are the part that can be wrong, and they run
    /// everywhere. This one is the end of the chain: `Ring::fit` really does
    /// rebuild the slots at the smaller size, and [`Ring::host_bytes`] — an
    /// accessor that until now had no caller outside a test — really does fall.
    ///
    /// `#[ignore]` for the reason the sibling device suites are:
    /// `cargo test -p squallar-gpu --lib -- --ignored`.
    #[test]
    #[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_real_ring_hands_its_pages_back_after_the_dwell() {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .expect("no wgpu adapter; this test is ignored by default for that reason");
        let (device, _queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("squallar.staging_ring.test"),
                required_features: adapter.features() & STAGING_RING_FEATURE,
                required_limits: adapter.limits(),
                memory_hints: Default::default(),
                experimental_features: Default::default(),
                trace: Default::default(),
            }))
            .expect("could not create a device on an adapter that was found");
        if !device_has_ring(&device) {
            eprintln!("adapter has no MAPPABLE_PRIMARY_BUFFERS; nothing to exercise");
            return;
        }

        let mut ring = Ring::new(&device, SETTLED, "squallar.staging_ring.test");
        let settled = ring.host_bytes();
        assert_eq!(
            settled,
            SETTLED as usize * STAGING_RING_DEPTH,
            "premise: the ring is holding every slot it was built with",
        );

        for _ in 0..STAGING_RING_SHRINK_DWELL {
            ring.fit(&device, TRIVIAL);
        }
        let after = ring.host_bytes();
        assert_eq!(
            after,
            TRIVIAL as usize * STAGING_RING_DEPTH,
            "the ring held {settled} B through {STAGING_RING_SHRINK_DWELL} \
             consecutive stagings of {TRIVIAL} B and gave back {} B",
            settled.saturating_sub(after),
        );

        // And it is a working ring afterwards, not a torn-down one: a staging
        // that needs the room gets it back on the spot.
        ring.fit(&device, SETTLED);
        assert_eq!(
            ring.host_bytes(),
            settled,
            "a shrunk ring must grow again immediately when a staging needs it",
        );
    }

    /// Growth is immediate, unconditional and resets the watch.
    #[test]
    fn growth_is_immediate_and_clears_the_dwell() {
        let mut watch = ShrinkWatch::default();
        for _ in 0..(STAGING_RING_SHRINK_DWELL - 1) {
            assert_eq!(watch.observe(SETTLED, TRIVIAL), None);
        }
        assert_eq!(
            watch.observe(SETTLED, SETTLED * 2),
            Some(SETTLED * 2),
            "a staging that does not fit must grow the ring on the spot;              waiting a dwell would make the frame fall back for that long",
        );
        for _ in 0..(STAGING_RING_SHRINK_DWELL - 1) {
            assert_eq!(
                watch.observe(SETTLED * 2, TRIVIAL),
                None,
                "and the dwell restarts from the growth, so a ring cannot be                  rebuilt twice in consecutive stagings",
            );
        }
    }
}
