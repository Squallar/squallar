//! GPU-side pass timing: what the raymarch, ground, mirror and main passes
//! cost the GPU, measured with timestamp queries rather than inferred from
//! CPU-side encode time.
//!
//! An opt-in instrument, not product telemetry: a [`GpuPassProbe`] exists only
//! when the device carries `TIMESTAMP_QUERY` **and** the install asked for the
//! frame timing lines. Everywhere else the probe is `None` and every call site
//! is behind that `Option`, so an ordinary install submits **zero** query
//! operations — the absence is count-gated, not merely cheap.
//!
//! # Shape
//!
//! One 8-slot timestamp query set — a begin/end pair per pass family
//! ([`ProbedPass`]) — and a ring of [`RING_SLOTS`] resolve+staging buffer
//! pairs. Per frame: each family's bracket is handed out **once** to the first
//! pass of that kind that asks (later passes are counted, never bracketed —
//! six volume panes are legal and each is one raymarch pass), the claimed
//! ranges are resolved into the frame's ring slot after the last pass, and the
//! slot's staging buffer is mapped with a **non-blocking** `map_async` whose
//! result is harvested on a later frame's [`GpuPassProbe::collect`]. The frame
//! thread never waits on the device: a slot that has not mapped yet is simply
//! not read this frame, which is what the ring is for.
//!
//! # Denominators
//!
//! Three figures leave this module and they count different things:
//! * a family's **pass count** is every pass of that kind encoded, bracketed
//!   or not;
//! * a family's **histogram** holds one sample per frame in which the family
//!   ran — the bracketed pass's duration;
//! * **frames** is the resolves collected, the non-vacuity floor under the
//!   histograms.
//!
//! A duration sample is `end - start` in whole microseconds through
//! [`Hist::record`], so every quoted percentile is a conservative bin upper
//! edge. **No figure from this module ever gates CI.**

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use egui_wgpu::wgpu;
use squallar_device_profile::hist::Hist;

/// The pass families the probe brackets, in query-set order: family `i` owns
/// timestamp indices `2i` (begin) and `2i + 1` (end).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbedPass {
    /// The volume raymarch into a pane's offscreen. Up to one per 3D pane per
    /// frame.
    Raymarch = 0,
    /// The terrain/prism ground pass. Encoded only for a target carrying
    /// ground attachments — 0 per frame in the shipped build until terrain
    /// wires in; the bracket is here so its first frame is a measured one.
    Ground = 1,
    /// The pane-mirror pass (the 3D floor's source copy). At most one per
    /// frame.
    Mirror = 2,
    /// egui's main render pass into the swapchain. At most one per frame.
    Main = 3,
}

/// The bracketed families, for iteration in family order.
pub const PROBED_PASSES: [ProbedPass; FAMILIES] = [
    ProbedPass::Raymarch,
    ProbedPass::Ground,
    ProbedPass::Mirror,
    ProbedPass::Main,
];

/// How many pass families the query set brackets.
const FAMILIES: usize = 4;

/// Timestamps in the query set: a begin/end pair per family.
const QUERY_COUNT: u32 = (FAMILIES as u32) * 2;

/// Resolve+staging pairs in flight at once. A slot is claimed at resolve time
/// and freed when its map result has been read; three cover the map latency
/// of a frame-per-submit cadence with room for one slow harvest.
const RING_SLOTS: usize = 3;

/// `resolve_query_set` requires a 256-byte-aligned destination offset, so
/// each family's 16-byte pair lives at its own aligned offset and the buffers
/// are sized to the last one.
const FAMILY_STRIDE: u64 = wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT;
const SLOT_BYTES: u64 = FAMILY_STRIDE * FAMILIES as u64;

/// The per-frame state both sides of the frame touch: the callback side
/// (through a [`GpuProbeHandle`] clone in egui's `CallbackResources`) claims
/// brackets and counts passes; the owner drains both at end of frame. All on
/// the frame thread — the atomics satisfy the resource map's `Sync` bound,
/// they are not a cross-thread protocol, so every ordering is `Relaxed`.
struct ProbeShared {
    query_set: wgpu::QuerySet,
    /// Whether family `i`'s bracket has been handed out this frame.
    claimed: [AtomicBool; FAMILIES],
    /// Passes of family `i` encoded this frame, bracketed or not.
    counts: [AtomicU32; FAMILIES],
}

