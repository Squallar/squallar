//! **Both arms of the promotion rule, and which is which.**
//!
//! A promotion path is only proved by a suite that goes red when the
//! promotion is removed AND stays green on an input that *resembles* a
//! recovery without being one. Redness on the defect is the easy half.
//! Over-firing is the worse direction here — a ladder that promotes on noise
//! re-rasterises every picture on consecutive ticks, which is the Android
//! behaviour this work exists to prevent — so the tests below are named for
//! what they defend against rather than for what they assert, and the
//! defending half outnumbers the enabling half.
//!
//! **What SHOULD promote and does**:
//! `a_recovery_that_holds_the_margin_promotes_one_step_after_the_dwell_and_no_sooner`.
//!
//! **What looks like a recovery and must produce ZERO promotions**: a dip
//! that does not hold, a sawtooth across the act line, a recovery short of
//! the margin, an oscillation around the scene's own need, and a spare inside
//! the dead band the margin's multiplier creates.

use super::*;
use crate::pressure::LinearMemoryWatch;
use squallar_device_profile::linear_memory::{
    LINEAR_MEMORY_REFIRE_STEP_BYTES, LinearMemoryVerdict, act_line,
};

const MIB: u64 = 1 << 20;

/// The desktop-classified page instance's wall, which is also the bound the
/// module is linked with.
const PAGE_MAX: u64 = 1024 * MIB;

/// **The `huge` leg's own picture**, per side at `percent` oversampling: the
/// user's 2878x1651 window less its forty-point top bar, four bytes a texel.
/// Firefox's allocation failures on that leg asked for exactly
/// `4317 * 2416 * 4` twelve times over, which is `picture(150)`.
fn picture(percent: u64) -> u64 {
    (2878 * percent / 100) * (1611 * percent / 100) * 4
}

/// Thirteen shown pictures plus the one arrival in flight — what the need
/// model prices as `pictures_host + picture_arrival_host`, and what the
/// watermark's action line is the wall less.
fn batch(percent: u64) -> u64 {
    14 * picture(percent)
}

/// The oversampling in force once a squeeze has shed one rung, and the rung
/// a promotion would give back.
const SQUEEZED: u64 = 125;
const WHOLE: u64 = 150;

/// The wasm bracket's declared host figure.
const HOST: u64 = 1 << 30;

/// Three quarters of it — `NEED_FRACTION`, the fraction every host figure is
/// allowed.
const ALLOWANCE: u64 = 768 * MIB;

/// What one promotion costs the host on this scene: the 125 %-to-150 % rung.
fn delta() -> u64 {
    batch(WHOLE) - batch(SQUEEZED)
}

/// What [`promotion_qualifies`] asks for once the promotion is paid for.
fn wanted() -> u64 {
    delta() * HOST_RECOVERY_MARGIN_DELTAS + LINEAR_MEMORY_REFIRE_STEP_BYTES
}

/// **The leg's figures, so every threshold below is a byte figure from a real
/// scene rather than a round number chosen to make a test pass.**
#[test]
fn the_fixtures_are_the_huge_legs_own_figures() {
    assert_eq!(picture(WHOLE), 41_719_488, "the leg's own reported picture");
    assert_eq!(batch(WHOLE), 584_072_832);
    assert_eq!(batch(SQUEEZED), 405_482_616);
    assert_eq!(delta(), 178_590_216, "one rung of overlay oversampling");
    assert_eq!(LINEAR_MEMORY_REFIRE_STEP_BYTES, 32 * MIB);
    assert_eq!(wanted(), 212_144_648);
    // The act line for the PROMOTED batch: the wall less that batch, which is
    // under the percentage line, so it is the line that binds.
    assert_eq!(act_line(PAGE_MAX, batch(WHOLE)), 489_668_992);
    // Live bytes at or under this leave `wanted()` of room after promoting;
    // one byte more does not. Every "recovers" and "does not recover" trace
    // below is placed against this one number.
    assert_eq!(act_line(PAGE_MAX, batch(WHOLE)) - wanted(), 277_524_344);
    assert!(
        walled_room_after(PAGE_MAX, batch(WHOLE), 277_524_344) >= wanted(),
        "the qualifying side of the threshold"
    );
    assert!(
        walled_room_after(PAGE_MAX, batch(WHOLE), 277_524_345) < wanted(),
        "the refusing side of the threshold"
    );
    // The model side has room to spare at this rung, so it is the heap
    // reading and not the model that decides every trace below.
    assert!(ALLOWANCE - batch(WHOLE) >= wanted());
}

