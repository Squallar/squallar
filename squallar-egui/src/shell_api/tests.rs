//! Contract test: **sentinel expression**. Every field of
//! [`FrameInputs`] applied through `Gui::apply_frame_inputs` surfaces through
//! the `Gui`'s own read side, and *persists* across frames with no
//! re-application — the seam stores facts, it does not merely borrow them for
//! a frame. The App re-states the facts every frame in production; the
//! persistence half is what makes a missed compose a stale value rather than
//! a reverted one.

use super::FrameInputs;
use crate::input_harness::InputHarness;
use crate::radar_layer::{CurrentVolumeStamp, RadarLiveness};
use squallar_device_profile::budget::TileCacheBudget;
use squallar_radar::chunk_feed::ChunkFeedStatus;

/// A timestamp no default produces, so reading it back can only mean the
/// applied entry was stored.
fn sentinel_stamp() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 18)
        .expect("a real date")
        .and_hms_opt(12, 34, 56)
        .expect("a real time")
}

#[test]
fn every_frame_input_surfaces_and_persists() {
    let mut h = InputHarness::new();

    let mut volumes = std::collections::HashMap::new();
    volumes.insert(
        "KTLX".to_owned(),
        CurrentVolumeStamp {
            newest: sentinel_stamp(),
            base_started: Some(sentinel_stamp()),
        },
    );
    let gps_at = web_time::Instant::now();
    // Sentinels throughout: no default produces any of these values, so each
    // assertion below can only pass because the apply stored the field.
    let status = ChunkFeedStatus {
        feeding: true,
        retired: false,
        interval_secs: 5,
        pushed: true,
        tilt: None,
    };
    let liveness = vec![crate::radar_layer::liveness_entry(RadarLiveness {
        chunk_status: status,
        current_volumes: volumes.clone(),
    })];
    h.gui_mut().apply_frame_inputs(FrameInputs {
        safe_area_insets: (11.0, 22.0, 33.0, 44.0),
        supports_exit: false,
        loop_frame_budget: 7,
        concurrent_renders: 5,
        tile_cache: TileCacheBudget {
            styled_bytes: 111_111,
            parsed_bytes: 222_222,
            terrain_bytes: 333_333,
            whole_zoom: true,
        },
        overlay_overdraw: 0.125,
        location_settings_available: true,
        location: (squallar_location::LocationPermission::Denied, false),
        gps: Some((squallar_location::Fix::from_lat_lon(12.5, -34.25), gps_at)),
        user_heading: Some(123.0),
        catalogue_pending: true,
        liveness: &liveness,
        floor_tile_zoom_bias: 2,
        mirror_plan_stamp: 0,
        frame_diagnostics: None,
        budget_readout: None,
        admission: None,
        admission_debit: None,
        admission_notice: None,
    });

    // Two frames, no re-application: the values must persist. Frame 1 lays the
    // UI out over the applied facts; frame 2 is the frame nothing re-composed.
    for frame in 1..=2u32 {
        h.frame();
        let gui = h.gui();
        assert_eq!(
            gui.safe_area_insets(),
            (11.0, 22.0, 33.0, 44.0),
            "safe_area_insets did not survive frame {frame}"
        );
        assert!(
            !gui.supports_exit(),
            "supports_exit did not survive frame {frame}"
        );
        assert_eq!(
            gui.loop_frame_budget_for_test(),
            7,
            "loop_frame_budget did not survive frame {frame}"
        );
        assert_eq!(
            gui.overlay_overdraw_for_test(),
            0.125,
            "overlay_overdraw did not survive frame {frame}"
        );
        assert!(
            gui.location_settings_available(),
            "location_settings_available did not survive frame {frame}"
        );
        assert_eq!(
            gui.location_permission(),
            squallar_location::LocationPermission::Denied,
            "location permission did not survive frame {frame}"
        );
        assert!(
            !gui.location_active(),
            "location_active did not survive frame {frame}"
        );
        let fix = gui
            .gps_fix()
            .unwrap_or_else(|| panic!("the gps fix did not survive frame {frame}"));
        assert_eq!((fix.point.lat, fix.point.lon), (12.5, -34.25));
        assert_eq!(
            gui.user_heading(),
            Some(123.0),
            "user_heading did not survive frame {frame}"
        );
        assert!(
            gui.catalogue_pending(),
            "catalogue_pending did not survive frame {frame}"
        );
        assert_eq!(
            crate::radar_layer::chunk_status(gui.liveness()),
            status,
            "the radar layer's chunk status did not survive frame {frame}"
        );
        assert_eq!(
            crate::radar_layer::current_volume_for(gui.liveness(), "KTLX"),
            Some(CurrentVolumeStamp {
                newest: sentinel_stamp(),
                base_started: Some(sentinel_stamp()),
            }),
            "the current-volume entry did not survive frame {frame}"
        );
        assert_eq!(
            gui.floor_tile_zoom_bias_for_test(),
            2,
            "floor_tile_zoom_bias did not survive frame {frame}"
        );
        assert_eq!(
            gui.concurrent_renders_for_test(),
            5,
            "concurrent_renders did not survive frame {frame}"
        );
        assert_eq!(
            gui.tile_cache_budget_for_test(),
            TileCacheBudget {
                styled_bytes: 111_111,
                parsed_bytes: 222_222,
                terrain_bytes: 333_333,
                whole_zoom: true,
            },
            "tile_cache did not survive frame {frame}"
        );
    }
}

