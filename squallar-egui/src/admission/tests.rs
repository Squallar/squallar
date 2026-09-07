//! **Both arms, at every door.**
//!
//! Every refusal fixture here has an admit fixture beside it that *resembles*
//! it — the same act, the same door, one fact changed — because over-firing is
//! the worse direction: a door that refuses what the machine can hold gets
//! switched off, and then nothing is gated at all.
//!
//! Most of what a test here observes is the **verdict** and the counters
//! rather than a changed outcome: the arithmetic is what these doors are, and
//! it is provable without a scene. Whether a refusal is then acted on is
//! [`super::ENFORCING`]'s business, and both of its arms are driven through
//! [`AdmissionLedger::decide`] — the wasm arm is the advisory one and no test
//! in this workspace executes a wasm build.
//!
//! These are the ledger's own rules. The same doors driven through the real
//! chrome are in `crate::ui::admission_door_tests`, which sits under the
//! module the private doors live in.

use super::*;
use squallar_source::id::LayerId;

const MIB: u64 = 1024 * 1024;

/// A cost table with `spare` on both pools and `per_layer` for every pane's
/// picture. Generation 1, so the doors are live — a `0` table admits
/// everything by construction.
fn costs(spare_bytes: u64, per_layer: u64, panes: usize) -> AdmissionCosts {
    AdmissionCosts {
        generation: 1,
        spare: Spare {
            gpu_bytes: Some(spare_bytes),
            host_bytes: Some(spare_bytes),
            joint_bytes: None,
        },
        panes: vec![
            PaneAdmission {
                show_layer: Increment::host(per_layer),
                ..PaneAdmission::default()
            };
            panes.max(1)
        ],
        new_pane: Increment::host(per_layer),
        layer_grids: Vec::new(),
        frames: squallar_device_profile::admit::LoopFrames {
            render_budget: 30,
            // What the capacity reaches; at or above the render budget it is
            // inert, which is the fixture's own arm.
            reachable: 30,
        },
        requested_percent: (100, 100),
    }
}

// ── The ledger ────────────────────────────────────────────────────────────

/// **A table nothing has composed refuses nothing.** Generation `0` is what a
/// fresh application carries before its first scene walk, and a door that
/// refused on it would turn the admission system into a wall at startup —
/// exactly when the config restore is putting the user's panes back.
#[test]
fn a_ledger_with_no_table_admits_everything() {
    let mut ledger = AdmissionLedger::default();
    assert_eq!(
        ledger.ask(Act::ShowLayer, None, Increment::host(u64::MAX)),
        Verdict::Admit,
    );
}

/// **Both arms of one door, one fact apart.** The same act against a spare
/// that holds it and a spare that does not.
#[test]
fn the_same_act_admits_on_a_pool_with_room_and_would_refuse_on_one_without() {
    let mut roomy = AdmissionLedger::default();
    roomy.adopt(&costs(100 * MIB, 10 * MIB, 1));
    assert_eq!(
        roomy.ask(Act::ShowLayer, None, Increment::host(10 * MIB)),
        Verdict::Admit,
    );

    let mut tight = AdmissionLedger::default();
    tight.adopt(&costs(4 * MIB, 10 * MIB, 1));
    assert!(
        !tight
            .ask(Act::ShowLayer, None, Increment::host(10 * MIB))
            .is_admit(),
        "10 MiB against 4 MiB of spare must produce a refusal verdict",
    );
}

/// **The spare is spent as it is admitted.** Six acts inside one telemetry
/// tick are compared against six shrinking spares, not against one standing
/// figure — otherwise a user who clicks six layers between two ticks gets all
/// six admitted on the room for one.
#[test]
fn admitted_increments_are_debited_until_a_fresher_table_arrives() {
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&costs(25 * MIB, 10 * MIB, 1));

    assert_eq!(
        ledger.ask(Act::ShowLayer, None, Increment::host(10 * MIB)),
        Verdict::Admit,
    );
    assert_eq!(ledger.spare().host_bytes, Some(15 * MIB));
    assert_eq!(
        ledger.ask(Act::ShowLayer, None, Increment::host(10 * MIB)),
        Verdict::Admit,
    );
    assert_eq!(ledger.spare().host_bytes, Some(5 * MIB));
    assert!(
        !ledger
            .ask(Act::ShowLayer, None, Increment::host(10 * MIB))
            .is_admit(),
        "the third act must see the two before it",
    );

    // A fresher table clears the debit: its spare already accounts for what
    // was admitted.
    let mut next = costs(25 * MIB, 10 * MIB, 1);
    next.generation = 2;
    ledger.adopt(&next);
    assert_eq!(ledger.spare().host_bytes, Some(25 * MIB));
}

