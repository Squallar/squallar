//! **A raster that painted nothing costs no picture, and still clears the
//! pane.**
//!
//! The measurement that opened this, over four Tier-2 browser legs on
//! 2026-09-04 — two targets, never added: Firefox 1928 pictures of which 368
//! carried no ink (19.1%) at 8.92 MB each, and 1856 of which 266 (14.3%);
//! Chromium 664 of which 72 (10.8%) at 8.26 MB each, and 681 of which 77
//! (11.3%). Every one of those was a full-size transparent RGBA buffer built
//! out of the reply, handed to `Context::load_texture` and uploaded, in order
//! to change no pixel of the frame.
//!
//! **The trap this file exists to hold shut.** A blank picture is not spare
//! work: it is how a layer whose data has gone away *replaces* the ink the
//! pane is drawing. An elision that simply skipped blanks would leave that ink
//! on the glass while `pictures`, `picture_bytes` and every upload figure
//! improved — a win in the ledger and a defect on the map. So the saving and
//! the clear are pinned together here, in both directions:
//!
//! * a blank arrival allocates no picture and uploads nothing, and the pane it
//!   reaches stops drawing the layer;
//! * an inked arrival is still built, still uploaded whole, and still drawn;
//! * a **failed** render still clears nothing, because a failure is the answer
//!   the arrival must ignore and a blank is the one it must obey;
//! * and a pane that has taken a blank does **not** ask for that raster again
//!   on the next frame, which is the dispatch storm a cleared cache with no
//!   memory of the clear would run.
//!
//! Every reply below goes through the production output stage
//! (`JobOut::discard_blank_rasters`, which is `offload::execute`'s second
//! half) and then the production `App::overlay_job_deliver`, over real
//! rasterizer bytes — so what decides blank-versus-painted is `has_ink` off
//! the frame thread and never the fixture.

use crate::app::tests::drain_uploads;
use squallar_egui::overlay_cache::ledger;
use squallar_geo::GeoBounds;
use squallar_source::id::known;

const W: u32 = 8;
const H: u32 = 5;
const KIND: squallar_source::id::LayerId = known::NWS_ALERTS;

fn bounds() -> GeoBounds {
    GeoBounds {
        min_lat: 34.0,
        max_lat: 36.0,
        min_lon: -99.0,
        max_lon: -97.0,
    }
}

/// A buffer of the dispatch's own size with ink in its **last** pixel — where
/// a predicate that peeks at the head or samples a stride gets it wrong.
fn inked() -> Vec<u8> {
    let mut rgba = vec![0u8; (W * H * 4) as usize];
    *rgba.last_mut().expect("the buffer is not empty") = 1;
    rgba
}

fn blank() -> Vec<u8> {
    vec![0u8; (W * H * 4) as usize]
}

/// Post `rgba` through the real deliver as the answer to a real dispatch, then
/// run the arrival.
///
/// **The mark a real dispatch leaves**, and this fixture has to leave it too:
/// `poll_overlay_render_results` accepts a raster only while the cache is
/// still waiting for that very dispatch, so a reply posted against no mark
/// exercises the drop path instead of the one under test.
fn arrive(app: &mut crate::app::App, ctx: &egui::Context, generation: u64, rgba: Option<Vec<u8>>) {
    arrive_for(app, ctx, generation, rgba, None)
}

/// [`arrive`] with a [`BlankReason`] armed on the way in, standing in for the
/// gridded rasterizer, which is the one row that arms today.
///
/// **The reason is armed and never settled here.** `discard_blank_rasters`
/// below is the production output stage: what it does with an armed reason —
/// keep it — and with an unarmed one — write `Unattributed` — is part of what
/// these tests exercise, so the fixture states only what a rasterizer states.
fn arrive_for(
    app: &mut crate::app::App,
    ctx: &egui::Context,
    generation: u64,
    rgba: Option<Vec<u8>>,
    reason: Option<squallar_overlays::render::rasterize::BlankReason>,
) {
    if let Some(pane) = app.gui.pane_mut(0) {
        pane.overlay_cache_mut(&KIND).renders.record(
            squallar_egui::overlay_cache::RenderTicket::whole(generation, bounds()),
        );
    }
    let response = crate::channels::OverlayRenderResponse {
        crop: None,
        picture: None,
        geo_bounds: bounds(),
        overlay_kind: KIND,
        generation,
        pane_indices: vec![0],
        zoom: 32,
        hit_map: None,
        // The pane's live raster, not a loop frame's.
        frame: None,
    };
    let out = rgba.map(|rgba| {
        use squallar_source::job::JobOut;
        let mut raster = squallar_overlays::render::rasterize::RasterizeOutput {
            crop: None,
            rgba: rgba.into(),
            hit_cells: None,
            alpha: squallar_overlays::render::rasterize::AlphaMode::Premultiplied,
            // Unjudged going in, exactly as a rasterizer hands it over: the
            // call below is the run funnel's own output stage, and it is what
            // decides. A fixture that set this itself would assert its own
            // input.
            blank: None,
            blank_reason: reason,
        };
        raster.discard_blank_rasters();
        squallar_source::job::DescribedOut(Box::new(raster))
    });
    crate::app::App::overlay_job_deliver(
        "test-blank",
        W,
        H,
        None,
        response,
        crate::channels::OverlayReplySink {
            sender: app.channels.overlay_render_sender.clone(),
            level: std::sync::Arc::clone(&app.channels.overlay_reply_bytes),
        },
        None,
    )(out);
    app.poll_overlay_render_results(ctx);
}

