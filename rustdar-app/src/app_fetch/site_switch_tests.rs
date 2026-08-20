use super::*;
use crate::app::tests::{empty_scan, headless, two_pane_app};
use crate::platform_double::TestBridge;
use rustdar_radar::types::ScanInfo;

/// The WSR-88D a pane is on before every switch below.
const WSR88D: &str = "KPBZ";
/// Pittsburgh's terminal radar — the TDWR that shares the metro with `KPBZ`,
/// and the site the Level III and dual-pol gates were measured against.
const TDWR: &str = "TPIT";

fn at(minute: u32) -> NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 11)
        .unwrap()
        .and_hms_opt(18, minute, 0)
        .unwrap()
}

/// What a WSR-88D pane holds once its volume has loaded.
fn wsr88d_scan_info() -> ScanInfo {
    let mut info = ScanInfo::from_scan(&empty_scan(), WSR88D, at(0), None);
    for product in [
        RadarProduct::DifferentialReflectivity,
        RadarProduct::CorrelationCoefficient,
        RadarProduct::DifferentialPhase,
        RadarProduct::HydrometeorClassification,
    ] {
        info.available_products.push(product);
        info.product_elevations
            .insert(product, vec![0.5, 1.5, 2.4, 3.4]);
    }
    info.available_products.sort_by_key(|p| p.sort_order());
    info
}

/// A partial volume's worth of `TPIT`: the cuts a live feed has sealed so far.
fn tdwr_chunk_scan_info(products: &[(RadarProduct, &[f32])], minute: u32) -> ScanInfo {
    crate::test_sites::install();
    ScanInfo {
        site: rustdar_radar::sites::get_radar_site(TDWR)
            .expect("TPIT is in the resolved site table")
            .clone(),
        site_source: rustdar_radar::site_position::SitePositionSource::Table,
        site_position: None,
        timestamp: at(minute),
        vcp_number: 80,
        available_products: products.iter().map(|(p, _)| *p).collect(),
        product_elevations: products
            .iter()
            .map(|(p, angles)| (*p, angles.to_vec()))
            .collect(),
        status: format!("minute {minute}"),
    }
}

/// A section pane on `WSR88D` with a line, a cut on screen, and the key saying
/// which radar's volume that cut came from.
fn section_pane_showing_the_wsr88ds_cut(app: &mut crate::app::App) {
    use rustdar_radar::sampler::SampleStatus;
    use rustdar_radar::xsect::{CrossSection, SECTION_HEIGHT, SECTION_WIDTH, SectionAxes};

    let line = rustdar_egui::pane::SectionLine::new(
        rustdar_geo::GeoPoint {
            lat: 40.4,
            lon: -80.2,
        },
        rustdar_geo::GeoPoint {
            lat: 40.6,
            lon: -79.9,
        },
    )
    .expect("a fixture line must be finite and have two distinct ends");
    let pixels = SECTION_WIDTH * SECTION_HEIGHT;
    let cut = CrossSection::from_parts(
        vec![0u8; pixels * 4],
        vec![f32::NAN; pixels],
        vec![SampleStatus::NoCoverage.wire_code(); pixels],
        SectionAxes {
            length_km: 100.0,
            base_km_msl: 0.4,
            top_km_msl: 20.4,
            near_ground_range_km: 10.0,
            far_ground_range_km: 110.0,
            coverage_ground_range_km: 0.0,
            cone_of_silence_km: 0.0,
            tilt_count: 1,
            widest_tilt_gap_deg: 0.0,
            top_tilt_deg: 0.5,
            top_declared_cut_deg: 19.5,
        },
        vec![0.5],
        vec![0],
    )
    .expect("a full-size, all-NoCoverage section is well formed");

    let pane = app.gui.pane_mut(0).expect("a fresh Gui has one pane");
    pane.set_site(WSR88D.to_string());
    pane.set_kind(rustdar_egui::pane::PaneKind::CrossSection);
    let product = pane.selected_product();
    let xsect = pane.cross_section_mut().expect("just converted");
    xsect.line = Some(line);
    xsect.section = Some(std::sync::Arc::new(cut));
    xsect.rendered_for = Some(rustdar_egui::pane::SectionTarget {
        volume: rustdar_egui::pane::VolumeStamp {
            site: WSR88D.to_string(),
            collected: at(0),
        },
        product,
        line,
        ladder: 0,
    });
}