/// A table restated at the same generation changes nothing — the seam's own
/// contract, and what keeps the debit from being cleared every frame.
#[test]
fn restating_one_generation_neither_clears_the_debit_nor_re_adopts() {
    let mut ledger = AdmissionLedger::default();
    let table = costs(25 * MIB, 10 * MIB, 1);
    ledger.adopt(&table);
    let _ = ledger.ask(Act::ShowLayer, None, Increment::host(10 * MIB));
    for _ in 0..10 {
        ledger.adopt(&table);
    }
    assert_eq!(ledger.spare().host_bytes, Some(15 * MIB));
}

/// **A batch charges nothing inside it.** The whole act is priced once by the
/// batch door; the inner doors would otherwise charge the same transitions a
/// second time and refuse a preset the machine can hold.
#[test]
fn an_open_batch_charges_nothing() {
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&costs(25 * MIB, 10 * MIB, 1));
    ledger.begin_batch();
    for _ in 0..10 {
        assert_eq!(
            ledger.ask(Act::ShowLayer, None, Increment::host(10 * MIB)),
            Verdict::Admit,
        );
    }
    ledger.end_batch();
    assert_eq!(
        ledger.spare().host_bytes,
        Some(25 * MIB),
        "a batch's inner doors must not spend the spare its own ask already \
         accounted for",
    );
    // And the door is live again the moment the batch closes.
    assert!(
        !ledger
            .ask(Act::ShowLayer, None, Increment::host(100 * MIB))
            .is_admit(),
    );
}

/// A layer whose grid some pane already holds is charged for the picture and
/// not for the grid — the second pane to show a layer owes nothing for a
/// decoded source that is one instance for the whole application.
#[test]
fn a_resident_grid_is_not_charged_twice() {
    let id = LayerId::new("test.layer");
    let mut table = costs(u64::MAX, 0, 1);
    table.layer_grids = vec![LayerGrid {
        id: id.clone(),
        grid_bytes: 64 * MIB,
        resident: true,
    }];
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&table);
    assert_eq!(ledger.layer_grid(&id), Increment::ZERO);

    table.layer_grids[0].resident = false;
    table.generation = 2;
    ledger.adopt(&table);
    assert_eq!(
        ledger.layer_grid(&id),
        Increment::host(64 * MIB),
        "control: the same layer with nothing holding its grid IS charged — \
         without this the row above passes on a table that prices no grids",
    );
}

/// A pane index the table has never seen asks for nothing. A door addressing
/// a pane composed after the last tick must not refuse on a figure that does
/// not exist.
#[test]
fn a_pane_the_table_has_not_seen_asks_for_nothing() {
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&costs(0, 10 * MIB, 1));
    assert_eq!(ledger.pane(9).show_layer, Increment::ZERO);
}

/// **Ruling 8, at the span door.** A second pane on a loop another pane owns
/// is priced at zero per frame by the App, so the span door charges nothing
/// for it — the admit fixture that resembles the refuse one below.
#[test]
fn a_span_change_charges_the_owner_and_not_its_alias() {
    let owner = PaneAdmission {
        looping: true,
        loop_frame: Increment {
            gpu_bytes: 8 * MIB,
            host_bytes: 0,
        },
        loop_frames_now: 4,
        cadence_secs: Some(300),
        ..PaneAdmission::default()
    };
    let alias = PaneAdmission {
        // The App writes an alias into the prospective scene as sharing the
        // owner's frames, so its per-frame cost prices at nothing.
        loop_frame: Increment::ZERO,
        ..owner
    };
    let mut table = costs(u64::MAX, 0, 1);
    table.panes = vec![owner, alias];

    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&table);
    // Six more frames on each pane; only the owner's cost anything.
    let wanted = table.frames.frames(30 * 60, Some(300));
    let added = wanted.saturating_sub(4) as u64;
    assert!(added > 0, "control: the wider window must add frames");
    let want = table.panes[0]
        .loop_frame
        .times(added)
        .plus(table.panes[1].loop_frame.times(added));
    assert_eq!(
        want,
        Increment {
            gpu_bytes: 8 * MIB * added,
            host_bytes: 0,
        },
        "the alias must contribute nothing to a span change",
    );
    assert_eq!(ledger.ask(Act::LoopSpan, None, want), Verdict::Admit);
}

/// **The notice survives long enough to be read and then goes.** A refusal
/// the user never sees is the defect the notice exists to prevent; one that
/// never leaves is chrome.
#[test]
fn a_notice_ages_out() {
    let mut ledger = AdmissionLedger::default();
    let now = web_time::Instant::now();
    ledger.raise_notice("no room".to_string(), now);
    assert!(ledger.notice(now).is_some());
    assert!(
        ledger.notice(now + NOTICE_LIFETIME / 2).is_some(),
        "the notice must still be up halfway through its life",
    );
    assert!(
        ledger.notice(now + NOTICE_LIFETIME * 2).is_none(),
        "and gone after it",
    );
}

