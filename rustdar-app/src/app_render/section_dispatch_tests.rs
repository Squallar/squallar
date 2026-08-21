use super::*;
use crate::platform_double::TestBridge;
use nexrad_model::data::{
    ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep,
    VolumeCoveragePattern, WaveformType,
};
use rustdar_egui::pane::{PaneKind, SectionLine, SectionUnavailable};
use rustdar_geo::GeoPoint;
use rustdar_radar::types::{RadarProduct, ScanInfo};

const SITE: &str = "KTLX";

fn volume_time() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
        .unwrap()
        .and_hms_opt(18, 30, 0)
        .unwrap()
}

fn line() -> SectionLine {
    SectionLine::new(
        GeoPoint {
            lat: 35.0,
            lon: -97.8,
        },
        GeoPoint {
            lat: 35.6,
            lon: -96.9,
        },
    )
    .expect("a fixture line must be finite and have two distinct ends")
}

/// One elevation cut, so the coverage pattern is a real tilt ladder rather
/// than the empty placeholder.
fn one_cut() -> ElevationCut {
    ElevationCut::new(
        0.5,
        ChannelConfiguration::ConstantPhase,
        WaveformType::CS,
        0.0,
        false,
        false,
        false,
        false,
        0,
        0,
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
        true,
    )
}

/// A one-sweep reflectivity volume.
fn volume(cuts: Vec<ElevationCut>) -> Arc<Scan> {
    volume_of(1, cuts)
}

/// The same volume with `sweeps` sweeps in it, for the live feed's growing `Scan`.
fn volume_of(sweeps: u8, cuts: Vec<ElevationCut>) -> Arc<Scan> {
    let radial = |elevation_number: u8| {
        Radial::new(
            1_760_000_000_000 + i64::from(elevation_number) * 1000,
            0,
            0.0,
            1.0,
            RadialStatus::ElevationStart,
            elevation_number,
            0.5,
            Some(MomentData::from_fixed_point(
                1,
                0,
                250,
                8,
                2.0,
                66.0,
                vec![32],
            )),
            None,
            None,
            None,
            None,
            None,
            None,
        )
    };
    Arc::new(Scan::new(
        VolumeCoveragePattern::new(
            if cuts.is_empty() { 0 } else { 212 },
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
            cuts,
        ),
        (1..=sweeps)
            .map(|n| Sweep::new(n, vec![radial(n)]))
            .collect(),
    ))
}

/// [`volume_of`] with the velocity moment filled instead of reflectivity —
/// what a storm-relative section has to be cut from.
fn velocity_volume(cuts: Vec<ElevationCut>) -> Arc<Scan> {
    let radial = Radial::new(
        1_760_000_000_000,
        0,
        0.0,
        1.0,
        RadialStatus::ElevationStart,
        1,
        0.5,
        None,
        Some(MomentData::from_fixed_point(
            1,
            0,
            250,
            8,
            2.0,
            129.0,
            vec![200],
        )),
        None,
        None,
        None,
        None,
        None,
    );
    Arc::new(Scan::new(
        VolumeCoveragePattern::new(
            if cuts.is_empty() { 0 } else { 212 },
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
            cuts,
        ),
        vec![Sweep::new(1, vec![radial])],
    ))
}

/// `n` copies of [`one_cut`], so a `volume_of(n, …)` keys every sweep.
fn cuts_for(n: u8) -> Vec<ElevationCut> {
    (0..n).map(|_| one_cut()).collect()
}

/// An `App` with one section pane aimed along [`line`], on a site whose
/// volume is `scan`.
fn app_with_section(product: RadarProduct, scan: Arc<Scan>) -> crate::app::App {
    let mut app = crate::app::tests::headless(TestBridge::desktop());
    let site = rustdar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is a real radar")
        .clone();
    {
        let pane = app.gui.pane_mut(0).unwrap();
        pane.set_site(SITE.to_owned());
        pane.set_selected_product(rustdar_radar::fields::spec(product).id.clone());
        pane.set_kind(PaneKind::CrossSection);
        pane.cross_section_mut().unwrap().line = Some(line());
    }
    app.gui
        .apply(rustdar_egui::shell_api::GuiEvent::ScanInfoForPane {
            pane_idx: 0,
            info: ScanInfo {
                site,
                site_source: rustdar_radar::site_position::SitePositionSource::Table,
                site_position: None,
                timestamp: volume_time(),
                vcp_number: 212,
                available_products: vec![product],
                product_elevations: std::collections::HashMap::new(),
                status: String::new(),
            },
        });
    app.render.ensure_pane_count(1);
    app.volumes
        .install_base(SITE.to_owned(), (scan, Default::default(), volume_time()));
    app
}

