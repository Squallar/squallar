use super::*;
use crate::app::tests::drain_uploads;
use rustdar_geo::GeoBounds;
use rustdar_source::id::known;

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

fn n_pane_app(n: usize) -> crate::app::App {
    crate::app::tests::n_pane_app(n, "KTLX")
}

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
            overlay_kind: known::NWS_ALERTS,
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
        .overlay_cache_mut(&known::NWS_ALERTS)
        .current()
        .expect("the poller placed an overlay on this pane")
        .texture
        .id()
}

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

#[test]
fn the_placed_overlay_is_described_by_its_own_picture() {
    let ctx = egui::Context::default();
    let mut app = n_pane_app(1);
    deliver(&mut app, &ctx, vec![0]);

    let entry = app
        .gui
        .pane_mut(0)
        .expect("pane exists")
        .overlay_cache_mut(&known::NWS_ALERTS)
        .current()
        .expect("the poller placed an overlay");
    assert_eq!((entry.width, entry.height), (W, H));
}

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