// ── Refusing, and being seen to ───────────────────────────────────────────

/// **A refusal names the setting that produced it**, by the label the
/// Settings screen actually shows. A notice the reader cannot act on is a
/// defect in the code, not a caption to word better - and the two memory
/// shares are exactly what the reader can move.
#[test]
fn a_refusal_names_the_memory_share_it_hit() {
    let mut ledger = AdmissionLedger::default();
    let mut table = costs(0, 10 * MIB, 1);
    table.requested_percent = (45, 60);
    ledger.adopt(&table);

    assert!(!ledger.enforce(Act::ShowLayer, None, Increment::host(10 * MIB)));
    let notice = ledger
        .notice(web_time::Instant::now())
        .expect("a refusal must leave a notice");
    assert!(
        notice.text.contains("System memory"),
        "a host refusal must name the host share: {}",
        notice.text,
    );
    assert!(
        notice.text.contains("60 %"),
        "and the value it is set to now: {}",
        notice.text,
    );
    assert!(
        notice.text.contains("Settings > Memory"),
        "and where to find it: {}",
        notice.text,
    );
    assert!(
        notice.text.contains("this layer"),
        "and what was refused: {}",
        notice.text,
    );
}

/// The GPU arm names the other share, and a unified pool names both - on one
/// memory either control moves the same wall.
#[test]
fn each_pool_names_the_share_that_binds_it() {
    let mut gpu = AdmissionLedger::default();
    let mut table = costs(0, 0, 1);
    table.requested_percent = (30, 70);
    gpu.adopt(&table);
    assert!(!gpu.enforce(
        Act::Panes { added: 1 },
        None,
        Increment {
            gpu_bytes: 8 * MIB,
            host_bytes: 0
        }
    ));
    let text = gpu
        .notice(web_time::Instant::now())
        .expect("a notice")
        .text
        .clone();
    assert!(
        text.contains("GPU memory") && text.contains("30 %"),
        "{text}"
    );
    assert!(
        !text.contains("System memory"),
        "a GPU refusal on a split pool must not send the user to the host \
         slider: {text}",
    );

    let mut joint = AdmissionLedger::default();
    let mut unified = costs(0, 0, 1);
    unified.requested_percent = (30, 70);
    unified.spare.joint_bytes = Some(0);
    joint.adopt(&unified);
    assert!(!joint.enforce(
        Act::Panes { added: 1 },
        None,
        Increment {
            gpu_bytes: 8 * MIB,
            host_bytes: 0
        }
    ));
    let text = joint
        .notice(web_time::Instant::now())
        .expect("a notice")
        .text
        .clone();
    assert!(
        text.contains("GPU memory") && text.contains("System memory"),
        "one memory: either share moves the wall, so the notice names both: \
         {text}",
    );
}

/// **An exemption refuses nothing and leaves no notice.** Restore is never a
/// refusal, and a plate saying otherwise would be a lie on the glass.
#[test]
fn an_exemption_refuses_nothing_and_raises_no_notice() {
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&costs(0, 10 * MIB, 1));
    ledger.begin_exempt();
    assert!(ledger.enforce(Act::ShowLayer, None, Increment::host(u64::MAX)));
    ledger.end_exempt();
    assert!(ledger.notice(web_time::Instant::now()).is_none());
    assert_eq!(ledger.counts().refused, 0);

    // Control: the same act outside the exemption refuses and does raise one.
    assert!(!ledger.enforce(Act::ShowLayer, None, Increment::host(u64::MAX)));
    assert!(ledger.notice(web_time::Instant::now()).is_some());
}

/// A refusal raised on the App's side crosses as its sentence, and is not
/// re-stamped by a frame that restates it - a notice re-raised every frame
/// would never age out.
#[test]
fn a_remote_notice_is_raised_once_and_ages_out() {
    let mut ledger = AdmissionLedger::default();
    let now = web_time::Instant::now();
    ledger.adopt_remote_notice(Some("no room for this loop"), now);
    let raised = ledger
        .notice(now)
        .expect("the remote notice is up")
        .raised_at;
    ledger.adopt_remote_notice(Some("no room for this loop"), now + NOTICE_LIFETIME / 2);
    assert_eq!(
        ledger
            .notice(now + NOTICE_LIFETIME / 2)
            .expect("still up")
            .raised_at,
        raised,
        "restating one sentence must not restamp it",
    );
    assert!(ledger.notice(now + NOTICE_LIFETIME * 2).is_none());
}

// ── Enforcing is per-arm; measuring is not ────────────────────────────────

