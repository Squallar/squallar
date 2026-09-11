//! **What the frame's arrival allowance actually did** — the counters behind
//! [`squallar_device_profile::constants::INGEST_BUDGET_PER_FRAME`], and the
//! fires counter it shipped without.
//!
//! # Why this exists
//!
//! [`crate::action_budget`] is this budget's younger sibling and it carries a
//! `bites` counter for a reason it states plainly: a budget whose precondition
//! never holds delivers exactly zero and looks landed. The older budget — the
//! one `PumpPhase::Ingest` runs under, the one the `pre_ingest` cut of the
//! `pre` segment is made of — has never had one. Nothing in the tree could say
//! whether it had ever stopped a drain, how many arrivals a frame applies, or
//! how much of the phase is the rows the budget does not reach.
//!
//! # The denominators — six, and they are never added
//!
//! * [`Totals::phases`] is **`Ingest` phases run**: one per `poll_data_channels`
//!   call, which is one per frame that got past `poll_platform_state`. The
//!   denominator of every other figure here, and of nothing else in the tree —
//!   it is NOT presented frames (`frame_ledger`'s denominator), because a frame
//!   that takes one of `handle_redraw`'s three early exits runs this phase and
//!   is never presented.
//! * [`Totals::arrivals`] is **arrival messages taken off a channel and
//!   handled on the frame thread**, summed over the phase's four
//!   `try_recv_arrival` drains. The pass's own rate. Said that way rather than
//!   "applied" on purpose: a message the drain then discards as stale is
//!   counted, because the frame paid for taking and judging it, and the
//!   drains' own staleness arms are where several of them return early. And a
//!   count of MESSAGES, never of bytes: a chunk round and a 49 MB mosaic are
//!   one apiece here.
//! * [`Totals::bites`] is **phases on which the budget stopped at least one
//!   drain.** This is the fires counter. A leg whose `bites` is zero paid
//!   nothing for this mechanism and saved nothing by it, whatever its
//!   `pre_ingest` figures say.
//! * [`Totals::stops`] is **drain-stops**, summed over those bites, and it is
//!   the figure that says what the bound really is. Each drain keeps its own
//!   always-one-arrival guarantee, so a phase can stop once per drain and each
//!   stop costs one whole arrival past the deadline. `stops` above `bites` is
//!   a phase whose real spend was the budget plus *several* arrivals, not the
//!   budget plus one that `INGEST_BUDGET_PER_FRAME`'s own note describes.
//! * [`Totals::builds`] is **3D volume payloads extracted on the frame
//!   thread** by the arrival dispatch — `App::prepare_volume`'s
//!   `extract_*_volume`, which walks a product's moments out of the merged
//!   volume and logs its own duration in milliseconds. One per pane whose
//!   volume arrived, and the one item in this phase that is heavy work by
//!   `CLAUDE.md`'s meaning rather than an apply.
//! * [`Totals::held_builds`] is **those the budget turned away.** Never
//!   dropped and never queued here: an ask that is held sets no
//!   `rendered_for`, so the pane's draw-time level trigger asks again on the
//!   next frame through `process_gui_actions` — the documented fallback
//!   `App::dispatch_arrived_volumes` already relies on when the render budget
//!   refuses one.
//!
//! # Testing these counters
//!
//! `squallar_egui::release_ledger`'s note applies verbatim, and
//! [`crate::action_budget`] repeats it: they are process-global `static`s
//! written by a live frame driver, so a test wanting an exact difference needs
//! a process with no frame driver in it.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// `Ingest` phases run. See the module doc.
static PHASES: AtomicU64 = AtomicU64::new(0);
/// Arrival messages taken off a channel on the frame thread, across the
/// phase's drains.
static ARRIVALS: AtomicU64 = AtomicU64::new(0);
/// Phases on which the budget stopped at least one drain — the fires counter.
static BITES: AtomicU64 = AtomicU64::new(0);
/// Drain-stops, summed over those phases.
static STOPS: AtomicU64 = AtomicU64::new(0);
/// 3D volume payloads extracted on the frame thread by the arrival dispatch.
static BUILDS: AtomicU64 = AtomicU64::new(0);
/// Arrival-dispatch builds the budget turned away.
static HELD_BUILDS: AtomicU64 = AtomicU64::new(0);