/// **The App's debited total reaches the UI's ledger over this seam**, and
/// the doors on both sides then spend one figure.
///
/// The wiring is the whole mechanism: two ledgers holding the same table but
/// a debit each are compared against one tick's spare twice, and nothing
/// about the table itself would show it. What crosses is a handle, computed
/// in `squallar-app` and re-stated every frame, which is the seam's own rule
/// — the UI reads verdicts, it does not price.
#[test]
fn the_apps_debited_total_crosses_the_frame_seam() {
    let mut h = InputHarness::new();
    let table = crate::admission::AdmissionCosts {
        generation: 1,
        spare: squallar_device_profile::admit::Spare {
            gpu_bytes: Some(0),
            host_bytes: Some(30 * 1024 * 1024),
            joint_bytes: None,
        },
        ..Default::default()
    };
    // The App's own ledger, holding the table it just composed.
    let mut app = crate::admission::AdmissionLedger::default();
    app.adopt(&table);

    h.gui_mut().apply_frame_inputs(FrameInputs {
        admission: Some(&table),
        admission_debit: Some(app.debit()),
        safe_area_insets: (0.0, 0.0, 0.0, 0.0),
        supports_exit: true,
        loop_frame_budget: 60,
        concurrent_renders: 1,
        tile_cache: crate::tile_source::default_tile_budget(),
        overlay_overdraw: crate::overlay_cache::OVERDRAW_FRACTION,
        location_settings_available: false,
        location: (squallar_location::LocationPermission::Granted, true),
        gps: None,
        user_heading: None,
        catalogue_pending: false,
        liveness: &[],
        floor_tile_zoom_bias: 0,
        mirror_plan_stamp: 0,
        frame_diagnostics: None,
        budget_readout: None,
        admission_notice: None,
    });

    assert!(
        h.gui().admission().debit().is_shared_with(app.debit()),
        "the handle must reach the ledger, or the two sides go on spending a \
         copy each",
    );

    // The App's loop door spends most of the tick's spare...
    assert!(
        app.ask(
            crate::admission::Act::ArmLoop,
            Some(0),
            squallar_device_profile::admit::Increment::host(25 * 1024 * 1024),
        )
        .is_admit(),
    );
    // ...and the UI's ledger, across the seam, sees what is left of it.
    assert_eq!(
        h.gui().admission().spare().host_bytes,
        Some(5 * 1024 * 1024),
        "a UI door asking after the App's has to be compared against what the \
         App left, not against the published figure again",
    );
}