/// The texture this pane is drawing for the layer, or `None` if it draws none.
fn on_screen(app: &mut crate::app::App) -> Option<egui::TextureId> {
    Some(
        app.gui
            .pane_mut(0)?
            .overlay_cache_mut(&KIND)
            .current()?
            .texture
            .id(),
    )
}

/// **The saving and the clear, in one arrival.**
///
/// Red on the tree before this landed, at the upload assertion: the blank
/// raster became a {W}x{H} `ColorImage`, was uploaded, and `picture_bytes`
/// grew by its full size.
#[test]
fn a_blank_raster_clears_a_pane_without_a_picture_sized_upload() {
    let ctx = egui::Context::default();
    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    let _ = drain_uploads(&ctx);

    // The ink the blank has to replace. Without this the clear below is
    // vacuous: a pane drawing nothing already draws nothing.
    arrive(&mut app, &ctx, 7, Some(inked()));
    let painted = on_screen(&mut app).expect(
        "fixture: the inked raster did not reach the pane, so the clear this \
         test asserts has nothing to clear",
    );
    assert_eq!(
        drain_uploads(&ctx).len(),
        1,
        "fixture: the inked raster was not uploaded, so an absent upload \
         below would prove nothing",
    );

    let before = ledger::totals();
    arrive(&mut app, &ctx, 8, Some(blank()));
    let after = ledger::totals();

    // ── The saving ──────────────────────────────────────────────────────
    let uploads = drain_uploads(&ctx);
    assert!(
        uploads.is_empty(),
        "a raster with no ink in it was uploaded anyway: {} texture(s), \
         {} pixels. Nothing on screen can change by it — that is what \
         `has_ink` established in the run funnel's output stage — so every \
         one of those bytes is a picture-sized allocation, transfer and \
         upload spent to draw nothing",
        uploads.len(),
        uploads.iter().map(|u| u.pixels.len()).sum::<usize>(),
    );
    assert_eq!(
        after.picture_bytes - before.picture_bytes,
        0,
        "the blank arrival was charged {} bytes of picture. The buffer is \
         given up where `has_ink` decides, so a non-zero figure here is the \
         transparent `ColorImage` still being allocated",
        after.picture_bytes - before.picture_bytes,
    );
    assert_eq!(
        (after.pictures - before.pictures, after.inked - before.inked),
        (1, 0),
        "a blank is still an arrival that reached a pane — one picture, no \
         ink — or `Totals::arrivals_balance` stops being an identity and \
         `pictures - inked` stops naming the blank population",
    );
    assert_eq!(
        after.dropped, before.dropped,
        "the blank was counted as a drop. A drop is what the arrival throws \
         away; this one was obeyed",
    );

    // ── The clear ───────────────────────────────────────────────────────
    assert!(
        on_screen(&mut app).is_none(),
        "the pane is still drawing the picture it had before the blank \
         arrived. A layer whose data goes away rasterizes blank, and this is \
         the arrival that has to take its ink off the glass — the saving \
         above is worth nothing if it is bought by leaving {painted:?} up",
    );
    assert!(
        app.gui
            .pane_mut(0)
            .expect("pane exists")
            .overlay_cache_mut(&KIND)
            .is_blank(),
        "the cache forgot that the answer it took was blank, which is not the \
         same state as never having been answered — see the re-dispatch \
         assertion below",
    );
}

