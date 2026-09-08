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

// ── One debit, two ledgers ────────────────────────────────────────────────

/// **A tick's spare is spent once across both ledgers, not once each.**
///
/// A running application has two: the UI's, and the App's, for the doors that
/// run inside the App's own borrow of the Gui. Holding the same table is not
/// enough — with a debited total each, an act admitted through one leaves the
/// other still reading the *published* spare, and a burst that mixes UI acts
/// with loop arms inside one telemetry tick is admitted against one tick's
/// spare twice. That is an over-admission: the direction that costs the user
/// their process rather than a rung.
#[test]
fn two_ledgers_on_one_table_spend_one_debit() {
    let table = costs(30 * MIB, 10 * MIB, 1);
    let mut ui = AdmissionLedger::default();
    let mut app = AdmissionLedger::default();
    app.adopt(&table);
    ui.adopt(&table);
    ui.share_debit(app.debit());

    // The App's door goes first, for 20 of the 30 MiB.
    assert!(
        app.ask(Act::ArmLoop, Some(0), Increment::host(20 * MIB))
            .is_admit(),
    );
    assert_eq!(
        ui.spare().host_bytes,
        Some(10 * MIB),
        "the UI's ledger must see the App's admission; a spare it reads whole \
         after the App has spent it is one tick's spare counted twice",
    );

    // And the same act the other way: what the UI spends, the App sees.
    assert!(
        ui.ask(Act::ShowLayer, Some(0), Increment::host(10 * MIB))
            .is_admit(),
    );
    assert_eq!(app.spare().host_bytes, Some(0));
    assert!(
        !app.ask(Act::ArmLoop, Some(0), Increment::host(1))
            .is_admit(),
        "with the tick's whole spare spent, the next door on either ledger \
         must be refused",
    );
}

/// **Both ledgers adopting one table zeroes the total once.**
///
/// Each would otherwise clear what has been spent on taking a fresh table,
/// and the second to arrive would wipe the first's admissions against it —
/// the same over-admission the shared total exists to close, arriving through
/// the handshake instead. The reset is keyed on the generation the cell
/// itself last saw.
#[test]
fn the_second_ledger_to_adopt_a_table_does_not_wipe_the_first() {
    let first = costs(30 * MIB, 10 * MIB, 1);
    let mut ui = AdmissionLedger::default();
    let mut app = AdmissionLedger::default();
    app.adopt(&first);
    ui.adopt(&first);
    ui.share_debit(app.debit());

    let second = AdmissionCosts {
        generation: 2,
        ..costs(30 * MIB, 10 * MIB, 1)
    };
    // The App composes and adopts on the tick; the UI's ledger takes the same
    // table on the frame that follows, and the App spends in between.
    app.adopt(&second);
    assert!(
        app.ask(Act::ArmLoop, Some(0), Increment::host(25 * MIB))
            .is_admit(),
    );
    ui.adopt(&second);

    assert_eq!(
        ui.spare().host_bytes,
        Some(5 * MIB),
        "the UI's adopt must not re-zero a total the App has already spent \
         against this very table",
    );
}

/// **A ledger joining a total carries what it has already spent into it.** The
/// handshake is re-stated every frame and must be free after the first, but
/// the first must not forget an admission either side made before it.
#[test]
fn joining_a_shared_total_carries_what_was_already_spent() {
    let table = costs(30 * MIB, 10 * MIB, 1);
    let mut ui = AdmissionLedger::default();
    let mut app = AdmissionLedger::default();
    app.adopt(&table);
    ui.adopt(&table);
    // Both spend before they have ever met.
    assert!(
        app.ask(Act::ArmLoop, Some(0), Increment::host(10 * MIB))
            .is_admit(),
    );
    assert!(
        ui.ask(Act::ShowLayer, Some(0), Increment::host(10 * MIB))
            .is_admit(),
    );

    ui.share_debit(app.debit());
    assert_eq!(
        ui.spare().host_bytes,
        Some(10 * MIB),
        "joining must add this ledger's spend to the total rather than \
         replace it",
    );

    // Re-stating it every frame is a no-op, not a second carry-over.
    for _ in 0..5 {
        ui.share_debit(app.debit());
    }
    assert_eq!(ui.spare().host_bytes, Some(10 * MIB));
}

/// **A cloned ledger is a second application, not a second holder of one
/// debit.** Sharing the handle through a clone would be this mechanism
/// running backwards: two unrelated applications spending each other's spare.
#[test]
fn a_cloned_ledger_gets_its_own_total_at_the_same_reading() {
    let table = costs(30 * MIB, 10 * MIB, 1);
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&table);
    assert!(
        ledger
            .ask(Act::ShowLayer, Some(0), Increment::host(10 * MIB))
            .is_admit(),
    );

    let mut copy = ledger.clone();
    assert_eq!(
        copy.spare().host_bytes,
        Some(20 * MIB),
        "the clone starts from what the original had spent",
    );
    assert!(
        copy.ask(Act::ShowLayer, Some(0), Increment::host(20 * MIB))
            .is_admit(),
    );
    assert_eq!(
        ledger.spare().host_bytes,
        Some(20 * MIB),
        "and spending on the clone must not move the original",
    );
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