/// Live bytes comfortably under the promotion threshold, and comfortably over
/// it.
const RECOVERED: u64 = 200 * MIB;
const STILL_FULL: u64 = 300 * MIB;

/// **One session of the two halves wired together**, as `App` wires them:
/// the watermark judges `byteLength` against the wall less the batch the rung
/// in force would rasterise, and the recovery judges live bytes against what
/// the promoted rung would leave spare.
///
/// A model of the application's loop and not the loop itself — the ladder is
/// two rungs here where `fit` walks seven — but every figure in it is the
/// application's own function, so a trace that oscillates here is a trace
/// that oscillates there.
struct Session {
    watch: LinearMemoryWatch,
    recovery: HostRecovery,
    /// Every reading, in order, with what it did — so a test can assert on
    /// the whole trace rather than on the end state.
    log: Vec<&'static str>,
}

impl Session {
    fn new() -> Self {
        Self {
            watch: LinearMemoryWatch::default(),
            recovery: HostRecovery::untouched(),
            log: Vec::new(),
        }
    }

    /// The oversampling in force: the whole rung until something is shed.
    fn rung(&self) -> u64 {
        if self.recovery.is_squeezed() {
            SQUEEZED
        } else {
            WHOLE
        }
    }

    /// One capacity reading: `byteLength` and live bytes, both in bytes.
    fn reading(&mut self, page_bytes: u64, live: u64) {
        let headroom_now = batch(self.rung());
        match self.watch.observe(page_bytes, PAGE_MAX, headroom_now) {
            LinearMemoryVerdict::Act => {
                self.recovery.squeeze(Some(HOST), page_bytes);
                self.log.push("squeeze");
            }
            _ => {
                let qualified = promotion_qualifies(
                    batch(self.rung()),
                    batch(WHOLE),
                    ALLOWANCE,
                    Some(walled_room_after(PAGE_MAX, batch(WHOLE), live)),
                );
                if self.recovery.observe(qualified) {
                    self.log.push("promote");
                } else {
                    self.log.push("hold");
                }
            }
        }
    }

    fn count(&self, what: &str) -> usize {
        self.log.iter().filter(|entry| **entry == what).count()
    }
}

/// **Pressure is answered on the reading it arrives on.** No dwell, no
/// margin, no bank: the level is up and the ceiling is down before the call
/// returns. Nothing in this module may slow the response to real pressure,
/// and the whole of it is about the other direction.
#[test]
fn pressure_takes_its_step_on_the_reading_it_arrives_on() {
    let mut recovery = HostRecovery::untouched();
    assert!(!recovery.is_squeezed());
    assert_eq!(recovery.ceiling(), None);

    let ceiling = recovery.squeeze(Some(HOST), 891 * MIB);
    assert_eq!(
        ceiling,
        Some(891 * MIB / 10 * 9),
        "the mark, one economy fraction"
    );
    assert_eq!(recovery.ceiling(), ceiling);
    assert_eq!(recovery.level(), 1);
    assert_eq!(recovery.acts(), 1);
    assert!(
        recovery.is_squeezed(),
        "the tile economies are not squeezed"
    );
    assert_eq!(recovery.held(), 0);

    // A second event, immediately: another step, again with no dwell. `host`
    // is the figure IN FORCE, which `App::capacity()` has already held to the
    // ceiling above — the caller hands in the modulated capacity, not the
    // bracket's raw figure — so the step compounds and the presumption
    // converges. These are the same two byte figures
    // `a_page_heap_event_lowers_the_host_ceiling_on_the_wasm_bracket` pins
    // through the whole application.
    let deeper = recovery.squeeze(ceiling, 900 * MIB);
    assert_eq!(ceiling, Some(840_853_089));
    assert_eq!(
        deeper,
        Some(756_767_772),
        "the second step is one fraction under the figure in force"
    );
    assert_eq!(recovery.level(), 2);
    assert_eq!(recovery.acts(), 2);
    // A later event that reads a HIGHER mark never raises the ceiling:
    // pressure may not buy capacity.
    let mut rising = HostRecovery::untouched();
    rising.squeeze(Some(HOST), 500 * MIB);
    let after = rising.squeeze(Some(HOST), 1000 * MIB);
    assert_eq!(
        after,
        Some(500 * MIB / 10 * 9),
        "an event raised the ceiling"
    );

    // **A bracket with no host figure still takes the step.** There is no
    // ceiling to write, and the tile economies go all the same: the level is
    // what holds them at nothing, and it is what a promotion later steps back
    // down. Reading the level off the ceiling would lose the whole event on
    // every bracket that carries no host figure.
    let mut hostless = HostRecovery::untouched();
    assert_eq!(hostless.squeeze(None, 891 * MIB), None);
    assert!(
        hostless.is_squeezed(),
        "a hostless bracket squeezed nothing"
    );
    assert_eq!(hostless.level(), 1);
    assert_eq!(hostless.acts(), 1);
    assert_eq!(hostless.ceiling(), None);
}

