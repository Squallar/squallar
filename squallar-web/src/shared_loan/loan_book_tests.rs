//! The ownership protocol, on the host.
//!
//! A browser is what makes the *views* real, and Tier 1
//! (`tests/tier1_wasm.rs`) is where a real `SharedArrayBuffer` crosses a real
//! `MessageChannel`. What is testable here is the half that decides when a
//! region may be freed, and that half is the one a bug would corrupt a raster
//! through: every case below is a way the peer could get a region back that
//! this instance had already handed to someone else.

use super::{Lent, LoanBook, LoanId, NO_LOAN, next_after};

/// The id space never issues the value the wire reserves for "no loan", **and
/// the case that matters is the wrap**, which no amount of lending in a test
/// can reach. A message whose loan reads 0 means it carries copies, so an
/// issued 0 would make a lending reply indistinguishable from a copying one —
/// and would make the Tier-2 assertion pass on a transport that had reverted.
#[test]
fn the_id_after_the_last_one_is_not_the_reserved_zero() {
    assert_eq!(
        next_after(LoanId::MAX),
        1,
        "the wrap must step OVER {NO_LOAN}, not onto it"
    );
    assert_eq!(next_after(1), 2, "and must otherwise just count");
    assert_eq!(next_after(LoanId::MAX - 1), LoanId::MAX);
}

/// The first loan a fresh book issues is not the reserved value either — the
/// trap a `derive(Default)` would walk straight into, since it would start the
/// counter at [`NO_LOAN`].
#[test]
fn the_first_loan_of_a_fresh_book_is_not_zero() {
    assert_ne!(LoanBook::new().lend(vec![vec![1]]), NO_LOAN);
    assert_ne!(LoanBook::default().lend(vec![vec![1]]), NO_LOAN);
}

/// The region is freed by the release and by nothing else. Two loans out at
/// once, one released: the other is still readable, which is the whole point —
/// a lender that freed on the next `lend` would be handing the peer a recycled
/// raster.
#[test]
fn releasing_one_loan_leaves_the_other_readable() {
    let mut book = LoanBook::new();
    let first = book.lend(vec![vec![0xAA; 4]]);
    let second = book.lend(vec![vec![0xBB; 8]]);
    assert_eq!(book.outstanding(), 2);

    assert!(book.release(first).is_some());
    assert_eq!(book.outstanding(), 1);
    assert_eq!(
        book.peek(second).map(bytes_of),
        Some(vec![vec![0xBB; 8]]),
        "the loan that was not released must still own its bytes"
    );
    assert!(book.peek(first).is_none(), "a released loan is gone");
}

/// **The violation the transport exists to catch.** A second release of the
/// same id means the peer is still reading a region this instance has freed and
/// may already have re-lent. `release` reports it rather than swallowing it, so
/// the caller can log it; a book that returned `Some` twice would let a
/// recycled raster reach a texture upload silently.
#[test]
fn a_second_release_of_the_same_loan_is_reported() {
    let mut book = LoanBook::new();
    let id = book.lend(vec![vec![7; 16]]);

    assert!(book.release(id).is_some(), "the first release hands back");
    assert!(
        book.release(id).is_none(),
        "the second release of loan {id} must report, not repeat"
    );
    assert_eq!(book.outstanding(), 0);
}

/// An id from another generation — a worker this page has replaced quoting a
/// loan the current one never issued — is reported the same way, and does not
/// free a live loan that happens to share the number.
#[test]
fn releasing_an_id_this_book_never_issued_is_reported() {
    let mut book = LoanBook::new();
    let live = book.lend(vec![vec![3; 2]]);
    let never_issued = live.wrapping_add(4096);

    assert!(book.release(never_issued).is_none());
    assert_eq!(
        book.outstanding(),
        1,
        "a forged release must not free somebody else's loan"
    );
    assert!(book.release(live).is_some());
}

/// `NO_LOAN` is never outstanding, so the caller's early return for it is not
/// hiding a real release.
#[test]
fn zero_is_never_an_outstanding_loan() {
    let mut book = LoanBook::new();
    book.lend(vec![vec![1]]);
    assert!(book.release(NO_LOAN).is_none());
    assert_eq!(book.outstanding(), 1);
}

/// The leak instrument reads the real total, over every buffer of every loan —
/// a reply is a head plus N tails and a per-loan count would under-report the
/// thing that grows.
#[test]
fn bytes_outstanding_counts_every_buffer_of_every_loan() {
    let mut book = LoanBook::new();
    assert_eq!(book.bytes_outstanding(), 0);

    let head_and_tails = book.lend(vec![vec![0; 64], vec![0; 4096], vec![0; 16]]);
    let other = book.lend(vec![vec![0; 100]]);
    assert_eq!(book.bytes_outstanding(), 64 + 4096 + 16 + 100);

    book.release(head_and_tails);
    assert_eq!(book.bytes_outstanding(), 100);
    book.release(other);
    assert_eq!(book.bytes_outstanding(), 0);
}

