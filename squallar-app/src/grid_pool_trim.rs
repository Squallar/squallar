//! **When a session that has stopped decoding grids gives its staging blocks
//! back.**
//!
//! `squallar_overlays::staging` parks one decode buffer per gridded source —
//! **224,000 B on MRMS and 15,000,000 B on GMGSI** — so that the next granule
//! is filled into a block that already exists instead of one the allocator has
//! to find.
//!
//! **The MRMS figure here said 49,000,000 B and was stale by 218x.** That was
//! the flat CONUS plane the decode used to build for the tiler to read; since
//! `decode::tile_png_codes` walks PNG rows instead, the slot holds
//! `mrms::CONUS_BAND_BYTES` — 16 rows, pinned at 224,000 B by
//! `mrms::staging::STAGING_POINTS`'s own `const _`. Every reading below that
//! rests on the 49 MB figure is marked where it sits.
//!
//! The pool has exactly one release lever and, until this module, exactly one
//! caller for it: `App::on_pressure`, which fires when the heap has already
//! refused an allocation. So on every session that never hits a wall the two
//! blocks are held for the life of the process.
//!
//! # What that costs, measured
//!
//! `overlay grids` is the census family the two blocks publish into, and on a
//! one-pane eighteen-layer still leg it read **122.9 MiB** (five legs, spanning
//! 2.0 MiB). Priced through the shipped registry's own doors
//! (`squallar-overlays/tests/overlay_grid_residency_split.rs`, and the same
//! doors again here), the still steady state of that family is:
//!
//! ```text
//! MRMS  live mosaic   49,000,000 B   read by hover and by every re-raster
//! MRMS  parked block  49,000,000 B   read by nothing
//! GMGSI live mosaic   15,000,000 B   read by hover and by every re-raster
//! GMGSI parked block  15,000,000 B   read by nothing
//!                    ------------
//!                    128,000,000 B = 122.07 MiB, and the leg's own 2.0 MiB
//!                    of leg-to-leg spread is the lightning cache above it.
//! ```
//!
//! **That table is the reading it was, and BOTH of its MRMS rows have since
//! moved.** The mosaic is tiled now
//! (`squallar_overlays::render::gridded::TiledU16`) and reads 2,350,138 to
//! 8,943,164 B over 28 granules of both shipped products; the parked block is
//! no longer the decode plane either, but the 224,000 B band above. So MRMS's
//! share of this family is a small fraction of what that table prices, and the
//! parked bytes worth taking are **GMGSI's**, which did not move.
//!
//! Measured on a 420 s six-pane HEAVY6 leg (2026-09-10), through the
//! `overlay grids` census family's own `parked` term: 14,997,000 B of GMGSI
//! against 224,000 B of MRMS.
//!
//! **What is parked is a block nothing reads.** It is not dead — the next
//! decode takes it — but a still leg decodes MRMS about once every two minutes,
//! so the block is held for two minutes to save one allocation. Measured on
//! this workspace's box (`std::alloc::System`, twenty samples), filling a
//! 49,000,000 B mosaic cost 46.04 ms p50 from a fresh allocation against
//! 26.90 ms p50 from a retained one: **19.15 ms per decode**, which against a
//! two-minute poll is 0.016 % of one thread.
//!
//! **That timing is the plane's and is kept only as the shape of the trade.**
//! It was measured filling a 49,000,000 B block; MRMS now parks 224,000 B and
//! the figure does not transfer to it. What it still says correctly is that the
//! saving is a fraction of a percent of one thread, which is what makes a
//! parked block worth giving up at all.
//!
//! # Two halves, because one of them moves an average and not a peak
//!
//! Releasing the parked block on its own is not enough. The slot refills the
//! moment the next granule displaces the one before it, so on a still MRMS leg
//! the block is gone for about eight seconds out of every two-minute poll and
//! a census sampling every two seconds still catches a parked mosaic: the
//! family's *high-water mark*, which is the figure a memory target is read
//! against, would not move at all. So a trim does both — takes the block **and
//! turns parking off** (`StagingPool::set_retaining`) — and a still session
//! parks nothing more until a busy reading turns it back on.
//!
//! `App::on_pressure`'s `release_all_retained` deliberately does **not** touch
//! the flag: that lever is a one-shot under a wall, and its own doc says the
//! slots are meant to refill from the next decode so a second event two minutes
//! later has a real block to take again.
//!
//! # The signal
//!
//! One fact, and it is not a new clock:
//! [`squallar_overlays::staging::decodes_served`] is a monotonic count of the
//! decodes the pools have handed a buffer to — served out of the slot or out
//! of the allocator, so it counts *decodes* and not the pool's luck at serving
//! them. Two readings that agree mean no granule was decoded in the interval.
//!
//! # Why four readings
//!
//! The reading is the telemetry tick, which rides a frame and so is *at least*
//! `RASTER_TELEMETRY_PERIOD` (2 s) apart and can be further. Four readings is
//! therefore a floor of eight seconds and never a ceiling, which is the safe
//! direction — the same count and the same reasoning as
//! [`crate::render_pool_trim`], which measures its four readings off the same
//! tick.
//!
//! Eight seconds is longer than any burst of decodes one user action produces,
//! and it is two orders of magnitude under the cadence of the next legitimate
//! decode: MRMS publishes about every two minutes and GMGSI hourly. A loop
//! *building* its frames decodes back to back, so the count moves on every
//! tick and the blocks are never taken out from under a loop that is staging;
//! a loop that has finished staging plays from its textures and holds no
//! decode at all.
//!
//! # What it costs
//!
//! **One allocation per decode while the trim holds, and nothing else.** No refetch, no
//! re-raster, no picture leaves the glass, and nothing about what is drawn
//! changes: a decode handed a fresh buffer produces the same grid as one handed
//! a parked buffer, which is what `StagingPool::take`'s fresh arm has always
//! done for the first decode of every session. A **loop** pays it exactly once:
//! its first frame decodes un-pooled, the tick after that reads Busy, and every
//! frame from there is served out of the slot as before.
//!
//! The free itself is 0.218–0.467 ms for both blocks (twelve samples, 64,000,000
//! B, `std::alloc::System`, this box under load) — **measured when the two
//! blocks summed to 64,000,000 B; they sum to 15,224,000 B nominal and read
//! 15,221,000 B parked on the HEAVY6 leg above**, so that range is an upper
//! bound rather than the current reading. Small — and still handed to
//! `squallar_worker::offload`'s free lane rather than spent on an interaction
//! frame, priced so `deferred drops` carries the bytes for the whole of the
//! hand-off.
//!
//! # Why not on wasm32
//!
//! [`TRIM_AFTER`] is `None` there, and the reason is not caution:
//!
//! * **The bytes do not come back.** A wasm linear memory never shrinks, so
//!   freeing a block moves it from `live_bytes` into the page's freed-but-
//!   reserved headroom and `byteLength` — the ceiling that actually kills the
//!   tab — does not move by one byte.
//! * **And the block might not be re-findable.** dlmalloc cannot coalesce
//!   across a live block; a 49 MB hole released and then re-requested with a
//!   loop's textures allocated into it in between is exactly the failure the
//!   pools were built for (`squallar_overlays::mrms::staging` carries that
//!   account: a 98 MB request refused with 192 MB free).
//!
//! So on wasm32 an idle trim is all cost and no gain, which is the case
//! `StagingPool::release_retained`'s own doc rules out when it says a short
//! idle threshold re-introduces an allocate-and-free per poll "on a heap that
//! cannot coalesce". A native heap coalesces, and a 49,000,000 B request is
//! past glibc's mmap threshold, so there the block is returned to the operating
//! system rather than to a free list. Selecting a value per target, never
//! forking behaviour inside a function.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};