/// **What SHOULD promote, and does.** The one enabling arm of this suite.
///
/// A session squeezed once, then a heap that holds still while its live bytes
/// sit under the threshold: the margin qualifies on every reading, and the
/// step comes back on the dwell's reading and on no earlier one. Exactly one
/// step, and nothing more after — a session one step down has nothing left to
/// give back.
#[test]
fn a_recovery_that_holds_the_margin_promotes_one_step_after_the_dwell_and_no_sooner() {
    let mut session = Session::new();
    session.reading(950 * MIB, STILL_FULL);
    assert_eq!(session.count("squeeze"), 1);
    let dwell = session.recovery.dwell();
    assert_eq!(dwell, HOST_RECOVERY_DWELL_READINGS, "the first squeeze");

    // One short of the dwell, all qualifying: nothing has come back yet.
    for reading in 1..dwell {
        session.reading(950 * MIB, RECOVERED);
        assert_eq!(
            session.count("promote"),
            0,
            "promoted on reading {reading} of a {dwell}-reading dwell",
        );
        assert_eq!(session.recovery.held(), reading);
    }
    session.reading(950 * MIB, RECOVERED);
    assert_eq!(session.count("promote"), 1);
    assert_eq!(session.recovery.level(), 0);
    assert!(
        !session.recovery.is_squeezed(),
        "the tile economies did not step back up with the rung"
    );
    assert_eq!(
        session.recovery.ceiling(),
        None,
        "the ceiling was not lifted"
    );

    // Twenty more of the same reading: there is nothing left to promote and
    // the counter does not move.
    for _ in 0..20 {
        session.reading(950 * MIB, RECOVERED);
    }
    assert_eq!(session.count("promote"), 1);
    assert_eq!(session.count("squeeze"), 1);
    assert_eq!(session.recovery.churn(), 0, "a promotion nothing undid");
}

/// **DEFENDS AGAINST: a recovery that is real but too small.** A heap that
/// comes down and stays down, forever, without ever leaving the promoted
/// rung's own byte cost plus a refire step spare, promotes nothing however
/// long it holds. Two hundred readings — sixteen times the dwell at its
/// longest — and the counter stays at zero.
#[test]
fn a_recovery_without_the_margin_never_promotes_however_long_it_holds() {
    let mut session = Session::new();
    session.reading(950 * MIB, STILL_FULL);
    assert_eq!(session.count("squeeze"), 1);
    // One byte past the threshold the fixture pins: a recovery in every sense
    // except the one that matters.
    let short = act_line(PAGE_MAX, batch(WHOLE)) - wanted() + 1;
    assert!(short < STILL_FULL, "the heap really did come down");
    for _ in 0..200 {
        session.reading(950 * MIB, short);
    }
    assert_eq!(session.count("promote"), 0);
    assert_eq!(session.recovery.level(), 1);
    assert_eq!(
        session.recovery.held(),
        0,
        "a reading short of the margin banked toward the dwell",
    );
}

/// **DEFENDS AGAINST: a dip that looks like a recovery.** The margin has to
/// hold *successively*. A trace that qualifies for one reading fewer than the
/// dwell and then does not, over and over, banks nothing: fifty cycles of it
/// — two hundred readings, more qualifying readings in total than any dwell
/// asks for — promote nothing.
#[test]
fn a_dip_that_does_not_hold_for_the_dwell_promotes_nothing() {
    let mut session = Session::new();
    session.reading(950 * MIB, STILL_FULL);
    let dwell = session.recovery.dwell();
    let mut qualifying = 0;
    for _ in 0..50 {
        for _ in 0..dwell - 1 {
            session.reading(950 * MIB, RECOVERED);
            qualifying += 1;
        }
        session.reading(950 * MIB, STILL_FULL);
    }
    assert!(
        qualifying > dwell * 8,
        "the trace did not carry more qualifying readings than any dwell asks for",
    );
    assert_eq!(session.count("promote"), 0);
    assert_eq!(session.recovery.level(), 1);
}

