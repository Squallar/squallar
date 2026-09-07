//! The two window questions the frame gate asks, answered from a cache that
//! events invalidate rather than from the window server every frame.
//!
//! `App::handle_redraw` abandons a frame whose window is minimized or has zero
//! area. Both answers used to be read straight off the window on **every**
//! frame, and on X11 both are synchronous round trips to the display server:
//! `is_minimized` is a `GetProperty` of `_NET_WM_STATE` looked up for
//! `_NET_WM_STATE_HIDDEN`, `inner_size` is a `GetGeometry`
//! (winit 0.30.13, `platform_impl/linux/x11/window.rs`, `is_minimized` and
//! `inner_size_physical`). The answers change only when the user resizes,
//! hides or restores the window, so asking per frame buys nothing at any
//! resolution — but what the pair costs, and what holding it saves, are two
//! different numbers and neither is a constant.
//!
//! # What the pair costs
//!
//! **Not one figure.** An earlier reading of a **steady 159 µs on every
//! interact frame — a floor, not a tail** is withdrawn: it was one unstated
//! condition quoted as if it were the property. The same two queries, same
//! box, same display, same binary, measure
//!
//! * **72.61 µs** per interact frame at 640×480, and
//! * **462–474 µs** at 1920×1080.
//!
//! A synchronous round trip is charged for whatever the server already has
//! queued ahead of the reply, so the cost tracks X connection and server
//! queue depth rather than sitting under each frame as a per-frame constant.
//! Quote it with the resolution it was read at, or do not quote it.
//!
//! # What removing it saves
//!
//! **Also not the same number.** On the 640×480 arm the two round trips took
//! 72.61 µs out of the gate segment — that segment goes to zero — but
//! 40.86 µs of it came back in the **pump** segment, 200× that column's floor
//! in the null control. The net is **49.94 µs per presented interact frame,
//! 68.8 % of what was removed**, and it holds outside the ledger: cadence
//! improved 49.4 µs once the null's −9.48 µs bias was subtracted, which
//! agrees with the service figure.
//!
//! The general form is what keeps this from rotting again: **a segment
//! falling to zero proves the work left that segment, not that its cost left
//! the frame.** Only a paired whole-frame figure settles that, and any future
//! reading of this cache owes both halves.
//!
//! Two thirds of a real per-frame cost is worth taking and none of this
//! argues for putting the queries back. The claim was overstated; the cache
//! is not.
//!
//! Arm: Xvfb, 640×480, RTX 3090 via Vulkan, scene A, paired adjacent frames,
//! medians, n = 5466 presented interact frames against a null control at
//! n = 5542; the 1920×1080 figure is the same rig at that geometry.
//!
//! It is a round trip only on X11. `is_minimized` is `isMiniaturized` on
//! macOS, `IsIconic` on Windows, and a flat `None` on Wayland, Android and
//! iOS (clients there are not told); `inner_size` is a local read on all of
//! them. The cache is one code path for every target regardless: the frame
//! path must not fork on the platform, and a cheap query skipped is still a
//! query skipped.
//!
//! # What makes a reading stale
//!
//! Not a clock, and not "probably nothing has happened". [`invalidated_by`]
//! names the events that DEFINE the two values, and a reading survives until
//! one of them arrives:
//!
//! * [`WindowEvent::Resized`] is winit's contract for a changed inner size on
//!   every backend, so it is the whole of the size half.
//! * [`WindowEvent::ScaleFactorChanged`] carries an `InnerSizeWriter`, so the
//!   size can move across it without a separate `Resized`.
//! * [`WindowEvent::Occluded`] is the visibility change itself — X11's
//!   `VisibilityNotify`, macOS's `windowDidChangeOcclusionState`, the web's
//!   intersection observer.
//! * [`WindowEvent::Focused`] is what carries a **restore** on X11. An
//!   iconified X11 window is an unmapped one, so de-iconifying it is a
//!   `MapNotify`, and winit's `map_notify` re-issues the focus state as a
//!   `Focused` event for exactly that reason (its own comment: the initial
//!   focused state cannot ride on `CreateNotify`). macOS reaches the same
//!   place through `windowDidBecomeKey`.
//!
//! The dangerous staleness is one-sided and this is what closes it. A stale
//! `minimized = false` costs a drawn frame nobody sees — the waste the gate
//! exists to avoid, and no worse than not having a gate. A stale
//! `minimized = true` would freeze a window the user has just restored, and
//! **that transition cannot happen without one of the four above**: on X11 it
//! is a map, on macOS a key/occlusion change, on Windows a `WM_SIZE` (so
//! `Resized`), on the web a resize or an intersection change. Wayland, Android
//! and iOS answer `None` to the question at all times, so the flag they cache
//! has no transition to miss.
//!
//! A new window is a new set of answers and [`WindowGate::invalidate`] is
//! called where one is created, because `request_inner_size` returns the new
//! size **synchronously and emits no event** on the backends that can satisfy
//! it at once (winit's own `Window::request_inner_size` contract).
//!
//! # What is deliberately not in the set
//!
//! Pointer motion, wheel, touch and key events. They are the frames this cache
//! exists for: invalidating on them would re-read the window on every interact
//! frame and buy nothing. `Moved` is out for the same reason — a drag emits
//! one per motion, and a move that also resizes emits `Resized` beside it.

