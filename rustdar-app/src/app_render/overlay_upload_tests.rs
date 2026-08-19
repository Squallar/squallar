//! What `poll_overlay_render_results` puts on the GPU.
//!
//! Driven against a bare `egui::Context`, which is the whole renderer this path
//! needs, and read back through the texture manager's own delta — the very
//! buffer egui hands its painter. That is what makes "the pixels are identical"
//! an observation rather than an argument: the conversion moved to another
//! thread, and the bytes that reach the GPU are compared against the conversion
//! written out by hand.
//!
//! # What this does *not* cover
//!
//! The **poller-to-GPU leg only.** These tests hand
//! `poll_overlay_render_results` a converted response; they do not drive
//! `spawn_overlay_render` itself (the wire tests beside it do —
//! `sites_wire_tests`, `polygon_wire_tests`, `hitmap_wire_tests`,
//! `model_wire_tests`).
//!
//! So the claim "the poller does not convert" rests on two separate things,
//! and neither substitutes for the other: the bytes below, and
//! `frame_thread_conversion_tests::every_overlay_dispatch_is_described_and_converts_nothing`,
//! which is a **source-text** assertion that no dispatch arm converts at all
//! — `offload::execute` converts inside the job, at the rasterizer's own
//! declaration. A conversion that changed shape there would be caught
//! textually and not by comparison. Anyone strengthening this should start
//! there.
//!
//! The conversion the fixture below hands in is `from_rgba_unmultiplied`, and
//! it stays that way deliberately. What is under test is that the poller
//! uploads *whatever image it was given*, byte for byte, so which arm a real
//! rasterizer's output would have taken is beside the point — and an
//! unmultiplied fixture keeps the three arms of `Color32`'s slow path in the
//! comparison. Which arm each rasterizer gets is
//! `rustdar_overlays::render::rasterize::alpha_tests`' subject, against the
//! bytes those rasterizers actually write.

use super::*;
use crate::app::tests::drain_uploads;
use rustdar_geo::GeoBounds;
use rustdar_overlays::render::overlay_state::OverlayKind;

/// A small overlay, and small on purpose: this is about which bytes arrive, and
/// a viewport-sized buffer would only make the comparison slower to run.
const W: u32 = 8;
const H: u32 = 5;

fn bounds() -> GeoBounds {
    GeoBounds {
        min_lat: 34.0,
        max_lat: 36.0,
        min_lon: -99.0,
        max_lon: -97.0,
    }
}

/// RGBA covering the three arms of `Color32::from_rgba_unmultiplied`, because
/// they are not one code path: `a == 0` and `a == 255` are early returns and
/// everything between them takes the multiply. `palette.rs` sets
/// `TRANSPARENCY = 180`, so the slow arm is the one nearly every real pixel
/// takes — and it is the arm a premultiplied shortcut would silently change.
fn rasterizer_output() -> Vec<u8> {
    let mut rgba = Vec::with_capacity((W * H) as usize * 4);
    for i in 0..(W * H) {
        let a = match i % 4 {
            0 => 0,
            1 => 180,
            2 => 255,
            _ => 1,
        };
        rgba.extend_from_slice(&[
            (i * 7 % 256) as u8,
            (i * 13 % 256) as u8,
            (i % 256) as u8,
            a,
        ]);
    }
    rgba
}

/// An app with `n` map panes, so an overlay result naming several of them has
/// somewhere to land.
fn n_pane_app(n: usize) -> crate::app::App {
    crate::app::tests::n_pane_app(n, "KTLX")
}

/// Post one finished overlay for `pane_indices`, converted where the rasterizer
/// converts it, and drain the poller.
fn deliver(app: &mut crate::app::App, ctx: &egui::Context, pane_indices: Vec<usize>) {
    let image = Arc::new(egui::ColorImage::from_rgba_unmultiplied(
        [W as usize, H as usize],
        &rasterizer_output(),
    ));
    app.channels
        .overlay_render_sender
        .send(crate::channels::OverlayRenderResponse {
            image: Some(image),
            geo_bounds: bounds(),
            overlay_kind: OverlayKind::NwsAlerts,
            generation: 7,
            pane_indices,
            zoom: 32,
            hit_map: None,
        })
        .expect("the receiver lives on the App");
    app.poll_overlay_render_results(ctx);
}

fn placed(app: &mut crate::app::App, pane_idx: usize) -> egui::TextureId {
    app.gui
        .pane_mut(pane_idx)
        .expect("pane exists")
        .overlay_cache_mut(&OverlayKind::NwsAlerts.id())
        .current()
        .expect("the poller placed an overlay on this pane")
        .texture
        .id()
}

/// The bytes that reach the GPU are the rasterizer's, unmultiplied exactly as
/// `poll_overlay_render_results` used to unmultiply them.
///
/// The move off the frame thread is a relocation of one call, and this is the
/// statement that it was only a relocation.
#[test]
fn the_uploaded_pixels_are_the_rasterizers_own() {
    let ctx = egui::Context::default();
    let mut app = n_pane_app(1);
    let _ = drain_uploads(&ctx);

    deliver(&mut app, &ctx, vec![0]);

    let uploads = drain_uploads(&ctx);
    assert_eq!(uploads.len(), 1, "one overlay, one upload");
    let expected =
        egui::ColorImage::from_rgba_unmultiplied([W as usize, H as usize], &rasterizer_output());
    assert_eq!(
        uploads[0].size, expected.size,
        "the uploaded texture is not the size the rasterizer was asked for"
    );
    assert_eq!(
        uploads[0].pixels, expected.pixels,
        "the pixels handed to the GPU are no longer \
         `from_rgba_unmultiplied` of the rasterizer's RGBA"
    );
}

/// The texture the panes are given is the size the picture is, not a pair of
/// numbers that travelled beside it.
#[test]
fn the_placed_overlay_is_described_by_its_own_picture() {
    let ctx = egui::Context::default();
    let mut app = n_pane_app(1);
    deliver(&mut app, &ctx, vec![0]);

    let entry = app
        .gui
        .pane_mut(0)
        .expect("pane exists")
        .overlay_cache_mut(&OverlayKind::NwsAlerts.id())
        .current()
        .expect("the poller placed an overlay");
    assert_eq!((entry.width, entry.height), (W, H));
}

/// One rasterization, one upload, however many panes asked for it.
///
/// This has always been true of the overlay path — it is the pattern the
/// plan-view path was missing — and it is pinned here because the conversion
/// moving off the frame thread rewrote the statement that does it.
#[test]
fn four_panes_share_one_overlay_texture() {
    let ctx = egui::Context::default();
    let mut app = n_pane_app(4);
    let _ = drain_uploads(&ctx);

    deliver(&mut app, &ctx, vec![0, 1, 2, 3]);

    assert_eq!(
        drain_uploads(&ctx).len(),
        1,
        "four panes on one overlay raster cost more than one upload"
    );
    let first = placed(&mut app, 0);
    for pane_idx in 1..4 {
        assert_eq!(
            placed(&mut app, pane_idx),
            first,
            "pane {pane_idx} holds its own copy of an overlay texture pane 0 \
             already has; the handle is meant to be cloned, not the picture \
             re-uploaded"
        );
    }
}