/// **DEFENDS AGAINST: a sawtooth across the act line.** A heap sliding up and
/// down across the line the watermark judges — the shape a page that is
/// genuinely at its limit makes — is not a recovery, and the promotion rule
/// must read it as one exactly zero times. The acts are bounded by the refire
/// step, which is the demotion side's own guard and is left alone here.
#[test]
fn a_sawtooth_across_the_act_line_promotes_nothing() {
    let mut session = Session::new();
    let line = act_line(PAGE_MAX, batch(WHOLE));
    for cycle in 0..60u64 {
        // The tooth: below the line, then above it, then below again, with
        // live bytes tracking `byteLength` the way a heap that is really full
        // does.
        for step in [0u64, 20, 40, 20] {
            let used = line - 40 * MIB + step * MIB + cycle % 3 * MIB;
            session.reading(used, used);
        }
    }
    assert_eq!(session.count("promote"), 0, "a sawtooth read as a recovery");
    assert!(
        session.count("squeeze") <= 2,
        "the refire step stopped bounding the acts: {}",
        session.count("squeeze"),
    );
}

/// **DEFENDS AGAINST the measured Android oscillation.** On an emulator leg
/// whose host allowance sat at about the scene's need, a fit with no
/// hysteresis toggled `steps 0` and `steps 1` on consecutive two-second
/// ticks, re-rasterising every picture at 150 % and then 125 % each tick.
///
/// Replayed here as a reading that oscillates +/- 5 % around the scene's own
/// need, forty times: **one** demotion, and **no** promotion. One demotion
/// because the refire step will not act again until the mark has grown by
/// 32 MiB, which a 5 % wobble at this scale does not do; no promotion because
/// live bytes at the scene's need leave nothing like `delta + 32 MiB` spare
/// after a promotion, which is the whole reason the margin is a band and not
/// a line.
#[test]
fn an_oscillation_around_the_scenes_need_demotes_once_and_never_promotes() {
    let mut session = Session::new();
    let centre = act_line(PAGE_MAX, batch(SQUEEZED)) + 8 * MIB;
    let swing = centre / 20;
    for i in 0..40u64 {
        let used = if i % 2 == 0 {
            centre + swing
        } else {
            centre - swing
        };
        session.reading(used, used);
    }
    assert_eq!(
        session.count("squeeze"),
        1,
        "an oscillation acted more than once: {:?}",
        session.log,
    );
    assert_eq!(
        session.count("promote"),
        0,
        "an oscillation was read as a recovery: {:?}",
        session.log,
    );
    assert_eq!(session.recovery.churn(), 0);
}

/// **The trace the brief asks for: rises, falls below the act line, rises
/// again.** The act count is BOUNDED and the level steps back up exactly once
/// per dwell — not once per qualifying reading, and not in a jump.
#[test]
fn a_reading_that_rises_falls_and_rises_again_acts_a_bounded_number_of_times() {
    let mut session = Session::new();
    // Rise: one action.
    session.reading(950 * MIB, STILL_FULL);
    assert_eq!(session.count("squeeze"), 1);
    assert_eq!(session.recovery.level(), 1);

    // Fall: the margin holds, and the level steps up once per dwell and not
    // once per reading.
    let first_dwell = session.recovery.dwell();
    for _ in 0..first_dwell * 3 {
        session.reading(950 * MIB, RECOVERED);
    }
    assert_eq!(
        session.count("promote"),
        1,
        "three dwells of held margin gave back more than the one step there was",
    );
    assert_eq!(session.recovery.level(), 0);

    // Rise again: the mark has to grow by the refire step to act a second
    // time, and it does.
    session.reading(950 * MIB + LINEAR_MEMORY_REFIRE_STEP_BYTES, STILL_FULL);
    assert_eq!(session.count("squeeze"), 2);
    assert_eq!(session.recovery.level(), 1);
    assert_eq!(
        session.recovery.churn(),
        0,
        "a squeeze a whole dwell after the promotion was counted as churn",
    );

    // BOUNDED: across the whole trace, two actions and one promotion, and the
    // second dwell is twice the first.
    assert_eq!(session.recovery.acts(), 2);
    assert_eq!(session.recovery.promotions(), 1);
    assert_eq!(session.recovery.dwell(), first_dwell * 2);
    for _ in 0..first_dwell * 2 - 1 {
        session.reading(950 * MIB + LINEAR_MEMORY_REFIRE_STEP_BYTES, RECOVERED);
    }
    assert_eq!(
        session.count("promote"),
        1,
        "the second promotion came before the lengthened dwell",
    );
    session.reading(950 * MIB + LINEAR_MEMORY_REFIRE_STEP_BYTES, RECOVERED);
    assert_eq!(session.count("promote"), 2);
    assert_eq!(session.recovery.level(), 0);
}

