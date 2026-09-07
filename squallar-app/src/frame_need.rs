//! **Frames drawn, against frames that needed drawing** — and what the ones
//! that did not are charged to.
//!
//! Every other instrument in this tree measures work *done*. The command
//! stream counts what a frame recorded; [`crate::frame_ledger`] times the
//! segments of frames that ran; `RerenderReason` charges overlay rasters to
//! the arm that asked for them. All three read a healthy zero on a frame that
//! should never have been drawn at all, so a permanently repainting map, a map
//! repainting on a value that did not change, and an application that never
//! goes idle are invisible to all of them — and are exactly what a user feels
//! as heat and battery.
//!
//! Two instances, both from one night in September 2026:
//!
//! * the web build was reported re-rendering tiles and redrawing alerts on the
//!   smallest pan;
//! * the viewport clamp of `0e2161f4` nearly shipped without a deadband. The
//!   centre is stored geographically, so re-reading it through a projection
//!   costs about `1e-10` points; a map resting against a bound would have been
//!   out of bounds **every frame**, moved a ten-billionth of a point and asked
//!   for a repaint forever. A counts-only instrument would have seen nothing.
//!
//! A third was already in the tree and documented before this existed:
//! `TouchGestures::pan_stranded` — egui clears neither `dragged` nor
//! `pointer.down` on an `Event::PointerGone`, so a pointer that leaves
//! mid-drag leaves walkers reading a live drag with a zero delta on every
//! frame that follows, "and `Map::show` turns the resulting `true` into a
//! `request_repaint` forever, with the map stationary".
//!
//! # The denominator, and why it is not circular
//!
//! **The trap is that the easy denominator agrees with the defect.** Take
//! "egui asked for a repaint", or "something called `request_repaint`", and
//! the self-nudging map above scores **100 % necessary**: it genuinely did
//! request every one of the frames it wasted. Any denominator built from the
//! repaint ask is a denominator the defect satisfies.
//!
//! So necessity is assembled from **causes**, in
//! [`squallar_egui::frame_need`], each raised at the site where a change
//! actually happens and none of them reachable from a repaint request — an
//! input event read off the raw input egui is about to be handed, a message
//! taken off a channel, a tile body handled, an animation factor strictly
//! between its endpoints, a surface rebuilt, a clock-restated string printing
//! different words from the ones it last printed. A frame that raises one of
//! those needed drawing. A frame that raises none is one the application drew
//! for its own reasons with nothing to show.
//!
//! The repaint ask is used, and only here: to say **who**. [`WakeClaim`] is
//! the attribution, on `RerenderReason`'s terms — a bare percentage nobody can
//! act on is a failure, and "43 % of your frames were unnecessary and 91 % of
//! those were an immediate repaint nobody had a reason for" is a place to
//! look. Attribution can be wrong about who asked; what it cannot do is make
//! an unnecessary frame read as necessary, because it is not in the verdict.
//!
//! # Denominator, stated once
//!
//! [`Reading::drawn`] is **every presented frame** — the same population
//! [`crate::frame_ledger::WorstFrame`] latches over, interact and idle alike,
//! and not the interact-only one the segment histograms use. A frame that
//! early-returned (minimized, zero area, a lost surface) drew nothing and is
//! in none of these figures; the causes it raised are still standing for the
//! frame that finally draws them, which is why the register is cleared by the
//! verdict rather than at `handle_redraw`'s entry.
//!
//! # Cost
//!
//! Per presented frame: one relaxed `swap`, one `u32` popcount-free bit test,
//! and between three and six `u64` increments. Per cause, on the frames where
//! one happens: one relaxed `fetch_or`. **No clock read and no bin search** —
//! the counts [`crate::frame_ledger`]'s module doc pins are untouched.
//!
//! The wake claim is all but free: the predicates are the ones
//! `handle_redraw` already asks to decide whether to post a redraw, asked in
//! the same order and with the same short-circuit, so a frame stops at the
//! first one that stands. The one addition is [`WakeClaim::Upload`], a
//! `VecDeque::is_empty` behind an `Option`, asked first — the renderer had
//! already acted on that fact when it zeroed the frame's repaint delay, and
//! this only reads it back.

