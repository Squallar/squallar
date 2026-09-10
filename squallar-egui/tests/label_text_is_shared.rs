//! **A basemap label's name is not copied to place it, and not copied to look
//! it up.**
//!
//! The pane's ground phase walks every vector tile on the glass on every
//! frame. For each label the tile carries it calls
//! `ShapeOrText::placed` (through `ui_map_overlays::place_one`) to move the
//! anchor into pane coordinates, and then `Text::galley_cached` (through
//! `lay_out_label`) to find the laid-out glyphs. Both spellings clone
//! `walkers::Text::text`.
//!
//! While that field owned its bytes, those two clones were **two `malloc`s and
//! two `memcpy`s of the place name, per label, per frame, on a map that had
//! not moved**. A `perf` profile of the release binary under the rig's scene D
//! (one 1920x1080 pane, KTLX, every layer on, the `ui-sweep` gesture script,
//! NVIDIA RTX 3090 / Vulkan, 58,122 samples on the frame thread) put
//! `place_one` at **26.2 % of the `render_panes` cut** — the largest single
//! item in it — and inside `place_one` the leaves were 20 % `copy_nonoverlapping`
//! and ~22 % libc `malloc`/`free`. The same leg's own always-on ledger
//! (`tile_mesh::ledger::label_anchors_placed`) counted 2,746,425 label anchors
//! over 12,425 frames, so the copies were being paid ~221 times a frame.
//!
//! The field is `Arc<str>`, so both clones are a refcount bump and the name is
//! allocated once, where the tile is styled.
//!
//! # What this gate counts
//!
//! Real `GlobalAlloc` calls at or above the fixture's own name length, taken
//! in three windows on one thread. The instrument knows nothing about labels;
//! it compiles and runs against a tree with the field spelled `String` and
//! disagrees with it, which is what makes it a gate rather than a restatement.
//!
//! The threshold is the fixture's name length and the expected counts are the
//! fixture's label count — both read from the fixture, never written down as a
//! number here.
//!
//! [`the_instrument_sees_a_name_sized_allocation`] is the other arm: it copies
//! the same names the same number of times and the counter must read exactly
//! that. Without it, a zero here would be indistinguishable from a counter
//! that never fires.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

/// One label's name, long enough that no `Vec` the measured windows grow can
/// reach it: the windows place and probe [`LABELS`] labels, and the largest
/// container involved is reserved before the window opens.
const NAME_BYTES: usize = 4096;

/// How many labels the fixture tile carries.
const LABELS: usize = 16;

static BIG_ALLOCS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// Whether **this thread** is inside a measured window. Thread-local
    /// rather than a global flag because libtest runs a binary's tests
    /// concurrently and a global one counts what the other tests allocate at
    /// the same moment. `const`-initialised so reading it inside the allocator
    /// cannot itself allocate.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

/// `try_with`, not `with`: allocations arrive during TLS teardown too, and a
/// destroyed key is not an error here, it is "not measuring".
fn counting() -> bool {
    COUNTING.try_with(Cell::get).unwrap_or(false)
}

fn record(size: usize) {
    if size >= NAME_BYTES && counting() {
        BIG_ALLOCS.fetch_add(1, Ordering::Relaxed);
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

    /// A grow is a fresh block of `new_size`; a shrink is not a new block.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() {
            record(new_size);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// The counter is process-global even though the flag gating it is not, so two
/// threads measuring at once would share it. One window at a time.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Runs `body` with the counter on and hands back `(value, name_sized_allocs)`.
fn allocations_during<T>(body: impl FnOnce() -> T) -> (T, usize) {
    // Bound to a name and taken before the window opens. Never inside an
    // `assert!`: a lock in an assertion's *message* is taken while the
    // condition still holds it, and that hangs instead of reddening.
    let _serialised: MutexGuard<'_, ()> = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    BIG_ALLOCS.store(0, Ordering::Relaxed);
    COUNTING.set(true);
    let value = body();
    COUNTING.set(false);
    (value, BIG_ALLOCS.load(Ordering::Relaxed))
}

/// [`LABELS`] distinct names, each exactly [`NAME_BYTES`] long.
///
/// Distinct because a memo keyed by the text would otherwise answer every
/// probe from one entry and the walk would be one label repeated.
fn names() -> Vec<String> {
    (0..LABELS)
        .map(|i| {
            let mut name = format!("label-{i:04}-");
            name.push_str(&"x".repeat(NAME_BYTES - name.len()));
            assert_eq!(name.len(), NAME_BYTES, "the fixture's own name length");
            name
        })
        .collect()
}

/// The fixture tile: [`LABELS`] labels in tile-extent coordinates.
fn tile_labels() -> Vec<walkers::ShapeOrText> {
    names()
        .into_iter()
        .enumerate()
        .map(|(i, name)| {
            walkers::ShapeOrText::Text(walkers::Text::new(
                egui::pos2(64.0 * i as f32, 32.0 * i as f32),
                name,
                12.0,
                egui::Color32::WHITE,
                0.0,
            ))
        })
        .collect()
}

/// The transform `paint_vector_tile` builds for a tile: `walkers::mvt::placement`
/// over the tile's rect.
fn placement() -> egui::emath::TSTransform {
    walkers::mvt::placement(egui::Rect::from_min_size(
        egui::pos2(120.0, 80.0),
        egui::vec2(512.0, 512.0),
    ))
}

/// A context that has laid out at least one frame, so `galley_cached` has real
/// fonts to answer from.
fn warm_context() -> egui::Context {
    let ctx = egui::Context::default();
    ctx.begin_pass(egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1920.0, 1080.0),
        )),
        ..Default::default()
    });
    let _ = ctx.end_pass();
    ctx
}

