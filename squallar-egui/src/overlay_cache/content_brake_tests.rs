//! **The content arm's brake: it may only ever act on a pane that has proved
//! it cannot land what it already asked for.**
//!
//! The arm returns `true` for a token it has no picture of — that is the whole
//! job — *except* while this cache is both waiting on an upload and has
//! already thrown one away. Everything here is one of the two halves of that:
//! the cases that must be untouched (which is most of them, and is what keeps
//! the GLM per-frame contract and the 1 fps end of the Speed slider exactly as
//! they were), and the one case that must brake.
//!
//! Every fixture below is written to fail against **both** tampers of the new
//! predicate — `return true` (the brake deleted) and `return false` (the brake
//! made unconditional) — because a fixture that only catches one of them is
//! satisfied by a constant and is not measuring the predicate at all.

use super::*;

fn texture(ctx: &egui::Context, name: &str) -> egui::TextureHandle {
    ctx.load_texture(
        name,
        egui::ColorImage::filled([1, 1], egui::Color32::RED),
        egui::TextureOptions::NEAREST,
    )
}

/// A whole picture for `generation`, sized and zoomed to match [`plan`] so
/// that no arm below the content one can fire and answer for it.
fn data(ctx: &egui::Context, name: &str, generation: u64) -> OverlayTextureData {
    OverlayTextureData {
        texture: texture(ctx, name),
        placed: PlacedRaster::of(GeoBounds {
            min_lat: 30.0,
            max_lat: 40.0,
            min_lon: -100.0,
            max_lon: -90.0,
        }),
        data_generation: generation,
        render_zoom: 0,
        width: 1,
        height: 1,
        radar_meta: None,
        hit_map: None,
    }
}

fn plan() -> OverlayTexturePlan {
    OverlayTexturePlan {
        width: 1,
        height: 1,
        overdraw: 0.0,
        pixels_per_point: 1.0,
        pane_px: [0, 0],
    }
}

/// Well inside the picture's ground, so the coverage arm has nothing to say.
fn viewport() -> GeoBounds {
    GeoBounds {
        min_lat: 34.0,
        max_lat: 36.0,
        min_lon: -97.0,
        max_lon: -95.0,
    }
}

/// The pane's whole question, at a still zoom the pictures were rendered at,
/// with no gesture driving it — the content arm is what these tests ask about.
fn asks(cache: &mut OverlayTextureCache, token: u64) -> bool {
    cache.needs_rerender(token, 0.0, ZoomDrive::AT_REST, &viewport(), &plan())
}

/// Land whatever is being held, the way a delivered upload does.
fn lands(cache: &mut OverlayTextureCache) {
    let held = cache
        .take_held_if_delivered(|_| true)
        .expect("a hold was in flight");
    cache.show(held.data);
}

/// **A pipeline that keeps up is never braked** — the 1 fps end of the Speed
/// slider, and every synchronous test pipeline including the GLM loop suite's.
///
/// Ten instants, each dispatched and each landing before the next is asked
/// for. Ten asks, ten dispatches: the brake cannot engage because nothing is
/// ever in flight when the question is put. Reddens on a `return false`
/// tamper at the first ask.
#[test]
fn a_pipeline_that_keeps_up_is_never_braked() {
    let ctx = egui::Context::default();
    let mut cache = OverlayTextureCache::new();
    cache.show(data(&ctx, "boot", 0));

    for token in 1..=10u64 {
        assert!(
            asks(&mut cache, token),
            "instant {token} was refused a picture by a pane with nothing in \
             flight. The brake is a statement about delivery; a pane that has \
             landed everything it asked for has proved the opposite",
        );
        cache.hold(data(&ctx, "landed", token), None);
        lands(&mut cache);
    }
}

/// **The brake itself.** A pane that threw away one upload and is still
/// waiting on its replacement stops asking, and starts again the moment a
/// picture lands.
///
/// Reddens on a `return true` tamper at the braked ask, and on a
/// `return false` tamper at the first ask before it.
#[test]
fn a_pane_that_discarded_an_upload_stops_asking_until_one_lands() {
    let ctx = egui::Context::default();
    let mut cache = OverlayTextureCache::new();
    cache.show(data(&ctx, "boot", 0));

    // One raster in flight. Nothing has been thrown away yet, so the next
    // instant is still dispatched — this is the ask that becomes the discard.
    cache.hold(data(&ctx, "first", 1), None);
    assert!(
        asks(&mut cache, 2),
        "a pane holding its FIRST picture must still dispatch: one hold in \
         flight is a pipeline keeping up, not one falling behind",
    );

    // That dispatch lands on a cache still holding, which is the discard.
    cache.hold(data(&ctx, "second", 2), None);
    assert!(
        !asks(&mut cache, 3),
        "the pane threw away an upload and is still waiting on its \
         replacement, and was asked to spend a third raster on a fourth \
         instant. This is the fling, on the content arm: spend, discard, \
         promote nothing",
    );

    // The replacement lands. Both clauses clear together, and the pane is
    // free to follow the clock again.
    lands(&mut cache);
    assert!(
        asks(&mut cache, 4),
        "a picture landed and cleared both clauses, but the pane stayed \
         braked. The brake would be permanent, and the layer would never \
         raster again",
    );
}

/// **A paused pane gets the instant it is parked on**, however many uploads
/// were discarded getting there. No hold in flight means no brake, which is
/// what makes a scrub that ends anywhere land its own picture.
#[test]
fn a_paused_clock_still_gets_its_own_picture() {
    let ctx = egui::Context::default();
    let mut cache = OverlayTextureCache::new();
    cache.show(data(&ctx, "boot", 0));

    // Arrive in the braked state, exactly as a scrub through a busy pipeline
    // would, then let the pipeline drain the way a released scrubber does.
    cache.hold(data(&ctx, "a", 1), None);
    cache.hold(data(&ctx, "b", 2), None);
    assert!(!asks(&mut cache, 3), "premise: the pane is braked");
    lands(&mut cache);

    assert!(
        asks(&mut cache, 99),
        "a pane parked on one instant, with nothing in flight, was refused \
         that instant's picture",
    );
}

/// **A live pane that re-tokenizes once is untouched** — a theme flip, a
/// filter change, an arriving data bump. One token move against an empty
/// flight is the common case in the tree and must not be braked.
#[test]
fn a_live_pane_that_retokenizes_once_is_unaffected() {
    let ctx = egui::Context::default();
    let mut cache = OverlayTextureCache::new();
    cache.show(data(&ctx, "boot", 7));

    assert!(
        asks(&mut cache, 8),
        "a live pane whose content signature moved once, holding nothing, was \
         refused a raster",
    );
}

/// **A pane with no picture at all always dispatches.** The brake reads a
/// picture's generation, and there is none to read; the early return above it
/// answers, and must go on answering.
#[test]
fn a_pane_holding_nothing_always_dispatches() {
    let mut cache = OverlayTextureCache::new();
    assert!(
        asks(&mut cache, 1),
        "a cache with neither a current picture nor a hold refused to \
         dispatch, so the layer would never draw at all",
    );
}
