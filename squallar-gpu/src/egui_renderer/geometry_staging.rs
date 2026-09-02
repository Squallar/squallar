//! Getting a frame's vertices and indices onto the GPU without the frame
//! thread pushing them across PCIe itself.
//!
//! `egui_wgpu::Renderer::update_buffers` stages both arrays through
//! `Queue::write_buffer_with`, whose mapping is a `MAP_WRITE | COPY_SRC`
//! buffer — `wgpu-hal`'s `MemoryLocation::CpuToGpu`, which on a discrete card
//! is the host-visible BAR window. [`crate::staging_ring`] documents what that
//! costs for textures. Measured here for the geometry shape, RTX 3090 /
//! Vulkan, by `squallar-gpu/tests/geometry_staging_gpu.rs`, whose module note
//! carries the table: **2.15 GB/s through the window -- stable to the second
//! decimal at every size and every chunking taken -- against a cached-RAM
//! memcpy an order of magnitude faster.** The window's figure not moving with
//! chunk count is what says it is the bytes being paid for and not the memcpy
//! calls. Over the function itself, 60 consecutive `update_buffers` calls at
//! 13.75 MB a frame, three runs at different box loads: **7 657 to 7 800 us a
//! frame through the window, 945 to 1 816 us through the ring**, nothing
//! declined in any of them. A 4.2x cut at worst and 8.1x at best — the window
//! side varies by 1.9% and the ring side by 1.9x, and that asymmetry is the
//! box, not the routes.
//!
//! The same ring answers it, with one difference from the texture path: a
//! frame's whole geometry has to be resident before the draw, so there is
//! nothing to band. One slot carries both arrays — indices at 0, vertices
//! after them — which is one claim, one unmap and one submit per staging
//! rather than two.
//!
//! A slot is sized to [`GEOMETRY_SLOT_GRANULARITY`] rather than to the frame's
//! exact total, because the ring is grow-only and rebuilds **both** slots when
//! it grows — two host allocations on the frame thread, which is the thing
//! being optimised. A per-frame total sized exactly would rebuild on every new
//! peak, and on native scene A the per-staging total climbs 36 KB to 32.6 MB
//! over the load, setting a new peak on most frames of it.
//!
//! Above [`MAX_STAGED_GEOMETRY_BYTES`] the ring is refused rather than grown:
//! it is grow-only and holds [`crate::staging_ring::STAGING_RING_DEPTH`]
//! slots, so a single outsized frame would pin twice its own size for the rest
//! of the session. A refused frame takes the route it takes today and costs
//! what it costs today; [`GeometryStagingTotals::declined`] is where that shows.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use egui_wgpu::wgpu;

use crate::staging_ring::{Ring, device_has_ring};

/// The largest single staging this path will take, and so the largest a ring
/// slot may grow to.
///
/// 64 MiB. Measured on native scene A (2026-09-02, `frame prep geometry:`
/// running totals differenced across two-second reports): the heaviest window
/// staged 32.6 MB per `update_buffers` call, so this is a shade under 2x the
/// worst frame that shape has produced. At
/// [`crate::staging_ring::STAGING_RING_DEPTH`] slots it bounds this path's
/// pinned host memory at 128 MiB, against the 16.9 MiB the texture ring holds.
pub const MAX_STAGED_GEOMETRY_BYTES: u64 = 64 << 20;

/// How coarsely a ring slot is sized.
///
/// 4 MiB. Every growth rebuilds both slots, so the granularity is what bounds
/// how many rebuilds a rising scene costs: native scene A's climb from 36 KB to
/// 32.6 MB is at most eight of them at this size, against one per new peak if
/// the slot were sized exactly. What it costs is at most this much waste per
/// slot, twice — 8 MiB held by a renderer that stages a kilobyte.
pub const GEOMETRY_SLOT_GRANULARITY: u64 = 4 << 20;