/// **The advisory arm counts the refusal and lets the act through.**
///
/// `ENFORCING` is `false` on wasm32, and the arm no test in this workspace
/// executes is exactly the arm that took three user-visible functions with it
/// when it started refusing. So the policy is a parameter of
/// [`AdmissionLedger::decide`] and both arms are driven from here.
///
/// What must survive the advisory arm is the **measurement**: the verdict is
/// taken, `would refuse` moves, and the pair `would refuse` / `refused` reads
/// `1 / 0` — which is the whole signal that a door wanted to refuse and did
/// not.
///
/// **And the reader is told.** This asserted *no notice at all* here until
/// 2026-09-07, for a reason that still stands — a plate reading "not enough
/// memory for this layer" beside the layer that did appear is a lie on the
/// glass. The conclusion changed and the reason did not: on the arm every web
/// user is on, a door that computed a refusal, logged it to a console nobody
/// has open and told the person nothing was **invisible by construction**,
/// and the page it was warning about went on to die. So a notice goes up on
/// both arms and only the enforcing one may say the act was refused. What is
/// pinned here is that property, not the absence.
#[test]
fn the_advisory_arm_counts_the_refusal_and_admits() {
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&costs(0, 10 * MIB, 1));

    assert!(
        ledger.decide(Act::ShowLayer, None, Increment::host(10 * MIB), false),
        "an advisory door must let the act through",
    );
    let counts = ledger.counts();
    assert_eq!(counts.asked, 1, "and it still asks");
    assert_eq!(
        counts.would_refuse, 1,
        "and still records that it would have refused",
    );
    assert_eq!(
        counts.refused, 0,
        "and does not claim to have turned anything away",
    );
    let text = ledger
        .notice(web_time::Instant::now())
        .expect("the reader is told, on this arm too")
        .text
        .clone();
    assert!(
        !text.contains("Not enough"),
        "a notice saying the layer was refused, beside the layer that did \
         appear, would be a lie on the glass: {text}",
    );
    assert!(
        text.contains("allowed anyway") && text.contains("may fail"),
        "the advisory notice must say what actually happened - the act went \
         through and the page is at risk: {text}",
    );
    assert!(
        text.contains("11 MB") && text.contains("this layer"),
        "and still name the shortfall and the act: {text}",
    );

    // The enforcing arm's own sentence, for the contrast: one fact changed,
    // and it is the only one allowed to state a refusal.
    let mut enforcing = AdmissionLedger::default();
    enforcing.adopt(&costs(0, 10 * MIB, 1));
    assert!(!enforcing.decide(Act::ShowLayer, None, Increment::host(10 * MIB), true));
    let refused = enforcing
        .notice(web_time::Instant::now())
        .expect("a notice")
        .text
        .clone();
    assert!(refused.contains("Not enough"), "{refused}");
    assert!(
        !refused.contains("allowed anyway"),
        "the enforcing arm did turn the act away and must not say otherwise: \
         {refused}",
    );
}

/// **The enforcing arm, same ledger, same act, one fact changed.** Over-firing
/// is the worse direction, so the pair is what makes either reading mean
/// anything.
#[test]
fn the_enforcing_arm_turns_the_same_act_away() {
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&costs(0, 10 * MIB, 1));

    assert!(!ledger.decide(Act::ShowLayer, None, Increment::host(10 * MIB), true));
    let counts = ledger.counts();
    assert_eq!(counts.would_refuse, 1);
    assert_eq!(counts.refused, 1, "and this one did turn it away");
    assert!(ledger.notice(web_time::Instant::now()).is_some());
}

/// **An act that fits is admitted on either arm** — the policy decides what
/// happens to a refusal and nothing else. Without this the advisory arm would
/// be indistinguishable from a door that had stopped asking.
#[test]
fn a_fitting_act_is_admitted_on_both_arms() {
    for enforcing in [false, true] {
        let mut ledger = AdmissionLedger::default();
        ledger.adopt(&costs(64 * MIB, 10 * MIB, 1));
        assert!(
            ledger.decide(Act::ShowLayer, None, Increment::host(10 * MIB), enforcing),
            "enforcing = {enforcing}",
        );
        let counts = ledger.counts();
        assert_eq!(counts.admitted, 1, "enforcing = {enforcing}");
        assert_eq!(counts.would_refuse, 0, "enforcing = {enforcing}");
        assert_eq!(counts.refused, 0, "enforcing = {enforcing}");
    }
}