fn pane_on(app: &mut crate::app::App, site: &str, info: Option<ScanInfo>) {
    let pane = app.gui.pane_mut(0).expect("a fresh Gui has one pane");
    pane.set_site(site.to_string());
    pane.scan_info = info;
}

fn offered(app: &crate::app::App) -> Vec<RadarProduct> {
    app.gui
        .pane(0)
        .and_then(|p| p.scan_info.as_ref())
        .map(|info| info.available_products.clone())
        .unwrap_or_default()
}

fn switch_to(app: &mut crate::app::App, site: &str) {
    app.handle_gui_action(
        GuiAction::SwitchRadarSite {
            site: site.to_string(),
            pane_idx: 0,
        },
        None,
    );
}

/// The staleness this closes: a pane keeps offering the radar it just left.
#[test]
fn switching_to_a_tdwr_stops_offering_the_wsr88ds_products() {
    let mut app = headless(TestBridge::desktop());
    pane_on(&mut app, WSR88D, Some(wsr88d_scan_info()));

    let before = offered(&app);
    assert!(
        before.contains(&RadarProduct::EchoTops)
            && before.contains(&RadarProduct::DifferentialReflectivity),
        "precondition: the pane must be offering the products a TDWR cannot, \
         or this asserts nothing; it offered {before:?}",
    );

    switch_to(&mut app, TDWR);

    assert_eq!(
        app.gui.pane(0).unwrap().site(),
        TDWR,
        "precondition: the switch did not move the pane",
    );
    for product in [
        RadarProduct::EchoTops,
        RadarProduct::VerticallyIntegratedLiquid,
        RadarProduct::VilDensity,
        RadarProduct::SpecificDifferentialPhase,
        RadarProduct::PrecipitationRate,
        RadarProduct::HydrometeorClassification,
        RadarProduct::DifferentialReflectivity,
        RadarProduct::CorrelationCoefficient,
        RadarProduct::DifferentialPhase,
    ] {
        assert!(
            !offered(&app).contains(&product),
            "{TDWR} is offering {}, which is the WSR-88D's list still standing \
             under a site that cannot produce it",
            product.name(),
        );
    }
}

/// The tilt picker travels with the product list, and for the same reason: the angles
/// in a `ScanInfo` are the previous site's VCP.
#[test]
fn switching_sites_drops_the_previous_vcps_tilts() {
    let mut app = headless(TestBridge::desktop());
    pane_on(&mut app, WSR88D, Some(wsr88d_scan_info()));

    switch_to(&mut app, TDWR);

    assert!(
        app.gui.pane(0).unwrap().scan_info.is_none(),
        "the pane still holds a tilt ladder measured by another radar",
    );
    assert_eq!(
        app.gui.get_rendering_params_for_pane(0),
        None,
        "the pane resolves rendering params from the old site's angles, which \
         is what dispatches a render off the wrong volume and files it under \
         the new site's cache key",
    );
}

/// What the status bar says about a pane with nothing on it.
#[test]
fn switching_sites_stops_dating_the_previous_sites_volume() {
    let mut app = headless(TestBridge::desktop());
    pane_on(&mut app, WSR88D, Some(wsr88d_scan_info()));
    app.gui.pane_mut(0).unwrap().data_time = Some(at(0));

    switch_to(&mut app, TDWR);

    assert_eq!(
        app.gui.pane(0).unwrap().data_time,
        None,
        "the pane is captioned with the age of a volume it no longer draws",
    );
}

