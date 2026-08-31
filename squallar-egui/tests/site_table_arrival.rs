//! **A `Gui` is born before the site table exists, and the site layers still
//! have to end up with the rows.**
//!
//! `App::new` builds the [`Gui`] and *then* resolves the process-wide site
//! table eleven lines later — and the site layers draw from a copy of that
//! table, taken in `Gui::new`, because `squallar-overlays` may not name
//! `squallar-radar` (WO-M3's edge cut). A copy of the empty table means they
//! answer `has_data` false, no coverage raster is ever asked for, and the map
//! draws radar *labels* — which come straight off `sites::radars()` every
//! frame — over no coverage at all.
//!
//! **Two layers, one publisher.** `Gui::publish_radar_sites` hands the same
//! rows to [`known::RADAR_SITES`], which paints markers and names per frame,
//! and to [`known::RADAR_COVERAGE`], which rasterizes the network's 230 km
//! discs as ground. A boot order that reached one and not the other is the same
//! bug with half the symptom, so both are asserted at every step below.
//!
//! **This is an integration test, and that is the point.** The site table is
//! a process-global that every in-crate harness resolves on its way up, so a
//! unit test cannot be run against an unresolved one: by the time the second
//! test in a binary starts, the table is already there and the boot order
//! under test cannot happen. An integration test gets its own process, so
//! the first line below can assert the table really is empty — and a
//! precondition that can fail is the difference between this test and a
//! vacuous one.
//!
//! Every test here shares that one process and therefore that one table, so
//! they run in a fixed order behind a single entry point rather than as four
//! `#[test]`s racing each other.

use squallar_egui::Gui;
use squallar_overlays::render::rasterize::CoverageInput;
use squallar_radar::site_position::SitePosition;
use squallar_radar::sites::SiteFix;
use squallar_source::id::known;

/// `(ICAO, lat_udeg, lon_udeg, site_height_m, tower_height_m)` — real
/// WSR-88Ds, positions as `api.weather.gov/radar/stations` publishes them.
const FIRST_FIXES: [(&str, i32, i32, i32, i32); 3] = [
    ("KTLX", 35_333_060, -97_277_500, 370, 19),
    ("KINX", 36_175_000, -95_565_000, 204, 30),
    ("KVNX", 36_741_000, -98_128_000, 369, 30),
];

/// One more, standing in for the catalogue that lands mid-session.
const LATE_FIX: (&str, i32, i32, i32, i32) = ("KDDC", 37_761_000, -99_969_000, 789, 24);

fn resolve(fixes: &[(&'static str, i32, i32, i32, i32)]) {
    squallar_radar::sites::resolve(fixes.iter().map(
        |&(name, lat_udeg, lon_udeg, site_height_m, tower_height_m)| {
            (
                name,
                SiteFix::Learned(SitePosition {
                    lat_udeg,
                    lon_udeg,
                    site_height_m,
                    tower_height_m,
                }),
            )
        },
    ));
}

/// One real frame, the way `EguiRenderer` drives one: `begin_pass`, the whole
/// UI, `end_pass`. Not a direct call to the republish — what is under test is
/// that *drawing* is enough, because nothing else in the app is going to
/// remember.
fn frame(gui: &mut Gui, ctx: &egui::Context) {
    ctx.begin_pass(egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1200.0, 800.0),
        )),
        time: Some(1.0),
        ..Default::default()
    });
    gui.ui(ctx);
    let _ = ctx.end_pass();
}

/// The stations the coverage wash would be rasterized for right now, by name —
/// read the way the dispatcher reads them, through `prepare_job`, so this is
/// the picture's own input and not a field that happens to be beside it.
///
/// The described input carries positions and no names: the wash is the whole
/// network's, so no station is distinguished in it. Each position is resolved
/// back through the same table the fixture placed, which keeps the assertions
/// below written in station names rather than in coordinate literals.
fn rows_the_layer_would_draw(gui: &Gui) -> Vec<String> {
    let pane = gui.pane(0).expect("a Gui always has a pane");
    let ctx = squallar_overlays::render::overlay_state::RasterizeContext {
        is_dark: true,
        zoom: 7.0,
        device_scale: 1.0,
        now: chrono::Utc::now().naive_utc(),
        as_of: chrono::Utc::now().naive_utc(),
        frame: None,
    };
    let Some(job) = gui.overlays.prepare_job(
        &known::RADAR_COVERAGE,
        &ctx,
        &pane.layer_ref(0, &known::RADAR_COVERAGE),
    ) else {
        return Vec::new();
    };
    job.downcast_ref::<CoverageInput>()
        .expect("the coverage layer describes a CoverageInput")
        .sites
        .iter()
        .map(|site| name_at(site.lat, site.lon))
        .collect()
}

/// The station standing at a described position, or the position itself when
/// nothing does — so a row that arrived mangled shows up in the failure message
/// as the coordinates it actually carried.
fn name_at(lat: f64, lon: f64) -> String {
    squallar_radar::sites::radars()
        .iter()
        .find(|site| site.lat == lat && site.lon == lon)
        .map_or_else(|| format!("({lat}, {lon})"), |site| site.name.to_string())
}

