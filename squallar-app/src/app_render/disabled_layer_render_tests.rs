//! **A layer that is not drawn is not rendered.**
//!
//! The dispatch used to gate on the pane's *kind* and on its scan offering the
//! selected product, and on nothing else — so a pane whose radar layer the user
//! had switched off still posted a full plan-view render, still filed it in the
//! shared render cache, still uploaded it, and still held it. On the
//! all-layers-disabled scene that is two 7362 px rasters (216,796,176 B each)
//! nobody can see.
//!
//! The gate is [`squallar_egui::Gui::plan_view_demand_for_pane`], which asks
//! `PaneState::is_overlay_enabled(&known::RADAR)` — **the accessor the draw
//! walk's own per-layer skip asks** (`ui_map_pane`'s `for id in &draw_order`
//! loop), not a second spelling of it.
//!
//! # The trap these tests exist to hold shut
//!
//! `is_overlay_enabled` is `slot(id).is_some_and(|slot| slot.enabled)`, so a
//! pane whose radar slot has never been *minted* answers `false`. Gate the
//! render on it carelessly and a user whose stack was never seeded gets no
//! radar at all — a far worse defect than two wasted renders. So the first two
//! tests here pin the fresh-install case from the front: a pane that has saved
//! nothing draws radar, and dispatches for it. The fetch-side lane pins the
//! mirror image of this on its own side.

use super::one_render_per_sweep_tests::{asked_for, recorder, sample_scan};
use squallar_radar::types::RadarProduct;
use squallar_source::id::known;
use std::sync::Arc;

const SITE: &str = "KTLX";
const TILT: f32 = 0.5;
const OTHER_TILT: f32 = 1.5;
const PRODUCT: RadarProduct = RadarProduct::Reflectivity;

/// How many frames a disabled scene is walked for.
///
/// **More than one on purpose.** A dispatch that fires once per *change* and a
/// dispatch that fires once per *frame* are different defects, and one frame
/// cannot tell them apart: the observed bug logged its two renders one second
/// after boot and then stopped, so a one-frame check would have read the same
/// on a fix that only deferred the waste.
const FRAMES: usize = 6;

fn volume_time() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 9, 7)
        .unwrap()
        .and_hms_opt(7, 22, 0)
        .unwrap()
}

/// Aim pane `idx` at [`SITE`] and put the volume where the dispatch will look,
/// through the app's own scan-info event rather than by writing the pane's
/// fields — the dispatch reads what that event installed.
fn point_at(app: &mut crate::app::App, idx: usize) {
    let radar = squallar_radar::sites::get_radar_site(SITE)
        .unwrap_or_else(|| panic!("{SITE} is in the test site table"))
        .clone();
    let mut product_elevations = std::collections::HashMap::new();
    product_elevations.insert(PRODUCT, vec![TILT, OTHER_TILT]);
    {
        let pane = app.gui.pane_mut(idx).expect("pane exists");
        pane.set_site(SITE.to_string());
        pane.set_selected_product(squallar_radar::fields::spec(PRODUCT).id.clone());
        pane.set_selected_elevation(TILT);
    }
    app.gui
        .apply(squallar_egui::shell_api::GuiEvent::ScanInfoForPane {
            pane_idx: idx,
            info: squallar_radar::types::ScanInfo {
                site: radar,
                site_source: squallar_radar::site_position::SitePositionSource::Table,
                site_position: None,
                timestamp: volume_time(),
                vcp_number: 212,
                available_products: vec![PRODUCT],
                product_elevations,
                status: String::new(),
            },
        });
    drop(app.volumes.install_still(
        SITE.to_string(),
        volume_time(),
        (
            sample_scan(),
            Arc::new(squallar_radar::nyquist::DeclaredNyquist::empty()),
        ),
    ));
}

/// One map pane on [`SITE`] with its volume resident, built through
/// `n_pane_app` — the app's own config load, not a hand-assembled `PaneState`.
fn app_on_a_sweep() -> crate::app::App {
    let mut app = crate::app::tests::n_pane_app(1, SITE);
    point_at(&mut app, 0);
    app
}