/// A section pane stops showing the radar it just left, and keeps its line.
#[test]
fn switching_sites_stops_showing_the_previous_radars_cut() {
    let mut app = headless(TestBridge::desktop());
    section_pane_showing_the_wsr88ds_cut(&mut app);
    app.gui.pane_mut(0).unwrap().scan_info = Some(wsr88d_scan_info());
    let drawn_line = app.gui.pane(0).unwrap().cross_section().unwrap().line;
    assert!(
        drawn_line.is_some(),
        "precondition: the fixture must have aimed the section",
    );

    switch_to(&mut app, TDWR);

    let xsect = app
        .gui
        .pane(0)
        .unwrap()
        .cross_section()
        .expect("the switch must not have changed the pane's kind");
    assert!(
        xsect.section.is_none(),
        "the pane is still showing {WSR88D}'s cut with {TDWR} on its pills, and \
         a hover still reads {WSR88D}'s values out of it",
    );
    assert!(
        xsect.texture.is_none(),
        "the raster of a cut that no longer exists is still uploaded, and \
         `restore_section_textures` would put it back on the next surface loss",
    );
    assert_eq!(
        xsect.rendered_for, None,
        "the pane still names the volume it was cut for, so the first target \
         built on the new site is compared against another radar's key",
    );
    assert_eq!(
        xsect.unavailable,
        Some(rustdar_egui::pane::SectionUnavailable::AwaitingVolume),
        "a section with a line, no picture and no stated reason reads as a cut \
         in flight, and nothing is in flight",
    );
    assert_eq!(
        xsect.line, drawn_line,
        "the drawn line is two geographic points and names the same ground \
         under the new radar; dropping it throws away the user's aim",
    );
}

/// The clear covers every pane the switch moves, not the clicked one alone.
#[test]
fn the_clear_reaches_every_pane_the_switch_moves() {
    let mut app = two_pane_app(WSR88D, WSR88D);
    for idx in 0..2 {
        let pane = app.gui.pane_mut(idx).expect("the fixture built two panes");
        pane.scan_info = Some(wsr88d_scan_info());
        pane.data_time = Some(at(0));
    }
    assert_eq!(
        app.gui.layer_sync_targets(0),
        vec![0, 1],
        "precondition: the panes must be layer-linked, or the switch moves \
         only pane 0 and the sibling below is asserting nothing",
    );

    switch_to(&mut app, TDWR);

    for idx in 0..2 {
        let pane = app.gui.pane(idx).expect("the fixture built two panes");
        assert_eq!(
            pane.site(),
            TDWR,
            "pane {idx} was not moved by the linked group's switch",
        );
        assert!(
            pane.scan_info.is_none(),
            "pane {idx} names {TDWR} and still holds {WSR88D}'s products and tilts",
        );
        assert_eq!(
            pane.data_time, None,
            "pane {idx} is captioned with the age of a volume it no longer draws",
        );
    }
}

/// Picking the site a pane is already on is not a switch.
#[test]
fn re_picking_the_site_a_pane_is_on_keeps_its_scan() {
    let mut app = headless(TestBridge::desktop());
    pane_on(&mut app, WSR88D, Some(wsr88d_scan_info()));
    app.gui.pane_mut(0).unwrap().data_time = Some(at(0));

    switch_to(&mut app, WSR88D);

    assert_eq!(
        offered(&app),
        wsr88d_scan_info().available_products,
        "a no-op pick emptied the product menu",
    );
    assert_eq!(
        app.gui.pane(0).unwrap().data_time,
        Some(at(0)),
        "a no-op pick disowned the image the pane is still showing",
    );
}