use squallar_egui::frame_need::NeedCause;

/// **Who kept the application awake for a frame that needed nothing** — the
/// attribution, on [`squallar_egui::overlay_cache::RerenderReason`]'s terms.
///
/// # Exactly one per unnecessary frame
///
/// Several claims can stand at once; this names the **first** in the order
/// `handle_redraw`'s tail asks them, which is the order that already decides
/// whether it posts a redraw. That makes [`Reading::charges_balance`] an
/// identity rather than a hope, and it makes the choice a documented
/// convention rather than an inference: a frame charged to [`Self::Upload`]
/// may also have had a render in flight and a raster held.
///
/// # How to read a charge
///
/// [`Self::Upload`] is a frame the application owes itself and cannot avoid:
/// the bands have to move and only a frame moves them. Read it as a cost, not
/// as waste to remove; it is first so that the claims below it are not
/// credited with frames it had already bought.
///
/// The next seven are the app's own standing claims — "there is work in
/// flight, wake me". An unnecessary frame charged to one of them is a **poll
/// that found nothing**: the answer had not arrived, and the frame that
/// finally receives it is necessary and counted so. A steady rate there is
/// the app spinning on a worker instead of being woken by one — and where the
/// worker already posts a redraw when it answers, that is a poll with no
/// reason to exist at all, which is what took `chunk_feeds.any_in_flight()`
/// out of [`Self::Chunk`].
///
/// [`Self::EguiNow`] is different in kind and is the self-nudge shape: a
/// widget asked egui for an immediate repaint during the pass, and the frame
/// it bought had nothing to show. Both instances this instrument was built
/// for land here.
///
/// [`Self::External`] is the app claiming nothing at all — a frame the
/// platform delivered anyway (a compositor expose, an occlusion change, a
/// foreign wake). Waste that is not ours to stop, and separated for exactly
/// that reason.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum WakeClaim {
    /// `state.egui_renderer.uploads_pending()` — the texture upload queue
    /// still holds bands.
    ///
    /// **First in the order, and that placement is the point.** A banded
    /// upload spends several frames by design, and it buys them itself: the
    /// renderer overrides the frame's repaint delay to zero while bands
    /// remain (`squallar_gpu::egui_renderer::EguiRenderer::end_pass_and_upload`),
    /// so the frame happens whatever every claim below says. Asked last, it
    /// would take only the frames nothing else was standing for, and every
    /// claim above it would read as buying frames that were already bought —
    /// which is exactly how removing one of them would look like a saving and
    /// be a relabelling. Asked first, what the claims below it hold is what
    /// removing them can actually recover.
    Upload,
    /// `render.any_render_in_flight()`.
    Render,
    /// `gui.any_loop_active()`.
    Loop,
    /// `gui.any_raster_held()`.
    Hold,
    /// `restore_pending` — a deferred restore nothing else speaks for.
    Restore,
    /// `chunk_notify.handshake_pending()` — a push socket that is connecting
    /// or reconnecting.
    ///
    /// **The socket alone.** This used to be `chunk_feeds.any_in_flight() ||
    /// chunk_notify.handshake_pending()`, on the ground that they are the same
    /// feed seen at two points. They are not the same *wake*: a round in
    /// flight is answered by a worker that posts a redraw of its own when it
    /// sends, and re-arming for it as well made every frame of a round's
    /// latency a frame with nothing to show. The handshake has no such
    /// producer — it is driven by `drive_chunk_notifications`, from a frame —
    /// so it is a claim and the round was a poll.
    Chunk,
    /// `squallar_worker::offload::has_deferred_drops()`.
    Drops,
    /// An armed gesture player — a hand that never lifts, whose next frame's
    /// events exist only if a next frame comes.
    Gesture,
    /// egui asked for an **immediate** repaint (`RepaintAction::Now`) and no
    /// claim above stood. The self-nudge shape; see the type note.
    EguiNow,
    /// egui asked for a repaint after a delay (`RepaintAction::After`), or the
    /// auto-poll deadline was what the loop was waiting on. A frame arriving
    /// on a timer with nothing to show is a timer set too fast, which is a
    /// different fix from a widget nudging itself.
    ///
    /// **"With nothing to show" is the load-bearing half**, and it used not to
    /// be true of this bucket. A timed repaint whose text genuinely moves
    /// raises [`NeedCause::Clock`] at the site that moves it and is never
    /// judged here at all; what is left is the timer that fires and changes
    /// nothing. Before that cause existed this bucket held both, and it was
    /// two frames in three on an idle app.
    Timed,
    /// **Nobody asked.** No claim stood and egui was idle; the frame came from
    /// the platform. See the type note.
    External,
}

