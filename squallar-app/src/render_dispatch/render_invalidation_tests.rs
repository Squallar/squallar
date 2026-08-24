use super::*;
// Used only by the gated-render instrument and the tests built on it, which are native-only
// with `Job::Opaque`; see `gated_render`.
#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc;

/// A render that does not finish until the test releases it.
#[cfg(not(target_arch = "wasm32"))]
fn gated_render() -> (mpsc::Sender<()>, squallar_worker::offload::Job) {
    let (release, held) = mpsc::channel::<()>();
    (
        release,
        squallar_worker::offload::Job::Opaque(Box::new(move || {
            held.recv().expect("every gated render is released");
            Some(squallar_source::job::DescribedOut(Box::new(
                squallar_radar::frame::RenderedFrame {
                    image: Vec::new(),
                    max_range_km: 230.0,
                    polar: Default::default(),
                    nyquist_ms: None,
                    melting_layer_source: None,
                    storm_motion: None,
                },
            )))
        })),
    )
}

/// [`gated_render`] for a render that answers nothing — what `Job::renders_nothing`
/// produces when no sweep carries the product, held open so the abandonment protocol can be
/// exercised around it.
#[cfg(not(target_arch = "wasm32"))]
fn gated_nothing() -> (mpsc::Sender<()>, squallar_worker::offload::Job) {
    let (release, held) = mpsc::channel::<()>();
    (
        release,
        squallar_worker::offload::Job::Opaque(Box::new(move || {
            held.recv().expect("every gated render is released");
            None
        })),
    )
}

/// One pane, on `site`, which is how `reset_panes_for_site` reads the layout.
fn gui_showing(site: &str) -> squallar_egui::Gui {
    let mut gui = squallar_egui::Gui::new();
    gui.pane_mut(0)
        .expect("a fresh Gui has one pane")
        .set_site(site.to_string());
    gui
}

/// The environmental heights route into the hail render parameters from the same map the
/// sounding drain writes, and a moved pair drops exactly that site's hail renders — the
/// per-site sibling of `changing_the_override_invalidates_the_storm_relative_renders`.
#[test]
fn a_landed_sounding_routes_into_hail_renders_and_a_moved_pair_drops_them() {
    let heights = |h0: f64, hm20: f64| squallar_radar::sounding::EnvHeights {
        h0c_km_msl: h0,
        hm20c_km_msl: hm20,
        fetched_at: chrono::Utc::now(),
    };
    let mut d = RenderDispatcher::new();
    let gui = gui_showing("KTLX");
    d.ensure_pane_count(1);

    assert_eq!(
        d.env_heights_km_msl_for(RadarProduct::ProbabilityOfSevereHail, "KTLX"),
        None,
        "before any sounding lands the render must draw nothing, not zeros",
    );
    assert!(
        d.set_env_heights("KTLX", heights(4.2, 7.1), &gui),
        "the first pair is a change from nothing",
    );
    assert_eq!(
        d.env_heights_km_msl_for(RadarProduct::MaxExpectedHailSize, "KTLX"),
        Some((4.2, 7.1)),
    );
    assert_eq!(
        d.env_heights_km_msl_for(RadarProduct::Reflectivity, "KTLX"),
        None,
        "reflectivity does not read the environment",
    );
    assert_eq!(
        d.env_heights_km_msl_for(RadarProduct::ProbabilityOfSevereHail, "KOUN"),
        None,
        "the environment is per-site",
    );

    d.pane_render[0].last_rendered = Some((RadarProduct::ProbabilityOfSevereHail, 0.5));
    d.cache_render(
        "KTLX",
        RadarProduct::MaxExpectedHailSize,
        squallar_radar::types::RenderView::PlanView,
        0.5,
        cached(1.0),
    );
    d.cache_render(
        "KTLX",
        RadarProduct::Reflectivity,
        squallar_radar::types::RenderView::PlanView,
        0.5,
        cached(2.0),
    );

    assert!(
        !d.set_env_heights("KTLX", heights(4.2, 7.1), &gui),
        "an identical refetch restarts the TTL and drops nothing",
    );
    assert_eq!(
        d.pane_render[0].last_rendered,
        Some((
            squallar_radar::types::RadarProduct::ProbabilityOfSevereHail,
            0.5
        )),
    );

    assert!(
        d.set_env_heights("KOUN", heights(1.0, 2.5), &gui),
        "another site's first sounding is a change there",
    );
    assert_eq!(
        d.pane_render[0].last_rendered,
        Some((
            squallar_radar::types::RadarProduct::ProbabilityOfSevereHail,
            0.5
        )),
        "another site's sounding must not touch this pane",
    );

    assert!(d.set_env_heights("KTLX", heights(4.4, 7.3), &gui));
    assert_eq!(
        d.pane_render[0].last_rendered, None,
        "a hail pane drawn against the old pair has to be redrawn",
    );
    assert!(
        d.get_cached_render(
            "KTLX",
            RadarProduct::MaxExpectedHailSize,
            squallar_radar::types::RenderView::PlanView,
            0.5
        )
        .is_none(),
        "the shared cache is keyed on (site, product, elevation), which \
             the environment is not part of",
    );
    assert!(
        d.get_cached_render(
            "KTLX",
            RadarProduct::Reflectivity,
            squallar_radar::types::RenderView::PlanView,
            0.5
        )
        .is_some(),
        "an unrelated product keeps its frame",
    );
}

