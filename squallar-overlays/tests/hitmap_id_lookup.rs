//! **What a hit map's id lookup costs to build.**
//!
//! `HitMap::from_cells` is handed `items` and a set of cells whose recorded ids
//! are *positions in that slice* — `0..items.len()`, dense, assigned by the
//! rasterizer's own enumeration. It used to answer those positions out of a
//! `HashMap<u32, Arc<dyn OverlayItem>>`: a hash table keyed by an index, built
//! by running SipHash-1-3 over every one of `0..n` to find the slot the index
//! already named. `<RandomState as BuildHasher>::hash_one::<&u32>` was 1.31% of
//! a 60 s native profile (27,027 samples), and 87% of that share arrived
//! through this call.
//!
//! Its own binary with a counting `#[global_allocator]`, for the reason
//! `gridded_projection_band.rs` sets out: the instrument counts real
//! `GlobalAlloc` calls and their requested sizes and knows nothing about hit
//! maps, so it compiles and runs against the **unmodified** tree and can
//! disagree with the fix.
//!
//! **The figure is bytes, not calls.** Both spellings take one allocation —
//! `collect` into a `HashMap` sizes the table from the iterator's exact hint,
//! and `to_vec` sizes the vector from the slice — so a call count cannot tell
//! them apart. What separates them is that a table holds `(u32, Arc)` pairs in
//! power-of-two buckets with control bytes beside them, and a vector holds `n`
//! `Arc`s and nothing else.

use std::alloc::{GlobalAlloc, Layout, System};
use std::any::Any;
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use squallar_overlays::render::overlay_state::{HitItems, OverlayItem, PopupContent};
use squallar_overlays::render::rasterize::{HitCells, HitMap};
use squallar_source::id::{LayerId, known};