/// **The other direction, and the one that matters more**: an over-firing
/// elision that suppressed real pictures would be worse than the waste it
/// removed.
#[test]
fn an_inked_raster_is_still_built_uploaded_and_drawn() {
    let ctx = egui::Context::default();
    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    let _ = drain_uploads(&ctx);

    let before = ledger::totals();
    arrive(&mut app, &ctx, 7, Some(inked()));
    let after = ledger::totals();

    let uploads = drain_uploads(&ctx);
    assert_eq!(
        uploads.len(),
        1,
        "an inked raster was not uploaded. The whole overlay layer is off the \
         glass and every byte figure reads better for it",
    );
    assert_eq!(
        uploads[0].size,
        [W as usize, H as usize],
        "the uploaded texture is not the size the rasterizer was asked for",
    );
    assert_eq!(
        after.picture_bytes - before.picture_bytes,
        u64::from(W * H * 4),
        "the inked picture was charged something other than its own size",
    );
    assert_eq!(
        after.inked - before.inked,
        1,
        "a picture with a non-transparent pixel was counted as painting \
         nothing",
    );
    assert!(
        on_screen(&mut app).is_some(),
        "the inked picture never reached the pane",
    );
}

/// **A failed render is not a clear.** The two arrive at the same place with
/// no picture on them, and telling them apart is the whole reason `Blank` is a
/// variant rather than an absence: a failure must leave the glass alone.
#[test]
fn a_failed_render_leaves_the_ink_it_could_not_replace() {
    let ctx = egui::Context::default();
    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    let _ = drain_uploads(&ctx);

    arrive(&mut app, &ctx, 7, Some(inked()));
    let painted = on_screen(&mut app).expect("fixture: the inked raster reached the pane");

    let before = ledger::totals();
    // No output at all: the worker died, or the job was withdrawn.
    arrive(&mut app, &ctx, 8, None);
    let after = ledger::totals();

    assert_eq!(
        on_screen(&mut app),
        Some(painted),
        "a render that failed wiped the picture the pane was drawing. \
         Nothing replaced it, so the map now shows nothing where it showed \
         data — the failure mode of treating every picture-less arrival as a \
         clear",
    );
    assert_eq!(
        after.dropped - before.dropped,
        1,
        "a failed render was not counted as a drop, so `arrivals_balance` no \
         longer holds",
    );
    assert_eq!(
        after.pictures, before.pictures,
        "a failed render was counted as a picture",
    );
}

/// **A cleared pane is up to date, not unanswered.**
///
/// `needs_rerender` returns `true` for a cache holding no picture, because
/// that is a pane that has never been answered. A blank answer leaves no
/// picture behind, so without [`OverlayTextureCache::show_blank`] remembering
/// what the clear was rendered for, every blank layer would re-ask for the
/// same empty raster on every frame for ever — a dispatch storm on exactly
/// the layers that cost the least to draw.
#[test]
fn a_pane_that_took_a_blank_does_not_ask_for_it_again() {
    let ctx = egui::Context::default();
    let mut app = crate::app::tests::n_pane_app(1, "KTLX");

    arrive(&mut app, &ctx, 8, Some(blank()));

    let plan = squallar_egui::overlay_cache::OverlayTexturePlan {
        width: W,
        height: H,
        overdraw: 0.0,
        pixels_per_point: 1.0,
        pane_px: [0, 0],
    };
    let cache = app
        .gui
        .pane_mut(0)
        .expect("pane exists")
        .overlay_cache_mut(&KIND);
    let asked = cache.needs_rerender(
        8,
        32.0 / squallar_egui::overlay_cache::ZOOM_QUANTIZATION_FACTOR,
        squallar_egui::overlay_cache::ZoomDrive::AT_REST,
        &bounds(),
        &plan,
    );
    assert!(
        !asked,
        "the pane asked for the same empty raster again on the very next \
         frame. A blank that is not remembered is a re-dispatch every frame \
         for as long as the layer keeps rasterizing empty",
    );
}

