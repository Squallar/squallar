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
use crate::radar_fan::{FanDraw, FanOutcome, FanRefusal, FanSweep, RadarFanPainter};
use crate::pane::{PaneState, RadarSurface};
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
        self.seen.lock().expect("no test panics holding this").clone()
    }
}

impl RadarFanPainter for Recorder {
    fn payload(&self, draw: FanDraw<'_>) -> Option<std::sync::Arc<dyn std::any::Any + Send + Sync>> {
        self.seen.lock().expect("no test panics holding this").push(Seen {
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
            earth_radius_km: squallar_geo::EARTH_RADIUS_KM,
            effective_radius_km: squallar_radar::beam::RE_EFF_KM,
        },
    }
}

fn fan_surface(sweeps: Vec<FanSweep>) -> RadarSurface {
    RadarSurface::Fan(std::sync::Arc::from(
        sweeps.into_iter().map(std::sync::Arc::new).collect::<Vec<_>>(),
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
    on_floor_strip: bool,
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
        &ui,
        &projector,
        &img,
        &mut pane,
        canvas,
        &prefs,
        painter,
        on_floor_strip,
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
    let raster = overlay_pass(raster_surface, None, false);
    let fan = overlay_pass(|_| fan_surface(vec![sweep()]), Some(&painter), false);

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
    assert!(!overlay_pass(raster_surface, None, false).contains(&"callback"));
}

/// One outcome per refusal, each against a fixture healthy in every other
/// respect.
#[test]
fn every_refusal_is_named_and_none_of_them_draws() {
    let answering: Arc<dyn RadarFanPainter> = Recorder::new(true);
    let declining: Arc<dyn RadarFanPainter> = Recorder::new(false);

    let cases: Vec<(FanOutcome, Option<&Arc<dyn RadarFanPainter>>, bool, Vec<FanSweep>)> = vec![
        (FanOutcome::Painted, Some(&answering), false, vec![sweep()]),
        (
            FanOutcome::Refused(FanRefusal::NoPainter),
            None,
            false,
            vec![sweep()],
        ),
        (
            FanOutcome::Refused(FanRefusal::FloorStrip),
            Some(&answering),
            true,
            vec![sweep()],
        ),
        (
            FanOutcome::Refused(FanRefusal::PainterDeclined),
            Some(&declining),
            false,
            vec![sweep()],
        ),
        (
            FanOutcome::Refused(FanRefusal::Malformed),
            Some(&answering),
            false,
            vec![FanSweep {
                edges: vec![[0.0, 90.0]],
                ..sweep()
            }],
        ),
        (
            FanOutcome::Refused(FanRefusal::Malformed),
            Some(&answering),
            false,
            Vec::new(),
        ),
    ];

    for (want, painter, on_floor_strip, sweeps) in cases {
        let got = one_issue(painter, on_floor_strip, sweeps);
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
        one_issue(Some(&painter), true, vec![sweep()]),
        FanOutcome::Refused(FanRefusal::FloorStrip)
    );
    assert!(
        recorder.seen().is_empty(),
        "the strip pass asked the painter for a payload it would then discard"
    );
    // The control: the same painter, the same sweeps, off the strip.
    assert_eq!(
        one_issue(Some(&painter), false, vec![sweep()]),
        FanOutcome::Painted
    );
    assert_eq!(recorder.seen().len(), 1);
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
        one_issue(Some(&painter), false, vec![sweep(), bad]),
        FanOutcome::Refused(FanRefusal::Malformed)
    );
    assert!(recorder.seen().is_empty());
    // Healthy control at the same arity, so the refusal is the payload's and
    // not "two sweeps".
    assert_eq!(
        one_issue(Some(&painter), false, vec![sweep(), sweep()]),
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
        draw_radar_fan(&ui, &projector, &img, sweeps, Some(&painter), false),
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
    let expect_km_per_px =
        1.0 / (f64::from(projector.scale_pixel_per_meter(walkers::lat_lon(SITE_LAT, SITE_LON))) * 1000.0);
    assert!((seen.km_per_px - expect_km_per_px).abs() < 1e-12);
    assert!(seen.km_per_px > 0.0);
}

/// [`draw_radar_fan`] over a fresh pass, returning only the outcome.
fn one_issue(
    painter: Option<&Arc<dyn RadarFanPainter>>,
    on_floor_strip: bool,
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
    let outcome = draw_radar_fan(&ui, &projector, &img, sweeps, painter, on_floor_strip);
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
