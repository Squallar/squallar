//! **The draw fork, and the claim that the two arms composite in the same
//! place.**
//!
//! A representation swap that moved the picture in the stack would be a change
//! nobody asked for, so the first thing here is not that the fan draws — it is
//! that it draws at the *same index of the same paint list* the raster's
//! textured rectangle occupied, with the range ring still behind it. The pane's
//! layer walk submits every arm through one `ui.painter()`, so submission order
//! **is** draw order, and an index is the whole of the claim.
//!
//! The rest is the refusal ladder. A polar surface has no fallback — the raster
//! it would fall back to is the allocation it exists not to make — so every way
//! it can fail to draw is a hole in the picture, and each is asserted on its
//! own against a fixture that is healthy in every other respect.

use super::*;
use crate::pane::{PaneState, RadarSurface};
use crate::radar_fan::{FanDraw, FanOutcome, FanRefusal, FanSweep, RadarFanPainter};
use squallar_overlays::render::overlay_state::{OverlayHandler as FanTestHandler, OverlayRegistry};
use std::sync::Mutex;

/// A painter that answers, and remembers what it was asked.
struct Recorder {
    /// `false` makes it decline every draw — the `PainterDeclined` arm, which
    /// is a renderer that is present and cannot draw *this*.
    answers: bool,
    seen: Mutex<Vec<Seen>>,
}

/// The parts of one [`FanDraw`] worth asserting on. Copied out because the
/// draw borrows.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Seen {
    sweeps: usize,
    opacity: f32,
    rect: egui::Rect,
    site_px: egui::Pos2,
    world_px: f64,
    km_per_px: f64,
}

impl Recorder {
    fn new(answers: bool) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            answers,
            seen: Mutex::new(Vec::new()),
        })
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen
            .lock()
            .expect("no test panics holding this")
            .clone()
    }
}

impl RadarFanPainter for Recorder {
    fn payload(
        &self,
        draw: FanDraw<'_>,
    ) -> Option<std::sync::Arc<dyn std::any::Any + Send + Sync>> {
        self.seen
            .lock()
            .expect("no test panics holding this")
            .push(Seen {
                sweeps: draw.sweeps.len(),
                opacity: draw.opacity,
                rect: draw.view.rect,
                site_px: draw.view.site_px,
                world_px: draw.view.world_px,
                km_per_px: draw.view.km_per_px,
            });
        self.answers
            .then(|| std::sync::Arc::new(()) as std::sync::Arc<dyn std::any::Any + Send + Sync>)
    }
}

const SITE_LAT: f64 = 35.33;
const SITE_LON: f64 = -97.28;
const RANGE_KM: f64 = 230.0;

fn canvas() -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0))
}

/// A well-formed single-sweep payload. Small on purpose: nothing here reads a
/// code, only the shape it declares.
fn sweep() -> FanSweep {
    FanSweep {
        field: squallar_radar::fields::known::REFLECTIVITY,
        radials: 4,
        gates: 2,
        codes: vec![0; 8],
        level_offsets: vec![0],
        lut_rgba: vec![0; crate::radar_fan::LUT_BYTES],
        edges: vec![[0.0, 90.0], [90.0, 180.0], [180.0, 270.0], [270.0, 360.0]],
        geometry: crate::radar_fan::FanGeometry {
            site_lat: SITE_LAT,
            site_lon: SITE_LON,
            first_gate_slant_km: 2.125,
            gate_interval_slant_km: 0.25,
            elevation_deg: Some(0.5),
            reach_gates: 2,
            reach_km: 2.5,
            first_gate_km: 2.0,
            earth_radius_km: squallar_geo::EARTH_RADIUS_KM,
            effective_radius_km: squallar_radar::beam::RE_EFF_KM,
        },
    }
}

fn fan_surface(sweeps: Vec<FanSweep>) -> RadarSurface {
    RadarSurface::Fan(std::sync::Arc::from(
        sweeps
            .into_iter()
            .map(std::sync::Arc::new)
            .collect::<Vec<_>>(),
    ))
}

