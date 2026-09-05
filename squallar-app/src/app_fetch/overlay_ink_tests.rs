//! **A picture that paints nothing is separable from one that paints.**
//!
//! The hole, measured 2026-08-31 and found while deliberately emptying a
//! raster: nothing anywhere discarded or marked a blank picture.
//! `ledger::note_picture` took the RGBA buffer's length and counted it whatever
//! was in it, so a texture layer emitting a fully transparent pixmap satisfied
//! every conjunct `--expect-overlay-rasters` had — `dispatched > 0`,
//! `arrived > 0`, `pictures > 0`, `picture_bytes > 0`, `shown + promoted > 0`,
//! and the arrival balance. The rig read 6 dispatched → 6 arrived → 6 pictures
//! over a layer drawing nothing, and anyone reasoning "the rig would catch it
//! if the layer stopped drawing" was wrong.
//!
//! **The reading is taken off the frame thread, and never at the arrival.**
//! `poll_overlay_render_results` runs inside `setup_egui_frame`; the question
//! costs a pass over a buffer that is 17.8 MiB on a 1080p pane. That is the
//! same rule `no_poller_unmultiplies_on_the_frame_thread` states for the
//! premultiply, and `OverlayPicture` is what carries the answer across.
//!
//! Since 2026-09-04 it is taken one seam earlier still — in the run funnel's
//! output stage, `RasterizeOutput::settle_blank`, beside the premultiply that
//! already walks the same buffer — so that a blank never reaches the reply
//! codec with a picture-sized payload attached. `App::overlay_job_deliver`
//! *reads* that answer; the fixtures below run the same output stage over real
//! rasterizer bytes and then the real deliver, so nothing here states the
//! answer it asserts.
//!
//! **What the reading now decides, and not only reports.** The blank picture
//! was still built, transferred and uploaded — a full-size transparent RGBA
//! buffer, 8.26 MB on the measured Chromium legs and 8.92 MB on the Firefox
//! ones, to draw nothing. It is now `OverlayPicture::Blank`, two `u32`s that
//! say what the buffer would have been, and the pane clears on it exactly as
//! it cleared on the transparent picture — see
//! `OverlayTextureCache::show_blank`. **A blank is a clear, never a skip**:
//! it is what replaces the ink of a layer whose data has gone away, so
//! dropping it would leave that ink on the glass with every figure improving.
//!
//! The two tests below are the two sides that make the reading non-vacuous,
//! and each fails on its own: a `has_ink` stuck at `false` turns every honest
//! picture into a clear and is caught by the first; one stuck at `true`
//! rebuilds the buffer this exists to elide and is caught by the second.
//! Neither can be satisfied by the fixture stating its own answer — both put
//! real rasterizer bytes through the real output stage and the real
//! `App::overlay_job_deliver`, and read the variant off what those built.

use squallar_geo::GeoBounds;
use squallar_source::id::known;

const W: u32 = 64;
const H: u32 = 48;

fn bounds() -> GeoBounds {
    GeoBounds {
        min_lat: 34.0,
        max_lat: 36.0,
        min_lon: -99.0,
        max_lon: -97.0,
    }
}

/// Run the real output stage and then the real deliver over `rgba`, and hand
/// back the response they built.
///
/// The picture goes in `None` deliberately: a fixture that seeded the answer it
/// then asserts would pass with the predicate deleted. Every variant below is
/// one production code wrote, and `blank` goes in `None` for the same reason —
/// `JobOut::discard_blank_rasters` is the production call `offload::execute`
/// makes, and it is what decides.
fn delivered(app: &crate::app::App, rgba: Vec<u8>) -> crate::channels::OverlayRenderResponse {
    delivered_for(app, rgba, None)
}

/// [`delivered`], for the pane's live raster (`None`) or for one frame of an
/// animating layer's loop (`Some`).
fn delivered_for(
    app: &crate::app::App,
    rgba: Vec<u8>,
    frame: Option<squallar_source::time::FrameStamp>,
) -> crate::channels::OverlayRenderResponse {
    let response = crate::channels::OverlayRenderResponse {
        picture: None,
        geo_bounds: bounds(),
        overlay_kind: known::NWS_ALERTS,
        generation: 5,
        pane_indices: vec![0],
        zoom: 32,
        hit_map: None,
        frame,
    };
    crate::app::App::overlay_job_deliver(
        "test-ink",
        W,
        H,
        None,
        response,
        app.channels.overlay_render_sender.clone(),
        None,
    )(Some(squallar_source::job::DescribedOut(settled(rgba))));
    app.channels
        .overlay_render_receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("every deliver answers, failure included")
}

/// `rgba` through the run funnel's output stage, boxed as the funnel hands it
/// over — `offload::execute`'s second half and nothing else.
fn settled(rgba: Vec<u8>) -> Box<dyn squallar_source::job::JobOut> {
    use squallar_source::job::JobOut;
    let mut out = squallar_overlays::render::rasterize::RasterizeOutput {
        rgba,
        hit_cells: None,
        alpha: squallar_overlays::render::rasterize::AlphaMode::Premultiplied,
        blank: None,
    };
    out.discard_blank_rasters();
    Box::new(out)
}

/// A buffer of the dispatch's own size, entirely transparent.
fn blank() -> Vec<u8> {
    vec![0u8; (W * H * 4) as usize]
}

