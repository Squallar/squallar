//! **What one gesture is allowed to cost.**
//!
//! The gate here is a *count of dispatches across a synthetic gesture*, never a
//! duration: the clock is the frame index, the pipeline is a whole number of
//! frames, and the quantity the user reported — "even the smallest of pans
//! causes tiles to have to be re-rendered and nws alerts redrawn" — is a count
//! of full-size rasterizations, not a latency.
//!
//! Every plan below comes out of [`plan_overlay_texture`] at a real oversample
//! rung rather than being written by hand, because the defect these tests pin
//! lived in that function: a hand-built plan would have hidden it.

use super::*;

/// The content token every question here is asked with, and the one every
/// fixture picture carries — so no answer below is ever a token mismatch.
const TOKEN: u64 = 4242;

/// The zoom a pan is driven at. Held bit-identical across every frame of a pan,
/// which is what makes a pan run's dispatch count attributable: the zoom arm
/// and the zoom band arm cannot be what answered.
const ZOOM: f64 = 9.0;

/// The pane the figures in this module are quoted for: the user's own window,
/// in points, at two device pixels per point — 2878x1610 physical.
const PANE_POINTS: [f32; 2] = [1439.0, 805.0];
const DENSITY: f32 = 2.0;

/// An adapter limit no plan in this module reaches. The measured
/// `max_texture_dimension_2d` on every browser arm this tree has legs for is
/// 8192 or more; [`a_pane_over_the_adapter_limit_still_gets_a_margin`] is the
/// one test that deliberately goes under it.
const MAX_SIDE: u32 = 16384;

/// Texture layers on the pane. Six is the user's own enabled set — every
/// handler whose `render_mode()` is `Texture` — and it is the denominator every
/// dispatch count in this module divides by.
const LAYERS: usize = 6;

/// Frames a gesture runs for, and frames of rest after it. The rest has to
/// outlast the settle countdown plus a whole raster and upload, or the
/// end-of-gesture picture has not landed when the correctness arm looks.
const GESTURE_FRAMES: u32 = 120;
const REST_FRAMES: u32 = 30;

fn pane_rect() -> egui::Rect {
    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(PANE_POINTS[0], PANE_POINTS[1]))
}

/// The plan a pane really produces at `oversample`, through the production
/// planner. 100 is the ladder's bottom rung and the one the user's browser
/// settles on.
fn plan_at(oversample: u16) -> OverlayTexturePlan {
    plan_overlay_texture(
        pane_rect(),
        MAX_SIDE,
        DENSITY,
        overdraw_for_oversample(oversample),
    )
}

/// A viewport one degree across, `east` viewports east of where it started.
fn viewport_at(east: f64) -> GeoBounds {
    GeoBounds {
        min_lat: 34.5,
        max_lat: 35.5,
        min_lon: -97.5 + east,
        max_lon: -96.5 + east,
    }
}

/// The span a zoom-out starts from. Small enough that ten notches — a factor of
/// 1024 — land inside the Mercator-valid range: `plan.coverage` clamps latitude
/// to it, so a viewport that runs past the pole is one no picture can ever be
/// said to cover and the correctness arm would fail on the projection rather
/// than on anything this module is about.
const ZOOM_OUT_START_SPAN: f64 = 0.05;

/// A viewport `span` degrees across, centred where [`viewport_at`] starts.
fn viewport_span(span: f64) -> GeoBounds {
    GeoBounds {
        min_lat: 35.0 - span / 2.0,
        max_lat: 35.0 + span / 2.0,
        min_lon: -97.0 - span / 2.0,
        max_lon: -97.0 + span / 2.0,
    }
}

/// Whether a picture's ground contains the whole viewport — what "the pane has
/// something to draw here" means, and the only thing the viewer can tell.
fn covers(tex: &GeoBounds, vp: &GeoBounds) -> bool {
    tex.min_lat <= vp.min_lat
        && tex.max_lat >= vp.max_lat
        && tex.min_lon <= vp.min_lon
        && tex.max_lon >= vp.max_lon
}

fn a_texture(ctx: &egui::Context) -> egui::TextureHandle {
    ctx.load_texture(
        "fixture",
        egui::ColorImage::filled([1, 1], egui::Color32::RED),
        egui::TextureOptions::NEAREST,
    )
}

