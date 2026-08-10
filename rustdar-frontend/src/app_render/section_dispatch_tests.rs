use super::*;
use crate::platform_double::TestBridge;
use nexrad_model::data::{
    ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep,
    VolumeCoveragePattern, WaveformType,
};
use rustdar_egui::pane::{GeoPoint, PaneKind, SectionLine, SectionUnavailable};
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

/// A one-sweep reflectivity volume. `cuts` empty is exactly what
/// `chunks::placeholder_coverage_pattern(0)` produces — the shape a volume
/// joined mid-scan has until its VCP message lands.
fn volume(cuts: Vec<ElevationCut>) -> Arc<Scan> {
    volume_of(1, cuts)
}

/// The same volume with `sweeps` sweeps in it, for the live feed's growing
/// `Scan`. Every sweep carries reflectivity and nothing else, so it also
/// stands in for the surveillance-only half of a split cut. Radials carry
/// real, distinct collection stamps because the ladder fingerprint hashes
/// them — a fixture stamping everything `0` would make two different
/// volumes' sweeps indistinguishable to the key under test.
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
        pane.site = SITE.to_owned();
        pane.selected_product = product;
        pane.set_kind(PaneKind::CrossSection);
        pane.cross_section_mut().unwrap().line = Some(line());
    }
    app.gui.set_scan_info_for_pane(
        0,
        ScanInfo {
            site,
            timestamp: volume_time(),
            vcp_number: 212,
            available_products: vec![product],
            product_elevations: std::collections::HashMap::new(),
            status: String::new(),
        },
    );
    app.render.ensure_pane_count(1);
    // Into the substrate's base holder, and deliberately **not** into
    // `scan_data`: sections cut from the merged current volume, and a
    // fixture that also filled the plan view's map would leave a
    // regression to reading `scan_data` invisible — the pin these tests
    // carry is precisely that a section works with the map's holder empty.
    app.base_scans
        .insert(SITE.to_owned(), (scan, Default::default(), volume_time()));
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
///
/// The reason is decided by [`section_source_refusal`], a pure function of
/// the two holders, and the two answers must stay distinct: an overlay
/// carrying sealed sweeps under `chunks.rs`' placeholder pattern resolves
/// itself at the next volume start, while an empty site resolves when its
/// first download lands — and with a base in hand there is no refusal at
/// all, however patternless the overlay, because the base alone is a
/// complete volume to cut.
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

/// The transient refusals leave the pane **asking**: the state resolves
/// itself, so the key must not be written.
#[test]
fn a_transient_section_refusal_keeps_the_pane_asking() {
    let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));
    // No source at all: the cold-start arm of the same refusal path the
    // mid-flight join takes.
    app.base_scans.clear();

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
        !app.render.pane_render[0].render_in_flight,
        "a render slot was spent to be told what the volume already said"
    );

    // The message names the cause and says it clears itself, which is the
    // whole reason it is not folded into a generic "no data".
    let message = SectionUnavailable::AwaitingCoveragePattern.message();
    assert!(message.contains("mid-scan"), "{message}");
    assert!(message.contains("next volume"), "{message}");
}