/// **A pane that did not change radar keeps its loop.**
#[test]
fn a_pane_that_did_not_change_radar_keeps_its_loop() {
    /// The bystander's radar: a third site, so "kept" cannot be confused with
    /// "the destination's state was rebuilt".
    const BYSTANDER: &str = "KTLX";

    let mut app = two_pane_app(WSR88D, BYSTANDER);
    app.gui
        .pane_mut(1)
        .expect("the fixture built two panes")
        .layer_link = false;
    assert_eq!(
        app.gui.layer_sync_targets(0),
        vec![0],
        "precondition: the switch must move pane 0 alone, or pane 1 is not a \
         bystander and this asserts nothing",
    );
    app.gui
        .pane_mut(0)
        .expect("the fixture built two panes")
        .scan_info = Some(wsr88d_scan_info());

    let bystander_site = rustdar_radar::sites::get_radar_site(BYSTANDER)
        .expect("KTLX is in the resolved site table")
        .clone();
    let pane = app.gui.pane_mut(1).expect("the fixture built two panes");
    *pane.loop_state_mut() = rustdar_egui::radar_layer::begin_loop(
        3600,
        &bystander_site,
        rustdar_radar::types::RenderView::PlanView,
    );
    for minute in [0, 4, 8] {
        let held = crate::app::render::loop_frames_held(
            crate::app::render::test_loop_allocation(),
            pane.loop_state(),
            &crate::app::render::test_budgets(),
        );
        let clock = pane.time.mode;
        super::append_polled_frame(pane.loop_state_mut(), BYSTANDER, at(minute), held, clock);
    }
    let frames_before: Vec<NaiveDateTime> = pane
        .loop_state()
        .frames
        .iter()
        .map(|frame| frame.timestamp)
        .collect();
    assert_eq!(
        frames_before.len(),
        3,
        "precondition: the bystander has frames"
    );

    app.loop_mgr
        .cache_scan(BYSTANDER, at(0), (empty_scan().into(), Default::default()));
    app.loop_mgr.cache_l3_product(BYSTANDER, "EET", at(4), None);
    app.loop_mgr.set_plan(
        1,
        rustdar_radar::loop_downloads::FramePlan::new(
            BYSTANDER.to_string(),
            [0u32, 4, 8].iter().map(|&minute| at(minute)).collect(),
        ),
    );
    assert!(
        app.loop_mgr
            .plan_downloads_for(1, RadarProduct::Reflectivity),
        "precondition: the plan really does derive a download queue",
    );
    app.loop_mgr.set_plan(
        0,
        rustdar_radar::loop_downloads::FramePlan::new(WSR88D.to_string(), vec![at(0)]),
    );
    assert!(
        app.loop_mgr
            .plan_downloads_for(0, RadarProduct::Reflectivity)
    );
    assert!(!app.loop_mgr.is_pane_done(0));
    assert!(!app.loop_mgr.is_pane_done(1));

    switch_to(&mut app, TDWR);

    let loop_state = &app
        .gui
        .pane(1)
        .expect("the fixture built two panes")
        .loop_state();
    assert!(
        loop_state.is_active(),
        "another pane's site switch switched this pane's loop off",
    );
    assert_eq!(
        loop_state
            .frames
            .iter()
            .map(|frame| frame.timestamp)
            .collect::<Vec<_>>(),
        frames_before,
        "another pane's site switch threw away this pane's frame list",
    );
    assert!(
        app.loop_mgr.is_cached(BYSTANDER, &at(0)),
        "another pane's site switch dropped a volume this pane's loop had \
         already downloaded and rendered from, so it plays blank",
    );
    assert!(
        app.loop_mgr.l3_is_resolved(BYSTANDER, "EET", &at(4)),
        "another pane's site switch dropped a Level III answer this pane's loop \
         had already paired, so the volume is paired again",
    );
    assert_eq!(
        app.loop_mgr.plan_frame_count(1),
        3,
        "another pane's site switch took this pane's frame plan, so nothing is \
         left to re-derive its downloads from",
    );
    assert!(
        !app.loop_mgr.is_pane_done(1),
        "another pane's site switch emptied this pane's download queue, so its \
         remaining frames have nothing queued to fill them",
    );

    assert!(
        app.loop_mgr.is_pane_done(0),
        "the pane that left a radar kept its queue, so the shared download \
         budget is spent on volumes of a site it is no longer on",
    );
    assert_eq!(
        app.loop_mgr.plan_frame_count(0),
        0,
        "the departed pane's frame plan survived, so the next re-derivation \
         refills a queue from a listing no loop asked for",
    );
}