/// The defect: `set_env_heights` invalidated the hail pair and nothing else, while
/// `env_heights_km_msl_for` had already grown a third consumer.
#[test]
fn a_moved_sounding_drops_every_render_that_read_the_old_environment() {
    let heights = |h0: f64, hm20: f64| squallar_radar::sounding::EnvHeights {
        h0c_km_msl: h0,
        hm20c_km_msl: hm20,
        fetched_at: chrono::Utc::now(),
    };
    let gui = gui_showing("KTLX");

    for &product in RadarProduct::all() {
        let consumes = product.reads_env_heights();

        let mut d = RenderDispatcher::new();
        d.ensure_pane_count(1);
        // A pane already showing this product, and a cached frame for it, both drawn
        // against the first pair.
        d.set_env_heights("KTLX", heights(4.2, 7.1), &gui);
        d.pane_render[0].last_rendered = Some((product, 0.5));
        d.cache_render(
            "KTLX",
            product,
            squallar_radar::types::RenderView::PlanView,
            0.5,
            cached(1.0),
        );

        // The other half of the agreement, and the one a mutation survived: invalidating a
        // pane is worth nothing if the redraw is then handed `None`.
        assert_eq!(
            d.env_heights_km_msl_for(product, "KTLX").is_some(),
            consumes,
            "{product:?}: the render parameters carry the pair exactly when \
             the product reads it",
        );

        // The sounding refetches and the environment has moved.
        assert!(
            d.set_env_heights("KTLX", heights(2.0, 5.0), &gui),
            "{product:?}: a moved pair is a change",
        );

        assert_eq!(
            d.pane_render[0].last_rendered.is_none(),
            consumes,
            "{product:?}: a pane is dropped exactly when its picture read the \
             environment (reads_env_heights = {consumes})",
        );
        assert_eq!(
            d.get_cached_render(
                "KTLX",
                product,
                squallar_radar::types::RenderView::PlanView,
                0.5,
            )
            .is_none(),
            consumes,
            "{product:?}: the cached frame is dropped exactly when it read the \
             environment (reads_env_heights = {consumes})",
        );
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn dispatch(
    d: &mut RenderDispatcher,
    pane_idx: usize,
    results: &mpsc::Sender<RenderResponse>,
) -> mpsc::Sender<()> {
    let (release, render) = gated_render();
    d.spawn_render(
        pane_idx,
        "KOUN",
        RadarProduct::Reflectivity,
        0.5,
        results.clone(),
        None,
        render,
    );
    release
}

/// How many renders were not abandoned.
#[cfg(not(target_arch = "wasm32"))]
fn arrivals(results: mpsc::Sender<RenderResponse>, rx: mpsc::Receiver<RenderResponse>) -> usize {
    drop(results);
    rx.iter().count()
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_scan_for_one_site_leaves_another_sites_render_alone() {
    let gui = gui_showing("KOUN");
    let mut d = RenderDispatcher::new();
    let (results, rx) = mpsc::channel();
    let release = dispatch(&mut d, 0, &results);

    // A scan for the other site lands while the KOUN pane is still rendering.
    let generation = d.render_generation;
    d.reset_panes_for_site("KTLX", &gui);
    assert!(
        !d.is_render_stale(generation),
        "a per-site reset must not move the global generation — the receiver \
             compares every pane against it"
    );

    release.send(()).expect("the render is still running");
    assert_eq!(
        arrivals(results, rx),
        1,
        "the KOUN pane's render was thrown away for a KTLX scan"
    );
}

/// The other half: a scan for the pane's own site does invalidate it, or the pane paints
/// the previous volume over the new one and then stops, since `last_rendered` records that
/// render as the one it is showing.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_scan_for_the_panes_own_site_abandons_its_render() {
    let gui = gui_showing("KOUN");
    let mut d = RenderDispatcher::new();
    let (results, rx) = mpsc::channel();
    let release = dispatch(&mut d, 0, &results);

    d.reset_panes_for_site("KOUN", &gui);
    assert!(
        !d.pane_render[0].render_in_flight(),
        "the pairing an abandoned send depends on: the pane must not be left \
             waiting for a result that will never come"
    );

    release.send(()).expect("the render is still running");
    assert_eq!(arrivals(results, rx), 0);
}

/// A pane can have more than one render running: the reset above clears `render_in_flight`
/// while the first is still going, so the next dispatch starts a second.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn every_render_a_pane_has_running_is_abandoned_at_once() {
    let gui = gui_showing("KOUN");
    let mut d = RenderDispatcher::new();
    let (results, rx) = mpsc::channel();
    let first = dispatch(&mut d, 0, &results);
    let second = dispatch(&mut d, 0, &results);

    d.reset_panes_for_site("KOUN", &gui);

    second.send(()).expect("both renders are still running");
    first.send(()).expect("both renders are still running");
    assert_eq!(arrivals(results, rx), 0);
}

/// A full reset is site-blind by design — surface loss, a layout change — and keeps
/// discarding everything.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_full_reset_abandons_every_panes_render() {
    let mut d = RenderDispatcher::new();
    let (results, rx) = mpsc::channel();
    let release = dispatch(&mut d, 0, &results);

    let generation = d.render_generation;
    d.reset_panes();
    assert!(
        d.is_render_stale(generation),
        "and the global generation still moves, so a result already in the \
             channel is discarded on arrival"
    );

    release.send(()).expect("the render is still running");
    assert_eq!(arrivals(results, rx), 0);
}