/// **A held line dispatches nothing, and a dropped line dispatches exactly
/// one cut** — the dispatcher's half of the endpoint drag's
/// re-cut-on-drop contract.
///
/// The egui half
/// (`dragging_an_endpoint_re_aims_the_section_on_drop_and_never_mid_drag`)
/// pins that a drag in flight never touches the pane's stored line and
/// that the drop writes it exactly once. This half pins what that buys:
/// the stored line is the staleness key's line, so any number of polls
/// against an unmoved line after the first cut must dispatch **nothing**
/// — a cut walks megabytes of gate bytes, and per-frame dispatch during a
/// drag is precisely what deferring the write to the drop exists to
/// prevent — while the drop's single write must make the very next poll
/// cut, or dropping a handle would do nothing until the next volume
/// happened along.
#[test]
fn a_dropped_line_re_cuts_once_and_a_held_line_never() {
    let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));

    // The first poll after the pane was aimed: one cut in flight, the key
    // written on dispatch.
    app.dispatch_section_renders();
    assert!(
        app.render.pane_render[0].render_in_flight,
        "precondition: the aimed pane never cut at all"
    );
    let first_key = state(&app)
        .rendered_for
        .clone()
        .expect("the key is written on dispatch");
    // The cut completes; the budget frees. The key stays, which is the
    // whole staleness machine.
    app.render.pane_render[0].render_in_flight = false;

    // A drag in flight: the preview lives on the map pane and the stored
    // line holds still, so every one of these polls is the dispatcher
    // looking at an unmoved key. Sixty of them — a second of frames —
    // must dispatch nothing.
    for frame in 0..60 {
        app.dispatch_section_renders();
        assert!(
            !app.render.pane_render[0].render_in_flight,
            "poll {frame} against an unmoved line dispatched a cut: that \
                 is a re-cut per frame for the length of every drag"
        );
    }
    assert_eq!(
        state(&app).rendered_for,
        Some(first_key.clone()),
        "an idle poll moved the staleness key"
    );

    // The drop: exactly the one write `Gui::apply_pending_section_edit`
    // makes, nothing else touched.
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
        app.render.pane_render[0].render_in_flight,
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
///
/// The mirror of the test above, and the pair is the point: nothing about
/// this volume or the next will make a column integral sliceable, so
/// re-asking every frame is a busy loop with no output and no symptom but a
/// warm machine.
#[test]
fn a_product_with_no_vertical_structure_says_so_and_stops_asking() {
    let mut app = app_with_section(RadarProduct::EchoTops, volume(vec![one_cut()]));

    app.dispatch_section_renders();

    assert_eq!(
        state(&app).unavailable,
        Some(SectionUnavailable::ProductHasNoVerticalStructure(
            RadarProduct::EchoTops
        )),
    );
    assert!(
        state(&app).rendered_for.is_some(),
        "nothing will ever make this product sliceable, so leaving the key \
             unwritten re-dispatches the same refusal on every frame"
    );
    assert!(!app.render.pane_render[0].render_in_flight);

    // Named, so the message can say which product and what to do instead.
    let message =
        SectionUnavailable::ProductHasNoVerticalStructure(RadarProduct::EchoTops).message();
    assert!(message.contains(RadarProduct::EchoTops.name()), "{message}");
}

/// An edit to the storm motion override invalidates the SRV vertical
/// views: the section's staleness key is cleared and the 3D store's SRV
/// entries are evicted, so both re-derive with the new vector.
///
/// The vector is a render *parameter*, not part of any target — without
/// this, an SRV volume and section keep painting the old vector's field
/// until the next volume rolls, silently. The plan-view invalidation
/// lives in `RenderDispatcher::set_storm_motion_override`; this pins its
/// vertical counterpart, and that an unchanged vector invalidates
/// nothing.
#[test]
fn an_override_edit_invalidates_the_srv_vertical_views() {
    let mut app = app_with_section(RadarProduct::StormRelativeVelocity, volume(vec![one_cut()]));
    let stale_section = rustdar_egui::pane::SectionTarget {
        volume: rustdar_egui::pane::VolumeStamp {
            site: SITE.to_owned(),
            collected: volume_time(),
        },
        product: RadarProduct::StormRelativeVelocity,
        line: line(),
        ladder: 7,
    };
    let srv_target = rustdar_egui::pane::VolumeTarget {
        volume: rustdar_egui::pane::VolumeStamp {
            site: SITE.to_owned(),
            collected: volume_time(),
        },
        product: RadarProduct::StormRelativeVelocity,
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
            crate::volume::bridge::VolumeEntry::Refused("the old vector's".into()),
        );
    };
    arm(&mut app);
    assert!(
        app.volume_store.lookup(&srv_target).is_some(),
        "precondition: the store holds an SRV entry",
    );

    // Flipping the override on is a vector change.
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

    // Re-armed with the vector unchanged: nothing is invalidated, or
    // every SRV pane would rebuild every frame.
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
///
/// Unreachable today: the only caller reaches `spawn_section_render` through
/// `pane_render.get(pane_idx)` two lines earlier. It is pinned because the
/// two `pane_render` indexes inside straddle the in-flight increment and the
/// `RenderGuard`, so the panic a future caller would earn would leave the
/// render budget permanently short by one as well as taking down the frame
/// thread — and on wasm the budget is one.
#[test]
fn a_dispatch_for_a_pane_that_does_not_exist_refuses_instead_of_panicking() {
    let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));
    let target = app.section_target_for_pane(0).expect("aimed with a volume");
    let data = Arc::clone(&app.base_scans.get(SITE).expect("the site has a volume").0);
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
///
/// The dispatcher's "no payload" answer used to be the same `false` as "the
/// render budget is full", and the caller's reading of `false` is "write no
/// staleness key, ask again next frame". So a section pane on a volume with
/// no such moment re-dispatched on every frame and painted "Cutting the
/// cross-section…" for as long as the volume stood: a permanent wait, which
/// the pane's own doc calls the worst state a pane can be in, and which
/// this codebase shipped once before and fixed.
///
/// The fixture is storm-relative velocity over a reflectivity-only volume:
/// no velocity anywhere, so `extract_volume_parts` answers `None` — the
/// same answer a refused derivation gives.
#[test]
fn a_volume_with_nothing_to_cut_is_named_rather_than_waited_on() {
    let mut app = app_with_section(RadarProduct::StormRelativeVelocity, volume(vec![one_cut()]));
    app.dispatch_section_renders();

    assert_eq!(
        state(&app).unavailable,
        Some(SectionUnavailable::ProductMissingFromVolume(
            RadarProduct::StormRelativeVelocity
        )),
        "the pane is waiting on a cut that can never come",
    );
    assert!(
        state(&app).rendered_for.is_some(),
        "without the staleness key the pane re-dispatches every frame — a \
             busy loop whose only symptom is a warm machine",
    );
    // And the message is a sentence, not a spinner.
    let message = state(&app).unavailable.expect("named").message();
    assert!(
        message.contains("carries no"),
        "the state has a name but no explanation: {message:?}",
    );

    // The key carries the volume stamp, so a volume that *does* carry
    // velocity asks again rather than inheriting the refusal.
    app.base_scans.insert(
        SITE.to_owned(),
        (
            velocity_volume(vec![one_cut()]),
            Default::default(),
            volume_time(),
        ),
    );
    app.render.pane_render[0].render_in_flight = false;
    app.dispatch_section_renders();
    assert_eq!(
        state(&app).unavailable,
        None,
        "the refusal outlived the volume it was about",
    );
}