impl WakeClaim {
    /// Every variant, in the order [`Self::index`] assigns — which is the
    /// order `handle_redraw`'s tail asks them in.
    pub(crate) const ALL: [Self; Self::COUNT] = [
        Self::Upload,
        Self::Render,
        Self::Loop,
        Self::Hold,
        Self::Restore,
        Self::Chunk,
        Self::Drops,
        Self::Gesture,
        Self::EguiNow,
        Self::Timed,
        Self::External,
    ];

    /// How many claims there are — the width of the charge array.
    pub(crate) const COUNT: usize = 11;

    /// This claim's slot in [`Self::ALL`] and in [`Reading::charged`].
    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Upload => 0,
            Self::Render => 1,
            Self::Loop => 2,
            Self::Hold => 3,
            Self::Restore => 4,
            Self::Chunk => 5,
            Self::Drops => 6,
            Self::Gesture => 7,
            Self::EguiNow => 8,
            Self::Timed => 9,
            Self::External => 10,
        }
    }

    /// A short name for a log line. Stable — the browser rig reads these.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Upload => "upload",
            Self::Render => "render",
            Self::Loop => "loop",
            Self::Hold => "hold",
            Self::Restore => "restore",
            Self::Chunk => "chunk",
            Self::Drops => "drops",
            Self::Gesture => "gesture",
            Self::EguiNow => "egui",
            Self::Timed => "timed",
            Self::External => "external",
        }
    }
}

/// A reading of the counters below, taken together. Running totals from boot,
/// so a window is a subtraction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Reading {
    /// Presented frames judged. **The denominator of every figure here** —
    /// see the module note on which frames are in it.
    pub(crate) drawn: u64,
    /// Of [`Self::drawn`], those that raised at least one cause.
    pub(crate) needed: u64,
    /// Of [`Self::drawn`], the frames each cause was raised on, indexed by
    /// [`NeedCause::index`].
    ///
    /// **These overlap and are never added**: a frame that took an arrival
    /// while the user was dragging raises two, and the sum of this array can
    /// therefore exceed [`Self::needed`]. What it can never do is fall below
    /// it — see [`Self::causes_cover_the_needed`].
    pub(crate) causes: [u64; NeedCause::COUNT],
    /// [`Self::unnecessary`] split by the claim that kept the app awake,
    /// indexed by [`WakeClaim::index`]. Exactly one per unnecessary frame, so
    /// this array sums to `unnecessary` — [`Self::charges_balance`].
    pub(crate) charged: [u64; WakeClaim::COUNT],
}

impl Reading {
    /// Frames that were drawn and needed nothing. **The figure this
    /// instrument exists to produce.**
    pub(crate) fn unnecessary(&self) -> u64 {
        self.drawn.saturating_sub(self.needed)
    }

