use super::*;

/// **The property the whole type exists for**: across a full pass in which
/// nothing commits, the ledger returns to zero.
///
/// This is `LoopFrameStore`'s `begin_pass`/`end_pass` bracket applied to
/// bytes — a pass that reserves and never settles must not leave the level
/// standing, because a level that only rises reads as a scene that keeps
/// growing, which is the exact symptom the budget model is watching for.
#[test]
fn reservations_return_to_zero_across_a_pass_with_no_commitments() {
    let led = Reservations::new();

    led.begin_pass();
    led.reserve(4 * 1024 * 1024);
    led.reserve(60 * 1024 * 1024);
    led.reserve(125 * 1024 * 1024);
    assert_eq!(led.outstanding_bytes(), 189 * 1024 * 1024);

    let released = led.end_pass();
    assert_eq!(
        released,
        189 * 1024 * 1024,
        "the pass did not report what it released",
    );
    assert_eq!(
        led.outstanding_bytes(),
        0,
        "a pass that committed nothing left bytes outstanding",
    );
    assert_eq!(
        led.released_unsettled_bytes(),
        189 * 1024 * 1024,
        "the release left no trace, so a leaking site would be invisible",
    );

    // And again, so the zero is a resting state and not a one-off.
    led.begin_pass();
    assert_eq!(led.end_pass(), 0);
    assert_eq!(led.outstanding_bytes(), 0);
}

/// A pass in which every reserve settles releases nothing, and the healthy
/// reading of `released_unsettled` stays zero. The other arm of the test
/// above: a gate that only fires is a gate that cannot tell a leak from
/// ordinary work.
#[test]
fn a_pass_whose_reserves_all_settle_releases_nothing() {
    let led = Reservations::new();
    led.begin_pass();

    let a = led.reserve(80 * 1024 * 1024);
    let b = led.reserve(125 * 1024 * 1024);
    led.settle(a, 74 * 1024 * 1024);
    led.settle(b, 125 * 1024 * 1024);

    assert_eq!(
        led.outstanding_bytes(),
        0,
        "settled reserves still standing"
    );
    assert_eq!(led.end_pass(), 0, "a fully settled pass released bytes");
    assert_eq!(
        led.released_unsettled_bytes(),
        0,
        "a healthy pass was recorded as leaking",
    );
    assert_eq!(
        led.over_arrivals(),
        0,
        "an arrival at or under its reserve was counted as an over-arrival",
    );
}

/// **The field's own report that a reserve is wrong.** An arrival larger than
/// what was reserved for it is counted, with the shortfall, so the next
/// reserve that is really a percentile says so from the field rather than
/// waiting for a corpus to be assembled against it.
#[test]
fn an_arrival_over_its_reserve_is_counted_with_its_shortfall() {
    let led = Reservations::new();
    led.begin_pass();

    let reserve = led.reserve(80 * 1024 * 1024);
    led.settle(reserve, 92 * 1024 * 1024);

    assert_eq!(led.over_arrivals(), 1);
    assert_eq!(led.over_arrival_bytes(), 12 * 1024 * 1024);

    // Exactly at the reserve is not over it: the boundary belongs to the
    // healthy side, or a reserve sized to the maximum ever seen would report
    // itself wrong on the very volume it was sized from.
    let exact = led.reserve(80 * 1024 * 1024);
    led.settle(exact, 80 * 1024 * 1024);
    assert_eq!(
        led.over_arrivals(),
        1,
        "an exact arrival was counted as over"
    );

    let under = led.reserve(80 * 1024 * 1024);
    led.settle(under, 1);
    assert_eq!(led.over_arrivals(), 1);
    assert_eq!(led.end_pass(), 0);
}