/// The same no-op pick, on the pane's **loop** — the half the rule above did
/// not reach.
#[test]
fn re_picking_the_site_a_pane_is_on_keeps_its_loop() {
    let mut app = headless(TestBridge::desktop());
    pane_on(&mut app, WSR88D, Some(wsr88d_scan_info()));

    let radar_site = rustdar_radar::sites::get_radar_site(WSR88D)
        .expect("KPBZ is in the resolved site table")
        .clone();
    let pane = app.gui.pane_mut(0).expect("a fresh Gui has one pane");
    *pane.loop_state_mut() = rustdar_egui::radar_layer::begin_loop(
        3600,
        &radar_site,
        rustdar_radar::types::RenderView::PlanView,
    );
    for minute in [0, 4, 8] {
        let held = crate::app::render::loop_frames_held(
            crate::app::render::test_loop_allocation(),
            pane.loop_state(),
            &crate::app::render::test_budgets(),
        );
        let clock = pane.time.mode;
        super::append_polled_frame(pane.loop_state_mut(), WSR88D, at(minute), held, clock);
    }
    let frames_before: Vec<NaiveDateTime> = pane
        .loop_state()
        .frames
        .iter()
        .map(|frame| frame.timestamp)
        .collect();
    assert_eq!(frames_before.len(), 3, "precondition: the loop has frames");

    app.loop_mgr
        .cache_scan(WSR88D, at(0), (empty_scan().into(), Default::default()));
    app.loop_mgr.insert_pending(
        0,
        rustdar_radar::loop_downloads::PendingDownloads {
            site: WSR88D.to_string(),
            queue: [at(8)].into_iter().collect(),
        },
    );
    assert!(
        !app.loop_mgr.is_pane_done(0),
        "precondition: the loop still owes a download",
    );

    switch_to(&mut app, WSR88D);

    let loop_state = &app
        .gui
        .pane(0)
        .expect("a fresh Gui has one pane")
        .loop_state();
    assert!(
        loop_state.is_active(),
        "a no-op pick switched the pane's loop off; nothing rebuilds it, so the \
         pane is back to its static image with the transport still reading \
         \"loop on\"",
    );
    assert_eq!(
        loop_state
            .frames
            .iter()
            .map(|frame| frame.timestamp)
            .collect::<Vec<_>>(),
        frames_before,
        "a no-op pick threw away a listing that named this very site's files",
    );
    assert!(
        app.loop_mgr.is_cached(WSR88D, &at(0)),
        "a no-op pick dropped a volume this very site's loop had already \
         downloaded and rendered from",
    );
    assert!(
        !app.loop_mgr.is_pane_done(0),
        "a no-op pick emptied the download queue out from under a surviving \
         loop, so its remaining frames have nothing queued to fill them",
    );
}

/// The switch ends the accumulation; it does not weaken it.
#[test]
fn the_new_sites_chunks_accumulate_from_nothing_rather_than_onto_the_old_site() {
    let mut app = headless(TestBridge::desktop());
    pane_on(&mut app, WSR88D, Some(wsr88d_scan_info()));

    switch_to(&mut app, TDWR);

    app.gui
        .apply(rustdar_egui::shell_api::GuiEvent::ChunkScanInfo {
            site: (TDWR).to_owned(),
            info: tdwr_chunk_scan_info(&[(RadarProduct::Reflectivity, &[0.3])], 5),
        });
    app.gui
        .apply(rustdar_egui::shell_api::GuiEvent::ChunkScanInfo {
            site: (TDWR).to_owned(),
            info: tdwr_chunk_scan_info(
                &[
                    (RadarProduct::Reflectivity, &[0.5]),
                    (RadarProduct::Velocity, &[0.5]),
                ],
                6,
            ),
        });

    let info = app
        .gui
        .pane(0)
        .unwrap()
        .scan_info
        .clone()
        .expect("the chunk feed delivered for TPIT");
    assert_eq!(
        info.available_products,
        vec![RadarProduct::Reflectivity, RadarProduct::Velocity],
        "the new site's picker is not exactly what its own volume carries",
    );
    assert_eq!(
        info.product_elevations[&RadarProduct::Reflectivity],
        vec![0.3, 0.5],
        "the union stopped accumulating within a site, so the tilt picker \
         shrinks and regrows as a live volume fills",
    );
    assert_eq!(
        info.site.name, TDWR,
        "the pane's scan info names a radar other than the one it is on",
    );
}