/// **An admitted listing is a verdict, and it moves `asked`.**
///
/// The defect this pins: `decide_loop_frames` returned early on every count
/// the session could hold without touching `record`, so the only figures a
/// browser rig can read — the process totals on `budget state:` — moved for a
/// refused loop and stayed at zero for every loop that fitted. A counter that
/// reads zero on a healthy session cannot tell "no door ran" from "every door
/// admitted", which is the difference the rig is there to see.
#[test]
fn an_admitted_listing_is_counted_like_a_refused_one() {
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&loop_costs(9));

    assert!(ledger.admit_loop_frames(0, 9));
    let counts = ledger.counts();
    assert_eq!(
        (counts.asked, counts.admitted),
        (1, 1),
        "a loop that fits took a verdict and it must be countable",
    );
    assert_eq!((counts.would_refuse, counts.refused), (0, 0));

    // Control, one fact changed: the same door on a count that does not fit
    // moves the other pair, and `asked` moves for both.
    let mut tight = AdmissionLedger::default();
    tight.adopt(&loop_costs(9));
    assert!(!tight.admit_loop_frames(0, 10));
    let counts = tight.counts();
    assert_eq!((counts.asked, counts.admitted), (1, 0));
    assert_eq!((counts.would_refuse, counts.refused), (1, 1));

    // A pane the table has never seen is still not counted: there was no
    // figure to compare against, so no verdict was taken.
    let mut absent = AdmissionLedger::default();
    absent.adopt(&loop_costs(9));
    assert!(absent.admit_loop_frames(9, 999));
    assert_eq!(absent.counts(), Totals::default());
}

/// **Every reach of the listing door lands in exactly one exit, and the exits
/// account for every reach.**
///
/// The invariant [`LoopDoorTotals::exits_balance`] states, driven rather than
/// asserted in prose. Its value is in the future: an exit added to
/// `decide_loop_frames` without a counter makes this red instead of quietly
/// under-counting a denominator a rig is reading.
///
/// Driven on **one ledger's own copy** and not on the process statics, which
/// every other test in this binary is moving at the same time.
#[test]
fn the_listing_doors_exits_partition_its_reaches() {
    let mut ledger = AdmissionLedger::default();
    assert_eq!(ledger.door_exits(), LoopDoorTotals::default());

    // Unpriced: no table has been published.
    assert!(ledger.admit_loop_frames(0, 999));
    // Exempt: a restore holds a batch open over the door.
    ledger.adopt(&loop_costs(9));
    ledger.begin_exempt();
    assert!(ledger.admit_loop_frames(0, 999));
    ledger.end_exempt();
    // No row: a pane the table has never seen.
    assert!(ledger.admit_loop_frames(9, 999));
    // Fit, then over.
    assert!(ledger.admit_loop_frames(0, 9));
    assert!(!ledger.admit_loop_frames(0, 10));

    let exits = ledger.door_exits();
    assert_eq!(
        (exits.reached, exits.unpriced, exits.batch, exits.no_row),
        (5, 1, 1, 1),
    );
    assert_eq!((exits.fit, exits.over), (1, 1));
    assert!(
        exits.exits_balance(),
        "an exit without a counter is an under-counted denominator: {exits:?}",
    );
    assert_eq!(
        (exits.worst_wanted, exits.worst_allowed),
        (10, 9),
        "the widest gap is ONE reach's pair - a count from one loop beside an \
         allowance from another is a fabricated finding",
    );
    // The verdict counters see only the two exits that had a table row to
    // answer from, which is the gap this family exists to make readable.
    let counts = ledger.counts();
    assert_eq!(
        (counts.asked, counts.admitted, counts.would_refuse),
        (2, 1, 1)
    );
}

// ── The overlay listing door ──────────────────────────────────────────────

/// **The non-radar half of the fork takes a verdict at all**, and it is the
/// share path's own arithmetic that decides it.
///
/// Before this door, `App::accept_loop_scan_listings`'s "every layer but
/// radar" arm built a frame list and dispatched it with nothing in this
/// ledger to say it had happened — on every target, native included. The two
/// figures are the caller's: what one of this layer's frames costs on this
/// pane, and what the pane's slice of the loop pool gave one animating layer.
#[test]
fn the_overlay_listing_door_counts_the_share_it_was_handed() {
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&costs(u64::MAX, 0, 1));

    // Six frames of 11.06 MB inside a 80 MiB share.
    ledger.advise_overlay_loop_frames(0, 6, FramePrice::Measured(11_059_200), 80 * MIB);
    let counts = ledger.counts();
    assert_eq!(
        (counts.asked, counts.admitted),
        (1, 1),
        "a list inside its share is an admitted verdict, not a silence",
    );
    assert_eq!(counts.would_refuse, 0);
    assert!(ledger.notice(web_time::Instant::now()).is_none());

    // One frame past it, same table: 8 x 11.06 MB is 88.5 MB against 83.9 MB.
    let mut over = AdmissionLedger::default();
    over.adopt(&costs(u64::MAX, 0, 1));
    over.advise_overlay_loop_frames(0, 8, FramePrice::Measured(11_059_200), 80 * MIB);
    let counts = over.counts();
    assert_eq!(
        (counts.asked, counts.would_refuse),
        (1, 1),
        "and a list past it is a verdict this door can see",
    );
    assert_eq!(
        counts.refused, 0,
        "but nothing was turned away, on either arm",
    );
}