/// The `gps: None` arm clears **both** halves of the fix — the position and
/// its arrival instant — subsuming the old `clear_gps_fix`. Leaving either
/// would be the app holding a position it has just been told it may not know.
#[test]
fn a_none_gps_clears_the_fix() {
    let mut h = InputHarness::new();
    let base = FrameInputs {
        safe_area_insets: (0.0, 0.0, 0.0, 0.0),
        supports_exit: true,
        loop_frame_budget: 60,
        concurrent_renders: 1,
        tile_cache: crate::tile_source::default_tile_budget(),
        overlay_overdraw: crate::overlay_cache::OVERDRAW_FRACTION,
        location_settings_available: false,
        location: (squallar_location::LocationPermission::Granted, true),
        gps: Some((
            squallar_location::Fix::from_lat_lon(35.25, -97.5),
            web_time::Instant::now(),
        )),
        user_heading: None,
        catalogue_pending: false,
        liveness: &[],
        floor_tile_zoom_bias: 0,
        mirror_plan_stamp: 0,
        frame_diagnostics: None,
        budget_readout: None,
        admission: None,
        admission_debit: None,
        admission_notice: None,
    };
    h.gui_mut().apply_frame_inputs(base);
    assert!(
        h.gui().gps_fix().is_some(),
        "precondition: a fix is showing"
    );

    h.gui_mut().apply_frame_inputs(FrameInputs {
        gps: None,
        location: (squallar_location::LocationPermission::Denied, false),
        safe_area_insets: (0.0, 0.0, 0.0, 0.0),
        supports_exit: true,
        loop_frame_budget: 60,
        concurrent_renders: 1,
        tile_cache: crate::tile_source::default_tile_budget(),
        overlay_overdraw: crate::overlay_cache::OVERDRAW_FRACTION,
        location_settings_available: false,
        user_heading: None,
        catalogue_pending: false,
        liveness: &[],
        floor_tile_zoom_bias: 0,
        mirror_plan_stamp: 0,
        frame_diagnostics: None,
        budget_readout: None,
        admission: None,
        admission_debit: None,
        admission_notice: None,
    });
    h.frame();
    assert!(
        h.gui().gps_fix().is_none(),
        "the dot outlived the consent that allowed it"
    );
}

/// **The readout crosses the seam on the App's cadence, and the Gui asks one
/// integer whether it moved.**
///
/// The App re-states the same borrowed readout on every frame and composes a
/// new one only on the tick that reads it. What the Gui must not do is pay for
/// that restatement: comparing the readouts structurally walks the pane
/// vector, both pool records and the grid list's layer ids — a string compare
/// each — on every frame, and copies whenever a byte figure moved, which
/// during a gesture is nearly every frame. The generation is the whole of the
/// question.
///
/// A test that asserted the *figures* arrive would have passed on the
/// structural compare too; what is asserted here is the copy count.
mod readout_cadence {
    use super::*;
    use crate::shell_api::{BudgetReadout, PaneBudget, PoolReadout};
    use squallar_source::id::LayerId;

    /// Two seconds of 120 Hz: the frames one composition has to survive.
    const FRAMES: usize = 240;

    /// A readout shaped like one the App composes — pane rows and a grid list,
    /// so a copy is a real pair of allocations and a structural compare would
    /// be a real walk. `need_bytes` distinguishes two readouts by content.
    fn readout(generation: u64, panes: usize, need_bytes: u64) -> BudgetReadout {
        BudgetReadout {
            generation,
            panes: vec![PaneBudget::default(); panes],
            gpu: PoolReadout {
                need_bytes,
                ..PoolReadout::default()
            },
            overlay_grids: vec![
                (LayerId::new("mrms-reflectivity"), 64 << 20),
                (LayerId::new("gmgsi-longwave"), 32 << 20),
            ],
            ..BudgetReadout::default()
        }
    }

    /// One frame's worth of inputs, everything but the readout held still —
    /// the App's per-frame restatement.
    fn apply(h: &mut InputHarness, budget_readout: Option<&BudgetReadout>) {
        h.gui_mut().apply_frame_inputs(FrameInputs {
            safe_area_insets: (0.0, 0.0, 0.0, 0.0),
            supports_exit: true,
            loop_frame_budget: 60,
            concurrent_renders: 1,
            tile_cache: crate::tile_source::default_tile_budget(),
            overlay_overdraw: crate::overlay_cache::OVERDRAW_FRACTION,
            location_settings_available: false,
            location: (squallar_location::LocationPermission::Denied, false),
            gps: None,
            user_heading: None,
            catalogue_pending: false,
            liveness: &[],
            floor_tile_zoom_bias: 0,
            mirror_plan_stamp: 0,
            frame_diagnostics: None,
            budget_readout,
            admission: None,
            admission_debit: None,
            admission_notice: None,
        });
    }

