//! Whose picture the coverage question is asked of, and what a coverage
//! dispatch is allowed to throw away.

use super::*;

/// The content token every question here is asked with, and the one every
/// fixture texture carries — so a `true` answer is never a token mismatch.
const TOKEN: u64 = 4242;

/// The zoom everything is rasterised at and asked at, so the settle arm and
/// the mid-gesture band arm can never be what answers.
const ZOOM: f64 = 7.0;

/// A wall-clock origin far from zero, in the unit the clock parameter uses.
const T0: f64 = 100.0;

/// Fixture texture dimensions — only their consistency with [`plan`] matters.
const W: u32 = 8;
const H: u32 = 5;

/// The overdraw the fixtures are planned and measured at, which is the app's.
const OVERDRAW: f32 = OVERDRAW_FRACTION;

fn plan() -> OverlayTexturePlan {
    OverlayTexturePlan {
        width: W,
        height: H,
        overdraw: OVERDRAW,
        pixels_per_point: 1.0,
    }
}

/// A one-degree viewport, `east` degrees east of where the pan started.
fn viewport_at(east: f64) -> GeoBounds {
    GeoBounds {
        min_lat: 34.5,
        max_lat: 35.5,
        min_lon: -97.5 + east,
        max_lon: -96.5 + east,
    }
}

/// Whether a texture's ground contains the whole viewport — what "the pane has
/// something to draw here" means, and the only thing the viewer can tell.
fn covers(tex: &GeoBounds, vp: &GeoBounds) -> bool {
    tex.min_lat <= vp.min_lat
        && tex.max_lat >= vp.max_lat
        && tex.min_lon <= vp.min_lon
        && tex.max_lon >= vp.max_lon
}

/// A picture rasterised for `vp`, satisfying every arm of the gate except
/// coverage.
fn data_for(texture: &egui::TextureHandle, vp: &GeoBounds) -> OverlayTextureData {
    OverlayTextureData {
        texture: texture.clone(),
        placed: PlacedRaster::of(plan().coverage(vp)),
        data_generation: TOKEN,
        render_zoom: current_quantized_zoom(ZOOM),
        width: W,
        height: H,
        radar_meta: None,
        hit_map: None,
    }
}

fn a_texture(ctx: &egui::Context) -> egui::TextureHandle {
    ctx.load_texture(
        "fixture",
        egui::ColorImage::filled([1, 1], egui::Color32::RED),
        egui::TextureOptions::NEAREST,
    )
}

// ── The dispatch loop, driven frame by frame ─────────────────────────────

/// The pane's frame, in the order [`crate::pane::PaneState`]'s host runs it:
/// promote a delivered hold, take the raster that came back, ask the gate,
/// dispatch what it asks for, draw, then move this frame's upload bands.
///
/// Everything here is counted, never timed: the clock is the frame index.
struct PanRig {
    cache: OverlayTextureCache,
    texture: egui::TextureHandle,
    /// Degrees of longitude the viewport moves per frame. One viewport is one
    /// degree, so `1.0 / 28.0` is one viewport per 28 frames.
    step: f64,
    /// Frames between a dispatch and the raster coming back.
    raster_frames: u32,
    /// Frames a banded upload takes: 2 with a staging ring, 3 without.
    upload_frames: u32,
    /// The dispatch the app would have marked `render_in_flight` for, and the
    /// viewport it was rasterised for.
    in_flight: Option<(u32, GeoBounds)>,
    bands_left: u32,
    pub dispatches: u32,
    pub promotions: u32,
    pub superseded: u32,
    /// Frames on which the pane had nothing covering the viewport to draw.
    pub dry: u32,
    pub counted: u32,
}

/// Frames run before anything is counted, so a cold cache is not a dry read.
const WARMUP: u32 = 20;

impl PanRig {
    fn new(ctx: &egui::Context, step: f64, upload_frames: u32) -> Self {
        let texture = a_texture(ctx);
        let mut cache = OverlayTextureCache::new();
        cache.show(data_for(&texture, &viewport_at(0.0)));
        Self {
            cache,
            texture,
            step,
            raster_frames: 1,
            upload_frames,
            in_flight: None,
            bands_left: 0,
            dispatches: 0,
            promotions: 0,
            superseded: 0,
            dry: 0,
            counted: 0,
        }
    }