/// **This door never turns a loop away, and the sentence it raises says so.**
///
/// The list it is handed has already been held to the same `afforded_bytes`
/// by the division that built it (`app_render::layer_share`), and the one way
/// it comes back over is that function's own `MIN_LOOP_FRAMES_PER_PANE`
/// floor — which it documents itself as exceeding the byte bound to honour.
/// Refusing there would overturn that constant's decision through a side
/// door, so what this adds is the count, the log line and the warning rather
/// than a refusal. The enforcing arm is driven all the same, so the day it is
/// wanted it is not being written for the first time.
#[test]
fn the_overlay_listing_door_is_advisory_on_both_arms() {
    for enforcing in [false, true] {
        let mut ledger = AdmissionLedger::default();
        let mut table = costs(u64::MAX, 0, 1);
        table.requested_percent = (50, 50);
        ledger.adopt(&table);

        let proceeded = ledger.decide_overlay_loop_frames(
            0,
            8,
            FramePrice::Measured(11_059_200),
            80 * MIB,
            enforcing,
        );
        assert_eq!(
            proceeded, !enforcing,
            "only the arm that enforces may stop the loop: enforcing = {enforcing}",
        );
        let counts = ledger.counts();
        assert_eq!(counts.would_refuse, 1, "the verdict is taken on both arms");
        assert_eq!(counts.refused, u32::from(enforcing));

        let text = ledger
            .notice(web_time::Instant::now())
            .expect("the reader is told on both arms")
            .text
            .clone();
        assert!(
            text.contains("GPU memory"),
            "an overlay loop frame is a texture, and the share the reader can \
             move is the GPU one: {text}",
        );
        assert_eq!(
            text.contains("allowed anyway"),
            !enforcing,
            "the advisory arm must not claim a refusal that did not happen: \
             {text}",
        );
    }

    // The door the application actually calls takes the advisory arm, so a
    // loop past its share plays and is counted rather than being stopped.
    let mut live = AdmissionLedger::default();
    live.adopt(&costs(u64::MAX, 0, 1));
    live.advise_overlay_loop_frames(0, 8, FramePrice::Measured(11_059_200), 80 * MIB);
    assert_eq!(live.counts().refused, 0);
    assert!(
        live.pending().is_empty(),
        "nothing turned away retains nothing"
    );
}

/// **The overlay door keeps the same partition over its own denominator.**
///
/// Four exits rather than five: it reads no table row, so `no_row` is
/// structurally absent here rather than merely unobserved. The two doors'
/// reaches are counted apart and never summed — one is per radar listing and
/// the other per non-radar one.
#[test]
fn the_overlay_doors_exits_partition_its_own_reaches() {
    let mut ledger = AdmissionLedger::default();

    ledger.advise_overlay_loop_frames(0, 4, FramePrice::Measured(11_059_200), 80 * MIB); // unpriced
    ledger.adopt(&costs(u64::MAX, 0, 1));
    ledger.begin_exempt();
    ledger.advise_overlay_loop_frames(0, 4, FramePrice::Measured(11_059_200), 80 * MIB); // batch
    ledger.end_exempt();
    ledger.advise_overlay_loop_frames(0, 4, FramePrice::Measured(11_059_200), 80 * MIB); // fit
    ledger.advise_overlay_loop_frames(0, 9, FramePrice::Nominal(11_059_200), 80 * MIB); // over

    let exits = ledger.overlay_door_exits();
    assert_eq!(
        (
            exits.reached,
            exits.unpriced,
            exits.batch,
            exits.fit,
            exits.over
        ),
        (4, 1, 1, 1, 1),
    );
    assert_eq!(exits.no_row, 0, "this door reads no row and never can");
    assert!(exits.exits_balance(), "{exits:?}");
    // **The second partition of the same denominator**: which arm priced
    // each reach. Counted before the exits, so the free ones carry it too -
    // three measured above and one nominal.
    assert_eq!((exits.priced_measured, exits.priced_nominal), (3, 1));
    assert!(
        exits.price_arms_balance(),
        "a verdict priced off a measured raster and one priced off the class          nominal are different confidences and must not be one number:          {exits:?}",
    );
    assert_eq!(
        (exits.worst_wanted, exits.worst_allowed),
        (9, 7),
        "the pair is in frames on both doors, so one line reads in one unit: \
         83,886,080 B of share buys 7 frames of 11,059,200 B",
    );
    assert_eq!(
        ledger.door_exits(),
        LoopDoorTotals::default(),
        "and not one of those reaches was counted against radar's door",
    );
    assert!(
        !ledger.door_exits().price_arms_balance() || ledger.door_exits().reached == 0,
        "radar prices from one arm and names none, so this invariant is the          overlay door's alone",
    );
}

