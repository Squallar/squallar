//! WO-E2 contract test 1 of 3: **sentinel expression**. Every field of
//! [`FrameInputs`] applied through `Gui::apply_frame_inputs` surfaces through
//! the `Gui`'s own read side, and *persists* across frames with no
//! re-application — the seam stores facts, it does not merely borrow them for
//! a frame. The App re-states the facts every frame in production; the
//! persistence half is what makes a missed compose a stale value rather than
//! a reverted one.

use super::FrameInputs;
use crate::input_harness::InputHarness;
use crate::ui::{ChunkFeedStatus, CurrentVolumeStamp};

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
    h.gui_mut().apply_frame_inputs(FrameInputs {
        safe_area_insets: (11.0, 22.0, 33.0, 44.0),
        supports_exit: false,
        loop_frame_budget: 7,
        location_settings_available: true,
        location: (rustdar_location::LocationPermission::Denied, false),
        gps: Some((rustdar_location::Fix::from_lat_lon(12.5, -34.25), gps_at)),
        user_heading: Some(123.0),
        catalogue_pending: true,
        chunk_status: status,
        current_volumes: &volumes,
        floor_tile_zoom_bias: 2,
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
        assert!(
            gui.location_settings_available(),
            "location_settings_available did not survive frame {frame}"
        );
        assert_eq!(
            gui.location_permission(),
            rustdar_location::LocationPermission::Denied,
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
            *gui.chunk_status(),
            status,
            "chunk_status did not survive frame {frame}"
        );
        assert_eq!(
            gui.current_volume_for("KTLX"),
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
    }
}

/// The `gps: None` arm clears **both** halves of the fix — the position and
/// its arrival instant — subsuming the old `clear_gps_fix`. Leaving either
/// would be the app holding a position it has just been told it may not know.
#[test]
fn a_none_gps_clears_the_fix() {
    let mut h = InputHarness::new();
    let volumes = std::collections::HashMap::new();
    let base = FrameInputs {
        safe_area_insets: (0.0, 0.0, 0.0, 0.0),
        supports_exit: true,
        loop_frame_budget: 60,
        location_settings_available: false,
        location: (rustdar_location::LocationPermission::Granted, true),
        gps: Some((
            rustdar_location::Fix::from_lat_lon(35.25, -97.5),
            web_time::Instant::now(),
        )),
        user_heading: None,
        catalogue_pending: false,
        chunk_status: ChunkFeedStatus::default(),
        current_volumes: &volumes,
        floor_tile_zoom_bias: 0,
    };
    h.gui_mut().apply_frame_inputs(base);
    assert!(
        h.gui().gps_fix().is_some(),
        "precondition: a fix is showing"
    );

    h.gui_mut().apply_frame_inputs(FrameInputs {
        gps: None,
        location: (rustdar_location::LocationPermission::Denied, false),
        safe_area_insets: (0.0, 0.0, 0.0, 0.0),
        supports_exit: true,
        loop_frame_budget: 60,
        location_settings_available: false,
        user_heading: None,
        catalogue_pending: false,
        chunk_status: ChunkFeedStatus::default(),
        current_volumes: &volumes,
        floor_tile_zoom_bias: 0,
    });
    h.frame();
    assert!(
        h.gui().gps_fix().is_none(),
        "the dot outlived the consent that allowed it"
    );
}
