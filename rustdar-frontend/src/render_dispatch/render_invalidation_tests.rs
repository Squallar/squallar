use super::*;
use std::sync::mpsc;

/// A render that does not finish until the test releases it.
///
/// The gate is the whole point: a reset only has something to act on while a
/// render is *running*, and a render of nothing would routinely finish before
/// the reset landed, so the test would pass on timing rather than on the
/// abandonment.
///
/// Deliberately a `Job::Opaque`: it has to *block*, and a described job is
/// executed by the funnel with no handle to hold it open. What is under
/// test is the abandonment protocol around a running render, which is the
/// same for both job shapes — `deliver` carries the flag either way.
fn gated_render() -> (mpsc::Sender<()>, crate::offload::Job) {
    let (release, held) = mpsc::channel::<()>();
    (
        release,
        crate::offload::Job::Opaque(Box::new(move || {
            held.recv().expect("every gated render is released");
            Some(crate::offload::JobOutput::Frame(
                (Vec::new(), 230.0, Vec::new()).into(),
            ))
        })),
    )
}

/// [`gated_render`] for a render that answers nothing — what
/// `Job::renders_nothing` produces when no sweep carries the product, held
/// open so the abandonment protocol can be exercised around it.
fn gated_nothing() -> (mpsc::Sender<()>, crate::offload::Job) {
    let (release, held) = mpsc::channel::<()>();
    (
        release,
        crate::offload::Job::Opaque(Box::new(move || {
            held.recv().expect("every gated render is released");
            None
        })),
    )
}

/// One pane, on `site`, which is how `reset_panes_for_site` reads the layout.
fn gui_showing(site: &str) -> rustdar_egui::Gui {
    let mut gui = rustdar_egui::Gui::new();
    gui.pane_mut(0).expect("a fresh Gui has one pane").site = site.to_string();
    gui
}

/// The environmental heights route into the hail render parameters from
/// the same map the sounding drain writes, and a moved pair drops exactly
/// that site's hail renders — the per-site sibling of
/// `changing_the_override_invalidates_the_storm_relative_renders`.
#[test]
fn a_landed_sounding_routes_into_hail_renders_and_a_moved_pair_drops_them() {
    let heights = |h0: f64, hm20: f64| rustdar_radar::sounding::EnvHeights {
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
        "only the hail pair reads the environment",
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
        rustdar_radar::types::RenderView::PlanView,
        0.5,
        cached(1.0),
    );
    d.cache_render(
        "KTLX",
        RadarProduct::Reflectivity,
        rustdar_radar::types::RenderView::PlanView,
        0.5,
        cached(2.0),
    );

    assert!(
        !d.set_env_heights("KTLX", heights(4.2, 7.1), &gui),
        "an identical refetch restarts the TTL and drops nothing",
    );
    assert_eq!(
        d.pane_render[0].last_rendered,
        Some((RadarProduct::ProbabilityOfSevereHail, 0.5)),
    );

    assert!(
        d.set_env_heights("KOUN", heights(1.0, 2.5), &gui),
        "another site's first sounding is a change there",
    );
    assert_eq!(
        d.pane_render[0].last_rendered,
        Some((RadarProduct::ProbabilityOfSevereHail, 0.5)),
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
            rustdar_radar::types::RenderView::PlanView,
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
            rustdar_radar::types::RenderView::PlanView,
            0.5
        )
        .is_some(),
        "an unrelated product keeps its frame",
    );
}

fn dispatch(
    d: &mut RenderDispatcher,
    pane_idx: usize,
    results: &mpsc::Sender<RenderResponse>,
) -> mpsc::Sender<()> {
    let (release, render) = gated_render();
    d.spawn_render(
        pane_idx,
        RadarProduct::Reflectivity,
        0.5,
        results.clone(),
        None,
        render,
    );
    release
}

/// How many renders were not abandoned. Ends when the last worker drops its
/// sender, so nothing here waits on a timeout.
fn arrivals(results: mpsc::Sender<RenderResponse>, rx: mpsc::Receiver<RenderResponse>) -> usize {
    drop(results);
    rx.iter().count()
}

/// The defect: a scan arriving for one site bumped a single global generation,
/// so every pane on every *other* site had its in-flight render discarded at
/// the receiver and respawned — a 2048² image and value grid redone per pane
/// per poll, recurring every interval in any multi-site layout.
#[test]
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

