//! The pool's own invariants, over buffers rather than over granules.
//!
//! What is checked here is the half that decides whether a retained buffer
//! can ever hand a grid the wrong bytes, at a width a test can allocate many
//! times over. The GMGSI instance checks the same invariants at the shipped
//! width in `gmgsi::staging::tests` — and, since the product's grid moved,
//! at the width the product actually publishes — and the decode gates live in
//! `tests/gmgsi_staging_blocks.rs`, where a counting global allocator watches
//! the real shipped path.

use super::*;

const POINTS: usize = 4096;
/// A second shape, for the granule that arrives at a width the slot is not
/// holding. Not a divisor or a multiple of [`POINTS`], so nothing can pass by
/// arithmetic coincidence.
const OTHER_POINTS: usize = 4093;

fn buffer<T: Copy>() -> Vec<T> {
    sized(POINTS)
}

fn sized<T: Copy>(points: usize) -> Vec<T> {
    let mut v: Vec<T> = Vec::new();
    v.try_reserve_exact(points).expect("fits");
    v
}

/// **The whole point, at the pool's own level**: hand a buffer back and the
/// next grid-sized decode is given that buffer instead of a new one.
#[test]
fn a_returned_buffer_is_the_next_decodes_buffer() {
    let pool: StagingPool<f32> = StagingPool::new(POINTS);

    let first = pool.take(POINTS).expect("a cold pool allocates");
    let address = first.as_ptr() as usize;
    assert_eq!(
        pool.totals(),
        StagingTotals {
            allocated: 1,
            reused: 0,
            declined: 0
        },
        "a cold pool has nothing to hand out and must say so",
    );
    assert_eq!(
        pool.retained_bytes(),
        0,
        "nothing is parked while it is out"
    );

    pool.give(first);
    assert_eq!(
        pool.retained_bytes(),
        POINTS * size_of::<f32>(),
        "the level reads the parked buffer's capacity in bytes",
    );
    let second = pool.take(POINTS).expect("the slot is full");
    assert_eq!(
        second.as_ptr() as usize,
        address,
        "the second grid must be decoded into the FIRST one's block. A pool \
         that answered with a fresh allocation of the same size would satisfy \
         every capacity assertion in this file and leave the fragmentation \
         this module exists to remove exactly as it was",
    );
    assert_eq!(
        pool.totals(),
        StagingTotals {
            allocated: 1,
            reused: 1,
            declined: 0
        },
    );
    assert_eq!(pool.resizes(), 0, "and one shape throughout is no resize");
    assert!(
        second.is_empty(),
        "a buffer comes out of the slot with nothing in it, whatever it held",
    );
    assert_eq!(second.capacity(), POINTS);
    assert_eq!(pool.retained_bytes(), 0);
}

/// **A grid that is not the retained buffer's shape never touches it.**
///
/// Every byte budget in this crate is spent against `len * size_of::<T>()`,
/// so a 400-byte grid handed the pooled block would report 400 bytes while
/// holding the whole slot, and the cache would go on filling until the tab
/// died. Matching capacity *exactly* rather than "≥" is what forbids it, and
/// that half is unchanged by the shape key.
///
/// **What the shape key changed** is the other half, and this pins the new
/// relation: the arriving shape wins the slot rather than being locked out of
/// it. The old rule kept the pooled buffer parked and let the mismatched grid
/// allocate — which reads as protecting the big block, and is in fact how a
/// pool declared at a width its product no longer publishes stays inert for
/// the life of the process, refusing every offer at the real shape.
#[test]
fn a_grid_of_another_shape_is_never_given_the_pooled_buffer() {
    let pool: StagingPool<f32> = StagingPool::new(POINTS);
    pool.give(buffer());
    let parked = pool.retained_points();
    assert_eq!(parked, POINTS, "premise: the slot is holding the big shape");

    let small = pool.take(100).expect("a small grid always allocates");
    assert_eq!(
        small.capacity(),
        100,
        "the small grid gets exactly its own points and never the pooled block",
    );
    assert_eq!(
        pool.totals(),
        StagingTotals {
            allocated: 1,
            reused: 0,
            declined: 0
        },
    );
    assert_eq!(
        pool.resizes(),
        1,
        "and the mismatch is counted rather than absorbed: this is the one \
         event that says a product's grid moved under a build",
    );
    assert_eq!(
        pool.retained_points(),
        0,
        "the buffer for the shape nobody is asking for is dropped, so the \
         arriving shape can become the retained one. Keeping it parked is \
         what left the shipped pool inert",
    );

    pool.give(small);
    assert_eq!(pool.retained_points(), 100, "which the next offer proves");
    let pooled = pool.take(100).expect("the slot holds the new shape now");
    assert_eq!(pool.totals().reused, 1);
    drop(pooled);
}