/// **A repeat squeeze lengthens the dwell, doubling to a cap**, so a session
/// that is pressured periodically cannot thrash; and a squeeze that lands
/// within [`HOST_RECOVERY_CHURN_READINGS`] of a promotion is counted, because
/// that count is the only thing that can ever say from the field that
/// [`HOST_RECOVERY_MARGIN_DELTAS`] is too lax.
#[test]
fn a_repeat_squeeze_lengthens_the_dwell_and_counts_the_promotion_it_undid() {
    let mut recovery = HostRecovery::untouched();
    assert_eq!(recovery.dwell(), HOST_RECOVERY_DWELL_READINGS);
    let mut seen = Vec::new();
    for _ in 0..6 {
        recovery.squeeze(Some(HOST), 900 * MIB);
        seen.push(recovery.dwell());
    }
    assert_eq!(
        seen,
        vec![
            HOST_RECOVERY_DWELL_READINGS,
            HOST_RECOVERY_DWELL_READINGS * 2,
            HOST_RECOVERY_DWELL_READINGS * 4,
            HOST_RECOVERY_DWELL_READINGS * 8,
            HOST_RECOVERY_DWELL_READINGS * 8,
            HOST_RECOVERY_DWELL_READINGS * 8,
        ],
        "the doubling is not capped at 8x, or is not keyed on acts ever",
    );

    // Churn: promote, then squeeze on the next reading.
    let mut churning = HostRecovery::untouched();
    churning.squeeze(Some(HOST), 900 * MIB);
    for _ in 0..churning.dwell() {
        churning.observe(true);
    }
    assert_eq!(churning.promotions(), 1);
    assert_eq!(churning.churn(), 0);
    churning.squeeze(Some(HOST), 900 * MIB);
    assert_eq!(
        churning.churn(),
        1,
        "a squeeze one reading after a promotion was not counted as churn",
    );

    // And a squeeze the window later is not churn: the promotion held.
    let mut settled = HostRecovery::untouched();
    settled.squeeze(Some(HOST), 900 * MIB);
    for _ in 0..settled.dwell() {
        settled.observe(true);
    }
    for _ in 0..HOST_RECOVERY_CHURN_READINGS {
        settled.observe(false);
    }
    settled.squeeze(Some(HOST), 900 * MIB);
    assert_eq!(
        settled.churn(),
        0,
        "a squeeze past the churn window was counted as churn",
    );
}

/// **DEFENDS AGAINST nobody knowing: the dead band this margin creates.**
///
/// [`HOST_RECOVERY_MARGIN_DELTAS`] asks for the promoted rung's own byte cost
/// to remain spare *again* after the promotion is paid for. So a device whose
/// spare sits between `delta + 32 MiB` and `2 x delta + 32 MiB` promotes
/// **never** — it is not a slower recovery, it is no recovery — and on this
/// scene's own figures that band is about 210 to 390 MB wide. That is the
/// right direction to err, because a device inside it is exactly as well off
/// as it was before this module existed while a device promoted on a margin
/// that was not there is a new visible defect. It is still a real population,
/// and this test is here so that nobody has to discover it by reading
/// arithmetic.
#[test]
fn a_spare_inside_the_dead_band_this_margin_creates_never_promotes() {
    let need_now = batch(SQUEEZED);
    let need_after = batch(WHOLE);
    let promoted_spare = |spare_after: u64| {
        promotion_qualifies(
            need_now,
            need_after,
            need_after + spare_after,
            Some(u64::MAX),
        )
    };
    // Spare after the promotion, in the band's own terms.
    assert!(
        !promoted_spare(LINEAR_MEMORY_REFIRE_STEP_BYTES),
        "the bottom of the band promoted",
    );
    assert!(
        !promoted_spare(100 * MIB),
        "a device with 100 MiB spare after promoting is inside the band",
    );
    assert!(
        !promoted_spare(wanted() - 1),
        "the last byte of the band promoted",
    );
    assert!(
        promoted_spare(wanted()),
        "the first byte past the band did not"
    );

    // The band, in the spare a device HAS before it decides — which is what a
    // reader comparing against a machine will want.
    let spare_before = |spare: u64| promoted_spare(spare.saturating_sub(delta()));
    assert!(!spare_before(delta() + LINEAR_MEMORY_REFIRE_STEP_BYTES));
    assert!(!spare_before(
        2 * delta() + LINEAR_MEMORY_REFIRE_STEP_BYTES - 1
    ));
    assert!(spare_before(2 * delta() + LINEAR_MEMORY_REFIRE_STEP_BYTES));
    assert_eq!(
        (2 * delta() + LINEAR_MEMORY_REFIRE_STEP_BYTES)
            - (delta() + LINEAR_MEMORY_REFIRE_STEP_BYTES),
        178_590_216,
        "the band's width is one rung's delta, 170 MiB on this scene",
    );
}