/// Switch pane 0's radar layer off — or on — through the pane's own door.
fn set_radar(app: &mut crate::app::App, on: bool) {
    app.gui
        .pane_mut(0)
        .expect("the fixture has one pane")
        .set_overlay_enabled(known::RADAR, on);
}

fn draws_radar(app: &crate::app::App) -> bool {
    app.gui
        .pane(0)
        .expect("the fixture has one pane")
        .is_overlay_enabled(&known::RADAR)
}

/// Whether pane 0 is holding a radar texture — the GPU-side holder, asked the
/// way `pane_kind_render_filter_tests` asks it.
fn holds_radar_texture(app: &mut crate::app::App) -> bool {
    app.gui
        .pane_mut(0)
        .expect("the fixture has one pane")
        .overlay_cache_mut(&known::RADAR)
        .current()
        .is_some()
}

/// The two host-side raster holders, **counted where the heap census counts
/// them** (`App::publish_heap_census` and `RenderDispatcher::publish_heap_census`):
/// the `render cache` family and the `cached renders` family. A figure read
/// anywhere else would be this test's own arithmetic wearing the census's name.
fn census_raster_bytes(app: &crate::app::App) -> (u64, u64) {
    (
        app.render.render_cache.resident_bytes() as u64,
        app.render.cached_render_bytes(),
    )
}

/// The side of the raster these tests deliver — deliberately tiny, so a figure
/// below is one this file put there and not the production raster.
const SIDE: usize = 8;

/// A finished raster of [`SIDE`].
fn small_raster() -> Arc<egui::ColorImage> {
    Arc::new(egui::ColorImage::from_rgba_unmultiplied(
        [SIDE, SIDE],
        &[9, 8, 7, 180].repeat(SIDE * SIDE),
    ))
}

/// **What one of these rasters weighs**, priced the way the census prices it —
/// `pixels.len() * size_of::<Color32>()`, the same expression
/// `RenderDispatcher::cached_render_bytes` and `RenderCache` use, over an empty
/// hover field that adds nothing.
///
/// Every precondition below is asserted against THIS FIGURE rather than against
/// "not zero", and that is deliberate. A holder that is later replaced by a
/// handle — a weak reference kept only for upload identity, say, at 64 bytes
/// instead of a raster — would still be "non-empty", and a release assertion
/// standing on a non-emptiness premise would then pass because there was
/// nothing left to release rather than because anything was released. Naming
/// the byte figure makes that change trip the precondition loudly instead.
///
/// # That change arrived on 2026-09-07, and this is what it caught
///
/// The paragraph above is kept as written because it came true, almost to the
/// word: `PaneRenderState::cached_render` — the pane's `Arc` clone of the
/// raster, held for restore — was replaced by
/// `PaneRenderState::uploaded_from`, a `Weak` kept only for upload identity,
/// and `RenderDispatcher::cached_render_bytes` now reads **zero by
/// construction**. Spelled "not zero" these preconditions would have gone
/// quiet; spelled in bytes they went red and sent the change's author here.
///
/// So the controls below now read `(raster_bytes(), 0)`, and the second term
/// is a **structural zero**: not a measurement, and no longer able to say
/// "the scene with the layer ON reaches the pane". What says that instead is
/// [`radar_texture_px`] — the pane-side holder that survived, asserted at its
/// pixel size beside every one of them. The zero is kept rather than dropped
/// because it is still a tripwire: a pane that starts holding pixels again
/// makes these lines red.
fn raster_bytes() -> u64 {
    (small_raster().pixels.len() * std::mem::size_of::<egui::Color32>()) as u64
}