fn state(app: &crate::app::App) -> &rustdar_egui::pane::CrossSectionPane {
    app.gui
        .pane(0)
        .unwrap()
        .cross_section()
        .expect("pane 0 is a section pane")
}

/// A volume joined mid-scan says so; a site with nothing at all says the
/// download is in flight; and neither writes the staleness key.
#[test]
fn the_section_refusal_tells_a_mid_scan_join_from_a_cold_start() {
    let mid_flight = volume(Vec::new());
    assert_eq!(
        section_source_refusal(None, Some(&mid_flight)),
        Some(SectionUnavailable::AwaitingCoveragePattern),
        "a mid-scan join is a blank pane with no explanation"
    );
    assert_eq!(
        section_source_refusal(None, None),
        Some(SectionUnavailable::AwaitingVolume),
        "an empty site must say its download is in flight"
    );
    let base = volume(vec![one_cut()]);
    assert_eq!(
        section_source_refusal(Some(&base), Some(&mid_flight)),
        None,
        "a base in hand is a complete volume to cut; refusing it because \
             the overlay has no pattern yet is the wait this substrate exists \
             to remove"
    );
}

/// **A volume that has started and sealed nothing has a name of its own.**
#[test]
fn a_volume_with_no_sealed_sweep_yet_is_named_rather_than_blamed_on_the_product() {
    let started = volume_of(0, vec![one_cut()]);
    assert!(
        started.sweeps().is_empty(),
        "precondition: the fixture is a started volume with nothing sealed"
    );
    assert!(
        !started.coverage_pattern().elevation_cuts().is_empty(),
        "precondition: the pattern has arrived, so this is not the \
             `AwaitingCoveragePattern` case"
    );
    assert!(
        rustdar_radar::current::resolve(None, Some((&*started).into())).is_some(),
        "precondition: the merge resolves — that is exactly why this state \
             had no name"
    );

    assert_eq!(
        section_source_refusal(None, Some(&started)),
        Some(SectionUnavailable::AwaitingFirstSweep),
        "a volume that has sealed nothing yet was reported as a product \
             problem, or not reported at all"
    );

    assert!(
        section_source_refusal(None, Some(&started)).is_some(),
        "the dispatch would be entered for a volume with nothing in it, and \
             would log its refusal for as long as the volume stood"
    );

    let message = SectionUnavailable::AwaitingFirstSweep.message();
    assert!(
        message.contains("only just started"),
        "the message does not say what is being waited on: {message}"
    );
    assert!(
        message.contains("tilt"),
        "the message does not name the thing that will end the wait: {message}"
    );
    assert!(
        !message.to_lowercase().contains("carries no"),
        "the message still blames the product for an empty volume: {message}"
    );

    let messages = [
        SectionUnavailable::AwaitingVolume.message(),
        SectionUnavailable::AwaitingCoveragePattern.message(),
        SectionUnavailable::AwaitingFirstSweep.message(),
    ];
    for (i, a) in messages.iter().enumerate() {
        for b in &messages[i + 1..] {
            assert_ne!(a, b, "two waiting states say the same thing");
        }
    }

    let sealed = volume_of(1, vec![one_cut()]);
    assert_eq!(
        section_source_refusal(None, Some(&sealed)),
        None,
        "a volume with a sealed sweep on a keyable pattern is a section"
    );
}

/// The refusal reaches the **pane**, not just the predicate: an empty live volume
/// leaves the section labelled and un-keyed.
#[test]
fn an_empty_live_volume_labels_the_pane_and_keeps_it_asking() {
    let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));
    app.volumes.forget_all_bases();
    app.volumes.install_base(
        SITE.to_owned(),
        (
            volume_of(0, vec![one_cut()]),
            Default::default(),
            volume_time(),
        ),
    );

    app.dispatch_section_renders();

    assert_eq!(
        state(&app).unavailable,
        Some(SectionUnavailable::AwaitingFirstSweep),
        "a started, empty volume left the pane blank or blamed the product"
    );
    assert_eq!(
        state(&app).rendered_for,
        None,
        "the key was written for a state the next sealed sweep resolves, so \
             the pane would never ask again"
    );
    assert!(
        !app.render.pane_render[0].render_in_flight(),
        "a render slot was spent on a volume with nothing in it"
    );
}