/// Consecutive quiet readings before the blocks are given up, or `None` on a
/// target where the trim buys nothing. See the module header for both.
#[cfg(target_arch = "wasm32")]
pub const TRIM_AFTER: Option<u32> = None;
/// See the wasm arm.
#[cfg(not(target_arch = "wasm32"))]
pub const TRIM_AFTER: Option<u32> = Some(4);

/// What [`observe_reading`] does with one reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    /// A grid was decoded in the interval: the count restarts, and a session
    /// that had already trimmed is armed again.
    Busy,
    /// Quiet, but not for long enough yet. Carries how many consecutive quiet
    /// readings have now been seen.
    Quiet(u32),
    /// Quiet for [`TRIM_AFTER`] readings, and this is the one that gives the
    /// blocks up.
    Trim,
    /// Quiet, and already trimmed: nothing is left to give up until a decode
    /// runs. Also what a target with no trim reports once it has gone quiet.
    Settled,
}

/// The count that stands for "trimmed, waiting for a decode". Distinct from
/// every real count, so [`decide`] stays a total function of its arguments.
const SETTLED: u32 = u32::MAX;

/// The reading of [`squallar_overlays::staging::decodes_served`] this module
/// last saw, and how many consecutive quiet readings have followed it.
///
/// Statics rather than fields on `App`, because what they track is a static:
/// the pools are process-global, one set per module instantiation, and an
/// `App` is not the thing that owns them.
/// **Per pool, and that is the whole of this policy's correctness.** A single
/// pair of these read [`squallar_overlays::staging::decodes_served`], the SUM
/// over both pools — so MRMS, which stages a granule per loop frame and
/// re-polls every 120 s, moved the count on essentially every tick, every
/// reading was [`Reading::Busy`], and GMGSI's 15,000,000 B block was never
/// given up on any scene where MRMS was live. Measured on a 420 s six-pane
/// HEAVY6 leg: the `overlay grids` census family's `parked` term sat at
/// 15,221,000 B across every sample of the leg and never once fell.
///
/// Indexed by [`squallar_overlays::staging::Pool::index`].
static LAST_SERVED: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
static QUIET: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