/// **A pane looping radar beside a model field asks two questions, and one
/// answer may not stand in for the other.**
///
/// The memo in [`AdmissionLedger::refused`] is keyed by `(act, pane)`. With
/// one variant for both halves of the fork, a radar loop refused on pane 0
/// would be replayed as the answer to that pane's overlay listing — a loop
/// priced at a decoded volume's reserve refusing one whose frames are
/// rasters.
#[test]
fn the_two_listing_doors_do_not_answer_for_each_other() {
    let mut ledger = AdmissionLedger::default();
    let mut table = loop_costs(3);
    table.requested_percent = (50, 50);
    ledger.adopt(&table);

    assert!(!ledger.admit_loop_frames(0, 14), "radar's loop is refused");
    // The same pane's overlay listing, comfortably inside its own share.
    ledger.advise_overlay_loop_frames(0, 4, FramePrice::Measured(11_059_200), 80 * MIB);
    let counts = ledger.counts();
    assert_eq!(
        (counts.asked, counts.admitted),
        (2, 1),
        "the overlay question was asked and answered on its own figures",
    );
    assert_eq!(
        counts.would_refuse, 1,
        "radar's refusal must not be replayed as the overlay's",
    );
}

// ── The wish a refusal leaves behind ──────────────────────────────────────
//
// Every fixture here is a pair: the refusal that retains a wish, and beside
// it the same act one fact apart that does not. The direction that matters is
// over-firing — a wish re-offered or re-driven when the room never came back
// is a scene change with nothing behind it — so the "still refused" arm is the
// one each of these leads with.

/// The two tables one recovery scene needs: the tight one that refuses, and
/// the fresher one the App publishes after the user frees room. Generation 2
/// on the second, because a table that does not move is not adopted at all.
fn roomier(spare_bytes: u64, per_layer: u64, panes: usize) -> AdmissionCosts {
    let mut table = costs(spare_bytes, per_layer, panes);
    table.generation = 2;
    table
}

/// **The sentence on the glass, for the arms that assert a tick announced
/// nothing.**
///
/// "Nothing was announced" is *not* "there is no notice": the refusal that
/// opened each of these scenes is still up at these readings — it lives
/// [`NOTICE_LIFETIME`], and the ticks below land well inside that. So the
/// assertion is that the sentence did not **change**, which is the property,
/// and asserting `None` instead would be a check that could only pass by
/// accident of timing.
fn notice_text(ledger: &AdmissionLedger, now: web_time::Instant) -> Option<String> {
    ledger.notice(now).map(|notice| notice.text.clone())
}

/// **A refused act is retained past the generation that refused it**, and the
/// reader is told when it starts fitting.
///
/// The defect: a refusal wrote nothing but a six-second notice, so a user who
/// did exactly what the notice told them — closed a pane, raised a share —
/// got no sign that it had worked and no way to know their layer was
/// affordable now. The wish is what carries the question across the tick.
#[test]
fn a_refused_layer_is_retained_and_re_offered_when_the_room_comes_back() {
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&costs(0, 10 * MIB, 1));
    let want = Increment::host(10 * MIB);

    assert!(!ledger.decide(Act::ShowLayer, Some(0), want, true));
    assert_eq!(
        ledger.pending().len(),
        1,
        "the user still wants the layer they clicked",
    );
    assert_eq!(ledger.pending()[0].act, Act::ShowLayer);
    assert_eq!(ledger.pending()[0].pane, Some(0));

    // The tick that republishes the spare, with the room the user freed.
    let at = web_time::Instant::now();
    ledger.adopt_at(&roomier(64 * MIB, 10 * MIB, 1), at);
    assert!(
        ledger.pending().is_empty(),
        "a wish that fits is resolved, not carried forever",
    );
    assert_eq!(
        ledger.notice(at).map(|notice| notice.text.as_str()),
        Some(reoffer_text(Act::ShowLayer)),
        "and the reader is told the gesture will work now",
    );
    assert!(
        ledger.take_granted().is_empty(),
        "a re-offered act is the user's to make; nothing may perform it",
    );

    // **The healthy input that resembles it**: the same act, the same door,
    // the same table — admitted the first time. Nothing is retained and
    // nothing is announced, because nothing was ever refused.
    let mut roomy = AdmissionLedger::default();
    roomy.adopt(&costs(64 * MIB, 10 * MIB, 1));
    assert!(roomy.decide(Act::ShowLayer, Some(0), want, true));
    assert!(roomy.pending().is_empty());
    let at = web_time::Instant::now();
    roomy.adopt_at(&roomier(64 * MIB, 10 * MIB, 1), at);
    assert!(
        roomy.notice(at).is_none(),
        "a table with room must not announce a wish nobody made",
    );
}