/// And the same safety rule on the way in: a buffer of another capacity is
/// **retained at its own capacity** and is never handed to a grid of the
/// shape the pool was declared for.
///
/// The old spelling of this test asserted the offer was *refused*. That is the
/// premise the shipped defect falsified: the offers GMGSI actually makes are
/// 14,997,000 points against a pool declared at 15,000,000, so refusing them
/// is refusing every real granule of the product.
#[test]
fn a_buffer_of_another_capacity_is_retained_but_never_handed_to_another_shape() {
    let pool: StagingPool<f32> = StagingPool::new(POINTS);
    pool.give(sized::<f32>(OTHER_POINTS));
    assert_eq!(
        pool.totals(),
        StagingTotals {
            allocated: 0,
            reused: 0,
            declined: 0
        },
        "an offer at the shape the product publishes is taken, whatever the \
         constant this pool was declared with says",
    );
    assert_eq!(pool.retained_points(), OTHER_POINTS);
    assert_eq!(
        pool.nominal_points(),
        POINTS,
        "and the nominal has not moved"
    );
    assert_eq!(
        pool.retained_bytes(),
        OTHER_POINTS * size_of::<f32>(),
        "the level reads the block the allocator is actually holding",
    );

    let fresh = pool.take(POINTS).expect("allocates");
    assert_eq!(
        fresh.capacity(),
        POINTS,
        "a grid of the nominal shape gets its own exact block and never the \
         shorter retained one",
    );
    assert_eq!(pool.totals().allocated, 1);
    assert_eq!(pool.totals().reused, 0);
    assert_eq!(pool.resizes(), 1);
    drop(fresh);
}

/// **One slot, not a free list.** The second buffer offered while the first
/// is still waiting is dropped: the budget this pool implements is one grid,
/// and a pool that grew without bound would be a memory leak sold as a fix.
#[test]
fn the_slot_holds_one_buffer_and_refuses_the_second() {
    let pool: StagingPool<f32> = StagingPool::new(POINTS);
    pool.give(buffer());
    pool.give(buffer());
    assert_eq!(
        pool.totals(),
        StagingTotals {
            allocated: 0,
            reused: 0,
            declined: 1
        },
        "the second offer is declined, and says so rather than growing the pool",
    );
}

/// A `Vec` that owns no allocation is not a buffer, and parking one would make
/// the slot read full while holding nothing.
#[test]
fn an_offer_with_no_allocation_behind_it_is_declined() {
    let pool: StagingPool<f32> = StagingPool::new(POINTS);
    pool.give(Vec::new());
    assert_eq!(pool.totals().declined, 1);
    assert_eq!(pool.retained_points(), 0, "and the slot is still empty");
    pool.give(buffer());
    assert_eq!(
        pool.retained_points(),
        POINTS,
        "so a real buffer still gets in",
    );
}

/// The element type is a parameter: a `u16` pool deals in `u16` capacities
/// and reports `u16` bytes, so a narrower source is not priced as `f32`.
#[test]
fn the_pool_is_generic_over_its_element() {
    let pool: StagingPool<u16> = StagingPool::new(POINTS);
    let mut v = pool.take(POINTS).expect("allocates");
    v.extend(std::iter::repeat_n(0xBEEFu16, POINTS));
    pool.give(v);
    assert_eq!(pool.retained_bytes(), POINTS * size_of::<u16>());
    let again = pool.take(POINTS).expect("reused");
    assert!(again.is_empty(), "cleared on the way in and on the way out");
    assert_eq!(again.capacity(), POINTS);
    assert_eq!(pool.totals().reused, 1);
}