/// The transient refusals leave the pane **asking**: the state resolves
/// itself, so the key must not be written.
#[test]
fn a_transient_section_refusal_keeps_the_pane_asking() {
    let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));
    app.volumes.forget_all_bases();

    app.dispatch_section_renders();

    assert_eq!(
        state(&app).unavailable,
        Some(SectionUnavailable::AwaitingVolume),
        "an empty site is a blank pane with no explanation"
    );
    assert_eq!(
        state(&app).rendered_for,
        None,
        "the key was written for a condition that clears itself, so the pane \
             will never ask again and never show a section"
    );
    assert!(
        !app.render.pane_render[0].render_in_flight(),
        "a render slot was spent to be told what the volume already said"
    );

    let message = SectionUnavailable::AwaitingCoveragePattern.message();
    assert!(message.contains("mid-scan"), "{message}");
    assert!(message.contains("next volume"), "{message}");
}

/// **A held line dispatches nothing, and a dropped line dispatches exactly one cut**.
#[test]
fn a_dropped_line_re_cuts_once_and_a_held_line_never() {
    let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));

    app.dispatch_section_renders();
    assert!(
        app.render.pane_render[0].render_in_flight(),
        "precondition: the aimed pane never cut at all"
    );
    let first_key = state(&app)
        .rendered_for
        .clone()
        .expect("the key is written on dispatch");
    app.render.pane_render[0].render_finished();

    for frame in 0..60 {
        app.dispatch_section_renders();
        assert!(
            !app.render.pane_render[0].render_in_flight(),
            "poll {frame} against an unmoved line dispatched a cut: that \
                 is a re-cut per frame for the length of every drag"
        );
    }
    assert_eq!(
        state(&app).rendered_for,
        Some(first_key.clone()),
        "an idle poll moved the staleness key"
    );

    let moved = SectionLine::new(
        GeoPoint {
            lat: 35.05,
            lon: -97.8,
        },
        GeoPoint {
            lat: 35.7,
            lon: -96.8,
        },
    )
    .expect("a valid moved line");
    assert_ne!(
        moved,
        line(),
        "precondition: the drop really moved the line"
    );
    app.gui
        .pane_mut(0)
        .unwrap()
        .cross_section_mut()
        .unwrap()
        .line = Some(moved);

    app.dispatch_section_renders();
    assert!(
        app.render.pane_render[0].render_in_flight(),
        "the dropped line did not re-cut: the handle drop is inert until \
             the next volume moves the key"
    );
    assert_eq!(
        state(&app).rendered_for.as_ref().map(|t| t.line),
        Some(moved),
        "the new cut was dispatched for the old line"
    );
}

/// A product with no vertical structure says so, and **stops** asking.
#[test]
fn a_product_with_no_vertical_structure_says_so_and_stops_asking() {
    let mut app = app_with_section(RadarProduct::EchoTops, volume(vec![one_cut()]));

    app.dispatch_section_renders();

    assert_eq!(
        state(&app).unavailable,
        Some(SectionUnavailable::ProductHasNoVerticalStructure(
            rustdar_radar::fields::known::ECHO_TOPS
        )),
    );
    assert!(
        state(&app).rendered_for.is_some(),
        "nothing will ever make this product sliceable, so leaving the key \
             unwritten re-dispatches the same refusal on every frame"
    );
    assert!(!app.render.pane_render[0].render_in_flight());

    let message =
        SectionUnavailable::ProductHasNoVerticalStructure(rustdar_radar::fields::known::ECHO_TOPS)
            .message();
    assert!(message.contains(RadarProduct::EchoTops.name()), "{message}");
}