    /// Whether any frame has been judged at all. **The non-vacuity floor**:
    /// every figure here is zero before the first presented frame, and a zero
    /// read without this one cannot be told from a path that never ran.
    // The three identities below are conservation laws the verdict's own
    // tests hold it to; production writes these counters and reports them
    // and never asks. Named rather than inlined into a test so that the
    // property has a spelling a reader can find from the type.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn ran(&self) -> bool {
        self.drawn > 0
    }

    /// Whether every unnecessary frame is charged to exactly one claim. False
    /// means a frame reached the verdict without a claim being chosen, which
    /// is the only way the two can disagree.
    ///
    /// **What this cannot detect: whether the claims are the right ones.** It
    /// is a conservation law, so it proves nothing is lost and nothing about
    /// what anything is called.
    // The three identities below are conservation laws the verdict's own
    // tests hold it to; production writes these counters and reports them
    // and never asks. Named rather than inlined into a test so that the
    // property has a spelling a reader can find from the type.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn charges_balance(&self) -> bool {
        self.charged.iter().sum::<u64>() == self.unnecessary()
    }

    /// Whether the causes cover the frames called necessary.
    ///
    /// A frame is necessary **because** a cause was raised on it, so every
    /// needed frame is in at least one cause bucket and the sum of the buckets
    /// is at least [`Self::needed`]. It exceeds it exactly by the frames that
    /// raised more than one. False means a frame was called necessary with no
    /// cause behind it, which is the shape a circular denominator takes.
    // The three identities below are conservation laws the verdict's own
    // tests hold it to; production writes these counters and reports them
    // and never asks. Named rather than inlined into a test so that the
    // property has a spelling a reader can find from the type.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn causes_cover_the_needed(&self) -> bool {
        self.causes.iter().sum::<u64>() >= self.needed
            && self.causes.iter().all(|&c| c <= self.needed)
    }

    /// One cause's frame count, read by name rather than by index.
    pub(crate) fn cause(&self, cause: NeedCause) -> u64 {
        self.causes[cause.index()]
    }

    /// One claim's charge, read by name rather than by index.
    pub(crate) fn charge(&self, claim: WakeClaim) -> u64 {
        self.charged[claim.index()]
    }
}

/// The verdict recorder the [`crate::frame_ledger::FrameLedger`] owns.
/// Single-writer, on the frame thread.
#[derive(Default)]
pub(crate) struct NeedLedger {
    reading: Reading,
    /// The claim standing when the **previous** frame's tail finished, which
    /// is the claim that bought the frame now being judged.
    ///
    /// `None` before the first tail has run — a frame judged before any tail
    /// (the boot frame) is charged to [`WakeClaim::External`], which is what
    /// it is: nobody in this application had yet asked for it.
    standing: Option<WakeClaim>,
    /// What this frame's tail claimed, held until the verdict has spent
    /// [`Self::standing`]. The tail runs before `finalize`, so without this
    /// second slot a frame would be charged to the claim it *caused* rather
    /// than to the one that caused it.
    next: Option<WakeClaim>,
}

impl NeedLedger {
    /// File what `handle_redraw`'s tail claimed, for the frame after this one.
    pub(crate) fn record_wake_claim(&mut self, claim: WakeClaim) {
        self.next = Some(claim);
    }

    /// Judge one presented frame against the causes raised for it.
    ///
    /// `causes` is the bitmask [`squallar_egui::frame_need::take`] returned —
    /// taken by the caller so that the register is cleared exactly once per
    /// presented frame and an unpresented frame's causes survive to the frame
    /// that draws them.
    pub(crate) fn record(&mut self, causes: u32) {
        self.reading.drawn += 1;
        if causes == 0 {
            let claim = self.standing.unwrap_or(WakeClaim::External);
            self.reading.charged[claim.index()] += 1;
        } else {
            self.reading.needed += 1;
            for cause in NeedCause::ALL {
                if causes & cause.bit() != 0 {
                    self.reading.causes[cause.index()] += 1;
                }
            }
        }
        // The tail that runs before this verdict was claiming for the NEXT
        // frame; it becomes the standing claim only now that this frame has
        // spent the previous one.
        if let Some(next) = self.next.take() {
            self.standing = Some(next);
        }
    }

    /// The counters, as a reading.
    pub(crate) fn reading(&self) -> Reading {
        self.reading
    }
}

#[cfg(test)]
#[path = "frame_need/tests.rs"]
mod tests;