/// A miss reported through [`StagingPool::decline`] lands on the same ledger
/// as one the slot refused itself, so a source's `Arc::into_inner` misses are
/// not a second, unread figure.
#[test]
fn an_external_decline_is_counted_with_the_rest() {
    let pool: StagingPool<f32> = StagingPool::new(POINTS);
    pool.decline();
    assert_eq!(
        pool.totals(),
        StagingTotals {
            allocated: 0,
            reused: 0,
            declined: 1
        },
    );
}

/// **Two shapes alternating over one slot corrupt nothing.**
///
/// Not a shipped sequence — one pool serves one product, and a product
/// publishes one shape at a time — but it is the sequence that would expose a
/// retained buffer being handed to a grid it does not fit, which is the only
/// way this module can produce wrong pixels rather than merely slow ones. Every
/// buffer handed out is capacity-exact for its own request, every one arrives
/// empty, and the thrash is counted where an operator can see it.
#[test]
fn two_shapes_alternating_never_hand_a_grid_the_wrong_block() {
    let pool: StagingPool<u32> = StagingPool::new(POINTS);
    const ROUNDS: usize = 6;
    for round in 0..ROUNDS {
        let points = if round % 2 == 0 { POINTS } else { OTHER_POINTS };
        let mut values = pool.take(points).expect("a buffer for this shape");
        assert_eq!(
            values.capacity(),
            points,
            "round {round} was handed a block of the wrong size",
        );
        assert!(values.is_empty(), "round {round} inherited content");
        values.extend(std::iter::repeat_n(round as u32, points));
        assert_eq!(values.len(), points);
        pool.give(values);
        assert_eq!(
            pool.retained_points(),
            points,
            "round {round} parked a buffer the slot then mis-describes",
        );
    }
    let totals = pool.totals();
    assert_eq!(
        totals.allocated, ROUNDS,
        "every round is a fresh block: one slot cannot hold two shapes, and \
         that is the budget working, not a leak",
    );
    assert_eq!(totals.reused, 0);
    assert_eq!(totals.declined, 0);
    assert_eq!(
        pool.resizes(),
        ROUNDS - 1,
        "and every round past the first is counted as the resize it is, so \
         alternating callers are legible rather than merely slow",
    );
    assert_eq!(
        pool.health(),
        StagingHealth::Inert,
        "a pool that reuses nothing must read as inert however busy it looks",
    );
}

/// **The reading that would have caught the shipped defect.**
///
/// A pool wired up, counted, and removing nothing must not read like a pool
/// nobody has touched yet. `reused: 0` is both, and only [`StagingPool::health`]
/// separates them.
#[test]
fn a_pool_that_reuses_nothing_reads_as_inert_and_not_as_cold() {
    let pool: StagingPool<f32> = StagingPool::new(POINTS);
    assert_eq!(pool.health(), StagingHealth::Cold, "nothing has run yet");

    // A decode at the shape a wrongly-declared pool cannot serve: allocate,
    // hand back, and the next one allocates again.
    let first = pool.take(OTHER_POINTS).expect("allocates");
    assert_eq!(
        pool.health(),
        StagingHealth::Inert,
        "one decode that had to allocate is already not `Cold`",
    );
    drop(first);
    assert_eq!(
        pool.totals(),
        StagingTotals {
            allocated: 1,
            reused: 0,
            declined: 0
        },
        "and the three totals alone read exactly like the healthy cold pool \
         above, which is why the verdict exists",
    );

    pool.give(sized::<f32>(OTHER_POINTS));
    let second = pool.take(OTHER_POINTS).expect("the slot is full");
    assert_eq!(
        pool.health(),
        StagingHealth::Reusing,
        "and one reuse is enough to say so",
    );
    drop(second);
}
