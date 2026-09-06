//! Why the frame being built had to exist at all.
//!
//! **Product telemetry, not a campaign instrument.** Always on, no feature
//! gate, no debug arm. Every write is one `fetch_or` with [`Relaxed`] ordering
//! on a `static` `AtomicU32`; nothing here allocates, formats, locks or takes
//! a clock. The sentence that reports the verdict is written by
//! `squallar-app`, off its own [`crate::frame_need`]-fed ledger — so no
//! formatting happens on any path that raises a cause.
//!
//! # The gap this fills
//!
//! Every other instrument in this tree measures **work done**: the command
//! stream counts what a frame recorded, `RerenderReason` charges overlay
//! rasters to the arm that asked, the frame ledger times the segments of
//! frames that ran. All three read a healthy zero on a frame that should
//! never have been drawn — a permanently repainting map, a map repainting on
//! a value that did not change, an application that never goes idle. That
//! class is what a user feels as heat and battery, and nothing in the tree
//! could see it.
//!
//! # The denominator, and why it is not circular
//!
//! The verdict is **frames drawn against frames that needed drawing**, and
//! the whole design is in the second half. The easy denominator is "egui asked
//! for a repaint", or "something called `request_repaint`" — and it is
//! **circular**: a map that nudges itself genuinely requests every one of the
//! repaints it wastes, so such a denominator scores the defect 100 % necessary
//! and agrees with it.
//!
//! So necessity is assembled from **causes**, raised at the site where the
//! change actually happens, never from the ask that follows one:
//!
//! * [`NeedCause::Input`] — the frame's raw input carried at least one event.
//!   Raised inside `EguiRenderer::begin_frame`, off the event vector egui is
//!   about to be handed, before any of it is interpreted.
//! * [`NeedCause::Arrival`] — something the frame drew arrived from off the
//!   frame thread this frame: a message taken off one of the app's channels,
//!   a tile body handled to completion, a held raster promoted onto the glass.
//! * [`NeedCause::Animation`] — an animation was running: a loop transport
//!   playing, or an egui widget animation still between its endpoints.
//! * [`NeedCause::Surface`] — the surface the frame draws into changed: a
//!   resize, a scale-factor or theme change, a renderer built.
//!
//! None of the four is reachable from a repaint request. A frame that raises
//! none of them is one the application drew for its own reasons with nothing
//! to show, and `squallar-app` charges it to the claim that kept the app
//! awake — which **is** the repaint ask, used for attribution and never for
//! the verdict. Attribution may be wrong about who; the verdict cannot be
//! made agreeable by asking louder.
//!
//! # Causes accumulate across frames that draw nothing, deliberately
//!
//! The register is cleared by [`take`], which `squallar-app` calls once per
//! **presented** frame. A frame that early-returns — minimized, zero area, a
//! lost surface — leaves the causes standing, because an arrival that was
//! never drawn still needs the next frame that is. Clearing per
//! `handle_redraw` entry instead would silently throw those away and report
//! the frame that finally draws them as unnecessary.
//!
//! # Cost
//!
//! A cause costs one relaxed `fetch_or` **on the frames where it happens**,
//! and nothing at all on the frames where it does not. The verdict costs one
//! `swap` per presented frame. No clock read: the pinned per-frame clock-read
//! and bin-search counts in `squallar_app::frame_ledger`'s module doc are
//! untouched by this module.
//!
//! # What this cannot see, stated rather than discovered
//!
//! The cause list is a **declared** set, and an animating picture whose driver
//! raises none of the four is scored unnecessary — an over-report, which is
//! the worse direction here because it is what makes an instrument
//! unbelievable. Two are known and covered by construction: egui's own widget
//! animations (through [`animate_bool`], the one spelling this crate
//! uses) and loop playback. A future picture that moves on a clock and tells
//! nobody would read as waste; the reading is then a hole in this list, and
//! the fix is a `note` at the site that moves it, never a threshold here.

use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

/// One reason a frame genuinely had to be drawn.
///
/// **Not mutually exclusive**: a frame can raise several, and
/// `squallar_app::frame_need`'s per-cause counters therefore overlap and are
/// never added to each other. What is exclusive is the verdict — a frame that
/// raised at least one of these is necessary, and one that raised none is not.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum NeedCause {
    /// The frame's raw input carried at least one egui event — pointer, touch,
    /// wheel, zoom, key, text, IME, focus or paste.
    ///
    /// **Deliberately wider than `EguiRenderer::frame_had_interaction`**,
    /// which is the frame ledger's interact/idle split and names five
    /// pointer-shaped variants only. That bar is "the picture answers the
    /// hand"; this one is "the user did something", and a keystroke that
    /// retypes a site name is something.
    Input,
    /// Something the frame draws arrived from off the frame thread: a channel
    /// message taken, a tile body handled, a finished raster promoted onto the
    /// glass.
    Arrival,
    /// An animation was running: a loop transport playing, or an egui widget
    /// animation still between its endpoints.
    Animation,
    /// The surface changed under the frame: a resize, a scale-factor or theme
    /// change, or the renderer being built.
    Surface,
}

impl NeedCause {
    /// Every variant, in the order [`Self::index`] assigns.
    pub const ALL: [Self; Self::COUNT] =
        [Self::Input, Self::Arrival, Self::Animation, Self::Surface];

    /// How many causes there are — the width of the reader's counter array.
    pub const COUNT: usize = 4;

    /// This cause's slot in [`Self::ALL`] and in the reader's array.
    pub const fn index(self) -> usize {
        match self {
            Self::Input => 0,
            Self::Arrival => 1,
            Self::Animation => 2,
            Self::Surface => 3,
        }
    }