/// The lock-out this closes: a render that finds no sweep used to send nothing at all.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_render_that_finds_nothing_still_reports_back() {
    let mut d = RenderDispatcher::new();
    let (results, rx) = mpsc::channel();
    let (release, nothing) = gated_nothing();
    d.spawn_render(
        0,
        "KOUN",
        RadarProduct::Reflectivity,
        0.5,
        results.clone(),
        None,
        nothing,
    );

    release.send(()).expect("the render is still running");
    drop(results);
    let replies: Vec<_> = rx.iter().collect();
    assert_eq!(
        replies.len(),
        1,
        "a render with nothing to draw stayed silent, so its pane is still \
             marked in flight and will never dispatch again"
    );
    assert!(
        replies[0].rendered.is_none(),
        "there was no sweep to draw, but a frame arrived anyway"
    );
}

/// The counterweight, and the reason the report is gated on `results_wanted` rather than
/// sent unconditionally: an abandoned render must stay silent.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn an_abandoned_render_that_finds_nothing_reports_nothing() {
    let gui = gui_showing("KOUN");
    let mut d = RenderDispatcher::new();
    let (results, rx) = mpsc::channel();
    let (release, nothing) = gated_nothing();
    d.spawn_render(
        0,
        "KOUN",
        RadarProduct::Reflectivity,
        0.5,
        results.clone(),
        None,
        nothing,
    );

    d.reset_panes_for_site("KOUN", &gui);

    release.send(()).expect("the render is still running");
    assert_eq!(arrivals(results, rx), 0);
}

/// One pane on `site` showing `product`, with `available` as the tilt list its selection
/// snaps within.
fn gui_on_tilt(
    site: &str,
    product: RadarProduct,
    selected: f32,
    available: &[f32],
) -> squallar_egui::Gui {
    use squallar_radar::sites::RadarSite;
    use squallar_radar::types::ScanInfo;
    let mut gui = squallar_egui::Gui::new();
    let pane = gui.pane_mut(0).expect("a fresh Gui has one pane");
    pane.set_site(site.to_string());
    pane.set_selected_product(squallar_radar::fields::spec(product).id.clone());
    pane.set_selected_elevation(selected);
    let mut product_elevations = std::collections::HashMap::new();
    product_elevations.insert(product, available.to_vec());
    pane.scan_info = Some(ScanInfo {
        site_source: squallar_radar::site_position::SitePositionSource::Table,
        site_position: None,
        site: RadarSite {
            name: "KOUN",
            network: squallar_radar::sites::RadarNetwork::of_id("KOUN"),
            lat: 35.2,
            lon: -97.4,
            heights: None,
        },
        timestamp: chrono::NaiveDate::from_ymd_opt(2026, 7, 28)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap(),
        vcp_number: 212,
        available_products: vec![product],
        product_elevations,
        status: String::new(),
    });
    gui
}

fn cached(range: f64) -> CachedRenderOutput {
    CachedRenderOutput {
        image: Arc::new(egui::ColorImage::default()),
        max_range_km: range,
        hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
    }
}

/// The defect this avoids: a cut completing in the real-time feed changes one sweep, not
/// the volume, so a pane on another tilt is still showing a correct image.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_finished_tilt_leaves_a_pane_on_another_tilt_alone() {
    let gui = gui_on_tilt("KOUN", RadarProduct::Reflectivity, 4.0, &[0.5, 4.0]);
    let mut d = RenderDispatcher::new();
    d.ensure_pane_count(1);
    let (results, rx) = mpsc::channel();
    let release = dispatch(&mut d, 0, &results);

    assert_eq!(
        d.reset_panes_for_tilts("KOUN", &gui, &[0.5]),
        0,
        "the 4.0° pane was invalidated by a 0.5° cut completing"
    );
    assert!(d.pane_render[0].render_in_flight());

    release.send(()).expect("still running");
    assert_eq!(
        arrivals(results, rx),
        1,
        "its render should survive: the image it is showing is still correct"
    );
}

