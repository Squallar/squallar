use super::tests::{empty_scan, headless};
use super::*;
use crate::platform_double::TestBridge;
use squallar_source::id::known;

/// The cell budget a device that says nothing about itself resolves.
const SHIPPED_CELLS: [u32; 3] = squallar_device_profile::constants::VOLUME_GRID_CELLS;

/// **The request the seam shapes for `target` on a device with this budget.**
///
/// Both halves of the production path and nothing else: `volume_job_context`
/// is exactly what `prepare_volume` hands over, and `request_for` is exactly
/// what `RadarSource::volume_job` makes of it before wrapping it in an
/// envelope. The moment is no longer an argument — it comes off the target's
/// own field, which is the one place a 3D ask names a field at all.
///
/// **The payload is a stand-in, and that is a statement rather than a
/// shortcut**: `request_for` is a pure function of the ground, the budget and
/// the field, and it never reads the payload. The downcast half — where the
/// payload is the whole question — is pinned separately in
/// `volume_layer_tests`, against a real extracted volume.
///
/// The **site** is handed over separately because the box is a rectangle in the
/// site's tangent frame and its floor is derived over that frame.
/// `RadarSource::volume_job` takes it off the payload, which a stand-in payload
/// cannot answer; this helper is already given the site's own coordinates and
/// passes them, so the request it shapes is the one production would shape.
fn shaped_request(
    target: &squallar_egui::pane::VolumeTarget,
    site_lat: f64,
    site_lon: f64,
    cells: [u32; 3],
    max_axis: u32,
) -> squallar_radar::voxel::VoxelRequest {
    let ctx = volume_job_context(target, site_lat, site_lon, cells, max_axis, Box::new(()));
    squallar_radar::voxel::request_for(&ctx, (site_lat, site_lon))
        .expect("Reflectivity is a field radar registers")
}

fn at(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 28)
        .unwrap()
        .and_hms_opt(18, minute, 0)
        .unwrap()
}

/// A live pane on KTLX already showing a volume assembled at `shown`.
fn app_showing(shown: chrono::NaiveDateTime) -> App {
    let mut app = headless(TestBridge::desktop());
    {
        let pane = app.gui.pane_mut(0).unwrap();
        pane.set_site("KTLX".to_string());
        pane.viewing_live = true;
        pane.scan_info = Some(squallar_radar::types::ScanInfo {
            site_source: squallar_radar::site_position::SitePositionSource::Table,
            site_position: None,
            site: squallar_radar::sites::RadarSite {
                name: "KTLX",
                network: squallar_radar::sites::RadarNetwork::of_id("KTLX"),
                lat: 35.3,
                lon: -97.3,
                heights: None,
            },
            timestamp: shown,
            vcp_number: 212,
            available_products: Vec::new(),
            product_elevations: Default::default(),
            status: String::new(),
        });
    }
    app
}

fn send_archive(app: &App, timestamp: chrono::NaiveDateTime) {
    send_archive_scan(app, timestamp, empty_scan());
}

/// [`send_archive`] with a caller-chosen scan, for assertions that need
/// the volume to carry a radial — a stamp only resolves off real data.
fn send_archive_scan(app: &App, timestamp: chrono::NaiveDateTime, scan: nexrad_model::data::Scan) {
    let generation = app.render.fetch_generation_for("KTLX");
    app.channels
        .scan_sender
        .send(crate::channels::ScanResponse {
            generation,
            site: "KTLX".to_string(),
            requester: crate::channels::FetchRequester::Site,
            result: Ok(crate::channels::ScanData {
                scan,
                declared_nyquist: Default::default(),
                site: "KTLX".to_string(),
                timestamp,
            }),
            is_auto_poll: false,
        })
        .unwrap();
}

/// The same archive volume, arriving from the auto-poll rather than from a
/// Refresh — which is the other arm that declines to put it on screen.
fn send_auto_poll_archive(app: &App, timestamp: chrono::NaiveDateTime) {
    let generation = app.render.fetch_generation_for("KTLX");
    app.channels
        .scan_sender
        .send(crate::channels::ScanResponse {
            generation,
            site: "KTLX".to_string(),
            requester: crate::channels::FetchRequester::Site,
            result: Ok(crate::channels::ScanData {
                scan: empty_scan(),
                declared_nyquist: Default::default(),
                site: "KTLX".to_string(),
                timestamp,
            }),
            is_auto_poll: true,
        })
        .unwrap();
}

/// The bug this closes: pressing Refresh while the real-time feed was ahead
/// reverted the display to the previous archive volume.
#[test]
fn an_archive_volume_older_than_the_feed_does_not_replace_it() {
    let mut app = app_showing(at(10));
    app.chunk_feeds.ensure("KTLX");
    assert!(app.chunks_are_feeding("KTLX"), "precondition: feed running");

    send_archive(&app, at(5));
    app.poll_data_channels();

    assert_eq!(
        app.gui
            .pane(0)
            .unwrap()
            .scan_info
            .as_ref()
            .unwrap()
            .timestamp,
        at(10),
        "Refresh walked the display back to the previous archive volume"
    );
    assert!(
        !app.volumes.holds_any_still("KTLX"),
        "and it replaced the volume the panes render from"
    );
}

/// The wait still has to end.
#[test]
fn a_skipped_archive_volume_still_ends_the_wait_it_belonged_to() {
    let mut app = app_showing(at(10));
    app.chunk_feeds.ensure("KTLX");
    app.gui
        .apply(squallar_egui::shell_api::GuiEvent::Fetching(true));
    app.gui.pane_mut(0).unwrap().loading_site = Some("KTLX".to_string());

    send_archive(&app, at(5));
    app.poll_data_channels();

    assert!(!app.gui.fetching(), "the spinner was left up");
    assert!(
        app.gui.pane(0).unwrap().loading_site.is_none(),
        "and the pane's loading marker with it"
    );
}

/// The counterweight: a genuinely newer archive volume is still applied, or
/// the guard would freeze the display whenever a feed existed.
#[test]
fn an_archive_volume_newer_than_the_feed_is_applied() {
    let mut app = app_showing(at(10));
    app.chunk_feeds.ensure("KTLX");

    send_archive(&app, at(15));
    app.poll_data_channels();

    assert!(
        app.volumes.holds_any_still("KTLX"),
        "a newer archive volume was refused"
    );
}

/// And with no feed running the archive is authoritative, which is what the
/// fallback depends on — a retired feed leaves the site here.
#[test]
fn without_a_feed_the_archive_is_applied_unconditionally() {
    let mut app = app_showing(at(10));
    assert!(!app.chunks_are_feeding("KTLX"));

    send_archive(&app, at(5));
    app.poll_data_channels();

    assert!(
        app.volumes.holds_any_still("KTLX"),
        "the fallback cannot restore a site if an older archive volume is \
             refused when no feed is running"
    );
}

/// **The overlay dies with the setting.**
#[test]
fn turning_live_chunks_off_stops_the_overlay_from_standing() {
    let mut app = app_showing(at(10));
    app.gui.apply_layer_control(
        &squallar_egui::radar_layer::POLL_LAYER,
        &squallar_egui::radar_layer::chunk_notifications_update(false),
    );
    app.chunk_feeds.ensure("KTLX");
    app.chunk_feeds
        .force_serving("KTLX", Arc::new(empty_scan()));
    assert!(
        app.chunk_feeds.snapshot("KTLX").is_some(),
        "precondition: the feed is serving an overlay",
    );

    app.gui.apply_layer_control(
        &squallar_egui::radar_layer::POLL_LAYER,
        &squallar_egui::radar_layer::live_chunks_update(false),
    );
    app.drive_chunk_feeds();

    assert!(
        app.chunk_feeds.snapshot("KTLX").is_none(),
        "the setting went off and the last assembler kept serving its \
             frozen overlay to every consumer of the merged current volume",
    );
}

/// The 3D texture limit these fixtures name when they ask what would be requested.
const DEVICE_AXIS: u32 = 2048;