/// **`enforce` is `decide` on this build's arm**, so the two cannot drift into
/// two policies. The assertion is written against the constant rather than
/// against `true`, so the wasm build of this file states the same identity.
#[test]
fn enforce_is_decide_on_this_arm() {
    let mut through_enforce = AdmissionLedger::default();
    through_enforce.adopt(&costs(0, 10 * MIB, 1));
    let mut through_decide = AdmissionLedger::default();
    through_decide.adopt(&costs(0, 10 * MIB, 1));

    assert_eq!(
        through_enforce.enforce(Act::ShowLayer, None, Increment::host(10 * MIB)),
        through_decide.decide(Act::ShowLayer, None, Increment::host(10 * MIB), ENFORCING),
    );
    assert_eq!(through_enforce.counts(), through_decide.counts());
}

// ── A refusal names something the reader can move ─────────────────────────

/// **A share already at its stop is not named.**
///
/// The Samsung Z Fold 7, 2026-09-06: a loop refused for 998 MB while Settings
/// read `100 % asked for, 100 % in force` on both shares, and the notice told
/// the user to raise one of them. That instruction had nothing behind it —
/// `never-warn-about-the-unfixable`, and the defect is in the sentence, not
/// in its tone. So at 100 % the notice may not mention the share at all, and
/// must name a part of the scene the reader can spend instead.
#[test]
fn a_refusal_at_the_share_stop_names_no_slider() {
    let mut ledger = AdmissionLedger::default();
    let mut table = costs(0, 10 * MIB, 1);
    table.requested_percent = (100, 100);
    ledger.adopt(&table);

    assert!(!ledger.enforce(Act::ArmLoop, None, Increment::host(998 * 1000 * 1000)));
    let text = ledger
        .notice(web_time::Instant::now())
        .expect("a refusal must leave a notice")
        .text
        .clone();

    for dead in ["System memory", "GPU memory", "Settings", "Raise", "100 %"] {
        assert!(
            !text.contains(dead),
            "a share at its stop is not a control the reader has: {text}",
        );
    }
    assert!(
        text.contains("lookback"),
        "and the notice must name what the reader CAN spend: {text}",
    );
    assert!(text.contains("998 MB short"), "{text}");
}

/// **The control arm: a share still short of its stop is named, with the
/// number it sits at.** The reason the notice exists at all is that the two
/// shares are the user's own setting; suppressing them whenever they are
/// movable would be the opposite defect.
#[test]
fn a_refusal_below_the_stop_still_names_the_share() {
    let mut ledger = AdmissionLedger::default();
    let mut table = costs(0, 10 * MIB, 1);
    table.requested_percent = (100, 60);
    ledger.adopt(&table);

    assert!(!ledger.enforce(Act::ArmLoop, None, Increment::host(998 * 1000 * 1000)));
    let text = ledger
        .notice(web_time::Instant::now())
        .expect("a refusal must leave a notice")
        .text
        .clone();
    assert!(text.contains("System memory"), "{text}");
    assert!(text.contains("60 %"), "{text}");
    assert!(text.contains("Settings > Memory"), "{text}");
}

/// **One memory, one share left**: a joint refusal names only the share that
/// is still a lever. Naming a dead control beside a live one sends the reader
/// to the wrong slider half the time.
#[test]
fn a_joint_refusal_names_only_the_movable_share() {
    let mut only_gpu = AdmissionLedger::default();
    let mut table = costs(0, 10 * MIB, 1);
    table.requested_percent = (40, 100);
    table.spare.joint_bytes = Some(0);
    only_gpu.adopt(&table);
    assert!(!only_gpu.enforce(Act::ShowLayer, None, Increment::host(10 * MIB)));
    let text = only_gpu
        .notice(web_time::Instant::now())
        .expect("a notice")
        .text
        .clone();
    assert!(
        text.contains("GPU memory") && text.contains("40 %"),
        "{text}"
    );
    assert!(!text.contains("System memory"), "{text}");

    let mut neither = AdmissionLedger::default();
    let mut stopped = costs(0, 10 * MIB, 1);
    stopped.requested_percent = (100, 100);
    stopped.spare.joint_bytes = Some(0);
    neither.adopt(&stopped);
    assert!(!neither.enforce(Act::ShowLayer, None, Increment::host(10 * MIB)));
    let text = neither
        .notice(web_time::Instant::now())
        .expect("a notice")
        .text
        .clone();
    assert!(
        !text.contains("GPU memory") && !text.contains("System memory"),
        "one memory with both shares at their stop leaves no slider to name: \
         {text}",
    );
    assert!(text.contains("turn off"), "{text}");
}

// ── A refusal is answered once per table, and is re-asked on the next ──────