/// One frame, differing from another only in its surface — which is what lets
/// the two arms be compared without a second fixture drifting from the first.
fn image(surface: RadarSurface) -> RadarImageData {
    RadarImageData {
        surface,
        lat: SITE_LAT,
        lon: SITE_LON,
        max_range_km: RANGE_KM,
        placed: squallar_radar::types::ImageBounds::from_radar_site(SITE_LAT, SITE_LON, RANGE_KM)
            .into(),
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
        hover: Arc::new(HoverSource::empty()),
    }
}

fn raster_surface(ctx: &egui::Context) -> RadarSurface {
    RadarSurface::Raster(ctx.load_texture(
        "fan-draw-fixture",
        egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]),
        egui::TextureOptions::NEAREST,
    ))
}

/// One pass of [`render_radar_overlay`] over a pane centred on the site, with
/// two markers around it so the surface's *index* in the paint list is
/// observable and not just its presence.
fn overlay_pass(
    surface_of: impl FnOnce(&egui::Context) -> RadarSurface,
    painter: Option<&Arc<dyn RadarFanPainter>>,
    surfaces: PaneSurfaces,
) -> Vec<&'static str> {
    let ctx = egui::Context::default();
    let canvas = canvas();
    let mut memory = walkers::MapMemory::default();
    memory.set_zoom(7.0).expect("7 is a zoom walkers accepts");
    let projector = walkers::Projector::new(canvas, &memory, walkers::lat_lon(SITE_LAT, SITE_LON));
    let mut pane = PaneState::new();
    let prefs = UserPreferences::default();

    ctx.begin_pass(egui::RawInput {
        screen_rect: Some(canvas),
        ..Default::default()
    });
    let surface = surface_of(&ctx);
    let img = image(surface);
    let ui = egui::Ui::new(
        ctx.clone(),
        egui::Id::new("radar-fan-draw"),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(canvas),
    );
    // A layer below radar in the walk, and one above it: the two things whose
    // relationship to the radar surface is the appearance claim.
    ui.painter().rect_filled(
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(4.0, 4.0)),
        0.0,
        egui::Color32::RED,
    );
    render_radar_overlay(
        &ui, &projector, &img, &mut pane, canvas, &prefs, painter, surfaces,
    );
    ui.painter().rect_filled(
        egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(4.0, 4.0)),
        0.0,
        egui::Color32::BLUE,
    );

    ctx.end_pass()
        .shapes
        .into_iter()
        .map(|clipped| match clipped.shape {
            egui::Shape::Callback(_) => "callback",
            // `Painter::image` is a textured `Mesh` in epaint, not a shape of
            // its own; the marker rects around it are `Rect`s, so the two are
            // still distinguishable without asserting on epaint's internals.
            egui::Shape::Mesh(_) => "image",
            egui::Shape::Circle(_) => "circle",
            egui::Shape::Rect(_) => "rect",
            egui::Shape::Noop => "noop",
            _ => "other",
        })
        .collect()
}

/// **The composition claim.** Both arms put their surface at the same index,
/// between the layer under radar and the layer over it, with the range ring
/// immediately after — so nothing that draws before or after radar moved.
#[test]
fn the_fan_composites_where_the_raster_composited() {
    let painter: Arc<dyn RadarFanPainter> = Recorder::new(true);
    let raster = overlay_pass(raster_surface, None, PaneSurfaces::GroundAndGlass);
    let fan = overlay_pass(
        |_| fan_surface(vec![sweep()]),
        Some(&painter),
        PaneSurfaces::GroundAndGlass,
    );

    assert_eq!(
        raster,
        vec!["rect", "image", "circle", "rect"],
        "the raster arm's own order changed"
    );
    assert_eq!(
        fan,
        vec!["rect", "callback", "circle", "rect"],
        "the fan does not sit where the raster sat"
    );
    // Said once more as the property rather than as two literals: the two
    // streams differ in exactly one position, and it is the surface's.
    assert_eq!(raster.len(), fan.len());
    let differing: Vec<usize> = (0..raster.len()).filter(|&i| raster[i] != fan[i]).collect();
    assert_eq!(differing, vec![1]);
}

/// The raster arm emits no callback, so nothing about it forces a primitive
/// boundary — the cost the fan arm adds and this control is what says the
/// raster never paid it.
#[test]
fn the_raster_arm_issues_no_callback() {
    assert!(
        !overlay_pass(raster_surface, None, PaneSurfaces::GroundAndGlass).contains(&"callback")
    );
}