/// The device's own limit reaches the request, and the shape it produces is the
/// one that device can hold.
#[test]
fn the_requested_shape_is_the_one_this_device_can_hold() {
    use squallar_egui::pane::{VolumeStamp, VolumeTarget};

    let target = VolumeTarget {
        volume: VolumeStamp {
            site: "KTLX".to_owned(),
            collected: chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
                .expect("a real date")
                .and_hms_opt(22, 33, 0)
                .expect("a real time"),
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region: None,
    };

    for axis in [256u32, 512, 2048] {
        let request = shaped_request(&target, 35.33, -97.28, SHIPPED_CELLS, axis);
        assert_eq!(
            request.shape,
            squallar_device_profile::constants::volume_grid_shape(axis),
            "a {axis}-reporting device must be asked for the shape its own \
             limit and this target's budget produce",
        );
        for (name, n) in [
            ("nx", request.shape.nx),
            ("ny", request.shape.ny),
            ("nz", request.shape.nz),
        ] {
            assert!(
                n as u32 <= axis,
                "a {axis}-reporting device was asked for {n} cells of {name}",
            );
        }
    }
    assert_eq!(
        shaped_request(
            &target,
            35.33,
            -97.28,
            SHIPPED_CELLS,
            squallar_device_profile::constants::WEBGL2_MAX_TEXTURE_DIMENSION_3D
        )
        .shape,
        squallar_device_profile::constants::VOLUME_GRID_FLOOR_SHAPE,
    );
}

/// A picked region decides the ground that is resampled; without one, the
/// default box about the site does.
#[test]
fn a_picked_region_decides_the_ground_that_is_resampled() {
    use squallar_egui::pane::{VolumeRegion, VolumeStamp, VolumeTarget};
    use squallar_geo::GeoPoint;

    let target = |region| VolumeTarget {
        volume: VolumeStamp {
            site: "KTLX".to_owned(),
            collected: chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
                .expect("a real date")
                .and_hms_opt(22, 33, 0)
                .expect("a real time"),
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region,
    };

    let default = shaped_request(&target(None), 35.33, -97.28, SHIPPED_CELLS, DEVICE_AXIS);
    assert_eq!(default.centre, (35.33, -97.28), "no region means the site");
    assert_eq!(
        default.half_extent_km, None,
        "no region must leave the extent to `build_voxels`, which is the only \
         side of the seam that holds the volume the reach comes from",
    );

    let picked = VolumeRegion::new(
        GeoPoint {
            lat: 36.1,
            lon: -98.4,
        },
        squallar_radar::voxel::HalfExtentKm::square(22.5),
    )
    .expect("a valid region");
    let aimed = shaped_request(
        &target(Some(picked)),
        35.33,
        -97.28,
        SHIPPED_CELLS,
        DEVICE_AXIS,
    );
    assert_eq!(
        aimed.centre,
        (36.1, -98.4),
        "a picked region must move the box off the site",
    );
    assert_eq!(
        aimed.half_extent_km,
        Some(squallar_radar::voxel::HalfExtentKm::square(22.5)),
    );
}

/// The vertical extent is not part of the region pick.
#[test]
fn a_region_pick_does_not_move_the_top_or_the_bottom_of_the_box() {
    use squallar_egui::pane::{VolumeRegion, VolumeStamp, VolumeTarget};
    use squallar_geo::GeoPoint;

    let make = |region| VolumeTarget {
        volume: VolumeStamp {
            site: "KTLX".to_owned(),
            collected: chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
                .expect("a real date")
                .and_hms_opt(22, 33, 0)
                .expect("a real time"),
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region,
    };
    let picked = VolumeRegion::new(
        GeoPoint {
            lat: 36.1,
            lon: -98.4,
        },
        squallar_radar::voxel::HalfExtentKm::square(15.0),
    );

    for target in [make(None), make(picked)] {
        let request = shaped_request(&target, 35.33, -97.28, SHIPPED_CELLS, DEVICE_AXIS);
        assert_eq!(
            request.base_km_msl,
            squallar_radar::voxel::DEFAULT_BASE_KM_MSL
        );
        assert_eq!(
            request.top_km_msl,
            squallar_radar::voxel::DEFAULT_TOP_KM_MSL
        );
    }
}

/// The pane and the resampler agree about the box a pane has before its first
/// grid lands.
#[test]
fn the_pane_and_the_resampler_agree_about_the_stand_in_box() {
    let base = squallar_egui::pane::BASE_HALF_WIDTH_KM;
    assert_eq!(base, squallar_radar::voxel::box_half_width_km(f64::NAN));
    assert_eq!(
        squallar_egui::pane::box_size_km(None, None),
        [
            (2.0 * base) as f32,
            (2.0 * base) as f32,
            (squallar_radar::voxel::DEFAULT_TOP_KM_MSL - squallar_radar::voxel::DEFAULT_BASE_KM_MSL)
                as f32,
        ],
    );
}

/// **The 3D build reads the merge base and never the still store.**
fn stamped_scan(minute: u32) -> nexrad_model::data::Scan {
    use nexrad_model::data::{
        ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus, Scan,
        Sweep, VolumeCoveragePattern, WaveformType,
    };
    let stamp_ms = at(minute).and_utc().timestamp_millis();
    let radial = Radial::new(
        stamp_ms,
        1,
        0.0,
        1.0,
        RadialStatus::IntermediateRadialData,
        1,
        0.5,
        Some(MomentData::from_fixed_point(
            2,
            2125,
            250,
            8,
            2.0,
            66.0,
            vec![100, 120],
        )),
        None,
        None,
        None,
        None,
        None,
        None,
    );
    Scan::new(
        VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            PulseWidth::Short,
            false,
            0,
            false,
            0,
            false,
            false,
            0,
            false,
            false,
            vec![ElevationCut::new(
                0.5,
                ChannelConfiguration::ConstantPhase,
                WaveformType::CS,
                20.0,
                true,
                true,
                false,
                false,
                1,
                20,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                false,
                0,
                false,
                0,
                false,
                false,
            )],
        ),
        vec![Sweep::new(1, vec![radial])],
    )
}

#[test]
fn the_3d_build_reads_the_base_volume_and_not_the_live_snapshot() {
    let target = squallar_egui::pane::VolumeTarget {
        volume: squallar_egui::pane::VolumeStamp {
            site: "KTLX".to_owned(),
            collected: at(10),
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region: None,
    };

    let mut live_only = headless(TestBridge::desktop());
    drop(live_only.volumes.install_still(
        "KTLX".to_string(),
        at(10),
        (Arc::new(stamped_scan(10)), Default::default()),
    ));
    live_only.handle_prepare_volume(0, &squallar_source::id::known::RADAR, target.clone());
    assert!(
        live_only.volume_store.lookup(&target).is_none(),
        "a volume only the map panes hold was handed to the resampler",
    );

    let mut based = headless(TestBridge::desktop());
    based.volumes.install_base(
        "KTLX".to_string(),
        (Arc::new(stamped_scan(10)), Default::default(), at(10)),
    );
    based.handle_prepare_volume(0, &squallar_source::id::known::RADAR, target.clone());
    assert!(
        based.volume_store.lookup(&target).is_some(),
        "the 3D pane was offered a base volume and the build never \
             reached it, so the pane waits for ever",
    );
}

/// **A budget-refused frame pays nothing for the refusal.**
#[test]
fn a_full_budget_refuses_the_3d_ask_before_paying_the_extraction() {
    use squallar_device_profile::constants::MAX_CONCURRENT_RENDERS;
    use std::sync::atomic::Ordering;

    let target = squallar_egui::pane::VolumeTarget {
        volume: squallar_egui::pane::VolumeStamp {
            site: "KTLX".to_owned(),
            collected: at(10),
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region: None,
    };
    let mut app = headless(TestBridge::desktop());
    app.volumes.install_base(
        "KTLX".to_string(),
        (Arc::new(stamped_scan(10)), Default::default(), at(10)),
    );

    app.render
        .renders_in_flight
        .store(MAX_CONCURRENT_RENDERS, Ordering::Relaxed);
    for _ in 0..3 {
        app.handle_prepare_volume(0, &squallar_source::id::known::RADAR, target.clone());
    }
    assert_eq!(
        app.volume_extractions.get(),
        0,
        "a budget-refused frame paid the multi-ms merged-volume walk, and \
             the level-triggered pane repeats it every frame until a slot frees",
    );
    assert!(
        app.volume_store.lookup(&target).is_none(),
        "the ask must stay pending: nothing dispatched and nothing marked",
    );

    app.render.renders_in_flight.store(0, Ordering::Relaxed);
    app.handle_prepare_volume(0, &squallar_source::id::known::RADAR, target.clone());
    assert_eq!(
        app.volume_extractions.get(),
        1,
        "the freed slot performs exactly one extraction",
    );
    assert!(
        app.volume_store.lookup(&target).is_some(),
        "the freed slot dispatches the build",
    );

    app.handle_prepare_volume(0, &squallar_source::id::known::RADAR, target.clone());
    assert_eq!(
        app.volume_extractions.get(),
        1,
        "the level-triggered re-ask must attach, not re-extract",
    );
}

/// A pane is handed the volume it named, or none.
#[test]
fn a_3d_pane_is_not_handed_a_volume_other_than_the_one_it_asked_for() {
    let target = squallar_egui::pane::VolumeTarget {
        volume: squallar_egui::pane::VolumeStamp {
            site: "KTLX".to_owned(),
            collected: at(10),
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region: None,
    };

    let mut app = headless(TestBridge::desktop());
    app.volumes.install_base(
        "KTLX".to_string(),
        (Arc::new(stamped_scan(15)), Default::default(), at(15)),
    );
    app.handle_prepare_volume(0, &squallar_source::id::known::RADAR, target.clone());

    assert!(
        app.volume_store.lookup(&target).is_none(),
        "the pane asked for the 18:10 volume and was built the 18:15 one",
    );
}

/// **Every archive path offers its volume to the 3D pane**, including the
/// two that decline to display it.
#[test]
fn every_archive_path_offers_its_volume_to_the_3d_pane() {
    let collected = |app: &App| app.volumes.base_collected_at("KTLX");

    let mut shown = app_showing(at(10));
    send_archive(&shown, at(15));
    shown.poll_data_channels();
    assert!(
        shown.volumes.holds_any_still("KTLX"),
        "precondition: this is the arm that puts the volume on screen",
    );
    assert_eq!(collected(&shown), Some(at(15)));

    let mut behind = app_showing(at(10));
    behind.chunk_feeds.ensure("KTLX");
    send_archive(&behind, at(5));
    behind.poll_data_channels();
    assert!(
        !behind.volumes.holds_any_still("KTLX"),
        "precondition: this is the `feed_is_ahead` arm",
    );
    assert_eq!(
        collected(&behind),
        Some(at(5)),
        "a site with a feed running takes this arm on every poll, so a 3D \
             pane on it would never be offered a volume at all",
    );

    let mut historic = app_showing(at(10));
    historic.gui.pane_mut(0).unwrap().viewing_live = false;
    send_auto_poll_archive(&historic, at(15));
    historic.poll_data_channels();
    assert!(
        !historic.volumes.holds_any_still("KTLX"),
        "precondition: this is the auto-poll-while-historic arm",
    );
    assert_eq!(collected(&historic), Some(at(15)));
}

/// **A Refresh in the pre-publication window must not walk the base back.**
#[test]
fn a_refresh_in_the_pre_publication_window_does_not_walk_the_base_back() {
    let based = |app: &App| app.volumes.base_collected_at("KTLX");
    let mut app = app_showing(at(10));
    app.chunk_feeds.ensure("KTLX");
    app.volumes.install_base(
        "KTLX".to_string(),
        (Arc::new(stamped_scan(10)), Default::default(), at(10)),
    );

    send_archive(&app, at(5));
    app.poll_data_channels();
    assert_eq!(
        based(&app),
        Some(at(10)),
        "a manual Refresh in the pre-publication window regressed the \
             merge base one volume, under every whole-volume consumer",
    );

    send_archive(&app, at(15));
    app.poll_data_channels();
    assert_eq!(based(&app), Some(at(15)), "a newer volume was refused");

    app.chunk_feeds
        .force_retire_at("KTLX", std::time::Duration::from_secs(1));
    assert!(
        !app.chunks_are_feeding("KTLX"),
        "precondition: feed retired"
    );
    send_archive(&app, at(12));
    app.poll_data_channels();
    assert_eq!(
        based(&app),
        Some(at(12)),
        "with no feed ahead the base must follow the volume on display, \
             or a navigated section cuts newer data under an older caption",
    );
}

/// And the recorded volume reaches the pane that has to name it.
#[test]
fn the_recorded_base_volume_is_published_to_the_pane_that_names_it() {
    let mut app = app_showing(at(10));
    app.chunk_feeds.ensure("KTLX");
    assert_eq!(
        squallar_egui::radar_layer::current_volume_for(app.gui.liveness(), "KTLX"),
        None,
        "precondition: nothing published yet",
    );

    send_archive_scan(&app, at(5), stamped_scan(5));
    app.poll_data_channels();
    app.push_frame_inputs();

    let stamp = squallar_egui::radar_layer::current_volume_for(app.gui.liveness(), "KTLX")
        .expect("the app holds a base volume the 3D pane is never told about");
    assert_eq!(
        stamp.newest,
        at(5),
        "the stamp must be the volume's own newest data time",
    );
    assert_eq!(
        stamp.base_started,
        Some(at(5)),
        "a pure base volume names itself as the base",
    );
}

/// A pane on `site` at `shown`, beside [`app_showing`]'s pane 0 — the state
/// a second linked-off or unlinked pane is in while its sibling navigates.
fn add_live_pane(app: &mut App, shown: chrono::NaiveDateTime) {
    let mut two = super::tests::two_pane_app("KTLX", "KTLX");
    std::mem::swap(&mut app.gui, &mut two.gui);
    for idx in [0, 1] {
        let pane = app.gui.pane_mut(idx).unwrap();
        pane.viewing_live = true;
        pane.scan_info = Some(squallar_radar::types::ScanInfo {
            site_source: squallar_radar::site_position::SitePositionSource::Table,
            site_position: None,
            site: squallar_radar::sites::RadarSite {
                name: "KTLX",
                network: squallar_radar::sites::RadarNetwork::of_id("KTLX"),
                lat: 35.3,
                lon: -97.3,
                heights: None,
            },
            timestamp: shown,
            vcp_number: 212,
            available_products: Vec::new(),
            product_elevations: Default::default(),
            status: String::new(),
        });
    }
    app.render.ensure_pane_count(2);
}

fn shown_stamp(app: &App) -> chrono::NaiveDateTime {
    app.gui
        .pane(0)
        .unwrap()
        .scan_info
        .as_ref()
        .unwrap()
        .timestamp
}

/// The "time controls are inert" root cause, pinned at its site.
#[test]
fn a_manual_navigation_outranks_the_feed_guard() {
    let mut app = app_showing(at(10));
    add_live_pane(&mut app, at(10));
    app.chunk_feeds.ensure("KTLX");
    assert!(app.chunks_are_feeding("KTLX"), "precondition: feed running");

    app.handle_gui_action(
        squallar_egui::actions::GuiAction::NavigateTime {
            pane_idx: 0,
            step_secs: -600,
        },
        None,
    );
    assert!(
        app.manual_nav_pending,
        "precondition: the navigation marked itself pending"
    );
    send_archive(&app, at(0));
    app.poll_data_channels();

    assert_eq!(
        shown_stamp(&app),
        at(0),
        "the feed guard swallowed a manual navigation's volume - the \
         transport's Back is inert again"
    );
    assert!(
        !app.manual_nav_pending,
        "the applied navigation must clear its pending flag"
    );
    assert!(
        app.volumes.holds_any_still("KTLX"),
        "the navigated volume must become the site's displayed scan"
    );
}

/// The single-pane race arm of the same break: the response drains on the very frame
/// the click was processed.
#[test]
fn a_navigation_response_on_a_parked_site_applies_even_mid_retire() {
    let mut app = app_showing(at(10));
    app.chunk_feeds.ensure("KTLX");
    assert!(app.chunks_are_feeding("KTLX"), "precondition: feed running");

    app.handle_gui_action(
        squallar_egui::actions::GuiAction::NavigateTime {
            pane_idx: 0,
            step_secs: -600,
        },
        None,
    );
    assert!(
        app.chunks_are_feeding("KTLX"),
        "precondition: the feed has not yet retired - this is the race"
    );
    send_archive(&app, at(0));
    app.poll_data_channels();

    assert_eq!(
        shown_stamp(&app),
        at(0),
        "a navigation on a parked site lost to a feed with no live viewer \
         left to protect"
    );
}

/// The exemption's own limit: an auto-poll result really is a "latest"
/// claim, so a pending navigation must not smuggle one past the guard.
#[test]
fn an_auto_poll_result_stays_behind_the_guard_even_mid_navigation() {
    let mut app = app_showing(at(10));
    app.chunk_feeds.ensure("KTLX");
    app.manual_nav_pending = true;

    send_auto_poll_archive(&app, at(5));
    app.poll_data_channels();

    assert_eq!(
        shown_stamp(&app),
        at(10),
        "an auto-poll volume walked a chunk-fed live display backwards \
         because a navigation happened to be in flight"
    );
}

/// Live on a chunk-fed site is a reattachment, not a fetch: the panes already hold the
/// feed's current volume.
#[test]
fn jump_to_live_on_a_serving_feed_reattaches_without_a_fetch() {
    let mut app = app_showing(at(10));
    add_live_pane(&mut app, at(10));
    app.gui.pane_mut(0).unwrap().viewing_live = false;
    app.chunk_feeds.ensure("KTLX");
    assert!(app.chunks_are_feeding("KTLX"), "precondition: feed running");
    let generation = app.render.fetch_generation_for("KTLX");

    app.handle_gui_action(
        squallar_egui::actions::GuiAction::JumpToLive { pane_idx: 0 },
        None,
    );

    assert!(
        app.gui.pane(0).unwrap().viewing_live,
        "Live must reattach the pane to the feed"
    );
    assert_eq!(
        app.render.fetch_generation_for("KTLX"),
        generation,
        "Live on a serving feed spent a fetch on data already on screen"
    );
    assert!(
        !app.manual_nav_pending,
        "a reattachment leaves nothing pending for the scan drain to settle"
    );
    assert!(
        !app.gui.fetching(),
        "a reattachment must not raise the fetch spinner"
    );
}

/// With the site parked and its feed retired, Live still takes the archive route:
/// cached volume if one was kept, else a real fetch.
#[test]
fn jump_to_live_with_the_feed_retired_still_fetches() {
    let mut app = app_showing(at(10));
    app.gui.pane_mut(0).unwrap().viewing_live = false;
    assert!(!app.chunks_are_feeding("KTLX"), "precondition: no feed");
    let generation = app.render.fetch_generation_for("KTLX");

    app.handle_gui_action(
        squallar_egui::actions::GuiAction::JumpToLive { pane_idx: 0 },
        None,
    );

    assert!(app.gui.pane(0).unwrap().viewing_live);
    assert_eq!(
        app.render.fetch_generation_for("KTLX"),
        generation + 1,
        "with no feed serving, Live must fetch the latest volume"
    );
    assert!(
        app.manual_nav_pending,
        "the fetch settles through the drain"
    );
}

/// `NavigateTime`'s payload, acted on: the step is relative to the pane's own scan
/// time, the pane parks out of live.
#[test]
fn navigate_time_steps_relative_to_the_panes_scan_and_parks_it() {
    let mut app = app_showing(at(30));
    let generation = app.render.fetch_generation_for("KTLX");

    app.handle_gui_action(
        squallar_egui::actions::GuiAction::NavigateTime {
            pane_idx: 0,
            step_secs: -600,
        },
        None,
    );

    assert!(!app.gui.pane(0).unwrap().viewing_live);
    assert!(app.gui.fetching());
    assert!(app.manual_nav_pending);
    assert_eq!(app.render.fetch_generation_for("KTLX"), generation + 1);
    let expected = chrono::TimeZone::from_utc_datetime(&chrono::Local, &at(20)).naive_local();
    assert_eq!(
        app.gui.selected_timestamp(),
        expected,
        "the fetch target must be the pane's scan time stepped by the payload"
    );
}

/// `NavigateOneScan` spends a generation on the adjacent-scan lookup and marks the
/// navigation pending.
#[test]
fn navigate_one_scan_spends_a_generation_and_marks_pending() {
    let mut app = app_showing(at(30));
    let generation = app.render.fetch_generation_for("KTLX");

    app.handle_gui_action(
        squallar_egui::actions::GuiAction::NavigateOneScan {
            pane_idx: 0,
            forward: false,
        },
        None,
    );
    assert!(app.manual_nav_pending);
    assert!(app.gui.fetching());
    assert_eq!(app.render.fetch_generation_for("KTLX"), generation + 1);

    let mut bare = headless(TestBridge::desktop());
    let generation = bare.render.fetch_generation_for("KTLX");
    bare.handle_gui_action(
        squallar_egui::actions::GuiAction::NavigateOneScan {
            pane_idx: 0,
            forward: true,
        },
        None,
    );
    assert!(
        !bare.manual_nav_pending && bare.render.fetch_generation_for("KTLX") == generation,
        "a pane with no scan info must not spend a fetch on an adjacent-scan \
         lookup with no reference moment"
    );
}

/// The loop transport's per-frame payloads, acted on: toggle drives the phase state
/// machine, step wraps at both ends.
#[test]
fn the_loop_transport_payloads_drive_the_playback_state() {
    use squallar_egui::actions::GuiAction;
    use squallar_egui::pane::{LoopFrame, LoopPhase};

    let mut app = app_showing(at(10));
    let site = squallar_radar::sites::get_radar_site("KTLX").unwrap();
    {
        let mut state = squallar_egui::radar_layer::begin_loop(
            3600,
            site,
            squallar_radar::types::RenderView::PlanView,
        );
        state.phase = LoopPhase::Ready;
        state.frames = (0..3)
            .map(|i| LoopFrame {
                timestamp: at(i),
                image: None,
                render_in_flight: false,
                render_failed: false,
            })
            .collect();
        *app.gui.pane_mut(0).unwrap().time_state_mut(&known::RADAR) = state;
    }
    let phase = |app: &App| app.gui.pane(0).unwrap().time_state(&known::RADAR).phase;
    let frame = |app: &App| {
        app.gui
            .pane(0)
            .unwrap()
            .time_state(&known::RADAR)
            .current_frame()
    };

    app.handle_gui_action(GuiAction::ToggleLoopPlayback { pane_idx: 0 }, None);
    assert_eq!(phase(&app), LoopPhase::Playing, "Ready + toggle = Playing");
    app.handle_gui_action(GuiAction::ToggleLoopPlayback { pane_idx: 0 }, None);
    assert_eq!(phase(&app), LoopPhase::Paused, "Playing + toggle = Paused");

    app.handle_gui_action(
        GuiAction::StepLoopFrame {
            pane_idx: 0,
            forward: false,
        },
        None,
    );
    assert_eq!(frame(&app), 2, "backward from 0 wraps to the last frame");
    app.handle_gui_action(
        GuiAction::StepLoopFrame {
            pane_idx: 0,
            forward: true,
        },
        None,
    );
    assert_eq!(frame(&app), 0, "forward from the last frame wraps to 0");

    app.handle_gui_action(
        GuiAction::SeekLoopFrame {
            pane_idx: 0,
            frame_index: 1,
        },
        None,
    );
    assert_eq!(frame(&app), 1, "seek lands on the asked-for frame");
    app.handle_gui_action(
        GuiAction::SeekLoopFrame {
            pane_idx: 0,
            frame_index: 99,
        },
        None,
    );
    assert_eq!(frame(&app), 1, "an out-of-range seek changes nothing");
}

/// A volume whose flight runs from `first` to `last` — two sweeps, dated
/// minutes apart, as a real VCP-212 volume's are.
fn spanning_scan(first: u32, last: u32) -> nexrad_model::data::Scan {
    use nexrad_model::data::{
        ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus, Scan,
        Sweep, VolumeCoveragePattern, WaveformType,
    };
    let sweep = |number: u8, elevation: f32, minute: u32| {
        let radials = (0..8u16)
            .map(|i| {
                Radial::new(
                    at(minute).and_utc().timestamp_millis() + i64::from(i),
                    i + 1,
                    f32::from(i) * 45.0,
                    45.0,
                    RadialStatus::IntermediateRadialData,
                    number,
                    elevation,
                    Some(MomentData::from_fixed_point(
                        4,
                        2125,
                        250,
                        8,
                        2.0,
                        66.0,
                        vec![120, 140, 160, 180],
                    )),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        Sweep::new(number, radials)
    };
    let cut = |angle: f64| {
        ElevationCut::new(
            angle,
            ChannelConfiguration::ConstantPhase,
            WaveformType::CS,
            20.0,
            true,
            true,
            false,
            false,
            1,
            20,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            false,
            0,
            false,
            0,
            false,
            false,
        )
    };
    Scan::new(
        VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            PulseWidth::Short,
            false,
            0,
            false,
            0,
            false,
            false,
            0,
            false,
            false,
            vec![cut(0.5), cut(1.5)],
        ),
        vec![sweep(1, 0.5, first), sweep(2, 1.5, last)],
    )
}

fn volume_target(collected: chrono::NaiveDateTime) -> squallar_egui::pane::VolumeTarget {
    squallar_egui::pane::VolumeTarget {
        volume: squallar_egui::pane::VolumeStamp {
            site: "KTLX".to_owned(),
            collected,
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region: None,
    }
}

/// **A 3D pane navigated off live is served the volume it names.**
#[test]
fn a_navigated_3d_pane_is_served_the_volume_it_names() {
    let mut app = headless(TestBridge::desktop());
    app.volumes.install_base(
        "KTLX".to_string(),
        (Arc::new(spanning_scan(10, 14)), Default::default(), at(10)),
    );
    let newest = app
        .current_volume_stamp("KTLX")
        .expect("the fixture volume resolves a stamp")
        .newest;
    assert!(
        newest >= at(14),
        "precondition: the volume's newest data time must be the end of the \
         flight and not the key it is held under ({}), or the two source \
         decisions cannot be told apart — got {newest}",
        at(10),
    );

    let navigated = volume_target(at(10));
    app.handle_prepare_volume(0, &squallar_source::id::known::RADAR, navigated.clone());
    assert!(
        app.volume_store.lookup(&navigated).is_some(),
        "the pane named the volume it is showing and the host had it in hand, \
         and no build was reached — so a scrubbed 3D pane waits for ever",
    );

    let live = volume_target(newest);
    app.handle_prepare_volume(1, &squallar_source::id::known::RADAR, live.clone());
    assert!(
        app.volume_store.lookup(&live).is_some(),
        "the live target stopped being served",
    );

    let unheld = volume_target(at(30));
    app.handle_prepare_volume(2, &squallar_source::id::known::RADAR, unheld.clone());
    assert!(
        app.volume_store.lookup(&unheld).is_none(),
        "a volume the app does not hold was answered for, which stops the pane \
         ever asking again",
    );
}

/// **The request carries the budget this device resolved, not the one this
/// binary compiled.**
#[test]
fn the_requested_shape_is_the_budget_this_device_resolved() {
    use squallar_egui::pane::{VolumeStamp, VolumeTarget};

    let target = VolumeTarget {
        volume: VolumeStamp {
            site: "KTLX".to_owned(),
            collected: chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
                .expect("a real date")
                .and_hms_opt(22, 33, 0)
                .expect("a real time"),
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region: None,
    };

    let web = |two_d: u32, three_d: u32| {
        squallar_device_profile::budget::resolve(&squallar_device_profile::budget::DeviceProfile {
            limits: squallar_device_profile::budget::BudgetLimits::WASM,
            platform: squallar_device_profile::budget::Platform::Web,
            adapter: squallar_device_profile::budget::AdapterCeilings {
                max_texture_dimension_2d: two_d,
                max_texture_dimension_3d: three_d,
            },
            ..squallar_device_profile::budget::DeviceProfile::for_target()
        })
    };
    let phone = web(2048, 256);
    let desktop = web(
        squallar_device_profile::budget::DESKTOP_CLASS_REPORT.max_texture_dimension_2d,
        squallar_device_profile::budget::DESKTOP_CLASS_REPORT.max_texture_dimension_3d,
    );

    let cells_on = |budgets: squallar_device_profile::budget::Budgets, axis: u32| {
        shaped_request(&target, 35.33, -97.28, budgets.grid_cells, axis)
            .shape
            .cells()
    };
    assert!(
        cells_on(desktop, 8192) > cells_on(phone, 256),
        "a browser on desktop-class silicon is put on the wire asking for the \
         same grid as one at the spec floor",
    );

    let call = include_str!("../app.rs")
        .split_once("let ctx = volume_job_context(")
        .map(|(_, rest)| rest.split_once(");").expect("a call site").0)
        .expect("`volume_job_context` is still called from `prepare_volume`");
    assert!(
        call.contains("self.budgets.grid_cells"),
        "the production call site passes `{call}` — the resolved budget is the \
         only thing that carries a promotion this far",
    );
}

/// **The budgets are re-resolved before anything downstream reads them.**
#[test]
fn the_device_profile_is_folded_in_before_any_budget_is_spent() {
    let body = include_str!("../app.rs")
        .split_once("fn install_volume_bridge(&mut self)")
        .and_then(|(_, rest)| rest.split_once("\n    }"))
        .map(|(body, _)| body)
        .expect("`install_volume_bridge` is still a method on `App`");

    let update = body
        .find("self.update_device_profile(")
        .expect("`install_volume_bridge` no longer re-resolves the budgets");
    for spender in [
        "LoopPoolLimits::from_budgets",
        "self.budgets.quality_ceiling",
    ] {
        let at = body
            .find(spender)
            .unwrap_or_else(|| panic!("`{spender}` is no longer read there"));
        assert!(
            update < at,
            "`{spender}` is read before the adapter's own reading is folded in, \
             so that one budget stays at the floor while the rest are promoted",
        );
    }
}

/// One shared render in the dispatcher's cache — economy for a pressure event
/// to evict. A 64 px raster, so the byte figure the cache charges is non-zero
/// and the eviction is of something rather than of nothing.
fn seed_render_cache(app: &mut App) {
    use crate::render_dispatch::CachedRenderOutput;
    let key = crate::render_key::render_cache_key(
        "KTLX",
        &squallar_radar::fields::known::REFLECTIVITY,
        squallar_radar::types::RenderView::PlanView,
        0.5,
    );
    app.render.render_cache.insert(
        key,
        CachedRenderOutput {
            image: std::sync::Arc::new(egui::ColorImage::new(
                [64, 64],
                vec![egui::Color32::BLACK; 64 * 64],
            )),
            max_range_km: 230.0,
            hover: std::sync::Arc::new(squallar_radar::hover::HoverSource::empty()),
            nyquist_ms: None,
            melting_layer_source: None,
            storm_motion: None,
        },
    );
    assert_eq!(
        app.render.render_cache.entry_count(),
        1,
        "precondition: the fixture did not land in the cache"
    );
}

/// **A lost surface evicts economy, steps the whole budget set down one rung,
/// and writes nothing.** Nothing is learned across sessions: the rung lives in
/// the device profile's memo for this process, and the store never hears of it.
#[test]
fn a_lost_surface_evicts_economy_and_steps_down_and_writes_nothing() {
    use squallar_device_profile::quality::GradientShading;
    use squallar_kv::KvStore;

    let platform = TestBridge::desktop();
    let store = platform.store();
    let mut app = headless(platform);

    app.loop_pool = crate::loop_pool::LoopPool::for_promotion(
        squallar_device_profile::budget::Promotion::Ceiling,
        None,
        crate::loop_pool::LoopPoolLimits::from_budgets(&app.budgets),
    );
    seed_render_cache(&mut app);
    let before = app.budgets;
    let pool_before = app.loop_pool.bytes();
    assert_eq!(
        before.quality_ceiling.shading,
        GradientShading::On,
        "precondition: this build's desktop bracket starts at the cloud rung",
    );

    app.on_pressure(crate::pressure::Pressure::SurfaceLost);

    assert_eq!(
        app.render.render_cache.entry_count(),
        0,
        "economy survived a pressure event: the render cache still holds its entry",
    );
    assert_eq!(app.render.extract_cache_len(), 0);
    assert_eq!(
        app.budgets.quality_ceiling.shading,
        GradientShading::Off,
        "a lost surface left the most expensive rung in the application in \
         place, so the next one costs the same and the ladder never runs",
    );
    assert_eq!(
        app.budgets.offscreen_bytes, before.offscreen_bytes,
        "the first rung took two things at once, so a device that only needed \
         to give up its lighting also lost its resolution",
    );
    assert!(
        app.loop_pool.bytes() < pool_before,
        "the pool did not halve beside it",
    );
    assert_eq!(app.budgets.steps_back, 1);
    // Nothing is learned across sessions (user ruling, 2026-09-01): the rung
    // is this process's alone, and the store has no entry to carry it.
    assert_eq!(
        store.load(crate::budget_memo::BUDGET_MEMO_KEY),
        None,
        "the ladder position was written to the store",
    );
    assert_eq!(
        store.load(crate::loop_pool::LOOP_POOL_KEY),
        None,
        "the pool size was written to the store",
    );
}

/// **A reopen starts at the ladder top whatever the store holds.** A stale
/// entry from an older install is ignored, not honoured — and not deleted
/// either, since the store cannot.
///
/// The seeded pool size sits strictly between the desktop floor and ceiling,
/// so honouring it would be visible: a value at or under the floor would be
/// held to the floor and read exactly like the class pick.
#[test]
fn a_reopen_starts_at_the_ladder_top_whatever_the_store_holds() {
    use squallar_device_profile::quality::GradientShading;
    use squallar_kv::KvStore;

    let platform = TestBridge::desktop();
    let store = platform.store();
    store
        .store(crate::budget_memo::BUDGET_MEMO_KEY, "2")
        .expect("the memory store cannot fail");
    store
        .store(crate::loop_pool::LOOP_POOL_KEY, "1024")
        .expect("the memory store cannot fail");
    let limits =
        crate::loop_pool::LoopPoolLimits::from_budgets(&squallar_device_profile::budget::resolve(
            &squallar_device_profile::budget::DeviceProfile::for_target(),
        ));
    let seeded = 1024 * 1024 * 1024;
    assert!(
        limits.floor < seeded && seeded < limits.ceiling,
        "the fixture's stale size ({seeded}) is not strictly inside the pool \
         limits ({limits:?}), so honouring it would be invisible here",
    );

    let reopened = headless(platform);
    let fresh = headless(TestBridge::desktop());

    assert_eq!(
        reopened.budgets.steps_back, 0,
        "a stale ladder position in the store was honoured",
    );
    assert_eq!(reopened.budgets, fresh.budgets);
    assert_eq!(
        reopened.budgets.quality_ceiling.shading,
        GradientShading::On,
    );
    assert_eq!(
        reopened.loop_pool, fresh.loop_pool,
        "a stale pool size in the store was honoured",
    );
    assert_ne!(reopened.loop_pool.bytes(), seeded);
    // Ignored is not deleted: the entries are still there, and harmless.
    assert_eq!(
        store.load(crate::budget_memo::BUDGET_MEMO_KEY).as_deref(),
        Some("2"),
    );
    assert_eq!(
        store.load(crate::loop_pool::LOOP_POOL_KEY).as_deref(),
        Some("1024"),
    );
}

/// **A machine that keeps failing settles rather than counting for ever**,
/// and never writes.
#[test]
fn the_ladder_position_stops_rising_once_every_rung_is_at_its_stop() {
    use squallar_kv::KvStore;

    let platform = TestBridge::desktop();
    let store = platform.store();
    let mut app = headless(platform);

    for _ in 0..12 {
        app.on_pressure(crate::pressure::Pressure::SurfaceLost);
    }
    let settled = app.budgets.steps_back;
    assert!(
        settled > 0 && settled < 12,
        "twelve lost surfaces resolved to rung {settled}, which is either no \
         ladder at all or a failure counter wearing one as a hat",
    );
    assert_eq!(
        store.load(crate::budget_memo::BUDGET_MEMO_KEY),
        None,
        "twelve pressure events, and one of them wrote the store",
    );
    let shipped = squallar_device_profile::budget::BudgetLimits::for_target();
    assert_eq!(app.budgets.grid_cells, shipped.grid_cells.floor);
    assert_eq!(app.budgets.offscreen_bytes, shipped.offscreen_bytes.floor);
    assert_eq!(
        app.budgets.raster_side_ceiling_px,
        shipped.long_range_image_side_px.floor,
    );
}

/// **An out-of-memory error, however many times the device raised it in one
/// frame, is one rung on that frame — and writes nothing.**
///
/// The counter is process-global; this is the only test in this binary that
/// notes into it or takes from it, and the frame path that takes it never
/// runs headless.
#[test]
fn an_out_of_memory_error_steps_the_ladder_once_per_frame_and_writes_nothing() {
    use squallar_kv::KvStore;

    let platform = TestBridge::desktop();
    let store = platform.store();
    let mut app = headless(platform);
    assert_eq!(app.budgets.steps_back, 0);

    squallar_gpu::pressure::note_out_of_memory();
    squallar_gpu::pressure::note_out_of_memory();
    app.absorb_gpu_pressure();
    assert_eq!(
        app.budgets.steps_back, 1,
        "two errors on one frame are one rung, not two",
    );

    // The next frame, with nothing new noted: the ladder holds.
    app.absorb_gpu_pressure();
    assert_eq!(app.budgets.steps_back, 1);

    assert_eq!(store.load(crate::budget_memo::BUDGET_MEMO_KEY), None);
    assert_eq!(store.load(crate::loop_pool::LOOP_POOL_KEY), None);
}

/// **A platform memory warning evicts economy and steps down**, and what it
/// evicts leaves through the deferred-drop path rather than on this thread.
///
/// `memory_warning` itself is one line handing `Pressure::MemoryWarning` to
/// the handler; it takes an `ActiveEventLoop` no host test can build, so the
/// handler is driven directly and the wiring is read from the source.
#[test]
fn a_memory_warning_evicts_economy_and_steps_down() {
    let mut app = headless(TestBridge::desktop());
    seed_render_cache(&mut app);
    let before = app.budgets.steps_back;

    app.on_pressure(crate::pressure::Pressure::MemoryWarning);

    assert_eq!(
        app.render.render_cache.entry_count(),
        0,
        "the render cache survived a memory warning",
    );
    assert_eq!(app.budgets.steps_back, before + 1);

    let handler = include_str!("../app.rs")
        .split_once("fn memory_warning(")
        .map(|(_, rest)| rest.split_once("\n    }").expect("a body").0)
        .expect("`memory_warning` is implemented on `App`");
    assert!(
        handler.contains("Pressure::MemoryWarning"),
        "the platform's memory warning no longer reaches the pressure handler: {handler}",
    );
    let body = include_str!("../app_render.rs")
        .split_once("fn on_pressure(")
        .map(|(_, rest)| rest.split_once("\n    }").expect("a body").0)
        .expect("`on_pressure` is implemented on `App`");
    assert_eq!(
        body.matches("squallar_worker::offload::discard_each(")
            .count(),
        2,
        "an evicted cache is freed on the frame thread rather than handed to \
         the deferred-drop path: {body}",
    );
    assert!(
        body.contains("self.evict_unneeded_loop_scans();"),
        "the loop caches' sweep is no longer part of the pressure answer: {body}",
    );
}

/// **The liveness entry is rebuilt when it CHANGES, not once a frame.**
///
/// WO-E8c's payload is an `Arc` the shell re-states every frame, and the map
/// inside it is a per-site `HashMap` the frame path recomputes anyway. A
/// per-frame rebuild would be a per-frame allocation and a per-frame clone for
/// a value that moves on the order of once a scan, so the shell compares
/// before it publishes.
///
/// The instrument is pointer identity: same `Arc`, no rebuild. It has a
/// **non-triviality floor** — the second act moves the volume and asserts the
/// pointer *did* change, so a build that never published at all could not pass
/// the first half by publishing nothing.
#[test]
fn an_unchanged_liveness_answer_is_restated_and_not_rebuilt() {
    fn entry(app: &App) -> std::sync::Arc<dyn std::any::Any + Send + Sync> {
        app.gui
            .liveness()
            .iter()
            .find(|e| e.id == squallar_egui::radar_layer::POLL_LAYER)
            .expect("the radar layer publishes its liveness")
            .payload
            .clone()
    }

    let mut app = app_showing(at(10));
    app.chunk_feeds.ensure("KTLX");
    send_archive_scan(&app, at(5), stamped_scan(5));
    app.poll_data_channels();
    app.push_frame_inputs();
    let first = entry(&app);

    // A frame that changes nothing.
    app.poll_data_channels();
    app.push_frame_inputs();
    assert!(
        std::sync::Arc::ptr_eq(&first, &entry(&app)),
        "the shell rebuilt the radar layer's liveness payload on a frame \
         where nothing about it moved",
    );

    // …and one that changes something.
    send_archive_scan(&app, at(15), stamped_scan(15));
    app.poll_data_channels();
    app.push_frame_inputs();
    let moved = entry(&app);
    assert!(
        !std::sync::Arc::ptr_eq(&first, &moved),
        "a newer volume did not reach the seam — the comparison above would \
         pass on a build that never published at all",
    );
    assert_eq!(
        squallar_egui::radar_layer::current_volume_for(app.gui.liveness(), "KTLX")
            .expect("a stamp")
            .newest,
        at(15),
        "the rebuilt payload does not carry the newer volume",
    );
}

/// **The chunk feed's status reaches the chip that prints it.**
///
/// Registered as a gap this land found rather than one it made: WO-E8c's
/// tamper — the shell never publishing the status at all — came back **green**
/// on the whole workspace. The sentinel-expression contract test drives
/// `FrameInputs` by hand and so pins the `Gui`'s half; nothing pinned the
/// App's, so `drive_chunk_feeds` could compute a status and drop it on the
/// floor with every gate still reading green.
///
/// The precondition is the non-triviality floor: the resting answer is `false`
/// on both halves, so the assertion below cannot pass on a default.
#[test]
fn the_chunk_feeds_status_reaches_the_seam_that_publishes_it() {
    let mut app = app_showing(at(10));
    app.drive_chunk_feeds();
    app.push_frame_inputs();
    let resting = squallar_egui::radar_layer::chunk_status(app.gui.liveness());
    assert!(
        !resting.feeding,
        "precondition: nothing is feeding yet, or the assertion below could \
         pass off a status that was already true; got {resting:?}",
    );
    assert!(
        resting.interval_secs > 0,
        "the shell published `ChunkFeedStatus::default()` rather than the \
         status it computed — the feed's own cadence is never zero; got \
         {resting:?}",
    );

    app.chunk_feeds.ensure("KTLX");
    app.chunk_feeds
        .force_serving("KTLX", Arc::new(empty_scan()));
    app.drive_chunk_feeds();
    app.push_frame_inputs();

    let live = squallar_egui::radar_layer::chunk_status(app.gui.liveness());
    assert!(
        live.feeding,
        "the feed is serving and the status bar would still say it is not: \
         the shell computed a status and never published it; got {live:?}",
    );
}

// ── The job-shaping seam (WO-M14b-2) ────────────────────────────────────

/// **A volume-capable layer that answers "I cannot shape this".**
///
/// It publishes radar's own registered rows, so the resolution's field gate
/// passes and this fixture varies exactly one thing: what `volume_job`
/// answers. That is the arm this build's ONE implementor cannot reach —
/// radar refuses an unregistered field earlier, at the extraction gate — and
/// it is the arm a second implementor will reach first.
struct RefusingVolumeLayer;

const REFUSING_LAYER: squallar_source::id::LayerId =
    squallar_source::id::LayerId::from_static("test.refuses-to-shape");

impl squallar_source::volume::VolumeCapable for RefusingVolumeLayer {
    fn volume_job(
        &self,
        _ctx: squallar_source::volume::VolumeJobContext,
    ) -> Option<squallar_source::job::DescribedJob> {
        None
    }
}

impl squallar_overlays::render::overlay_state::OverlayHandler for RefusingVolumeLayer {
    fn id(&self) -> squallar_source::id::LayerId {
        REFUSING_LAYER
    }
    /// It holds neither frames nor items with windows — it exists to refuse a
    /// job — so `Live` is the true answer and not merely the inherited one.
    fn time_axis(&self) -> squallar_source::time::TimeAxis {
        squallar_source::time::TimeAxis::Live
    }
    fn surface(&self) -> squallar_source::handler::Surface {
        squallar_source::handler::Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        0
    }
    fn display_name(&self) -> &str {
        "Refusing"
    }
    fn render_mode(&self) -> squallar_source::handler::RenderMode {
        squallar_source::handler::RenderMode::Texture
    }
    fn data_generation(&self) -> u64 {
        0
    }
    fn has_data(&self, _pane: &squallar_source::handler::PaneRef<'_>) -> bool {
        true
    }
    fn is_fetching(&self) -> bool {
        false
    }
    fn set_fetching(&mut self, _fetching: bool, _pane: &squallar_source::handler::PaneRef<'_>) {}
    fn fetch_time(&self) -> Option<web_time::Instant> {
        None
    }
    fn apply_fetch_result(
        &mut self,
        _result: squallar_source::handler::FetchPayload,
        _pane: &squallar_source::handler::PaneRef<'_>,
    ) {
    }
    fn retain_selections(
        &self,
        _selections: &mut Vec<std::sync::Arc<dyn squallar_source::handler::OverlayItem>>,
        _pane: &squallar_source::handler::PaneRef<'_>,
    ) {
    }
    fn products(&self) -> &'static [squallar_source::product::ProductSpec] {
        squallar_radar::fields::products()
    }
    fn volume(&self) -> Option<&dyn squallar_source::volume::VolumeCapable> {
        Some(self)
    }
}

/// **A layer that cannot shape the job is refused into the store, not left
/// pending.**
///
/// The seam's whole point is that the frontend does not know how a volume is
/// built — so it cannot know in advance that a build is impossible, and the
/// only honest answer to `None` is a refusal the pane can show. Left as
/// `Waiting` instead, the level-trigger would re-ask every frame and pay the
/// extraction every time, for ever.
///
/// **The preconditions are asserted, not assumed**: the ask must get past the
/// budget gate and past extraction, or this would be green over an arm it
/// never reached.
#[test]
fn a_capable_layer_that_cannot_shape_a_job_is_refused_rather_than_left_pending() {
    let target = squallar_egui::pane::VolumeTarget {
        volume: squallar_egui::pane::VolumeStamp {
            site: "KTLX".to_owned(),
            collected: at(10),
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region: None,
    };
    let mut app = headless(TestBridge::desktop());
    let mut handlers = squallar_egui::sources::all();
    handlers.push(Box::new(RefusingVolumeLayer));
    app.gui.overlays =
        squallar_overlays::render::overlay_state::OverlayRegistry::with_handlers(handlers);
    app.volumes.install_base(
        "KTLX".to_string(),
        (Arc::new(stamped_scan(10)), Default::default(), at(10)),
    );

    app.handle_prepare_volume(0, &REFUSING_LAYER, target.clone());

    assert_eq!(
        app.volume_extractions.get(),
        1,
        "precondition: the ask must have got past the budget gate and paid \
         the extraction, or the refusal below is about some earlier gate",
    );
    let entry = app
        .volume_store
        .lookup(&target)
        .expect("a layer that cannot shape a job must still answer into the store")
        .entry
        .clone();
    let squallar_volumetric::bridge::VolumeEntry::Refused(why) = entry else {
        panic!("the store answered with a build rather than the refusal the layer gave");
    };
    assert!(
        why.contains(REFUSING_LAYER.as_str()),
        "the refusal must name the layer that could not build it; got {why:?}",
    );

    // The level-trigger is quiesced: a second ask attaches to the refusal
    // rather than paying the walk again.
    app.handle_prepare_volume(0, &REFUSING_LAYER, target.clone());
    assert_eq!(
        app.volume_extractions.get(),
        1,
        "the refusal did not quiesce the level-trigger: the pane re-extracts \
         a multi-ms merged volume every frame for a build that can never \
         happen",
    );
}

/// **An asymmetric reach survives the seam with its axes in place.**
///
/// The reach crosses as a bare pair — `squallar-source` cannot name
/// `HalfExtentKm` — so it is taken apart on this side and put back together
/// on radar's. **Two swaps would cancel**, and every other region fixture in
/// this workspace is square, where neither swap is visible; this drives both
/// halves of the round trip with `east != north`.
#[test]
fn an_asymmetric_region_survives_the_seam_with_its_axes_in_place() {
    use squallar_egui::pane::{VolumeRegion, VolumeStamp, VolumeTarget};
    use squallar_geo::GeoPoint;

    let picked = VolumeRegion::new(
        GeoPoint {
            lat: 36.1,
            lon: -98.4,
        },
        squallar_radar::voxel::HalfExtentKm {
            east_km: 12.5,
            north_km: 47.5,
        },
    )
    .expect("a valid region");
    let target = VolumeTarget {
        volume: VolumeStamp {
            site: "KTLX".to_owned(),
            collected: at(10),
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region: Some(picked),
    };
    let request = shaped_request(&target, 35.33, -97.28, SHIPPED_CELLS, DEVICE_AXIS);
    assert_eq!(
        request.half_extent_km,
        Some(squallar_radar::voxel::HalfExtentKm {
            east_km: 12.5,
            north_km: 47.5,
        }),
        "a swap on either side alone gives (47.5, 12.5); a swap on BOTH gives \
         this. MEASURED, not argued: with both sides swapped this test is \
         green and radar's own \
         `the_reach_that_crosses_the_seam_keeps_east_east_and_north_north` is \
         what fires. This pin is not sufficient on its own.",
    );
}

/// **The dispatch side names no voxel job type.**
///
/// Since WO-M14b-2 the layer hands back an envelope already shaped, and this
/// side owns the render slot, the run envelope and the reply — nothing about
/// what is being resampled. The zero is checked against a presence control on
/// the SAME scrape: the file still names other radar job rows, which are other
/// orders' business, so a zero here is a moved type rather than a needle that
/// rotted or a file the walk never read.
#[test]
fn the_voxel_dispatch_names_no_voxel_job_type() {
    let dispatch = include_str!("../render_dispatch.rs");
    assert!(
        dispatch.contains("fn spawn_voxel_build"),
        "control: the scrape is not reading the dispatch it exists to guard",
    );
    let others = dispatch.matches("squallar_radar::jobs::").count();
    assert!(
        others > 0,
        "control: the file names no radar job row at all, so the zero below \
         would be about a haystack that moved rather than about the voxel job",
    );
    assert_eq!(
        dispatch.matches("VoxelJob").count(),
        0,
        "the voxel job type is named in the dispatch again. Shaping the job \
         is the answering layer's — `VolumeCapable::volume_job` — and this \
         side receives it as a `DescribedJob` it cannot look inside.",
    );
    assert_eq!(
        dispatch.matches("VoxelRequest").count(),
        0,
        "the voxel request type is named in the dispatch again; see above.",
    );
}

/// **The field that crosses the seam is the target's own**, not a constant.
///
/// The ask names a field, the walk matched it against the answering layer's
/// own rows, and the handover has to carry that one rather than whatever the
/// 3D view happens to open on. A build of the wrong moment looks entirely
/// plausible on screen.
#[test]
fn the_field_the_seam_carries_is_the_targets_own() {
    for (id, product) in [
        (
            squallar_radar::fields::known::VELOCITY,
            squallar_radar::types::RadarProduct::Velocity,
        ),
        (
            squallar_radar::fields::known::CORRELATION_COEFFICIENT,
            squallar_radar::types::RadarProduct::CorrelationCoefficient,
        ),
    ] {
        let target = squallar_egui::pane::VolumeTarget {
            volume: squallar_egui::pane::VolumeStamp {
                site: "KTLX".to_owned(),
                collected: at(10),
            },
            product: id,
            region: None,
        };
        assert_eq!(
            shaped_request(&target, 35.33, -97.28, SHIPPED_CELLS, DEVICE_AXIS).product,
            product,
            "the handover carried a moment other than the one the target names",
        );
    }
}

/// **The volume the frontend extracted is what the layer is handed** — and the
/// proof is that the real handler accepts it.
///
/// The payload crosses as `dyn Any`, so nothing about the handover is checked
/// by the compiler: a stand-in, a stale buffer or another layer's type would
/// all compile, and every one of them would come back as a refusal that reads
/// exactly like a build the app never started. This drives the whole
/// production path — seed, extract, hand over, shape, dispatch — and asserts
/// the store is left BUILDING.
#[test]
fn the_volume_the_frontend_extracted_is_what_the_layer_is_handed() {
    let target = squallar_egui::pane::VolumeTarget {
        volume: squallar_egui::pane::VolumeStamp {
            site: "KTLX".to_owned(),
            collected: at(10),
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region: None,
    };
    let mut app = headless(TestBridge::desktop());
    app.volumes.install_base(
        "KTLX".to_string(),
        (Arc::new(stamped_scan(10)), Default::default(), at(10)),
    );

    app.handle_prepare_volume(0, &squallar_source::id::known::RADAR, target.clone());

    assert_eq!(
        app.volume_extractions.get(),
        1,
        "precondition: the extraction must have happened, or what follows is \
         about an ask that never got that far",
    );
    let entry = app
        .volume_store
        .lookup(&target)
        .expect("the ask must have been answered into the store")
        .entry
        .clone();
    assert!(
        matches!(entry, squallar_volumetric::bridge::VolumeEntry::Building),
        "the layer did not accept the payload the frontend extracted, so the \
         pane is showing a refusal where a build belongs",
    );
}

/// One telemetry tick, asked for rather than waited on: the 2 s cadence is a
/// clock `telemetry_is_due` reads, and clearing it is what makes the next
/// report due — the same reason that predicate takes both instants.
fn tick(app: &mut App) {
    app.frame_telemetry_said = None;
    app.report_frame_telemetry();
}

const MIB: u64 = 1 << 20;

/// A heap reading with the worker instance at `worker_mib` and the page well
/// under every line, so what is judged is the fuller of the two.
fn worker_heap(worker_mib: u64) -> crate::platform::LinearMemory {
    crate::platform::LinearMemory {
        page_bytes: 100 * MIB,
        worker_bytes: Some(worker_mib * MIB),
    }
}

/// **The heap watermark at the action line evicts economy and steps the ladder
/// down once**, on the telemetry tick, from a reading rather than a failure —
/// and a second tick at the same reading does nothing more, because a wasm
/// heap that has acted once would otherwise act on every tick for the rest of
/// the session. Nothing is written to the store.
#[test]
fn a_heap_watermark_at_the_act_line_evicts_economy_and_steps_down_once() {
    use squallar_kv::KvStore;

    let platform = TestBridge::web().with_linear_memory(worker_heap(891));
    let store = platform.store();
    let mut app = headless(platform);
    seed_render_cache(&mut app);
    assert_eq!(app.budgets.steps_back, 0);

    tick(&mut app);

    assert_eq!(
        app.budgets.steps_back, 1,
        "a reading at the action line did not step the ladder",
    );
    assert_eq!(
        app.render.render_cache.entry_count(),
        0,
        "economy survived the watermark: the render cache still holds its entry",
    );
    assert_eq!(app.linear_memory_watch.last_acted_at(), Some(891 * MIB));

    seed_render_cache(&mut app);
    tick(&mut app);
    assert_eq!(
        app.budgets.steps_back, 1,
        "the same reading on the next tick stepped the ladder again",
    );
    assert_eq!(
        app.render.render_cache.entry_count(),
        1,
        "the same reading on the next tick evicted again",
    );

    assert_eq!(store.load(crate::budget_memo::BUDGET_MEMO_KEY), None);
    assert_eq!(store.load(crate::loop_pool::LOOP_POOL_KEY), None);
}

/// **A heap that grows past the refire step acts again**; one that grows less
/// does not. The page instance is the fuller one here, so both arms of the
/// `max(page, worker)` are exercised across this test and the one above.
#[test]
fn a_heap_that_grows_past_the_refire_step_acts_again() {
    use squallar_device_profile::linear_memory::LINEAR_MEMORY_REFIRE_STEP_BYTES;
    use squallar_kv::KvStore;

    let platform = TestBridge::web();
    let gauge = platform.linear_memory_gauge();
    let store = platform.store();
    let page = |bytes: u64| {
        Some(crate::platform::LinearMemory {
            page_bytes: bytes,
            worker_bytes: Some(50 * MIB),
        })
    };
    gauge.set(page(891 * MIB));
    let mut app = headless(platform);

    tick(&mut app);
    assert_eq!(app.budgets.steps_back, 1);

    gauge.set(page(891 * MIB + LINEAR_MEMORY_REFIRE_STEP_BYTES - 1));
    tick(&mut app);
    assert_eq!(
        app.budgets.steps_back, 1,
        "growth short of the refire step acted again",
    );

    gauge.set(page(891 * MIB + LINEAR_MEMORY_REFIRE_STEP_BYTES));
    tick(&mut app);
    assert_eq!(
        app.budgets.steps_back, 2,
        "growth past the refire step did not act again",
    );
    assert_eq!(
        app.linear_memory_watch.last_acted_at(),
        Some(891 * MIB + LINEAR_MEMORY_REFIRE_STEP_BYTES),
    );
    assert_eq!(store.load(crate::budget_memo::BUDGET_MEMO_KEY), None);
}

/// **A native profile has no heap reading and is never pressured by the
/// tick.** The bridge answers the platform question — `None` — and the
/// watermark is never consulted, however many ticks pass.
#[test]
fn a_native_profile_with_no_heap_reading_is_never_pressured_by_the_tick() {
    use squallar_kv::KvStore;

    let platform = TestBridge::desktop();
    let store = platform.store();
    let mut app = headless(platform);
    assert_eq!(app.platform.linear_memory(), None, "precondition");
    seed_render_cache(&mut app);

    for _ in 0..10 {
        tick(&mut app);
    }

    assert_eq!(app.budgets.steps_back, 0);
    assert_eq!(app.render.render_cache.entry_count(), 1);
    assert_eq!(
        app.linear_memory_watch,
        crate::pressure::LinearMemoryWatch::default(),
        "the watermark moved on a bridge that reads no heap",
    );
    assert_eq!(store.load(crate::budget_memo::BUDGET_MEMO_KEY), None);
}

/// **A reading at the warning line is noted and steps nothing.** The line is
/// said once per crossing — `the_warn_line_is_said_once_per_crossing` holds
/// the once — and this holds that the tick reaches the watch and stops there.
#[test]
fn a_heap_at_the_warn_line_is_noted_and_steps_nothing() {
    use squallar_kv::KvStore;

    let platform = TestBridge::web().with_linear_memory(worker_heap(800));
    let store = platform.store();
    let mut app = headless(platform);
    seed_render_cache(&mut app);
    assert!(!app.linear_memory_watch.has_warned());

    tick(&mut app);
    tick(&mut app);

    assert!(
        app.linear_memory_watch.has_warned(),
        "a reading past the warning line was not noted",
    );
    assert_eq!(app.linear_memory_watch.last_acted_at(), None);
    assert_eq!(app.budgets.steps_back, 0, "a warning stepped the ladder");
    assert_eq!(app.render.render_cache.entry_count(), 1);
    assert_eq!(store.load(crate::budget_memo::BUDGET_MEMO_KEY), None);
}
