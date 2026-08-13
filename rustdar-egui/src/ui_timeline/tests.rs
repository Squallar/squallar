//! The timeline transport's own unit tests: the loop caption's span clause.
//!
//! The cadences these assert against are **measured**, not nominal, from the
//! `unidata-nexrad-level2` listings for 2026-08-11 — the bucket the application
//! itself reads. A TDWR volume is 360 s on both VCP 80 (23 cuts) and VCP 90 (16
//! cuts); a WSR-88D precip volume (VCP 212/215) is 259 s; a WSR-88D clear-air
//! volume (VCP 35) is 517 s. Six TDWR and four WSR-88D sites, 2550 volumes.
//!
//! Those figures are the point of the tests, not decoration. The caption exists
//! to tell a user whether they are watching every volume or a sample of them,
//! and every threshold it uses has to sit correctly against real intervals: the
//! 193–356 s a WSR-88D actually wanders over as SAILS and AVSET reshape its
//! volume, the doubling a dropped scan makes, and the 259 s → 517 s step a site
//! makes when it changes VCP mid-window.

use super::{format_span, loop_span_phrase, markedly_longer};
use crate::pane::LoopFrame;

/// Measured medians, in seconds. See the module note.
const TDWR: i64 = 360;
const WSR_PRECIP: i64 = 259;
const WSR_CLEAR_AIR: i64 = 517;

/// A recorded site cadence, in the field's own type. The constants above are
/// `i64` because that is what a gap between two timestamps is.
fn cadence(secs: i64) -> Option<u32> {
    Some(u32::try_from(secs).expect("a measured cadence fits a u32"))
}

fn frame_at(timestamp: chrono::NaiveDateTime) -> LoopFrame {
    LoopFrame {
        timestamp,
        image: None,
        render_in_flight: false,
        render_failed: false,
    }
}

/// A frame list whose consecutive gaps are exactly `gaps` — so `gaps.len() + 1`
/// frames, oldest-first, which is the order the caption is entitled to assume.
/// The timestamp is the only field it reads.
fn loop_frames(gaps: &[i64]) -> Vec<LoopFrame> {
    let mut at = chrono::NaiveDate::from_ymd_opt(2026, 8, 11)
        .expect("a real date")
        .and_hms_opt(0, 0, 0)
        .expect("a real time");
    let mut frames = vec![frame_at(at)];
    for gap in gaps {
        at += chrono::Duration::seconds(*gap);
        frames.push(frame_at(at));
    }
    frames
}

/// A loop built the way `accept_scan_listing` builds one: a listing whose
/// consecutive gaps are `listing_gaps`, run through the **shipped** sampler at a
/// cap of `held`, with the fidelity flag that sampling recorded.
///
/// [`crate::pane::listing_sample_indices`] is called rather than restated, and
/// that is the point of the helper. The predecessor of these tests wrote
/// `i * (total - 1) / (held - 1)` out in the test body, which is the same shape
/// of mistake `MAX_FRAMING_PASSES` documents: a test that re-runs the shipped
/// arithmetic in its own words is green against its own words.
fn loop_from_listing(listing_gaps: &[i64], held: usize) -> (Vec<LoopFrame>, Option<bool>) {
    let listing = loop_frames(listing_gaps);
    match crate::pane::listing_sample_indices(listing.len(), held) {
        Some(indices) => (
            indices
                .into_iter()
                .map(|i| frame_at(listing[i].timestamp))
                .collect(),
            Some(true),
        ),
        None => (listing, Some(false)),
    }
}

/// `format_span` rounds to nearest, and the 359 s row is the whole reason.
///
/// A TDWR volume arrives every 360 s, but the listing timestamps are whole
/// seconds and the gaps measure 359 s about as often as 360. Truncating — which
/// is what the status bar's age formatter correctly does for an *age* — would
/// print a six-minute radar as a five-minute one, an error of one entire volume
/// in the number the caption exists to convey.
#[test]
fn a_span_reads_in_the_largest_unit_that_still_says_something() {
    assert_eq!(format_span(45), "45 s");
    assert_eq!(format_span(59), "59 s");
    assert_eq!(format_span(60), "1 min");
    assert_eq!(format_span(WSR_PRECIP), "4 min");
    assert_eq!(format_span(359), "6 min", "359 s is a 6-minute volume");
    assert_eq!(format_span(TDWR), "6 min");
    assert_eq!(format_span(WSR_CLEAR_AIR), "9 min");
    assert_eq!(format_span(3599), "1h 0m");
    assert_eq!(format_span(10440), "2h 54m");
}

