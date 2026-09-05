//! **How many blocks a GLM poll asks the allocator for, and how many it hands
//! back on the thread that installed it.**
//!
//! A busy 20 s GLM poll delivers on the order of 125,000 flashes. The handler
//! used to turn each one into its own `Arc<GlmFlashItem>` at
//! `apply_fetch_result`, so that `hit_items` could hand the hit map a list of
//! pointers — one allocation per flash on arrival, and one free per flash when
//! the next poll's `self.data = data` dropped the previous granule. Both land
//! on the frame thread, and a click reads exactly one element of that list.
//!
//! Its own binary with a counting `#[global_allocator]`, for the reason
//! `hitmap_id_lookup.rs` and `mrms_staging_blocks.rs` both set out: the
//! instrument counts real `GlobalAlloc` calls and knows nothing about GLM, so
//! it compiles and runs against the **unmodified** tree and is able to disagree
//! with the fix.
//!
//! **The denominator is stated in every assertion.** Both windows below are
//! measured with a granule of `FLASHES` rows already resident, because it is
//! the *replacement* that is the poll: installing over a previous granule is
//! where the frees are. The rows themselves are built outside the window — the
//! parse allocates that `Vec` either way and the handler moves it rather than
//! copying it — so what the counters see is exactly the churn the
//! representation decides.
//!
//! **Every block, at every size.** An earlier draft of this file counted only
//! blocks under 1 KiB, which put the one figure that matters for `hit_items` —
//! a 320,000-byte run of pointers, one per flash — on the wrong side of the
//! filter and made that arm read `0` on both trees. A count that is zero
//! whatever the code does is not a measurement. Hence the byte figure as well
//! as the call count: for `hit_items` the calls differ by one and the bytes by
//! five orders of magnitude.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::thread::LocalKey;

use squallar_overlays::glm::{
    GlmDataLevel, GlmFetchOutcome, GlmFetchResult, GlmFlash, GlmSatellite, RecordDrops,
};
use squallar_overlays::render::overlay_state::{
    OverlayFetchResult, OverlayRegistry, PaneMut, PaneRef,
};
use squallar_source::id::known;

/// Enough rows that a per-row cost cannot hide in the noise of the fixed ones,
/// and small enough to run in a debug build in well under a second. The shipped
/// figure is ~125,000; the assertions below are per flash, so the count they
/// gate does not depend on this.
const FLASHES: usize = 20_000;