/// What this renderer's geometry staging has actually done.
///
/// **Product telemetry, not a campaign instrument.** Always on: three atomic
/// adds per `update_buffers` call, on a path that has just memcpy'd megabytes.
///
/// # Denominator
///
/// **Every `update_buffers` call that had geometry to stage**, which is the
/// denominator [`super::pass_costs::StagedGeometry::calls`] has minus the
/// calls whose picture was entirely paint callbacks. `staged + declined` is
/// that count; the split is which route the bytes took.
///
/// # Why a zero here is readable
///
/// `staged == 0 && declined == 0` is a build that never installed a stager —
/// every device without [`crate::staging_ring::STAGING_RING_FEATURE`], which
/// is all of the web. `staged == 0 && declined > 0` is a stager that is
/// installed and refusing, which reads identically in a frame clock and not at
/// all identically here. Without the second field the first would be the only
/// reading, and a permanently refusing ring would look exactly like a build
/// that never had one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GeometryStagingTotals {
    /// Stagings that went through the ring.
    pub staged: u64,
    /// Stagings the ring refused — over [`MAX_STAGED_GEOMETRY_BYTES`], or
    /// every slot still feeding a copy. These took
    /// `Queue::write_buffer_with`, exactly as a build without a stager does.
    pub declined: u64,
    /// Bytes that went through the ring. Indices and vertices together, which
    /// is the same total `StagedGeometry::bytes` counts for the same calls.
    pub bytes: u64,
}

/// The counters themselves. Behind an [`Arc`] because the stager is moved into
/// `egui_wgpu::Renderer` and the reader stays outside it.
#[derive(Debug, Default)]
struct Counters {
    staged: AtomicU64,
    declined: AtomicU64,
    bytes: AtomicU64,
}

/// The read side of [`GeometryStaging`], cloneable and cheap.
#[derive(Clone, Debug, Default)]
pub struct GeometryStagingLedger(Arc<Counters>);

impl GeometryStagingLedger {
    /// This renderer's running totals. See [`GeometryStagingTotals`] for the
    /// denominator and for why each zero is readable.
    pub fn totals(&self) -> GeometryStagingTotals {
        GeometryStagingTotals {
            staged: self.0.staged.load(Ordering::Relaxed),
            declined: self.0.declined.load(Ordering::Relaxed),
            bytes: self.0.bytes.load(Ordering::Relaxed),
        }
    }
}

/// Whether this device can stage geometry through cached host memory at all.
/// The same capability the texture path asks for, asked the same way.
pub fn available(device: &wgpu::Device) -> bool {
    device_has_ring(device)
}

/// A [`Ring`] wired up as egui's geometry staging.
pub struct GeometryStaging {
    /// Built on the first staging, so a renderer that never draws a mesh holds
    /// no pinned host memory.
    ring: Option<Ring>,
    counters: Arc<Counters>,
}

impl GeometryStaging {
    /// A stager reporting into `ledger`.
    pub fn new(ledger: &GeometryStagingLedger) -> Self {
        Self {
            ring: None,
            counters: Arc::clone(&ledger.0),
        }
    }
}

