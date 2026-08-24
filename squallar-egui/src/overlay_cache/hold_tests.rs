//! A pane keeps its picture until the next one is whole.

use super::*;

/// A texture id and a handle for it, from a bare context.
fn texture(ctx: &egui::Context, name: &str) -> egui::TextureHandle {
    ctx.load_texture(
        name,
        egui::ColorImage::filled([1, 1], egui::Color32::RED),
        egui::TextureOptions::NEAREST,
    )
}

fn data(texture: egui::TextureHandle, max_lat: f64) -> OverlayTextureData {
    OverlayTextureData {
        texture,
        placed: PlacedRaster::of(GeoBounds {
            min_lat: 34.0,
            max_lat,
            min_lon: -98.0,
            max_lon: -96.0,
        }),
        data_generation: 0,
        render_zoom: 0,
        width: 1,
        height: 1,
        radar_meta: None,
        hit_map: None,
    }
}

/// Nothing delivers anything, which is the state of a hold on the frame it is
/// staged and of every hold whose renderer has been rebuilt.
fn nothing(_: egui::TextureId) -> bool {
    false
}

/// The whole point: the previous picture stays on screen while the next arrives.
#[test]
fn the_picture_on_screen_is_the_previous_one_until_the_next_is_whole() {
    let ctx = egui::Context::default();
    let mut cache = OverlayTextureCache::new();
    cache.show(data(texture(&ctx, "first"), 36.0));

    let arriving = texture(&ctx, "second");
    cache.hold(data(arriving.clone(), 40.0), None);

    let on_screen = cache.current().expect("the pane still has a picture");
    assert_eq!(
        on_screen.placed.geo.max_lat, 36.0,
        "the new raster's bounds landed while its pixels were still crossing",
    );
    assert!(cache.is_holding());

    let id = arriving.id();
    assert!(cache.take_held_if_delivered(nothing).is_none());
    let held = cache
        .take_held_if_delivered(|asked| asked == id)
        .expect("a delivered raster is handed over");
    cache.show(held.data);
    assert_eq!(cache.current().expect("swapped").placed.geo.max_lat, 40.0);
    assert!(!cache.is_holding());
}

/// A first raster has no predecessor, and the pane draws no radar until it is
/// whole.
#[test]
fn a_pane_with_no_previous_picture_shows_none_while_the_first_arrives() {
    let ctx = egui::Context::default();
    let mut cache = OverlayTextureCache::new();
    cache.hold(data(texture(&ctx, "first"), 36.0), None);
    assert!(cache.current().is_none());
    assert!(cache.is_holding());
}

/// A newer render replaces a hold rather than queueing behind it.
#[test]
fn a_newer_raster_supersedes_one_still_arriving() {
    let ctx = egui::Context::default();
    let mut cache = OverlayTextureCache::new();
    cache.show(data(texture(&ctx, "first"), 36.0));

    let superseded = texture(&ctx, "second");
    cache.hold(data(superseded.clone(), 40.0), None);
    let newest = texture(&ctx, "third");
    cache.hold(data(newest.clone(), 44.0), None);

    let stale = superseded.id();
    assert!(cache.take_held_if_delivered(|id| id == stale).is_none());
    assert_eq!(
        cache.current().expect("still the first").placed.geo.max_lat,
        36.0,
    );

    let id = newest.id();
    let held = cache
        .take_held_if_delivered(|asked| asked == id)
        .expect("the newest raster is the one that lands");
    cache.show(held.data);
    assert_eq!(cache.current().expect("swapped").placed.geo.max_lat, 44.0);
}

/// A superseding hold gives the replaced picture's allocation back.
#[test]
fn a_superseding_hold_releases_the_picture_it_replaces() {
    let ctx = egui::Context::default();
    let live = || ctx.tex_manager().read().num_allocated();
    let mut cache = OverlayTextureCache::new();

    cache.show(data(texture(&ctx, "first"), 36.0));
    let _ = ctx.end_pass();
    let shown_and_arriving = {
        cache.hold(data(texture(&ctx, "second"), 40.0), None);
        let _ = ctx.end_pass();
        live()
    };

    for burst in 0..3 {
        cache.hold(data(texture(&ctx, &format!("burst-{burst}")), 44.0), None);
        let _ = ctx.end_pass();
        assert_eq!(
            live(),
            shown_and_arriving,
            "supersede {burst} left the replaced held texture allocated: a \
             burst of mid-gesture renders accumulates a full-size raster per \
             result instead of pinning two",
        );
    }

    assert_eq!(
        cache.current().expect("still showing").placed.geo.max_lat,
        36.0,
    );
}

/// Clearing a pane clears what it was about to show as well.
#[test]
fn clearing_a_cache_also_drops_the_picture_it_was_about_to_show() {
    let ctx = egui::Context::default();
    let mut cache = OverlayTextureCache::new();
    cache.show(data(texture(&ctx, "first"), 36.0));
    let arriving = texture(&ctx, "second");
    cache.hold(data(arriving.clone(), 40.0), None);

    cache.clear();
    assert!(cache.current().is_none());
    assert!(!cache.is_holding());

    let id = arriving.id();
    assert!(
        cache.take_held_if_delivered(|asked| asked == id).is_none(),
        "a cleared pane put the raster back up when its last band landed",
    );
}

/// A released hold is gone without ever being shown.
#[test]
fn releasing_a_hold_drops_it_without_showing_it() {
    let ctx = egui::Context::default();
    let mut cache = OverlayTextureCache::new();
    cache.show(data(texture(&ctx, "first"), 36.0));
    let arriving = texture(&ctx, "second");
    cache.hold(data(arriving.clone(), 40.0), None);

    cache.release_hold();
    assert!(!cache.is_holding());
    assert_eq!(
        cache
            .current()
            .expect("releasing a hold is not clearing the pane")
            .placed
            .geo
            .max_lat,
        36.0,
    );
}

/// Showing a picture drops a hold rather than letting it swap in later.
#[test]
fn showing_a_picture_drops_a_hold_that_was_still_arriving() {
    let ctx = egui::Context::default();
    let mut cache = OverlayTextureCache::new();
    let arriving = texture(&ctx, "arriving");
    cache.hold(data(arriving.clone(), 40.0), None);
    cache.show(data(texture(&ctx, "restored"), 36.0));

    assert!(!cache.is_holding());
    let id = arriving.id();
    assert!(cache.take_held_if_delivered(|asked| asked == id).is_none());
    assert_eq!(cache.current().expect("shown").placed.geo.max_lat, 36.0);
}

/// An undelivered hold leaves the cache untouched, so the question can be asked
/// again.
#[test]
fn asking_about_a_hold_that_has_not_landed_changes_nothing() {
    let ctx = egui::Context::default();
    let mut cache = OverlayTextureCache::new();
    cache.show(data(texture(&ctx, "first"), 36.0));
    let arriving = texture(&ctx, "second");
    cache.hold(data(arriving.clone(), 40.0), None);

    for _ in 0..10 {
        assert!(cache.take_held_if_delivered(nothing).is_none());
        assert!(cache.is_holding());
        assert_eq!(cache.current().expect("kept").placed.geo.max_lat, 36.0);
    }

    let id = arriving.id();
    assert!(cache.take_held_if_delivered(|asked| asked == id).is_some());
}
