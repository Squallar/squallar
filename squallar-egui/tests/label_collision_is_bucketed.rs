//! **A pane's label collision test is a search over a neighbourhood, not over
//! the pane.**
//!
//! Every label a pane draws asks `walkers::OccupiedAreas::try_occupy` whether
//! the screen it needs is free — the basemap names through
//! `ui_map_overlays::lay_out_label`, the station names through
//! `site_marker::try_draw_site_label`, both against one `OccupiedAreas` for
//! the whole pane. The claim is answered by
//! `walkers::text::OrientedRect::intersects`, a bounding-box compare plus a
//! separating-axis test over eight corners.
//!
//! While the claims already made were held in one flat `Vec`, answering the
//! `i`-th claim ran `i - 1` of those tests, so a pane laying out `n` labels
//! ran **`n(n-1)/2`** of them — quadratic in a number that is set by how many
//! names the tiles on the glass carry.
//!
//! # Why it was worth changing
//!
//! Measured on the native rig's scene D (one 1920x1080 pane, KTLX, all 18
//! layers, the `ui-sweep` gesture script, NVIDIA RTX 3090 / Vulkan on Xvfb,
//! `perf` over 100 s, 53,993 samples), the app's own always-on ledger
//! (`tile_mesh::ledger::label_anchors_placed`) counted **343 label anchors per
//! frame** — and 282 on an earlier leg of the same scene with a colder tile
//! cache — while `OccupiedAreas::try_occupy` was **the largest single symbol of
//! this workspace's or walkers' own code on the frame thread**: 0.34 % of all
//! on-CPU samples, of which ~78 % sat in the scan loop itself. `n(n-1)/2` at
//! `n = 282` is 39,621 tests per frame, and the arithmetic fits the
//! measurement: ~30 µs of frame-thread CPU per drawn frame at roughly a
//! nanosecond a test.
//!
//! # What this gate counts
//!
//! **`walkers::intersect_tests()` — a real always-on counter incremented
//! inside `OrientedRect::intersects` itself.** It knows nothing about how the
//! claims are stored, so it counts the flat scan and the bucketed search
//! alike; [`the_instrument_counts_every_intersection_test`] is the arm that
//! shows it fires, by running the `n(n-1)/2` pairs by hand and requiring
//! exactly that many.
//!
//! Both expectations are read from the fixture — its own label count and its
//! own geometry against `walkers::BUCKET_POINTS` — never written down here as
//! a number.
//!
//! # The correctness conjunct is separate
//!
//! A search that refuses more labels is cheaper and wrong.
//! [`bucketing_places_exactly_what_a_full_scan_places`] holds the *decisions*:
//! the accept/reject answer for every label of the fixture, in order, must be
//! the one a full scan gives. It shares no code with the count gate and would
//! fail on its own.

use walkers::text::OrientedRect;

/// The pane the fixture lays labels out on: the app's own default window.
const PANE: egui::Vec2 = egui::vec2(1920.0, 1080.0);

/// One label's claim, in points. A wrapped basemap place name at the
/// committed styles' text sizes is about this: two rows tall and a few words
/// wide.
const LABEL: egui::Vec2 = egui::vec2(96.0, 28.0);

/// How many labels the fixture asks to be placed.
///
/// The point of the fixture is to be the size of the thing measured, so the
/// figures here can be read against the ones in the module doc. 282 is the
/// LOWER of the two legs' per-frame anchor counts (282 and 343), so the count
/// below understates a real frame rather than flattering it.
const LABELS: usize = 282;

/// A deterministic anchor field over [`PANE`].
///
/// A regular grid would be the wrong fixture in both directions: with wide
/// spacing nothing ever collides and the accept/reject sequence is constant
/// `true`, and with tight spacing everything after the first row collides.
/// This is a grid with a reproducible offset per cell, which produces a mix —
/// [`bucketing_places_exactly_what_a_full_scan_places`] asserts the mix is
/// really there rather than assuming it.
fn anchors() -> Vec<egui::Pos2> {
    // A small LCG, written out so the fixture needs no dependency and is the
    // same on every platform. Constants from Numerical Recipes' `ranqd1`.
    let mut state: u32 = 0x1234_5678;
    let mut next = || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        // The high bits are the well-mixed ones.
        (state >> 8) as f32 / (1 << 24) as f32
    };

    let columns = 24;
    (0..LABELS)
        .map(|i| {
            let cell = egui::vec2(PANE.x / columns as f32, PANE.y / 12.0);
            let col = (i % columns) as f32;
            let row = (i / columns) as f32;
            egui::pos2(cell.x * (col + next()), cell.y * (row.min(11.0) + next()))
        })
        .collect()
}

