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
            picture: Some(crate::channels::OverlayPicture::Painted(image)),
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
            picture: Some(crate::channels::OverlayPicture::Painted(image)),
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

/// Post `rgba` through the production deliver **without draining**, so the
/// window `overlay replies` measures is observable from the outside.
///
/// Everything here is the real path: the production
/// [`crate::app::App::overlay_job_deliver`], the production output stage
/// (`JobOut::discard_blank_rasters`, which is `offload::execute`'s second
/// half) deciding blank-versus-painted off the bytes, and the App's own
/// channel and level.
fn send_reply(app: &mut crate::app::App, generation: u64, rgba: Vec<u8>) {
    use squallar_source::job::JobOut;

    if let Some(pane) = app.gui.pane_mut(0) {
        pane.overlay_cache_mut(&known::NWS_ALERTS).renders.record(
            squallar_egui::overlay_cache::RenderTicket::whole(generation, bounds()),
        );
    }
    let mut raster = squallar_overlays::render::rasterize::RasterizeOutput {
        rgba: rgba.into(),
        hit_cells: None,
        alpha: squallar_overlays::render::rasterize::AlphaMode::Premultiplied,
        blank: None,
    };
    raster.discard_blank_rasters();
    crate::app::App::overlay_job_deliver(
        "test-reply-level",
        W,
        H,
        None,
        crate::channels::OverlayRenderResponse {
            picture: None,
            geo_bounds: bounds(),
            overlay_kind: known::NWS_ALERTS,
            generation,
            pane_indices: vec![0],
            zoom: 32,
            hit_map: None,
            frame: None,
        },
        crate::channels::OverlayReplySink {
            sender: app.channels.overlay_render_sender.clone(),
            level: std::sync::Arc::clone(&app.channels.overlay_reply_bytes),
        },
        None,
    )(Some(squallar_source::job::DescribedOut(Box::new(raster))));
}

/// What the App's own `overlay replies` level reads, in bytes.
fn reply_level(app: &crate::app::App) -> usize {
    app.channels
        .overlay_reply_bytes
        .load(std::sync::atomic::Ordering::Relaxed)
}

/// **An overlay picture is priced from the send to the take, and by nothing
/// else on either side of it.**
///
/// This is the family's whole claim, measured rather than described. The
/// window it names — a finished overlay raster sitting in the reply channel —
/// was priced by no census family at all until 2026-09-08: `renders in
/// flight` is the same instrument for the RADAR replies and reaches no
/// overlay picture, `overlay grids` and `overlay items` price the source data
/// a handler decodes and never the raster drawn from it, and `upload pending`
/// starts only once `Context::load_texture` has filed the picture and the
/// renderer has banded it.
///
/// **Exact, not `>=`, because the level is the App's own.** The census static
/// behind it is process-global and this binary runs its tests in parallel, so
/// a `census()` reading here would be asserting the harness's scheduling; the
/// `Arc<AtomicUsize>` on this `App`'s `ChannelHub` is reachable by nothing
/// else in the process, which is why the level lives there and not in a
/// module static.
///
/// **Three arms, and the third is not padding.** A level that only ever rose
/// would pass the first two; a level that priced the *dispatch* rather than
/// the picture would pass all three but read the same for a blank, so the
/// blank arm is what separates "bytes that exist" from "a raster happened".
/// A blank allocates no `ColorImage` at all — that is the saving
/// `blank_raster_tests` holds — so the honest figure for it is zero.
#[test]
fn an_overlay_picture_is_priced_from_the_reply_until_the_frame_thread_takes_it() {
    /// One picture at this fixture's plan. The real one is far larger — a
    /// 4317x2477 overlay plan is 42,772,836 B — and the arithmetic is the
    /// same at both ends: this family's level is the sum over every reply in
    /// the channel.
    const PICTURE: usize = (W * H * 4) as usize;

    let ctx = egui::Context::default();
    let mut app = n_pane_app(1);
    let _ = drain_uploads(&ctx);

    assert_eq!(
        reply_level(&app),
        0,
        "a fresh App has sent no reply and must price none",
    );

    send_reply(&mut app, 7, rasterizer_output());
    assert_eq!(
        reply_level(&app),
        PICTURE,
        "one {W}x{H} RGBA picture is in the channel and the level does not \
         name its {PICTURE} bytes",
    );

    send_reply(&mut app, 8, rasterizer_output());
    assert_eq!(
        reply_level(&app),
        2 * PICTURE,
        "two pictures are in the channel at once and the level is not their \
         sum — it is a LEVEL over the whole channel, not one reply's size",
    );

    app.poll_overlay_render_results(&ctx);
    assert_eq!(
        reply_level(&app),
        0,
        "the frame thread took both replies and the level did not fall; from \
         the take on, the picture is `load_texture`'s and then the renderer's \
         band queue (`upload pending`), and a level that does not fall here \
         double-counts every picture the census ever sees",
    );

    send_reply(&mut app, 9, vec![0u8; PICTURE]);
    assert_eq!(
        reply_level(&app),
        0,
        "a blank reply allocated no `ColorImage` — that is the saving \
         `blank_raster_tests` holds — so there are no bytes to name, and a \
         level that moved here is pricing the dispatch and not the picture",
    );
    app.poll_overlay_render_results(&ctx);
    assert_eq!(reply_level(&app), 0);
}