/// **DEFENDS AGAINST a promotion nothing can observe.** Where no counting
/// allocator answered there is no falling figure at all, and the model's own
/// spare — which has no term for loop scans, the extract cache, egui's
/// buffers or the deferred-drop queue — is exactly the arithmetic that would
/// admit one more layer into a trap. A recovery has to be observed or it has
/// not happened.
#[test]
fn a_promotion_with_no_falling_reading_to_observe_it_never_qualifies() {
    let generous = promotion_qualifies(batch(SQUEEZED), batch(WHOLE), u64::MAX, Some(u64::MAX));
    assert!(generous, "precondition: everything else about it qualifies");
    assert!(
        !promotion_qualifies(batch(SQUEEZED), batch(WHOLE), u64::MAX, None),
        "a promotion was taken on the model's figure with nothing to bound it",
    );
}

/// A promotion that costs nothing still needs the refire step spare: a rung
/// whose delta is zero is free to take, but taking it on a heap with less
/// than one refire step of room is taking it on noise.
#[test]
fn a_promotion_that_costs_nothing_still_needs_the_refire_step() {
    let need = batch(WHOLE);
    assert!(!promotion_qualifies(
        need,
        need,
        need + LINEAR_MEMORY_REFIRE_STEP_BYTES - 1,
        Some(u64::MAX)
    ));
    assert!(promotion_qualifies(
        need,
        need,
        need + LINEAR_MEMORY_REFIRE_STEP_BYTES,
        Some(u64::MAX)
    ));
}

/// The step cap bounds the stack without stopping the ceiling from falling: a
/// session squeezed past it keeps taking steps down and holds the cap's worth
/// of them.
#[test]
fn the_step_cap_bounds_the_stack_and_the_ceiling_still_falls() {
    let mut recovery = HostRecovery::untouched();
    let mut last = u64::MAX;
    for i in 0..HOST_RECOVERY_MAX_STEPS * 2 {
        let ceiling = recovery
            .squeeze(Some(HOST), 900 * MIB - i as u64 * MIB)
            .expect("a host figure was handed in");
        assert!(ceiling <= last, "the ceiling rose at step {i}");
        last = ceiling;
    }
    assert_eq!(recovery.level(), HOST_RECOVERY_MAX_STEPS as u32);
    assert_eq!(recovery.acts(), HOST_RECOVERY_MAX_STEPS as u32 * 2);
    assert_eq!(recovery.ceiling(), Some(last));
}

/// An unsqueezed session banks nothing however many readings qualify: there
/// is no ladder to climb and the bank would otherwise fire the instant the
/// first squeeze landed.
#[test]
fn an_unsqueezed_session_banks_no_dwell_against_a_future_squeeze() {
    let mut recovery = HostRecovery::untouched();
    for _ in 0..100 {
        assert!(!recovery.observe(true));
    }
    assert_eq!(recovery.held(), 0);
    recovery.squeeze(Some(HOST), 900 * MIB);
    assert!(!recovery.observe(true), "a banked dwell promoted at once");
    assert_eq!(recovery.held(), 1);
}

/// **The card's axis, whose whole rule is a clock.**
///
/// [`GpuRecovery`] has no promotion predicate and cannot have one: nothing in
/// this tree observes a card's memory coming back, so there is no "margin
/// held" to assert against. What is asserted instead is the shape that stands
/// in for one — a dwell, a doubling that a repeat event pays for, a cap on
/// that doubling, and one step per dwell and no more.
///
/// **The defending half.** As on the host side, over-firing is the worse
/// direction: a ceiling that lifts while the wall is still there re-fits the
/// scene up a rung and is squeezed straight back down, and the user sees the
/// pumping. So the tests that must produce ZERO restorations — quiet short of
/// the dwell, and a session squeezed inside every dwell it is given — sit
/// beside the one that must produce exactly one.
mod gpu {
    use crate::recovery::{
        GPU_RECOVERY_DWELL, GPU_RECOVERY_DWELL_MAX_DOUBLINGS, GPU_RECOVERY_MAX_STEPS, GpuRecovery,
    };