/// An edit to the storm motion override invalidates the SRV vertical views.
#[test]
fn an_override_edit_invalidates_the_srv_vertical_views() {
    let mut app = app_with_section(RadarProduct::StormRelativeVelocity, volume(vec![one_cut()]));
    let stale_section = rustdar_egui::pane::SectionTarget {
        volume: rustdar_egui::pane::VolumeStamp {
            site: SITE.to_owned(),
            collected: volume_time(),
        },
        product: rustdar_radar::fields::known::STORM_RELATIVE_VELOCITY,
        line: line(),
        ladder: 7,
    };
    let srv_target = rustdar_egui::pane::VolumeTarget {
        volume: rustdar_egui::pane::VolumeStamp {
            site: SITE.to_owned(),
            collected: volume_time(),
        },
        product: rustdar_radar::fields::known::STORM_RELATIVE_VELOCITY,
        region: None,
    };
    let arm = |app: &mut crate::app::App| {
        app.gui
            .pane_mut(0)
            .unwrap()
            .cross_section_mut()
            .unwrap()
            .rendered_for = Some(stale_section.clone());
        app.volume_store.insert(
            0,
            srv_target.clone(),
            rustdar_volumetric::bridge::VolumeEntry::Refused("the old vector's".into()),
        );
    };
    arm(&mut app);
    assert!(
        app.volume_store.lookup(&srv_target).is_some(),
        "precondition: the store holds an SRV entry",
    );

    app.gui.storm_motion_override.enabled = true;
    let ctx = egui::Context::default();
    app.dispatch_pane_renders(&ctx);

    assert_eq!(
        state(&app).rendered_for,
        None,
        "the section must forget its cut and re-derive with the new vector",
    );
    assert!(
        app.volume_store.lookup(&srv_target).is_none(),
        "the store must evict the SRV grid derived with the old vector",
    );

    arm(&mut app);
    app.dispatch_pane_renders(&ctx);
    assert!(
        state(&app).rendered_for.is_some(),
        "an unchanged vector must not invalidate the section",
    );
    assert!(
        app.volume_store.lookup(&srv_target).is_some(),
        "an unchanged vector must not evict the grid",
    );
}

/// A pane with no volume yet is waiting, not broken.
#[test]
fn a_section_with_no_volume_is_told_it_is_waiting() {
    let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));
    app.gui.pane_mut(0).unwrap().scan_info = None;

    app.dispatch_section_renders();
    assert_eq!(
        state(&app).unavailable,
        Some(SectionUnavailable::AwaitingVolume)
    );
    assert_eq!(state(&app).rendered_for, None);
}

/// A dispatch for a pane the dispatcher does not have refuses rather than
/// panicking, and takes no budget on the way out.
#[test]
fn a_dispatch_for_a_pane_that_does_not_exist_refuses_instead_of_panicking() {
    let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));
    let target = app.section_target_for_pane(0).expect("aimed with a volume");
    let data = app.volumes.base_for(SITE).expect("the site has a volume").0;
    assert_eq!(app.render.pane_render.len(), 1, "precondition");

    let dispatched = app.render.spawn_section_render(
        7,
        &target,
        move || {
            rustdar_radar::render_input::RenderInput::extract_volume(
                &data,
                RadarProduct::Reflectivity,
                35.3333,
                -97.2778,
            )
        },
        app.channels.section_sender.clone(),
        None,
    );

    assert_eq!(
        dispatched,
        crate::render_dispatch::SectionDispatch::Busy,
        "a pane that does not exist got a cut",
    );
    assert_eq!(
        app.render
            .renders_in_flight
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "the refusal took a render slot with it, so the budget is short by \
             one for the life of the process"
    );
}

/// A volume that carries nothing to cut is **told so**, not left waiting.
#[test]
fn a_volume_with_nothing_to_cut_is_named_rather_than_waited_on() {
    let mut app = app_with_section(RadarProduct::StormRelativeVelocity, volume(vec![one_cut()]));
    app.dispatch_section_renders();

    assert_eq!(
        state(&app).unavailable,
        Some(SectionUnavailable::ProductMissingFromVolume(
            rustdar_radar::fields::known::STORM_RELATIVE_VELOCITY
        )),
        "the pane is waiting on a cut that can never come",
    );
    assert!(
        state(&app).rendered_for.is_some(),
        "without the staleness key the pane re-dispatches every frame — a \
             busy loop whose only symptom is a warm machine",
    );
    let message = state(&app).unavailable.as_ref().expect("named").message();
    assert!(
        message.contains("carries no"),
        "the state has a name but no explanation: {message:?}",
    );

    app.volumes.install_base(
        SITE.to_owned(),
        (
            velocity_volume(vec![one_cut()]),
            Default::default(),
            volume_time(),
        ),
    );
    app.render.pane_render[0].render_finished();
    app.dispatch_section_renders();
    assert_eq!(
        state(&app).unavailable,
        None,
        "the refusal outlived the volume it was about",
    );
}