/// A product the radar *derives* tilt by tilt gets a cut, not a refusal.
///
/// The third of the three UI-facing gates that admit SRV, NROT and KDP to
/// the vertical views, and the last of them to get a test. All three could
/// be reverted to `sampler::samplable` — the exact pre-admission code —
/// with every test in the workspace green, and every derived section pane
/// would answer `ProductHasNoVerticalStructure` permanently, for a volume
/// the worker can slice perfectly well. The headline feature of the
/// products WP had no UI-facing pin at all.
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
///
/// The reviewer's probe, and it failed on the shipped code with
/// `left: Some((20.0, 240.0)), right: Some((60.0, 90.0))`. The payload
/// cache keyed on `(site, collected, product, ladder)`, so an override
/// edit — which clears the pane's staleness key but not the payload —
/// left `reusable` true, skipped `extract()` and shipped the previous
/// vector's field. On screen the section visibly redrew showing the old
/// vector, for up to a whole volume, while the plan view and the 3D
/// volume re-derived correctly: a silent wrong field, and the worst kind,
/// because the redraw is the user's evidence that it worked.
#[test]
fn a_storm_motion_edit_re_derives_the_cross_section() {
    let mut app = app_with_section(
        RadarProduct::StormRelativeVelocity,
        velocity_volume(vec![one_cut()]),
    );
    // Driven through the settings panel's own state and the app's own
    // edit path, so the staleness key is cleared the way a real drag
    // clears it — the half that already worked, and the half that made
    // this bug invisible.
    let drag = |app: &mut crate::app::App, speed: f32, direction: f32, enabled: bool| {
        app.gui.storm_motion_override = rustdar_egui::StormMotionOverride {
            enabled,
            speed_kt: speed,
            direction_deg: direction,
        };
        assert!(app.apply_storm_motion_override(), "the vector must move");
        // The previous cut has landed. A section pane with a cut in
        // flight does not re-dispatch, which in the app is the arrival
        // that clears this and in a test has to be said out loud.
        app.render.pane_render[0].render_in_flight = false;
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

    // Clearing the override back to the volume's own Bunkers fit is the
    // same edit in the other direction, and was equally invisible.
    drag(&mut app, 60.0, 90.0, false);
    assert_eq!(app.render.section_payload_motion(), Some(None));
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
    app.render.pane_render[0].render_in_flight = true;

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
        !app.render.pane_render[0].render_in_flight,
        "a pane that never hears back stops asking for another cut"
    );

    // **And it is uploaded `NEAREST`**, which is one of the three honesty
    // devices the section pane rests on and the only one that leaves no
    // trace in the source of the module it protects. A section's rows are
    // the tilt ladder's rungs stretched to fill the gaps between them;
    // bilinear filtering blends those edges into a smooth gradient and
    // paints exactly the impression the caption exists to refuse — that the
    // vertical structure was measured continuously. Nothing about the
    // picture would look broken, which is why it is asserted rather than
    // left to a comment.
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
///
/// A section takes an order of magnitude longer to produce than the user
/// takes to draw another line over it, so this is ordinary rather than
/// exotic — and the failure is the worst kind: a section of the *previous*
/// line, on screen, captioned with the current volume, looking authoritative.
#[test]
fn a_cut_for_a_line_the_pane_has_left_behind_is_dropped() {
    let ctx = egui::Context::default();
    let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));
    let superseded = app.section_target_for_pane(0).expect("aimed with a volume");

    // The pane moves on: a new line, and the key that goes with it.
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
///
/// The whole cycle, in the order the app runs it:
/// `clear_graphics_state` (Android's `onPause`, a foldable unfolding, a GPU
/// reset — `app_render.rs`'s surface-loss arm and `app.rs`'s resume path
/// both land here) drops the handle, then `restore_cached_render` runs when
/// the renderer is rebuilt, then the ordinary per-frame dispatch.
///
/// Without the restore this is the pane's worst state and it is silent: the
/// handle is gone, the cut and its `rendered_for` are not, so
/// `render_cross_section` paints "Cutting the cross-section…" — the *in
/// flight* message — while `dispatch_section_renders` reads the matching key
/// and declines to cut. Nothing is in flight and nothing ever will be. The
/// hover readout goes with it, since the paint returns before it. Live, the
/// next volume rescues it in about six minutes; on an archived volume it
/// never recovers at all, which is exactly the "waiting that will never end"
/// `ui_section_pane`'s doc calls the worst state a pane can be in.
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
    app.render.pane_render[0].render_in_flight = true;
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

    // Suspend, unfold, or a wgpu surface loss. Same call in all three.
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

    // The renderer is rebuilt and the app restores what it can.
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

    // The restored raster is the honest one. `restore_section_textures` and
    // `poll_section_results` share `upload_section_raster` for this reason —
    // a resume that silently re-uploaded LINEAR would blend the rungs into a
    // smooth gradient and nothing about the picture would look wrong.
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

    // And it was a **re-upload**, not a re-cut. The key is untouched, so the
    // dispatcher stays quiet: a resume must not walk a 15.6 MB volume for a
    // picture already in memory, and must not depend on that volume still
    // being in memory at all.
    assert_eq!(
        state(&app).rendered_for,
        Some(target),
        "the resume path moved the staleness key"
    );
    app.render.pane_render[0].render_in_flight = false;
    app.dispatch_section_renders();
    assert!(
        !app.render.pane_render[0].render_in_flight,
        "the pane re-cut its section on resume instead of re-uploading it"
    );
}