/// **A wish that still does not fit stays a wish.** The over-firing arm of the
/// test above: the tick came, the room did not, and announcing it anyway would
/// send the reader back to a wall.
#[test]
fn a_wish_the_fresher_table_still_cannot_hold_stays_pending() {
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&costs(0, 10 * MIB, 1));
    assert!(!ledger.decide(Act::ShowLayer, Some(0), Increment::host(10 * MIB), true));

    let at = web_time::Instant::now();
    let refusal = notice_text(&ledger, at);
    assert!(
        refusal.as_deref().is_some_and(|t| t.contains("Not enough")),
        "the fixture must start with the refusal on the glass: {refusal:?}",
    );
    // A fresher table, and the shortage is exactly as it was.
    ledger.adopt_at(&roomier(0, 10 * MIB, 1), at);
    assert_eq!(
        ledger.pending().len(),
        1,
        "the wish outlives a table that could not hold it",
    );
    assert_eq!(
        notice_text(&ledger, at),
        refusal,
        "and nothing new is announced, because nothing changed",
    );

    // One more tick, and this one has the room: the same wish resolves.
    let mut third = roomier(64 * MIB, 10 * MIB, 1);
    third.generation = 3;
    ledger.adopt_at(&third, at);
    assert!(ledger.pending().is_empty());
    assert_eq!(
        ledger.notice(at).map(|notice| notice.text.as_str()),
        Some(reoffer_text(Act::ShowLayer)),
    );
}

/// **The invariant acts are re-driven; the gestures are re-offered.** The
/// per-act split is the whole product answer, and this is the pair that shows
/// the two halves are actually different code paths and not one policy with
/// two names.
#[test]
fn a_pane_s_default_layers_are_re_driven_where_a_click_is_only_re_offered() {
    let want = Increment::host(10 * MIB);

    // The invariant: nothing was clicked to ask for a pane's default layers
    // and nothing will be clicked to ask again, so the application puts them
    // back itself.
    let mut invariant = AdmissionLedger::default();
    invariant.adopt(&costs(0, 10 * MIB, 1));
    assert!(!invariant.decide(Act::DefaultLayers, None, want, true));
    let at = web_time::Instant::now();
    let refusal = notice_text(&invariant, at);
    assert!(
        refusal.as_deref().is_some_and(|t| t.contains("Not enough")),
        "the fixture must start with the refusal on the glass: {refusal:?}",
    );
    invariant.adopt_at(&roomier(64 * MIB, 10 * MIB, 1), at);
    let granted = invariant.take_granted();
    assert_eq!(
        granted.iter().map(|wish| wish.act).collect::<Vec<_>>(),
        [Act::DefaultLayers],
        "the seeding walk is the application's to re-run",
    );
    assert!(invariant.pending().is_empty());
    assert_eq!(
        notice_text(&invariant, at),
        refusal,
        "and it announces nothing: there is no gesture to ask the reader for",
    );

    // **One fact changed — the act.** A click on one pane's eye is the user's
    // curation, and the door may not make it for them.
    let mut gesture = AdmissionLedger::default();
    gesture.adopt(&costs(0, 10 * MIB, 1));
    assert!(!gesture.decide(Act::ShowLayer, Some(0), want, true));
    let at = web_time::Instant::now();
    gesture.adopt_at(&roomier(64 * MIB, 10 * MIB, 1), at);
    assert!(
        gesture.take_granted().is_empty(),
        "nothing may paint a layer the user has not asked for twice",
    );
    assert!(gesture.notice(at).is_some(), "they are told instead");
}

/// **The acts a standing loop already re-asks retain nothing.**
///
/// `App::hydrate_parked_panes` re-drives a refused loop arm off its own parked
/// queue on every redraw, and `Gui::propagate_pane_sync` re-drives the
/// layer-link fan-out at the end of every shell frame. A wish for either would
/// be a second copy of one that already exists, and the copy is the thing that
/// drifts — so the ledger holds none.
#[test]
fn the_self_driven_acts_are_retained_by_their_own_loops_and_not_here() {
    for act in [Act::ArmLoop, Act::AdoptLayers] {
        let mut ledger = AdmissionLedger::default();
        ledger.adopt(&costs(0, 10 * MIB, 1));
        assert!(!ledger.decide(act, Some(0), Increment::host(10 * MIB), true));
        assert_eq!(
            ledger.counts().refused,
            1,
            "{act:?} was still refused and still counted",
        );
        assert!(
            ledger.pending().is_empty(),
            "{act:?} is re-asked by its own loop; a wish here would be a \
             second copy of it",
        );
    }

    // The control, one fact changed: an act with no loop behind it is
    // retained by the same door on the same table.
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&costs(0, 10 * MIB, 1));
    assert!(!ledger.decide(Act::ShowLayer, Some(0), Increment::host(10 * MIB), true));
    assert_eq!(ledger.pending().len(), 1);
}