/// A product the radar *derives* tilt by tilt gets a cut, not a refusal.
#[test]
fn a_section_of_a_derived_product_is_cut_rather_than_refused_by_name() {
    for product in [
        RadarProduct::StormRelativeVelocity,
        RadarProduct::NormalizedRotation,
        RadarProduct::SpecificDifferentialPhase,
    ] {
        assert!(
            rustdar_radar::sampler::samplable(product).is_none(),
            "precondition: {} has no native moment, so this is about the \
                 `volume_slot` gate and not about `samplable`",
            product.name(),
        );
        let mut app = app_with_section(product, velocity_volume(vec![one_cut()]));
        app.dispatch_section_renders();
        assert_eq!(
            state(&app).unavailable,
            None,
            "{} is derived tilt by tilt and the section refused it",
            product.name(),
        );
        assert!(
            state(&app).rendered_for.is_some(),
            "{} never got a cut dispatched",
            product.name(),
        );
    }
}

/// Dragging the storm motion vector re-derives the cross-section.
#[test]
fn a_storm_motion_edit_re_derives_the_cross_section() {
    let mut app = app_with_section(
        RadarProduct::StormRelativeVelocity,
        velocity_volume(vec![one_cut()]),
    );
    let drag = |app: &mut crate::app::App, speed: f32, direction: f32, enabled: bool| {
        app.gui.storm_motion_override = rustdar_egui::StormMotionOverride {
            enabled,
            speed_kt: speed,
            direction_deg: direction,
        };
        assert!(app.apply_storm_motion_override(), "the vector must move");
        app.render.pane_render[0].render_finished();
        app.dispatch_section_renders();
        assert!(
            state(app).rendered_for.is_some(),
            "the cut was never dispatched, so the assertion below is \
                 about a payload nothing asked for",
        );
    };

    drag(&mut app, 20.0, 240.0, true);
    assert_eq!(
        app.render.section_payload_motion(),
        Some(Some((20.0, 240.0))),
        "precondition: the first cut carries the vector in force",
    );

    drag(&mut app, 60.0, 90.0, true);
    assert_eq!(
        app.render.section_payload_motion(),
        Some(Some((60.0, 90.0))),
        "the section redrew from the previous vector's field",
    );

    drag(&mut app, 60.0, 90.0, false);
    assert_eq!(app.render.section_payload_motion(), Some(None));
}

/// **Commit on release.** A vector still under the pointer is not applied, and
/// the moment it is let go it is.
#[test]
fn a_storm_motion_drag_commits_on_release_rather_than_per_frame() {
    let mut app = app_with_section(
        RadarProduct::StormRelativeVelocity,
        velocity_volume(vec![one_cut()]),
    );
    app.gui.storm_motion_override = rustdar_egui::StormMotionOverride {
        enabled: true,
        speed_kt: 20.0,
        direction_deg: 240.0,
    };
    assert!(
        app.apply_storm_motion_override(),
        "precondition: the first vector must land, or there is no committed \
         state for the drag below to be held against",
    );

    app.gui.storm_motion_editing = true;
    assert!(
        app.gui.storm_motion_mid_edit(),
        "precondition: the drag must be in progress",
    );
    for (speed, direction) in [(30.0, 200.0), (45.0, 150.0), (60.0, 90.0)] {
        app.gui.storm_motion_override.speed_kt = speed;
        app.gui.storm_motion_override.direction_deg = direction;
        assert!(
            !app.apply_storm_motion_override(),
            "a value produced mid-drag was applied, so every frame of the drag \
             evicts and rebuilds every storm-relative grid and section",
        );
    }

    app.gui.storm_motion_editing = false;
    assert!(
        app.apply_storm_motion_override(),
        "the released vector was never applied, so the edit is lost and the \
         picture keeps the old vector for ever",
    );
    assert!(
        !app.apply_storm_motion_override(),
        "the committed vector applied twice, so the change detector is not \
         idempotent and every frame after a drag evicts again",
    );
}

