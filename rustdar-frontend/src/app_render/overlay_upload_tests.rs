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
//! `poll_overlay_render_results` a response that was converted the way
//! `spawn_overlay_render` converts one; they do not drive
//! `spawn_overlay_render` itself, because it captures a
//! `Box<dyn FnOnce(..) -> ..>` holding overlay-handler state that no test can
//! construct without a live `OverlayRegistry` and fetched data behind it.
//!
//! So the claim "the conversion did not change" rests on two separate things,
//! and neither substitutes for the other: the bytes below, and
//! `frame_thread_conversion_tests::both_overlay_rasterizers_convert_before_they_send`,
//! which is a **source-text** assertion that both `offload` arms still call
//! `from_rgba_unmultiplied` on the rasterizer's own output. A conversion that
//! changed shape inside those closures would be caught textually and not by
//! comparison. Anyone strengthening this should start there.

use super::*;
use crate::app::tests::headless;
use crate::platform_double::TestBridge;
use rustdar_overlays::render::overlay_state::OverlayKind;
use rustdar_overlays::types::GeoBounds;

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

/// Every whole-texture upload egui has been handed since this was last called,
/// with the pixels it was handed.
///
/// `TexturesDelta::set` is what the renderer uploads and nothing else does, so
/// counting it counts `queue.write_texture` calls exactly — including the ones
/// nobody meant to make.
fn drain_uploads(ctx: &egui::Context) -> Vec<Arc<egui::ColorImage>> {
    ctx.tex_manager()
        .write()
        .take_delta()
        .set
        .into_iter()
        .filter(|(_, delta)| delta.pos.is_none())
        .map(|(_, delta)| {
            let egui::epaint::image::ImageData::Color(image) = delta.image;
            image
        })
        .collect()
}

/// An app with `n` panes, all of them maps, so an overlay result naming several
/// of them has somewhere to land.
fn n_pane_app(n: usize) -> crate::app::App {
    use rustdar_egui::config_store::{ConfigStore, UI_CONFIG_KEY};

    let mut app = headless(TestBridge::desktop());
    let panes = (0..n)
        .map(|_| r#"{"site":"KTLX"}"#.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let store = rustdar_egui::config_store::MemoryConfigStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            &format!(r#"{{"pane_count":{n},"site":"KTLX","panes":[{panes}]}}"#),
        )
        .expect("the memory store always accepts a write");
    assert!(app.gui.load_ui_config(&store), "the fixture config parsed");
    assert_eq!(app.gui.pane_count(), n, "precondition: {n} panes");
    app.render.ensure_pane_count(n);
    app
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
            image,
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
        .overlay_cache_mut(OverlayKind::NwsAlerts)
        .current
        .as_ref()
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
        .overlay_cache_mut(OverlayKind::NwsAlerts)
        .current
        .take()
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