/// The one threshold, against the intervals it has to separate.
///
/// Everything here is measured against a **median**, which is how the caption
/// uses it — see the real-jitter test below for why that matters.
#[test]
fn only_a_difference_worth_a_whole_minute_and_half_again_counts() {
    assert!(
        !markedly_longer(356, 269),
        "a WSR-88D's longest volume against its own median is not a gap"
    );
    assert!(
        !markedly_longer(TDWR, 359),
        "the second a TDWR volume wobbles by is not a gap"
    );
    assert!(
        markedly_longer(2 * TDWR, TDWR),
        "a dropped scan doubles the gap and must show"
    );
    assert!(
        markedly_longer(WSR_CLEAR_AIR, WSR_PRECIP),
        "a VCP 212 to VCP 35 change must show"
    );
    assert!(
        !markedly_longer(89, 45),
        "just under 2x is still under a printed minute apart, so it stays quiet"
    );
    assert!(
        markedly_longer(105, 45),
        "the floor is inclusive: a full minute apart, and over 1.5x, does show"
    );
}

/// **Real intervals, not nominal ones.** The 29 consecutive gaps below are
/// verbatim from KPBZ on 2026-08-11 — the widest-spread 29-gap window the whole
/// day produced, 210 s to 356 s, a 1.7x ratio as SAILS and AVSET reshape the
/// volume.
///
/// This is one full desktop render set of an ordinary precip loop, and it must
/// read as evenly spaced. It is the test that decides the caption compares
/// against the median gap rather than the shortest: against the shortest this
/// window is 356 vs 210 and every WSR-88D loop in the country would announce
/// itself as unevenly spaced, burying the dropped scans that actually matter.
#[test]
fn a_real_wsr88d_loops_own_jitter_still_reads_as_evenly_spaced() {
    let measured = [
        264, 261, 269, 270, 356, 282, 260, 282, 282, 282, 282, 269, 281, 269, 274, 282, 282, 267,
        268, 269, 256, 255, 243, 229, 229, 235, 229, 230, 210,
    ];
    assert_eq!(
        loop_span_phrase(
            &loop_frames(&measured),
            Some(false),
            cadence(WSR_PRECIP),
            true
        ),
        Some("This loop spans 2h 8m over 30 frames, every scan, ~4 min apart".to_owned())
    );
}

/// Nothing to say for a pane with no frames — every pane that is not looping,
/// and a loop still fetching its listing.
#[test]
fn a_loop_with_no_frames_says_nothing() {
    assert_eq!(
        loop_span_phrase(&[], Some(false), cadence(WSR_PRECIP), true),
        None
    );
}

/// **Full fidelity.** An hour of WSR-88D precip fits the cap whole, so the
/// caption commits to "every scan" and the user knows nothing is being skipped.
#[test]
fn a_loop_holding_every_scan_says_so() {
    assert_eq!(
        loop_span_phrase(
            &loop_frames(&[WSR_PRECIP; 13]),
            Some(false),
            cadence(WSR_PRECIP),
            true,
        ),
        Some("This loop spans 56 min over 14 frames, every scan, ~4 min apart".to_owned())
    );
}

/// The same loop on a TDWR, at the measured 359 s the gaps actually carry —
/// the rounding case, end to end. A truncating formatter would report this
/// six-minute radar as five-minute in both numbers at once.
#[test]
fn a_tdwr_loop_reports_six_minute_volumes_not_five() {
    assert_eq!(
        loop_span_phrase(&loop_frames(&[359; 10]), Some(false), cadence(359), true),
        Some("This loop spans 1h 0m over 11 frames, every scan, ~6 min apart".to_owned())
    );
}

/// **Decimated** — the case the whole clause exists for, built by the shipped
/// sampler rather than by a restatement of it.
///
/// A 24 h lookback on a WSR-88D in precip lists ~333 volumes; a desktop loop
/// keeps 60. `accept_scan_listing` does not truncate that, it evenly samples,
/// so the span stays a full day and four scans in five are silently dropped.
/// "spans 23h 53m" alone would be true and would hide it; "sampled from ~4 min
/// scans, ~26 min apart" is the same loop, unable to hide it.
#[test]
fn a_decimated_loop_admits_it_is_showing_a_sample() {
    let (frames, sampled) = loop_from_listing(&[WSR_PRECIP; 332], 60);
    assert_eq!(frames.len(), 60, "precondition: the desktop cap");

    assert_eq!(
        loop_span_phrase(&frames, sampled, cadence(WSR_PRECIP), true),
        Some(
            "This loop spans 23h 53m over 60 frames, sampled from ~4 min scans, ~26 min apart"
                .to_owned()
        )
    );
}