/// The counterweight: the pane whose tilt it was must be invalidated, or the new sweep
/// never reaches the screen.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_finished_tilt_invalidates_the_pane_showing_it() {
    let gui = gui_on_tilt("KOUN", RadarProduct::Reflectivity, 0.5, &[0.5, 4.0]);
    let mut d = RenderDispatcher::new();
    d.ensure_pane_count(1);
    let (results, rx) = mpsc::channel();
    let release = dispatch(&mut d, 0, &results);

    assert_eq!(d.reset_panes_for_tilts("KOUN", &gui, &[0.5]), 1);
    assert!(
        !d.pane_render[0].render_in_flight(),
        "the pairing an abandoned send depends on"
    );
    release.send(()).expect("still running");
    assert_eq!(arrivals(results, rx), 0);
}

/// Echo tops integrates every reflectivity tilt and clamps each column to the topmost one
/// present, so a partial volume gives a plausible, low, wrong number with no error and no
/// NaN.
#[test]
fn a_finished_tilt_leaves_the_volumetric_pane_for_the_closing_volume() {
    let gui = gui_on_tilt("KOUN", RadarProduct::EchoTopsInterpolated, 0.5, &[0.5]);
    let mut d = RenderDispatcher::new();
    d.ensure_pane_count(1);

    assert_eq!(
        d.reset_panes_for_tilts("KOUN", &gui, &[0.5]),
        0,
        "echo tops was invalidated by a single cut completing"
    );
}

/// NROT fits its wind profile from every velocity tilt — the only wind source since the NVW
/// fetch left — so it is volume-wide too, and only the closing volume refreshes it.
#[test]
fn nrot_waits_for_the_volume() {
    let gui = gui_on_tilt("KOUN", RadarProduct::NormalizedRotation, 0.5, &[0.5]);
    let mut d = RenderDispatcher::new();
    d.ensure_pane_count(1);

    assert_eq!(
        d.reset_panes_for_tilts("KOUN", &gui, &[0.5]),
        0,
        "NROT fits its profile from every velocity tilt, so a partial \
             volume would halve its shear"
    );
}

/// SRV reads the same profile, for its dealias seed and for its default Bunkers vector, so
/// it belongs on the same side of the split.
#[test]
fn srv_waits_for_the_volume() {
    let gui = gui_on_tilt("KOUN", RadarProduct::StormRelativeVelocity, 0.5, &[0.5]);
    let mut d = RenderDispatcher::new();
    d.ensure_pane_count(1);

    assert_eq!(
        d.reset_panes_for_tilts("KOUN", &gui, &[0.5]),
        0,
        "SRV re-rendered off a single completed cut, fitting its hodograph \
             from whatever velocity tilts had arrived"
    );
}

/// A Level III pane's pixels come from `level3_data`; a Level II cut completing says
/// nothing about them, and its tilts are refetched only when the volume closes.
#[test]
fn a_finished_tilt_does_not_touch_a_level3_pane() {
    let gui = gui_on_tilt(
        "KOUN",
        RadarProduct::VerticallyIntegratedLiquid,
        0.5,
        &[0.5],
    );
    let mut d = RenderDispatcher::new();
    d.ensure_pane_count(1);
    assert_eq!(d.reset_panes_for_tilts("KOUN", &gui, &[0.5]), 0);
}

/// The other side of every skip above: what the tilt reset passes over, the site reset
/// takes.
#[test]
fn every_product_a_tilt_reset_skips_is_taken_by_a_site_reset() {
    let mut skipped = 0;
    let mut taken_by_tilts = 0;
    for &product in RadarProduct::all() {
        let gui = gui_on_tilt("KOUN", product, 0.5, &[0.5]);
        let mut d = RenderDispatcher::new();
        d.ensure_pane_count(1);

        if d.reset_panes_for_tilts("KOUN", &gui, &[0.5]) == 1 {
            taken_by_tilts += 1;
            continue;
        }
        skipped += 1;
        d.pane_render[0].last_rendered = Some((product, 0.5));
        d.cache_render(
            "KOUN",
            product,
            squallar_radar::types::RenderView::PlanView,
            0.5,
            cached(1.0),
        );

        d.reset_panes_for_site("KOUN", &gui);

        assert!(
            d.pane_render[0].last_rendered.is_none(),
            "{product:?} is skipped by the tilt reset and not picked up by the \
                 site reset either, so nothing refreshes it while the site is live",
        );
        assert!(
            d.get_cached_render(
                "KOUN",
                product,
                squallar_radar::types::RenderView::PlanView,
                0.5
            )
            .is_none(),
            "{product:?}'s stale image survived the site reset, so the pane \
                 re-renders straight back into it",
        );
    }
    // precondition: both arms ran.
    assert!(
        skipped > 0 && taken_by_tilts > 0,
        "the tilt reset put every product on one side: {skipped} skipped, \
             {taken_by_tilts} taken",
    );
}

