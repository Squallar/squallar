//! The pool's own invariants, over buffers rather than over granules.
//!
//! What is checked here is the half that decides whether a retained buffer
//! can ever hand a grid the wrong bytes, at a width a test can allocate many
//! times over. The GMGSI instance checks the same invariants at the shipped
//! width in `gmgsi::staging::tests`, and the decode gates live in
//! `tests/gmgsi_staging_blocks.rs`, where a counting global allocator watches
//! the real shipped path.

use super::*;

const POINTS: usize = 4096;

fn buffer<T: Copy>() -> Vec<T> {
    let mut v: Vec<T> = Vec::new();
    v.try_reserve_exact(POINTS).expect("fits");
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
    assert!(
        second.is_empty(),
        "a buffer comes out of the slot with nothing in it, whatever it held",
    );
    assert_eq!(second.capacity(), POINTS);
    assert_eq!(pool.retained_bytes(), 0);
}

/// **A grid that is not the pool's shape never touches the retained buffer.**
///
/// Every byte budget in this crate is spent against `len * size_of::<T>()`,
/// so a 400-byte grid handed the pooled block would report 400 bytes while
/// holding the whole slot, and the cache would go on filling until the tab
/// died. Matching capacity *exactly* rather than "≥" is what forbids it.
#[test]
fn a_grid_of_another_shape_is_never_given_the_pooled_buffer() {
    let pool: StagingPool<f32> = StagingPool::new(POINTS);
    pool.give(buffer());

    let small = pool.take(100).expect("a small grid always allocates");
    assert_eq!(small.capacity(), 100);
    assert_eq!(
        pool.totals(),
        StagingTotals {
            allocated: 1,
            reused: 0,
            declined: 0
        },
        "and the pooled buffer must still be in the slot, untouched",
    );
    let pooled = pool.take(POINTS).expect("the slot still holds one");
    assert_eq!(pool.totals().reused, 1, "which the next grid then takes");
    drop((small, pooled));
}

/// And the same rule on the way in.
#[test]
fn a_buffer_of_another_capacity_is_refused_by_the_slot() {
    let pool: StagingPool<f32> = StagingPool::new(POINTS);
    let mut odd: Vec<f32> = Vec::new();
    odd.try_reserve_exact(POINTS - 1).expect("fits");
    pool.give(odd);
    assert_eq!(
        pool.totals(),
        StagingTotals {
            allocated: 0,
            reused: 0,
            declined: 1
        },
    );
    assert_eq!(pool.retained_bytes(), 0);
    let fresh = pool.take(POINTS).expect("so the slot was empty");
    assert_eq!(pool.totals().allocated, 1, "and the next grid allocates");
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