fn data_for(
    texture: &egui::TextureHandle,
    plan: &OverlayTexturePlan,
    vp: &GeoBounds,
    zoom: f64,
) -> OverlayTextureData {
    OverlayTextureData {
        texture: texture.clone(),
        placed: PlacedRaster::of(plan.coverage(vp)),
        data_generation: TOKEN,
        render_zoom: current_quantized_zoom(zoom),
        width: plan.width,
        height: plan.height,
        radar_meta: None,
        hit_map: None,
    }
}

// ── The rig ──────────────────────────────────────────────────────────────

/// One texture layer's own pipeline on the pane.
struct Layer {
    cache: OverlayTextureCache,
    /// Rasters on the wire: the frame each lands on, and what it was asked for.
    /// The *mark* lives in the cache, exactly as the app's does; this is the
    /// wire the app cannot see.
    in_flight: Vec<(u32, GeoBounds, f64)>,
    bands_left: u32,
    dispatches: u32,
    superseded: u32,
    promotions: u32,
    /// Frames on which the pane had nothing covering the viewport to draw.
    dry: u32,
}

/// A pane of [`LAYERS`] texture layers, run frame by frame in the order
/// `ui_map_pane` runs it: promote a delivered hold, take the raster that came
/// back, ask the gate, dispatch what it admits, then move this frame's bands.
struct GestureRig {
    layers: Vec<Layer>,
    texture: egui::TextureHandle,
    plan: OverlayTexturePlan,
    /// Frames the rasterizer takes before the upload starts, and frames the
    /// banded upload takes: 2 with a staging ring, 3 without. Three is the
    /// deeper pipeline and the one the published sweeps are quoted at.
    raster_frames: u32,
    upload_frames: u32,
    /// The device's `Budgets::concurrent_renders`, which is what the draw loop
    /// hands [`RendersInFlight::admits`].
    limit: usize,
    /// The most dispatches any single frame asked for, across all layers —
    /// what a settle that drains every layer at once costs one frame.
    peak_frame_batch: u32,
}

impl GestureRig {
    fn new(ctx: &egui::Context, plan: OverlayTexturePlan, start: &GeoBounds, zoom: f64) -> Self {
        let texture = a_texture(ctx);
        let layers = (0..LAYERS)
            .map(|_| {
                let mut cache = OverlayTextureCache::new();
                cache.show(data_for(&texture, &plan, start, zoom));
                Layer {
                    cache,
                    in_flight: Vec::new(),
                    bands_left: 0,
                    dispatches: 0,
                    superseded: 0,
                    promotions: 0,
                    dry: 0,
                }
            })
            .collect();
        Self {
            layers,
            texture,
            plan,
            raster_frames: 1,
            upload_frames: 3,
            limit: 1,
            peak_frame_batch: 0,
        }
    }

    fn frame(&mut self, f: u32, vp: GeoBounds, zoom: f64, drive: ZoomDrive) {
        let mut batch = 0;
        for layer in &mut self.layers {
            if layer.bands_left == 0
                && let Some(held) = layer.cache.take_held_if_delivered(|_| true)
            {
                layer.cache.show(held.data);
                layer.promotions += 1;
            }

            let mut landed: Vec<(GeoBounds, f64)> = Vec::new();
            layer.in_flight.retain(|(arrives, for_vp, for_zoom)| {
                if f >= *arrives {
                    landed.push((*for_vp, *for_zoom));
                    false
                } else {
                    true
                }
            });
            for (for_vp, for_zoom) in landed {
                // The app's own arrival test: a raster is filed only while the
                // cache is still waiting for that very dispatch.
                if !layer
                    .cache
                    .renders
                    .retire(&RenderTicket::whole(TOKEN, self.plan.coverage(&for_vp)))
                {
                    continue;
                }
                let data = data_for(&self.texture, &self.plan, &for_vp, for_zoom);
                if layer.cache.current().is_none() {
                    layer.cache.show(data);
                } else {
                    if layer.cache.is_holding() {
                        layer.superseded += 1;
                    }
                    layer.cache.hold(data, None);
                    layer.bands_left = self.upload_frames;
                }
            }

            // Asked every frame the overlay is live, exactly as `ui_map_pane`
            // asks it, and only the dispatch is gated on the in-flight mark.
            let needs = layer
                .cache
                .needs_rerender(TOKEN, zoom, drive, &vp, &self.plan);
            if needs && layer.cache.renders.admits(RenderSlot::WHOLE, self.limit) {
                layer
                    .cache
                    .renders
                    .record(RenderTicket::whole(TOKEN, self.plan.coverage(&vp)));
                layer.in_flight.push((f + self.raster_frames, vp, zoom));
                layer.dispatches += 1;
                batch += 1;
            }

            if !layer
                .cache
                .current()
                .is_some_and(|t| covers(&t.placed.geo, &vp))
            {
                layer.dry += 1;
            }

            if layer.cache.is_holding() {
                layer.bands_left = layer.bands_left.saturating_sub(1);
            }
        }
        self.peak_frame_batch = self.peak_frame_batch.max(batch);
    }

