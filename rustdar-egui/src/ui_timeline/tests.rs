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
        loop_span_phrase(&loop_frames(&measured), cadence(WSR_PRECIP), true),
        Some("This loop spans 2h 8m over 30 frames, every scan, ~4 min apart".to_owned())
    );
}

/// Nothing to say for a pane with no frames — every pane that is not looping,
/// and a loop still fetching its listing.
#[test]
fn a_loop_with_no_frames_says_nothing() {
    assert_eq!(loop_span_phrase(&[], cadence(WSR_PRECIP), true), None);
}

/// **Full fidelity.** An hour of WSR-88D precip fits the cap whole, so the
/// caption commits to "every scan" and the user knows nothing is being skipped.
#[test]
fn a_loop_holding_every_scan_says_so() {
    assert_eq!(
        loop_span_phrase(&loop_frames(&[WSR_PRECIP; 13]), cadence(WSR_PRECIP), true),
        Some("This loop spans 56 min over 14 frames, every scan, ~4 min apart".to_owned())
    );
}

/// The same loop on a TDWR, at the measured 359 s the gaps actually carry —
/// the rounding case, end to end. A truncating formatter would report this
/// six-minute radar as five-minute in both numbers at once.
#[test]
fn a_tdwr_loop_reports_six_minute_volumes_not_five() {
    assert_eq!(
        loop_span_phrase(&loop_frames(&[359; 10]), cadence(359), true),
        Some("This loop spans 1h 0m over 11 frames, every scan, ~6 min apart".to_owned())
    );
}

/// **Decimated** — the case the whole clause exists for, built from the real
/// sampling arithmetic rather than a guess at it.
///
/// A 24 h lookback on a WSR-88D in precip lists ~333 volumes; a desktop loop
/// keeps 60. `accept_scan_listing` does not truncate that, it evenly samples,
/// so the span stays a full day and four scans in five are silently dropped.
/// "spans 23h 53m" alone would be true and would hide it; "sampled from ~4 min
/// scans, ~26 min apart" is the same loop, unable to hide it.
#[test]
fn a_decimated_loop_admits_it_is_showing_a_sample() {
    // The frame list `accept_scan_listing` would build: `scans[i * (total - 1)
    // / (held - 1)]` over a 333-scan listing, capped at the desktop 60.
    let (total, held) = (333_i64, 60_i64);
    let sampled: Vec<i64> = (0..held).map(|i| i * (total - 1) / (held - 1)).collect();
    let gaps: Vec<i64> = sampled
        .windows(2)
        .map(|pair| (pair[1] - pair[0]) * WSR_PRECIP)
        .collect();

    let phrase = loop_span_phrase(&loop_frames(&gaps), cadence(WSR_PRECIP), true);
    assert_eq!(
        phrase,
        Some(
            "This loop spans 23h 53m over 60 frames, sampled from ~4 min scans, ~26 min apart"
                .to_owned()
        )
    );
}

/// **Still filling.** The counts can still move, so every clause takes "so far"
/// rather than the caption waiting for the loop to finish — which is exactly
/// when the user is watching it.
#[test]
fn a_loop_still_filling_says_so_far() {
    assert_eq!(
        loop_span_phrase(&loop_frames(&[WSR_PRECIP; 13]), cadence(WSR_PRECIP), false),
        Some("This loop spans 56 min over 14 frames so far, every scan, ~4 min apart".to_owned())
    );
}

/// **A single frame.** There is a count but no span, and "spans 0 s" would read
/// as a broken loop rather than a young one.
#[test]
fn a_single_frame_has_no_span_to_report() {
    assert_eq!(
        loop_span_phrase(&loop_frames(&[]), cadence(TDWR), false),
        Some("This loop is 1 frame so far, so it spans no time yet".to_owned())
    );
    assert_eq!(
        loop_span_phrase(&loop_frames(&[]), cadence(TDWR), true),
        Some("This loop is 1 frame, so it spans no time yet".to_owned())
    );
}

/// Several frames sharing one timestamp take the single-frame sentence too:
/// the count is real, the span is not, and the plural follows the count.
#[test]
fn frames_at_one_instant_report_the_count_and_no_span() {
    assert_eq!(
        loop_span_phrase(&loop_frames(&[0, 0]), cadence(TDWR), true),
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
        loop_span_phrase(&loop_frames(&gaps), cadence(TDWR), true),
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
        loop_span_phrase(&loop_frames(&gaps), cadence(WSR_PRECIP), true),
        Some("This loop spans 3h 5m over 30 frames, every scan, 4 min to 9 min apart".to_owned())
    );
}

/// With no cadence recorded — a loop built before the field existed, or a
/// listing too short to have a gap — the caption states the spacing and claims
/// nothing about fidelity. Silence, not a guess.
#[test]
fn without_a_known_cadence_the_caption_makes_no_fidelity_claim() {
    let phrase = loop_span_phrase(&loop_frames(&[WSR_PRECIP; 13]), None, true)
        .expect("frames enough for a span");
    assert_eq!(
        phrase,
        "This loop spans 56 min over 14 frames, ~4 min apart".to_owned()
    );
    assert!(!phrase.contains("every scan"));
    assert!(!phrase.contains("sampled"));
}