/// **A blank names its reason on the always-on counters, and the split is a
/// partition of the blanks.**
///
/// The figure this closes the hole under: `pictures - inked` was measured at
/// 37-63 % of dispatches on two browser legs and filed as *waste*. Read as "a
/// picture disappeared from under the user whenever the previous one had ink",
/// the same number is a *correctness* figure — and nothing in the tree could
/// say which of the two a given run deserved, because the reason went
/// unrecorded. A count of blanks is what already existed four times over; what
/// did not exist is why any individual one blanked.
///
/// Three conjuncts, and the second and third are what keep the first from
/// being vacuous:
///
/// * each reason lands on **its own** counter, at a count no other holds, so a
///   transposition between two slots cannot read as correct;
/// * the reasons **partition** the blanks (`blank_reasons_balance`), which is
///   an identity rather than an expectation because `note_blank` writes both
///   sides — it reading false is a blank counted by one path and not the
///   other;
/// * and `blanks_over_covered_ground` is **strictly between zero and the blank
///   total**, which is the only shape that proves the split discriminates. A
///   counter that answered one variant for everything would satisfy the
///   balance completely and say nothing.
#[test]
fn a_blank_names_its_reason_and_the_reasons_partition_the_blanks() {
    use squallar_overlays::render::rasterize::BlankReason;

    let ctx = egui::Context::default();
    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    let _ = drain_uploads(&ctx);

    // Distinct counts, none a multiple or prefix of another, and one of them
    // the CORRECT clear so the covered-ground subtotal has both sides in it.
    let plan = [
        (BlankReason::NoDataInWindow, 3u64),
        (BlankReason::OutsideCoverage, 5),
        (BlankReason::ExtentDeclaredEmpty, 2),
    ];

    let before = ledger::totals();
    let mut generation = 700;
    for (reason, times) in plan {
        for _ in 0..times {
            generation += 1;
            arrive_for(&mut app, &ctx, generation, Some(blank()), Some(reason));
        }
    }
    let after = ledger::totals();

    // The ledger is process-global and other tests in this binary write it, so
    // every reading below is a DIFFERENCE across this arrival run and never an
    // absolute. See the crate note on filtered runs.
    let posted: u64 = plan.iter().map(|(_, n)| n).sum();
    assert_eq!(
        after.blanks() - before.blanks(),
        posted,
        "the {posted} blanks posted here did not all reach `pictures - inked`",
    );
    for (reason, times) in plan {
        assert_eq!(
            after.blank_reason(reason) - before.blank_reason(reason),
            times,
            "{times} blanks were posted as {reason:?} and the counter for it \
             moved by {}. A reason that lands on a neighbour's slot balances \
             perfectly and attributes every one of them wrongly",
            after.blank_reason(reason) - before.blank_reason(reason),
        );
    }
    assert!(
        after.blank_reasons_balance(),
        "the reasons do not account for every blank. `note_blank` writes both \
         sides, so this is not an expectation that can drift — it is a blank \
         counted by one path and not the other",
    );

    // The only proven-correct clear is `OutsideCoverage`; every other reason
    // may have cleared a pane the layer still covered.
    // 3 `NoDataInWindow` + 2 `ExtentDeclaredEmpty`. The second pair is the
    // arm worth stating: a handler's own refusal LOOKS like a correct clear
    // and counts here anyway, because nothing downstream checks it.
    let covered = after.blanks_over_covered_ground() - before.blanks_over_covered_ground();
    assert_eq!(
        covered, 5,
        "of the {posted} blanks posted, 5 were not shown to be correct and \
         {covered} were counted so",
    );
    assert!(
        covered > 0 && covered < posted,
        "the covered-ground subtotal is all or nothing of the blanks, so it \
         discriminates nothing and a run cannot be read as a loss or a saving",
    );
    assert!(
        after.distinct_blank_reasons() >= 3,
        "fewer than three reasons have ever been observed, so the breakdown \
         is not shown to split anything",
    );
}

/// **An unarmed blank arrives as `Unattributed`, and is never given a
/// plausible reason.**
///
/// The alerts rasterizer this file drives arms nothing, and that is the
/// shipping state of every row but the gridded one. What must not happen is
/// `settle_blank` filling the gap in from the buffer's shape: a reason derived
/// downstream agrees with the code that produced it by construction, so it can
/// never contradict it, and the whole point of the counter is that it can.
/// `Unattributed` is a reading — "this row has not been taught yet" — and the
/// hole stays visible instead of inflating whichever reason it was merged
/// with.
#[test]
fn a_blank_no_rasterizer_explained_is_counted_as_unexplained() {
    use squallar_overlays::render::rasterize::BlankReason;

    let ctx = egui::Context::default();
    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    let _ = drain_uploads(&ctx);

    let before = ledger::totals();
    arrive(&mut app, &ctx, 811, Some(blank()));
    let after = ledger::totals();

    assert_eq!(
        after.blank_reason(BlankReason::Unattributed)
            - before.blank_reason(BlankReason::Unattributed),
        1,
        "a blank whose producer armed no reason was not counted as \
         unattributed",
    );
    for reason in BlankReason::ALL {
        if reason == BlankReason::Unattributed {
            continue;
        }
        assert_eq!(
            after.blank_reason(reason) - before.blank_reason(reason),
            0,
            "an unarmed blank was filed as {reason:?}. That reason was \
             invented downstream of the branch that would have known it, \
             which is the failure `BlankReason` exists to make impossible",
        );
    }
    assert!(
        after.blanks_over_covered_ground() - before.blanks_over_covered_ground() == 1,
        "an unattributed blank must count AGAINST covered ground: nothing \
         said the layer was absent here, so the pane was cleared over ground \
         the layer may well still cover",
    );
}