/// The pixel dimensions of the radar texture pane 0 is holding, or `None` when
/// it holds none — the GPU-side holder, named by its size for the same reason
/// [`raster_bytes`] exists.
fn radar_texture_px(app: &mut crate::app::App) -> Option<(u32, u32)> {
    let tex = app
        .gui
        .pane_mut(0)
        .expect("the fixture has one pane")
        .overlay_cache_mut(&known::RADAR)
        .current()?;
    Some((tex.width, tex.height))
}

/// A cache entry of the same pixels, for the tests that seed the shared cache
/// directly.
fn cached_output() -> crate::render_dispatch::CachedRenderOutput {
    crate::render_dispatch::CachedRenderOutput {
        surface: crate::channels::StillSurface::Raster(small_raster()),
        max_range_km: 230.0,
        hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
    }
}

/// Put a finished render for pane 0 on the channel the worker replies over,
/// **without** draining it — the caller decides when the arrival is polled, so
/// a toggle can land inside the window.
fn deliver_a_raster(app: &mut crate::app::App, elevation: f32) {
    app.channels
        .render_sender
        .send(crate::channels::RenderResponse {
            rendered: Some(crate::channels::RenderedImage {
                surface: crate::channels::StillSurface::Raster(small_raster()),
                max_range_km: 230.0,
                hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
                nyquist_ms: None,
                melting_layer_source: None,
                storm_motion: None,
            }),
            product: PRODUCT,
            elevation,
            generation: app.render.render_generation,
            pane_idx: 0,
            speculative_for: None,
        })
        .expect("the receiver lives on the App");
}

/// Walk `n` frames of the dispatch. A frame with nothing to do must still be a
/// frame: this is what makes "zero renders" a claim about the steady state and
/// not about one instant.
fn walk(app: &mut crate::app::App, ctx: &egui::Context, n: usize) {
    for _ in 0..n {
        app.dispatch_pane_renders(ctx);
    }
}

/// **The fresh-install premise, established before anything is gated on it.**
///
/// `n_pane_app`'s config names a site and a pane count and no layer stack at
/// all, which is what a first launch has: the slots come from
/// `Gui::initialize_pane_enabled` seeding the pane from the handlers'
/// `default_enabled`. If that ever stops happening — an admission door that
/// refuses the default layers, a constructor that skips the seed — every
/// assertion below about a *disabled* pane would still pass while a first-run
/// user saw an empty map.
#[test]
fn a_pane_that_has_saved_nothing_still_draws_radar() {
    let app = app_on_a_sweep();
    assert!(
        draws_radar(&app),
        "a pane built from a config with no layer stack came up NOT drawing \
         radar. The dispatch gate reads this exact accessor, so a first-run \
         user would now get no radar at all — which is a worse defect than \
         the wasted renders this suite is about",
    );
    // And the same for a pane no config touched at all — `headless` loads no
    // store, so this is the barest construction the app has.
    let bare = crate::app::tests::headless(crate::platform_double::TestBridge::desktop());
    assert!(
        bare.gui
            .pane(0)
            .expect("a fresh app has a pane")
            .is_overlay_enabled(&known::RADAR),
        "an app constructed with no config store at all came up not drawing \
         radar",
    );
}

