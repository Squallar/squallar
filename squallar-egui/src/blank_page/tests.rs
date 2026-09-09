//! The registry's own doors. Its effect on the upload router is
//! `squallar-gpu`'s
//! `a_noted_page_is_four_bytes_on_the_queue_where_it_was_thirteen_megabytes`.
//!
//! **One test**, because the list is a process-global static: a second test in
//! this binary could not tell its own entry from one another test left behind,
//! which is the reason `raster_atlas`'s page cache and the staging slots are
//! tested the same way.

use super::*;

#[test]
fn a_noted_page_is_claimed_once_and_an_unnoted_one_is_never_claimed() {
    let page = egui::TextureId::Managed(9_001);
    let other = egui::TextureId::Managed(9_002);
    let stale = egui::TextureId::Managed(9_003);

    assert!(
        !take(page),
        "premise: nothing has noted this id, so the claim below is this \
         test's own and not a leftover",
    );

    note(page);
    assert!(
        take(page),
        "a noted page is claimed by its allocation delta",
    );
    assert!(
        !take(page),
        "and only ONCE: a page is blank when it is minted, so a claim that \
         did not remove would make every later re-allocation under this id \
         transfer nothing and draw a blank page",
    );

    // The discriminating half. Without it a `take` that answered `true` for
    // everything would pass the rows above, and every real texture in the
    // application would be allocated and never transferred.
    note(page);
    assert!(
        !take(other),
        "an id nobody noted is never claimed, even while another one is \
         outstanding",
    );
    assert!(
        take(page),
        "and the outstanding one is still there to claim"
    );

    // The bound: a page retired before its delta was filed leaves nothing
    // behind. The property that lets this fail is that `stale` is really in
    // the list when `forget` runs — an empty list reports zero either way.
    note(stale);
    assert_eq!(
        outstanding(),
        1,
        "premise: the entry `forget` is about to drop is really there",
    );
    forget(&[stale]);
    assert_eq!(
        outstanding(),
        0,
        "a page freed before its delta arrived stayed in the list, which grows \
         with the session",
    );
}
