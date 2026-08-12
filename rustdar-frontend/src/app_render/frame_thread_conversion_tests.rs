//! The unmultiply stays off the frame thread.
//!
//! Read off the source, for the reason `frame_build_order_tests` gives: what is
//! being asserted is *where a statement is written*, and no runtime observation
//! distinguishes a conversion that ran on the frame thread from one that ran on
//! the rasterizer's — both produce the same pixels, which is the whole point.

const APP_RENDER: &str = include_str!("../app_render.rs");
const APP_FETCH: &str = include_str!("../app_fetch.rs");

/// The body of the function `signature` opens, up to its closing brace at
/// method indentation.
fn body_of<'a>(source: &'a str, signature: &str) -> &'a str {
    let (_, rest) = source
        .split_once(signature)
        .unwrap_or_else(|| panic!("`{signature}` is no longer written here"));
    rest.split_once("\n    }")
        .map(|(body, _)| body)
        .unwrap_or_else(|| panic!("`{signature}` has no recognisable body"))
}

/// The per-pixel unmultiply, by the only name it is ever called by.
const UNMULTIPLY: &str = "from_rgba_unmultiplied";

/// Every function `setup_egui_frame` reaches that used to walk a full-size
/// buffer, and no longer may.
///
/// `apply_render_to_pane` is the radar half — 4.19 M pixels at 2048², 6.66 ms
/// measured, paid once per pane on the same site rather than once per volume.
/// `poll_overlay_render_results` is the overlay half and the larger one: an
/// overlay texture is planned against the viewport, so on a desktop it is
/// 18.7 M pixels and ~47 ms, drained unbounded over every enabled overlay kind.
///
/// `TRANSPARENCY = 180` in `palette.rs` is what makes the figure what it is:
/// every data pixel takes the slow arm of `Color32::from_rgba_unmultiplied`,
/// neither the `a == 0` nor the `a == 255` fast path, at 31.8× the instructions
/// of the premultiplied constructor.
#[test]
fn no_poller_unmultiplies_on_the_frame_thread() {
    for signature in [
        "fn poll_render_results(",
        "fn apply_render_to_pane(",
        "fn poll_overlay_render_results(",
        "pub(super) fn restore_cached_render(",
    ] {
        let body = body_of(APP_RENDER, signature);
        assert!(
            !body.contains(UNMULTIPLY),
            "`{signature}` unmultiplies a full-size buffer again. It runs inside \
             `setup_egui_frame`, which is the frame-pacing path; the conversion \
             belongs in the `offload` closure that produced the pixels, where \
             the loop path has always done it. See \
             `channels::RenderedImage::image` and \
             `channels::OverlayRenderResponse::image`.",
        );
    }
}

/// The other half of the same claim: the conversion did not merely leave the
/// frame thread, it landed where the rasterization is.
///
/// Both overlay producers, because they are two sends of one response type and
/// a conversion added back to only one of them would be invisible — the
/// `RadarSites` raster covers the same viewport as any other overlay's.
#[test]
fn both_overlay_rasterizers_convert_before_they_send() {
    let body = body_of(APP_FETCH, "pub(super) fn spawn_overlay_render(");
    let conversions = body.matches(UNMULTIPLY).count();
    assert_eq!(
        conversions, 2,
        "`spawn_overlay_render` converts {conversions} of its two rasters \
         before sending. Each `offload` arm has to do it: an unconverted \
         `OverlayRenderResponse` has nowhere to be converted but \
         `poll_overlay_render_results`, on the frame thread.",
    );
}

/// The one full-size unmultiply still on the frame thread, named so that it is
/// a decision rather than an oversight.
///
/// A section raster is `SECTION_WIDTH × SECTION_HEIGHT` — 8 MiB natively,
/// against the plan view's 16 MiB and the overlay's 71.2 MiB — and it is the
/// only one of the three whose producer does not already hold the pixels in a
/// throwaway buffer. `SectionResponse` carries the `CrossSection` itself,
/// which the pane **retains**: the hover reads it, and
/// `restore_section_textures` re-uploads from it after a suspend rather than
/// walking a 15.6 MB volume again. Converting before the send would mean
/// carrying the `ColorImage` *as well as* the cut, permanently, per section
/// pane — a memory cost paid for the life of the session to save 8 MiB of
/// frame thread once per cut.
///
/// If that trade is ever taken, delete this test rather than editing it.
#[test]
fn the_section_raster_is_the_known_exception() {
    let body = body_of(APP_RENDER, "fn upload_section_raster(");
    assert!(
        body.contains(UNMULTIPLY),
        "`upload_section_raster` no longer converts, so either the section \
         raster now arrives converted — in which case this test has been \
         superseded and should go — or the upload has stopped happening.",
    );
}