/// One row of the refusal ladder: what it should decide, and the three inputs
/// that decide it.
type Case<'a> = (
    FanOutcome,
    Option<&'a Arc<dyn RadarFanPainter>>,
    PaneSurfaces,
    Vec<FanSweep>,
);

/// One outcome per refusal, each against a fixture healthy in every other
/// respect.
#[test]
fn every_refusal_is_named_and_none_of_them_draws() {
    let answering: Arc<dyn RadarFanPainter> = Recorder::new(true);
    let declining: Arc<dyn RadarFanPainter> = Recorder::new(false);

    let cases: Vec<Case<'_>> = vec![
        (
            FanOutcome::Painted,
            Some(&answering),
            PaneSurfaces::GroundAndGlass,
            vec![sweep()],
        ),
        (
            FanOutcome::Refused(FanRefusal::NoPainter),
            None,
            PaneSurfaces::GroundAndGlass,
            vec![sweep()],
        ),
        (
            FanOutcome::Refused(FanRefusal::FloorStrip),
            Some(&answering),
            PaneSurfaces::GroundOnly,
            vec![sweep()],
        ),
        (
            FanOutcome::Refused(FanRefusal::PainterDeclined),
            Some(&declining),
            PaneSurfaces::GroundAndGlass,
            vec![sweep()],
        ),
        (
            FanOutcome::Refused(FanRefusal::Malformed),
            Some(&answering),
            PaneSurfaces::GroundAndGlass,
            vec![FanSweep {
                edges: vec![[0.0, 90.0]],
                ..sweep()
            }],
        ),
        (
            FanOutcome::Refused(FanRefusal::Malformed),
            Some(&answering),
            PaneSurfaces::GroundAndGlass,
            Vec::new(),
        ),
    ];

    for (want, painter, surfaces, sweeps) in cases {
        let got = one_issue(painter, surfaces, sweeps);
        assert_eq!(got, want, "outcome for {want:?}");
    }
}

/// **A refusal is checked before the renderer is asked, in the order the
/// refusals are declared.** A floor strip with a painter installed must not
/// reach the painter at all: the swap that empties its callbacks happens later
/// and would leave the `prepare` already run.
#[test]
fn a_floor_strip_never_reaches_the_painter() {
    let recorder = Recorder::new(true);
    let painter: Arc<dyn RadarFanPainter> = recorder.clone();
    assert_eq!(
        one_issue(Some(&painter), PaneSurfaces::GroundOnly, vec![sweep()]),
        FanOutcome::Refused(FanRefusal::FloorStrip)
    );
    assert!(
        recorder.seen().is_empty(),
        "the strip pass asked the painter for a payload it would then discard"
    );
    // The control: the same painter, the same sweeps, off the strip.
    assert_eq!(
        one_issue(Some(&painter), PaneSurfaces::GroundAndGlass, vec![sweep()]),
        FanOutcome::Painted
    );
    assert_eq!(recorder.seen().len(), 1);
}

/// **When two refusals are both true, the strip is the one counted.**
///
/// A pane can be a floor strip *and* have no renderer installed, and both are
/// honest descriptions of why there is no radar on it. They are not equally
/// useful: a missing painter is a build or a device that lost its GPU and is
/// something to go and fix, while a floor strip is a permanent property of
/// that pass and nothing is wrong. Counting the strip's holes under
/// `no_painter` would send a reader hunting for a renderer that is installed
/// and working — so the order the refusals are checked in is part of what the
/// ledger means, not an accident of how the function reads.
#[test]
fn a_strip_with_no_painter_is_counted_as_a_strip() {
    assert_eq!(
        one_issue(None, PaneSurfaces::GroundOnly, vec![sweep()]),
        FanOutcome::Refused(FanRefusal::FloorStrip),
        "a floor strip's hole was attributed to the absent renderer"
    );
    // Each on its own, so the row above is the two together and not either
    // one leaking.
    assert_eq!(
        one_issue(None, PaneSurfaces::GroundAndGlass, vec![sweep()]),
        FanOutcome::Refused(FanRefusal::NoPainter)
    );
    let painter: Arc<dyn RadarFanPainter> = Recorder::new(true);
    assert_eq!(
        one_issue(Some(&painter), PaneSurfaces::GroundOnly, vec![sweep()]),
        FanOutcome::Refused(FanRefusal::FloorStrip)
    );

    // And a malformed payload on a strip is still the strip's: the shape of
    // what would have been drawn is not why it was not drawn.
    assert_eq!(
        one_issue(None, PaneSurfaces::GroundOnly, Vec::new()),
        FanOutcome::Refused(FanRefusal::FloorStrip)
    );
}