/// **The measured defect: a third of the scans dropped, and the caption said
/// "every scan".**
///
/// Not an extreme — it is the ordinary case of a lookback a little past what
/// the cap covers, on both shipped raster caps:
///
/// | target | listing | held | dropped |
/// |---|---|---|---|
/// | browser | 20 scans, 1h 22m | 14 | 6, **30.0%** |
/// | desktop | 89 scans, 6h 20m | 60 | 29, **32.6%** |
///
/// The caps appear here as bare numbers because the figure the application
/// resolves them from — `budget::Budgets::loop_frames_held` — lives in the
/// frontend crate and this one cannot see it. The half that reads the resolved
/// budget is `the_caption_fixtures_name_caps_this_workspace_ships` in the
/// frontend's `app_render/loop_dispatch_tests.rs`, which parses the two `held`
/// cells of the table above and requires each to be a shipped arm.
///
/// It used to name `a_listing_one_scan_over_the_cap_is_recorded_as_sampled`
/// for that job, and that test cannot do it: it reads
/// `test_budgets().loop_frames_held`, which on a host build is the **desktop**
/// arm and says nothing about the browser row. So when
/// `constants::LOOP_SPAN_BUDGET_SECS` moved the browser cap from 12 to 14, the
/// browser row went stale with every test in both crates still green — which is
/// the failure this table's own existence is an argument against.
#[test]
fn a_loop_that_dropped_a_third_of_the_scans_never_claims_every_scan() {
    for (listing_scans, held, dropped_pct) in [(20usize, 14usize, 30.0), (89, 60, 32.6)] {
        let (frames, sampled) = loop_from_listing(&vec![WSR_PRECIP; listing_scans - 1], held);
        assert_eq!(frames.len(), held, "precondition: the loop filled its cap");
        let dropped = listing_scans - held;
        assert!(
            ((dropped as f64 / listing_scans as f64) * 100.0 - dropped_pct).abs() < 0.05,
            "the fixture no longer drops the {dropped_pct}% this row records",
        );

        let phrase = loop_span_phrase(&frames, sampled, cadence(WSR_PRECIP), true)
            .expect("frames enough for a span");
        assert!(
            phrase.contains("sampled from ~4 min scans"),
            "a loop holding {held} of {listing_scans} scans — {dropped} dropped \
             — captioned itself {phrase:?}",
        );
        assert!(
            !phrase.contains("every scan"),
            "{phrase:?} claims every scan while dropping {dropped} of \
             {listing_scans}",
        );
    }
}

/// **Why the median comparison could never have caught the row above**, pinned
/// on the shipped [`markedly_longer`] rather than argued in prose.
///
/// The old rule asked whether the frame list's median gap was markedly longer
/// than the listing's own median step. Both are medians over the same
/// timestamps, so even sampling keeps the frame median at exactly one listing
/// step until two-step gaps are the *majority* — which needs a listing more
/// than twice the cap. At 20-into-14 and at 89-into-60 the frame median is still
/// one step and the rule is silent, whatever fraction was dropped.
///
/// This is the property rather than the symptom: the signal is blind below
/// about 1.5x decimation by construction, so no threshold on it would have
/// worked.
#[test]
fn the_frame_medians_own_step_is_blind_to_a_third_of_the_scans_going_missing() {
    for (listing_scans, held) in [(20usize, 14usize), (89, 60)] {
        let (frames, _) = loop_from_listing(&vec![WSR_PRECIP; listing_scans - 1], held);
        let mut gaps: Vec<i64> = frames
            .windows(2)
            .map(|pair| (pair[1].timestamp - pair[0].timestamp).num_seconds())
            .collect();
        gaps.sort_unstable();
        let typical = gaps[gaps.len() / 2];

        assert_eq!(
            typical, WSR_PRECIP,
            "{listing_scans}-into-{held}: the frame median moved off the listing \
             step, so this test is no longer about the blind case",
        );
        assert!(
            !markedly_longer(typical, WSR_PRECIP),
            "the old fidelity rule fired at {listing_scans}-into-{held}, so the \
             defect it was measured on is not reproduced here",
        );
    }
}