    const MIB: u64 = 1 << 20;

    /// A squeeze that steps a 1 GiB ceiling down by an economy fraction each
    /// time, the way `App::refit_under_pressure` does, with no floor in the
    /// way.
    fn step(recovery: &mut GpuRecovery, at: web_time::Instant, from: u64) -> u64 {
        recovery.squeeze(from / 10 * 9, at)
    }

    /// **A repeat event doubles the dwell, and the doubling stops at 8x.**
    ///
    /// The doubling is what bounds thrash: a session whose wall has not gone
    /// away pays for each wrong inference with a longer wait before the next
    /// one. The CAP is what keeps restoration reachable at all — without it a
    /// periodic out-of-memory source (a software rasterizer, a resize storm,
    /// another application taking the pool) doubles the dwell past any
    /// session's length and the axis is a one-way latch wearing a dwell's
    /// name, which is the defect this module exists to remove.
    #[test]
    fn a_repeat_event_doubles_the_dwell_and_the_doubling_stops_at_the_cap() {
        let t0 = web_time::Instant::now();
        let mut recovery = GpuRecovery::untouched();
        assert_eq!(
            recovery.dwell(),
            GPU_RECOVERY_DWELL,
            "a session with no event does not print the base dwell",
        );

        let mut seen = Vec::new();
        for _ in 0..8 {
            recovery.squeeze(512 * MIB, t0);
            seen.push(recovery.dwell_multiplier());
        }
        assert_eq!(
            seen,
            vec![1, 2, 4, 8, 8, 8, 8, 8],
            "the dwell did not double once per event, or did not stop at the cap",
        );
        assert_eq!(
            recovery.dwell(),
            GPU_RECOVERY_DWELL * (1 << GPU_RECOVERY_DWELL_MAX_DOUBLINGS),
            "the capped dwell is not the constant it is documented as",
        );
    }

    /// **A dwell with no new event restores exactly one step, and the next
    /// step costs a whole dwell again.**
    ///
    /// One at a time is what keeps a restoration cheap to be wrong about: a
    /// session ten steps down that jumped back to the top would re-fit the
    /// whole ladder on one inference, and the re-squeeze would be ten steps
    /// of scene churn rather than one.
    #[test]
    fn a_dwell_with_no_new_event_restores_exactly_one_step() {
        let t0 = web_time::Instant::now();
        let mut recovery = GpuRecovery::untouched();
        let first = step(&mut recovery, t0, 1024 * MIB);
        let second = step(&mut recovery, t0, first);
        let third = step(&mut recovery, t0, second);
        assert_eq!(recovery.ceiling(), Some(third));
        assert_eq!(recovery.level(), 3);

        // Three events, so the dwell is 4x the base by now.
        let dwell = recovery.dwell();
        assert_eq!(dwell, GPU_RECOVERY_DWELL * 4);

        assert!(recovery.observe(t0 + dwell), "the dwell restored nothing");
        assert_eq!(
            recovery.ceiling(),
            Some(second),
            "the restoration did not step back to the ceiling under the top",
        );
        assert_eq!(recovery.level(), 2);
        assert_eq!(recovery.restorations(), 1);

        // And no second step comes back on the same clock: the quiet restarts
        // from the restoration.
        assert!(
            !recovery.observe(t0 + dwell),
            "one dwell released two steps",
        );
        assert!(
            !recovery.observe(t0 + dwell + dwell - std::time::Duration::from_millis(1)),
            "a step came back a millisecond short of the second dwell",
        );
        assert!(
            recovery.observe(t0 + dwell + dwell),
            "the second dwell restored nothing",
        );
        assert_eq!(recovery.ceiling(), Some(first));
        assert_eq!(recovery.level(), 1);
        assert_eq!(recovery.restorations(), 2);

        // The last step leaves no ceiling at all, which is the whole point:
        // the session is back to what the device profile and the user's share
        // decide, with nothing learned held against it.
        assert!(recovery.observe(t0 + dwell * 3));
        assert_eq!(recovery.ceiling(), None);
        assert!(!recovery.is_squeezed());
        assert!(
            !recovery.observe(t0 + dwell * 9),
            "an unsqueezed session restored a step it never took",
        );
    }