/// **The other arm.** Copying the same names the same number of times must
/// read exactly [`LABELS`]; a gate that cannot see the allocation it asserts
/// the absence of is not a gate.
#[test]
fn the_instrument_sees_a_name_sized_allocation() {
    let names = names();
    let (copies, allocs) = allocations_during(|| {
        let mut sink: Vec<String> = Vec::with_capacity(names.len());
        for name in &names {
            sink.push(name.clone());
        }
        sink.len()
    });
    assert_eq!(copies, names.len(), "the control copied every name");
    assert_eq!(
        allocs,
        names.len(),
        "one name-sized allocation per copied name: the instrument fires"
    );
}

/// **Placing a tile's labels copies no names.**
///
/// This is `ui_map_overlays::place_one`'s Text arm: it calls
/// `ShapeOrText::placed` once per label and keeps the result.
#[test]
fn placing_every_label_of_a_tile_allocates_no_names() {
    let shapes = tile_labels();
    let placement = placement();
    // Reserved before the window: the point of the threshold is to select the
    // name blocks, and a `Vec` doubling its way to `LABELS` entries would be
    // counted too on a fixture with shorter names.
    let mut placed: Vec<walkers::ShapeOrText> = Vec::with_capacity(shapes.len());

    let (_, allocs) = allocations_during(|| {
        for shape in &shapes {
            placed.push(shape.placed(placement));
        }
    });

    // The floor under the zero: the walk really did place every label.
    assert_eq!(
        placed.len(),
        shapes.len(),
        "every shape in the fixture was placed"
    );
    for (before, after) in shapes.iter().zip(&placed) {
        let (walkers::ShapeOrText::Text(before), walkers::ShapeOrText::Text(after)) =
            (before, after)
        else {
            panic!("the fixture is labels only");
        };
        assert_eq!(&*after.text, &*before.text, "the name placed is the name");
        assert_eq!(
            after.position,
            placement.scaling * before.position + placement.translation,
            "the anchor is the placed anchor"
        );
    }
    assert_eq!(
        allocs,
        0,
        "placing {} labels allocated {allocs} name-sized blocks; the name is \
         shared, not copied",
        shapes.len()
    );
}

/// **Looking a label's galley up copies no names either.**
///
/// This is `ui_map_overlays::lay_out_label`, which reaches
/// `Text::galley_cached`. The window is the *hit* path — the miss lays the
/// text out and allocates for that, which is the work the memo exists to do
/// once — so the cache is warmed first, exactly as a second frame over an
/// unmoved map finds it.
#[test]
fn probing_the_galley_memo_for_every_label_allocates_no_names() {
    let ctx = warm_context();
    let shapes = tile_labels();
    let mut galleys = walkers::GalleyCache::default();
    galleys.begin_frame(&ctx);

    let texts: Vec<&walkers::Text> = shapes
        .iter()
        .map(|shape| match shape {
            walkers::ShapeOrText::Text(text) => text,
            walkers::ShapeOrText::Shape(_) => panic!("the fixture is labels only"),
        })
        .collect();

    // Warm every entry. This is the miss path and it is not measured.
    for text in &texts {
        let _ = text.galley_cached(&ctx, &mut galleys, ctx.pixels_per_point());
    }
    let warmed = galleys.hits();

    let (_, allocs) = allocations_during(|| {
        for text in &texts {
            let _ = text.galley_cached(&ctx, &mut galleys, ctx.pixels_per_point());
        }
    });

    // The floor under the zero: every probe was answered from the memo, so
    // the window really did run the per-frame path and not the layout.
    assert_eq!(
        galleys.hits() - warmed,
        texts.len() as u64,
        "every probe in the window was a memo hit"
    );
    assert_eq!(
        allocs,
        0,
        "probing the memo for {} labels allocated {allocs} name-sized blocks; \
         the key shares the name rather than copying it",
        texts.len()
    );
}