/// A malformed sweep **anywhere** in the set refuses the whole draw, because
/// the renderer indexes all of them.
#[test]
fn one_bad_sweep_refuses_the_whole_set() {
    let recorder = Recorder::new(true);
    let painter: Arc<dyn RadarFanPainter> = recorder.clone();
    let bad = FanSweep {
        codes: vec![0; 7],
        ..sweep()
    };
    assert_eq!(
        one_issue(
            Some(&painter),
            PaneSurfaces::GroundAndGlass,
            vec![sweep(), bad]
        ),
        FanOutcome::Refused(FanRefusal::Malformed)
    );
    assert!(recorder.seen().is_empty());
    // Healthy control at the same arity, so the refusal is the payload's and
    // not "two sweeps".
    assert_eq!(
        one_issue(
            Some(&painter),
            PaneSurfaces::GroundAndGlass,
            vec![sweep(), sweep()]
        ),
        FanOutcome::Painted
    );
    assert_eq!(recorder.seen().last().map(|s| s.sweeps), Some(2));
}

/// **What the payload is told about the frame.** The opacity is the layer
/// walk's, because a callback is the one shape a painter cannot tint; the site
/// lands where the projector puts it; the rect is the pane's.
#[test]
fn the_draw_carries_the_frames_own_view() {
    let recorder = Recorder::new(true);
    let painter: Arc<dyn RadarFanPainter> = recorder.clone();
    let canvas = canvas();
    let mut memory = walkers::MapMemory::default();
    memory.set_zoom(7.0).expect("7 is a zoom walkers accepts");
    let projector = walkers::Projector::new(canvas, &memory, walkers::lat_lon(SITE_LAT, SITE_LON));

    let ctx = egui::Context::default();
    ctx.begin_pass(egui::RawInput {
        screen_rect: Some(canvas),
        ..Default::default()
    });
    let img = image(fan_surface(vec![sweep()]));
    let mut ui = egui::Ui::new(
        ctx.clone(),
        egui::Id::new("radar-fan-view"),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(canvas),
    );
    ui.set_opacity(0.25);
    let RadarSurface::Fan(sweeps) = &img.surface else {
        unreachable!("the fixture is a fan")
    };
    assert_eq!(
        draw_radar_fan(
            &ui,
            &projector,
            (img.lat, img.lon),
            sweeps,
            Some(&painter),
            PaneSurfaces::GroundAndGlass
        ),
        FanOutcome::Painted
    );
    let _ = ctx.end_pass();

    let seen = recorder.seen();
    assert_eq!(seen.len(), 1);
    let seen = seen[0];
    assert!((seen.opacity - 0.25).abs() < 1e-6, "{}", seen.opacity);
    assert_eq!(seen.rect, canvas);
    // The map is centred on the site, so the site projects to the rect's
    // centre — the one screen position this fixture can state independently.
    assert!(
        (seen.site_px - canvas.center()).length() < 1e-3,
        "{:?} is not the canvas centre",
        seen.site_px
    );
    assert_eq!(seen.world_px, projector.world_pixels());
    // The level selector is a positive ground scale, and it agrees with the
    // projector's own metres-per-point at the site rather than being a second
    // opinion about zoom.
    let expect_km_per_px = 1.0
        / (f64::from(projector.scale_pixel_per_meter(walkers::lat_lon(SITE_LAT, SITE_LON)))
            * 1000.0);
    assert!((seen.km_per_px - expect_km_per_px).abs() < 1e-12);
    assert!(seen.km_per_px > 0.0);
}