/// **Forty identical lines are not forty verdicts.**
///
/// `App::hydrate_parked_panes` runs on every `RedrawRequested`, and a loop
/// waiting on its transport comes straight back onto its queue — so the loop
/// door was re-driven by the redraw rate, not by anything about the scene. On
/// the Tier-2 `long` leg (Chromium, page instance, 2026-09-07) that logged the
/// same refusal about forty times in the first seven seconds, and on the
/// enforcing arm it would have re-stamped the notice often enough that it
/// could never age off the glass.
///
/// The second ask returns the same verdict and moves nothing.
#[test]
fn the_same_question_against_one_table_is_answered_once() {
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&costs(0, 10 * MIB, 2));
    let want = Increment::host(10 * MIB);

    let first = ledger.ask(Act::ArmLoop, Some(0), want);
    assert!(!first.is_admit());
    let after_one = ledger.counts();
    assert_eq!(after_one.asked, 1);
    assert_eq!(after_one.would_refuse, 1);

    for _ in 0..40 {
        assert_eq!(
            ledger.ask(Act::ArmLoop, Some(0), want),
            first,
            "a repeat must answer the same way - the spare only shrinks \
             inside a generation, so a refusal cannot become an admission",
        );
    }
    assert_eq!(
        ledger.counts(),
        after_one,
        "forty redraws are not forty verdicts",
    );
    assert!(ledger.already_refused(Act::ArmLoop, Some(0)));

    // **Keyed, not global.** Another pane and another act are different
    // questions and are still asked.
    assert!(!ledger.already_refused(Act::ArmLoop, Some(1)));
    assert!(!ledger.already_refused(Act::ShowLayer, Some(0)));
    let _ = ledger.ask(Act::ArmLoop, Some(1), want);
    assert_eq!(ledger.counts().asked, 2, "a second pane asks for itself");
}

/// **A refusal is not terminal: a fresher table asks again.**
///
/// The defect this closes is the one a peer called worse than anything four
/// performance lanes were fixing — a refused loop meant the user could not
/// lower their span and retry without restarting the app, and because the
/// wish is persisted it was *every* session.
///
/// So the scene here is the user's: refused, then they free room, then the
/// App publishes the next tick's table, and the same act is admitted.
#[test]
fn lowering_the_ask_after_a_refusal_gets_the_loop_on_the_next_generation() {
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&costs(64 * MIB, 10 * MIB, 1));

    let too_much = Increment::host(600 * MIB);
    assert!(!ledger.enforce(Act::ArmLoop, Some(0), too_much));
    assert!(ledger.already_refused(Act::ArmLoop, Some(0)));
    assert!(
        ledger.notice(web_time::Instant::now()).is_some(),
        "and the reader was told why",
    );

    // Still refused against the SAME table, however often it is re-driven:
    // nothing about the scene has changed yet.
    assert!(!ledger.enforce(Act::ArmLoop, Some(0), too_much));

    // The next telemetry tick, with the room the user freed.
    let mut fresher = costs(1024 * MIB, 10 * MIB, 1);
    fresher.generation = 2;
    ledger.adopt(&fresher);
    assert!(
        !ledger.already_refused(Act::ArmLoop, Some(0)),
        "a fresh table is a fresh answer",
    );
    assert!(
        ledger.enforce(Act::ArmLoop, Some(0), too_much),
        "the user freed the room and must get their loop",
    );

    // And a shorter span against the tight table would have been admitted
    // too - the refusal was about the size of the ask, not about the pane.
    let mut tight = AdmissionLedger::default();
    tight.adopt(&costs(64 * MIB, 10 * MIB, 1));
    assert!(!tight.enforce(Act::ArmLoop, Some(0), too_much));
    let mut next = costs(64 * MIB, 10 * MIB, 1);
    next.generation = 2;
    tight.adopt(&next);
    assert!(
        tight.enforce(Act::ArmLoop, Some(0), Increment::host(32 * MIB)),
        "a lookback the user shortened fits where the long one did not",
    );
}

/// **A loop nobody has listed has no price, so the door does not refuse it.**
///
/// The App prices `PaneAdmission::arm_loop` off the pane's prospective scene,
/// and with no cadence the model's frame count is the render budget's ceiling
/// rather than the span's own answer. Spending that figure is what refused a
/// clear-air loop at 1.56x its cost. `arm_loop` returns `None` there and the
/// verdict waits for the listing.
#[test]
fn a_pane_with_no_cadence_yet_has_no_arm_loop_price() {
    let mut table = costs(0, 10 * MIB, 1);
    table.panes[0].arm_loop = Increment::host(1040 * MIB);
    table.panes[0].cadence_secs = None;

    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&table);
    assert_eq!(
        ledger.arm_loop(0),
        None,
        "a door handed this figure refuses a loop nobody has priced",
    );

    // Control, one fact changed: a listing landed and said 230 s. The price
    // is now the App's own and the door may spend it.
    table.panes[0].cadence_secs = Some(230);
    table.generation = 2;
    ledger.adopt(&table);
    assert_eq!(ledger.arm_loop(0), Some(Increment::host(1040 * MIB)));
    assert!(
        !ledger.enforce(Act::ArmLoop, Some(0), ledger.arm_loop(0).unwrap()),
        "and with a real price against no spare it does refuse",
    );

    // A zero cadence converts no span and is not a cadence.
    table.panes[0].cadence_secs = Some(0);
    table.generation = 3;
    ledger.adopt(&table);
    assert_eq!(ledger.arm_loop(0), None);
}

