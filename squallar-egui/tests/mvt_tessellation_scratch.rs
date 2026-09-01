//! **What one tile's fills cost the allocator.**
//!
//! `walkers::mvt::styled` tessellates every filled polygon in a tile through
//! lyon. Each tessellation needs three scratch containers — lyon's event queue,
//! the path's point and verb vectors, and the vertex/index buffers the
//! tessellator writes into — and none of them is output: the event queue is
//! rebuilt from the path every time, and the buffers are copied into the
//! returned `Mesh`. Taken fresh per polygon they each grow from empty per
//! polygon; held across the tile they grow once, to the tile's largest polygon.
//!
//! Its own binary with a counting `#[global_allocator]`, for the reason
//! `squallar-overlays/tests/gridded_projection_band.rs` sets out: the
//! instrument counts real `GlobalAlloc` calls and knows nothing about
//! tessellation, so it compiles and runs against the **unmodified** tree and
//! can disagree with the fix.
//!
//! Unlike that file this one counts *calls* rather than large blocks, and
//! counts a growing `realloc` as one: the defect is the number of trips to the
//! allocator, and a `Vec` that grew its way up made a trip whatever the call
//! was named. A shrinking `realloc` is not counted — it is the same block a
//! grow already paid for.
//!
//! The output identity that says the reuse is sound lives next to the code, in
//! `vendor/walkers/src/mvt.rs`'s own test module, where lyon's point type is in
//! scope without this crate taking a dependency on it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use walkers::Style;
use walkers::mvt::{parse, styled};

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
thread_local! {
    /// Whether **this thread** is inside a measured window. Off outside it, so
    /// the fixture decode, the style parse and anything the harness itself does
    /// are not in the figure.
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
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if counting() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    /// A grow counts; a shrink does not. `Vec::shrink_to_fit` and
    /// `into_boxed_slice` both arrive here, and neither is a new block.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() && counting() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// `ALLOCS` is process-global, so two threads measuring at once would share it
/// even though the flag that gates them is not. One window at a time.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Runs `body` with the counter on, returning `(value, allocator calls)`.
fn allocations_during<T>(body: impl FnOnce() -> T) -> (T, usize) {
    // Bound to a name and taken before the window opens. Never inside an
    // `assert!`: a lock in an assertion's *message* is taken while the
    // condition still holds it, and that hangs instead of reddening.
    let _serialised: MutexGuard<'_, ()> = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ALLOCS.store(0, Ordering::Relaxed);
    COUNTING.set(true);
    let value = body();
    COUNTING.set(false);
    (value, ALLOCS.load(Ordering::Relaxed))
}

/// z14/8529/5974 of `squallar-egui/testdata/monaco.pmtiles`, the same tile
/// `squallar-buildings` extrudes and `squallar-gpu`'s cost tests read. Its
/// `building` source-layer is drawn by two `fill` layers of the committed dark
/// style, so styling it reaches the tessellator twice per building.
const TILE: &[u8] =
    include_bytes!("../../squallar-buildings/testdata/monaco-building-z14-8529-5974.mvt");

/// The committed dark style, verbatim — `squallar_egui::basemap_style` compiles
/// in this same file. Read directly rather than through that module so the
/// figure cannot depend on which theme a memoising helper saw first.
const DARK: &str = include_str!("../../www/styles/dark.json");

/// The number of allocator calls `styled` may make over the committed fixture.
///
/// **Measured, on this tree and on the tree before it, same binary and same
/// fixture, twice each and identical both times:** 15,099 calls with the
/// tile-scoped scratch, **52,421** with the per-polygon one. Both arms produce
/// 15,102 tessellated vertices, which is the identity that says the two are
/// doing the same work.
///
/// 18,000 is 15,099 with about a fifth of headroom, so that unrelated growth in
/// the style walk does not red the suite, and still 2.9x under what the
/// per-polygon spelling costs. A tree that goes back to building the scratch per
/// polygon cannot land under it.
const ALLOCATION_CEILING: usize = 18_000;

/// How many tessellated vertices a run of `styled` produced.
///
/// **Not the mesh count.** `styled` folds each run of adjacent meshes into one
/// (`coalesce_adjacent_meshes`), so a tile of dozens of tessellated polygons
/// comes back as a single `Shape::Mesh`; counting meshes would read 1 whatever
/// the tile held. Vertices survive the fold, and they scale with the number of
/// polygons that reached lyon — which is what the allocation figure is about.
fn tessellated_vertices(shapes: &[walkers::ShapeOrText]) -> usize {
    shapes
        .iter()
        .map(|shape| match shape {
            walkers::ShapeOrText::Shape(egui::Shape::Mesh(mesh)) => mesh.vertices.len(),
            _ => 0,
        })
        .sum()
}

#[test]
fn styling_one_tile_does_not_pay_the_tessellator_setup_per_polygon() {
    let tile = parse(TILE).expect("the committed fixture decodes");
    let style = Style::from_json(DARK).expect("the committed dark style parses");

    let (shapes, allocs) = allocations_during(|| styled(&tile, &style, 14));
    let vertices = tessellated_vertices(&shapes);

    assert!(
        vertices >= 200,
        "the fixture must actually reach the tessellator, and it produced only \
         {vertices} tessellated vertices -- a gate over a tile that tessellates \
         nothing would be measuring the style walk"
    );

    assert!(
        allocs <= ALLOCATION_CEILING,
        "styling the committed z14 building tile made {allocs} allocator calls \
         for {vertices} tessellated vertices, over the {ALLOCATION_CEILING} \
         this tree holds itself to. Per-polygon tessellator setup is the shape \
         that puts it here: one lyon event queue, one path point/verb pair and \
         one 512/1024-slot VertexBuffers built from empty per polygon rather \
         than once per tile."
    );
}
