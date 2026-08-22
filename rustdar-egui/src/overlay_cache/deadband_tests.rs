//! The deadband on the coverage trigger.
//!
//! Everything here is asked at **zero overdraw** — the pane wider than the
//! adapter's limit, where [`OverlayTexturePlan::overdraw`] is `0.0` and the
//! texture's ground is exactly the viewport it was rasterised for. That is the
//! case a deadband priced in band cannot reach, and the case that had no
//! deadband at all: four strict inequalities against that viewport, tripped by
//! any nonzero motion whatsoever.

use super::*;

/// The content token every question here is asked with, and the one every
/// fixture texture carries — so a `true` answer is never a token mismatch.
const TOKEN: u64 = 4242;

/// The zoom everything is rasterised at and asked at, so the settle arm and the
/// mid-gesture band arm can never be what answers.
const ZOOM: f64 = 7.0;

/// A wall-clock origin far from zero, in the unit the clock parameter uses.
const T0: f64 = 100.0;

/// A pane at the WebGL2 floor: 2048 texels on a side is what a texture gets once
/// the viewport alone fills the limit, and it is where the overdraw goes to zero.
const SIDE: u32 = 2048;

/// One degree of viewport on each axis, so a texel is `1.0 / SIDE` degrees and
/// the arithmetic below is readable.
fn viewport_at(east: f64) -> GeoBounds {
    GeoBounds {
        min_lat: 34.5,
        max_lat: 35.5,
        min_lon: -97.5 + east,
        max_lon: -96.5 + east,
    }
}

/// One texel of the fixture picture, in degrees of longitude — the unit the
/// deadband is priced in.
const TEXEL: f64 = 1.0 / SIDE as f64;

fn plan(overdraw: f32) -> OverlayTexturePlan {
    OverlayTexturePlan {
        width: SIDE,
        height: SIDE,
        overdraw,
        pixels_per_point: 1.0,
    }
}

fn a_texture(ctx: &egui::Context) -> egui::TextureHandle {
    ctx.load_texture(
        "fixture",
        egui::ColorImage::filled([1, 1], egui::Color32::RED),
        egui::TextureOptions::NEAREST,
    )
}

/// A picture rasterised for `vp`, satisfying every arm of the gate but coverage.
fn data_for(texture: &egui::TextureHandle, vp: &GeoBounds, overdraw: f32) -> OverlayTextureData {
    OverlayTextureData {
        texture: texture.clone(),
        placed: PlacedRaster::of(plan(overdraw).coverage(vp)),
        data_generation: TOKEN,
        render_zoom: current_quantized_zoom(ZOOM),
        width: SIDE,
        height: SIDE,
        radar_meta: None,
        hit_map: None,
    }
}

/// A pane showing one picture, asked the gate once per frame for a viewport
/// that moves by `offset(frame)` — nothing is dispatched, so the picture on
/// screen stays the one the offsets are measured from. Counts the `true`s.
fn asks_over(overdraw: f32, frames: u32, offset: impl Fn(u32) -> f64) -> u32 {
    let ctx = egui::Context::default();
    let texture = a_texture(&ctx);
    let mut cache = OverlayTextureCache::new();
    cache.show(data_for(&texture, &viewport_at(0.0), overdraw));

    let mut asked = 0;
    for f in 0..frames {
        let now = T0 + f as f64 / 60.0;
        if cache.needs_rerender(TOKEN, ZOOM, now, &viewport_at(offset(f)), &plan(overdraw)) {
            asked += 1;
        }
    }
    asked
}

/// **The regressor.** A viewport that is not still — float noise in the
/// projector, a trackpad resting under a finger — but never moves far enough to
/// put any feature in a different texel. Nothing about the picture on screen
/// would change, and at zero overdraw this dispatched a full-size raster on
/// every frame of it.
///
/// The property is counted, not timed: **rasters asked for**. The control below
/// is what stops a rule that answers `false` to everything from passing.
#[test]
fn a_sub_texel_wobble_at_zero_overdraw_asks_for_nothing() {
    // Well under a texel, and both signs: the west edge and the east edge are
    // separate inequalities and a deadband on one of them is not a deadband.
    let wobble = |f: u32| match f % 4 {
        0 => 0.0,
        1 => 0.4 * TEXEL,
        2 => 1e-9,
        _ => -0.4 * TEXEL,
    };
    assert_eq!(
        asks_over(0.0, 600, wobble),
        0,
        "a viewport wobbling by less than a texel asked for full-size rasters: \
         at zero overdraw the coverage trigger is the exact viewport the texture \
         was rasterised for, so any motion at all trips it",
    );

    // Non-triviality floor: the same rig, with the same shape of motion sized
    // past the deadband, must ask for one on every frame it moves.
    let past = |f: u32| match f % 4 {
        0 => 0.0,
        1 => 1.6 * TEXEL,
        2 => 3.0 * TEXEL,
        _ => -1.6 * TEXEL,
    };
    assert_eq!(
        asks_over(0.0, 600, past),
        450,
        "control: motion past the deadband must still be asked about on every \
         frame it happens, or the count above is a rule that says `false` to \
         everything",
    );
}

