//! What a reading of the cache ledger is allowed to mean.
//!
//! About the arithmetic and the classification, not about the statics, on
//! [`super::super::take_ledger`]'s terms: the counters are process-global and
//! this binary runs its tests in parallel over them, so an assertion on an
//! absolute value would be an assertion about harness scheduling. The one
//! test that touches a static ([`the_statics_move_by_at_least_what_one_source_applied`])
//! asserts a difference of two readings and asks only `>=`.

use super::{CacheEvent, EvictedKind, PutKind, ROLES, Totals, note, totals};

/// **A put lands in exactly one kind.** Four counters whose contents were
/// interchangeable would answer "was this body fetched for nothing?" with the
/// same number as "was this the tile's first sight?".
#[test]
fn every_put_kind_is_its_own_counter() {
    for kind in [
        PutKind::First,
        PutKind::Restyle,
        PutKind::Duplicate,
        PutKind::Orphan,
    ] {
        let mut t = Totals::default();
        t.apply(CacheEvent::Put(kind));
        assert_eq!(
            t.puts(),
            1,
            "one put into {kind:?} was counted {} times",
            t.puts()
        );
        let landed = [
            (PutKind::First, t.puts_first),
            (PutKind::Restyle, t.puts_restyle),
            (PutKind::Duplicate, t.puts_duplicate),
            (PutKind::Orphan, t.puts_orphan),
        ];
        for (other, count) in landed {
            assert_eq!(
                count,
                u64::from(other == kind),
                "a put into {kind:?} read {count} in {other:?}"
            );
        }
        assert_eq!(t.requests + t.restyle_asks + t.refetch_after_eviction, 0);
        assert_eq!(t.evicted(), 0);
    }
}

/// **A pending eviction carries no bytes and a resident one carries its
/// slot's.** The byte figure is what makes the eviction count a memory
/// statement rather than a slot statement.
#[test]
fn evictions_split_by_what_the_slot_held() {
    let mut t = Totals::default();
    t.apply(CacheEvent::Evicted {
        kind: EvictedKind::Pending,
        bytes: 0,
    });
    t.apply(CacheEvent::Evicted {
        kind: EvictedKind::Resident,
        bytes: 4_096,
    });
    t.apply(CacheEvent::Evicted {
        kind: EvictedKind::Resident,
        bytes: 1_000,
    });
    assert_eq!((t.evicted_pending, t.evicted_resident), (1, 2));
    assert_eq!(t.evicted(), 3);
    assert_eq!(t.evicted_bytes, 5_096);
}

/// **The three asks are disjoint.** A theme flip moves `restyle_asks` and
/// nothing else; a refetch moves `requests` and `refetch_after_eviction`
/// together, because a refetch is a request the cache remembers.
#[test]
fn the_asks_are_three_counters_and_a_refetch_is_a_request() {
    let mut t = Totals::default();
    t.apply(CacheEvent::RestyleAsk);
    assert_eq!(
        (t.requests, t.restyle_asks, t.refetch_after_eviction),
        (0, 1, 0)
    );
    // The cache records a refetch as both events, in this order.
    t.apply(CacheEvent::RefetchAfterEviction);
    t.apply(CacheEvent::Request);
    assert_eq!(
        (t.requests, t.restyle_asks, t.refetch_after_eviction),
        (1, 1, 1)
    );
}