    fn run(&mut self, frames: u32) {
        for f in 0..frames {
            let vp = viewport_at(f as f64 * self.step);

            if self.bands_left == 0
                && let Some(held) = self.cache.take_held_if_delivered(|_| true)
            {
                self.cache.show(held.data);
                if f >= WARMUP {
                    self.promotions += 1;
                }
            }

            if let Some((arrives, dispatched_for)) = self.in_flight
                && f >= arrives
            {
                let data = data_for(&self.texture, &dispatched_for);
                if self.cache.current().is_none() {
                    self.cache.show(data);
                } else {
                    if self.cache.is_holding() && f >= WARMUP {
                        self.superseded += 1;
                    }
                    self.cache.hold(data, None);
                    self.bands_left = self.upload_frames;
                }
                self.in_flight = None;
            }

            // The gate is asked every frame the overlay is live, exactly as
            // `ui_map_pane` asks it, and only the dispatch is gated on the
            // in-flight mark.
            let now = T0 + f as f64 / 60.0;
            let needs = self.cache.needs_rerender(TOKEN, ZOOM, now, &vp, &plan());
            if needs && self.in_flight.is_none() {
                self.in_flight = Some((f + self.raster_frames, vp));
                if f >= WARMUP {
                    self.dispatches += 1;
                }
            }

            if f >= WARMUP {
                self.counted += 1;
                if !self
                    .cache
                    .current()
                    .is_some_and(|tex| covers(&tex.placed.geo, &vp))
                {
                    self.dry += 1;
                }
            }

            if self.cache.is_holding() {
                self.bands_left = self.bands_left.saturating_sub(1);
            }
        }
    }
}

/// **The freeze.** A fling past what the pipeline can follow used to close into
/// a loop with no exit: the gate read the picture still crossing to the GPU,
/// found it short of margin, dispatched, and the raster that came back replaced
/// the one that was uploading — over and over, so nothing ever reached the
/// screen and the pane drew the picture it had when the fling started, for as
/// long as the finger kept moving.
///
/// The property asserted is counted, not timed: **pictures promoted**. Zero is
/// a frozen pane.
#[test]
fn a_fling_the_pipeline_cannot_follow_still_puts_pictures_on_screen() {
    let ctx = egui::Context::default();

    // Control first, or the failure below is not about the fling. One viewport
    // per 28 frames is inside what this pipeline sustains, and there the pane
    // never runs dry at all.
    let mut sustained = PanRig::new(&ctx, 1.0 / 28.0, 3);
    sustained.run(620);
    assert_eq!(
        sustained.dry, 0,
        "control: a sustainable pan must never leave the pane without a picture \
         covering the viewport, or the fling numbers below have nothing to be \
         measured against",
    );
    assert!(
        sustained.promotions > 0,
        "control: a sustainable pan must promote pictures",
    );

    // The fling: one viewport per 12 frames, 2.3x the sustainable pan.
    let mut fling = PanRig::new(&ctx, 1.0 / 12.0, 3);
    fling.run(620);

    assert!(
        fling.dispatches > 0,
        "fixture: the fling must ask for rasters, or the freeze under test \
         cannot be the thing being measured",
    );
    assert!(
        fling.promotions > 0,
        "the pane promoted nothing across {} frames of a fling while spending \
         {} full-size rasters: every raster that came back replaced the one \
         still uploading, so the viewer kept the picture the fling started \
         with. {} pictures were thrown away mid-upload",
        fling.counted,
        fling.dispatches,
        fling.superseded,
    );
    // Non-triviality floor: the pipeline is 4 frames deep (1 raster + 3 upload),
    // so 620 frames admit ~150 promotions at best. A tenth of that is well below
    // what the brake achieves and far above the zero it replaced.
    assert!(
        fling.promotions >= 15,
        "the pane promoted only {} pictures in {} frames: the fling is still \
         starving the screen",
        fling.promotions,
        fling.counted,
    );
    // And it costs fewer rasters, not more: a dispatch that throws away an
    // upload is work spent for nothing.
    assert!(
        fling.dispatches < fling.counted / 2,
        "the fling dispatched {} rasters over {} frames — better than one every \
         other frame is the whole point of refusing the second supersede",
        fling.dispatches,
        fling.counted,
    );
}