// ── The listing door ──────────────────────────────────────────────────────

/// A table whose pane 0 may hold `allowed` loop frames, each reserved at
/// 80 MiB — the web bracket's own figure.
fn loop_costs(allowed: usize) -> AdmissionCosts {
    let mut table = costs(u64::MAX, 0, 1);
    table.panes[0].loop_frames_allowed = allowed;
    table.panes[0].loop_frame_reserve_bytes = 80 * MIB;
    table
}

/// **The listing door resolves one frame, at the spare that exactly buys the
/// loop.**
///
/// This is the assertion that earns every other one here. An admit with
/// frames to spare passes on a door biased by one, by two, or by a door that
/// never refuses at all; only the boundary says the instrument can see what
/// it claims to measure. So: the exact count admits, one more refuses, and
/// nothing in between is left unstated.
#[test]
fn the_listing_door_resolves_a_single_frame_at_the_boundary() {
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&loop_costs(9));

    assert!(
        ledger.admit_loop_frames(0, 9),
        "the count the session can hold must be admitted",
    );
    assert_eq!(
        ledger.counts().would_refuse,
        0,
        "an admitted loop leaves no refusal behind",
    );
    assert!(
        ledger.notice(web_time::Instant::now()).is_none(),
        "and nothing on the glass",
    );

    // One frame more, same table.
    let mut tight = AdmissionLedger::default();
    tight.adopt(&loop_costs(9));
    assert!(
        !tight.admit_loop_frames(0, 10),
        "one frame past what the session can hold must be refused, or this \
         door cannot see a one-frame difference and its admits mean nothing",
    );
    assert_eq!(tight.counts().refused, 1);
}

/// **A loop that fits overwhelmingly is admitted and costs nothing to ask.**
///
/// The other half of the pair: over-firing is the worse direction, so a door
/// that refuses a two-frame loop on a session with room for fourteen would be
/// worse than no door. Asserted across the whole range, not at one point.
#[test]
fn the_listing_door_admits_every_count_the_session_can_hold() {
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&loop_costs(14));
    for wanted in 0..=14 {
        assert!(
            ledger.admit_loop_frames(0, wanted),
            "{wanted} frames of a possible 14 must be admitted",
        );
    }
    assert_eq!(
        ledger.counts().would_refuse,
        0,
        "not one of those may have taken a refusing verdict",
    );
    assert!(ledger.notice(web_time::Instant::now()).is_none());
}

/// **A refused listing is seen, says what to do, and states itself in bytes.**
///
/// The door decides in frames because that is the honest unit at a listing,
/// but "three frames short" is not something a reader can act on. The
/// sentence names the shortfall in the units the memory settings are in, and
/// the lever for a loop is the lookback.
#[test]
fn a_refused_listing_names_the_shortfall_in_bytes_and_the_lever() {
    let mut ledger = AdmissionLedger::default();
    let mut table = loop_costs(3);
    // A share still short of its stop, so the notice names the slider rather
    // than the scene.
    table.requested_percent = (50, 50);
    ledger.adopt(&table);

    assert!(!ledger.admit_loop_frames(0, 14));
    let text = ledger
        .notice(web_time::Instant::now())
        .expect(
            "a refused loop the reader cannot see is worse than the \
                 allocation it prevented",
        )
        .text
        .clone();
    // 14 frames wanted, 3 allowed, 80 MiB apiece: 11 x 83_886_080 B, stated
    // in the decimal MB the notice uses.
    assert!(
        text.contains("923 MB"),
        "the shortfall must be the frames the loop could not have, priced at \
         the reserve the scene charges them: {text}",
    );
    assert!(
        text.contains("this loop") && text.contains("System memory"),
        "and it must name the act and the control: {text}",
    );
    // **The second action, and it is not decoration.** This door is not
    // re-asked for the user - re-driving it would put a fresh frame listing
    // on the network every redraw - so lowering the lookback alone does
    // nothing they can see. A notice stopping at the lever would leave them
    // having done exactly what they were told with no result.
    assert!(
        text.contains("Then turn the loop back on."),
        "a refusal nobody retries must say what to do after the lever, or \
         the instruction is a trap: {text}",
    );

    // **An ARM refusal must NOT say it**, because that one IS re-asked from
    // the memo on the next table. The day this door gains an automatic
    // re-drive, this pair is what says to drop the clause.
    let mut armed = AdmissionLedger::default();
    // Its own table: `loop_costs` leaves `u64::MAX` on both pools so that only
    // the COUNT door bites, and a byte door compared against that admits
    // everything.
    let mut tight = costs(8 * MIB, 0, 1);
    tight.requested_percent = (50, 50);
    armed.adopt(&tight);
    assert!(!armed.enforce(Act::ArmLoop, Some(0), Increment::host(600 * MIB)));
    let arm_text = armed
        .notice(web_time::Instant::now())
        .expect("a notice")
        .text
        .clone();
    assert!(
        !arm_text.contains("turn the loop back on"),
        "an arm refusal is retried for the user and must not tell them to do \
         it themselves: {arm_text}",
    );

    // The scene lever, where no share is left to move.
    let mut stopped = AdmissionLedger::default();
    let mut at_stop = loop_costs(3);
    at_stop.requested_percent = (100, 100);
    stopped.adopt(&at_stop);
    assert!(!stopped.admit_loop_frames(0, 14));
    let text = stopped
        .notice(web_time::Instant::now())
        .expect("a notice")
        .text
        .clone();
    assert!(
        text.contains("shorten the lookback"),
        "on a device with no more to give, the lever is the user's own \
         setting: {text}",
    );
}

