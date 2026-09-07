//! **When a session that has stopped rendering gives its render buffers back.**
//!
//! `squallar_radar::render` keeps one cell buffer, one RGBA texture and one
//! value grid between plan-view renders, and decides whether to keep each by
//! weighing it against the demand of the renders before it. That rule runs on
//! a checkout, so it can only fire while renders are happening. A session
//! whose last render was a large one and that then goes quiet is the case it
//! cannot see: no checkout arrives to weigh anything, and the decaying maximum
//! behind the rule cannot decay either, because its generation only rolls on a
//! render.
//!
//! Measured on the campaign's floor legs (`FLOOR.f1`, `FLOOR.f2`, which agree
//! figure for figure): one volume arrival, four completed plan-view renders at
//! side 7362 in two pairs — t ≈ 2 s and t ≈ 167 s on f1, t ≈ 2 s and t ≈ 155 s
//! on f2 — and then nothing for the remaining ~250 s. The heap census read
//! `render pools` at 867,184,704 B — 7362² × 16 — on 99 of its 100 ticks, the
//! hundredth being the one before the first render. 827 MiB held for the life
//! of the process on a scene that had finished drawing, and the second pair of
//! renders did not shift it: four renders is under the eight
//! `DEMAND_WINDOW_RENDERS` a generation roll needs, so the maximum behind the
//! retention rule never decayed either.
//!
//! # The signal
//!
//! Two facts, neither of them a new clock:
//!
//! * **`squallar_radar::render::renders_begun()`** — a monotonic count of the
//!   plan-view renders this instance has started. Compared against the previous
//!   reading, an unchanged count is "nothing was dispatched in that interval".
//! * **[`RenderDispatcher::in_flight_image_bytes`]** — the level the census
//!   publishes as `renders in flight`, which the telemetry tick already folds.
//!   Zero is "no reply's raster is outstanding".
//!
//! [`RenderDispatcher::in_flight_image_bytes`]: crate::render_dispatch::RenderDispatcher::in_flight_image_bytes
//!
//! # Why four readings
//!
//! The reading is the telemetry tick, which rides a frame and so is *at least*
//! `RASTER_TELEMETRY_PERIOD` (2 s) apart and can be further — on the floor leg
//! the quiet ticks were 2–10 s apart. Four readings is therefore a floor of
//! eight seconds and never a ceiling, which is the safe direction: the window
//! can only be longer than the count suggests, never shorter. It is the same
//! count and the same reasoning as [`crate::recovery`]'s dwell, which measures
//! its four readings off the same tick.
//!
//! Eight seconds is longer than any burst of renders one user action produces —
//! a pane split, a product change or a tilt change dispatches its renders inside
//! a frame or two — and two orders of magnitude under the volume cadence that is
//! the next legitimate render, which is 4–6 minutes for every VCP the network
//! flies. So a session between volumes gives the buffers back and pays one
//! re-allocation when the next volume lands.
//!
//! # What it costs
//!
//! **A cache miss, and nothing else.** The next render after a trim allocates
//! its buffers instead of resizing pooled ones; measured on the same scene and
//! the same allocator, building all three at side 7362 is 17.1–44.3 ms, and it
//! happens on the render thread, never on the frame thread. Nothing about the
//! raster changes: every slot hands out a buffer resized to exactly what the
//! render asked for, whether it came from the pool or from the allocator.
//!
//! The free itself is 18.4–31.7 ms of one thread (twelve samples, 867,184,704 B
//! in three buffers, `std::alloc::System`, idle box) and so may not land on the
//! frame thread at any cadence this application aims at. It goes to
//! `squallar_worker::offload`'s free lane, priced so `deferred drops` carries
//! the bytes for the whole of the hand-off.
//!
//! **A render still running when the window closes** — one that began before
//! the last reading that saw the count move and is still going four readings
//! later — costs a cache miss and nothing more either. Its cell buffer is
//! already checked out, so the trim cannot take it; the texture and value slots
//! it will ask for at `into_output` are taken, so it allocates instead; and the
//! `demand::forget` behind the trim can only decline its buffers on the way
//! back, which frees them on the reply thread. Its own `carry_ceiling_px` was
//! read before any of this and is untouched, so nothing it draws changes.
//!
//! # The web gap
//!
//! On the browser the page and the rasterization worker are separate module
//! instances with separate pools, and the worker holds the larger — it is where
//! the rasterizing happens. This trim runs on the frame thread, so it reaches
//! the page instance's pools and not the worker's. The worker is message-driven
//! with its job queue held by the event loop, so "no job pending" is not a fact
//! it can observe about itself, and nothing here closes that. The worker's
//! `render pools` figure is unchanged by this module.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering::Relaxed};