/// The same fling on a device with no staging ring, where the upload is a frame
/// longer and the loop used to close that much sooner.
#[test]
fn the_fling_survives_a_three_frame_upload_too() {
    let ctx = egui::Context::default();
    let mut fling = PanRig::new(&ctx, 1.0 / 10.0, 3);
    fling.run(620);
    assert!(
        fling.promotions > 0,
        "nothing reached the screen in {} frames ({} rasters, {} thrown away)",
        fling.counted,
        fling.dispatches,
        fling.superseded,
    );
}

// ── Whose picture answers the coverage question ──────────────────────────

/// **The picture the viewer can see decides.** A hold is rasterised for a later
/// viewport than the one on screen, so on a pan that reverses it can be the one
/// short of margin while the picture actually being drawn still has plenty. The
/// gate used to dispatch on that — throwing away an upload the viewer had no
/// need of — because one texture answered every question it asked.
#[test]
fn a_hold_short_of_margin_does_not_dispatch_while_the_shown_picture_has_room() {
    let ctx = egui::Context::default();
    let texture = a_texture(&ctx);
    let here = viewport_at(0.0);

    // The pan went 0.2 degrees east and came back. The band is 0.25 degrees per
    // side and half of it is the trigger, so a picture rasterised 0.2 east is
    // short of margin on the west for the viewport we have returned to — and one
    // rasterised here is not.
    let mut cache = OverlayTextureCache::new();
    cache.show(data_for(&texture, &here));
    assert!(
        !cache.needs_rerender(TOKEN, ZOOM, T0, &here, &plan()),
        "fixture: a picture rasterised for this very viewport must satisfy the \
         gate, or nothing below is about the hold",
    );

    // Control: that picture, on screen, *is* out of margin here — so the
    // `false` below is the arm under test and not a geometry that never fires.
    let mut control = OverlayTextureCache::new();
    control.show(data_for(&texture, &viewport_at(0.2)));
    assert!(
        control.needs_rerender(TOKEN, ZOOM, T0, &here, &plan()),
        "control: a picture rasterised 0.2 degrees away must be short of margin \
         here, or the hold below is not short of margin either",
    );

    cache.hold(data_for(&texture, &viewport_at(0.2)), None);
    assert!(
        !cache.needs_rerender(TOKEN, ZOOM, T0, &here, &plan()),
        "the gate dispatched on the margin of a picture the viewer cannot see \
         yet while the one on screen still had room: the raster that comes back \
         replaces the upload in progress, so the pane shows neither",
    );
    assert!(
        cache.is_holding(),
        "judging coverage must not disturb the hold",
    );
}

/// With nothing held, the coverage arm is exactly the free function over the
/// picture on screen — unchanged, at every distance across the band and past it.
#[test]
fn nothing_held_leaves_the_coverage_answer_exactly_where_it_was() {
    let ctx = egui::Context::default();
    let texture = a_texture(&ctx);
    let here = viewport_at(0.0);
    let tex_bounds = plan().coverage(&here);

    let mut saw_true = false;
    let mut saw_false = false;
    for east in [0.0, 0.05, 0.1, 0.12, 0.13, 0.2, 0.25, 0.5] {
        let vp = viewport_at(east);
        let mut cache = OverlayTextureCache::new();
        cache.show(data_for(&texture, &here));
        let asked = cache.needs_rerender(TOKEN, ZOOM, T0, &vp, &plan());
        let expected = pan_exceeds_coverage(&tex_bounds, &vp);
        assert_eq!(
            asked, expected,
            "with nothing held, the gate at {east} degrees east answered \
             {asked} where `pan_exceeds_coverage` answers {expected}",
        );
        saw_true |= expected;
        saw_false |= !expected;
    }
    assert!(
        saw_true && saw_false,
        "the sweep must cross the trigger, or the equality above is one value \
         agreed on eight times",
    );
}