/// A whole-site `render_cache.retain` would throw away the images the panes this reset
/// deliberately left alone are still sharing.
#[test]
fn a_tilt_reset_keeps_the_other_tilts_cached_renders() {
    let gui = gui_on_tilt("KOUN", RadarProduct::Reflectivity, 0.5, &[0.5, 4.0]);
    let mut d = RenderDispatcher::new();
    d.ensure_pane_count(1);
    d.cache_render(
        "KOUN",
        RadarProduct::Reflectivity,
        squallar_radar::types::RenderView::PlanView,
        0.5,
        cached(1.0),
    );
    d.cache_render(
        "KOUN",
        RadarProduct::Reflectivity,
        squallar_radar::types::RenderView::PlanView,
        4.0,
        cached(2.0),
    );

    d.reset_panes_for_tilts("KOUN", &gui, &[0.5]);
    assert!(
        d.get_cached_render(
            "KOUN",
            RadarProduct::Reflectivity,
            squallar_radar::types::RenderView::PlanView,
            0.5
        )
        .is_none(),
        "the completed tilt's stale image survived"
    );
    assert!(
        d.get_cached_render(
            "KOUN",
            RadarProduct::Reflectivity,
            squallar_radar::types::RenderView::PlanView,
            4.0
        )
        .is_some(),
        "an untouched tilt's image was evicted with it"
    );
}

/// A tilt-independent plan view has no tilt for a tilt reset to name, so a completed 0.0°
/// sweep does not evict it.
#[test]
fn a_tilt_reset_keeps_a_tilt_independent_plan_views_cached_render() {
    use squallar_radar::types::RenderView;
    // Found rather than named, so the pin cannot rot into testing the ordinary path if the
    // set is re-cut.
    let tilt_blind = *RadarProduct::all()
        .iter()
        .find(|p| p.tilt_independent_plan_view())
        .expect(
            "premise: some product must key with no elevation part, or there is \
             nothing here to be immune",
        );
    assert!(
        !RadarProduct::Reflectivity.tilt_independent_plan_view(),
        "premise: the control product must be tilt-dependent",
    );

    let gui = gui_on_tilt("KOUN", RadarProduct::Reflectivity, 0.0, &[0.0, 4.0]);
    let mut d = RenderDispatcher::new();
    d.ensure_pane_count(1);
    d.cache_render("KOUN", tilt_blind, RenderView::PlanView, 3.0, cached(1.0));
    // The control, and the reason this test cannot pass on a reset that evicts nothing: a
    // tilt-dependent entry in the 0.0° bucket must still go.
    d.cache_render(
        "KOUN",
        RadarProduct::Reflectivity,
        RenderView::PlanView,
        0.0,
        cached(2.0),
    );

    d.reset_panes_for_tilts("KOUN", &gui, &[0.0]);

    assert!(
        d.get_cached_render("KOUN", tilt_blind, RenderView::PlanView, 3.0)
            .is_some(),
        "a 0.0° sweep evicted {tilt_blind:?}, a tilt-independent plan view — the sentinel is \
         back, and the picture it threw away is the same at every tilt",
    );
    assert!(
        d.get_cached_render(
            "KOUN",
            RadarProduct::Reflectivity,
            RenderView::PlanView,
            0.0
        )
        .is_none(),
        "control: the completed tilt's own stale image survived, so this reset \
         evicted nothing and the assertion above proves nothing",
    );
}

/// The flag list is bounded by what is still stoppable, not by how many renders a session
/// has dispatched.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn finished_renders_stop_being_tracked() {
    let mut d = RenderDispatcher::new();
    let (results, rx) = mpsc::channel();
    for _ in 0..5 {
        let release = dispatch(&mut d, 0, &results);
        release.send(()).expect("the render is still running");
        // Taking the result is what proves the render released its flag: it let go of it
        // before sending, so this `recv` is ordered after that release.
        rx.recv().expect("an unabandoned render arrives");
    }
    // Each dispatch prunes before pushing, and every render but the last has answered, so
    // exactly the one just added is held.
    assert_eq!(
        d.pane_render[0].results_wanted.len(),
        1,
        "flags accumulated for renders that had already answered",
    );
}