/// **A wish the user made a minute ago is not a wish now.**
///
/// Wall clock, not frames: what is being bounded is how long ago the person
/// asked, and a frame count measures how busy the machine has been instead.
#[test]
fn a_wish_ages_out_and_is_not_offered() {
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&costs(0, 10 * MIB, 1));
    let asked_at = web_time::Instant::now();
    assert!(!ledger.decide(Act::ShowLayer, Some(0), Increment::host(10 * MIB), true));
    assert_eq!(ledger.pending().len(), 1);

    // The room arrives, too late to be what they wanted. Twice the life
    // rather than exactly it: the verdict's own clock reading is taken a few
    // nanoseconds after `asked_at`, so an `at + WISH_LIFETIME` reading is
    // fractionally INSIDE the window and would test the other arm.
    let late = asked_at + WISH_LIFETIME * 2;
    ledger.adopt_at(&roomier(64 * MIB, 10 * MIB, 1), late);
    assert!(
        ledger.pending().is_empty(),
        "an expired wish is dropped, not carried",
    );
    assert_eq!(
        ledger.notice(late),
        None,
        "and a layer nobody wants any more is not offered back",
    );

    // **The same scene one fact apart**: the room arrives while the gesture
    // is still theirs.
    let mut in_time = AdmissionLedger::default();
    in_time.adopt(&costs(0, 10 * MIB, 1));
    let asked_at = web_time::Instant::now();
    assert!(!in_time.decide(Act::ShowLayer, Some(0), Increment::host(10 * MIB), true));
    let soon = asked_at + WISH_LIFETIME / 2;
    in_time.adopt_at(&roomier(64 * MIB, 10 * MIB, 1), soon);
    assert_eq!(
        in_time.notice(soon).map(|notice| notice.text.as_str()),
        Some(reoffer_text(Act::ShowLayer)),
    );
}

/// **A repeat inside one generation is not a new wish**, and does not restart
/// the clock.
///
/// `App::hydrate_parked_panes` re-drives its door on every redraw, and the
/// within-generation memo is what stops that being forty verdicts. If a
/// re-drive also refreshed `wished_at`, a door driven at frame rate would hold
/// a wish forever and [`WISH_LIFETIME`] would bound nothing at all.
#[test]
fn re_driving_a_refused_door_neither_adds_a_wish_nor_moves_its_clock() {
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&costs(0, 10 * MIB, 1));
    let want = Increment::host(10 * MIB);
    assert!(!ledger.decide(Act::ShowLayer, Some(0), want, true));
    let first = ledger.pending()[0].wished_at;

    for _ in 0..40 {
        assert!(!ledger.decide(Act::ShowLayer, Some(0), want, true));
    }
    assert_eq!(
        ledger.pending().len(),
        1,
        "forty redraws are not forty wishes"
    );
    assert_eq!(
        ledger.pending()[0].wished_at,
        first,
        "and not one of them may push the expiry out",
    );
    assert_eq!(ledger.counts().would_refuse, 1, "nor take a second verdict");
}

/// **A wish whose pane is gone is not a wish.** Closing the pane is one of the
/// ways the room comes back, and offering the reader a layer for a pane that
/// is no longer on the glass names something they cannot look at.
#[test]
fn a_wish_dies_with_the_pane_it_named() {
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&costs(0, 10 * MIB, 2));
    assert!(!ledger.decide(Act::ShowLayer, Some(1), Increment::host(10 * MIB), true));
    assert_eq!(ledger.pending().len(), 1);

    // The user closed pane 1, which is what made the room.
    let at = web_time::Instant::now();
    let refusal = notice_text(&ledger, at);
    ledger.adopt_at(&roomier(64 * MIB, 10 * MIB, 1), at);
    assert!(ledger.pending().is_empty());
    assert_eq!(
        notice_text(&ledger, at),
        refusal,
        "a pane that is gone is offered nothing",
    );

    // **One fact changed**: the same room, and the pane still there.
    let mut kept = AdmissionLedger::default();
    kept.adopt(&costs(0, 10 * MIB, 2));
    assert!(!kept.decide(Act::ShowLayer, Some(1), Increment::host(10 * MIB), true));
    let at = web_time::Instant::now();
    kept.adopt_at(&roomier(64 * MIB, 10 * MIB, 2), at);
    assert_eq!(
        kept.notice(at).map(|notice| notice.text.as_str()),
        Some(reoffer_text(Act::ShowLayer)),
    );
}

/// **The retained set is bounded by the questions a scene can ask**, not by
/// how often it asks them. An unbounded pending set is a leak, and this
/// campaign has spent itself on that class.
#[test]
fn the_wish_set_holds_one_entry_per_question_however_often_it_is_asked() {
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&costs(0, 10 * MIB, 4));
    let want = Increment::host(10 * MIB);

    // Twenty **distinct** questions to the memo — the payload differs, so each
    // takes its own verdict — and one wish, because a user who asked for
    // twenty splits wants the last one.
    for added in 1..=20 {
        assert!(!ledger.decide(Act::Panes { added }, None, want, true));
    }
    assert_eq!(
        ledger.counts().would_refuse,
        20,
        "each payload is its own question to the memo",
    );
    assert_eq!(
        ledger.pending().len(),
        1,
        "and one wish, the last one asked",
    );
    assert_eq!(ledger.pending()[0].act, Act::Panes { added: 20 });

    // A different pane is a different question and keeps its own entry.
    for pane in 0..4 {
        assert!(!ledger.decide(Act::ShowLayer, Some(pane), want, true));
    }
    assert_eq!(ledger.pending().len(), 5);
    assert!(
        ledger.pending().len() <= PENDING_CAP,
        "the ceiling exists to be unreachable, not to be approached",
    );
}

