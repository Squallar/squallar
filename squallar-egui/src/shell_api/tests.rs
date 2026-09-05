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