/// The other half: a scan for the pane's own site does invalidate it, or the
/// pane paints the previous volume over the new one and then stops, since
/// `last_rendered` records that render as the one it is showing.
#[test]
fn a_scan_for_the_panes_own_site_abandons_its_render() {
    let gui = gui_showing("KOUN");
    let mut d = RenderDispatcher::new();
    let (results, rx) = mpsc::channel();
    let release = dispatch(&mut d, 0, &results);

    d.reset_panes_for_site("KOUN", &gui);
    assert!(
        !d.pane_render[0].render_in_flight,
        "the pairing an abandoned send depends on: the pane must not be left \
             waiting for a result that will never come"
    );

    release.send(()).expect("the render is still running");
    assert_eq!(arrivals(results, rx), 0);
}

/// A pane can have more than one render running: the reset above clears
/// `render_in_flight` while the first is still going, so the next dispatch
/// starts a second. Abandoning only the newest would leave the older free to
/// arrive last and paint the scan the reset was meant to replace.
#[test]
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

/// A full reset is site-blind by design — surface loss, a layout change — and
/// keeps discarding everything.
#[test]
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

/// The lock-out this closes: a render that finds no sweep used to send
/// nothing at all. `render_in_flight` is cleared by the receiver or by a
/// reset and nowhere else, and `dispatch_pane_renders` refuses to dispatch
/// while it is set — so the pane went quiet until something unrelated reset
/// it, and a user changing product saw nothing happen.
///
/// Rare against an archive volume, which carries every cut it will ever
/// have. Routine against a volume still being assembled from the real-time
/// chunk feed, where an upper tilt has simply not been scanned yet.
#[test]
fn a_render_that_finds_nothing_still_reports_back() {
    let mut d = RenderDispatcher::new();
    let (results, rx) = mpsc::channel();
    let (release, nothing) = gated_nothing();
    d.spawn_render(
        0,
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

/// The counterweight, and the reason the report is gated on `results_wanted`
/// rather than sent unconditionally: an abandoned render must stay silent.
/// Reporting would clear `render_in_flight` for the render that *superseded*
/// it, and the pane would dispatch a third while the second was still going.
#[test]
fn an_abandoned_render_that_finds_nothing_reports_nothing() {
    let gui = gui_showing("KOUN");
    let mut d = RenderDispatcher::new();
    let (results, rx) = mpsc::channel();
    let (release, nothing) = gated_nothing();
    d.spawn_render(
        0,
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

/// One pane on `site` showing `product`, with `available` as the tilt list
/// its selection snaps within.
///
/// One pane rather than several because `Gui::set_pane_count_for_test` is
/// `#[cfg(test)]` inside `rustdar-egui` and so does not exist for this
/// crate's tests. The property under test — that a reset picks panes by
/// their snapped tilt — is the same either way, and the pair of tests below
/// covers both answers.
fn gui_on_tilt(
    site: &str,
    product: RadarProduct,
    selected: f32,
    available: &[f32],
) -> rustdar_egui::Gui {
    use rustdar_radar::sites::RadarSite;
    use rustdar_radar::types::ScanInfo;
    let mut gui = rustdar_egui::Gui::new();
    let pane = gui.pane_mut(0).expect("a fresh Gui has one pane");
    pane.site = site.to_string();
    pane.selected_product = product;
    pane.selected_elevation = selected;
    let mut product_elevations = std::collections::HashMap::new();
    product_elevations.insert(product, available.to_vec());
    pane.scan_info = Some(ScanInfo {
        site: RadarSite {
            name: "KOUN",
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
        image_data: Arc::new(Vec::new()),
        max_range_km: range,
        value_data: Arc::new(Vec::new()),
    }
}

/// The defect this avoids: a cut completing in the real-time feed changes one
/// sweep, not the volume, so a pane on another tilt is still showing a
/// correct image. Resetting it dispatches a render whose `extract` answers
/// `None` — a wasted slot in the render budget, on every cut of every volume.
#[test]
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
    assert!(d.pane_render[0].render_in_flight);

    release.send(()).expect("still running");
    assert_eq!(
        arrivals(results, rx),
        1,
        "its render should survive: the image it is showing is still correct"
    );
}

/// The counterweight: the pane whose tilt it was must be invalidated, or the
/// new sweep never reaches the screen.
#[test]
fn a_finished_tilt_invalidates_the_pane_showing_it() {
    let gui = gui_on_tilt("KOUN", RadarProduct::Reflectivity, 0.5, &[0.5, 4.0]);
    let mut d = RenderDispatcher::new();
    d.ensure_pane_count(1);
    let (results, rx) = mpsc::channel();
    let release = dispatch(&mut d, 0, &results);

    assert_eq!(d.reset_panes_for_tilts("KOUN", &gui, &[0.5]), 1);
    assert!(
        !d.pane_render[0].render_in_flight,
        "the pairing an abandoned send depends on"
    );
    release.send(()).expect("still running");
    assert_eq!(arrivals(results, rx), 0);
}

/// Echo tops integrates every reflectivity tilt and clamps each column to the
/// topmost one present, so a partial volume gives a plausible, low, wrong
/// number with no error and no NaN. It must wait for the volume to close.
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

/// NROT fits its wind profile from every velocity tilt — the only wind
/// source since the NVW fetch left — so it is volume-wide too, and only
/// the closing volume refreshes it.
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

/// SRV reads the same profile, for its dealias seed and for its default
/// Bunkers vector, so it belongs on the same side of the split. The copy of
/// the predicate that used to live in this module left it off, so an SRV pane
/// was invalidated by every completed cut and re-rendered mid-volume, fitting
/// its hodograph from however many velocity tilts had landed so far. It was
/// still put right when the volume closed — that path is
/// `reset_panes_for_site`, which does not consult this predicate — so the
/// cost was wrong pixels in the meantime, plus a render slot per cut.
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

/// A Level III pane's pixels come from `level3_data`; a Level II cut
/// completing says nothing about them, and its tilts are refetched only when
/// the volume closes.
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

/// The other side of every skip above: what the tilt reset passes over, the
/// site reset takes.
///
/// Stated once, over every product, rather than as a second assertion inside
/// each of the four tests above. `reset_panes_for_site` does not consult the
/// product at all, so per-product repetitions of this would have been the same
/// claim four times — which is what the deleted `reset_panes_for_volume` was
/// doing there. What is worth pinning is that the skips are not a hole: a
/// product the tilt reset declines *and* a site reset declined would never be
/// refreshed at all while a site is live.
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
        // Cached *after* the tilt reset, not before: that reset's own
        // `render_cache.retain` is product-blind — it drops every entry for
        // the site at the angles it was given, whatever the pane is showing —
        // so an entry seeded earlier would already be gone and the assertion
        // below would pass without the site reset doing anything.
        d.cache_render(
            "KOUN",
            product,
            rustdar_radar::types::RenderView::PlanView,
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
                rustdar_radar::types::RenderView::PlanView,
                0.5
            )
            .is_none(),
            "{product:?}'s stale image survived the site reset, so the pane \
                 re-renders straight back into it",
        );
    }
    // precondition: both arms ran. A count of *how many* land on each side
    // would be a hand-maintained census of the product roster, which is the
    // defect this module already removed once — but with everything on one
    // side the loop body above proves nothing, so that much is asserted.
    assert!(
        skipped > 0 && taken_by_tilts > 0,
        "the tilt reset put every product on one side: {skipped} skipped, \
             {taken_by_tilts} taken",
    );
}

/// A whole-site `render_cache.retain` would throw away the images the panes
/// this reset deliberately left alone are still sharing.
#[test]
fn a_tilt_reset_keeps_the_other_tilts_cached_renders() {
    let gui = gui_on_tilt("KOUN", RadarProduct::Reflectivity, 0.5, &[0.5, 4.0]);
    let mut d = RenderDispatcher::new();
    d.ensure_pane_count(1);
    d.cache_render(
        "KOUN",
        RadarProduct::Reflectivity,
        rustdar_radar::types::RenderView::PlanView,
        0.5,
        cached(1.0),
    );
    d.cache_render(
        "KOUN",
        RadarProduct::Reflectivity,
        rustdar_radar::types::RenderView::PlanView,
        4.0,
        cached(2.0),
    );

    d.reset_panes_for_tilts("KOUN", &gui, &[0.5]);
    assert!(
        d.get_cached_render(
            "KOUN",
            RadarProduct::Reflectivity,
            rustdar_radar::types::RenderView::PlanView,
            0.5
        )
        .is_none(),
        "the completed tilt's stale image survived"
    );
    assert!(
        d.get_cached_render(
            "KOUN",
            RadarProduct::Reflectivity,
            rustdar_radar::types::RenderView::PlanView,
            4.0
        )
        .is_some(),
        "an untouched tilt's image was evicted with it"
    );
}

/// The flag list is bounded by what is actually running, not by how many
/// renders a session has dispatched.
#[test]
fn finished_renders_stop_being_tracked() {
    let mut d = RenderDispatcher::new();
    let (results, rx) = mpsc::channel();
    for _ in 0..5 {
        let release = dispatch(&mut d, 0, &results);
        release.send(()).expect("the render is still running");
        // The worker has to drop its flag before the next dispatch prunes.
        rx.recv().expect("an unabandoned render arrives");
    }
    // Each dispatch prunes before pushing, so only the render just added — and
    // at most one whose worker had not quite dropped its flag — can be held.
    assert!(
        d.pane_render[0].results_wanted.len() <= 2,
        "flags accumulated: {}",
        d.pane_render[0].results_wanted.len()
    );
}
