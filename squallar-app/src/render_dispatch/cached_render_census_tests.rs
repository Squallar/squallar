//! The `cached renders` census family: the rasters the **panes** hold for
//! restore, which until 2026-09-07 no family named.
//!
//! On the empty steady scene — every overlay off, no gesture, no loop — one
//! pane held 216,796,176 B of `Color32` and 5,281,920 B of hover, 211.8 MiB,
//! and `publish_heap_census` did not mention a byte of it. That was 87 % of
//! the whole unaccounted heap on a scene where the user had enabled nothing.
//!
//! Two properties, and the second is the one that makes the first believable:
//! the figure de-duplicates panes that share a buffer, **and** it rises when
//! they do not. A counter that always answered "once" would pass the dedup
//! test and be useless.

use super::*;

/// Small on purpose. The family prices `pixels * 4`, so the side is free to be
/// whatever makes the arithmetic unmissable; the shipped 7362 would cost the
/// test 206 MiB an image to say the same thing.
const SIDE: usize = 64;

/// What one of these rasters costs as `Color32`.
const IMAGE_BYTES: u64 = (SIDE * SIDE * std::mem::size_of::<egui::Color32>()) as u64;

fn raster() -> Arc<egui::ColorImage> {
    Arc::new(egui::ColorImage::filled(
        [SIDE, SIDE],
        egui::Color32::TRANSPARENT,
    ))
}

fn hover() -> Arc<squallar_radar::hover::HoverSource> {
    Arc::new(squallar_radar::hover::HoverSource::empty())
}

fn cached(
    image: Arc<egui::ColorImage>,
    hover: Arc<squallar_radar::hover::HoverSource>,
) -> CachedPaneRender {
    CachedPaneRender {
        image,
        max_range_km: 230.0,
        hover,
        product: squallar_radar::types::RadarProduct::Reflectivity,
        elevation: 0.5,
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
    }
}

fn holding(panes: Vec<Option<CachedPaneRender>>) -> RenderDispatcher {
    let mut dispatcher = RenderDispatcher::new();
    dispatcher.ensure_pane_count(panes.len());
    for (idx, cached) in panes.into_iter().enumerate() {
        dispatcher.pane_render[idx].cached_render = cached;
    }
    dispatcher
}

/// A dispatcher whose panes hold nothing prices nothing — a real zero, and the
/// state the family reads in before the first render lands.
#[test]
fn panes_holding_no_cached_render_price_nothing() {
    assert_eq!(holding(vec![None, None]).cached_render_bytes(), 0);
}

/// **One pane, one raster**: the pixels and the hover field, and the hover term
/// is read off the fixture rather than pinned to a constant, so this says
/// "both terms are present" without pinning `HoverSource::empty`'s size.
#[test]
fn one_pane_is_priced_at_its_pixels_and_its_hover() {
    let hover = hover();
    let expected = IMAGE_BYTES + hover.resident_bytes() as u64;
    let dispatcher = holding(vec![Some(cached(raster(), hover))]);
    assert_eq!(dispatcher.cached_render_bytes(), expected);
}

/// **Two panes showing the SAME raster hold one buffer and are priced once.**
///
/// This is the defect `render cache` still has and this family was born
/// without: `apply_render_to_pane` gives every pane a clone of the reply's
/// `Arc`, so pricing per pane would report two buffers where the allocator
/// granted one.
#[test]
fn two_panes_sharing_one_raster_are_priced_once() {
    let image = raster();
    let hover = hover();
    let one = IMAGE_BYTES + hover.resident_bytes() as u64;
    let dispatcher = holding(vec![
        Some(cached(Arc::clone(&image), Arc::clone(&hover))),
        Some(cached(image, hover)),
    ]);
    assert_eq!(
        dispatcher.cached_render_bytes(),
        one,
        "two panes sharing one buffer were priced as two"
    );
}

/// **The case where it must dominate**: two panes on two different rasters are
/// two buffers, and the figure doubles.
///
/// Without this the dedup above is satisfied by a counter that answers "one
/// raster" whatever it is shown, which would hide exactly the multi-pane scene
/// the campaign cares most about. A null from an instrument never shown to be
/// sensitive is not a null.
#[test]
fn two_panes_holding_distinct_rasters_are_priced_twice() {
    let first = hover();
    let second = hover();
    let expected = 2 * IMAGE_BYTES + first.resident_bytes() as u64 + second.resident_bytes() as u64;
    let dispatcher = holding(vec![
        Some(cached(raster(), first)),
        Some(cached(raster(), second)),
    ]);
    assert_eq!(
        dispatcher.cached_render_bytes(),
        expected,
        "two distinct buffers were collapsed into one"
    );
}

/// The two `Arc`s are asked their own questions: panes may share the pixels
/// and not the hover, and the figure must carry both hovers when they do.
#[test]
fn a_shared_raster_with_distinct_hovers_prices_both_hovers() {
    let image = raster();
    let first = hover();
    let second = hover();
    let expected = IMAGE_BYTES + first.resident_bytes() as u64 + second.resident_bytes() as u64;
    let dispatcher = holding(vec![
        Some(cached(Arc::clone(&image), first)),
        Some(cached(image, second)),
    ]);
    assert_eq!(dispatcher.cached_render_bytes(), expected);
}