/// The defect this closes is not "the classification is slightly off".
#[test]
fn a_melting_layer_object_is_only_ever_applied_to_the_volume_it_names() {
    let volume = |minute: u32| {
        chrono::NaiveDate::from_ymd_opt(2026, 1, 15)
            .unwrap()
            .and_hms_opt(12, minute, 0)
            .unwrap()
    };
    let object = |start: chrono::NaiveDateTime| MeltingLayerObject {
        volume_start: start,
        bytes: Arc::new(vec![0xAB; 8]),
    };
    let hca = RadarProduct::HydrometeorClassification;

    let mut d = RenderDispatcher::new();
    let gui = gui_showing("KTLX");
    d.ensure_pane_count(1);

    assert!(
        d.melting_layer_product_for(hca, "KTLX", volume(0))
            .is_none(),
        "with nothing cached the classification must fall back, not invent",
    );

    assert!(
        d.set_melting_layer("KTLX", object(volume(0)), &gui),
        "the first object for a site is a change from nothing",
    );
    assert!(
        d.melting_layer_product_for(hca, "KTLX", volume(0))
            .is_some(),
        "the object for this very volume must reach the render",
    );

    // The volume rolls.
    for other in [volume(6), volume(12), volume(1)] {
        assert!(
            d.melting_layer_product_for(hca, "KTLX", other).is_none(),
            "an object naming {} was handed to a render of {other}",
            volume(0),
        );
    }

    // Per site, on the same terms: KOUN's volume happens to start at the same instant and
    // still gets nothing, because it has no object of its own.
    assert!(
        d.melting_layer_product_for(hca, "KOUN", volume(0))
            .is_none(),
        "one site's melting layer was applied to another site's volume",
    );

    // And no other product reads one, whatever is cached.
    for product in [
        RadarProduct::Reflectivity,
        RadarProduct::CorrelationCoefficient,
        RadarProduct::ProbabilityOfSevereHail,
    ] {
        assert!(
            d.melting_layer_product_for(product, "KTLX", volume(0))
                .is_none(),
            "{product:?} was handed a melting layer it does not classify with",
        );
    }
}

/// A landed object drops exactly the classification renders that were drawn without it —
/// the per-volume sibling of the sounding invalidation above.
#[test]
fn a_landed_melting_layer_drops_the_classification_renders_that_missed_it() {
    let volume = |minute: u32| {
        chrono::NaiveDate::from_ymd_opt(2026, 1, 15)
            .unwrap()
            .and_hms_opt(12, minute, 0)
            .unwrap()
    };
    let object = |start: chrono::NaiveDateTime| MeltingLayerObject {
        volume_start: start,
        bytes: Arc::new(vec![0xAB; 8]),
    };

    let mut d = RenderDispatcher::new();
    let gui = gui_showing("KTLX");
    d.ensure_pane_count(1);

    d.pane_render[0].last_rendered = Some((RadarProduct::HydrometeorClassification, 0.5));
    d.cache_render(
        "KTLX",
        RadarProduct::HydrometeorClassification,
        squallar_radar::types::RenderView::PlanView,
        0.5,
        cached(1.0),
    );
    d.cache_render(
        "KTLX",
        RadarProduct::Reflectivity,
        squallar_radar::types::RenderView::PlanView,
        0.5,
        cached(2.0),
    );

    assert!(d.set_melting_layer("KTLX", object(volume(0)), &gui));
    assert_eq!(
        d.pane_render[0].last_rendered, None,
        "the classification pane kept a picture drawn without the object",
    );
    assert!(
        d.get_cached_render(
            "KTLX",
            RadarProduct::HydrometeorClassification,
            squallar_radar::types::RenderView::PlanView,
            0.5,
        )
        .is_none(),
        "the shared cache would hand the pre-object raster straight back",
    );
    assert!(
        d.get_cached_render(
            "KTLX",
            RadarProduct::Reflectivity,
            squallar_radar::types::RenderView::PlanView,
            0.5,
        )
        .is_some(),
        "reflectivity classifies nothing and must not be redrawn",
    );

    // A refetch of the same volume's object changes nothing and drops nothing: the answer
    // for that volume is already on screen.
    d.pane_render[0].last_rendered = Some((RadarProduct::HydrometeorClassification, 0.5));
    assert!(
        !d.set_melting_layer("KTLX", object(volume(0)), &gui),
        "an object for the volume already in hand is not a change",
    );
    assert_eq!(
        d.pane_render[0].last_rendered,
        Some((
            squallar_radar::types::RadarProduct::HydrometeorClassification,
            0.5
        )),
    );

    // The next volume's object is, and the cache moves with it.
    assert!(d.set_melting_layer("KTLX", object(volume(6)), &gui));
    assert_eq!(d.melting_layer_volume("KTLX"), Some(volume(6)));
    assert!(
        d.melting_layer_product_for(RadarProduct::HydrometeorClassification, "KTLX", volume(0))
            .is_none(),
        "the previous volume's object survived the replacement",
    );
}

