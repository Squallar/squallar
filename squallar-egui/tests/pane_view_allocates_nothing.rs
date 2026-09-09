//! **`PaneState::view` allocates nothing**, at any slot count.
//!
//! `PaneView` used to materialise a table of every slot's `(&LayerId,
//! &Value)` so that `PaneRef::sibling` could look one up — one `Vec` sized by
//! the pane's whole stack, per call, on a path the draw walk reaches once per
//! pane per frame and `App::spawn_overlay_render` reaches again per dispatched
//! overlay request. No handler in the workspace ever read it, so the table
//! went and this holds it gone.
//!
//! Its own binary with a counting `#[global_allocator]`, the arrangement
//! `squallar-overlays/tests/metar_raster_one_picture.rs` uses: the allocator
//! counts real `GlobalAlloc` calls made on the measuring thread inside the
//! window and knows nothing about what is being built.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use squallar_egui::pane::{LayerSlot, PaneState};
use squallar_source::id::LayerId;

/// The stack the pane is given. Deliberately more than a couple of slots: the
/// table this pins away scaled with the stack, so a one-slot fixture would be
/// the weakest case it has.
const SLOTS: usize = 12;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// Whether **this thread** is inside the measured window. Thread-local
    /// and not a global flag: libtest runs a binary's tests concurrently and
    /// a global would count whatever a neighbour allocated at the same
    /// instant. `const`-initialised so reading it cannot itself allocate.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

fn counting() -> bool {
    COUNTING.try_with(Cell::get).unwrap_or(false)
}

fn record(size: usize) {
    if counting() {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(size, Ordering::Relaxed);
    }
}

struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    /// A grow is a fresh block; a shrink is not. A `Vec` built by `collect`
    /// over a sized iterator arrives through `alloc`, and one built by
    /// repeated `push` arrives here — both are counted.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() {
            record(new_size);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// The counters are process-global even though the flag gating them is not.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// A pane carrying [`SLOTS`] slots, each with a non-null config — the shape
/// the removed table was built out of.
fn pane_with_a_stack() -> PaneState {
    let mut pane = PaneState::new();
    for i in 0..SLOTS {
        let id = LayerId::new(format!("test.slot.{i}"));
        let mut slot = LayerSlot::new(id, true);
        slot.config = serde_json::json!({ "n": i });
        pane.layers.push(slot);
    }
    pane
}

#[test]
fn building_a_pane_view_allocates_nothing() {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let pane = pane_with_a_stack();
    assert_eq!(
        pane.draw_order().len(),
        SLOTS,
        "fixture precondition: the pane must actually carry a stack, or a \
         zero-length table would allocate nothing whatever this test pins",
    );

    ALLOCS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    COUNTING.with(|c| c.set(true));
    let view = pane.view(0);
    COUNTING.with(|c| c.set(false));

    let allocs = ALLOCS.load(Ordering::Relaxed);
    let bytes = BYTES.load(Ordering::Relaxed);

    // Asked outside the window, so the handle is shown to be a working one and
    // not an empty shell that allocates nothing because it carries nothing.
    // `PaneView` implements no `Drop`, so its end of scope cannot allocate
    // either.
    assert!(
        view.layer(&LayerId::new("test.slot.3"))
            .config
            .get("n")
            .is_some(),
        "the view must still answer about a slot this pane holds",
    );

    assert_eq!(
        (allocs, bytes),
        (0, 0),
        "`PaneState::view` allocated {allocs} block(s) / {bytes} B over a \
         {SLOTS}-slot pane; it is built once per pane per frame by the draw \
         walk and again per dispatched overlay render, so anything here is \
         paid at that rate",
    );
}

/// **The whole walk, not just the handle.** A view built once and asked about
/// every slot in turn is what the draw walk does; it must cost nothing at all.
///
/// The memo `LayerStack::position_of` keeps is **warmed first, outside the
/// window**. It is rebuilt on the first lookup after the stack is mutated —
/// one `Vec` of marks, 8 B a slot, amortised over every lookup until the next
/// mutation — and measuring the fixture's very first lookup would be counting
/// that setup rather than the per-frame cost this is about.
#[test]
fn a_walk_over_every_layer_off_one_view_allocates_nothing() {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let pane = pane_with_a_stack();
    let ids: Vec<LayerId> = (0..SLOTS)
        .map(|i| LayerId::new(format!("test.slot.{i}")))
        .collect();
    for id in &ids {
        assert!(
            pane.layer_ref(0, id).config.get("n").is_some(),
            "fixture precondition: every id must name a slot this pane holds, \
             and this warms the stack's lookup memo outside the window",
        );
    }

    ALLOCS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    COUNTING.with(|c| c.set(true));
    let view = pane.view(0);
    let mut seen = 0usize;
    for id in &ids {
        seen += usize::from(view.layer(id).state.is_none());
    }
    COUNTING.with(|c| c.set(false));

    let allocs = ALLOCS.load(Ordering::Relaxed);
    let bytes = BYTES.load(Ordering::Relaxed);

    assert_eq!(seen, SLOTS, "every slot must have been asked about");
    assert_eq!(
        (allocs, bytes),
        (0, 0),
        "one view asked about {SLOTS} layers allocated {allocs} block(s) / \
         {bytes} B; that is the draw walk's per-pane cost every frame",
    );
}