/// **Every act kind is its own retention key**, which is what makes the
/// ceiling above arithmetic rather than a guess.
#[test]
fn every_act_kind_has_its_own_retention_key() {
    let mut keys: Vec<u8> = ACT_KINDS.iter().map(|act| act.retention_key()).collect();
    let total = keys.len();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(
        keys.len(),
        total,
        "two acts sharing a key would silently replace each other's wishes",
    );

    // The ceiling, re-derived here against the layout's own maximum: every
    // retaining kind against every addressable pane plus the `None` key.
    let retaining = ACT_KINDS
        .iter()
        .filter(|act| !matches!(act.recovery(), Recovery::SelfDriven))
        .count();
    let reachable = retaining * (squallar_device_profile::budget::MAX_PANES_DESKTOP + 1);
    let cap = PENDING_CAP;
    assert!(
        reachable <= cap,
        "the backstop must sit above what the key space can actually reach: \
         {reachable} > {cap}",
    );
}

/// **Exactly one act is re-driven without asking**, and the walk that re-drives
/// it (`Gui::replay_granted_admissions`) has an arm for exactly that one. A
/// second `Silent` act added without an arm there would do nothing at all —
/// a silent partial success — so the roster is pinned here instead of being
/// left to the match's empty arms.
#[test]
fn default_layers_is_the_only_silently_replayed_act() {
    let silent: Vec<Act> = ACT_KINDS
        .iter()
        .copied()
        .filter(|act| act.recovery() == Recovery::Silent)
        .collect();
    assert_eq!(
        silent,
        [Act::DefaultLayers],
        "add an arm to Gui::replay_granted_admissions before adding a \
         Recovery::Silent act",
    );
}

/// **The advisory arm retains nothing**, because nothing was turned away.
/// Retaining a wish for an act that already happened would re-drive it, which
/// is a worse defect than the refusal that did not occur.
#[test]
fn the_advisory_arm_counts_a_refusal_and_keeps_no_wish() {
    let mut advisory = AdmissionLedger::default();
    advisory.adopt(&costs(0, 10 * MIB, 1));
    assert!(advisory.decide(Act::ShowLayer, Some(0), Increment::host(10 * MIB), false));
    assert_eq!(
        advisory.counts().would_refuse,
        1,
        "the measurement runs on every arm",
    );
    assert!(
        advisory.pending().is_empty(),
        "and the act it let through leaves nothing to re-ask",
    );

    // One fact changed: the arm.
    let mut enforcing = AdmissionLedger::default();
    enforcing.adopt(&costs(0, 10 * MIB, 1));
    assert!(!enforcing.decide(Act::ShowLayer, Some(0), Increment::host(10 * MIB), true));
    assert_eq!(enforcing.pending().len(), 1);
}

/// **Resolving a wish commits nothing.** The re-ask is a filter, not a grant:
/// a re-offered act has not happened and a re-driven one goes back through its
/// real door, which is what debits. Spending here would hold bytes against a
/// scene nobody has asked for yet.
#[test]
fn resolving_a_wish_spends_none_of_the_spare() {
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&costs(0, 10 * MIB, 1));
    assert!(!ledger.decide(Act::ShowLayer, Some(0), Increment::host(10 * MIB), true));

    let fresher = roomier(64 * MIB, 10 * MIB, 1);
    ledger.adopt_at(&fresher, web_time::Instant::now());
    assert_eq!(
        ledger.spare(),
        fresher.spare,
        "the published spare, untouched: no act has happened",
    );
}

/// **The listing door's wish is held in frames**, because that is the unit its
/// question is asked in. Re-asking a frame count as a byte increment against
/// the spare is the mis-comparison `AdmissionLedger::admit_loop_frames`
/// exists to avoid, and it would refuse every count there is.
#[test]
fn a_refused_listing_is_retained_in_frames_and_re_offered_when_the_loop_fits() {
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&loop_costs(2));
    assert!(
        !ledger.decide_loop_frames(0, 8, true),
        "eight frames against a two-frame allowance",
    );
    assert_eq!(ledger.pending().len(), 1);
    assert_eq!(ledger.pending()[0].act, Act::LoopFrames);

    // A fresher table whose allowance still cannot hold the listing.
    let at = web_time::Instant::now();
    let refusal = notice_text(&ledger, at);
    assert!(
        refusal.as_deref().is_some_and(|t| t.contains("Not enough")),
        "the fixture must start with the refusal on the glass: {refusal:?}",
    );
    let mut tight = loop_costs(4);
    tight.generation = 2;
    ledger.adopt_at(&tight, at);
    assert_eq!(
        ledger.pending().len(),
        1,
        "four frames is more room and still not enough",
    );
    assert_eq!(notice_text(&ledger, at), refusal);

    // **One fact changed**: the allowance now covers the listing.
    let mut roomy = loop_costs(8);
    roomy.generation = 3;
    ledger.adopt_at(&roomy, at);
    assert!(ledger.pending().is_empty());
    assert_eq!(
        ledger.notice(at).map(|notice| notice.text.as_str()),
        Some(reoffer_text(Act::LoopFrames)),
        "and the reader is told to turn the loop back on, which is the \
         gesture the refusal's own follow-up named",
    );
}