/// A reply that outlives its pass settles against a ledger that has already
/// been zeroed. That must saturate rather than wrap: an unsigned level driven
/// below zero would read as sixteen exabytes outstanding and refuse every
/// admission for the rest of the session.
#[test]
fn a_settle_after_its_pass_closed_saturates_instead_of_wrapping() {
    let led = Reservations::new();
    led.begin_pass();
    let ticket = led.reserve(80 * 1024 * 1024);
    assert_eq!(led.end_pass(), 80 * 1024 * 1024);

    led.settle(ticket, 74 * 1024 * 1024);
    assert_eq!(
        led.outstanding_bytes(),
        0,
        "a late settle wrapped the level",
    );

    // And the late arrival's own evidence still lands.
    let late = led.reserve(10);
    led.settle(late, 999);
    assert_eq!(led.over_arrivals(), 1);
    assert_eq!(led.over_arrival_bytes(), 989);
}

/// A pass left open when the next one starts does not carry its bytes
/// forward, and the abandonment is recorded. `begin_pass` is the one moment
/// outstanding bytes are knowable without asking every site.
#[test]
fn opening_a_pass_discards_and_records_what_the_last_one_left() {
    let led = Reservations::new();
    led.begin_pass();
    led.reserve(50);
    // No `end_pass`: the frame threw, or a site returned early.
    led.begin_pass();

    assert_eq!(led.outstanding_bytes(), 0, "a leaked pass carried forward");
    assert_eq!(led.released_unsettled_bytes(), 50);
}

/// The peak is a high-water mark across passes, so a readout can show what a
/// session's worst moment declared rather than whatever the instant of the
/// read happened to catch.
#[test]
fn the_peak_survives_a_pass_boundary() {
    let led = Reservations::new();
    led.begin_pass();
    led.reserve(125 * 1024 * 1024);
    led.end_pass();

    led.begin_pass();
    led.reserve(1024);
    assert_eq!(led.peak_bytes(), 125 * 1024 * 1024);
    assert_eq!(led.outstanding_bytes(), 1024);
    led.end_pass();
}

/// The census line names every figure it prints, so a reader cannot take the
/// peak for the instant or the over-arrival count for its bytes.
#[test]
fn the_census_line_names_what_it_prints() {
    let led = Reservations::new();
    led.begin_pass();
    let t = led.reserve(2048);
    led.settle(t, 4096);
    let line = led.line();
    for word in ["reserved", "peak", "over-arrivals", "released unsettled"] {
        assert!(line.contains(word), "{line:?} does not name {word:?}");
    }
}

/// **A guarded reserve abandoned by `?` releases itself**, and is not counted
/// as an overshoot: a decode that never happened did not overshoot anything.
/// This is the path every fallible decoder actually takes when the data is
/// bad, so it is the one that must not read as a leak.
#[test]
fn a_guarded_reserve_releases_itself_when_the_decode_gives_up() {
    let led = Reservations::new();
    led.begin_pass();

    fn decode(led: &Reservations, ok: bool) -> Result<u64, &'static str> {
        let held = led.take(60 * 1024 * 1024);
        if !ok {
            return Err("the granule would not parse");
        }
        let actual = 58 * 1024 * 1024;
        held.settle(actual);
        Ok(actual)
    }

    assert!(decode(&led, false).is_err());
    assert_eq!(
        led.outstanding_bytes(),
        0,
        "a decode that gave up left its reserve standing",
    );
    assert_eq!(
        led.over_arrivals(),
        0,
        "a refusal was counted as an overshoot"
    );

    assert!(decode(&led, true).is_ok());
    assert_eq!(led.outstanding_bytes(), 0);
    assert_eq!(led.over_arrivals(), 0);

    assert_eq!(
        led.end_pass(),
        0,
        "neither path settled, so the pass had to release",
    );
    assert_eq!(
        led.released_unsettled_bytes(),
        0,
        "the guard let a reserve reach the pass boundary",
    );
}

/// The guard still reports an overshoot when the decode succeeds at a size
/// over its reserve — the guard changes who releases, not what is counted.
#[test]
fn a_guarded_reserve_still_counts_an_overshoot() {
    let led = Reservations::new();
    led.begin_pass();
    let held = led.take(1024);
    assert_eq!(held.bytes(), 1024);
    held.settle(4096);
    assert_eq!(led.over_arrivals(), 1);
    assert_eq!(led.over_arrival_bytes(), 3072);
    assert_eq!(led.end_pass(), 0);
}