    /// **Quiet short of the dwell restores nothing** — the input that
    /// resembles a recovery without being one.
    #[test]
    fn quiet_short_of_the_dwell_restores_nothing() {
        let t0 = web_time::Instant::now();
        let mut recovery = GpuRecovery::untouched();
        recovery.squeeze(512 * MIB, t0);
        let dwell = recovery.dwell();
        for cut in [1u32, 2, 4, 10, 100] {
            let at = t0 + dwell - dwell / cut;
            assert!(
                !recovery.observe(at),
                "a step came back after {:?} of a {dwell:?} dwell",
                at.duration_since(t0),
            );
        }
        assert_eq!(recovery.level(), 1);
        assert_eq!(recovery.restorations(), 0);
    }

    /// **A session squeezed inside every dwell it is given never restores**,
    /// and the counters say the ceiling is still going one way.
    ///
    /// This is the periodic-source case with the doubling working as
    /// intended: each event lands before the quiet it would have taken to
    /// restore, so nothing is inferred and nothing is given back. What the
    /// cap guarantees is only that the wait stops growing, not that a session
    /// under continuous pressure gets anything back — pressure still wins,
    /// which is the correct direction.
    #[test]
    fn a_session_squeezed_inside_every_dwell_never_restores() {
        let t0 = web_time::Instant::now();
        let mut recovery = GpuRecovery::untouched();
        let mut at = t0;
        let mut ceiling = 1024 * MIB;
        for _ in 0..20 {
            ceiling = step(&mut recovery, at, ceiling);
            // Half a dwell later, another event — and a look at the clock in
            // between, which must find nothing.
            at += recovery.dwell() / 2;
            assert!(
                !recovery.observe(at),
                "a step came back half a dwell after the event that took it",
            );
        }
        assert_eq!(recovery.restorations(), 0);
        assert_eq!(
            recovery.churn(),
            0,
            "nothing was restored, so nothing churned"
        );
        assert_eq!(
            recovery.level() as usize,
            GPU_RECOVERY_MAX_STEPS,
            "twenty events did not stop stacking at the cap",
        );
    }

    /// **A squeeze that lands inside a dwell of a restoration is counted as
    /// churn.**
    ///
    /// [`GPU_RECOVERY_DWELL`] is argued from what a card's bursts look like,
    /// not measured, and this counter is the only thing that can ever say
    /// from the field that the argument was too short. It rides the `budget
    /// state:` line for exactly that reason.
    #[test]
    fn a_squeeze_inside_a_dwell_of_a_restoration_is_counted_as_churn() {
        let t0 = web_time::Instant::now();
        let mut recovery = GpuRecovery::untouched();
        recovery.squeeze(512 * MIB, t0);
        let dwell = recovery.dwell();
        assert!(recovery.observe(t0 + dwell));

        recovery.squeeze(512 * MIB, t0 + dwell);
        assert_eq!(
            recovery.churn(),
            1,
            "an event on the heels of a restoration was not counted as churn",
        );

        // A restoration that stands for longer than the dwell before the next
        // event is not churn: the inference was right for as long as it was
        // asked to be.
        let mut settled = GpuRecovery::untouched();
        settled.squeeze(512 * MIB, t0);
        let dwell = settled.dwell();
        assert!(settled.observe(t0 + dwell));
        settled.squeeze(512 * MIB, t0 + dwell + dwell + dwell);
        assert_eq!(
            settled.churn(),
            0,
            "a restoration that outlived its own dwell was counted as wrong",
        );
    }

    /// **Pressure never buys capacity.** A step is never above the one in
    /// force, whatever figure the caller's arithmetic arrived at, and a
    /// restoration only ever gives back a ceiling this session actually held.
    #[test]
    fn a_squeeze_never_raises_the_ceiling_and_a_restoration_only_gives_back_what_was_held() {
        let t0 = web_time::Instant::now();
        let mut recovery = GpuRecovery::untouched();
        recovery.squeeze(512 * MIB, t0);
        recovery.squeeze(4096 * MIB, t0);
        assert_eq!(
            recovery.ceiling(),
            Some(512 * MIB),
            "an event raised the ceiling above the step already in force",
        );
        let dwell = recovery.dwell();
        assert!(recovery.observe(t0 + dwell));
        assert_eq!(
            recovery.ceiling(),
            Some(512 * MIB),
            "a restoration invented a ceiling this session never held",
        );
    }
}