/// **The floor: a picture that paints is built and delivered.**
///
/// Without this the elision could be satisfied by a `has_ink` that always
/// answers `false`, which turns every overlay on the map into a clear and
/// would be diagnosed as a broken rasterizer rather than as a broken
/// predicate.
///
/// The ink is in the **last** pixel, which is where the cheap wrong answers —
/// peek at the head, sample a stride — get it wrong. An alerts raster whose
/// only content is one polygon has exactly this shape.
#[test]
fn a_picture_with_one_pixel_of_ink_is_marked_as_painting() {
    let app = crate::app::tests::n_pane_app(1, "KTLX");

    let mut rgba = blank();
    *rgba.last_mut().expect("the buffer is not empty") = 1;
    let painted = delivered(&app, rgba);

    let Some(crate::channels::OverlayPicture::Painted(image)) = painted.picture else {
        panic!(
            "a picture with a non-transparent pixel was not delivered as one. \
             A `Blank` here is the whole overlay layer elided off the glass, \
             and `--expect-overlay-rasters` reads `inked == 0` on a working \
             pipeline",
        );
    };
    assert_eq!(
        image.as_raw().len(),
        (W * H * 4) as usize,
        "the delivered buffer is not the dispatch's own size",
    );
}

/// **The acceptance: a fully transparent raster costs no picture at all, and
/// is still an answer the pane must obey.**
///
/// The second half is what makes this the fix rather than a skip. `Blank` is
/// `Some`, not `None`: a failed render is what the arrival ignores, and this
/// is what it acts on. `a_blank_raster_clears_a_pane_without_a_picture_sized_upload`
/// is the same statement one layer up, over the pane the clear lands on.
#[test]
fn a_fully_transparent_picture_is_marked_as_painting_nothing() {
    let app = crate::app::tests::n_pane_app(1, "KTLX");

    let empty = delivered(&app, blank());
    let mut inked_rgba = blank();
    *inked_rgba.last_mut().expect("the buffer is not empty") = 1;
    let painted = delivered(&app, inked_rgba);

    assert_eq!(
        empty.geo_bounds, painted.geo_bounds,
        "the fixtures differ somewhere other than their pixels",
    );

    match empty.picture {
        Some(crate::channels::OverlayPicture::Blank { width, height }) => {
            assert_eq!(
                (width, height),
                (W, H),
                "the blank must carry the size the picture would have been — \
                 it is what the pane's rebuild gate judges the clear by, and a \
                 wrong one re-dispatches the same empty raster for ever",
            );
        }
        Some(crate::channels::OverlayPicture::Painted(image)) => panic!(
            "a raster of {} fully transparent bytes was built into a picture \
             anyway. That is the cost this exists to remove, once per blank \
             arrival, in a copy out of the reply buffer and an upload of the \
             result",
            image.as_raw().len(),
        ),
        None => panic!(
            "a blank raster was delivered as a failed render. The arrival \
             drops a failure, so the pane keeps drawing the ink of a layer \
             whose data has gone away — the exact regression an elision that \
             skipped blanks would cause",
        ),
    }

    assert!(
        matches!(
            painted.picture,
            Some(crate::channels::OverlayPicture::Painted(_))
        ),
        "the blank raster and the painted one produced the same answer, so \
         nothing downstream can tell a layer that stopped drawing from one \
         that never did",
    );
}

/// **A loop frame that painted nothing still gets a picture.**
///
/// The destination decides this and not the pixels. A pane's overlay cache can
/// hold "this layer draws nothing here" (`OverlayTextureCache::show_blank`); a
/// `LoopFrameImage` has no such state, so for a loop frame a `Blank` would be
/// filed as a frame that failed or one still owed — holding the previous
/// frame's ink on the glass, or re-asking for the same empty raster for ever.
///
/// It is worth its own test because the *mechanism* changed on 2026-09-04 and
/// the behaviour did not: the transparent buffer is given up in the run
/// funnel's output stage now, so it is no longer there to be copied. The
/// picture is filled here instead, from the dispatch's own dimensions, with
/// the `Color32::TRANSPARENT` every byte of that buffer would have decoded to
/// — the same picture, and none of it on the wire.
#[test]
fn a_blank_loop_frame_is_still_delivered_as_a_picture() {
    let app = crate::app::tests::n_pane_app(1, "KTLX");
    let stamp = squallar_source::time::FrameStamp {
        valid: chrono::NaiveDate::from_ymd_opt(2026, 9, 4)
            .expect("a real date")
            .and_hms_opt(18, 0, 0)
            .expect("a real time"),
        run: None,
    };

    let framed = delivered_for(&app, blank(), Some(stamp));

    let Some(crate::channels::OverlayPicture::Painted(image)) = framed.picture else {
        panic!(
            "a loop frame whose raster painted nothing was not delivered as a \
             picture. `LoopFrameImage` has no blank state, so the frame reads \
             as failed or still owed and the loop either holds the previous \
             frame's ink or re-dispatches this raster for ever",
        );
    };
    assert_eq!(
        image.size,
        [W as usize, H as usize],
        "the filled loop frame is not the dispatch's own size",
    );
    assert!(
        image.as_raw().iter().all(|&byte| byte == 0),
        "the filled loop frame has ink in it that the raster did not paint",
    );

    // And the same bytes to the pane's live raster are still a clear: the
    // frame is what makes the difference, not the pixels.
    assert!(
        matches!(
            delivered(&app, blank()).picture,
            Some(crate::channels::OverlayPicture::Blank { .. })
        ),
        "the pane's live raster took the loop frame's arm; the blank that \
         clears the pane has been turned back into a picture",
    );
}
