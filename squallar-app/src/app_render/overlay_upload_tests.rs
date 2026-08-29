use super::*;
use crate::app::tests::drain_uploads;
use squallar_geo::GeoBounds;
use squallar_source::id::known;

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
    // **The mark a real dispatch leaves**, and these fixtures have to leave it
    // too. `poll_overlay_render_results` accepts a raster only while the cache
    // is still waiting for that very dispatch — the stale-result policy on
    // `RendersInFlight::retire` — so a reply posted against no mark at all
    // exercises that path instead of the one under test here. Every reply below
    // is the answer to a dispatch, so every one gets its ticket.
    for &idx in &pane_indices {
        if let Some(pane) = app.gui.pane_mut(idx) {
            pane.overlay_cache_mut(&known::NWS_ALERTS).renders.record(
                squallar_egui::overlay_cache::RenderTicket::whole(7, bounds()),
            );
        }
    }
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
            // The pane's live raster, not a loop frame's.
            frame: None,
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

/// Post a reply nothing is waiting for. `RendersInFlight::retire` answers
/// stale, `retain` empties the pane list, and the picture is thrown away
/// **before** `Context::load_texture` — the drop arm of the arrival path.
fn deliver_unmarked(app: &mut crate::app::App, ctx: &egui::Context, pane_indices: Vec<usize>) {
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
            frame: None,
        })
        .expect("the receiver lives on the App");
    app.poll_overlay_render_results(ctx);
}

/// **The raster ledger's counters move on the real arrival path, and every
/// arrival lands on exactly one side of the upload branch.**
///
/// Two arms, deliberately: a reply the cache is still waiting for, which
/// becomes a picture, and a reply it is not, which is thrown away before the
/// upload. Without the second the byte figure would be an assertion that
/// *something* was counted rather than that the right thing was, and a path
/// that had stopped dropping anything — counting every stale raster's bytes as
/// uploaded — would pass.
///
/// **The deltas are `>=` and the balance is `==`, and that is not laziness.**
/// The counters are process-global `static`s and this binary runs its tests in
/// parallel, so another test's arrival can land between two readings here; a
/// `==` on a delta would be asserting the harness's scheduling. The two things
/// that survive that are asserted instead: a monotone counter can only be
/// pushed *up* by a concurrent test, so a delta that must grow still fails if
/// this path stopped counting; and `arrivals_balance` is a process-wide
/// identity, so it holds under any interleaving and breaks the moment any
/// arrival anywhere takes a third exit. The *exact* per-event figures are
/// pinned where nothing is shared — `UploadTotals` is per renderer, see
/// `the_upload_ledger_counts_every_byte_of_a_banded_raster_once`, and each
/// Tier-2 browser leg is a fresh process.
#[test]
fn every_arrival_is_either_a_picture_or_a_drop() {
    use squallar_egui::overlay_cache::ledger;
    let ctx = egui::Context::default();
    let mut app = n_pane_app(1);
    let _ = drain_uploads(&ctx);

    let before = ledger::totals();

    deliver(&mut app, &ctx, vec![0]);
    let uploaded = placed(&mut app, 0);
    let after_picture = ledger::totals();

    assert!(
        after_picture.arrived > before.arrived,
        "a reply crossed the receiver and the ledger recorded no arrival",
    );
    assert!(
        after_picture.pictures > before.pictures,
        "the pixels reached `load_texture` and the ledger recorded no picture",
    );
    assert!(
        after_picture.picture_bytes >= before.picture_bytes + u64::from(W * H * 4),
        "a {W}x{H} RGBA picture was uploaded and the byte figure grew by less \
         than its {} bytes, so it is not counting the picture's own size",
        W * H * 4,
    );
    assert!(
        after_picture.on_screen() > before.on_screen(),
        "the picture went on screen and neither route counted it",
    );

    deliver_unmarked(&mut app, &ctx, vec![0]);
    let after_drop = ledger::totals();

    assert_eq!(
        placed(&mut app, 0),
        uploaded,
        "the pane took a raster it had not asked for, so this arm is not \
         exercising the drop it says it is",
    );
    assert!(
        after_drop.dropped > after_picture.dropped,
        "a stale raster was thrown away before the upload and the ledger \
         recorded no drop, so its rasterized bytes are invisible",
    );
    // The balance is a process-wide identity, but it is only an identity *at
    // rest*: a concurrent test that has counted an arrival and not yet
    // counted its picture or drop is mid-path, and a snapshot taken inside
    // that window reads unbalanced without any arrival having leaked (seen
    // in the wild: 32 against 29+2, once, not reproducible). So the read is
    // retried briefly. This spends none of the assertion's power — an
    // arrival that truly left by a third exit stays unbalanced forever, and
    // the bounded wait still fails on it.
    let mut latest = after_drop;
    for _ in 0..50 {
        if latest.arrivals_balance() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        latest = ledger::totals();
    }
    assert!(
        latest.arrivals_balance(),
        "{} arrivals against {} pictures and {} drops: an arrival left the \
         path by an exit neither counter names",
        latest.arrived,
        latest.pictures,
        latest.dropped,
    );
}