impl ProbeShared {
    /// One pass of `pass`'s family is being encoded: count it, and hand out
    /// the family's begin/end bracket if this frame has not already.
    fn pass_timestamps(&self, pass: ProbedPass) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        let family = pass as usize;
        self.counts[family].fetch_add(1, Ordering::Relaxed);
        if self.claimed[family].swap(true, Ordering::Relaxed) {
            return None;
        }
        Some(wgpu::RenderPassTimestampWrites {
            query_set: &self.query_set,
            beginning_of_pass_write_index: Some(family as u32 * 2),
            end_of_pass_write_index: Some(family as u32 * 2 + 1),
        })
    }
}

/// The cheap, cloneable half of the probe, held in egui's `CallbackResources`
/// so a paint callback's `prepare` — which can name nothing else — can bracket
/// the passes it encodes.
#[derive(Clone)]
pub struct GpuProbeHandle {
    shared: Arc<ProbeShared>,
}

impl GpuProbeHandle {
    /// See [`ProbeShared::pass_timestamps`]. `None` means "this family is
    /// already bracketed this frame — encode the pass without timestamps",
    /// which the `*_with_timestamps` encoders accept as-is.
    pub fn pass_timestamps(&self, pass: ProbedPass) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        self.shared.pass_timestamps(pass)
    }
}

/// One resolve+staging pair and where it is in its life.
struct RingSlot {
    resolve: wgpu::Buffer,
    staging: wgpu::Buffer,
    state: SlotState,
    /// Set by the `map_async` callback; readable the frame after the map
    /// completes. `Arc`ed because the callback outlives the borrow.
    mapped: Arc<AtomicBool>,
}

/// A slot's life: free → resolved into this frame's submit → mapping (the
/// one `map_async` asked, its completion signalled through
/// [`RingSlot::mapped`]) → read and free again. The claimed-family set rides
/// the state so a harvest reads exactly the ranges the resolve wrote.
enum SlotState {
    Free,
    Resolved([bool; FAMILIES]),
    Mapping([bool; FAMILIES]),
}

/// A copy of everything the probe has measured, for the telemetry line.
/// Fields are indexed by [`ProbedPass`] as usize; see the module doc for what
/// each figure counts.
#[derive(Clone, Copy)]
pub struct GpuPassReport {
    /// One histogram of bracketed-pass durations per family.
    pub hists: [Hist; FAMILIES],
    /// Cumulative passes encoded per family, bracketed or not.
    pub passes: [u64; FAMILIES],
    /// Resolves collected — frames represented in the histograms.
    pub frames: u64,
}

impl GpuPassReport {
    /// The family's duration histogram.
    pub fn hist(&self, pass: ProbedPass) -> &Hist {
        &self.hists[pass as usize]
    }

    /// The family's cumulative encoded-pass count.
    pub fn passes(&self, pass: ProbedPass) -> u64 {
        self.passes[pass as usize]
    }
}

/// The owning half: ring, histograms and totals. Owned by the renderer, one
/// per device; the [`GpuProbeHandle`] clones it hands out share the query set
/// and the per-frame claim state.
pub struct GpuPassProbe {
    shared: Arc<ProbeShared>,
    /// Nanoseconds per timestamp tick, from `Queue::get_timestamp_period`.
    period_ns: f32,
    ring: [RingSlot; RING_SLOTS],
    hists: [Hist; FAMILIES],
    passes: [u64; FAMILIES],
    frames: u64,
}