// ── What reached the glass ────────────────────────────────────────────────

/// **A notice raised onto one that is still showing is a re-stamp; one raised
/// after the last aged out is just the next notice.**
///
/// The two are indistinguishable in a total and opposite in what they mean: a
/// re-stamp restarts the six seconds under the reader's eyes, which is what a
/// user described as a notice appearing and going away. Nothing in this tree
/// could confirm or refute that report before this counter — **no per-act
/// refusal is logged anywhere**, and `raise_notice` draws without logging, so
/// the hypothesis was unfalsifiable on every log the application emits.
///
/// Three arms, because two of them would let a counter that simply counts
/// calls pass: the fresh one, the re-stamp, and the one that is neither.
#[test]
fn a_notice_raised_over_a_live_one_is_counted_apart_from_one_raised_after_it_aged() {
    let mut ledger = AdmissionLedger::default();
    let at = web_time::Instant::now();

    ledger.raise_notice("first".to_string(), at);
    assert_eq!(ledger.counts().raised, 1, "the sentence went up");
    assert_eq!(
        ledger.counts().raised_live,
        0,
        "there was nothing on the glass for it to replace",
    );

    // **The re-stamp**: inside NOTICE_LIFETIME, so the reader can still see
    // the sentence this one replaces.
    ledger.raise_notice("second".to_string(), at + NOTICE_LIFETIME / 2);
    assert_eq!(ledger.counts().raised, 2);
    assert_eq!(
        ledger.counts().raised_live,
        1,
        "a sentence replaced under the reader's eyes is the whole measurement",
    );

    // **One fact changed — the clock.** The same call after the notice aged
    // out is the next notice, not a re-stamp, and must not move `live`.
    ledger.raise_notice("third".to_string(), at + NOTICE_LIFETIME * 3);
    assert_eq!(ledger.counts().raised, 3);
    assert_eq!(
        ledger.counts().raised_live,
        1,
        "a notice raised after the last one expired replaced nothing the \
         reader could see",
    );
}

/// **A refusal's own stamp goes through the same counter**, so the figure is
/// of the glass and not of one call site. Both arms of the door: the enforcing
/// one raises, and the advisory one raises too — it says something different,
/// but it is a sentence on the glass either way.
#[test]
fn a_refusal_counts_its_stamp_on_both_arms() {
    for enforcing in [true, false] {
        let mut ledger = AdmissionLedger::default();
        ledger.adopt(&costs(0, 10 * MIB, 1));
        let before = ledger.counts();
        ledger.decide(
            Act::ShowLayer,
            Some(0),
            Increment::host(10 * MIB),
            enforcing,
        );
        let moved = ledger.counts().since(before);
        assert_eq!(moved.raised, 1, "enforcing = {enforcing}");
        assert_eq!(
            moved.raised_live, 0,
            "the first refusal of a session replaces nothing: enforcing = {enforcing}",
        );
        assert_eq!(
            moved.reoffered, 0,
            "a refusal is not a re-offer: enforcing = {enforcing}",
        );
    }
}

/// **This lane's own re-offer counts itself**, so it cannot masquerade as the
/// phantom re-stamp the counter was written to find.
///
/// A wish resolving on the telemetry tick puts a sentence up like any other,
/// and without `reoffered` beside `raised` it would be indistinguishable from
/// a refusal being re-stamped — the new code reading as the defect it was
/// added to measure. `raised - reoffered` is the refusal-driven figure.
#[test]
fn the_re_offer_path_counts_itself_apart_from_a_refusal_re_stamp() {
    let mut ledger = AdmissionLedger::default();
    ledger.adopt(&costs(0, 10 * MIB, 1));
    assert!(!ledger.decide(Act::ShowLayer, Some(0), Increment::host(10 * MIB), true));
    let after_refusal = ledger.counts();
    assert_eq!(after_refusal.raised, 1);
    assert_eq!(after_refusal.reoffered, 0);

    // The tick that publishes the room. The wish resolves and says so — ON
    // TOP of the refusal notice, which is still inside its six seconds, so
    // this is a genuine re-stamp AND a re-offer, and the two are counted
    // separately rather than one standing in for the other.
    let at = web_time::Instant::now();
    ledger.adopt_at(&roomier(64 * MIB, 10 * MIB, 1), at);
    let moved = ledger.counts().since(after_refusal);
    assert_eq!(moved.raised, 1, "the re-offer is a stamp like any other");
    assert_eq!(moved.reoffered, 1, "and it is attributable to this path");
    assert_eq!(
        ledger.counts().raised - ledger.counts().reoffered,
        1,
        "the refusal-driven figure is what is left after subtracting it",
    );

    // **The control that makes the subtraction mean something**: a tick with
    // no wish to resolve raises nothing at all.
    let mut quiet = AdmissionLedger::default();
    quiet.adopt(&costs(64 * MIB, 10 * MIB, 1));
    let before = quiet.counts();
    quiet.adopt_at(&roomier(64 * MIB, 10 * MIB, 1), web_time::Instant::now());
    let moved = quiet.counts().since(before);
    assert_eq!(moved.raised, 0);
    assert_eq!(moved.reoffered, 0);
}
