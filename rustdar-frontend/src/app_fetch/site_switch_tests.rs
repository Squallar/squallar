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