impl egui_wgpu::GeometryStager for GeometryStaging {
    fn stage(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        index: (&wgpu::Buffer, u64),
        vertex: (&wgpu::Buffer, u64),
        fill: &mut dyn FnMut(&mut wgpu::BufferViewMut),
    ) -> bool {
        let (index_buffer, index_bytes) = index;
        let (vertex_buffer, vertex_bytes) = vertex;
        let total = index_bytes + vertex_bytes;
        if total == 0 {
            // Not a refusal — nothing was asked for, and counting it would put
            // the ring's own idle frames in the same field as the ones it could
            // not serve.
            return false;
        }
        let slot_bytes = total.next_multiple_of(GEOMETRY_SLOT_GRANULARITY);
        if slot_bytes > MAX_STAGED_GEOMETRY_BYTES {
            self.counters.declined.fetch_add(1, Ordering::Relaxed);
            return false;
        }

        let ring = self
            .ring
            .get_or_insert_with(|| Ring::new(device, slot_bytes, "squallar.geometry.staging"));
        ring.grow(device, slot_bytes);
        let Some(slot) = ring.claim(device) else {
            self.counters.declined.fetch_add(1, Ordering::Relaxed);
            return false;
        };

        {
            let mut region = slot.buffer().get_mapped_range_mut(..total);
            fill(&mut region);
        }
        slot.buffer().unmap();

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("squallar.geometry.staging"),
        });
        // Both counts are multiples of four — `u32` indices and a 20-byte
        // `Vertex` — so the vertex half starts on a `COPY_BUFFER_ALIGNMENT`
        // boundary and neither copy needs padding.
        if index_bytes > 0 {
            encoder.copy_buffer_to_buffer(slot.buffer(), 0, index_buffer, 0, index_bytes);
        }
        if vertex_bytes > 0 {
            encoder.copy_buffer_to_buffer(
                slot.buffer(),
                index_bytes,
                vertex_buffer,
                0,
                vertex_bytes,
            );
        }
        // Submitted here rather than on the frame's own encoder, for the reason
        // `super::texture_upload` submits its band copies here: a map asked for
        // against an unsubmitted copy resolves early and panics at submission.
        // Queue order is what makes it land before the draw — this submission
        // is ahead of the one the caller has not made yet.
        queue.submit(Some(encoder.finish()));
        slot.remap();

        self.counters.staged.fetch_add(1, Ordering::Relaxed);
        self.counters.bytes.fetch_add(total, Ordering::Relaxed);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Per-frame staged totals shaped like native scene A's load: 36 KB to
    /// 32.6 MB, rising on **every** frame. That monotonic shape is the worst
    /// case for a grow-only ring and is what the control below checks is real.
    fn a_rising_scene() -> Vec<u64> {
        let first = 35_976.0f64;
        let last = 32_642_086.0f64;
        (0..200)
            .map(|frame| {
                let along = f64::from(frame) / 199.0;
                (first * (last / first).powf(along)) as u64
            })
            .collect()
    }

    /// Ring rebuilds a run of frames costs, given how a slot is sized.
    /// [`Ring::grow`] rebuilds when the wanted size exceeds what is held, so
    /// this is the same monotone comparison it makes.
    fn rebuilds(frames: &[u64], slot_size: fn(u64) -> u64) -> usize {
        let mut held = 0;
        let mut count = 0;
        for &total in frames {
            let wanted = slot_size(total);
            if wanted > held {
                held = wanted;
                count += 1;
            }
        }
        count
    }

    /// **Sizing a slot to the frame's exact total rebuilds the ring on every
    /// frame of a rising scene**, and each rebuild is two host allocations on
    /// the frame thread *plus* a frame that falls back to the BAR window,
    /// because [`Ring::new`]'s slots are not mapped yet when `claim` asks.
    ///
    /// The first assertion is the control, and it is what stops the second
    /// from passing on a curve that simply does not rise: it says this scene
    /// really does set a new peak on all 200 frames.
    #[test]
    fn the_granularity_is_what_keeps_a_rising_scene_from_rebuilding_every_frame() {
        let frames = a_rising_scene();

        let exact = rebuilds(&frames, |total| total);
        assert_eq!(
            exact,
            frames.len(),
            "the control scene rebuilt {exact} times over {} frames rather than \
             every frame, so it does not rise the way native scene A's load \
             does and the figure below is not a reduction of anything",
            frames.len(),
        );

        let granular = rebuilds(&frames, |total| {
            total.next_multiple_of(GEOMETRY_SLOT_GRANULARITY)
        });
        assert!(
            granular <= 9,
            "{granular} rebuilds over the same climb at a \
             {GEOMETRY_SLOT_GRANULARITY} B granularity. The span is 36 KB to \
             32.6 MB, so no more than nine distinct multiples can be crossed; \
             more than that means the rounding is not being applied.",
        );
    }

    /// The cap is reachable, rather than being rounded past.
    ///
    /// A slot is sized to a whole [`GEOMETRY_SLOT_GRANULARITY`] and *then*
    /// compared with [`MAX_STAGED_GEOMETRY_BYTES`], so a cap that is not a
    /// whole number of granules would refuse frames below the number it
    /// advertises.
    #[test]
    fn the_cap_is_a_whole_number_of_granules() {
        assert_eq!(
            MAX_STAGED_GEOMETRY_BYTES % GEOMETRY_SLOT_GRANULARITY,
            0,
            "a {MAX_STAGED_GEOMETRY_BYTES} B cap is not a whole number of \
             {GEOMETRY_SLOT_GRANULARITY} B granules, so the largest frame this \
             path actually takes is below the cap it names",
        );
    }
}
