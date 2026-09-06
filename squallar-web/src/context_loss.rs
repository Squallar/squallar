//! **The cell the canvas's two context listeners and the bridge share.**
//!
//! A browser's WebGL2 context can go at any moment — a GPU reset, a driver
//! update, some backgrounding paths — and the page is told through two DOM
//! events on the canvas rather than through anything wgpu reports. This module
//! is what those two listeners write into and what
//! `PlatformBridge::poll_graphics_restore` reads out, kept apart from the
//! listeners themselves so the part with a decision in it compiles and is
//! tested on the host: the wasm-only half is two `addEventListener` calls
//! and nothing else.
//!
//! # The three facts it holds
//!
//! * **a restore is reported exactly once.** [`Self::take_restore`] consumes,
//!   so the app's teardown cannot run twice for one restore, and a frame that
//!   read `false` was genuinely told nothing rather than told late;
//! * **a loss that arrives before the app has drained the last restore does
//!   not erase it.** The two events are separate flags, and a lost/restored
//!   pair that lands between two frames is one restore to drain, not zero;
//! * **the wake is what makes any of it reachable.** The app runs on
//!   `ControlFlow::Wait`; a DOM event is not a winit event, so a restore that
//!   only set a flag would sit unread until the user happened to move the
//!   mouse. [`Self::note_restored`] asks for a frame, and that ask is what the
//!   poll rides in on.
//!
//! The wake is a boxed closure rather than the app's own `RedrawWaker` for one
//! reason: `squallar-app` is a wasm32-only dependency of this crate, and a
//! module that named that type could not compile on the host, which is where
//! the tests below run.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

type Wake = Box<dyn Fn()>;

/// A handle on the shared cell. Cheap to clone — every clone is the same cell,
/// which is what lets one live in the bridge and one in each DOM closure.
#[derive(Clone, Default)]
pub struct ContextLoss {
    inner: Rc<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Set by the `webglcontextrestored` listener, cleared by the app's poll.
    restored: Cell<bool>,
    /// Every `webglcontextlost` this page has seen, for the log line. A total,
    /// never cleared: a page that has lost its context five times is a page
    /// with a problem, and the count is the only place that shows.
    losses: Cell<u32>,
    /// How the listener asks the event loop for a frame. `None` until
    /// `PlatformBridge::set_redraw_waker` has run, which is after the
    /// listeners are installed — a loss in that window is still recorded and
    /// still drained by the next frame, of which boot has many.
    wake: RefCell<Option<Wake>>,
}

impl ContextLoss {
    /// A fresh cell: nothing lost, nothing restored, nothing to wake with.
    pub fn new() -> Self {
        Self::default()
    }

    /// Install what [`Self::note_restored`] asks for a frame through.
    pub fn set_wake(&self, wake: Wake) {
        *self.inner.wake.borrow_mut() = Some(wake);
    }

    /// The `webglcontextlost` listener's whole record. **It does not clear a
    /// restore already waiting**: a lost/restored pair that both land between
    /// two frames leaves one restore for the app to act on, which is the
    /// truth — the context the app holds handles for is gone either way.
    pub fn note_lost(&self) {
        self.inner.losses.set(self.inner.losses.get() + 1);
    }

    /// The `webglcontextrestored` listener's record, **and the ask for a
    /// frame**: the flag alone would be read whenever the user next happened
    /// to generate an event, which on an idle map is never.
    pub fn note_restored(&self) {
        self.inner.restored.set(true);
        // The borrow ends before the call: a wake reaches winit, and holding a
        // `RefCell` guard across foreign code is how a re-entrant listener
        // panics instead of being merely surprising.
        let wake = self.inner.wake.borrow();
        if let Some(wake) = wake.as_ref() {
            wake();
        }
    }

    /// **Consuming**: whether a restore has landed since this was last asked.
    pub fn take_restore(&self) -> bool {
        self.inner.restored.replace(false)
    }

    /// Every `webglcontextlost` seen on this page, for the log.
    pub fn losses(&self) -> u32 {
        self.inner.losses.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn counting() -> (ContextLoss, Arc<AtomicUsize>) {
        let cell = ContextLoss::new();
        let count = Arc::new(AtomicUsize::new(0));
        let sink = Arc::clone(&count);
        cell.set_wake(Box::new(move || {
            sink.fetch_add(1, Ordering::Relaxed);
        }));
        (cell, count)
    }

    /// A fresh cell has nothing to report, which is what every frame of a
    /// healthy page reads.
    #[test]
    fn a_page_that_has_not_lost_its_context_reports_nothing() {
        let cell = ContextLoss::new();
        assert!(!cell.take_restore());
        assert_eq!(cell.losses(), 0);
    }

    /// A restore is drained once and then gone: the app's teardown is not run
    /// twice for one event.
    #[test]
    fn a_restore_is_reported_exactly_once() {
        let cell = ContextLoss::new();
        cell.note_lost();
        cell.note_restored();
        assert!(cell.take_restore(), "the restore was not reported at all");
        assert!(
            !cell.take_restore(),
            "the same restore was reported a second time; the app would tear \
             its graphics state down twice for one event",
        );
    }

    /// **A loss landing on top of an undrained restore does not erase it.**
    /// The pair can straddle one frame boundary, and the flag the app acts on
    /// is what says the handles it holds are stale — which a second loss makes
    /// more true, not less.
    #[test]
    fn a_second_loss_does_not_swallow_a_restore_nobody_has_read() {
        let cell = ContextLoss::new();
        cell.note_restored();
        cell.note_lost();
        assert!(
            cell.take_restore(),
            "a loss arriving before the poll cleared the restore the poll \
             exists to deliver",
        );
        assert_eq!(cell.losses(), 1);
    }

    /// **The restore asks for a frame.** Without it the app sits on
    /// `ControlFlow::Wait` with a dead canvas and a flag nobody comes to read.
    #[test]
    fn a_restore_with_no_pending_redraw_asks_for_one() {
        let (cell, woke) = counting();
        assert_eq!(woke.load(Ordering::Relaxed), 0, "precondition: idle");
        cell.note_restored();
        assert_eq!(
            woke.load(Ordering::Relaxed),
            1,
            "a restored context did not ask the event loop for a frame, so \
             nothing would poll it until the user moved the mouse",
        );
    }

    /// A loss is not a repaint: the page cannot draw anything useful until the
    /// restore, and waking to redraw a dead context is a frame spent on
    /// nothing.
    #[test]
    fn a_loss_alone_does_not_ask_for_a_frame() {
        let (cell, woke) = counting();
        cell.note_lost();
        assert_eq!(woke.load(Ordering::Relaxed), 0);
        assert!(!cell.take_restore());
    }

    /// Losses accumulate; they are a running total and no poll clears them.
    #[test]
    fn losses_are_a_running_total() {
        let cell = ContextLoss::new();
        for _ in 0..3 {
            cell.note_lost();
            cell.note_restored();
            assert!(cell.take_restore());
        }
        assert_eq!(cell.losses(), 3);
    }

    /// A restore before any waker is installed is still delivered — boot
    /// installs the listeners before `App::new` hands the bridge its waker,
    /// and the frames boot draws anyway are what drain it.
    #[test]
    fn a_restore_before_a_waker_exists_is_still_drained() {
        let cell = ContextLoss::new();
        cell.note_restored();
        assert!(cell.take_restore());
    }
}
