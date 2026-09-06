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