/// [`a_melting_layer_object_is_only_ever_applied_to_the_volume_it_names`]'s sibling, and
/// the defect it closes has the same shape with a sharper edge.
#[test]
fn a_storm_motion_vector_is_only_ever_applied_to_the_volume_it_names() {
    let volume = |minute: u32| {
        chrono::NaiveDate::from_ymd_opt(2026, 1, 15)
            .unwrap()
            .and_hms_opt(12, minute, 0)
            .unwrap()
    };
    let object = |start: chrono::NaiveDateTime| StormMotionObject {
        volume_start: start,
        motion: (34.0, 225.0),
    };
    let srv = RadarProduct::StormRelativeVelocity;

    let mut d = RenderDispatcher::new();
    let gui = gui_showing("KTLX");
    d.ensure_pane_count(1);

    assert_eq!(
        d.rpg_storm_motion_for(srv, "KTLX", volume(0)),
        None,
        "with nothing cached SRV must fall back, not invent",
    );

    assert!(
        d.set_storm_motion("KTLX", object(volume(0)), &gui),
        "the first vector for a site is a change from nothing",
    );
    assert_eq!(
        d.rpg_storm_motion_for(srv, "KTLX", volume(0)),
        Some((34.0, 225.0)),
        "the vector for this very volume must reach the render",
    );

    // The volume rolls.
    for other in [volume(6), volume(12), volume(1)] {
        assert_eq!(
            d.rpg_storm_motion_for(srv, "KTLX", other),
            None,
            "a vector naming {} was handed to a render of {other}",
            volume(0),
        );
    }

    // Per site, on the same terms: KOUN's volume happens to start at the same instant and
    // still gets nothing, because it has no vector of its own.
    assert_eq!(
        d.rpg_storm_motion_for(srv, "KOUN", volume(0)),
        None,
        "one site's storm motion was applied to another site's volume",
    );

    // And no other product applies one, whatever is cached.
    for product in [
        RadarProduct::Reflectivity,
        RadarProduct::Velocity,
        RadarProduct::CorrelationCoefficient,
    ] {
        assert_eq!(
            d.rpg_storm_motion_for(product, "KTLX", volume(0)),
            None,
            "{product:?} was handed a storm motion vector it does not shift by",
        );
    }

    // The replacement discipline, as the melting layer's: the same volume is not a change,
    // the next one is, and the previous vector does not survive.
    assert!(
        !d.set_storm_motion("KTLX", object(volume(0)), &gui),
        "a vector for the volume already in hand is not a change",
    );
    assert!(d.set_storm_motion("KTLX", object(volume(6)), &gui));
    assert_eq!(d.storm_motion_volume("KTLX"), Some(volume(6)));
    assert_eq!(
        d.rpg_storm_motion_for(srv, "KTLX", volume(0)),
        None,
        "the previous volume's vector survived the replacement",
    );
}

/// This one has no melting-layer analogue, and it is the trap this path is most likely to
/// fall into: `0.0 kt from 0.0°` looks exactly like an uninitialised pair, and every
/// instinct in a codebase full of `Option`-shaped absences says to treat it as one.
#[test]
fn a_zero_storm_motion_vector_is_a_reading_and_is_carried_like_any_other() {
    let volume = |minute: u32| {
        chrono::NaiveDate::from_ymd_opt(2026, 1, 15)
            .unwrap()
            .and_hms_opt(12, minute, 0)
            .unwrap()
    };
    let srv = RadarProduct::StormRelativeVelocity;

    let mut d = RenderDispatcher::new();
    let gui = gui_showing("KTLX");
    d.ensure_pane_count(1);

    let zero = StormMotionObject {
        volume_start: volume(0),
        motion: (0.0, 0.0),
    };
    assert!(
        d.set_storm_motion("KTLX", zero, &gui),
        "a zero vector landing is a change from nothing, exactly as any other \
         reading is — a `set` that treated it as no news would leave the pane \
         on whatever rung it had already drawn",
    );
    assert_eq!(
        d.storm_motion_volume("KTLX"),
        Some(volume(0)),
        "a zero vector was not recorded as an object of this volume, so the \
         fetch gate would re-download it on every poll",
    );
    assert_eq!(
        d.rpg_storm_motion_for(srv, "KTLX", volume(0)),
        Some((0.0, 0.0)),
        "a zero vector was withheld from the render, which then shifts the \
         field by a derived rung the RPG never applied",
    );

    // And the volume gate still holds over it.
    assert_eq!(
        d.rpg_storm_motion_for(srv, "KTLX", volume(6)),
        None,
        "an unshifted volume's zero was handed to a volume that never claimed it",
    );

    // Replacing a zero with a real vector is a change; the comparison is on the volume, so
    // this cannot be reached by comparing the pairs.
    assert!(d.set_storm_motion(
        "KTLX",
        StormMotionObject {
            volume_start: volume(6),
            motion: (41.0, 190.0),
        },
        &gui,
    ));
    assert_eq!(
        d.rpg_storm_motion_for(srv, "KTLX", volume(6)),
        Some((41.0, 190.0)),
    );
}

// ── A loop frame is keyed by the archive, the cache by the first radial ────

