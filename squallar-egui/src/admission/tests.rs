//! **Both arms, at every door.**
//!
//! Every refusal fixture here has an admit fixture beside it that *resembles*
//! it — the same act, the same door, one fact changed — because over-firing is
//! the worse direction: a door that refuses what the machine can hold gets
//! switched off, and then nothing is gated at all.
//!
//! The doors are advisory under WO-G, so what a test can observe is the
//! **verdict** and the counters, not a changed outcome. That is the point of
//! the land: the arithmetic is proved before anything acts on it.
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
            budget_span_secs: 60 * 60,
            render_budget: 30,
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
        ledger.ask(Act::ShowLayer, Increment::host(u64::MAX)),
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
        roomy.ask(Act::ShowLayer, Increment::host(10 * MIB)),
        Verdict::Admit,
    );

    let mut tight = AdmissionLedger::default();
    tight.adopt(&costs(4 * MIB, 10 * MIB, 1));
    assert!(
        !tight
            .ask(Act::ShowLayer, Increment::host(10 * MIB))
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
        ledger.ask(Act::ShowLayer, Increment::host(10 * MIB)),
        Verdict::Admit,
    );
    assert_eq!(ledger.spare().host_bytes, Some(15 * MIB));
    assert_eq!(
        ledger.ask(Act::ShowLayer, Increment::host(10 * MIB)),
        Verdict::Admit,
    );
    assert_eq!(ledger.spare().host_bytes, Some(5 * MIB));
    assert!(
        !ledger
            .ask(Act::ShowLayer, Increment::host(10 * MIB))
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
    let _ = ledger.ask(Act::ShowLayer, Increment::host(10 * MIB));
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
            ledger.ask(Act::ShowLayer, Increment::host(10 * MIB)),
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
            .ask(Act::ShowLayer, Increment::host(100 * MIB))
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
    assert_eq!(ledger.ask(Act::LoopSpan, want), Verdict::Admit);
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

    assert!(!ledger.enforce(Act::ShowLayer, Increment::host(10 * MIB)));
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
    assert!(ledger.enforce(Act::ShowLayer, Increment::host(u64::MAX)));
    ledger.end_exempt();
    assert!(ledger.notice(web_time::Instant::now()).is_none());
    assert_eq!(ledger.counts().refused, 0);

    // Control: the same act outside the exemption refuses and does raise one.
    assert!(!ledger.enforce(Act::ShowLayer, Increment::host(u64::MAX)));
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