    /// This cause's bit in the register [`take`] returns.
    pub const fn bit(self) -> u32 {
        1 << self.index()
    }

    /// A short name for a log line. Stable — the browser rig reads these.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Arrival => "arrival",
            Self::Animation => "animation",
            Self::Surface => "surface",
        }
    }
}

/// The register this thread raises causes into and reads them back from.
///
/// Production keeps one `static`; a test build keeps one per thread. Both
/// arms and the whole account of why are
/// [`crate::overlay_cache::ledger`]'s — a test binary runs its tests
/// concurrently in one process, so a register every test shares is one every
/// test writes, and a reader is then measuring its siblings. The premise the
/// isolation rests on is the same one: **every `note` below happens on the
/// frame thread**, which in a fixture is the test's own thread. A `note` added
/// on a worker, a rayon thread or a task would silently stop being counted in
/// a test build, and nothing here can detect that.
#[cfg(not(any(test, feature = "test-support")))]
fn sink() -> &'static AtomicU32 {
    static SHARED: AtomicU32 = AtomicU32::new(0);
    &SHARED
}

/// One register per thread — the production arm above carries the account.
#[cfg(any(test, feature = "test-support"))]
fn sink() -> &'static AtomicU32 {
    thread_local! {
        /// Leaked so this arm hands back the same `&'static AtomicU32` the
        /// production arm does and every body below stays one spelling.
        static OWN: &'static AtomicU32 = Box::leak(Box::new(AtomicU32::new(0)));
    }
    OWN.with(|register| *register)
}

/// Raise `cause` for the frame now being built.
///
/// Idempotent within a frame: the register is a bitmask, so a cause raised
/// forty times by forty arrivals is one bit. That is the point — the verdict
/// is per frame, and a frame is not more necessary for having two reasons.
pub fn note(cause: NeedCause) {
    sink().fetch_or(cause.bit(), Relaxed);
}

/// Every cause raised since the last [`take`], and clear the register.
///
/// Called once per **presented** frame by `squallar-app`. See the module
/// note on why an unpresented frame must not clear it.
pub fn take() -> u32 {
    sink().swap(0, Relaxed)
}

/// Every cause raised so far without clearing — for tests and for a reader
/// that wants the standing set without consuming it.
pub fn peek() -> u32 {
    sink().load(Relaxed)
}

/// `Context::animate_bool_with_time`, and **the one spelling this workspace
/// uses** — held by `the_animation_cause_has_no_bypass_in_the_ui_layer`.
///
/// egui's animation is the one picture-mover in the tree that neither the
/// input register nor an arrival can see: a drawer sliding, a status bar
/// expanding, a toast fading. Those frames genuinely need drawing, so without
/// this the verdict would call every one of them unnecessary — an over-report,
/// which is the failure that makes an instrument ignored.
///
/// **The test is the factor, not the ask.** A factor strictly between the two
/// endpoints is an animation that has not arrived, which is a fact about the
/// picture egui just computed; a factor sitting exactly on an endpoint is an
/// animation at rest, and one with `seconds == 0` lands on its endpoint the
/// first time it is asked. That makes the reading exact rather than a
/// heuristic — and, unlike the `request_repaint` egui makes on the same
/// frames, it is not the repaint ask this instrument exists to judge.
pub fn animate_bool(ctx: &egui::Context, id: egui::Id, target: bool, seconds: f32) -> f32 {
    let factor = ctx.animate_bool_with_time(id, target, seconds);
    if factor > 0.0 && factor < 1.0 {
        note(NeedCause::Animation);
    }
    factor
}

#[cfg(test)]
mod tests {
    use super::{NeedCause, note, peek, take};

    /// **Every cause has its own bit**, which is what lets the reader charge
    /// one frame to several causes without any of them shadowing another.
    ///
    /// Held by construction rather than by a literal table: a variant added
    /// with a duplicated `index` would collide silently, and a frame raising
    /// both would report one.
    #[test]
    fn the_four_causes_occupy_four_distinct_bits() {
        let mut seen = 0u32;
        for cause in NeedCause::ALL {
            assert_eq!(
                seen & cause.bit(),
                0,
                "{} shares a bit with a cause before it, so a frame raising \
                 both reports one",
                cause.name(),
            );
            seen |= cause.bit();
        }
        assert_eq!(
            seen.count_ones() as usize,
            NeedCause::COUNT,
            "the bits the causes occupy do not number the causes",
        );
        // Names are the rig's interface, so a duplicate is a family that
        // silently absorbs another's count.
        let mut names: Vec<&str> = NeedCause::ALL.iter().map(|c| c.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), NeedCause::COUNT, "two causes share a name");
    }

    /// **A cause raised many times in one frame is one bit**, and `take`
    /// clears — the two properties the per-frame verdict rests on.
    #[test]
    fn the_register_is_a_set_and_take_clears_it() {
        let _ = take();
        note(NeedCause::Arrival);
        note(NeedCause::Arrival);
        note(NeedCause::Arrival);
        assert_eq!(peek(), NeedCause::Arrival.bit());
        assert_eq!(take(), NeedCause::Arrival.bit());
        assert_eq!(peek(), 0, "take left a cause standing into the next frame");
        assert_eq!(take(), 0);
    }

    /// **Causes accumulate until a frame presents.** The register is cleared
    /// by `take` alone, so an arrival that landed on a frame which drew
    /// nothing is still standing for the frame that does.
    #[test]
    fn causes_survive_a_frame_that_never_presented() {
        let _ = take();
        note(NeedCause::Arrival);
        // A `handle_redraw` that early-returned: no `take`.
        note(NeedCause::Input);
        assert_eq!(take(), NeedCause::Arrival.bit() | NeedCause::Input.bit());
    }
}