/// A cut of the right shape and no content, for the receive path.
fn blank_cut() -> Box<rustdar_radar::xsect::CrossSection> {
    use rustdar_radar::sampler::SampleStatus;
    use rustdar_radar::xsect::{CrossSection, SECTION_HEIGHT, SECTION_WIDTH, SectionAxes};
    let pixels = SECTION_WIDTH * SECTION_HEIGHT;
    Box::new(
        CrossSection::from_parts(
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
        .expect("a full-size, all-NoCoverage section is well formed"),
    )
}

/// A cut lands on the pane that asked for it, and clears its in-flight flag.
#[test]
fn a_finished_cut_lands_on_the_pane_that_asked_for_it() {
    let ctx = egui::Context::default();
    let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));
    let target = app.section_target_for_pane(0).expect("aimed with a volume");
    app.gui
        .pane_mut(0)
        .unwrap()
        .cross_section_mut()
        .unwrap()
        .rendered_for = Some(target.clone());
    app.render.pane_render[0].render_started(None);

    app.channels
        .section_sender
        .send(crate::channels::SectionResponse {
            pane_idx: 0,
            generation: app.render.render_generation,
            target,
            section: Some(blank_cut()),
        })
        .expect("the receiver is alive");
    app.poll_section_results(&ctx);

    assert!(
        state(&app).section.is_some(),
        "the cut never reached the pane"
    );
    assert!(
        state(&app).texture.is_some(),
        "the raster was never uploaded"
    );
    assert_eq!(state(&app).unavailable, None);
    assert!(
        !app.render.pane_render[0].render_in_flight(),
        "a pane that never hears back stops asking for another cut"
    );

    let id = state(&app).texture.as_ref().expect("uploaded").id();
    let manager = ctx.tex_manager();
    let manager = manager.read();
    let meta = manager
        .meta(id)
        .expect("the handle is alive, so its meta is");
    assert_eq!(
        meta.options,
        egui::TextureOptions::NEAREST,
        "the section raster is filtered, which paints the interpolation as \
             measurement"
    );
}

/// A cut for a line the pane is no longer aimed along is dropped, and the
/// key is left alone.
#[test]
fn a_cut_for_a_line_the_pane_has_left_behind_is_dropped() {
    let ctx = egui::Context::default();
    let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));
    let superseded = app.section_target_for_pane(0).expect("aimed with a volume");

    app.gui
        .pane_mut(0)
        .unwrap()
        .cross_section_mut()
        .unwrap()
        .line = SectionLine::new(
        GeoPoint {
            lat: 35.0,
            lon: -97.8,
        },
        GeoPoint {
            lat: 36.4,
            lon: -95.9,
        },
    );
    let current = app.section_target_for_pane(0).expect("still aimed");
    assert_ne!(
        current, superseded,
        "precondition: the pane really moved on"
    );
    app.gui
        .pane_mut(0)
        .unwrap()
        .cross_section_mut()
        .unwrap()
        .rendered_for = Some(current.clone());

    app.channels
        .section_sender
        .send(crate::channels::SectionResponse {
            pane_idx: 0,
            generation: app.render.render_generation,
            target: superseded,
            section: Some(blank_cut()),
        })
        .expect("the receiver is alive");
    app.poll_section_results(&ctx);

    assert!(
        state(&app).section.is_none(),
        "a cut of the line the user has already replaced is on screen"
    );
    assert_eq!(
        state(&app).rendered_for,
        Some(current),
        "the superseded cut took the key with it, so the cut still in flight \
             will be dropped too and the pane will wait for ever"
    );
}

/// **A section pane comes back from a suspend, a display change or a lost
/// surface** — and comes back with its picture rather than with a promise.
#[test]
fn a_section_pane_gets_its_picture_back_after_the_context_dies() {
    let ctx = egui::Context::default();
    let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));
    let target = app.section_target_for_pane(0).expect("aimed with a volume");
    app.gui
        .pane_mut(0)
        .unwrap()
        .cross_section_mut()
        .unwrap()
        .rendered_for = Some(target.clone());
    app.render.pane_render[0].render_started(None);
    app.channels
        .section_sender
        .send(crate::channels::SectionResponse {
            pane_idx: 0,
            generation: app.render.render_generation,
            target: target.clone(),
            section: Some(blank_cut()),
        })
        .expect("the receiver is alive");
    app.poll_section_results(&ctx);
    let before = state(&app).texture.as_ref().expect("uploaded").id();

    app.gui.clear_graphics_state();
    assert!(
        state(&app).texture.is_none(),
        "precondition: the handle has to actually be released, or this test \
             passes against a build that never had the bug"
    );
    assert!(
        state(&app).section.is_some() && state(&app).rendered_for.is_some(),
        "precondition: the cut and its key survive the release — that pair is \
             what makes the pane look busy while nothing is running"
    );

    app.restore_cached_render(&ctx);

    let after = state(&app).texture.as_ref().map(|t| t.id());
    assert!(
        after.is_some(),
        "the pane came back with no raster, so it paints \"Cutting the \
             cross-section…\" for a cut that will never be dispatched",
    );
    assert_ne!(
        after,
        Some(before),
        "the same handle came back, so nothing was uploaded and the id is a \
             dangling one from the context that died"
    );

    let manager = ctx.tex_manager();
    let manager = manager.read();
    assert_eq!(
        manager
            .meta(after.expect("uploaded"))
            .expect("the handle is alive, so its meta is")
            .options,
        egui::TextureOptions::NEAREST,
        "the restored raster is filtered, which paints the interpolation as \
             measurement"
    );
    drop(manager);

    assert_eq!(
        state(&app).rendered_for,
        Some(target),
        "the resume path moved the staleness key"
    );
    app.render.pane_render[0].render_finished();
    app.dispatch_section_renders();
    assert!(
        !app.render.pane_render[0].render_in_flight(),
        "the pane re-cut its section on resume instead of re-uploading it"
    );
}