/// The loss path frees everything the peer will never release, and says how
/// much it found. A worker replaced mid-job owes releases it cannot send.
#[test]
fn release_all_frees_every_loan_and_counts_them() {
    let mut book = LoanBook::new();
    let kept_id = book.lend(vec![vec![0; 8]]);
    book.lend(vec![vec![0; 8]]);
    book.lend(vec![vec![0; 8]]);

    assert_eq!(book.release_all(), 3);
    assert_eq!(book.outstanding(), 0);
    assert_eq!(book.bytes_outstanding(), 0);
    assert!(
        book.release(kept_id).is_none(),
        "a release arriving after the sweep must report, not resurrect"
    );

    assert_eq!(book.release_all(), 0, "a second sweep finds nothing");
}

/// After a sweep the book keeps issuing fresh ids rather than restarting, so a
/// straggling release from the dead peer cannot name a loan the new peer holds.
#[test]
fn ids_do_not_restart_after_a_sweep() {
    let mut book = LoanBook::new();
    let before = book.lend(vec![vec![0; 1]]);
    book.release_all();
    let after = book.lend(vec![vec![0; 1]]);

    assert_ne!(
        before, after,
        "a reused id would let a replaced worker's release free the new one's loan"
    );
}

/// The bytes each part of a loan puts on the wire, for comparing a peek.
///
/// Only meaningful for owned parts; a borrowed part's bytes live in the
/// resident data and are not the book's to reproduce.
fn bytes_of(parts: &[Lent]) -> Vec<Vec<u8>> {
    parts
        .iter()
        .map(|part| match part {
            Lent::Owned(bytes) => bytes.clone(),
            Lent::Borrowed { .. } => Vec::new(),
        })
        .collect()
}

/// **The point of the borrowed arm: nothing is copied.** The view a borrowed
/// part describes has to land on the SAME address as the data the owner holds,
/// because "we lent it in place" is exactly the claim that the address is not a
/// copy's address. A `Lent::borrowed` that copied would still pass every
/// bookkeeping assertion above and would silently reinstate the memcpy this
/// whole arm exists to remove.
#[test]
fn a_borrowed_part_views_the_owner_s_own_bytes_and_does_not_copy_them() {
    let grid: std::sync::Arc<Vec<u8>> = std::sync::Arc::new(vec![0xAB; 16]);
    let want_addr = grid.as_ptr() as usize;
    let want_len = grid.len();

    let part = Lent::borrowed(grid.clone(), |g| &g[..]);

    assert_eq!(
        part.addr(),
        want_addr,
        "the view must start at the owner's own allocation, not at a copy"
    );
    assert_eq!(
        part.len(),
        want_len,
        "the whole region travels, not a prefix of it"
    );
}

/// A borrowed part keeps its owner alive for exactly as long as the loan is
/// outstanding. This is the whole safety argument for viewing memory the book
/// does not own: the peer reads that region asynchronously, so the region must
/// not be freeable while the id is outstanding.
#[test]
fn an_outstanding_borrowed_loan_holds_its_owner_alive() {
    let grid: std::sync::Arc<Vec<u8>> = std::sync::Arc::new(vec![0u8; 128]);
    let mut book = LoanBook::new();
    let id = book.lend_parts(vec![Lent::borrowed(grid.clone(), |g| &g[..])]);

    drop(grid);
    assert_eq!(
        book.bytes_outstanding(),
        128,
        "the region is still held down after every other handle is dropped"
    );

    let released = book.release(id).expect("the loan was outstanding");
    assert_eq!(released.len(), 1, "release hands the parts back");
    assert_eq!(book.bytes_outstanding(), 0, "and stops holding the region");
}

/// A mixed loan is the shape a gridded job actually posts: a small OWNED
/// envelope the encoder built, beside a large BORROWED payload it did not.
/// `bytes_outstanding` has to count both, because it is the leak instrument.
#[test]
fn a_mixed_loan_counts_the_envelope_and_the_payload() {
    let grid: std::sync::Arc<Vec<u8>> = std::sync::Arc::new(vec![0u8; 1024]);
    let mut book = LoanBook::new();
    let id = book.lend_parts(vec![
        Lent::Owned(vec![0u8; 40]),
        Lent::borrowed(grid, |g| &g[..]),
    ]);

    assert_eq!(
        book.bytes_outstanding(),
        40 + 1024,
        "the envelope and the payload are both on the wire"
    );
    assert_eq!(book.peek(id).map(<[Lent]>::len), Some(2));
}
