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
///
/// The Level III half comes from `ScanInfo::from_scan`, so those five entries
/// are the ones the real path lists rather than a hand-written guess. The
/// dual-pol half is added here because they are listed off a radial's moments
/// and the cheap test volume has no radials — a real `KPBZ` volume carries
/// them, and they are exactly the entries `discover_product_elevations`
/// withholds at a single-pol site.
fn wsr88d_scan_info() -> ScanInfo {
    // No learned position: the fixture volume states none either, so the row
    // this resolves through is the table's, which is what these tests are
    // about — which *site* a pane is on, not where that site is.
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
    // Nothing is compiled in, so the row this reads has to be placed first.
    crate::test_sites::install();
    ScanInfo {
        site: rustdar_radar::sites::get_radar_site(TDWR)
            .expect("TPIT is in the resolved site table")
            .clone(),
        // The row, unmodified: a chunk-fed `ScanInfo` is assembled without a
        // volume to state a position and nothing has been learned for `TPIT`
        // in these tests.
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
///
/// The cut is all-`NoCoverage` because none of these tests read a pixel: what
/// they are about is whether the three planes are *there*, and a full-size one
/// is the only kind `CrossSection::from_parts` will build.
fn section_pane_showing_the_wsr88ds_cut(app: &mut crate::app::App) {
    use rustdar_radar::sampler::SampleStatus;
    use rustdar_radar::xsect::{CrossSection, SECTION_HEIGHT, SECTION_WIDTH, SectionAxes};

    let line = rustdar_egui::pane::SectionLine::new(
        rustdar_egui::pane::GeoPoint {
            lat: 40.4,
            lon: -80.2,
        },
        rustdar_egui::pane::GeoPoint {
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
    pane.site = WSR88D.to_string();
    pane.set_kind(rustdar_egui::pane::PaneKind::CrossSection);
    let product = pane.selected_product;
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
    pane.site = site.to_string();
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
///
/// `ScanInfo` is the product picker, and it is a claim about one site.
/// Switching from a WSR-88D to a TDWR left the previous site's claim standing
/// until a completed volume replaced it wholesale, so for up to a volume period
/// the picker offered six products `TPIT` can never draw — the five Level III
/// entries, which come from an RPG the TDWR network does not have, and the
/// hybrid classification, which needs ΦDP and ρHV a single-pol instrument does
/// not measure — plus the dual-pol moments themselves.
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
        app.gui.pane(0).unwrap().site,
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

/// The tilt picker travels with the product list, and for the same reason: the
/// angles in a `ScanInfo` are the previous site's VCP. `TPIT` flies neither the
/// number of cuts nor the angles `KPBZ` does, and `get_rendering_params` snaps
/// the selection to the nearest *listed* angle — so a leftover ladder aims the
/// pane at a tilt the new radar never flew.
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
///
/// `data_time` is when the data behind the image *on screen* was collected, and
/// the image goes when the `ScanInfo` does — `dispatch_pane_renders` tears the
/// radar texture down for a pane that resolves no rendering params and holds no
/// scan. Left behind, it ages the radar the user just left against a pane that
/// is showing nothing at all.
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
///
/// The one pane kind the plan view's own clear does not reach. Dropping
/// `scan_info` makes `section_target_for_pane` return `None` — it reads the
/// volume time off the scan — so no `SectionTarget` is built and the site term
/// it carries is never compared against anything. What runs instead is
/// `mark_section_unavailable`, which by design leaves the picture up: right for
/// the previous *volume*, which is the same radar's older data and is captioned
/// with its own time, and wrong for the previous *site*, which is another radar
/// entirely. Left alone the pane holds `KPBZ`'s cut, and answers a hover with
/// `KPBZ`'s values, under pills that say `TPIT`, until the new site's first
/// volume lands.
///
/// The line stays: it is two geographic points, so it names the same ground
/// under the new radar, and re-cutting it there is the whole point of keeping
/// it.
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
///
/// `SwitchRadarSite` writes `layer_sync_targets(pane_idx)` — the whole
/// layer-linked group when the source is linked — so on a split of two linked
/// panes a pill click moves both. A clear written for the source alone would
/// leave the sibling named `TPIT` and still offering `KPBZ`'s menu: the
/// original staleness, on the pane the user was not looking at when they
/// clicked, and the pane most likely to be left on it.
///
/// The two-site case is covered by [`re_picking_the_site_a_pane_is_on_keeps_its_scan`]'s
/// rule rather than repeated here — `moving` is filtered pane by pane on
/// `pane.site != site`, so a linked sibling already on the destination is the
/// no-op pick with an extra pane in front of it.
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
            pane.site, TDWR,
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
///
/// Every entry point that raises `SwitchRadarSite` — the site pill, the
/// inspector's list, a map icon — will happily emit the current site, and
/// `layer_sync_targets` hands the handler the whole linked group, which may
/// include a pane already there. Clearing on those would blank a pane whose
/// menu, tilts and image are all correct, and a fetch that returns "already
/// latest" sends no response at all, so nothing would put them back.
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

/// The same no-op pick, on the pane's **loop** — the half the rule above did
/// not reach.
///
/// The scan, the section and the `data_time` all moved behind the
/// `pane.site != site` guard when the site-switch release landed; the loop
/// reset and `LoopDownloadManager::clear_all` were left in front of it. So the
/// two halves of one handler disagreed about what a re-pick is, and this is the
/// half that costs the most: a re-pick threw away a listing, every downloaded
/// volume and every rendered frame of a loop that was correct — and raises
/// neither of the two actions that rebuild one (`handle_enable_loop`,
/// `reinit_active_loops`), so the pane fell back to its static image for the
/// rest of the session with the transport still showing the loop on.
///
/// Both halves are asserted because either alone leaves the loop dead. A
/// surviving `loop_state` whose scans and frame plan were cleared underneath it
/// is a loop that plays blank frames and has nothing queued to fill them.
#[test]
fn re_picking_the_site_a_pane_is_on_keeps_its_loop() {
    use rustdar_radar::archive::Identifier;

    let mut app = headless(TestBridge::desktop());
    pane_on(&mut app, WSR88D, Some(wsr88d_scan_info()));

    let radar_site = rustdar_radar::sites::get_radar_site(WSR88D)
        .expect("KPBZ is in the resolved site table")
        .clone();
    let pane = app.gui.pane_mut(0).expect("a fresh Gui has one pane");
    pane.loop_state = rustdar_egui::pane::LoopPlaybackState::new_for_loop(
        3600,
        &radar_site,
        rustdar_radar::types::RenderView::PlanView,
    );
    for minute in [0, 4, 8] {
        let held = crate::app::render::loop_frames_held(
            crate::app::render::test_loop_allocation(),
            &pane.loop_state,
            &crate::app::render::test_budgets(),
        );
        super::append_polled_frame(&mut pane.loop_state, WSR88D, at(minute), held);
    }
    let frames_before: Vec<NaiveDateTime> = pane
        .loop_state
        .frames
        .iter()
        .map(|frame| frame.timestamp)
        .collect();
    assert_eq!(frames_before.len(), 3, "precondition: the loop has frames");

    // The two halves of the loop's download state: a volume already in hand for
    // the oldest frame, and the queue the rest are still owed through.
    app.loop_mgr
        .cache_scan(WSR88D, at(0), (empty_scan().into(), Default::default()));
    app.loop_mgr.insert_pending(
        0,
        crate::loop_downloads::PendingDownloads {
            site: WSR88D.to_string(),
            queue: [(
                at(8),
                Identifier::new("KPBZ20260811_180800_V06".to_string()),
            )]
            .into_iter()
            .collect(),
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
        .loop_state;
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
///
/// `apply_chunk_scan_info` unions a partial volume's products and tilts into
/// what the pane holds and never removes one, so the picker does not shrink and
/// regrow every few seconds as a live volume fills. That union is right
/// *within* one site and only within one — this walks both halves in order: the
/// switch clears, the new site's first sealed cut lands on an empty pane, and
/// its second cut adds to the first without resurrecting anything of `KPBZ`'s.
#[test]
fn the_new_sites_chunks_accumulate_from_nothing_rather_than_onto_the_old_site() {
    let mut app = headless(TestBridge::desktop());
    pane_on(&mut app, WSR88D, Some(wsr88d_scan_info()));

    switch_to(&mut app, TDWR);

    // The surveillance cut, then the first Doppler cut of the same volume.
    app.gui.apply_chunk_scan_info(
        TDWR,
        tdwr_chunk_scan_info(&[(RadarProduct::Reflectivity, &[0.3])], 5),
    );
    app.gui.apply_chunk_scan_info(
        TDWR,
        tdwr_chunk_scan_info(
            &[
                (RadarProduct::Reflectivity, &[0.5]),
                (RadarProduct::Velocity, &[0.5]),
            ],
            6,
        ),
    );

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

// ---------------------------------------------------------------------------
// The 3D pane's voxel grid, and the moment it stops describing anything.
//
// The three above are about what a pane *says* after a switch. These are about
// what it still *holds*: a resolved `VoxelGrid` is 8.00 MiB of host bytes and
// 36.6 MiB of GPU texture at the desktop cell budget (built through
// `rustdar_radar::voxel::build_voxels` at `voxel::DESKTOP_SHAPE`, which is the
// budget triple rather than the shape a discrete desktop respends it into),
// and the pane went on holding the radar it had just left.
//
// Every assertion here is in **bytes off a real `VoxelGrid`**, for the reason
// the hidden-pane suite gives: a store of `Refused` stubs satisfies an
// entry-count assertion while giving nothing back, so counting entries would
// pass on a fix that freed nothing.
// ---------------------------------------------------------------------------

use crate::volume::bridge::tests::ready_grid;
use crate::volume::bridge::{Hold, VolumeEntry};
use rustdar_egui::pane::{VolumeStamp, VolumeTarget};

/// A 3D target on `site`, at a time that separates one volume from the next.
fn volume_target(site: &str, minute: u32) -> VolumeTarget {
    VolumeTarget {
        region: None,
        product: RadarProduct::Reflectivity,
        volume: VolumeStamp {
            site: site.to_owned(),
            collected: at(minute),
        },
    }
}

/// GPU texture bytes one [`ready_grid`] costs the store — what the assertions
/// below are denominated in, so that a fix which drops the entry without
/// freeing the grid cannot pass.
fn one_grid_bytes() -> usize {
    let VolumeEntry::Ready(grid) = ready_grid() else {
        unreachable!("ready_grid is Ready")
    };
    let shape = grid.shape();
    crate::volume::raymarch::resident_grid_bytes([
        u32::try_from(shape.nx).unwrap(),
        u32::try_from(shape.ny).unwrap(),
        u32::try_from(shape.nz).unwrap(),
    ])
    .expect("a fixture grid cannot overflow")
}

/// Make `pane_idx` a 3D pane on `WSR88D`, already served — `rendered_for` set
/// is what stops `PrepareVolume` firing again, and so what a release has to
/// clear.
fn volume_pane_on_the_wsr88d(app: &mut crate::app::App, pane_idx: usize, t: &VolumeTarget) {
    let pane = app.gui.pane_mut(pane_idx).expect("the pane exists");
    pane.site = WSR88D.to_owned();
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
///
/// This is the case the defect was actually about. Nothing is ever fetched for
/// `TPIT` here — no scan, no stamp, no `PrepareVolume` — which is exactly the
/// state a pane sits in while a fetch is in flight, and the state it sits in
/// *for ever* when that fetch fails or the site has no data. Before the fix the
/// store still read one whole grid at this point, because every path that would
/// have shed it runs off a target the pane cannot yet form.
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
///
/// The half that is easy to miss: releasing only what is *resolved* would let
/// the in-flight resample land afterwards and put a fresh grid for the
/// abandoned radar into the store, where nothing would ask for it again.
/// Detaching the pane prunes the `Building` entry, and that absence is what
/// `VolumeStore::complete` reads as "nothing is waiting for this".
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
///
/// `dispatch_loop_renders` does reclaim a torn-down loop's set — the switch
/// resets `loop_state`, and the next frame's `retire_queues` pass calls
/// `release_set`. That left the set resident for the frame in between, which
/// on the desktop shape is fourteen grids. The release is now edge-triggered on
/// the switch, so no frame is drawn with it still held, and the set holder is
/// unmarked with it.
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
///
/// The over-release direction, and the one a blanket "drop everything on a
/// switch" would fail. `layer_sync_targets` moves the linked group alone, so an
/// unlinked second pane stays where it is — and the store refcounts by pane, so
/// the entry the two share must survive the first pane letting go of it.
#[test]
fn a_pane_that_did_not_change_radar_keeps_its_3d_grid() {
    let one = one_grid_bytes();
    let mut app = two_pane_app(WSR88D, WSR88D);
    let shared = volume_target(WSR88D, 0);
    volume_pane_on_the_wsr88d(&mut app, 0, &shared);
    volume_pane_on_the_wsr88d(&mut app, 1, &shared);
    // Panes are layer-linked by default (`PaneState::layer_link`), and a linked
    // group moves together — which would make this the *same* transition as the
    // test above rather than its complement. Unlinking the second pane is what
    // leaves it a pane the switch genuinely does not move.
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

    // Pane 0 alone. `two_pane_app`'s panes are not layer-linked, so
    // `layer_sync_targets` names only the pane that was clicked.
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