static BYTES: AtomicUsize = AtomicUsize::new(0);
static CALLS: AtomicUsize = AtomicUsize::new(0);
thread_local! {
    /// Whether **this thread** is inside a measured window. Off outside it, so
    /// building the items — `n` `Arc` allocations of their own — is not in the
    /// figure.
    ///
    /// Thread-local and not a global `AtomicBool`, because libtest runs the
    /// tests in this binary concurrently: a global flag counts whatever the
    /// *other* tests are allocating at the same moment. That is not a
    /// hypothetical — the first reading taken with a global flag was 11
    /// allocations for a call that makes 1, and 1 on the next run of the same
    /// binary. `const`-initialised so that reading it inside the allocator
    /// cannot itself allocate.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

/// Whether the calling thread is measuring. `try_with` rather than `with`:
/// allocations arrive during TLS teardown too, and a destroyed key is not an
/// error here, it is "not measuring".
fn counting() -> bool {
    COUNTING.try_with(Cell::get).unwrap_or(false)
}

struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if counting() {
            CALLS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if counting() {
            CALLS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    /// A grow is a fresh block of `new_size`; a shrink is not a new block.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() && counting() {
            CALLS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(new_size, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// `CALLS` and `BYTES` are process-global, so two threads measuring at once
/// would share them even though the flag that gates them is not. One window at
/// a time.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Runs `body` with the counter on, returning `(value, calls, bytes)`.
fn allocations_during<T>(body: impl FnOnce() -> T) -> (T, usize, usize) {
    // Bound to a name and taken before the window opens. Never inside an
    // `assert!`: a lock in an assertion's *message* is taken while the
    // condition still holds it, and that hangs instead of reddening.
    let _serialised: MutexGuard<'_, ()> = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    CALLS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    COUNTING.set(true);
    let value = body();
    COUNTING.set(false);
    (
        value,
        CALLS.load(Ordering::Relaxed),
        BYTES.load(Ordering::Relaxed),
    )
}

/// An overlay item that is nothing but its own position, so a hit answering the
/// wrong item is visible as a wrong number rather than as a wrong pointer.
#[derive(Debug)]
struct Numbered(u32);

impl OverlayItem for Numbered {
    fn layer_id(&self) -> LayerId {
        known::NWS_ALERTS
    }

    fn popup_content(&self, _prefs: &squallar_units::UserPreferences) -> PopupContent {
        PopupContent {
            title: self.0.to_string(),
            accent_rgb: [0, 0, 0],
            width: 320.0,
            sections: Vec::new(),
            actions: Vec::new(),
        }
    }

    fn matches(&self, other: &dyn OverlayItem) -> bool {
        other
            .as_any()
            .downcast_ref::<Numbered>()
            .is_some_and(|other| other.0 == self.0)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The materialised arm, [`HitItems::Rows`] — the shape every hit-map layer but
/// GLM lightning answers, and the one the byte figure below is about. The slab
/// arm holds one handle and builds nothing until a click asks, so it is not
/// measured here and could not be: there is no per-item cost to count.
fn numbered(n: u32) -> HitItems {
    (0..n)
        .map(|i| Arc::new(Numbered(i)) as Arc<dyn OverlayItem>)
        .collect()
}

/// The item a hit named, by its number.
fn hit_numbers(map: &HitMap, u: f32, v: f32) -> Vec<u32> {
    map.hit_test(u, v)
        .iter()
        .map(|item| {
            item.as_any()
                .downcast_ref::<Numbered>()
                .expect("the fixture puts only Numbered items in")
                .0
        })
        .collect()
}

/// **The gate.** Building the lookup for `n` items costs `n` pointers and not a
/// hash table's worth of buckets.
#[test]
fn the_id_lookup_holds_one_pointer_per_item_and_no_table() {
    const N: u32 = 1024;
    let items = numbered(N);
    let cells = HitCells::new(512, 512);

    let (map, calls, bytes) = allocations_during(|| HitMap::from_cells(cells, &items));

    let want = (N as usize) * std::mem::size_of::<Arc<dyn OverlayItem>>();
    // Not the discriminating figure -- both spellings take exactly one, which
    // is why the byte count below is the gate. It is here so that a future
    // rewrite that reaches the right *size* by way of several allocations is
    // still visible.
    assert_eq!(
        calls, 1,
        "building the lookup for {N} items took {calls} allocations totalling \
         {bytes} bytes; one contiguous run of pointers is one allocation of \
         {want}"
    );
    assert_eq!(
        bytes, want,
        "building the lookup for {N} items asked the allocator for {bytes} \
         bytes where {N} pointers are {want}. A hash table keyed by the index \
         is the shape that puts it above: power-of-two buckets holding \
         (u32, Arc) pairs, with a control byte each, and SipHash-1-3 run over \
         every one of 0..{N} to find the slot the index already named."
    );

    // The lookup still answers, and the empty grid answers nothing rather than
    // answering wrongly.
    assert!(
        hit_numbers(&map, 0.5, 0.5).is_empty(),
        "no cell was recorded, so no hit can be"
    );
}

/// **The correctness floor.** Positions in, the same positions out — including
/// the order a cell recorded them in, which is what a caller reads as z-order.
#[test]
fn a_hit_names_the_items_its_cell_recorded_in_the_order_it_recorded_them() {
    let items = numbered(64);
    let mut cells = HitCells::new(64, 64);

    // Quarter-resolution grid is 16 x 16. Record at three separate pixels, one
    // of them twice with different items and once with a repeat.
    cells.record(0.0, 0.0, 7);
    cells.record(1.0, 2.0, 3); // same quarter-cell as (0, 0)
    cells.record(1.0, 2.0, 7); // already there, must not repeat
    cells.record(40.0, 8.0, 11);

    let map = HitMap::from_cells(cells, &items);

    assert_eq!(
        hit_numbers(&map, 0.5 / 16.0, 0.5 / 16.0),
        vec![7, 3],
        "cell (0, 0) recorded 7 then 3, deduped, and hands them back in that \
         order"
    );
    assert_eq!(
        hit_numbers(&map, 10.5 / 16.0, 2.5 / 16.0),
        vec![11],
        "cell (10, 2) recorded only item 11"
    );
    assert!(
        hit_numbers(&map, 15.5 / 16.0, 15.5 / 16.0).is_empty(),
        "a cell nothing was drawn into answers nothing"
    );
    assert!(
        hit_numbers(&map, -0.1, 0.5).is_empty(),
        "a probe outside the texture answers nothing"
    );
}

/// **An id past the end answers nothing rather than panicking or wrapping.**
///
/// `Vec::get` and `HashMap::get` differ here only if the index is written as
/// `items[id]`, which is exactly the mistake the swap invites. The dispatch in
/// `App::overlay_job_deliver` refuses a reply whose `max_id` is out of range,
/// so this is the second line rather than the first — but it is the line that
/// makes the swap safe on its own terms.
#[test]
fn an_id_past_the_end_of_the_items_answers_nothing() {
    let items = numbered(4);
    let mut cells = HitCells::new(16, 16);
    cells.record(0.0, 0.0, 2);
    cells.record(0.0, 0.0, 99);

    let map = HitMap::from_cells(cells, &items);

    assert_eq!(
        hit_numbers(&map, 0.5 / 4.0, 0.5 / 4.0),
        vec![2],
        "id 2 is in range and id 99 is not; the hit is the one that is"
    );
}