/// [`draw_radar_fan`] over a fresh pass, returning only the outcome.
fn one_issue(
    painter: Option<&Arc<dyn RadarFanPainter>>,
    surfaces: PaneSurfaces,
    sweeps: Vec<FanSweep>,
) -> FanOutcome {
    let ctx = egui::Context::default();
    let canvas = canvas();
    let mut memory = walkers::MapMemory::default();
    memory.set_zoom(7.0).expect("7 is a zoom walkers accepts");
    let projector = walkers::Projector::new(canvas, &memory, walkers::lat_lon(SITE_LAT, SITE_LON));
    ctx.begin_pass(egui::RawInput {
        screen_rect: Some(canvas),
        ..Default::default()
    });
    let img = image(fan_surface(sweeps));
    let ui = egui::Ui::new(
        ctx.clone(),
        egui::Id::new("radar-fan-issue"),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(canvas),
    );
    let RadarSurface::Fan(sweeps) = &img.surface else {
        unreachable!("the fixture is a fan")
    };
    let outcome = draw_radar_fan(
        &ui,
        &projector,
        (img.lat, img.lon),
        sweeps,
        painter,
        surfaces,
    );
    let _ = ctx.end_pass();
    outcome
}

/// **The two arms are different pictures to every cache keyed on the picture.**
///
/// A floor strip that skipped a repaint because a fan's address collided with
/// a texture id would be a stale map on a 3D floor with every gate green, so
/// the key is tagged by arm and not only by value.
#[test]
fn the_two_arms_never_share_a_picture_key() {
    let ctx = egui::Context::default();
    let raster = raster_surface(&ctx);
    let fan = fan_surface(vec![sweep()]);
    assert_ne!(raster.picture_key(), fan.picture_key());
    assert_eq!(raster.picture_key().0, 0);
    assert_eq!(fan.picture_key().0, 1);
    // Same payload, same key; a second payload with identical contents is a
    // different picture as far as an upload is concerned, and says so.
    assert_eq!(fan.picture_key(), fan.clone().picture_key());
    assert_ne!(fan.picture_key(), fan_surface(vec![sweep()]).picture_key());
}

/// A fan holds its own bytes; a raster's pixels live in egui's texture manager
/// and are counted there, so this reports zero for one arm on purpose.
#[test]
fn only_the_fan_arm_reports_host_bytes() {
    let ctx = egui::Context::default();
    assert_eq!(raster_surface(&ctx).resident_bytes(), 0);
    let one = sweep();
    assert_eq!(
        fan_surface(vec![sweep()]).resident_bytes(),
        one.resident_bytes()
    );
    assert_eq!(
        fan_surface(vec![sweep(), sweep()]).resident_bytes(),
        2 * one.resident_bytes()
    );
}

/// A pane playing a one-frame radar loop whose picture is `surface`, which is
/// the only thing about it that ever differs between two calls below.
fn pane_showing(surface: RadarSurface) -> PaneState {
    use crate::pane::{LoopFrame, LoopFrameImage, LoopPhase, TimeMode};

    let at = chrono::NaiveDate::from_ymd_opt(2026, 8, 22)
        .expect("a real date")
        .and_hms_opt(12, 0, 0)
        .expect("a real time");
    let mut pane = PaneState::new();
    {
        let state = pane.time_state_mut(&known::RADAR);
        state.phase = LoopPhase::Paused;
        state.frames = vec![LoopFrame {
            timestamp: at,
            image: Some(LoopFrameImage::PlanView(image(surface))),
            render_in_flight: false,
            render_failed: false,
        }];
    }
    pane.set_time_mode(TimeMode::AsOf(at));
    assert!(
        pane.time_state(&known::RADAR).is_active(),
        "fixture: the key's radar arm only reads a picture while the loop is active",
    );
    assert!(
        pane.active_image().is_some(),
        "fixture: the pane must be showing a plan view for the key to hash one",
    );
    pane
}

/// A ground handler under the **radar** id, so the strip key's walk reaches
/// the radar arm at all. The walk skips a layer that is disabled, unregistered
/// or on the glass, and without this the key would hash the same number for
/// every picture and the assertions below would hold over nothing.
struct RadarGround;