    fn dispatches(&self) -> u32 {
        self.layers.iter().map(|l| l.dispatches).sum()
    }
    fn superseded(&self) -> u32 {
        self.layers.iter().map(|l| l.superseded).sum()
    }
    fn dry(&self) -> u32 {
        self.layers.iter().map(|l| l.dry).sum()
    }

    /// Whether every layer is drawing a picture rasterised for `zoom` that
    /// covers `vp` — "the gesture ended with the right picture on the glass".
    fn settled_correctly(&self, vp: &GeoBounds, zoom: f64) -> bool {
        self.layers.iter().all(|l| {
            l.cache.current().is_some_and(|t| {
                covers(&t.placed.geo, vp) && t.render_zoom == current_quantized_zoom(zoom)
            })
        })
    }
}

/// A pan of `viewports` viewports, driven with a live gesture the whole way and
/// then let go.
fn run_pan(ctx: &egui::Context, plan: OverlayTexturePlan, viewports: f64) -> GestureRig {
    let mut rig = GestureRig::new(ctx, plan, &viewport_at(0.0), ZOOM);
    for f in 0..GESTURE_FRAMES {
        let t = f as f64 / GESTURE_FRAMES as f64;
        rig.frame(f, viewport_at(viewports * t), ZOOM, ZoomDrive::LIVE);
    }
    for f in GESTURE_FRAMES..GESTURE_FRAMES + REST_FRAMES {
        rig.frame(f, viewport_at(viewports), ZOOM, ZoomDrive::AT_REST);
    }
    rig
}

/// `notches` zoom levels out, smoothed across the gesture the way a wheel's
/// scroll smoothing really delivers it, and then let go.
fn run_zoom_out(ctx: &egui::Context, plan: OverlayTexturePlan, notches: f64) -> GestureRig {
    let span = |t: f64| ZOOM_OUT_START_SPAN * 2f64.powf(notches * t);
    let mut rig = GestureRig::new(ctx, plan, &viewport_span(span(0.0)), ZOOM);
    for f in 0..GESTURE_FRAMES {
        let t = f as f64 / GESTURE_FRAMES as f64;
        rig.frame(
            f,
            viewport_span(span(t)),
            ZOOM - notches * t,
            ZoomDrive::LIVE,
        );
    }
    let end_vp = viewport_span(span(1.0));
    for f in GESTURE_FRAMES..GESTURE_FRAMES + REST_FRAMES {
        rig.frame(f, end_vp, ZOOM - notches, ZoomDrive::AT_REST);
    }
    rig
}

// ── The planner: ground and texels are two numbers ───────────────────────

/// **The bottom rung keeps a pan margin, and it costs no bytes to keep it.**
///
/// Before 2026-09-05 `plan_overlay_texture` spent the whole oversample budget on
/// ground, so oversample 100 — the rung the ladder walks to under host pressure
/// — produced `overdraw == 0.0`: a picture covering exactly the viewport it was
/// rasterised for, whose entire pan margin was the one-texel deadband. This is
/// the property that replaced it, and the byte equality beside it is what makes
/// it free.
#[test]
fn the_bottom_rung_keeps_a_pan_margin_for_the_same_bytes() {
    let full = plan_at(150);
    let bottom = plan_at(100);

    assert!(
        bottom.overdraw >= (MIN_COVERAGE_SCALE - 1.0) / 2.0,
        "the bottom rung planned {} of overdraw: a picture covering exactly its \
         own viewport re-rasterises on any pan a person can see",
        bottom.overdraw,
    );

    // The bytes. `pane_px` and both sides are what the device profile's
    // `picture_bytes` prices, and none of them may move.
    assert_eq!(
        bottom.pane_px, full.pane_px,
        "the glass a picture covers is the pane, whatever rung it is planned at",
    );
    let pane = pane_rect();
    let expect = |points: f32, scale: f32| ((points * DENSITY * scale) as u32).min(MAX_SIDE);
    assert_eq!(
        [bottom.width, bottom.height],
        [expect(pane.width(), 1.0), expect(pane.height(), 1.0)],
        "the bottom rung must still plan exactly one texel per device pixel of \
         glass — the margin is bought with resolution, not with bytes",
    );
    assert_eq!(
        [full.width, full.height],
        [expect(pane.width(), 1.5), expect(pane.height(), 1.5)],
        "the top rung's texel count must not have moved either",
    );

    // And what pays for it: the density the rasterizer is told to draw at.
    assert_eq!(
        full.pixels_per_point, DENSITY,
        "a rung whose asked scale already clears the coverage floor must draw \
         at the display's own density, bit-exact",
    );
    assert!(
        bottom.pixels_per_point < DENSITY,
        "the ground has to be paid for out of resolution, or it is not free",
    );
    assert_eq!(
        bottom.pixels_per_point,
        DENSITY / MIN_COVERAGE_SCALE,
        "at the bottom rung the whole coverage floor is bought with resolution",
    );
}