/// The two timestamps for one volume, built exactly as the two production routes build
/// them: `(cached-side, loop-frame-side)`.
fn the_two_statements_of_one_volume(
    hms: &str,
    millis_into_the_second: i64,
) -> (chrono::NaiveDateTime, chrono::NaiveDateTime) {
    let date = chrono::NaiveDate::from_ymd_opt(2026, 3, 2).expect("a real date");
    let from_key = date.and_time(
        chrono::NaiveTime::parse_from_str(hms, "%H%M%S").expect("a real archive key tail"),
    );
    let from_radial = chrono::DateTime::from_timestamp_millis(
        from_key.and_utc().timestamp_millis() + millis_into_the_second,
    )
    .expect("a real collection timestamp")
    .naive_utc();
    (from_radial, from_key)
}

/// cached for the very volume it draws.
#[test]
fn a_loop_frame_keyed_by_the_archive_reaches_this_volumes_melting_layer() {
    let hca = RadarProduct::HydrometeorClassification;
    let gui = gui_showing("KTLX");

    for millis in [1, 517, 993] {
        let (from_radial, from_key) = the_two_statements_of_one_volume("120347", millis);
        assert_ne!(
            from_radial, from_key,
            "premise: the two routes never state one volume identically",
        );

        let mut d = RenderDispatcher::new();
        d.ensure_pane_count(1);
        assert!(d.set_melting_layer(
            "KTLX",
            MeltingLayerObject {
                volume_start: from_radial,
                bytes: Arc::new(vec![0xAB; 8]),
            },
            &gui,
        ));

        assert!(
            d.melting_layer_product_for(hca, "KTLX", from_radial)
                .is_some(),
            "premise: the still frame, where both sides come off the radial",
        );
        assert!(
            d.melting_layer_product_for(hca, "KTLX", from_key).is_some(),
            "the loop frame keyed {from_key} was refused the object measured \
             for it at {from_radial}",
        );
    }
}

/// [`a_loop_frame_keyed_by_the_archive_reaches_this_volumes_melting_layer`]'s sibling, in
/// SRV's terms: a loop frame reaches the `N0S` vector fetched for the volume it draws.
#[test]
fn a_loop_frame_keyed_by_the_archive_reaches_this_volumes_storm_motion() {
    let srv = RadarProduct::StormRelativeVelocity;
    let gui = gui_showing("KTLX");

    for millis in [1, 517, 993] {
        let (from_radial, from_key) = the_two_statements_of_one_volume("120347", millis);

        let mut d = RenderDispatcher::new();
        d.ensure_pane_count(1);
        assert!(d.set_storm_motion(
            "KTLX",
            StormMotionObject {
                volume_start: from_radial,
                motion: (34.0, 225.0),
            },
            &gui,
        ));

        assert_eq!(
            d.rpg_storm_motion_for(srv, "KTLX", from_radial),
            Some((34.0, 225.0)),
            "premise: the still frame, where both sides come off the radial",
        );
        assert_eq!(
            d.rpg_storm_motion_for(srv, "KTLX", from_key),
            Some((34.0, 225.0)),
            "the loop frame keyed {from_key} was refused the vector fetched \
             for it at {from_radial}",
        );
    }
}

/// The widening stops at the volume boundary: a frame one scan cycle away gets nothing,
/// whichever way either start was stated.
#[test]
fn a_frame_one_scan_cycle_away_reaches_neither_the_melting_layer_nor_the_motion() {
    let hca = RadarProduct::HydrometeorClassification;
    let srv = RadarProduct::StormRelativeVelocity;
    let gui = gui_showing("KTLX");

    let (from_radial, from_key) = the_two_statements_of_one_volume("120347", 517);

    let mut d = RenderDispatcher::new();
    d.ensure_pane_count(1);
    assert!(d.set_melting_layer(
        "KTLX",
        MeltingLayerObject {
            volume_start: from_radial,
            bytes: Arc::new(vec![0xAB; 8]),
        },
        &gui,
    ));
    assert!(d.set_storm_motion(
        "KTLX",
        StormMotionObject {
            volume_start: from_radial,
            motion: (34.0, 225.0),
        },
        &gui,
    ));

    // The shortest measured WSR-88D volume interval, then the nominal precip, TDWR and
    // clear-air figures — and one second, the finest step the archive key can express,
    // which is already a different volume.
    for gap in [1, 198, 259, 360, 517] {
        for neighbour in [
            from_radial + chrono::Duration::seconds(gap),
            from_radial - chrono::Duration::seconds(gap),
            from_key + chrono::Duration::seconds(gap),
            from_key - chrono::Duration::seconds(gap),
        ] {
            assert!(
                d.melting_layer_product_for(hca, "KTLX", neighbour)
                    .is_none(),
                "a melting layer measured at {from_radial} reached a frame \
                 {gap} s away at {neighbour}",
            );
            assert_eq!(
                d.rpg_storm_motion_for(srv, "KTLX", neighbour),
                None,
                "a storm motion fetched for {from_radial} reached a frame \
                 {gap} s away at {neighbour}",
            );
        }
    }
}