impl FanTestHandler for RadarGround {
    fn id(&self) -> LayerId {
        known::RADAR
    }
    fn time_axis(&self) -> TimeAxis {
        TimeAxis::Live
    }
    fn surface(&self) -> Surface {
        Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        30
    }
    fn display_name(&self) -> &str {
        "Radar"
    }
    fn render_mode(&self) -> squallar_source::handler::RenderMode {
        squallar_source::handler::RenderMode::Texture
    }
    fn data_generation(&self) -> u64 {
        0
    }
    fn has_data(&self, _pane: &squallar_source::handler::PaneRef<'_>) -> bool {
        true
    }
    fn is_fetching(&self) -> bool {
        false
    }
    fn set_fetching(&mut self, _fetching: bool, _pane: &squallar_source::handler::PaneRef<'_>) {}
    fn fetch_time(&self) -> Option<web_time::Instant> {
        None
    }
    fn apply_fetch_result(
        &mut self,
        _result: squallar_source::handler::FetchPayload,
        _pane: &squallar_source::handler::PaneRef<'_>,
    ) {
    }
    fn retain_selections(
        &self,
        _selections: &mut Vec<Arc<dyn OverlayItem>>,
        _pane: &squallar_source::handler::PaneRef<'_>,
    ) {
    }
}

/// The floor strip's content key for that pane. Every other input is fixed, so
/// the radar surface is the only thing that can move this number.
fn strip_key(surface: RadarSurface) -> u64 {
    let mut pane = pane_showing(surface);
    pane.set_overlay_enabled(known::RADAR, true);
    let overlays = OverlayRegistry::with_handlers(vec![Box::new(RadarGround)]);
    assert!(
        overlays.handler_by_id(&known::RADAR).is_some(),
        "fixture: without a registered ground handler the key never reaches the radar arm",
    );
    let preferences = UserPreferences::default();
    let memory = walkers::MapMemory::default();
    ground_content_key(
        &GroundKeyInputs {
            overlays: &overlays,
            preferences: &preferences,
            pane: &pane,
            pane_idx: 0,
            strip: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(512.0, 512.0)),
            centre: walkers::lat_lon(SITE_LAT, SITE_LON),
            memory: &memory,
            basemap_generation: None,
            terrain_generation: None,
            tile_zoom_bias: 0,
            is_dark: false,
            user_location: None,
            user_heading: None,
            user_fix_present: false,
        },
        GroundIsMesh::PLAN_VIEW,
    )
}

/// **A new radar picture moves the 3D floor's content key; the same picture
/// does not.**
///
/// Both halves are the claim. Without the first the floor freezes on one loop
/// frame while the volume above it animates — a stale map with every gate
/// where it was a tick ago. Without the second the strip repaints every frame
/// forever, which is a frame cost paid for nothing.
///
/// This goes at the key rather than through the pane harness on purpose. A
/// harness that advances the playhead also moves the as-of quantum, and every
/// `EventLifetime` layer on the pane re-tokenizes with it — so the strip
/// repaints whether or not the radar arm contributed anything, and a test
/// riding on that would pass with this term deleted. Here nothing moves but
/// the surface.
#[test]
fn a_new_radar_picture_moves_the_floor_key_and_the_same_one_does_not() {
    let ctx = egui::Context::default();
    let one = raster_surface(&ctx);
    let another = RadarSurface::Raster(ctx.load_texture(
        "fan-draw-fixture-second",
        egui::ColorImage::from_rgba_unmultiplied([1, 1], &[0, 0, 0, 255]),
        egui::TextureOptions::NEAREST,
    ));
    assert_ne!(
        strip_key(one.clone()),
        strip_key(another),
        "two different rasters hash the same, so the strip would keep showing the first"
    );
    assert_eq!(
        strip_key(one.clone()),
        strip_key(one),
        "the same raster hashed differently twice, so the strip repaints forever"
    );

    let fan = fan_surface(vec![sweep()]);
    assert_ne!(
        strip_key(fan.clone()),
        strip_key(fan_surface(vec![sweep()])),
        "two different sweep sets hash the same, so a polar floor would freeze"
    );
    assert_eq!(
        strip_key(fan.clone()),
        strip_key(fan.clone()),
        "the same sweep set hashed differently twice"
    );

    // And across the arms: a pane that swapped representation without changing
    // anything else is a new picture to the strip.
    assert_ne!(strip_key(fan), strip_key(raster_surface(&ctx)));
}