use winit::event::WindowEvent;
use winit::window::Window;

/// What the two window queries last answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Reading {
    /// `window.is_minimized()`, with `None` — the platforms that do not tell a
    /// client — read as "not minimized", which is what the gate did with it
    /// before this cache existed.
    pub(crate) minimized: bool,
    /// `window.inner_size()`, physical pixels.
    pub(crate) size: (u32, u32),
}

impl Reading {
    /// Ask the window. The only place in the tree that spells either query on
    /// the frame path.
    fn from_window(window: &Window) -> Self {
        let size = window.inner_size();
        Self {
            minimized: window.is_minimized().unwrap_or(false),
            size: (size.width, size.height),
        }
    }

    /// Zero on either axis: nothing to draw into.
    pub(crate) fn zero_area(self) -> bool {
        self.size.0 == 0 || self.size.1 == 0
    }
}

/// The cached answer, plus the event rule that retires it.
#[derive(Default)]
pub(crate) struct WindowGate {
    /// `None` means the next read goes to the window. Set back to `None` by
    /// [`Self::note_event`] and [`Self::invalidate`], and never by a clock.
    cached: Option<Reading>,
}

impl WindowGate {
    /// Retire the cached answer: the next read goes to the window.
    pub(crate) fn invalidate(&mut self) {
        self.cached = None;
    }

    /// Retire it if this event is one that can move either answer.
    pub(crate) fn note_event(&mut self, event: &WindowEvent) {
        if invalidated_by(event) {
            self.invalidate();
        }
    }

    /// This frame's answer: the cache when it is warm, the window when it is
    /// not.
    pub(crate) fn read(&mut self, window: &Window) -> Reading {
        self.read_or_else(|| Reading::from_window(window))
    }

    /// [`Self::read`] with the query supplied, so a test can count how often
    /// the cache actually went and asked.
    fn read_or_else(&mut self, ask: impl FnOnce() -> Reading) -> Reading {
        match self.cached {
            Some(reading) => reading,
            None => *self.cached.insert(ask()),
        }
    }
}

/// Whether this event can change the minimized flag or the inner size.
///
/// Exhaustive on purpose rather than a `matches!` over four names with a `_`
/// arm: a winit upgrade that adds a variant carrying a size or a visibility
/// change should fail this build rather than be swallowed by a wildcard.
pub(crate) fn invalidated_by(event: &WindowEvent) -> bool {
    match event {
        // `Resized` is the size, and is winit's contract for it on every
        // backend. `ScaleFactorChanged` carries an `InnerSizeWriter`, so the
        // size can move across it without a `Resized` beside it. `Occluded`
        // is the visibility change itself. `Focused` is what carries a
        // RESTORE on X11 — an iconified X11 window is an unmapped one, so
        // de-iconifying it is a `MapNotify`, and winit's `map_notify`
        // re-issues the focus state as a `Focused` event. `Destroyed` ends
        // the window these answers were about.
        WindowEvent::Resized(_)
        | WindowEvent::ScaleFactorChanged { .. }
        | WindowEvent::Occluded(_)
        | WindowEvent::Focused(_)
        | WindowEvent::Destroyed => true,
        // Everything else. The pointer, wheel, touch and key families are the
        // frames this cache exists for and must not retire it; `Moved` is out
        // with them, because a drag emits one per motion and a move that also
        // resizes emits `Resized` beside it.
        WindowEvent::ActivationTokenDone { .. }
        | WindowEvent::Moved(_)
        | WindowEvent::CloseRequested
        | WindowEvent::DroppedFile(_)
        | WindowEvent::HoveredFile(_)
        | WindowEvent::HoveredFileCancelled
        | WindowEvent::KeyboardInput { .. }
        | WindowEvent::ModifiersChanged(_)
        | WindowEvent::Ime(_)
        | WindowEvent::CursorMoved { .. }
        | WindowEvent::CursorEntered { .. }
        | WindowEvent::CursorLeft { .. }
        | WindowEvent::MouseWheel { .. }
        | WindowEvent::MouseInput { .. }
        | WindowEvent::PinchGesture { .. }
        | WindowEvent::PanGesture { .. }
        | WindowEvent::DoubleTapGesture { .. }
        | WindowEvent::RotationGesture { .. }
        | WindowEvent::TouchpadPressure { .. }
        | WindowEvent::AxisMotion { .. }
        | WindowEvent::Touch(_)
        | WindowEvent::ThemeChanged(_)
        | WindowEvent::RedrawRequested => false,
    }
}

#[cfg(test)]
mod tests;