/// A reading of every counter, taken together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Totals {
    /// `Ingest` phases run.
    pub(crate) phases: u64,
    /// Arrival messages taken off a channel and handled on the frame thread.
    pub(crate) arrivals: u64,
    /// Phases on which the budget stopped at least one drain.
    pub(crate) bites: u64,
    /// Drain-stops, summed over those phases.
    pub(crate) stops: u64,
    /// Volume payloads extracted on the frame thread by the arrival dispatch.
    pub(crate) builds: u64,
    /// Arrival-dispatch builds the budget turned away.
    pub(crate) held_builds: u64,
}

impl Totals {
    /// How far along this ledger is, so a caller can tell "nothing has
    /// happened since I last looked" in one compare.
    ///
    /// **Never `phases`, and never `arrivals`.** The first moves on every
    /// frame of every session and the second on every frame of a session that
    /// is downloading anything, so folding either in would make the sentence
    /// speak on every telemetry tick and say nothing —
    /// [`crate::action_budget::Totals`]' argument about `handled`, verbatim.
    /// `builds` is in because a whole-volume extract on the frame thread is
    /// rare, is exactly what this ledger exists to expose, and a reader who
    /// never sees one needs to know that rather than assume it.
    fn progress(self) -> u64 {
        self.bites
            .wrapping_add(self.stops)
            .wrapping_add(self.builds)
            .wrapping_add(self.held_builds)
    }
}

/// Read every counter. Six [`Relaxed`] loads, not an atomic snapshot.
pub(crate) fn totals() -> Totals {
    Totals {
        phases: PHASES.load(Relaxed),
        arrivals: ARRIVALS.load(Relaxed),
        bites: BITES.load(Relaxed),
        stops: STOPS.load(Relaxed),
        builds: BUILDS.load(Relaxed),
        held_builds: HELD_BUILDS.load(Relaxed),
    }
}

/// The last [`Totals::progress`] a caller was handed by [`totals_if_moved`].
static REPORTED: AtomicU64 = AtomicU64::new(0);

/// [`totals`], but only when the budget or the arrival dispatch has done
/// something since the last time this was asked. `None` is the reading of a
/// session that never overran a frame's arrival allowance and never built a
/// volume from an arrival — and that silence is itself the reading.
pub(crate) fn totals_if_moved() -> Option<Totals> {
    let totals = totals();
    let progress = totals.progress();
    if REPORTED.swap(progress, Relaxed) == progress {
        return None;
    }
    Some(totals)
}

/// Open an `Ingest` phase, handing back the stop count as it stood so
/// [`close_phase`] can tell whether this phase was one of the ones that bit.
///
/// A reading rather than a per-phase flag on the `App`: the drains that write
/// [`note_stop`] are free functions' worth of code spread over three files,
/// and a flag would have to be threaded through every one of them to answer a
/// question two loads already answer.
///
/// **Exact because every writer of [`note_stop`] is the frame thread**, inside
/// the walk these two bracket. A second writer anywhere would make the
/// difference this compares a difference between two threads' work and `bites`
/// would count something other than frames — so a drain moved off this thread
/// must take its `note_stop` with it.
pub(crate) fn open_phase() -> u64 {
    STOPS.load(Relaxed)
}

/// Close the phase [`open_phase`] opened.
///
/// `bites` is incremented at most once here however many drains stopped, which
/// is what keeps it a count of FRAMES; `stops` is the per-drain figure and the
/// two are never added.
pub(crate) fn close_phase(stops_at_open: u64) {
    PHASES.fetch_add(1, Relaxed);
    if STOPS.load(Relaxed) != stops_at_open {
        BITES.fetch_add(1, Relaxed);
    }
}

/// Record one arrival message taken off a channel on the frame thread.
pub(crate) fn note_arrival() {
    ARRIVALS.fetch_add(1, Relaxed);
}

/// Record one drain stopped by the budget. Called once per drain that stopped,
/// not once per frame — see [`Totals::stops`].
pub(crate) fn note_stop() {
    STOPS.fetch_add(1, Relaxed);
}

/// Record one 3D volume payload extracted on the frame thread.
pub(crate) fn note_build() {
    BUILDS.fetch_add(1, Relaxed);
}

/// Record one arrival-dispatch build the budget turned away.
pub(crate) fn note_held_build() {
    HELD_BUILDS.fetch_add(1, Relaxed);
}