/// The fixture's claims, in the order a pane would ask for them.
fn claims() -> Vec<OrientedRect> {
    anchors()
        .into_iter()
        .map(|at| OrientedRect::new(at, 0.0, LABEL))
        .collect()
}

/// Fresh claims are needed per arm because `OrientedRect` is not `Clone`.
fn claim_at(at: egui::Pos2) -> OrientedRect {
    OrientedRect::new(at, 0.0, LABEL)
}

/// The counter is process-global and the instrument is deliberately blind to
/// who is asking, so **every** arm in this file has to hold this for its whole
/// body — not just the ones that read the counter. libtest runs a binary's
/// tests concurrently, and an arm that merely *places* labels without holding
/// this adds its tests to whatever window is open: measured that way, a window
/// that really ran 281 tests read 6,300.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Exclusive use of the counter for the caller's whole test.
///
/// Bound to a name by the caller and never taken inside an `assert!`: a lock
/// in an assertion's *message* is taken while the condition still holds it,
/// and that hangs instead of reddening.
fn serialised() -> std::sync::MutexGuard<'static, ()> {
    ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Runs `body` and hands back `(value, intersection tests it ran)`. The caller
/// must already hold [`serialised`].
fn tests_during<T>(body: impl FnOnce() -> T) -> (T, u64) {
    let before = walkers::intersect_tests();
    let value = body();
    (value, walkers::intersect_tests() - before)
}

/// The bucket coordinates a claim's bounding box touches, derived here from
/// the fixture's own geometry and `walkers::BUCKET_POINTS`.
///
/// This is not a copy of the search: it is the *definition* of which claims
/// can be candidates for which query, which is what lets the count below be
/// stated as a number the fixture produces rather than a number typed in.
fn buckets_of(at: egui::Pos2) -> Vec<(i32, i32)> {
    let half = LABEL * 0.5;
    let min = at - half;
    let max = at + half;
    let cell = |v: f32| (v / walkers::BUCKET_POINTS).floor() as i32;
    let (x0, y0, x1, y1) = (cell(min.x), cell(min.y), cell(max.x), cell(max.y));
    let mut out = Vec::new();
    for cy in y0..=y1 {
        for cx in x0..=x1 {
            out.push((cx, cy));
        }
    }
    out
}

/// Every claim a bucketed search may put through `intersects`, summed over the
/// fixture's own queries: for query `i`, the accepted claims before it that
/// share a bucket with it.
///
/// An upper bound rather than an equality, and deliberately so — the search
/// stops at the first claim that really does intersect, and modelling *that*
/// here would be a second copy of the implementation, which is the one thing a
/// gate must not be.
fn candidate_pairs(decisions: &[bool]) -> u64 {
    let ats = anchors();
    let mut kept: Vec<usize> = Vec::new();
    let mut total = 0u64;
    for (i, at) in ats.iter().enumerate() {
        let mine = buckets_of(*at);
        total += kept
            .iter()
            .filter(|&&j| buckets_of(ats[j]).iter().any(|b| mine.contains(b)))
            .count() as u64;
        if decisions[i] {
            kept.push(i);
        }
    }
    total
}

/// **The other arm: the instrument fires.**
///
/// Every unordered pair of the fixture's claims, put through `intersects` by
/// hand. The counter must read exactly that many — a gate that cannot see the
/// tests it is counting would report zero for any implementation at all.
///
/// It is also the ceiling on what the flat scan this replaces ran: the scan
/// answered claim `i` against every claim it had *kept*, so on a fixture where
/// some are refused it runs fewer than every pair —
/// [`claiming_a_pane_of_labels_tests_only_its_own_neighbourhood`] measures
/// that figure rather than deriving it from this one.
#[test]
fn the_instrument_counts_every_intersection_test() {
    let _serialised = serialised();
    let all = claims();
    let (pairs, tested) = tests_during(|| {
        let mut pairs = 0u64;
        for i in 0..all.len() {
            for j in 0..i {
                let _ = all[i].intersects(&all[j]);
                pairs += 1;
            }
        }
        pairs
    });

    let expected = (all.len() * (all.len() - 1) / 2) as u64;
    assert_eq!(pairs, expected, "the fixture's own pair count");
    assert_eq!(
        tested, expected,
        "the counter must see every one of the {expected} tests the fixture \
         ran; it saw {tested}"
    );
}