/// **A diff subtracts the counters and keeps the levels.** A level has no
/// window: "entries resident" at the end of a window is the reading, not the
/// reading minus an earlier one.
#[test]
fn a_diff_subtracts_counters_and_keeps_levels() {
    let earlier = Totals {
        requests: 10,
        restyle_asks: 2,
        refetch_after_eviction: 3,
        puts_first: 8,
        puts_restyle: 1,
        puts_duplicate: 1,
        puts_orphan: 1,
        evicted_pending: 4,
        evicted_resident: 5,
        evicted_bytes: 500,
        resident_entries: 90,
        resident_bytes: 9_000,
        overrun_bytes: 1,
        floor_entries: 2,
        wanted_on_glass: 3,
        wanted_net: 4,
        parsed_entries: 20,
        parsed_bytes: 5,
        snapped: 0,
    };
    let later = Totals {
        requests: 25,
        restyle_asks: 2,
        refetch_after_eviction: 9,
        puts_first: 20,
        puts_restyle: 1,
        puts_duplicate: 3,
        puts_orphan: 2,
        evicted_pending: 6,
        evicted_resident: 12,
        evicted_bytes: 1_200,
        resident_entries: 100,
        resident_bytes: 10_000,
        overrun_bytes: 11,
        floor_entries: 12,
        wanted_on_glass: 13,
        wanted_net: 14,
        parsed_entries: 24,
        parsed_bytes: 15,
        snapped: 1,
    };
    let window = later.diff(&earlier);
    assert_eq!(
        window,
        Totals {
            requests: 15,
            restyle_asks: 0,
            refetch_after_eviction: 6,
            puts_first: 12,
            puts_restyle: 0,
            puts_duplicate: 2,
            puts_orphan: 1,
            evicted_pending: 2,
            evicted_resident: 7,
            evicted_bytes: 700,
            resident_entries: 100,
            resident_bytes: 10_000,
            overrun_bytes: 11,
            floor_entries: 12,
            wanted_on_glass: 13,
            wanted_net: 14,
            parsed_entries: 24,
            parsed_bytes: 15,
            snapped: 1,
        }
    );
    // Saturating, never wrapping: a reading taken before another source's
    // counters were folded in reads as zero movement, not as 2^64.
    assert_eq!(earlier.diff(&later).requests, 0);
}

/// **Two roles, two words, two slots.** The line names the role and the rig
/// matches on the word; a shared slot would fold the hillshade's rasters into
/// the basemap's vector figures.
#[test]
fn the_roles_are_distinguishable_by_slot_and_by_name() {
    let mut slots: Vec<usize> = ROLES.iter().map(|role| role.index()).collect();
    slots.sort_unstable();
    slots.dedup();
    assert_eq!(slots.len(), ROLES.len());
    let mut labels: Vec<&str> = ROLES.iter().map(|role| role.label()).collect();
    labels.sort_unstable();
    labels.dedup();
    assert_eq!(labels.len(), ROLES.len());
    for role in ROLES {
        assert!(
            role.label().is_ascii() && role.label() == role.label().to_ascii_lowercase(),
            "{role:?}'s word must be lowercase ASCII for the rig's pattern"
        );
    }
}

/// **The statics receive what a source applied**, role by role, by at least
/// that much. `>=` and not `==`: the tiles tests build sources of both roles in
/// this same binary, so another test's events may land inside this window,
/// but none of them can make the window smaller than what was applied here.
#[test]
fn the_statics_move_by_at_least_what_one_source_applied() {
    for role in ROLES {
        let before = totals(role);
        let mut mirror = Totals::default();
        let events = [
            CacheEvent::Request,
            CacheEvent::Request,
            CacheEvent::RefetchAfterEviction,
            CacheEvent::RestyleAsk,
            CacheEvent::Put(PutKind::First),
            CacheEvent::Put(PutKind::Orphan),
            CacheEvent::Evicted {
                kind: EvictedKind::Pending,
                bytes: 0,
            },
            CacheEvent::Evicted {
                kind: EvictedKind::Resident,
                bytes: 777,
            },
        ];
        for event in events {
            mirror.apply(event);
            note(role, event);
        }
        let window = totals(role).diff(&before);
        assert!(
            window.requests >= mirror.requests,
            "{role:?}: {window:?} < {mirror:?}"
        );
        assert!(window.refetch_after_eviction >= mirror.refetch_after_eviction);
        assert!(window.restyle_asks >= mirror.restyle_asks);
        assert!(window.puts_first >= mirror.puts_first);
        assert!(window.puts_orphan >= mirror.puts_orphan);
        assert!(window.evicted_pending >= mirror.evicted_pending);
        assert!(window.evicted_resident >= mirror.evicted_resident);
        assert!(window.evicted_bytes >= mirror.evicted_bytes);
    }
}