/// Whether the table has reached a layer. Both layers are fed by one publisher
/// and either one missing the rows is the boot-order bug.
fn has_data(gui: &Gui, id: &squallar_source::id::LayerId) -> bool {
    let pane = gui.pane(0).expect("a Gui always has a pane");
    gui.overlays.has_data(id, &pane.layer_ref(0, id))
}

/// Both site layers at once, for the assertions that are about the publisher
/// rather than about either handler.
fn both_have_data(gui: &Gui) -> bool {
    has_data(gui, &known::RADAR_SITES) && has_data(gui, &known::RADAR_COVERAGE)
}

/// The token the coverage raster on the glass is keyed by — the same function
/// the draw loop and the arrival path both call, so this is the value that
/// decides whether a cached picture survives, not a proxy for it.
///
/// The theme argument does not matter here: the wash is one ink and the layer
/// declares itself theme-independent.
fn coverage_cache_token(gui: &Gui) -> u64 {
    let pane = gui.pane(0).expect("a Gui always has a pane");
    squallar_egui::overlay_cache_token(&gui.overlays, 0, pane, &known::RADAR_COVERAGE, false)
}

#[test]
fn the_site_layer_ends_up_with_the_table_however_late_it_lands() {
    // ── The precondition that makes the rest mean anything ──────────────
    assert!(
        squallar_radar::sites::radars().is_empty(),
        "this process has already resolved a site table, so the boot order \
         under test cannot happen here and every assertion below would pass \
         for the wrong reason",
    );

    let ctx = egui::Context::default();
    // Exactly what `App::new` does at line one of the pair.
    let mut gui = Gui::new();
    // The frames below must not build a live tile source: its IO thread
    // fetches the production archive, and what its tiles paint depends on
    // how much wall-clock time this test takes. See
    // `MapTileState::go_offline_for_tests`.
    gui.go_offline_for_tests();

    assert!(
        !has_data(&gui, &known::RADAR_SITES) && !has_data(&gui, &known::RADAR_COVERAGE),
        "precondition: a Gui built on an empty table copies an empty table",
    );

    // ── 1. The boot order: the table resolves AFTER the Gui exists ──────
    resolve(&FIRST_FIXES);
    assert_eq!(
        squallar_radar::sites::radars().len(),
        FIRST_FIXES.len(),
        "precondition: the fixture placed its radars",
    );
    assert!(
        !has_data(&gui, &known::RADAR_SITES) && !has_data(&gui, &known::RADAR_COVERAGE),
        "precondition: resolving the table does not reach into the layers by \
         itself — something has to notice",
    );

    frame(&mut gui, &ctx);

    let mut drawn = rows_the_layer_would_draw(&gui);
    drawn.sort();
    assert_eq!(
        drawn,
        vec!["KINX".to_string(), "KTLX".to_string(), "KVNX".to_string()],
        "the coverage layer never heard about the table `App::new` resolved \
         after building the Gui: it holds {} row(s), so `has_data` is false, no \
         raster is dispatched, and the map draws site labels over no coverage \
         at all",
        drawn.len(),
    );
    // **One publisher, two handlers.** The rows above prove the coverage layer
    // heard; this is the other half of the same delivery, and a publisher that
    // fed one layer and not the other would leave the markers describing a
    // network the wash does not.
    assert!(
        both_have_data(&gui),
        "the table reached one site layer and not the other: `RadarSites` \
         has_data = {}, `RadarCoverage` has_data = {}",
        has_data(&gui, &known::RADAR_SITES),
        has_data(&gui, &known::RADAR_COVERAGE),
    );

    // ── 2. A table that moves mid-session, i.e. the first catalogue ─────
    let before = gui.pane(0).expect("a pane").radar_sites_render_gen;
    let token_before = coverage_cache_token(&gui);
    resolve(&[LATE_FIX]);
    frame(&mut gui, &ctx);

    let drawn = rows_the_layer_would_draw(&gui);
    assert!(
        drawn.iter().any(|name| name == "KDDC"),
        "a radar the table learned mid-session never reached the layer: {drawn:?}",
    );
    assert!(
        both_have_data(&gui),
        "the mid-session table reached one site layer and not the other",
    );
    assert_ne!(
        gui.pane(0).expect("a pane").radar_sites_render_gen,
        before,
        "the rows changed and the republish generation did not, so nothing \
         noticed the table move",
    );
    assert_ne!(
        coverage_cache_token(&gui),
        token_before,
        "the rows changed and the coverage layer's cache token did not, so the \
         wash already on the glass — drawn for the network as it was — would \
         never be superseded",
    );

    // ── 3. A frame that follows a table which did NOT move is free ──────
    //
    // Without this the fix could be "republish every frame", which allocates
    // a String per radar on the frame thread ~200 times a frame.
    let steady = gui.pane(0).expect("a pane").radar_sites_render_gen;
    let steady_token = coverage_cache_token(&gui);
    frame(&mut gui, &ctx);
    frame(&mut gui, &ctx);
    assert_eq!(
        gui.pane(0).expect("a pane").radar_sites_render_gen,
        steady,
        "the layers re-copied the table on a frame where the table had not \
         moved; that is 200 allocations and a re-rasterize per frame",
    );
    assert_eq!(
        coverage_cache_token(&gui),
        steady_token,
        "the coverage layer's cache token moved on a frame where the table had \
         not, so every frame throws away the wash it just rasterized",
    );
}