/// **The fires counter, per pool** — trims that fired, blocks given up, and
/// bytes given up, plus the readings on both sides of the decision.
///
/// Blocks and bytes are **different currencies and are never added**: one
/// block is one pool's parked buffer whatever it is holding, and a pool that
/// has moved off its nominal shape is priced at what it actually held.
///
/// `busy` and `quiet` are the denominator this policy is rare or common
/// against. Without them a `trims 0` row cannot distinguish "the mechanism
/// never fired" from "the scene never went quiet", which is the distinction
/// that would have caught this defect the day the trim landed.
static TRIMS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
static TRIM_BLOCKS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
static TRIM_BYTES: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
static BUSY_READINGS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
static QUIET_READINGS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

/// What this policy has given back, per pool: `(trims, blocks, bytes, busy,
/// quiet)` — see the statics above for why blocks and bytes are apart.
pub fn trim_totals(pool: squallar_overlays::staging::Pool) -> (u64, u64, u64, u64, u64) {
    let i = pool.index();
    (
        TRIMS[i].load(Relaxed),
        TRIM_BLOCKS[i].load(Relaxed),
        TRIM_BYTES[i].load(Relaxed),
        BUSY_READINGS[i].load(Relaxed),
        QUIET_READINGS[i].load(Relaxed),
    )
}

/// **What one reading means**, as a function of its inputs alone.
///
/// `served` and `last_served` are two readings of the monotonic decode count;
/// `quiet` is the count of consecutive quiet readings behind them, or
/// [`SETTLED`]. `trim_after` is [`TRIM_AFTER`], passed in so the policy is one
/// function on every target and both arms are testable from either.
///
/// Split out so the policy is testable without a process that decodes grids:
/// the stateful half is two atomics around this.
fn decide(served: u64, last_served: u64, quiet: u32, trim_after: Option<u32>) -> Reading {
    if served != last_served {
        return Reading::Busy;
    }
    let Some(threshold) = trim_after else {
        return Reading::Settled;
    };
    match quiet {
        SETTLED => Reading::Settled,
        n if n + 1 >= threshold => Reading::Trim,
        n => Reading::Quiet(n + 1),
    }
}

/// Fold one reading in, and answer what it meant.
///
/// **Call it once a telemetry tick, from the frame thread.** Once a tick
/// because that is the clock the count is denominated in; from the frame thread
/// because a [`Reading::Trim`] hands the blocks to
/// `squallar_worker::offload::discard`, whose deferred queue is thread-local
/// and drained by the frame loop.
pub fn observe_reading() -> Reading {
    // **Each pool asked with its OWN count**, never the sum: a summed count
    // answers "did anything decode", and this policy is asking "may THIS
    // block go". See [`LAST_SERVED`].
    let mut worst = Reading::Settled;
    for pool in squallar_overlays::staging::Pool::ALL {
        let reading = observe_served_of(pool, squallar_overlays::staging::decodes_served_of(pool));
        // Busy on any pool is what a single-`Reading` caller should hear, so
        // an existing reader of this answer keeps its old meaning: "is the
        // grid-decode side of this session working".
        if reading == Reading::Busy {
            worst = Reading::Busy;
        } else if worst != Reading::Busy && reading == Reading::Trim {
            worst = Reading::Trim;
        }
    }
    worst
}