/// **A pane wider than the adapter's texture limit gets a margin too**, which
/// is the case the coverage floor could most easily have missed: the planner
/// gives up overdraw rather than exceed `max_texture_side`, so before this
/// change that path resolved to `0.0` at *every* rung and no ladder movement
/// could reach it.
///
/// The floor is applied after that clamp, not before, so the clamp still bounds
/// the texel count and the ground is widened underneath it. Nothing in the tree
/// is known to plan here — measured `max_texture_dimension_2d` is 8192 or more
/// on every browser arm — and that is exactly why it needs a test.
#[test]
fn a_pane_over_the_adapter_limit_still_gets_a_margin() {
    let limit = 2048;
    let plan = plan_overlay_texture(pane_rect(), limit, DENSITY, OVERDRAW_FRACTION);
    assert_eq!(
        [plan.width, plan.height],
        [limit, limit.min((PANE_POINTS[1] * DENSITY) as u32)],
        "fixture: this pane must really be over the limit on at least one axis, \
         or the clamp under test never fires",
    );
    assert!(
        plan.overdraw >= (MIN_COVERAGE_SCALE - 1.0) / 2.0,
        "a clamped pane planned {} of overdraw: the pan trigger there is the \
         one-texel deadband and no rung of the ladder can move it",
        plan.overdraw,
    );
    assert!(
        plan.pixels_per_point < DENSITY,
        "a clamped picture cannot be at full density and cover more ground than \
         its own viewport at the same time",
    );
}

// ── The gesture: one gesture is not one raster per frame ──────────────────

/// **The user's sentence, counted.** "Even the smallest of pans causes tiles to
/// have to be re-rendered and nws alerts redrawn."
///
/// A pan of 0.05 of a viewport — five percent of the pane — across
/// [`GESTURE_FRAMES`] frames of live gesture and [`REST_FRAMES`] of rest, on
/// [`LAYERS`] texture layers at the ladder's bottom rung. **Measured on this
/// rig before the fix: 288 full-size rasters dispatched and 144 of them thrown
/// away mid-upload** — 32% of the 900 layer-frames in the run. And the same run
/// panning 2 whole viewports spent 294, which is the tell: the count did not
/// move with the distance, because the trigger was motion and not distance.
#[test]
fn the_smallest_pan_rerasterizes_nothing() {
    let ctx = egui::Context::default();
    let rig = run_pan(&ctx, plan_at(100), 0.05);

    assert_eq!(
        rig.dispatches(),
        0,
        "a pan of a twentieth of a viewport spent {} full-size rasters across \
         {LAYERS} layers and threw away {} of them. The picture on screen \
         covers {MIN_COVERAGE_SCALE} viewports; nothing about this pan needs a \
         new one",
        rig.dispatches(),
        rig.superseded(),
    );
    assert_eq!(
        rig.dry(),
        0,
        "and the pane must never have been left without a picture to draw",
    );
}

