//! **The resize arm, and what a raster already in flight does to it.**
//!
//! [`RerenderReason::PlanResized`] fires when the picture a pane holds is no
//! longer the size that pane would ask for: a display-density change, a window
//! moved to another monitor, a browser zoom, the pane resized — or the
//! degradation ladder taking or giving back its overlay-oversampling rung,
//! which changes the planned texel count and nothing else
//! (`squallar_device_profile::constants::OVERLAY_OVERSAMPLE_PERCENTS`).
//!
//! These fixtures exist to keep one claim honest. The arm carries **no** brake
//! of its own — unlike the content and coverage arms, it never consults
//! `hold_superseded` — and it does not need one, because the door above it
//! does the refusing: `RendersInFlight::admits` is
//! `!holds(slot) && out.len() < limit` and a whole-picture layer has exactly
//! one destination, so a second raster for a resize cannot go out while the
//! first is flying, at any budget. What a resize under a live raster really
//! cost was one line further on — the floor strip's completeness latch, pinned
//! by `crate::ui_map::floor_strip_cache_tests::
//! an_overlay_raster_in_flight_does_not_hold_the_strip_open`. Everything here
//! is the reading that fixture's doc leans on and cannot take for itself.

use super::*;

fn texture(ctx: &egui::Context, name: &str) -> egui::TextureHandle {
    ctx.load_texture(
        name,
        egui::ColorImage::filled([1, 1], egui::Color32::RED),
        egui::TextureOptions::NEAREST,
    )
}

const TOKEN: u64 = 7;

/// A whole picture for [`TOKEN`], `side` texels square, at the zoom and on the
/// ground every other arm below is satisfied by — so the only arm that can
/// speak is the one this module is about.
fn data(ctx: &egui::Context, name: &str, side: u32) -> OverlayTextureData {
    OverlayTextureData {
        crop: None,
        texture: texture(ctx, name),
        placed: PlacedRaster::of(GeoBounds {
            min_lat: 30.0,
            max_lat: 40.0,
            min_lon: -100.0,
            max_lon: -90.0,
        }),
        data_generation: TOKEN,
        render_zoom: 0,
        width: side,
        height: side,
        radar_meta: None,
        hit_map: None,
    }
}

/// The plan the pane would ask for, `side` texels square.
fn plan(side: u32) -> OverlayTexturePlan {
    OverlayTexturePlan {
        width: side,
        height: side,
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

fn asks(cache: &mut OverlayTextureCache, side: u32) -> bool {
    cache.needs_rerender(TOKEN, 0.0, ZoomDrive::AT_REST, &viewport(), &plan(side))
}

/// A dispatch the way the app spends one: the mark, and the reason the mark
/// consumes.
fn dispatch(cache: &mut OverlayTextureCache) {
    cache.renders.record(RenderTicket::whole(TOKEN, viewport()));
}

/// **The arm is named, and it is this one.** Without this reading every
/// fixture below would be satisfied by the content arm firing for a token
/// mismatch nobody noticed, and the module would be measuring something else
/// under the resize arm's name.
#[test]
fn a_picture_of_the_wrong_size_names_the_resize_arm() {
    let ctx = egui::Context::default();
    let mut cache = OverlayTextureCache::new();
    cache.show(data(&ctx, "shown", 1));

    assert!(
        asks(&mut cache, 2),
        "a picture that is no longer the size the pane would ask for owes a \
         raster; nothing else in this gate can notice a density or pane \
         change, so without this the pane keeps a half-density texture for as \
         long as it stands still",
    );
    assert_eq!(
        cache.renders.armed(),
        Some(RerenderReason::PlanResized),
        "the raster was armed under another arm's name. The ledger this tree \
         prices rebuilds from reads these, so an arm that fires under a \
         neighbour's label makes every figure taken off it untrue",
    );
    assert!(
        !asks(&mut cache, 1),
        "the same picture at the size the pane does ask for still owed a \
         raster, so the fixture above proves nothing about the size term",
    );
}

/// **A resize cannot spend a second raster while the first is flying — at
/// every budget.**
///
/// This is the property that lets the resize arm carry no brake of its own.
/// The bound is `RenderSlot`'s livelock guard, not the device figure, so the
/// sweep is over budgets rather than at one of them: `concurrent_renders` is
/// 1 on the web bracket, 3 on mobile and 6 on desktop, and a one-destination
/// cache must refuse at all three.
#[test]
fn a_resize_cannot_spend_a_second_raster_at_any_budget() {
    for limit in [1usize, 3, 6] {
        let ctx = egui::Context::default();
        let mut cache = OverlayTextureCache::new();
        cache.show(data(&ctx, "shown", 1));

        assert!(
            asks(&mut cache, 2),
            "limit {limit}: the resize arm is quiet"
        );
        assert!(
            cache.renders.admits(RenderSlot::WHOLE, limit),
            "limit {limit}: a cache with nothing out refused the first raster",
        );
        dispatch(&mut cache);

        assert!(
            asks(&mut cache, 2),
            "limit {limit}: the resize arm went quiet while a raster was out. \
             It is allowed to — but then the fixture below is measuring a \
             silent arm rather than a refused dispatch, and the strip-latch \
             gate this module's doc points at is the only thing left holding \
             the claim",
        );
        assert!(
            !cache.renders.admits(RenderSlot::WHOLE, limit),
            "limit {limit}: a second raster for the one destination this layer \
             has was admitted. Two rasters for one destination cannot both \
             reach the screen — `hold` replaces rather than queues — so the \
             second throws the first's upload away and promotes nothing",
        );
    }
}

/// **The picture that answers the resize ends it**, and the arrival is what
/// carries the pane there. Held rather than shown, because a hold is what an
/// arrival makes and the gate takes the held shape over the shown one — so
/// this is the state the very next frame after an arrival sees.
#[test]
fn a_held_picture_at_the_new_size_ends_the_resize_arm() {
    let ctx = egui::Context::default();
    let mut cache = OverlayTextureCache::new();
    cache.show(data(&ctx, "shown", 1));
    assert!(asks(&mut cache, 2), "the resize arm never fired");
    dispatch(&mut cache);

    // The arrival: the app retires the mark and hands the picture to the
    // cache, both in `PumpPhase::Apply` and both before the draw pass that
    // asks the question again.
    assert!(
        cache
            .renders
            .retire(&RenderTicket::whole(TOKEN, viewport())),
        "the cache was not waiting for the dispatch it had just made",
    );
    cache.hold(data(&ctx, "arrived", 2), None);

    assert!(
        !asks(&mut cache, 2),
        "a pane holding a picture of exactly the size it asks for still owed \
         a raster, which is a dispatch loop with no exit",
    );
    assert!(
        cache.renders.admits(RenderSlot::WHOLE, 1),
        "the mark survived the arrival that retired it",
    );
}