use crate::volume_fixture::ready_grid;
use rustdar_egui::pane::{VolumeStamp, VolumeTarget};
use rustdar_volumetric::bridge::{Hold, VolumeEntry};

/// A 3D target on `site`, at a time that separates one volume from the next.
fn volume_target(site: &str, minute: u32) -> VolumeTarget {
    VolumeTarget {
        region: None,
        product: rustdar_radar::fields::known::REFLECTIVITY,
        volume: VolumeStamp {
            site: site.to_owned(),
            collected: at(minute),
        },
    }
}

/// GPU texture bytes one [`ready_grid`] costs the store — what the assertions below are
/// denominated in.
fn one_grid_bytes() -> usize {
    let VolumeEntry::Ready(grid) = ready_grid() else {
        unreachable!("ready_grid is Ready")
    };
    let shape = grid.shape();
    rustdar_volumetric::raymarch::resident_grid_bytes([
        u32::try_from(shape.nx).unwrap(),
        u32::try_from(shape.ny).unwrap(),
        u32::try_from(shape.nz).unwrap(),
    ])
    .expect("a fixture grid cannot overflow")
}

/// Make `pane_idx` a 3D pane on `WSR88D`, already served — `rendered_for` set is what
/// stops `PrepareVolume` firing again.
fn volume_pane_on_the_wsr88d(app: &mut crate::app::App, pane_idx: usize, t: &VolumeTarget) {
    let pane = app.gui.pane_mut(pane_idx).expect("the pane exists");
    pane.set_site(WSR88D.to_owned());
    pane.set_view(rustdar_radar::types::RenderView::Volume);
    pane.volume_mut()
        .expect("a 3D pane has volume state")
        .rendered_for = Some(t.clone());
}

/// Open and resolve a build the way production does.
fn make_resident(app: &crate::app::App, pane_idx: usize, t: &VolumeTarget, hold: Hold) {
    app.volume_store.begin_build_held(pane_idx, t, hold);
    assert!(
        app.volume_store.complete(t, ready_grid()),
        "precondition: the entry this just opened takes the result",
    );
}

/// **The switch gives the previous radar's grid back, and does not wait for the
/// new one to arrive to do it.**
#[test]
fn switching_radar_releases_the_previous_sites_3d_grid_without_waiting_for_the_new_one() {
    let one = one_grid_bytes();
    assert!(one > 0, "precondition: a resident grid costs something");

    let mut app = headless(TestBridge::desktop());
    let left_behind = volume_target(WSR88D, 0);
    volume_pane_on_the_wsr88d(&mut app, 0, &left_behind);
    make_resident(&app, 0, &left_behind, Hold::Single);

    assert_eq!(
        app.volume_store.texture_bytes(),
        one,
        "precondition: the pane holds exactly one grid's worth of GPU texture",
    );
    let host_before = app.volume_store.memory_bytes();
    assert!(
        host_before > 0,
        "precondition: the resident grid has host bytes to give back",
    );

    switch_to(&mut app, TDWR);

    assert_eq!(
        app.volume_store.texture_bytes(),
        0,
        "the radar the pane just left is still resident on the GPU. Nothing \
         downstream reclaims it: `ui_map` returns the \"Downloading the first \
         …\" empty state before it can emit `PrepareVolume`, so no shed runs, \
         and `enforce_budget` only fires over budget — which one grid never is",
    );
    assert_eq!(
        app.volume_store.memory_bytes(),
        0,
        "the host grid outlived the site it describes",
    );
    assert!(
        app.volume_store.live_ids().is_empty(),
        "the store is still holding an entry for a radar nobody is on",
    );
    assert_eq!(
        app.gui
            .pane(0)
            .and_then(|p| p.volume())
            .and_then(|v| v.rendered_for.clone()),
        None,
        "`rendered_for` still names the radar that was left. `PrepareVolume` \
         is level-triggered on it, so switching *back* to a site whose stamp is \
         still published would match this stale key, never re-ask, and leave \
         the pane reading \"Building…\" for ever",
    );
}