/// **The listing door obeys the ledger's own rules**: exemptions, an unpriced
/// table, a pane the table has never seen, one answer per table, and a fresh
/// answer on the next one.
///
/// It decides in a different unit from every other door here, so each of
/// those rules is a place it could have drifted into its own policy.
#[test]
fn the_listing_door_shares_the_ledgers_rules() {
    // An application that has priced nothing refuses nothing.
    let mut unpriced = AdmissionLedger::default();
    assert!(unpriced.admit_loop_frames(0, 999));

    // Restore is never a refusal.
    let mut exempt = AdmissionLedger::default();
    exempt.adopt(&loop_costs(2));
    exempt.begin_exempt();
    assert!(exempt.admit_loop_frames(0, 999));
    exempt.end_exempt();
    assert!(exempt.notice(web_time::Instant::now()).is_none());
    assert!(
        !exempt.admit_loop_frames(0, 999),
        "control: outside, it refuses"
    );

    // A pane the table has not seen asks for nothing - the loop door is
    // reached by index from a queue drained a frame later.
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&loop_costs(2));
    assert!(ledger.admit_loop_frames(9, 999));

    // One answer per table, then a fresh one.
    let mut repeat = AdmissionLedger::default();
    repeat.adopt(&loop_costs(2));
    assert!(!repeat.admit_loop_frames(0, 14));
    let after_one = repeat.counts();
    for _ in 0..20 {
        assert!(!repeat.admit_loop_frames(0, 14));
    }
    assert_eq!(
        repeat.counts(),
        after_one,
        "20 listings are not 20 verdicts"
    );

    let mut roomier = loop_costs(14);
    roomier.generation = 2;
    repeat.adopt(&roomier);
    assert!(
        repeat.admit_loop_frames(0, 14),
        "a fresher table is a fresh answer",
    );
}

/// **Both arms, and only the enforcing one turns the listing away.**
///
/// `ENFORCING` is false on wasm32 and no test here runs a wasm build, so the
/// policy is a parameter and both arms are driven from native — the same
/// reason `decide` takes one.
#[test]
fn the_listing_door_is_advisory_on_the_arm_that_does_not_enforce() {
    for enforcing in [false, true] {
        let mut ledger = AdmissionLedger::default();
        ledger.adopt(&loop_costs(3));
        let proceeded = ledger.decide_loop_frames(0, 14, enforcing);
        assert_eq!(
            proceeded, !enforcing,
            "the arm decides whether the loop goes ahead: enforcing = \
             {enforcing}",
        );
        let counts = ledger.counts();
        assert_eq!(counts.would_refuse, 1, "the verdict is taken on both arms");
        assert_eq!(counts.refused, u32::from(enforcing));

        let text = ledger
            .notice(web_time::Instant::now())
            .expect("the reader is told on both arms")
            .text
            .clone();
        assert_eq!(
            text.contains("Not enough"),
            enforcing,
            "only the arm that turned the loop away may say it did: \
             {text}",
        );
        assert_eq!(
            text.contains("allowed anyway"),
            !enforcing,
            "and only the arm that let it through may say that: {text}",
        );
        // The advisory arm let the loop through, so it is playing: telling
        // the reader to turn it back on is the same false statement in the
        // other direction.
        assert_eq!(
            text.contains("turn the loop back on"),
            enforcing,
            "the second action belongs only where the loop was actually \
             turned away: {text}",
        );
    }
}