/// One full pane walk with radar showing a fan and a renderer installed, over
/// a stack the caller has ordered. Returns which layers the walk dispatched,
/// in order, and the pass's shapes as kind names.
fn ordered_walk(order: impl FnOnce(&mut PaneState)) -> (Vec<LayerId>, Vec<&'static str>) {
    let canvas = canvas();
    let egui_ctx = egui::Context::default();
    let mut overlays = OverlayRegistry::with_handlers(crate::sources::all());
    let mut pane = pane_showing(fan_surface(vec![sweep()]));
    for id in [
        known::RADAR,
        known::BASEMAP_TILES,
        known::CITY_LABELS,
        known::RADAR_SITES,
        known::COLOR_SCALE,
    ] {
        pane.set_overlay_enabled(id, true);
    }
    pane.hydrate_layer_states(&overlays, 0);
    order(&mut pane);

    let mut memory = walkers::MapMemory::default();
    memory.set_zoom(7.0).expect("7 is a zoom walkers accepts");
    let projector = walkers::Projector::new(canvas, &memory, walkers::lat_lon(SITE_LAT, SITE_LON));
    let painter: Arc<dyn RadarFanPainter> = Recorder::new(true);
    let preferences = UserPreferences::default();
    let mut actions = Vec::new();
    let mut click_consumed = false;

    egui_ctx.begin_pass(egui::RawInput {
        screen_rect: Some(canvas),
        ..Default::default()
    });
    let mut ui = egui::Ui::new(
        egui_ctx.clone(),
        egui::Id::new("radar-fan-order"),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(canvas),
    );
    let budget = std::cell::Cell::new(u64::MAX);
    let mut ctx = PaneRenderCtx {
        admission_notice: None,
        cost: None,
        overlay_dispatch_budget: &budget,
        pane_idx: 0,
        pane: &mut pane,
        overlays: &mut overlays,
        user_location: None,
        user_heading: None,
        user_fix: None,
        basemap_labels: Vec::new(),
        galley_cache: &mut walkers::GalleyCache::default(),
        label_cache: &mut crate::label_cache::LabelCache::default(),
        point_text_meshes: &mut crate::point_painter::PointTextMeshes::default(),
        ground_meshes: None,
        radar_fan: Some(&painter),
        basemap_tiles: None,
        terrain_tiles: None,
        tile_zoom_bias: 0,
        overlay_render_limit: 1,
        overlay_overdraw: crate::overlay_cache::OVERDRAW_FRACTION,
        actions: &mut actions,
        pane_rect: canvas,
        surfaces: PaneSurfaces::GroundAndGlass,
        draws_3d_ground: GroundIsMesh::PLAN_VIEW,
        horizontal_color_scale: true,
        color_scale_floor: canvas.max.y,
        pointer_available: false,
        excluded_rects: Vec::new(),
        long_press_pos: None,
        overlay_click_pos: None,
        click_consumed: &mut click_consumed,
        preferences: &preferences,
        paint_order: Vec::new(),
    };
    render_pane_map_content(&mut ui, &projector, memory.zoom(), &mut ctx);
    let dispatched: Vec<LayerId> = ctx.paint_order.iter().map(|(id, _)| id.clone()).collect();
    let shapes: Vec<&'static str> = egui_ctx
        .end_pass()
        .shapes
        .into_iter()
        .map(|clipped| match clipped.shape {
            egui::Shape::Callback(_) => "callback",
            egui::Shape::Noop => "noop",
            _ => "shape",
        })
        .collect();
    (dispatched, shapes)
}

