//! **The gridded ledger's process-wide reading: that it sums across every
//! worker, and that what it sums is right.**
//!
//! `rasterize_gridded` runs on the offload pool: production writes this ledger
//! from many threads and reads it from one. The ledger keeps two readings for
//! that reason — a thread-local one, exact and private to its caller, and a
//! process-wide one summed across every thread — and only the second is the
//! app's.
//!
//! Until the `gridded scatter:` line landed, nothing read the process-wide one
//! at all, and the only test over this ledger took its delta off
//! `thread_totals`. That is not merely an unread figure but **a test that
//! cannot observe the failure it exists for**: production rasterises on the
//! worker pool while the test reads its own thread, so a regression that broke
//! the cross-thread sum would leave that test green and the app's line flat,
//! and a reader would call the flat line "the change did not move it".
//!
//! # Two claims, and the second is why this file is not just wiring
//!
//! 1. **It sums across workers.** Four spawned threads raster; a fifth reads
//!    and rasters nothing — the arrangement a `thread_totals`-shaped gate
//!    structurally cannot make, because its reader would have to be one of its
//!    writers.
//! 2. **What it sums is the right number.** A ledger nothing has ever read has
//!    never been validated by use, so its figures may be wrong in ways nobody
//!    would have noticed. Three of the four terms have an answer known
//!    independently of the ledger — from the fixture and the call count, not
//!    from the counters — and this asserts them:
//!
//!    * `pictures` = the number of `rasterize_gridded` calls made.
//!    * `picture_px` = `W * H` per call, summed.
//!    * `cells` = `NI * NJ` per call: the fixture's every cell carries an
//!      opaque value and the whole grid lies inside the view, so none is lost
//!      to the no-data, transparent-colour or unprojectable guards.
//!
//!    `written_px` has no closed form — it is the sum of each cell's clipped
//!    rect area, which is the projection's business — so it is bounded from
//!    below by the one quantity computable **outside** the ledger: the ink in
//!    the returned pictures. Every painted pixel took at least one store.
//!
//!    It is **not** bounded below by the cell count, and this file asserted
//!    that it was until the assertion failed on a pristine tree. See the note
//!    at that assertion: a cell counted by the value guards can be clipped to
//!    nothing by a later cell and store no pixels at all.
//!
//! **Its own binary, and exactly one `#[test]`.** The counters are process
//! globals, so a second test in this binary rastering concurrently would make
//! the equalities below inexact; one test per process is what buys `==`
//! instead of `>=`. `gridded_picture_blocks.rs` takes its own binary on the
//! same terms.

use squallar_geo::GeoBounds;
use squallar_overlays::hrrr::GridCoords;
use squallar_overlays::render::rasterize::gridded_ledger::{self, Totals};
use squallar_overlays::render::rasterize::{
    GridWindow, GriddedInput, IndexWindow, rasterize_gridded,
};

const NI: usize = 64;
const NJ: usize = 32;
const W: u32 = 256;
const H: u32 = 128;

/// Threads that raster. More than one is the whole point; four is enough that
/// a dropped global write shows as a shortfall rather than as an off-by-one.
const THREADS: usize = 4;
/// Rasters each thread draws.
const PER_THREAD: usize = 3;

/// Every cell opaque, so `cells` has a known answer. Reflectivity composite
/// paints this range solid, and the `% 10` keeps neighbours different so the
/// picture is still sensitive to which cell won a pixel — a constant field
/// would look the same however the cells were assigned.
fn values(seed: usize) -> Vec<f32> {
    (0..NI * NJ)
        .map(|k| 35.0 + ((k + seed) % 10) as f32)
        .collect()
}

/// A regular grid in the closed form the decoders build, on
/// `gridded_picture_blocks.rs`'s terms. Wholly inside [`bounds`].
fn grid(values: Vec<f32>) -> GriddedInput {
    GriddedInput::Window(GridWindow {
        field: squallar_overlays::mrms::fields::spec(
            squallar_overlays::mrms::MrmsProduct::ReflectivityComposite,
        )
        .id
        .clone(),
        ni: NI,
        nj: NJ,
        coords: GridCoords::Regular {
            lat0: 54.95,
            lon0: -129.95,
            dlat: -0.1,
            dlon: 0.1,
            ni: NI,
            nj: NJ,
            scan_mode: 0,
        },
        win: IndexWindow {
            i0: 0,
            i1: NI,
            j0: 0,
            j1: NJ,
        },
        values: squallar_overlays::render::gridded::GridValues::F32(values),
    })
}