impl GpuPassProbe {
    /// Build the probe, or answer `None` on a device that cannot time a pass.
    /// The caller gates on the *install asking* (frame telemetry keyed loud);
    /// this gates on the device being able to answer.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("squallar.gpu_probe.queries"),
            ty: wgpu::QueryType::Timestamp,
            count: QUERY_COUNT,
        });
        let ring = std::array::from_fn(|slot| RingSlot {
            resolve: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("squallar.gpu_probe.resolve.{slot}")),
                size: SLOT_BYTES,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            staging: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("squallar.gpu_probe.staging.{slot}")),
                size: SLOT_BYTES,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            state: SlotState::Free,
            mapped: Arc::new(AtomicBool::new(false)),
        });
        Some(Self {
            shared: Arc::new(ProbeShared {
                query_set,
                claimed: std::array::from_fn(|_| AtomicBool::new(false)),
                counts: std::array::from_fn(|_| AtomicU32::new(0)),
            }),
            period_ns: queue.get_timestamp_period(),
            ring,
            hists: [Hist::new(); FAMILIES],
            passes: [0; FAMILIES],
            frames: 0,
        })
    }

    /// A handle for egui's `CallbackResources`, so `prepare` can bracket the
    /// raymarch and ground passes it encodes.
    pub fn handle(&self) -> GpuProbeHandle {
        GpuProbeHandle {
            shared: Arc::clone(&self.shared),
        }
    }

    /// [`ProbeShared::pass_timestamps`] for the owner's own call sites — the
    /// mirror and main passes, which the renderer encodes itself.
    pub fn pass_timestamps(&self, pass: ProbedPass) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        self.shared.pass_timestamps(pass)
    }

    /// Close the frame: drain the per-frame counts into the totals and, when
    /// any bracket was claimed and a ring slot is free, encode the claimed
    /// families' resolves and the staging copy into `encoder`. Runs after the
    /// last pass of the frame is recorded and before the frame's submit, on
    /// every frame — the skipped-surface path included, whose mirror and
    /// raymarch work is still submitted.
    ///
    /// A frame whose claims find every slot in flight drops them: the stamps
    /// are overwritten by the next frame's brackets and no resolve ever reads
    /// them. A missed sample, never a wait.
    pub fn end_frame(&mut self, encoder: &mut wgpu::CommandEncoder) {
        let mut claimed = [false; FAMILIES];
        let mut any = false;
        for (family, taken) in claimed.iter_mut().enumerate() {
            self.passes[family] += u64::from(self.shared.counts[family].swap(0, Ordering::Relaxed));
            *taken = self.shared.claimed[family].swap(false, Ordering::Relaxed);
            any |= *taken;
        }
        if !any {
            return;
        }
        let Some(slot) = self
            .ring
            .iter_mut()
            .find(|slot| matches!(slot.state, SlotState::Free))
        else {
            return;
        };
        for (family, claimed) in claimed.iter().enumerate() {
            if !claimed {
                continue;
            }
            let first = family as u32 * 2;
            encoder.resolve_query_set(
                &self.shared.query_set,
                first..first + 2,
                &slot.resolve,
                family as u64 * FAMILY_STRIDE,
            );
        }
        encoder.copy_buffer_to_buffer(&slot.resolve, 0, &slot.staging, 0, SLOT_BYTES);
        slot.state = SlotState::Resolved(claimed);
        slot.mapped.store(false, Ordering::Relaxed);
    }

    /// Harvest the ring: ask the just-submitted slot to map, and fold every
    /// slot whose earlier map has completed into the histograms. Called after
    /// the frame's `queue.submit`; nothing here blocks — `map_async` results
    /// are delivered by the device's own later maintenance (the next frame's
    /// submit, in practice), which is why a sample lands a frame or more after
    /// the passes it times.
    pub fn collect(&mut self) {
        for slot in &mut self.ring {
            match slot.state {
                SlotState::Free => {}
                // Resolved this frame (or a frame whose collect this is the
                // first since): ask for the map exactly once. `map_async` on
                // a buffer already mapping is an error, which is what the
                // Resolved→Mapping edge exists to prevent.
                SlotState::Resolved(claimed) => {
                    let mapped = Arc::clone(&slot.mapped);
                    slot.staging
                        .slice(..)
                        .map_async(wgpu::MapMode::Read, move |result| {
                            // A failed map (device loss) never sets the flag;
                            // the slot then idles in Mapping, which on a lost
                            // device is the whole instrument's state anyway.
                            if result.is_ok() {
                                mapped.store(true, Ordering::Relaxed);
                            }
                        });
                    slot.state = SlotState::Mapping(claimed);
                }
                SlotState::Mapping(claimed) => {
                    if !slot.mapped.load(Ordering::Relaxed) {
                        continue;
                    }
                    let mut frame_counted = false;
                    {
                        let view = slot.staging.slice(..).get_mapped_range();
                        for (family, claimed) in claimed.iter().enumerate() {
                            if !claimed {
                                continue;
                            }
                            let at = family * FAMILY_STRIDE as usize;
                            let begin =
                                u64::from_le_bytes(view[at..at + 8].try_into().expect("8 bytes"));
                            let end = u64::from_le_bytes(
                                view[at + 8..at + 16].try_into().expect("8 bytes"),
                            );
                            let ticks = end.wrapping_sub(begin);
                            // Ticks → whole microseconds, saturating into the
                            // histogram's over-ceiling clamp rather than
                            // wrapping: a non-monotone pair reads as an
                            // outlier, never as a small number.
                            let us = (ticks as f64 * f64::from(self.period_ns) / 1_000.0)
                                .min(f64::from(u32::MAX))
                                as u32;
                            self.hists[family].record(us);
                            frame_counted = true;
                        }
                    }
                    slot.staging.unmap();
                    slot.state = SlotState::Free;
                    if frame_counted {
                        self.frames += 1;
                    }
                }
            }
        }
    }

    /// Everything measured so far, copied out for the telemetry line.
    pub fn report(&self) -> GpuPassReport {
        GpuPassReport {
            hists: self.hists,
            passes: self.passes,
            frames: self.frames,
        }
    }
}