/// **The restore reaches as far as the release does**, hidden panes
/// included.
#[test]
fn the_section_restore_walks_every_remembered_pane() {
    let (_, rest) = include_str!("../app_render.rs")
        .split_once("fn restore_section_textures(")
        .expect("restore_section_textures is no longer a method here");
    let body = rest
        .split_once("\n    }")
        .map(|(body, _)| body)
        .expect("restore_section_textures has no recognisable body");
    assert!(
        body.contains("self.gui.remembered_pane_count()"),
        "the section restore is bounded by something other than the \
             remembered pane count, so a section pane hidden across a suspend \
             comes back holding a released texture that nothing will replace: \
             {body}",
    );
    assert!(
        !body.contains("self.gui.pane_count()"),
        "the section restore stops at the visible pane count while \
             `clear_graphics_state` releases every remembered pane",
    );
}

/// A cut answering nothing says so, rather than leaving the pane looking as
/// though it were still working.
#[test]
fn a_cut_that_answered_nothing_says_it_failed() {
    let ctx = egui::Context::default();
    let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));
    let target = app.section_target_for_pane(0).expect("aimed with a volume");
    app.gui
        .pane_mut(0)
        .unwrap()
        .cross_section_mut()
        .unwrap()
        .rendered_for = Some(target.clone());

    app.channels
        .section_sender
        .send(crate::channels::SectionResponse {
            pane_idx: 0,
            generation: app.render.render_generation,
            target,
            section: None,
        })
        .expect("the receiver is alive");
    app.poll_section_results(&ctx);

    assert_eq!(
        state(&app).unavailable,
        Some(SectionUnavailable::RenderFailed),
        "a pane that will never get a picture must not look like one that is \
             about to"
    );
}

/// A result from a superseded *generation* is dropped **and clears the key**.
#[test]
fn a_result_from_a_dead_generation_puts_the_pane_back_to_asking() {
    let ctx = egui::Context::default();
    let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));
    let target = app.section_target_for_pane(0).expect("aimed with a volume");
    app.gui
        .pane_mut(0)
        .unwrap()
        .cross_section_mut()
        .unwrap()
        .rendered_for = Some(target.clone());
    let stale = app.render.render_generation;
    app.render.render_generation += 1;

    app.channels
        .section_sender
        .send(crate::channels::SectionResponse {
            pane_idx: 0,
            generation: stale,
            target,
            section: Some(blank_cut()),
        })
        .expect("the receiver is alive");
    app.poll_section_results(&ctx);

    assert!(state(&app).section.is_none(), "a stale cut was drawn");
    assert_eq!(
        state(&app).rendered_for,
        None,
        "the key outlived the answer that was thrown away, so the pane will \
             never ask again and never show a section"
    );
}

/// A new volume for the site makes the section on screen stale **by the
/// same comparison** that notices a moved endpoint or a changed moment.
#[test]
fn a_new_volume_makes_the_section_on_screen_stale_with_no_reset_arm() {
    let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));

    let before = app
        .section_target_for_pane(0)
        .expect("the pane is aimed and has a volume");

    if let Some(info) = app.gui.pane_mut(0).unwrap().scan_info.as_mut() {
        info.timestamp = volume_time() + chrono::Duration::minutes(6);
    }
    let after = app.section_target_for_pane(0).expect("still aimed");
    assert_ne!(before, after, "a new volume did not make the key move");

    app.gui
        .pane_mut(0)
        .unwrap()
        .set_selected_product(rustdar_radar::fields::known::VELOCITY);
    assert_ne!(app.section_target_for_pane(0), Some(after));

    app.gui
        .pane_mut(0)
        .unwrap()
        .cross_section_mut()
        .unwrap()
        .line = SectionLine::new(
        GeoPoint {
            lat: 35.0,
            lon: -97.8,
        },
        GeoPoint {
            lat: 36.0,
            lon: -96.0,
        },
    );
    let moved = app.section_target_for_pane(0).expect("still aimed");
    assert_ne!(moved.line, before.line);
}