    #[test]
    fn the_seam_copies_once_per_composition_and_not_once_per_frame() {
        let mut h = InputHarness::new();
        assert_eq!(
            h.gui().budget_readout_copies(),
            0,
            "precondition: nothing has crossed the seam yet",
        );

        let composed = readout(1, 2, 100);
        for _ in 0..FRAMES {
            apply(&mut h, Some(&composed));
        }
        assert_eq!(
            h.gui().budget_readout_copies(),
            1,
            "{FRAMES} frames restating one composition took {} copies; the \
             seam is comparing readouts again instead of generations",
            h.gui().budget_readout_copies(),
        );
        assert_eq!(
            h.gui().budget_readout(),
            Some(&composed),
            "the one copy has to be the real readout, or the count is vacuous",
        );

        // **And a gesture's worth of moving figures under one generation.**
        // The App does not publish these — it composes once a tick, so
        // between ticks the readout is byte-identical — but the seam's
        // guarantee is that it would not pay for them if it did, which is
        // what makes the compare O(1) rather than O(panes + layers). A
        // structural compare copies on every one of these frames.
        for frame in 0..FRAMES {
            apply(&mut h, Some(&readout(1, 2, 100 + frame as u64)));
        }
        assert_eq!(
            h.gui().budget_readout_copies(),
            1,
            "{FRAMES} frames of moving byte figures under one composition              took {} copies; the seam is walking the readout again",
            h.gui().budget_readout_copies(),
        );

        // The App composed again: the generation moved, so the copy is taken.
        let next = readout(2, 2, 200);
        apply(&mut h, Some(&next));
        assert_eq!(
            h.gui().budget_readout_copies(),
            2,
            "a new composition did not cross, so the UI would paint the last \
             one forever",
        );
        assert_eq!(
            h.gui().budget_readout().map(|r| r.gpu.need_bytes),
            Some(200),
            "the copy carried the old figures",
        );

        // Clearing is not a copy, and still clears.
        apply(&mut h, None);
        assert!(
            h.gui().budget_readout().is_none(),
            "`None` no longer clears the held readout",
        );
        assert_eq!(
            h.gui().budget_readout_copies(),
            2,
            "clearing counted as a copy",
        );
    }

    /// **The generation is the whole question, and that is a contract the App
    /// keeps.** Figures that moved under an unmoved generation are not copied
    /// — which is sound only because `App::compose_budget_readout` bumps the
    /// generation on every composition, with no arm that rebuilds without it.
    /// `app_render/budget_readout_cadence_tests.rs` is what pins that half.
    #[test]
    fn an_unmoved_generation_is_not_re_copied_however_the_figures_moved() {
        let mut h = InputHarness::new();
        apply(&mut h, Some(&readout(7, 2, 100)));
        assert_eq!(h.gui().budget_readout_copies(), 1);

        apply(&mut h, Some(&readout(7, 5, 999)));
        assert_eq!(
            h.gui().budget_readout_copies(),
            1,
            "the seam copied on the figures, so the generation is not what it \
             is asking and every gesture frame pays a clone",
        );
    }
}

/// Contract test: **the liveness seam copies on the layers' cadence, not the
/// frame's.**
///
/// `FrameInputs::liveness` is a borrowed slice the App re-states on every
/// frame and rebuilds an entry of only when that layer's own answer moves
/// (`App::republish_liveness`, pinned from the producer side by
/// `chunk_feed_precedence_tests::an_unchanged_liveness_answer_is_restated_and_not_rebuilt`).
/// What the Gui must not do is pay for the restatement: an unguarded copy is a
/// heap allocation, an atomic bump per entry and the matching drop, on every
/// frame of every gesture, to re-state entries that are byte-for-byte the ones
/// already held.
///
/// A test that asserted the *entries arrive* would have passed on the
/// unguarded copy too; what is asserted here is the copy count.
mod liveness_cadence {
    use super::*;
    use squallar_source::liveness::SourceLiveness;

    /// Two seconds of 120 Hz: the frames one published answer has to survive.
    const FRAMES: usize = 240;

    fn entries(secs: u64) -> Vec<SourceLiveness> {
        vec![crate::radar_layer::liveness_entry(RadarLiveness {
            chunk_status: ChunkFeedStatus {
                feeding: true,
                retired: false,
                interval_secs: secs,
                pushed: true,
                tilt: None,
            },
            current_volumes: std::collections::HashMap::new(),
        })]
    }

