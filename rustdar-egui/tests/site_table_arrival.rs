//! **A `Gui` is born before the site table exists, and the site layer still
//! has to end up with the rows.**
//!
//! `App::new` builds the [`Gui`] and *then* resolves the process-wide site
//! table eleven lines later — and the site layer draws from a copy of that
//! table, taken in `Gui::new`, because `rustdar-overlays` may not name
//! `rustdar-radar` (WO-M3's edge cut). A copy of the empty table means the
//! layer answers `has_data` false, no raster is ever asked for, and the map
//! draws radar *labels* — which come straight off `sites::radars()` every
//! frame — with no marker under any of them.
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

use rustdar_egui::Gui;
use rustdar_overlays::render::rasterize::SitesInput;
use rustdar_radar::site_position::SitePosition;
use rustdar_radar::sites::SiteFix;
use rustdar_source::id::known;

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
    rustdar_radar::sites::resolve(fixes.iter().map(
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

/// The rows the site layer would rasterize right now, by name — read the way
/// the dispatcher reads them, through `prepare_job`, so this is the picture's
/// own input and not a field that happens to be beside it.
fn rows_the_layer_would_draw(gui: &Gui) -> Vec<String> {
    let pane = gui.pane(0).expect("a Gui always has a pane");
    let ctx = rustdar_overlays::render::overlay_state::RasterizeContext {
        is_dark: true,
        zoom: 7.0,
        device_scale: 1.0,
        now: chrono::Utc::now().naive_utc(),
        as_of: chrono::Utc::now().naive_utc(),
    };
    let Some(job) = gui.overlays.prepare_job(
        &known::RADAR_SITES,
        &ctx,
        &pane.layer_ref(0, &known::RADAR_SITES),
    ) else {
        return Vec::new();
    };
    job.downcast_ref::<SitesInput>()
        .expect("the sites layer describes a SitesInput")
        .sites
        .iter()
        .map(|site| site.name.clone())
        .collect()
}

fn has_data(gui: &Gui) -> bool {
    let pane = gui.pane(0).expect("a Gui always has a pane");
    gui.overlays
        .has_data(&known::RADAR_SITES, &pane.layer_ref(0, &known::RADAR_SITES))
}

#[test]
fn the_site_layer_ends_up_with_the_table_however_late_it_lands() {
    // ── The precondition that makes the rest mean anything ──────────────
    assert!(
        rustdar_radar::sites::radars().is_empty(),
        "this process has already resolved a site table, so the boot order \
         under test cannot happen here and every assertion below would pass \
         for the wrong reason",
    );

    let ctx = egui::Context::default();
    // Exactly what `App::new` does at line one of the pair.
    let mut gui = Gui::new();

    assert!(
        !has_data(&gui),
        "precondition: a Gui built on an empty table copies an empty table",
    );

    // ── 1. The boot order: the table resolves AFTER the Gui exists ──────
    resolve(&FIRST_FIXES);
    assert_eq!(
        rustdar_radar::sites::radars().len(),
        FIRST_FIXES.len(),
        "precondition: the fixture placed its radars",
    );
    assert!(
        !has_data(&gui),
        "precondition: resolving the table does not reach into the layer by \
         itself — something has to notice",
    );

    frame(&mut gui, &ctx);

    let mut drawn = rows_the_layer_would_draw(&gui);
    drawn.sort();
    assert_eq!(
        drawn,
        vec!["KINX".to_string(), "KTLX".to_string(), "KVNX".to_string()],
        "the site layer never heard about the table `App::new` resolved after \
         building the Gui: it holds {} row(s), so `has_data` is false, no \
         raster is dispatched, and the map draws site labels with no markers \
         under them",
        drawn.len(),
    );

    // ── 2. A table that moves mid-session, i.e. the first catalogue ─────
    let before = gui.pane(0).expect("a pane").radar_sites_render_gen;
    resolve(&[LATE_FIX]);
    frame(&mut gui, &ctx);

    let drawn = rows_the_layer_would_draw(&gui);
    assert!(
        drawn.iter().any(|name| name == "KDDC"),
        "a radar the table learned mid-session never reached the layer: {drawn:?}",
    );
    assert_ne!(
        gui.pane(0).expect("a pane").radar_sites_render_gen,
        before,
        "the rows changed and the cache token did not, so a raster already on \
         the glass would never be superseded",
    );

    // ── 3. A frame that follows a table which did NOT move is free ──────
    //
    // Without this the fix could be "republish every frame", which allocates
    // a String per radar on the frame thread ~200 times a frame.
    let steady = gui.pane(0).expect("a pane").radar_sites_render_gen;
    frame(&mut gui, &ctx);
    frame(&mut gui, &ctx);
    assert_eq!(
        gui.pane(0).expect("a pane").radar_sites_render_gen,
        steady,
        "the layer re-copied the table on a frame where the table had not \
         moved; that is 200 allocations and a re-rasterize per frame",
    );
}