/// **The restore reaches as far as the release does**, hidden panes
/// included.
///
/// `Gui::clear_graphics_state` walks every *remembered* pane on purpose,
/// and its own test says why: a handle belonging to a pane the user split
/// away from is just as invalid once the context is gone, and a re-split
/// would hand it straight back to the renderer. So the restore has to walk
/// exactly as far. Bounding it at `pane_count()` — the *visible* count, and
/// the natural thing to reach for — leaves a section pane that was hidden
/// during a suspend holding a released texture with its `rendered_for`
/// still satisfied: the same permanently-waiting pane, reached by splitting
/// up instead of by backgrounding the app.
///
/// Read off the source because the two counts differ only when a pane is
/// remembered but not shown, and the API that produces that state
/// (`Gui::grow_panes` and the pane picker behind it) is `pub(crate)` to
/// `rustdar-egui`. Same reason, and same technique, as
/// `restore_describes_its_image_tests` below.
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
///
/// The opposite of the case above, and the asymmetry is the point. There the
/// pane has already asked for something else, so its key belongs to a cut
/// still in flight. Here the pane is still waiting and the answer has been
/// thrown away, so leaving the key would tell the dispatcher this cut had
/// been answered — and nothing else would ever ask again.
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
///
/// This is what buys the absence of a `reset_panes_for_*` arm for section
/// panes — the kind of thing that gets remembered for one of the two reset
/// paths and not the other. Asserted on the key itself, because the key is
/// what the dispatch decides on.
#[test]
fn a_new_volume_makes_the_section_on_screen_stale_with_no_reset_arm() {
    let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));

    let before = app
        .section_target_for_pane(0)
        .expect("the pane is aimed and has a volume");

    // Nothing but the volume time moves.
    if let Some(info) = app.gui.pane_mut(0).unwrap().scan_info.as_mut() {
        info.timestamp = volume_time() + chrono::Duration::minutes(6);
    }
    let after = app.section_target_for_pane(0).expect("still aimed");
    assert_ne!(before, after, "a new volume did not make the key move");

    // The product picker moves it too, so the one comparison really does
    // cover every input rather than only the one it was written for.
    app.gui.pane_mut(0).unwrap().selected_product = RadarProduct::Velocity;
    assert_ne!(app.section_target_for_pane(0), Some(after));

    // And so does the line, which is the input the interaction produces.
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
///
/// This is the configuration the feature actually ships in — live chunks are
/// on by default — and it is the one the volume-time key does not cover.
/// `ScanInfo::timestamp` is the *first* sweep's first radial, and on the
/// chunk feed `sweeps[0]` is fixed for the whole volume, so the stamp is a
/// constant for five to six minutes while the merged ladder refreshes rung
/// by rung. Observed live before the original fix: a map pane full of
/// echo, a section pane empty, and a caption reading `1 tilts` for six
/// minutes.
///
/// Two preconditions carry the test. `before.volume` and `after.volume` are
/// asserted **equal**, so a future change that makes the stamp move on the
/// live feed fails here on the premise rather than passing for the wrong
/// reason. And `product_elevations` is left **untouched** across the growth,
/// because that is the source this discriminator was first written against
/// and it is wrong: `Gui::apply_chunk_scan_info` merges angles in and never
/// removes one, so after a session's first complete volume the pane already
/// knows the whole VCP and the count never moves again.
#[test]
fn a_live_volume_that_is_still_filling_re_cuts_as_it_fills() {
    let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));
    // The pane already knows every angle the VCP flies — the state it is in
    // for every volume after the first one of a session.
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

    // More sweeps land: the volume grows and nothing else does.
    app.base_scans.insert(
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

    // And the pane really re-dispatches on it: with the one-sweep key
    // stored, the four-sweep target no longer matches and the
    // short-circuit at the top of `dispatch_section_renders` falls through.
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

/// **The re-cut skip.** A seal that changes no chosen rung for the pane's
/// moment leaves the section key exactly where it was — and the same seal
/// moves the key of the moment it *does* change.
///
/// The waste this pins from the app side: a split cut's Doppler half
/// carries a short-range reflectivity copy, and the old sweep-count key
/// moved on its seal — ~6 byte-identical re-cuts per VCP-212 volume, each
/// a 15.6 MB walk and a render slot. The rung-choice fingerprint cannot be
/// moved by a seal that changes no choice; `current::tests` pin the
/// fingerprint itself, and this pins that the section target actually
/// rides it.
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

    app.base_scans.insert(
        SITE.to_owned(),
        (with_doppler_half(), Default::default(), volume_time()),
    );
    let after = app.section_target_for_pane(0).expect("still aimed");
    assert_eq!(
        before, after,
        "the Doppler half changed no reflectivity rung, and the key moved \
             anyway: that is a byte-identical re-cut per split cut per volume"
    );

    // The same seal is a real change for velocity — the moment it carries
    // — and the key must move there, or the skip is a freeze.
    app.gui.pane_mut(0).unwrap().selected_product = RadarProduct::Velocity;
    let vel_after = app.section_target_for_pane(0).expect("aimed at velocity");
    app.base_scans.insert(
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

/// One half of a split cut: the surveillance pass carries reflectivity
/// alone, the Doppler pass carries reflectivity's short-range copy plus
/// velocity — the shape whose seal used to force the byte-identical
/// re-cut.
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