/// **Radar is an overlay like any other, and the fan draws where the user put
/// it.**
///
/// The polar path draws through a paint callback rather than a textured
/// rectangle, and a callback is the one shape that could plausibly have been
/// hoisted out of the walk — issued once per pane beside the ground callbacks,
/// where it would ride inside a GPU state reset the basemap already paid for.
/// That siting is cheaper and it is not what this does, because it would pin
/// radar beneath every layer in the stack and stop it answering to the user's
/// ordering at all. Radar has no special slot at either end.
///
/// So the claim is the ordinary one, and it is stated the ordinary way: move
/// radar in the draw order and the fan's callback moves with it, by the same
/// rule that moves any other layer's shapes.
#[test]
fn the_fan_draws_at_the_position_the_user_ordered_radar_into() {
    let radar_first = |pane: &mut PaneState| pane.set_draw_order(&[known::RADAR]);
    let radar_last = |pane: &mut PaneState| {
        let rest: Vec<LayerId> = pane
            .draw_order_vec()
            .into_iter()
            .filter(|id| *id != known::RADAR)
            .collect();
        pane.set_draw_order(&rest);
    };

    let (first_order, first_shapes) = ordered_walk(radar_first);
    let (last_order, last_shapes) = ordered_walk(radar_last);

    // The walk dispatched radar where the order put it, and the two orders
    // really are different — otherwise everything below holds over one scene
    // walked twice.
    assert_eq!(
        first_order.first(),
        Some(&known::RADAR),
        "radar was ordered first and the walk dispatched {first_order:?}"
    );
    assert_eq!(
        last_order.last(),
        Some(&known::RADAR),
        "radar was ordered last and the walk dispatched {last_order:?}"
    );
    assert!(
        first_order.len() > 1,
        "fixture: one dispatched layer cannot show a position"
    );

    // And the fan's callback is where that put it. Counted as the shapes drawn
    // before it, which is a position in the pass and not a claim about which
    // layer drew what.
    let before = |shapes: &[&str]| {
        shapes
            .iter()
            .position(|s| *s == "callback")
            .expect("the fan was issued as a callback")
    };
    let (early, late) = (before(&first_shapes), before(&last_shapes));
    assert!(
        early < late,
        "the fan drew at the same point ({early} vs {late} shapes before it) \
         whichever end of the stack radar was ordered into, so it is not \
         taking its position from the order",
    );
    // Exactly one callback either way: the fan is one draw per pane, and the
    // count is what the reset cost is a function of.
    assert_eq!(first_shapes.iter().filter(|s| **s == "callback").count(), 1);
    assert_eq!(last_shapes.iter().filter(|s| **s == "callback").count(), 1);
}

// ── The still pane's own arm ────────────────────────────────────────────────

/// **A still pane holding a plane draws it through the renderer**, at radar's
/// own position in the pane's layer walk.
///
/// The loop arm above is exercised through `render_radar_overlay` directly;
/// this one has to go through the whole walk, because the still branch is
/// *selected* there — the pane holds one surface or the other and the walk is
/// where it asks which. So the harness runs real frames, and what is asserted
/// is that the installed painter was asked for a payload with this pane's
/// sweep in it.
///
/// **The raster control is the second half**, and it is what stops this
/// reading green through a walk that asks the painter about everything: the
/// same pane, the same site, the same frames, a texture instead of a plane,
/// and the painter is never asked.
///
/// TAMPER: drop the `still_radar_fan` arm from the walk's radar branch and the
/// first assertion goes red; ask the painter unconditionally and the control
/// goes red.
#[test]
fn a_still_pane_holding_a_plane_draws_it_through_the_renderer() {
    let recorder = Recorder::new(true);
    let painter: Arc<dyn RadarFanPainter> = recorder.clone();
    let mut harness = crate::input_harness::InputHarness::new();
    harness.install_fan_painter(Arc::clone(&painter));
    harness.load_scan("KTLX");
    harness.place_radar_fan(
        0,
        &squallar_radar::fields::known::REFLECTIVITY,
        0.5,
        Arc::new(sweep()),
    );

    let seen = recorder.seen();
    assert!(
        !seen.is_empty(),
        "a still pane holding a plane never reached the renderer, so its \
         radar layer drew nothing at all",
    );
    assert!(
        seen.iter().all(|s| s.sweeps == 1),
        "the still pane submitted {seen:?}, not its one cut",
    );

    // The control: the same pane, the same frames, a texture instead.
    let control = Recorder::new(true);
    let control_painter: Arc<dyn RadarFanPainter> = control.clone();
    let mut harness = crate::input_harness::InputHarness::new();
    harness.install_fan_painter(Arc::clone(&control_painter));
    harness.load_scan("KTLX");
    harness.place_radar_image(
        0,
        &squallar_radar::fields::known::REFLECTIVITY,
        0.5,
        None,
        None,
        None,
    );
    assert!(
        control.seen().is_empty(),
        "a still pane showing a texture asked the fan renderer for a payload",
    );
}