/// A live volume that is still filling re-cuts as it fills, **even though
/// its timestamp never moves**.
#[test]
fn a_live_volume_that_is_still_filling_re_cuts_as_it_fills() {
    let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));
    if let Some(info) = app.gui.pane_mut(0).unwrap().scan_info.as_mut() {
        info.product_elevations.insert(
            RadarProduct::Reflectivity,
            vec![0.5, 0.9, 1.3, 1.8, 2.4, 3.1, 4.0, 5.1, 6.4],
        );
    }

    let before = app
        .section_target_for_pane(0)
        .expect("the pane is aimed and has a volume");
    assert_ne!(before.ladder, 0, "the fixture volume's ladder resolves");

    app.volumes.install_base(
        SITE.to_owned(),
        (volume_of(4, cuts_for(4)), Default::default(), volume_time()),
    );
    let after = app.section_target_for_pane(0).expect("still aimed");

    assert_eq!(
        before.volume, after.volume,
        "precondition: the live feed's volume stamp really is frozen, so it \
             cannot be what notices the volume growing"
    );
    assert_ne!(
        before.ladder, after.ladder,
        "three more sweeps arrived and the key never moved, so the pane \
             goes on showing a one-sweep section"
    );

    app.gui
        .pane_mut(0)
        .unwrap()
        .cross_section_mut()
        .unwrap()
        .rendered_for = Some(before);
    app.dispatch_section_renders();
    assert_eq!(
        state(&app).rendered_for.as_ref().map(|t| t.ladder),
        Some(after.ladder),
        "the dispatcher short-circuited on a key cut from a quarter of the \
             volume"
    );
}

/// **The re-cut skip.**
#[test]
fn a_seal_that_changes_no_chosen_rung_does_not_move_the_section_key() {
    let surveillance_only = || {
        Arc::new(Scan::new(
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
                vec![one_cut(), one_cut()],
            ),
            vec![split_half(1, false)],
        ))
    };
    let with_doppler_half = || {
        Arc::new(Scan::new(
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
                vec![one_cut(), one_cut()],
            ),
            vec![split_half(1, false), split_half(2, true)],
        ))
    };

    let mut app = app_with_section(RadarProduct::Reflectivity, surveillance_only());
    let before = app.section_target_for_pane(0).expect("aimed");
    assert_ne!(before.ladder, 0, "precondition: the ladder resolves");

    app.volumes.install_base(
        SITE.to_owned(),
        (with_doppler_half(), Default::default(), volume_time()),
    );
    let after = app.section_target_for_pane(0).expect("still aimed");
    assert_eq!(
        before, after,
        "the Doppler half changed no reflectivity rung, and the key moved \
             anyway: that is a byte-identical re-cut per split cut per volume"
    );

    app.gui
        .pane_mut(0)
        .unwrap()
        .set_selected_product(rustdar_radar::fields::known::VELOCITY);
    let vel_after = app.section_target_for_pane(0).expect("aimed at velocity");
    app.volumes.install_base(
        SITE.to_owned(),
        (surveillance_only(), Default::default(), volume_time()),
    );
    let vel_before = app.section_target_for_pane(0).expect("still aimed");
    assert_ne!(
        vel_before.ladder, vel_after.ladder,
        "velocity gained its first rung from that seal and the key never \
             noticed"
    );
}

/// One half of a split cut: the surveillance pass carries reflectivity alone.
fn split_half(elevation_number: u8, doppler: bool) -> Sweep {
    let moment = || MomentData::from_fixed_point(1, 0, 250, 8, 2.0, 66.0, vec![32]);
    let radial = Radial::new(
        1_760_000_000_000 + i64::from(elevation_number) * 1000,
        0,
        0.0,
        1.0,
        RadialStatus::ElevationStart,
        elevation_number,
        0.5,
        Some(moment()),
        doppler.then(moment),
        None,
        None,
        None,
        None,
        None,
    );
    Sweep::new(elevation_number, vec![radial])
}