/// The deadband is a texel of the picture being judged, on each edge — not a
/// fraction of a band that is zero here, and not a fixed number of degrees.
#[test]
fn the_deadband_is_one_texel_on_every_edge() {
    for (name, east) in [("west", -1.0), ("east", 1.0)] {
        assert_eq!(
            asks_over(0.0, 1, |_| east * 0.9 * TEXEL),
            0,
            "{name}: nine tenths of a texel is inside the deadband",
        );
        assert_eq!(
            asks_over(0.0, 1, |_| east * 1.1 * TEXEL),
            1,
            "{name}: eleven tenths of a texel is past it, and the pane really is \
             off its own texture there — at zero overdraw there is no band to be \
             inside of",
        );
    }
}

/// A band that exists was never the case this is for. At `OVERDRAW_FRACTION` the
/// margin already swallows a wobble hundreds of times larger than the deadband,
/// and the deadband is a rounding error against the cover it leaves — the
/// ground the pane draws on while a replacement rasterises.
#[test]
fn a_real_band_swallows_the_wobble_without_the_deadband_being_what_did_it() {
    assert_eq!(
        asks_over(OVERDRAW_FRACTION, 600, |f| (f % 7) as f64 * 1e-6),
        0,
        "fixture: at a real band a micro-degree wobble was never dispatching",
    );

    // The deadband cannot eat the cover: one texel against half of a 0.25 band.
    let band = 1.0 * OVERDRAW_FRACTION as f64;
    let cover = band * (1.0 - PAN_REBUILD_THRESHOLD as f64);
    assert!(
        TEXEL < cover / 100.0,
        "a texel ({TEXEL}) is not negligible against the cover ({cover}), so the \
         deadband is delaying the dispatch by ground the pane needed",
    );
}

// ── The deadband cannot stall a pane ─────────────────────────────────────

/// The dispatch loop of `coverage_dispatch_tests::PanRig`, at zero overdraw:
/// promote a delivered hold, take the raster that came back, ask the gate,
/// dispatch, then move this frame's upload bands. The clock is the frame index.
struct ZeroBandRig {
    cache: OverlayTextureCache,
    texture: egui::TextureHandle,
    /// Degrees of longitude per frame. One viewport is one degree.
    step: f64,
    in_flight: Option<(u32, GeoBounds)>,
    bands_left: u32,
    dispatches: u32,
    promotions: u32,
}

impl ZeroBandRig {
    fn new(ctx: &egui::Context, step: f64) -> Self {
        let texture = a_texture(ctx);
        let mut cache = OverlayTextureCache::new();
        cache.show(data_for(&texture, &viewport_at(0.0), 0.0));
        Self {
            cache,
            texture,
            step,
            in_flight: None,
            bands_left: 0,
            dispatches: 0,
            promotions: 0,
        }
    }

    fn run(&mut self, frames: u32) {
        for f in 0..frames {
            let vp = viewport_at(f as f64 * self.step);

            if self.bands_left == 0
                && let Some(held) = self.cache.take_held_if_delivered(|_| true)
            {
                self.cache.show(held.data);
                self.promotions += 1;
            }

            if let Some((arrives, dispatched_for)) = self.in_flight
                && f >= arrives
            {
                self.cache
                    .hold(data_for(&self.texture, &dispatched_for, 0.0), None);
                self.bands_left = 3;
                self.in_flight = None;
            }

            let now = T0 + f as f64 / 60.0;
            if self.cache.needs_rerender(TOKEN, ZOOM, now, &vp, &plan(0.0))
                && self.in_flight.is_none()
            {
                self.in_flight = Some((f + 1, vp));
                self.dispatches += 1;
            }

            if self.cache.is_holding() {
                self.bands_left = self.bands_left.saturating_sub(1);
            }
        }
    }
}

/// **The freeze the third arm of the coverage rule exists for is still not
/// reachable, and the deadband has not opened a second way in.** A deadband can
/// only withhold dispatches, so the loop that spent a raster per frame and
/// promoted nothing cannot be reached through it; what has to be shown is the
/// other direction — that a pane really moving is not held back from asking.
///
/// Counted, not timed: **pictures promoted**. Zero is a frozen pane.
#[test]
fn the_deadband_cannot_stall_a_pane_that_is_really_moving() {
    let ctx = egui::Context::default();

    // A pan of exactly the deadband per frame — the slowest motion that is not
    // inside it, and so the hardest case for a rule that paces on ground.
    let mut crawl = ZeroBandRig::new(&ctx, 1.1 * TEXEL);
    crawl.run(600);
    assert!(
        crawl.dispatches > 0 && crawl.promotions > 0,
        "a pane crawling past its own deadband asked for {} rasters and put {} \
         on screen: the deadband became a floor on how slowly a pane may move \
         and still be drawn",
        crawl.dispatches,
        crawl.promotions,
    );

    // And the fling, at the speed the third arm was written against.
    let mut fling = ZeroBandRig::new(&ctx, 1.0 / 12.0);
    fling.run(600);
    assert!(
        fling.dispatches > 0,
        "fixture: the fling must ask for rasters, or the promotions below are \
         not about the freeze",
    );
    assert!(
        fling.promotions > 0,
        "the pane promoted nothing across 600 frames of a fling at zero \
         overdraw while spending {} rasters",
        fling.dispatches,
    );
}