/// Consecutive quiet readings before the pools are given up. See the module
/// header for why four.
pub const QUIET_READINGS: u32 = 4;

/// What [`observe_reading`] does with one reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    /// A render was dispatched or a reply was outstanding: the count restarts,
    /// and a session that had already trimmed is armed again.
    Busy,
    /// Quiet, but not for long enough yet. Carries how many consecutive quiet
    /// readings have now been seen.
    Quiet(u32),
    /// Quiet for [`QUIET_READINGS`] readings, and this is the one that gives
    /// the buffers up.
    Trim,
    /// Quiet, and already trimmed: nothing is left to give up until a render
    /// begins.
    Settled,
}

/// The count that stands for "trimmed, waiting for a render". Distinct from
/// every real count, so [`decide`] stays a total function of two integers.
const SETTLED: u32 = u32::MAX;

/// The reading of [`squallar_radar::render::renders_begun`] this module last
/// saw, and how many consecutive quiet readings have followed it.
///
/// Statics rather than fields on `App`, because what they track is a static:
/// the pools are process-global, one set per module instantiation, and an
/// `App` is not the thing that owns them.
static LAST_BEGUN: AtomicUsize = AtomicUsize::new(0);
static QUIET: AtomicU32 = AtomicU32::new(0);

/// **What one reading means**, as a function of its inputs alone.
///
/// `begun` and `last_begun` are two readings of the monotonic render count;
/// `quiet` is the count of consecutive quiet readings behind them, or
/// [`SETTLED`]. `replies_quiet` is whether the dispatcher's in-flight raster
/// level is zero.
///
/// Split out so the policy is testable without a process that renders: the
/// stateful half is three atomics around this.
fn decide(begun: usize, last_begun: usize, quiet: u32, replies_quiet: bool) -> Reading {
    if begun != last_begun || !replies_quiet {
        return Reading::Busy;
    }
    match quiet {
        SETTLED => Reading::Settled,
        n if n + 1 >= QUIET_READINGS => Reading::Trim,
        n => Reading::Quiet(n + 1),
    }
}

/// Fold one reading in, and answer what it meant.
///
/// **Call it once a telemetry tick, from the frame thread.** Once a tick
/// because that is the clock the count is denominated in; from the frame
/// thread because a [`Reading::Trim`] hands the buffers to
/// `squallar_worker::offload::discard`, whose deferred queue is thread-local
/// and drained by the frame loop.
///
/// `replies_quiet` is the dispatcher's own answer, passed in rather than read
/// here: this module does not own a dispatcher and must not reach for one.
pub fn observe_reading(replies_quiet: bool) -> Reading {
    let begun = squallar_radar::render::renders_begun();
    let reading = decide(
        begun,
        LAST_BEGUN.load(Relaxed),
        QUIET.load(Relaxed),
        replies_quiet,
    );
    LAST_BEGUN.store(begun, Relaxed);
    QUIET.store(
        match reading {
            Reading::Busy => 0,
            Reading::Quiet(n) => n,
            Reading::Trim | Reading::Settled => SETTLED,
        },
        Relaxed,
    );
    if reading == Reading::Trim {
        release();
    }
    reading
}

/// Empty the slots and hand what came out to the free lane.
///
/// Priced at what the buffers hold, so `deferred drops` carries the bytes from
/// the instant they stop being `render pools`' until the lane has freed them —
/// a reader watching `live_bytes` fall sees one family's figure move to
/// another's rather than a gap. Nothing is filed for an empty take: a payload
/// of three `None`s is a queue entry that frees nothing.
fn release() {
    let taken = squallar_radar::render::take_pools();
    let bytes = taken.bytes() as u64;
    if bytes == 0 {
        return;
    }
    log::debug!("render pools: giving up {bytes} B after a quiet session");
    squallar_worker::offload::discard(
        "render pools",
        squallar_worker::offload::Priced::new(bytes, taken),
    );
}

#[cfg(test)]
mod tests;