/// **A build dispatched for the radar being left is dropped rather than
/// admitted.**
#[test]
fn a_resample_dispatched_for_the_radar_being_left_lands_on_a_store_that_dropped_it() {
    let mut app = headless(TestBridge::desktop());
    let in_flight = volume_target(WSR88D, 0);
    volume_pane_on_the_wsr88d(&mut app, 0, &in_flight);
    app.volume_store.begin_build(0, &in_flight);

    switch_to(&mut app, TDWR);

    assert!(
        !app.volume_store.complete(&in_flight, ready_grid()),
        "the abandoned radar's resample was admitted after the switch, so the \
         store gained a whole grid for a site no pane is on",
    );
    assert_eq!(
        app.volume_store.memory_bytes(),
        0,
        "a grid for the abandoned radar is resident on the host",
    );
}

/// **A 3D loop's whole resident set goes on the switch itself, not a frame
/// later.**
#[test]
fn switching_radar_releases_a_3d_loops_whole_resident_set_on_the_switch_frame() {
    let mut app = headless(TestBridge::desktop());
    let live = volume_target(WSR88D, 0);
    volume_pane_on_the_wsr88d(&mut app, 0, &live);
    for minute in 1..=3 {
        make_resident(&app, 0, &volume_target(WSR88D, minute), Hold::Set);
    }
    assert_eq!(
        app.volume_store.texture_bytes(),
        one_grid_bytes() * 3,
        "precondition: the pane holds a three-frame set",
    );

    switch_to(&mut app, TDWR);

    assert_eq!(
        app.volume_store.texture_bytes(),
        0,
        "the loop's frames outlived the radar they were resampled from, for at \
         least the frame between the switch and the next `dispatch_loop_renders`",
    );
    assert!(
        !app.volume_store.holds_set(0),
        "the pane is still marked a set holder, which exempts it from every \
         shed there is — so its next live grid would never be shed either",
    );
}

/// **A pane that did not change radar keeps what it holds.**
#[test]
fn a_pane_that_did_not_change_radar_keeps_its_3d_grid() {
    let one = one_grid_bytes();
    let mut app = two_pane_app(WSR88D, WSR88D);
    let shared = volume_target(WSR88D, 0);
    volume_pane_on_the_wsr88d(&mut app, 0, &shared);
    volume_pane_on_the_wsr88d(&mut app, 1, &shared);
    app.gui.pane_mut(1).expect("the pane exists").layer_link = false;
    make_resident(&app, 0, &shared, Hold::Single);
    assert!(
        app.volume_store.share(1, &shared),
        "precondition: the second pane attaches to the same entry",
    );
    assert_eq!(
        app.volume_store.texture_bytes(),
        one,
        "precondition: two panes on one volume share one grid",
    );

    switch_to(&mut app, TDWR);

    assert_eq!(
        app.volume_store.texture_bytes(),
        one,
        "releasing the switching pane took the grid out from under the pane \
         that is still on that radar and still painting it",
    );
    assert_eq!(
        app.gui
            .pane(1)
            .and_then(|p| p.volume())
            .and_then(|v| v.rendered_for.clone()),
        Some(shared),
        "the pane that did not move had its level-triggered key cleared, so it \
         will rebuild a grid it is already holding",
    );
}