/// The behavioural half of the premise above: the first-run pane does not
/// merely *read* enabled, it actually gets its render.
#[test]
fn a_first_run_pane_still_dispatches_its_render() {
    let (recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = app_on_a_sweep();

    assert!(
        draws_radar(&app),
        "premise: the fixture must draw radar, or the dispatch below is not \
         the first-run case",
    );

    app.dispatch_pane_renders(&ctx);

    assert_eq!(
        asked_for(&recorded),
        vec![(PRODUCT, TILT)],
        "a pane that has saved no layer stack dispatched no render — the \
         gate is reading 'has a saved opinion', not 'is drawn'",
    );
}

/// The defect itself: every layer off, including radar, and the dispatch still
/// posts renders, files them and uploads them.
#[test]
fn a_pane_with_radar_switched_off_dispatches_nothing() {
    let (recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = app_on_a_sweep();
    set_radar(&mut app, false);
    assert!(
        !draws_radar(&app),
        "premise: the fixture must really have radar off",
    );

    walk(&mut app, &ctx, FRAMES);

    assert_eq!(
        asked_for(&recorded),
        Vec::new(),
        "{FRAMES} frames on a pane that draws no radar posted {} plan-view \
         render jobs. At the production raster that is 216,796,176 B of \
         `Color32` apiece, for a picture the draw walk skips",
        asked_for(&recorded).len(),
    );
    assert!(
        !app.render.pane_render[0].render_in_flight(),
        "the dispatcher marked a render in flight for a pane that draws none",
    );
    assert_eq!(
        app.render.render_cache.entry_count(),
        0,
        "the shared render cache took an entry for a layer nothing paints",
    );
    assert_eq!(
        app.render.pane_render[0].last_rendered, None,
        "the dispatcher recorded a render for a pane it must not have served",
    );
    assert!(
        !holds_radar_texture(&mut app),
        "a plan-view texture was uploaded to a pane that paints none",
    );
    assert_eq!(
        census_raster_bytes(&app),
        (0, 0),
        "the `render cache` and `cached renders` census families are holding \
         bytes for a layer that is switched off",
    );

    // The zeros above are only a claim about this fix while the same scene
    // with the layer ON reaches those holders at all. It does, at
    // [`raster_bytes`] — so a future change that empties a holder by deleting
    // it fails HERE, rather than leaving the four assertions above passing
    // because there is nothing left to hold.
    let mut control = app_on_a_sweep();
    control.dispatch_pane_renders(&ctx);
    deliver_a_raster(&mut control, TILT);
    control.poll_render_results(&ctx);
    assert_eq!(
        census_raster_bytes(&control),
        (raster_bytes(), 0),
        "control: the same scene with radar DRAWN must put {} B into the \
         shared `render cache`. If it no longer does, the holder set has moved \
         and every zero asserted above has become vacuous - re-derive it. The \
         second term is a structural zero since 2026-09-07; see \
         [`raster_bytes`]",
        raster_bytes(),
    );
    assert_eq!(
        radar_texture_px(&mut control),
        Some((SIDE as u32, SIDE as u32)),
        "control: the same scene with radar DRAWN must reach the PANE too. \
         This is what the pane's byte figure used to say before that holder \
         became an identity; without it the zeros above are only about the \
         shared cache",
    );
}

/// And the fix is not "never render": the layer coming back on is served on
/// the very next frame, not after a timeout and not on some later event.
#[test]
fn switching_radar_back_on_dispatches_on_the_next_frame() {
    let (recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = app_on_a_sweep();
    set_radar(&mut app, false);
    walk(&mut app, &ctx, FRAMES);
    assert_eq!(
        asked_for(&recorded),
        Vec::new(),
        "premise: the quiet frames must really have been quiet",
    );

    set_radar(&mut app, true);
    app.dispatch_pane_renders(&ctx);

    assert_eq!(
        asked_for(&recorded),
        vec![(PRODUCT, TILT)],
        "switching the layer back on did not render it on the next frame. \
         Interaction is realtime: the user's next frame, not a timeout",
    );
}

/// **A resident render is given back when the layer goes off.** Holding
/// 216,796,176 B for a picture the user has switched off is the same waste as
/// making it, one step later.
///
/// Counted at the two places the heap census counts them, so a figure that
/// went back here is a figure that goes back in the census line.
#[test]
fn switching_radar_off_gives_back_the_resident_render() {
    let (_recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = app_on_a_sweep();

    // A finished render, delivered the way the worker delivers one.
    app.dispatch_pane_renders(&ctx);
    deliver_a_raster(&mut app, TILT);
    app.poll_render_results(&ctx);

    // **Three holders, each preconditioned at its byte figure.** See
    // [`raster_bytes`]: "not zero" would let this whole test pass for the
    // wrong reason the day a holder is replaced by a handle, because a release
    // assertion whose premise is non-emptiness goes quiet rather than red when
    // there is nothing left to release. The holder set is the one derivable at
    // this file's base commit; whoever changes it next re-derives rather than
    // trusting this list, and these three lines are what tell them to.
    let (cache_before, panes_before) = census_raster_bytes(&app);
    assert_eq!(
        (cache_before, panes_before),
        (raster_bytes(), 0),
        "precondition: the shared `render cache` must be holding this \
         raster's {} B, and the pane's `cached renders` figure must be the \
         structural zero it became on 2026-09-07. A first term that is \
         neither that nor zero, or a second term that is not zero, means a \
         holder now keeps something other than what this file believes — \
         re-derive the holder set and the release below with it, do not relax \
         this line",
        raster_bytes(),
    );
    assert_eq!(
        radar_texture_px(&mut app),
        Some((SIDE as u32, SIDE as u32)),
        "precondition: the delivery must have left a {SIDE}x{SIDE} texture on \
         the pane — the third holder, named by its size for the same reason",
    );

    set_radar(&mut app, false);
    app.dispatch_pane_renders(&ctx);

    assert_eq!(
        census_raster_bytes(&app),
        (0, 0),
        "switching the layer off left its raster resident: `render cache` and \
         `cached renders` were {cache_before} B and {panes_before} B and did \
         not go back",
    );
    assert_eq!(
        radar_texture_px(&mut app),
        None,
        "switching the layer off left its GPU texture on the pane",
    );
}

/// **The seam.** Two predicates that are individually right can be jointly
/// wrong at the moment between them: the dispatch decided while the layer was
/// on, the raster arrives after the user switched it off. The arrival asks the
/// same door the dispatch asked, so the late raster is dropped rather than
/// filed in the shared cache and uploaded to a pane painting none.
#[test]
fn a_raster_arriving_after_the_layer_went_off_is_not_taken() {
    let (_recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = app_on_a_sweep();

    app.dispatch_pane_renders(&ctx);
    assert!(
        app.render.pane_render[0].render_in_flight(),
        "premise: a render must really be in flight, or there is no window \
         for the toggle to land in",
    );

    // **The control, on the same fixture and the same delivery**, so the zeros
    // below are a refusal and not an empty run: with the layer left on, this
    // arrival really does fill both holders at their byte figure.
    {
        let mut control = app_on_a_sweep();
        control.dispatch_pane_renders(&ctx);
        deliver_a_raster(&mut control, TILT);
        control.poll_render_results(&ctx);
        assert_eq!(
            census_raster_bytes(&control),
            (raster_bytes(), 0),
            "control: a delivered raster on a drawn layer must land in the \
             shared cache at {} B, or the zeros asserted below are what an \
             empty run looks like rather than what a refusal looks like. The \
             second term is a structural zero; see [`raster_bytes`]",
            raster_bytes(),
        );
        assert_eq!(
            radar_texture_px(&mut control),
            Some((SIDE as u32, SIDE as u32)),
            "control: the delivered raster must reach the pane when the layer \
             is drawn, or the refusal below is indistinguishable from a \
             delivery that never worked",
        );
    }

    // The user's toggle lands while the worker is still rasterizing.
    set_radar(&mut app, false);
    deliver_a_raster(&mut app, TILT);
    app.poll_render_results(&ctx);

    assert!(
        !holds_radar_texture(&mut app),
        "a raster that finished after its layer was switched off was uploaded \
         to the pane anyway",
    );
    assert_eq!(
        app.render.render_cache.entry_count(),
        0,
        "a raster that finished after its layer was switched off was filed in \
         the shared render cache",
    );
    assert_eq!(
        census_raster_bytes(&app),
        (0, 0),
        "the late raster left bytes in the census's raster families",
    );
}

/// The broadcast half of the same rule: one render is handed to every sibling
/// pane showing the same picture, and a sibling that paints no radar is not one
/// of them.
#[test]
fn a_sibling_that_paints_no_radar_is_handed_no_raster() {
    let (_recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = crate::app::tests::n_pane_app(2, SITE);
    point_at(&mut app, 0);
    point_at(&mut app, 1);

    // Premise: with both panes drawing, the sibling really does take the
    // broadcast — otherwise the assertion below is about a path nothing walks.
    app.dispatch_pane_renders(&ctx);
    deliver_a_raster(&mut app, TILT);
    app.poll_render_results(&ctx);
    assert!(
        app.gui
            .pane_mut(1)
            .expect("two panes")
            .overlay_cache_mut(&known::RADAR)
            .current()
            .is_some(),
        "premise: a drawing sibling must take the broadcast",
    );

    let mut app = crate::app::tests::n_pane_app(2, SITE);
    point_at(&mut app, 0);
    point_at(&mut app, 1);
    app.gui
        .pane_mut(1)
        .expect("two panes")
        .set_overlay_enabled(known::RADAR, false);

    app.dispatch_pane_renders(&ctx);
    deliver_a_raster(&mut app, TILT);
    app.poll_render_results(&ctx);

    assert!(
        app.gui
            .pane_mut(1)
            .expect("two panes")
            .overlay_cache_mut(&known::RADAR)
            .current()
            .is_none(),
        "the broadcast uploaded a full plan-view raster to a sibling pane that \
         paints no radar",
    );
}

/// **The release is one key, not a sweep**, and this is what says so.
///
/// `maybe_spawn_speculative_render` files the *adjacent* tilt ahead of a tilt
/// change, so that change is instant when it comes. No pane wants that entry
/// yet, so a release written as "keep what the panes are asking for" would
/// throw it away on any frame with a layer switched off — turning this fix
/// into a regression of a different feature.
#[test]
fn the_release_leaves_an_unwanted_speculative_raster_alone() {
    let (_recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = app_on_a_sweep();

    // The picture the pane is showing, and the one speculation filed beside it.
    app.render.cache_render(
        SITE,
        PRODUCT,
        squallar_radar::types::RenderView::PlanView,
        TILT,
        cached_output(),
    );
    app.render.cache_render(
        SITE,
        PRODUCT,
        squallar_radar::types::RenderView::PlanView,
        OTHER_TILT,
        cached_output(),
    );
    app.dispatch_pane_renders(&ctx);
    assert_eq!(
        app.render.render_cache.entry_count(),
        2,
        "premise: both entries must really be resident",
    );

    set_radar(&mut app, false);
    app.dispatch_pane_renders(&ctx);

    assert_eq!(
        app.render.render_cache.entry_count(),
        1,
        "the release took more than the picture the pane was painting: the \
         adjacent-tilt raster speculation filed for the next tilt change went \
         with it",
    );
    assert!(
        app.render
            .get_cached_render(
                SITE,
                PRODUCT,
                squallar_radar::types::RenderView::PlanView,
                OTHER_TILT,
            )
            .is_some(),
        "the surviving entry is not the speculative one",
    );
}

/// **What decides how many rasters get wasted, and why the figure read 0, 1 or
/// 2 on six identical legs.**
///
/// The waste is not one-per-frame and not one-per-session: `last_rendered`
/// latches, so a pane dispatches once and then stops — until something
/// invalidates it. On a live site that something is the feed: every volume
/// that lands calls `RenderDispatcher::reset_panes_for_site`, which puts
/// `last_rendered` back to `None`, and the next frame pays for another raster
/// nobody can see. **So the count is one per volume arrival the leg lived
/// through**, which is why it quantised in whole renders and why where a leg
/// falls in the VCP cycle decided the figure.
///
/// After the fix the count is not a smaller draw from that distribution — it
/// is exactly zero for every arrival count, which is what makes this a gate
/// and not a measurement.
#[test]
fn the_wasted_render_count_is_one_per_volume_arrival() {
    let ctx = egui::Context::default();

    // The control: with the layer ON, each arrival really does cost a render,
    // so the arm below is measuring against a live mechanism and not against a
    // path that never fires.
    for arrivals in 0..3usize {
        let (recorded, _worker) = recorder();
        let mut app = app_on_a_sweep();
        walk(&mut app, &ctx, 2);
        for _ in 0..arrivals {
            app.render.reset_panes_for_site(SITE, &app.gui);
            walk(&mut app, &ctx, 2);
        }
        assert_eq!(
            asked_for(&recorded).len(),
            1 + arrivals,
            "control: a drawn layer must cost one render per volume arrival, \
             or the arm below is asserted against a mechanism that is not \
             running",
        );
    }

    for arrivals in 0..3usize {
        let (recorded, _worker) = recorder();
        let mut app = app_on_a_sweep();
        set_radar(&mut app, false);
        walk(&mut app, &ctx, 2);
        for _ in 0..arrivals {
            app.render.reset_panes_for_site(SITE, &app.gui);
            walk(&mut app, &ctx, 2);
        }
        assert_eq!(
            asked_for(&recorded),
            Vec::new(),
            "{arrivals} volume arrivals on a pane that paints no radar cost \
             {} rasters. The count is what varies — bytes are count x one \
             render — so a gate that let ANY through would leave the figure a \
             race with the feed",
            asked_for(&recorded).len(),
        );
    }
}

/// **The second raster, and why the gate belongs at the dispatch.**
///
/// The reported scene made *two* rasters off one "Spawning background render"
/// line. The second has no log line of its own: it is
/// `App::maybe_spawn_speculative_render`, fired off the FIRST render's
/// delivery, pre-rendering the adjacent tilt so a tilt change is instant and
/// caching it unconditionally.
///
/// So the two are not independent — the second exists only because the first
/// was delivered — and a gate at the dispatch stops both, while a gate
/// anywhere downstream can stop one and keep the other. Against a count that
/// already varies 0/1/2 across identical legs, that would halve the figure
/// instead of zeroing it, and a halving is not distinguishable from a leg that
/// landed on the low side of the race.
///
/// **Asserted separately from the dispatch count on purpose**, so a fix that
/// stops one and not the other fails here rather than passing at half
/// strength.
#[test]
fn a_disabled_layer_spawns_no_speculative_render_either() {
    let ctx = egui::Context::default();

    // The control: with the layer drawn, a delivery really does arm
    // speculation, so the arm below is measured against a live mechanism.
    // The raster is delivered without a dispatch in front of it: the recorder
    // sink never answers the job it is handed, so a dispatch would leave
    // `renders_in_flight` raised and speculation's own free-slot gate would be
    // what declined — not the layer's state, which is what is under test.
    let (control_jobs, _worker) = recorder();
    let mut app = app_on_a_sweep();
    deliver_a_raster(&mut app, TILT);
    app.poll_render_results(&ctx);
    assert!(
        app.render.speculative_in_flight(),
        "control: a delivered raster on a drawn layer must arm the \
         adjacent-tilt speculation, or the assertion below is about a path \
         nothing reaches",
    );
    assert_eq!(
        asked_for(&control_jobs).len(),
        1,
        "control: the posted job is the speculation the delivery armed — the \
         second of the two rasters the reported scene made from one log line",
    );

    // The arm: the layer is off, so nothing is dispatched, nothing is
    // delivered, and speculation is never armed.
    let (jobs, _worker) = recorder();
    let mut app = app_on_a_sweep();
    set_radar(&mut app, false);
    walk(&mut app, &ctx, FRAMES);
    // Delivered the same way as the control, so the only difference between
    // the two arms is the pane's layer flag.
    deliver_a_raster(&mut app, TILT);
    app.poll_render_results(&ctx);

    assert!(
        !app.render.speculative_in_flight(),
        "a pane that paints no radar armed the adjacent-tilt speculation - the \
         second of the two rasters the reported scene made",
    );
    assert_eq!(
        asked_for(&jobs),
        Vec::new(),
        "{} render jobs were posted for a pane that paints no radar",
        asked_for(&jobs).len(),
    );
}