fn bounds() -> GeoBounds {
    GeoBounds {
        min_lon: -130.0,
        max_lon: -60.0,
        min_lat: 20.0,
        max_lat: 55.0,
    }
}

/// Output pixels that would change the frame they were drawn on. **Coverage,
/// never stores** — this is the independent lower bound on `written_px`, and
/// conflating the two is the confusion the ledger's own module doc warns about.
fn painted(rgba: &[u8]) -> u64 {
    rgba.chunks_exact(4).filter(|px| px[3] != 0).count() as u64
}

/// Field-by-field addition, so the expected process-wide figure is built from
/// the workers' own readings rather than from a count asserted twice.
fn add(a: Totals, b: &Totals) -> Totals {
    Totals {
        pictures: a.pictures + b.pictures,
        cells: a.cells + b.cells,
        written_px: a.written_px + b.written_px,
        picture_px: a.picture_px + b.picture_px,
    }
}

/// One worker's exact, thread-private reading plus the ink it produced.
struct Worked {
    delta: Totals,
    painted: u64,
}

#[test]
fn the_process_wide_reading_is_every_workers_rasters_summed() {
    // The reading thread rasters nothing, and that is load-bearing: it is what
    // makes the process-wide and thread-local readings distinguishable at all.
    let reader_before = gridded_ledger::thread_totals();
    let before = gridded_ledger::totals();

    let workers: Vec<Worked> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                scope.spawn(move || {
                    let input = grid(values(t));
                    let win = bounds();
                    let mine_before = gridded_ledger::thread_totals();
                    let mut ink = 0;
                    for _ in 0..PER_THREAD {
                        let out = rasterize_gridded(&input, &win, W, H);
                        ink += painted(out.rgba.as_bytes());
                    }
                    Worked {
                        delta: gridded_ledger::thread_totals().since(&mine_before),
                        painted: ink,
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("a rasterising worker panicked"))
            .collect()
    });

    let moved = gridded_ledger::totals().since(&before);
    let calls = (THREADS * PER_THREAD) as u64;
    let ink: u64 = workers.iter().map(|w| w.painted).sum();

    // ── Anti-vacuity, before any equality is believed ─────────────────────
    assert!(
        ink > 0,
        "the fixture painted nothing across {calls} rasters; every figure \
         below would be a reading of an empty picture",
    );

    // ── Claim 2: the terms with an answer known outside the ledger ────────
    assert_eq!(
        moved.pictures, calls,
        "the ledger counted {} pictures over {calls} `rasterize_gridded` \
         calls; `pictures` is one per call by construction",
        moved.pictures,
    );
    assert_eq!(
        moved.picture_px,
        calls * u64::from(W) * u64::from(H),
        "the ledger's denominator is {} over {calls} rasters of {W}x{H}; \
         `picture_px` is the pictures' own sizes summed, so this term is \
         arithmetic and cannot depend on the projection",
        moved.picture_px,
    );
    assert_eq!(
        moved.cells,
        calls * (NI * NJ) as u64,
        "the ledger counted {} painted cells over {calls} rasters of a \
         {NI}x{NJ} grid whose every cell is opaque and inside the view. A \
         shortfall means cells are being lost to a guard this fixture was \
         built to clear, not that the count is differently defined",
        moved.cells,
    );

    // `written_px` has no closed form. Its one sound floor is the ink, counted
    // from the returned pictures and so computed outside the ledger entirely.
    assert!(
        moved.written_px >= ink,
        "the ledger recorded {} pixel stores but the pictures carry {ink} \
         painted pixels. Every painted pixel took at least one store, so a \
         `written_px` below the ink is an undercount",
        moved.written_px,
    );
    // **`written_px >= cells` is NOT an invariant, and asserting it here was
    // wrong.** Measured on this fixture: 4608 stores against 24576 counted
    // cells. `drawn_cells` is incremented where a cell passes the no-data,
    // transparent-colour and unprojectable guards (`rasterize.rs`), which is
    // *before* `emit_cell_row` clips it. `CellRect::clip_x` sets
    // `x1 = x0 - 1` when a later cell covers this one outright, and `fill`
    // then returns on `is_empty()` having added nothing. So a counted cell can
    // write zero pixels, and at sub-pixel spacing — this fixture, and the
    // zoomed-out viewport the overdraw work was cut from — most do. That is
    // the clip optimisation working, not a miscount, and the ledger's two
    // terms answer different questions: `cells` is cells the *values* admitted,
    // `written_px` is stores the *geometry* survived.

    // ── Claim 1: the global is the sum across threads ─────────────────────
    let expected = workers
        .iter()
        .map(|w| &w.delta)
        .fold(Totals::default(), add);
    assert_eq!(
        moved, expected,
        "the process-wide ledger moved by {moved:?} while the {THREADS} \
         workers' own thread-local readings sum to {expected:?}. These must be \
         the same figure: production rasterises on the offload pool and the \
         app's `gridded scatter:` line reads the process-wide counters, so a \
         global write that is dropped, or a `totals()` that answers from the \
         caller's thread, makes that line report a fraction of the work and \
         read as a fall that never happened",
    );

    // The conjunct a thread-local reading cannot satisfy: the reader rastered
    // nothing, so a `totals()` that answered from the calling thread would be
    // flat here while twelve pictures had just been drawn.
    assert_eq!(
        gridded_ledger::thread_totals().since(&reader_before),
        Totals::default(),
        "the reading thread rastered nothing, so its own thread-local ledger \
         must not have moved; if it did, this gate is no longer distinguishing \
         the process-wide sum from the caller's own thread",
    );
    assert!(
        moved.pictures > 0,
        "the process-wide ledger did not move while {THREADS} workers rastered \
         {PER_THREAD} pictures each — the reading is the calling thread's, not \
         the process's",
    );

    // ── The derived figures the app's line prints ─────────────────────────
    let now = gridded_ledger::totals();
    assert!(
        now.overdraw().is_some_and(|r| r > 0.0),
        "the overdraw ratio is unreadable ({} written over {} picture) after a \
         run that rastered; the line would print `overdraw unread` on a scene \
         that really did scatter",
        now.written_px,
        now.picture_px,
    );
    assert!(
        now.per_cell().is_some_and(|r| r > 0.0),
        "written-per-cell is unreadable after a run that painted cells",
    );

    // ── The per-field split, on the same rasters ──────────────────────────
    //
    // One field is the whole fixture, so the split has a known answer: the
    // field's row IS the whole reading, and its `cells` term is the same
    // `moved.cells` asserted above. The claim is not that the numbers agree by
    // accident but that `by_field` is written from the same place and on the
    // same population — a split written from a different site, or one arm
    // short, is what this catches.
    let by_field = gridded_ledger::by_field();
    let field = squallar_overlays::mrms::fields::spec(
        squallar_overlays::mrms::MrmsProduct::ReflectivityComposite,
    )
    .id
    .clone();
    let row = by_field
        .iter()
        .find(|(name, _)| name == field.as_str())
        .map(|(_, t)| *t)
        .unwrap_or_else(|| {
            panic!(
                "`by_field` has no row for `{}` after {calls} rasters of it; it                  holds {by_field:?}. An absent field is the reading that says                  nothing ever drew this source's grid, which is exactly the                  claim a missing write would make falsely",
                field.as_str(),
            )
        });
    assert_eq!(
        (row.pictures, row.cells),
        (moved.pictures, moved.cells),
        "the per-field split reads {row:?} where the process-wide ledger moved          by {} pictures / {} cells over the one field this fixture draws. The          two are written from the same site and must sum to the same          population; the app prints them as one line's total and its split",
        moved.pictures,
        moved.cells,
    );
    assert_eq!(
        by_field.len(),
        1,
        "`by_field` holds {} fields after a run that drew exactly one:          {by_field:?}. A field with a row nothing drew would read as a          consumer that fired",
        by_field.len(),
    );
}