thread_local! {
    /// Thread-local, not a global flag: libtest runs this binary's tests
    /// concurrently and a global one would count whatever another test happened
    /// to be allocating. `const`-initialised so reading it inside the allocator
    /// cannot itself allocate.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    /// **And so are the counters, for the same reason the flag is.** They were
    /// global atomics that every window `store(0)`d before it ran: the flag
    /// kept a concurrent test's *allocations* out of the total, but nothing
    /// kept its **reset** out, so two measured windows overlapping zeroed each
    /// other mid-count. Caught by a tamper whose second dispatch printed 144
    /// bytes where it had just copied 20,000 rows — a figure that would have
    /// read as a passing bar.
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static FREES: Cell<usize> = const { Cell::new(0) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
    static BYTES_FREED: Cell<usize> = const { Cell::new(0) };
}

fn counting() -> bool {
    COUNTING.try_with(Cell::get).unwrap_or(false)
}

/// Add to one counter, tolerating a thread whose TLS is already torn down.
fn bump(counter: &'static LocalKey<Cell<usize>>, by: usize) {
    let _ = counter.try_with(|c| c.set(c.get().wrapping_add(by)));
}

fn read(counter: &'static LocalKey<Cell<usize>>) -> usize {
    counter.try_with(Cell::get).unwrap_or(0)
}

struct SmallBlocks;

unsafe impl GlobalAlloc for SmallBlocks {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if counting() {
            bump(&ALLOCS, 1);
            bump(&BYTES, layout.size());
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if counting() {
            bump(&ALLOCS, 1);
            bump(&BYTES, layout.size());
        }
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if counting() {
            bump(&FREES, 1);
            bump(&BYTES_FREED, layout.size());
        }
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if counting() {
            bump(&ALLOCS, 1);
            bump(&BYTES, new_size);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static A: SmallBlocks = SmallBlocks;

/// Blocks taken, blocks handed back, bytes taken, bytes handed back.
///
/// **The four counters are thread-local, and that is what makes one window
/// independent of another.** They were process-global statics gated by a
/// thread-local `COUNTING` flag: the flag decided WHICH thread's allocations
/// were counted and could not keep two counting threads apart, so two windows
/// open at once on the harness's several threads reset and added to each
/// other's figures, and `a_resident_granule_is_the_rows_and_little_else`
/// subtracted a `BYTES_FREED` its own window never took and panicked on the
/// overflow. Per-thread counters answer that at the counter rather than by
/// serialising the windows: a window can only ever see its own thread's
/// figures, so there is nothing left for a lock to order.
///
/// All four readings are still taken before the caller gets control back,
/// which is the second half of the same rule: a figure read after the window
/// closes is a figure the next window on this thread may already have reset.
fn blocks_during<T>(f: impl FnOnce() -> T) -> (T, usize, usize, usize, usize) {
    for counter in [&ALLOCS, &FREES, &BYTES, &BYTES_FREED] {
        counter.with(|c| c.set(0));
    }
    COUNTING.with(|c| c.set(true));
    let out = f();
    COUNTING.with(|c| c.set(false));
    (
        out,
        read(&ALLOCS),
        read(&FREES),
        read(&BYTES),
        read(&BYTES_FREED),
    )
}

fn now() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 24)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
}

/// One poll's payload, built **outside** every measured window: the rows
/// themselves are the parse's cost and are not what changed.
fn a_granule(n: usize) -> OverlayFetchResult {
    let flashes: Vec<GlmFlash> = (0..n)
        .map(|i| GlmFlash {
            lat: 33.0 + (i % 400) as f64 * 0.01,
            lon: -99.0 + (i % 300) as f64 * 0.01,
            energy: Some(1e-14),
            area: None,
            time: now() - chrono::Duration::seconds((i % 250) as i64),
            satellite: GlmSatellite::GoesEast,
            level: GlmDataLevel::Flash,
        })
        .collect();
    OverlayFetchResult {
        kind: known::LIGHTNING,
        data: Box::new(GlmFetchResult(Ok(GlmFetchOutcome {
            flashes,
            dead_feeds: Vec::new(),
            queried: Vec::new(),
            parse_failures: None,
            transport_failures: None,
            level_failures: Vec::new(),
            evaluated_levels: Vec::new(),
            listing_failures: Vec::new(),
            window_gaps: Vec::new(),
            record_drops: RecordDrops::default(),
        }))),
    }
}

fn a_registry_holding_a_granule() -> OverlayRegistry {
    let mut registry = OverlayRegistry::default();
    registry.set_enabled(&known::LIGHTNING, true, &mut PaneMut::bare(0));
    registry.apply_fetch_result(a_granule(FLASHES), &PaneRef::bare(0));
    assert_eq!(
        registry.item_count(&known::LIGHTNING, &PaneRef::bare(0)),
        FLASHES,
        "premise: the fixture granule reached the handler",
    );
    registry
}

/// **The poll.** Installing a granule over a resident one is one block taken
/// and one handed back — the `Arc` around the rows — whatever the flash count.
#[test]
fn installing_a_granule_over_another_is_a_constant_number_of_blocks() {
    let mut registry = a_registry_holding_a_granule();
    let next = a_granule(FLASHES);

    let ((), allocs, frees, bytes, _freed) = blocks_during(|| {
        registry.apply_fetch_result(next, &PaneRef::bare(0));
    });

    assert_eq!(
        registry.item_count(&known::LIGHTNING, &PaneRef::bare(0)),
        FLASHES,
        "the measured install must have landed, or the counts below are of \
         nothing",
    );
    // Printed whether or not the bar below is what fails: the figure is the
    // point, and a bar tells a reader only which side of it the run landed on.
    println!(
        "install over a resident granule: {allocs} blocks taken, {frees} \
         handed back, {bytes} bytes, over {FLASHES} flashes",
    );
    assert!(
        allocs <= 8,
        "installing {FLASHES} flashes over {FLASHES} resident ones took \
         {allocs} blocks ({bytes} bytes). One `Arc` around the granule's own \
         `Vec` is one; a block per flash is {FLASHES}, which is what one \
         `Arc<GlmFlashItem>` per row costs at every 20 s poll.",
    );
    assert!(
        frees <= 8,
        "installing {FLASHES} flashes over {FLASHES} resident ones handed back \
         {frees} blocks. These frees run wherever the install runs, which is \
         the frame thread; a free per retired flash is {FLASHES} of them \
         there.",
    );
}

/// **The dispatch.** Capturing the page-side half of a hit map is one refcount
/// bump on the slab, not a list of one pointer per flash — and `hit_items` is
/// called on the frame thread once per overlay render, far more often than a
/// poll lands.
#[test]
fn capturing_the_hit_items_of_a_granule_is_a_constant_number_of_blocks() {
    let registry = a_registry_holding_a_granule();

    let (items, allocs, _frees, bytes, _freed) =
        blocks_during(|| registry.hit_items(&known::LIGHTNING).expect("seeded"));

    assert_eq!(
        items.len(),
        FLASHES,
        "the captured id space must still cover every row, or a cell naming a \
         late flash resolves to nothing",
    );
    println!(
        "hit_items capture: {allocs} blocks taken, {bytes} bytes, over \
         {FLASHES} flashes",
    );
    assert!(
        bytes <= 1024,
        "capturing the hit items of {FLASHES} flashes asked the allocator for \
         {bytes} bytes in {allocs} block(s); a list of one pointer per flash \
         is {} bytes of pointers alone, and `hit_items` runs on the frame \
         thread once per overlay render rather than once per poll",
        FLASHES * std::mem::size_of::<usize>() * 2,
    );
}

/// **What a resident granule costs in bytes**, which is a different question
/// from how many blocks it took to get there: the payload is built inside the
/// window as well as installed, so the net — bytes taken less bytes handed back
/// — is everything one poll leaves on the heap.
#[test]
fn a_resident_granule_is_the_rows_and_little_else() {
    let mut registry = OverlayRegistry::default();
    registry.set_enabled(&known::LIGHTNING, true, &mut PaneMut::bare(0));

    let ((), _allocs, _frees, taken, handed_back) = blocks_during(|| {
        registry.apply_fetch_result(a_granule(FLASHES), &PaneRef::bare(0));
    });
    let net = taken.saturating_sub(handed_back);

    assert_eq!(
        registry.item_count(&known::LIGHTNING, &PaneRef::bare(0)),
        FLASHES,
        "the measured install must have landed",
    );
    println!(
        "resident granule: {net} bytes net for {FLASHES} flashes = {} bytes \
         per flash",
        net as f64 / FLASHES as f64,
    );
    // A `GlmFlash` is 48 bytes with its padding, so the rows alone are 48 per
    // flash and nothing else this layer holds scales with the flash count. The
    // bar is 64 rather than 48 so that a row growing a field is not a red gate
    // on its own; a per-flash heap block on top of the rows is 72 bytes and
    // lands well above it.
    assert!(
        net < FLASHES * 64,
        "a resident granule of {FLASHES} flashes is {net} bytes, {} per flash \
         — the rows themselves are 48 per flash, so anything approaching 120 \
         is a second per-flash block beside them",
        net as f64 / FLASHES as f64,
    );
}

/// **The other direction.** A slab that resolved hits lazily but resolved them
/// *wrongly* would satisfy both counts above. Every row must still answer its
/// own flash, at its own index, with the position the rasterizer recorded.
#[test]
fn every_row_still_resolves_to_its_own_flash() {
    let registry = a_registry_holding_a_granule();
    let items = registry.hit_items(&known::LIGHTNING).expect("seeded");
    let prefs = squallar_units::UserPreferences::default();

    for i in [0usize, 1, 2, 7, 999, FLASHES / 2, FLASHES - 2, FLASHES - 1] {
        let item = items.get(i).unwrap_or_else(|| panic!("row {i} resolves"));
        assert_eq!(item.layer_id(), known::LIGHTNING);
        let grid = match &item.popup_content(&prefs).sections[1] {
            squallar_overlays::render::overlay_state::PopupSection::KeyValueGrid(g) => g.clone(),
            _ => panic!("row {i} popup section 1 is not the key-value grid"),
        };
        let want_lat = format!("{:.4}°", 33.0 + (i % 400) as f64 * 0.01);
        let want_lon = format!("{:.4}°", -99.0 + (i % 300) as f64 * 0.01);
        assert!(
            grid.contains(&("Latitude".to_string(), want_lat.clone())),
            "row {i} resolved to a flash at another latitude: {grid:?} wanted \
             {want_lat}",
        );
        assert!(
            grid.contains(&("Longitude".to_string(), want_lon.clone())),
            "row {i} resolved to a flash at another longitude: {grid:?} wanted \
             {want_lon}",
        );
    }

    assert!(
        items.get(FLASHES).is_none(),
        "an index past the end must answer nothing rather than wrap or panic",
    );
}

// ── The dispatch's paint rows ─────────────────────────────────────────

/// The dispatch context two overlay renders of an unmoved map supply. Only
/// `zoom` differs between the two below, because a real pair of dispatches
/// under a wheel gesture differs by exactly that and the rows must not care.
fn a_ctx(zoom: f64) -> squallar_overlays::render::overlay_state::RasterizeContext {
    squallar_overlays::render::overlay_state::RasterizeContext {
        is_dark: false,
        zoom,
        device_scale: 1.0,
        now: now(),
        as_of: now(),
        frame: None,
    }
}

fn flash_rows(
    job: &squallar_source::job::DescribedJob,
) -> &std::sync::Arc<Vec<squallar_overlays::render::rasterize::FlashPaint>> {
    &job.downcast_ref::<squallar_overlays::render::rasterize::GlmStrikesInput>()
        .expect("the dispatch described a GLM job")
        .flashes
}

/// **The dispatch.** `prepare_job` used to turn every flash of the resident
/// granule into its own `FlashPaint` row and `collect` them into a fresh
/// `Vec`, on the frame thread, on every overlay render — 40 bytes a flash,
/// ~5 MB at the ~125,000 a busy poll delivers, for rows that are a function
/// of the granule alone.
///
/// A second dispatch whose picture has not moved must copy **nothing**: the
/// rows are built once per granule and handed out as a refcount clone, and
/// what is built per dispatch is the five scalars around them.
#[test]
fn a_second_dispatch_of_one_granule_copies_no_flashes() {
    let registry = a_registry_holding_a_granule();
    let pane = PaneRef::bare(0);

    let (first, first_allocs, _, first_bytes, _freed) =
        blocks_during(|| registry.prepare_job(&known::LIGHTNING, &a_ctx(7.0), &pane));
    let first = first.expect("a granule is resident, so the layer describes a job");

    let (second, allocs, _frees, bytes, _freed) =
        blocks_during(|| registry.prepare_job(&known::LIGHTNING, &a_ctx(7.5), &pane));
    let second = second.expect("the second dispatch describes a job too");

    // Printed before any bar, for the reason this file's header gives: a bar
    // tells a reader only which side of it the run landed on.
    println!(
        "prepare_job, first dispatch of a granule: {first_allocs} blocks, \
         {first_bytes} bytes over {FLASHES} flashes = {:.1} bytes per flash",
        first_bytes as f64 / FLASHES as f64,
    );
    println!(
        "prepare_job, second dispatch, picture unmoved: {allocs} blocks, \
         {bytes} bytes over {FLASHES} flashes = {:.2} bytes per flash",
        bytes as f64 / FLASHES as f64,
    );
    assert_eq!(
        flash_rows(&second).len(),
        FLASHES,
        "every row must still travel, or the counts above are of a shorter \
         picture",
    );
    assert!(
        bytes <= 1024,
        "a second dispatch of the same granule asked the allocator for \
         {bytes} bytes in {allocs} block(s), over {FLASHES} flashes. A row \
         per flash is {} bytes ({} per flash), copied on the frame thread \
         once per overlay render; the only block this dispatch needs is the \
         `Arc` the described job itself is",
        FLASHES * std::mem::size_of::<squallar_overlays::render::rasterize::FlashPaint>(),
        std::mem::size_of::<squallar_overlays::render::rasterize::FlashPaint>(),
    );
    assert!(
        std::sync::Arc::ptr_eq(flash_rows(&first), flash_rows(&second)),
        "the two dispatches must share ONE row allocation; an equal-but-\
         separate `Vec` is the copy this test exists to catch",
    );
}

/// **The other direction, and the instrument's own liveness.** The rows must
/// rebuild when the granule moves — a memo that never missed would draw the
/// previous poll's lightning forever — and the same counting window that
/// reads ~0 above must be able to read the whole copy here, or its zero says
/// nothing.
#[test]
fn a_dispatch_after_a_poll_rebuilds_the_rows_and_the_counter_sees_it() {
    let mut registry = a_registry_holding_a_granule();
    let pane = PaneRef::bare(0);
    let before = registry
        .prepare_job(&known::LIGHTNING, &a_ctx(7.0), &pane)
        .expect("a granule is resident");

    registry.apply_fetch_result(a_granule(FLASHES), &pane);

    let (after, allocs, _frees, bytes, _freed) =
        blocks_during(|| registry.prepare_job(&known::LIGHTNING, &a_ctx(7.0), &pane));
    let after = after.expect("the new granule describes a job");

    assert!(
        !std::sync::Arc::ptr_eq(flash_rows(&before), flash_rows(&after)),
        "a poll must rebuild the rows: sharing them with the retired granule \
         would draw the retired granule",
    );
    println!(
        "prepare_job, first dispatch after a poll: {allocs} blocks, {bytes} \
         bytes over {FLASHES} flashes = {:.1} bytes per flash",
        bytes as f64 / FLASHES as f64,
    );
    let row = std::mem::size_of::<squallar_overlays::render::rasterize::FlashPaint>();
    assert!(
        bytes >= FLASHES * row,
        "the rebuild after a poll asked for {bytes} bytes over {FLASHES} \
         flashes; the rows alone are {} — a window that cannot see the copy \
         here cannot be quoted for the ~0 the unmoved dispatch reads",
        FLASHES * row,
    );
}

/// **What the memo keeps alive, which is not nothing.**
///
/// The memoised value is a built row set, not a handle onto something the hit
/// map already holds, so the layer's resident bytes grow by it: one live row
/// set for the current generation, plus the one row set a rollover parks for
/// a discard seam that has no production drainer yet. Both are `Arc`s over a
/// single block of `FlashPaint`, and the rollover *replaces* the parked slot
/// rather than pushing to it, so the parked half is one row set at steady
/// state and not one per poll.
///
/// Measured as the net of a poll-and-dispatch window — everything allocated
/// inside it, so what the window leaves behind is what the layer now holds.
/// The claim under test is **flat, not small**: the fourth and fifth polls
/// must leave no more behind than the second did.
#[test]
fn the_memo_keeps_a_bounded_number_of_row_sets_however_many_polls_land() {
    let mut registry = OverlayRegistry::default();
    registry.set_enabled(&known::LIGHTNING, true, &mut PaneMut::bare(0));
    let pane = PaneRef::bare(0);

    let mut cumulative: i64 = 0;
    let mut per_poll: Vec<i64> = Vec::new();
    for _ in 0..6 {
        let ((), _allocs, _frees, taken, handed_back) = blocks_during(|| {
            registry.apply_fetch_result(a_granule(FLASHES), &pane);
            drop(registry.prepare_job(&known::LIGHTNING, &a_ctx(7.0), &pane));
        });
        let net = taken as i64 - handed_back as i64;
        cumulative += net;
        per_poll.push(net);
    }

    println!(
        "resident after 6 polls, each dispatched: {cumulative} bytes over \
         {FLASHES} flashes = {:.1} bytes per flash; per-poll net {per_poll:?}",
        cumulative as f64 / FLASHES as f64,
    );
    let row = std::mem::size_of::<squallar_overlays::render::rasterize::FlashPaint>() as i64;
    for (i, net) in per_poll.iter().enumerate().skip(2) {
        assert!(
            *net < FLASHES as i64 * row / 2,
            "poll {} left {net} bytes behind over {FLASHES} flashes; past the \
             first two the residency must be flat, and a poll that keeps half \
             a row set ({} bytes) is the memo growing with the poll count \
             rather than holding a bounded number of row sets",
            i + 1,
            FLASHES as i64 * row / 2,
        );
    }
    assert!(
        cumulative < FLASHES as i64 * 160,
        "the layer holds {cumulative} bytes over {FLASHES} flashes ({:.1} per \
         flash) after six polls. The budget is the 48-byte slab row plus at \
         most two 40-byte paint row sets — the live one and the parked one — \
         which is 128 per flash",
        cumulative as f64 / FLASHES as f64,
    );
}