    /// One frame's worth of inputs, everything but the liveness held still —
    /// the App's per-frame restatement.
    fn apply(h: &mut InputHarness, liveness: &[SourceLiveness]) {
        h.gui_mut().apply_frame_inputs(FrameInputs {
            safe_area_insets: (0.0, 0.0, 0.0, 0.0),
            supports_exit: true,
            loop_frame_budget: 60,
            concurrent_renders: 1,
            tile_cache: crate::tile_source::default_tile_budget(),
            overlay_overdraw: crate::overlay_cache::OVERDRAW_FRACTION,
            location_settings_available: false,
            location: (squallar_location::LocationPermission::Denied, false),
            gps: None,
            user_heading: None,
            catalogue_pending: false,
            liveness,
            floor_tile_zoom_bias: 0,
            mirror_plan_stamp: 0,
            frame_diagnostics: None,
            budget_readout: None,
            admission: None,
            admission_debit: None,
            admission_notice: None,
        });
    }

    #[test]
    fn the_seam_copies_once_per_published_answer_and_not_once_per_frame() {
        let mut h = InputHarness::new();
        assert_eq!(
            h.gui().liveness_copies(),
            0,
            "precondition: nothing has crossed the seam yet",
        );

        // The App's steady state: one published answer, re-stated every frame.
        let published = entries(5);
        for _ in 0..FRAMES {
            apply(&mut h, &published);
        }
        assert_eq!(
            h.gui().liveness_copies(),
            1,
            "{FRAMES} frames restating one published answer took {} copies; \
             the seam is cloning the slice again instead of comparing it",
            h.gui().liveness_copies(),
        );
        assert_eq!(
            crate::radar_layer::chunk_status(h.gui().liveness()).interval_secs,
            5,
            "the one copy has to be the real answer, or the count is vacuous",
        );

        // A layer republished: a new payload allocation, so the copy is taken.
        let republished = entries(9);
        apply(&mut h, &republished);
        assert_eq!(
            h.gui().liveness_copies(),
            2,
            "a republished answer did not cross, so the UI would paint the \
             last one forever",
        );
        assert_eq!(
            crate::radar_layer::chunk_status(h.gui().liveness()).interval_secs,
            9,
            "the copy carried the old answer",
        );

        // …and it settles again on the new one.
        for _ in 0..FRAMES {
            apply(&mut h, &republished);
        }
        assert_eq!(
            h.gui().liveness_copies(),
            2,
            "the seam does not settle: it copied on {FRAMES} frames that \
             restated the answer it had just taken",
        );
    }

    /// **A layer that stops publishing, and one that starts, both cross.** The
    /// compare is over the whole entry set, so the length is part of it — a
    /// guard that only walked the entries it held in common would serve a
    /// retired layer's last answer forever.
    #[test]
    fn an_entry_appearing_or_vanishing_is_copied() {
        let mut h = InputHarness::new();
        let published = entries(5);
        apply(&mut h, &published);
        assert_eq!(h.gui().liveness_copies(), 1);

        apply(&mut h, &[]);
        assert_eq!(
            h.gui().liveness_copies(),
            2,
            "the layer stopped publishing and the seam kept its last answer",
        );
        assert!(
            h.gui().liveness().is_empty(),
            "the emptied slice did not cross",
        );

        apply(&mut h, &published);
        assert_eq!(
            h.gui().liveness_copies(),
            3,
            "the layer published again and the seam stayed empty",
        );
    }

    /// **The question is pointer identity, and it is asked of the payloads.**
    /// Two byte-identical answers built separately are two allocations, and
    /// the seam copies — it must, because the payload is opaque and nothing on
    /// this path may look inside one to find out otherwise. Recorded so the
    /// cheapness claim is read as what it is: the App's restatement is free,
    /// a rebuild is not, and no frame path rebuilds.
    #[test]
    fn a_rebuilt_but_identical_answer_is_copied_because_the_payload_is_opaque() {
        let mut h = InputHarness::new();
        apply(&mut h, &entries(5));
        assert_eq!(h.gui().liveness_copies(), 1);

        apply(&mut h, &entries(5));
        assert_eq!(
            h.gui().liveness_copies(),
            2,
            "a separately built payload compared equal — the seam is looking \
             inside an opaque payload, which it may not do",
        );
    }
}