/// A first picture arriving over an empty pane keeps reading itself — there is
/// nothing else to read — and the brake is what stops that from becoming a
/// dispatch every frame against a pane that has never drawn anything.
#[test]
fn a_first_picture_over_an_empty_pane_asks_once_and_then_waits() {
    let ctx = egui::Context::default();
    let texture = a_texture(&ctx);
    let far = viewport_at(1.0);

    let mut cache = OverlayTextureCache::new();
    cache.hold(data_for(&texture, &viewport_at(0.0)), None);
    assert!(cache.current().is_none(), "fixture: nothing is on screen");

    assert!(
        cache.needs_rerender(TOKEN, ZOOM, T0, &far, &plan()),
        "the only picture this pane has does not reach the viewport, and there \
         is nothing on screen behind it: the first ask stands",
    );

    // That dispatch comes back and replaces the hold. From here on, asking again
    // would throw away the only picture the pane has ever had.
    cache.hold(data_for(&texture, &viewport_at(0.5)), None);
    assert!(
        !cache.needs_rerender(TOKEN, ZOOM, T0, &far, &plan()),
        "an empty pane asked for a second raster against a hold it had already \
         replaced once: every arrival restarts the upload, so the pane never \
         draws anything at all",
    );

    // And once something does reach the screen, the pane is free to ask again.
    let held = cache
        .take_held_if_delivered(|_| true)
        .expect("the hold is delivered");
    cache.show(held.data);
    assert!(
        cache.needs_rerender(TOKEN, ZOOM, T0, &far, &plan()),
        "the brake outlived the picture that set it",
    );
}

/// The brake is the coverage arm's alone. A hold that is stale in *content* is
/// still superseded, however many have been superseded before it — otherwise a
/// pane that flung once would stop taking new data.
#[test]
fn the_brake_never_holds_back_a_hold_that_is_stale_in_content() {
    let ctx = egui::Context::default();
    let texture = a_texture(&ctx);
    let here = viewport_at(0.0);

    let mut cache = OverlayTextureCache::new();
    cache.show(data_for(&texture, &here));
    cache.hold(data_for(&texture, &here), None);
    cache.hold(data_for(&texture, &here), None);

    // The brake is set and a hold is pending: coverage cannot dispatch.
    assert!(
        !cache.needs_rerender(TOKEN, ZOOM, T0, &viewport_at(0.2), &plan()),
        "fixture: the brake must be engaged, or the token below proves nothing",
    );
    assert!(
        cache.needs_rerender(TOKEN + 1, ZOOM, T0, &here, &plan()),
        "a hold rendered for content the pane has moved on from was kept \
         because an earlier hold had been superseded: new data would never \
         reach a pane that had flung once",
    );

    // Same for the size arm, which no hold may answer either.
    let denser = OverlayTexturePlan {
        width: W * 2,
        height: H * 2,
        ..plan()
    };
    assert!(
        cache.needs_rerender(TOKEN, ZOOM, T0, &here, &denser),
        "a hold at the old display density was kept by the coverage brake",
    );
}

/// Letting go of a hold lets go of the brake with it: nothing may be left
/// blocked behind a picture that is no longer coming.
#[test]
fn releasing_a_hold_releases_the_brake() {
    let ctx = egui::Context::default();
    let texture = a_texture(&ctx);
    let here = viewport_at(0.0);
    let away = viewport_at(0.2);

    let braked = |cache: &mut OverlayTextureCache| {
        cache.show(data_for(&texture, &here));
        cache.hold(data_for(&texture, &here), None);
        cache.hold(data_for(&texture, &here), None);
        assert!(
            !cache.needs_rerender(TOKEN, ZOOM, T0, &away, &plan()),
            "fixture: the brake must be engaged before it can be released",
        );
    };
    let asks_again = |cache: &mut OverlayTextureCache, how: &str| {
        assert!(
            cache.needs_rerender(TOKEN, ZOOM, T0, &away, &plan()),
            "the brake survived {how}, so this pane will never rebuild for a \
             pan again",
        );
    };

    // The renderer was rebuilt and every arriving picture was let go of.
    let mut released = OverlayTextureCache::new();
    braked(&mut released);
    released.release_hold();
    asks_again(&mut released, "`release_hold`");

    // The overlay was switched off and back on.
    let mut cleared = OverlayTextureCache::new();
    braked(&mut cleared);
    cleared.clear();
    cleared.show(data_for(&texture, &here));
    asks_again(&mut cleared, "`clear`");

    // And the ordinary end of a hold: its last band landed and it went up.
    let mut promoted = OverlayTextureCache::new();
    braked(&mut promoted);
    let held = promoted
        .take_held_if_delivered(|_| true)
        .expect("the hold is delivered");
    promoted.show(held.data);
    asks_again(&mut promoted, "the promotion that ended it");
}