/// [`observe_reading`] with the count supplied — the stateful fold and the
/// release, over a reading a caller made.
///
/// Split out for the reason [`decide`] is: a test that had to move the real
/// pools' decode count to drive this would be driving process-global state
/// another test in the binary also writes, and a race is a regression rather
/// than a flake.
///
/// `pub` so that suite can be its **own test binary**: the shipped slots are
/// process-global, so a test that parks a block in one cannot tell its own
/// block from one another test in the same binary left behind — the reason
/// `squallar-overlays/tests/overlay_grid_residency_split.rs` is one test in one
/// binary too.
pub fn observe_served_of(pool: squallar_overlays::staging::Pool, served: u64) -> Reading {
    let i = pool.index();
    let reading = decide(
        served,
        LAST_SERVED[i].load(Relaxed),
        QUIET[i].load(Relaxed),
        TRIM_AFTER,
    );
    LAST_SERVED[i].store(served, Relaxed);
    QUIET[i].store(
        match reading {
            Reading::Busy => 0,
            Reading::Quiet(n) => n,
            Reading::Trim | Reading::Settled => SETTLED,
        },
        Relaxed,
    );
    match reading {
        Reading::Busy => {
            BUSY_READINGS[i].fetch_add(1, Relaxed);
        }
        _ => {
            QUIET_READINGS[i].fetch_add(1, Relaxed);
        }
    }
    match reading {
        // **Parking back on.** A cadence the pools are worth something to has
        // come back, and the block they park will be reused by the decode
        // after this one. Set on every busy reading rather than on the edge:
        // it is one relaxed store per pool per tick and a level cannot drift
        // out of step with the policy the way an edge can.
        Reading::Busy => squallar_overlays::staging::set_retaining_of(pool, true),
        Reading::Trim => release(pool),
        Reading::Quiet(_) | Reading::Settled => {}
    }
    reading
}

/// Empty the slots and hand what came out to the free lane.
///
/// Priced at what the blocks hold, so `deferred drops` carries the bytes from
/// the instant they stop being `overlay grids`' until the lane has freed them —
/// a reader watching `live_bytes` fall sees one family's figure move to
/// another's rather than a gap. Nothing is filed for an empty take: a payload
/// of two `None`s is a queue entry that frees nothing.
fn release(pool: squallar_overlays::staging::Pool) {
    // **Both halves, and the order matters.** Parking off first, so a decode
    // racing this cannot park a block into a slot the take below has already
    // passed; the two are one relaxed store apiece and neither blocks.
    //
    // Turning parking off is what makes this move a *peak* rather than an
    // average. The block alone comes back the moment the next granule is
    // displaced into the slot — about eight seconds after each two-minute MRMS
    // poll — and a census sampling every two seconds catches that, so the
    // family's high-water mark would not move. Held off, a still session parks
    // nothing at all from here until a busy reading turns it back on, which
    // costs a loop exactly one un-pooled decode at its start.
    squallar_overlays::staging::set_retaining_of(pool, false);
    let taken = squallar_overlays::staging::take_retained_of(pool);
    let bytes = taken.bytes();
    // **Counted before the empty exit**, so a trim that decided to fire and
    // found nothing is still a fired trim. A counter that only counted the
    // productive ones could not tell a policy that never runs from one that
    // runs and has nothing to give.
    let i = pool.index();
    TRIMS[i].fetch_add(1, Relaxed);
    if bytes == 0 {
        return;
    }
    TRIM_BLOCKS[i].fetch_add(1, Relaxed);
    TRIM_BYTES[i].fetch_add(bytes, Relaxed);
    log::debug!(
        "grid staging pool {}: giving up {bytes} B after a quiet session",
        pool.name()
    );
    squallar_worker::offload::discard(
        "grid staging pools",
        squallar_worker::offload::Priced::new(bytes, taken),
    );
}

/// **The fires-counter row for this policy** — per pool, always emitted,
/// all-zero included.
///
/// A row of zeros is a policy that ran and never fired; **no row at all** is a
/// binary without the policy. Those are different findings and this must not
/// merge them — which is exactly the confusion that let this trim sit unable
/// to fire, since it shipped with no row of any kind.
///
/// `trims` counts every decision to fire, including one that found an empty
/// slot; `blocks` and `bytes` count what came out. Blocks and bytes are
/// different currencies and are never added.
pub fn trim_line(instance: &str) -> String {
    use core::fmt::Write;

    let mut out = String::new();
    let _ = write!(out, "grid pool trim ({instance}):");
    for (n, pool) in squallar_overlays::staging::Pool::ALL
        .into_iter()
        .enumerate()
    {
        let (trims, blocks, bytes, busy, quiet) = trim_totals(pool);
        let held = squallar_overlays::staging::retained_bytes_of(pool);
        let _ = write!(
            out,
            "{} {} {trims} trims, {blocks} blocks, {bytes} B given, {held} B held, \
             {busy} busy, {quiet} quiet",
            if n == 0 { "" } else { ";" },
            pool.name(),
        );
    }
    out
}

#[cfg(test)]
mod tests;