/// A sampled loop with no measurable listing cadence says it is sampled and
/// quotes no figure. Unreachable from `accept_scan_listing` — a listing short
/// enough to have no gap fits every cap — and written down anyway, because the
/// two facts are independent and an arm that assumed otherwise would print
/// "sampled from ~0 min scans".
#[test]
fn a_sampled_loop_with_no_known_cadence_quotes_no_figure() {
    let phrase = loop_span_phrase(&loop_frames(&[WSR_PRECIP; 13]), Some(true), None, true)
        .expect("frames enough for a span");
    assert_eq!(
        phrase,
        "This loop spans 56 min over 14 frames, sampled, ~4 min apart".to_owned()
    );
}

/// **Still filling.** The counts can still move, so every clause takes "so far"
/// rather than the caption waiting for the loop to finish — which is exactly
/// when the user is watching it.
#[test]
fn a_loop_still_filling_says_so_far() {
    assert_eq!(
        loop_span_phrase(
            &loop_frames(&[WSR_PRECIP; 13]),
            Some(false),
            cadence(WSR_PRECIP),
            false,
        ),
        Some("This loop spans 56 min over 14 frames so far, every scan, ~4 min apart".to_owned())
    );
}

/// **A single frame.** There is a count but no span, and "spans 0 s" would read
/// as a broken loop rather than a young one.
#[test]
fn a_single_frame_has_no_span_to_report() {
    assert_eq!(
        loop_span_phrase(&loop_frames(&[]), Some(false), cadence(TDWR), false),
        Some("This loop is 1 frame so far, so it spans no time yet".to_owned())
    );
    assert_eq!(
        loop_span_phrase(&loop_frames(&[]), Some(false), cadence(TDWR), true),
        Some("This loop is 1 frame, so it spans no time yet".to_owned())
    );
}

/// Several frames sharing one timestamp take the single-frame sentence too:
/// the count is real, the span is not, and the plural follows the count.
#[test]
fn frames_at_one_instant_report_the_count_and_no_span() {
    assert_eq!(
        loop_span_phrase(&loop_frames(&[0, 0]), Some(false), cadence(TDWR), true),
        Some("This loop is 3 frames, so it spans no time yet".to_owned())
    );
}

/// **A gap in the frames**, from a scan that never landed. The range makes the
/// hole visible; "every scan" stays true because the listing had no more to
/// give, and it is the listing this loop is faithful to.
#[test]
fn a_dropped_scan_shows_up_as_a_range() {
    let mut gaps = vec![TDWR; 18];
    gaps.push(2 * TDWR);
    assert_eq!(
        loop_span_phrase(&loop_frames(&gaps), Some(false), cadence(TDWR), true),
        Some("This loop spans 2h 0m over 20 frames, every scan, 6 min to 12 min apart".to_owned())
    );
}

/// **A VCP change mid-loop.** Not a corner case: on the day measured, every
/// site sampled but TDFW alternated VCPs through the day.
///
/// This and the dropped scan above are deliberately not told apart. From
/// timestamps alone they are the same evidence — one run of gaps at roughly
/// twice another — so the caption reports the range it can defend and never
/// guesses at a cause.
#[test]
fn a_mid_loop_vcp_change_widens_the_range_rather_than_being_explained() {
    let mut gaps = vec![WSR_PRECIP; 15];
    gaps.extend(std::iter::repeat_n(WSR_CLEAR_AIR, 14));
    assert_eq!(
        loop_span_phrase(&loop_frames(&gaps), Some(false), cadence(WSR_PRECIP), true),
        Some("This loop spans 3h 5m over 30 frames, every scan, 4 min to 9 min apart".to_owned())
    );
}

/// With no cadence recorded — a loop built before the field existed, or a
/// listing too short to have a gap — the caption states the spacing and claims
/// nothing about fidelity. Silence, not a guess.
#[test]
fn without_a_known_cadence_the_caption_makes_no_fidelity_claim() {
    let phrase = loop_span_phrase(&loop_frames(&[WSR_PRECIP; 13]), None, None, true)
        .expect("frames enough for a span");
    assert_eq!(
        phrase,
        "This loop spans 56 min over 14 frames, ~4 min apart".to_owned()
    );
    assert!(!phrase.contains("every scan"));
    assert!(!phrase.contains("sampled"));
}