/// **The correctness conjunct: the same labels are placed.**
///
/// A cheaper search that refuses a label the full scan accepted — or accepts
/// one it refused — is a bug that no count would show. The decisions are
/// compared label by label, in the order a pane asks for them.
#[test]
fn bucketing_places_exactly_what_a_full_scan_places() {
    let _serialised = serialised();
    let ats = anchors();

    // The reference: the flat scan, spelled out. First claim to ask for a
    // piece of screen keeps it.
    let mut kept: Vec<OrientedRect> = Vec::new();
    let scanned: Vec<bool> = ats
        .iter()
        .map(|at| {
            let rect = claim_at(*at);
            let free = !kept.iter().any(|existing| existing.intersects(&rect));
            if free {
                kept.push(rect);
            }
            free
        })
        .collect();

    let mut occupied = walkers::OccupiedAreas::new();
    let bucketed: Vec<bool> = ats
        .iter()
        .map(|at| occupied.try_occupy(claim_at(*at)))
        .collect();

    // Non-vacuity, both ways: a fixture that placed everything, or nothing,
    // would agree with any implementation whatsoever.
    let placed = scanned.iter().filter(|kept| **kept).count();
    assert!(
        placed > 0 && placed < scanned.len(),
        "the fixture placed {placed} of {} labels; a fixture that collides \
         never or always proves nothing",
        scanned.len()
    );

    assert_eq!(
        bucketed,
        scanned,
        "the bucketed search made a different decision from the full scan on \
         at least one of the fixture's {} labels",
        ats.len()
    );
}

/// **A claim too large to file is still found.**
///
/// A claim whose bounding box spans more buckets than are worth writing is
/// held apart from them, and every later query has to test it anyway or the
/// screen it holds would be handed out twice. A pane can produce one: a
/// projector with no scale yet puts a tile's whole extent on the glass, and
/// `walkers::text::OrientedRect` is built from whatever geometry it is handed.
/// Anything with a coordinate that is not finite lands here too.
#[test]
fn an_unfileable_claim_still_refuses_the_labels_it_covers() {
    let _serialised = serialised();
    let huge = egui::vec2(PANE.x * 64.0, PANE.y * 64.0);
    let centre = egui::pos2(PANE.x / 2.0, PANE.y / 2.0);

    let mut occupied = walkers::OccupiedAreas::new();
    assert!(
        occupied.try_occupy(OrientedRect::new(centre, 0.0, huge)),
        "the first claim always wins"
    );

    let covered = anchors();
    let refused = covered
        .iter()
        .filter(|at| !occupied.try_occupy(claim_at(**at)))
        .count();
    assert_eq!(
        refused,
        covered.len(),
        "every one of the fixture's {} labels falls inside the huge claim and \
         must be refused by it; {refused} were",
        covered.len()
    );
}

/// **The count gate: a pane's labels are answered from their own
/// neighbourhood.**
///
/// The bound is what the fixture's own geometry allows a bucketed search to
/// look at ([`candidate_pairs`]), and the figure it replaces is what the flat
/// scan ran over the same fixture — both counted by the same instrument, not
/// derived from one another.
#[test]
fn claiming_a_pane_of_labels_tests_only_its_own_neighbourhood() {
    let _serialised = serialised();
    let ats = anchors();

    // The flat scan, over the same fixture and through the same counter.
    let mut kept: Vec<OrientedRect> = Vec::new();
    let (scanned, scan_tests) = tests_during(|| {
        ats.iter()
            .map(|at| {
                let rect = claim_at(*at);
                let free = !kept.iter().any(|existing| existing.intersects(&rect));
                if free {
                    kept.push(rect);
                }
                free
            })
            .collect::<Vec<bool>>()
    });

    let mut occupied = walkers::OccupiedAreas::new();
    let (bucketed, bucket_tests) = tests_during(|| {
        ats.iter()
            .map(|at| occupied.try_occupy(claim_at(*at)))
            .collect::<Vec<bool>>()
    });

    // The floor under the comparison: both arms really placed the fixture,
    // and placed it the same way. A search that answered nothing would run no
    // tests at all and pass a bound.
    assert_eq!(bucketed, scanned, "the two arms placed different labels");
    assert!(
        scan_tests > 0 && bucket_tests > 0,
        "one of the arms ran no intersection test at all: scan {scan_tests}, \
         bucketed {bucket_tests}"
    );

    let allowed = candidate_pairs(&scanned);
    println!(
        "MEASURED labels={} placed={} scan_tests={scan_tests} bucket_tests={bucket_tests} allowed={allowed}",
        ats.len(),
        scanned.iter().filter(|k| **k).count()
    );
    assert!(
        bucket_tests <= allowed,
        "the bucketed search ran {bucket_tests} intersection tests over the \
         fixture's {} labels; its own geometry allows at most {allowed}, and \
         the flat scan this replaces ran {scan_tests}",
        ats.len()
    );
    assert!(
        allowed < scan_tests,
        "the fixture is too dense to show anything: its geometry allows \
         {allowed} candidate pairs against the flat scan's {scan_tests}"
    );
}