/// The other arm of the same gate: **a pan that really does leave the picture
/// still gets one**, and ends with the right picture on the glass. Without this
/// the test above is satisfied by a gate that never dispatches at all.
#[test]
fn a_pan_that_leaves_the_picture_is_answered_and_settles_correctly() {
    let ctx = egui::Context::default();
    let rig = run_pan(&ctx, plan_at(100), 2.0);

    assert!(
        rig.dispatches() > 0,
        "a two-viewport pan asked for no rasters at all: the coalescing has \
         stopped answering pans rather than stopped repeating them",
    );
    assert!(
        rig.settled_correctly(&viewport_at(2.0), ZOOM),
        "the gesture ended with a picture that does not cover where it ended",
    );
    // **Bounded by the ground crossed, not by the frame count** — which is the
    // whole of the defect stated as a property. The picture covers
    // MIN_COVERAGE_SCALE viewports, so it holds for a margin of
    // `(MIN_COVERAGE_SCALE - 1) / 2` of a viewport on the side the pan is
    // heading; crossing that much ground is what genuinely needs another
    // picture, and one more falls out at the settle. Derived here rather than
    // fitted, so the bound moves with the constant instead of pinning a
    // measurement.
    let crossings = (2.0 / f64::from((MIN_COVERAGE_SCALE - 1.0) / 2.0)).ceil() as u32;
    let ceiling = (crossings + 1) * LAYERS as u32;
    assert!(
        rig.dispatches() <= ceiling,
        "a two-viewport pan spent {} rasters across {LAYERS} layers against a \
         ceiling of {ceiling}: more than the ground it crossed can account for, \
         so something is dispatching on frames rather than on distance",
        rig.dispatches(),
    );
    assert_eq!(
        rig.superseded(),
        0,
        "{} uploads were thrown away mid-flight. A dispatch made while the view \
         is still moving supersedes by construction; none should be made",
        rig.superseded(),
    );
}

/// **A ten-notch zoom-out is one rebuild, not one per notch and not one per
/// frame.** Measured on this rig before the fix: 294 dispatched, 144 superseded.
///
/// A zoom-out is the harder half, because growing the viewport leaves ground the
/// picture genuinely does not cover — so unlike a pan it cannot simply be
/// refused, or the pane draws nothing at its edges. What it can be is bounded by
/// the pipeline instead of by the frame rate.
#[test]
fn a_ten_notch_zoom_out_is_bounded_by_the_pipeline_not_the_frame_rate() {
    let ctx = egui::Context::default();
    let rig = run_zoom_out(&ctx, plan_at(100), 10.0);

    assert!(
        rig.dispatches() > 0,
        "fixture: a ten-notch zoom-out must ask for rasters, or this test is \
         measuring nothing",
    );
    // Derived, not fitted, the same way the pan's is: a picture covering
    // MIN_COVERAGE_SCALE viewports is outgrown when the span has multiplied by
    // that much, so ten notches — a factor of 1024 — outgrow it
    // `log(1024) / log(MIN_COVERAGE_SCALE)` times, and one more falls out at
    // the settle. That the count sits *at* this floor is the property: it is a
    // function of the ground the gesture crossed and of nothing else.
    let crossings = (10.0f64.mul_add(2f64.ln(), 0.0) / f64::from(MIN_COVERAGE_SCALE).ln()).ceil();
    let ceiling = (crossings as u32 + 1) * LAYERS as u32;
    assert!(
        rig.dispatches() <= ceiling,
        "a ten-notch zoom-out spent {} rasters across {LAYERS} layers in {} \
         frames, against a ceiling of {ceiling} derived from the ground it \
         crossed",
        rig.dispatches(),
        GESTURE_FRAMES + REST_FRAMES,
    );
    assert_eq!(
        rig.superseded(),
        0,
        "{} uploads were thrown away mid-flight during one zoom-out",
        rig.superseded(),
    );
    assert!(
        rig.settled_correctly(
            &viewport_span(ZOOM_OUT_START_SPAN * 2f64.powf(10.0)),
            ZOOM - 10.0
        ),
        "the zoom-out ended with a picture that is not the settled view",
    );
}

/// **The settle drains every layer onto one frame**, which is the hitch a
/// person reads as pan lag. Pinned as a count so that a fix which bands it —
/// spreading the drain across frames — has something to move, and so that a
/// change which makes it *worse* is a build failure.
#[test]
fn the_settle_drain_is_every_layer_on_one_frame() {
    let ctx = egui::Context::default();
    let rig = run_pan(&ctx, plan_at(100), 2.0);
    assert_eq!(
        rig.peak_frame_batch, LAYERS as u32,
        "the settle asks {LAYERS} layers for a full-screen raster on the same \
         frame. This is the known cost of the drain and is not banded here; if \
         it has moved, say which way and why",
    );
}
