//! The unmultiply stays off the frame thread.
//!
//! Read off the source, for the reason `frame_build_order_tests` gives: what is
//! being asserted is *where a statement is written*, and no runtime observation
//! distinguishes a conversion that ran on the frame thread from one that ran on
//! the rasterizer's — both produce the same pixels, which is the whole point.
//!
//! There are now two ways a full-size buffer can be kept off the frame thread,
//! and the distinction is what this module is about. The **opaque** overlay
//! rasters convert in the `offload` closure that drew them, because their
//! producer is opaque to the job funnel. Everything **described** — the radar
//! rasters, plan view and section alike, and now the sites overlay
//! (`JobRequest::Overlay`) — does not convert on arrival at all:
//! `offload::execute` premultiplies inside the job, so what every consumer
//! holds is already egui's own bytes and reading them through
//! `from_rgba_premultiplied` computes nothing. That is why the assertions
//! below look for a *missing* unmultiply in `app_render`, a *present*
//! conversion in `app_fetch`'s one remaining opaque overlay arm, and a
//! *compute-nothing read* in its described one.

const APP_RENDER: &str = include_str!("../app_render.rs");
const APP_FETCH: &str = include_str!("../app_fetch.rs");
const OFFLOAD: &str = include_str!("../offload.rs");

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

/// [`body_of`] for a **free** function, whose closing brace is at column 0.
///
/// A separate helper and not a parameter, because the two terminators are not
/// interchangeable: `"\n    }"` matches the first four-space brace inside a free
/// function — `execute`'s `match` closes on one — and would cut the body off
/// before the line this module reads.
fn free_body_of<'a>(source: &'a str, signature: &str) -> &'a str {
    let (_, rest) = source
        .split_once(signature)
        .unwrap_or_else(|| panic!("`{signature}` is no longer written here"));
    rest.split_once("\n}")
        .map(|(body, _)| body)
        .unwrap_or_else(|| panic!("`{signature}` has no recognisable body"))
}

/// The per-pixel unmultiply, by the only name it is ever called by.
const UNMULTIPLY: &str = "from_rgba_unmultiplied";

/// The overlay path's one converter: the function that reads
/// `RasterizeOutput::alpha` and picks the egui constructor from it.
const OVERLAY_CONVERT: &str = "overlay_color_image";

/// The same, as a *call* — so the comment sitting above each one does not get
/// counted as a second conversion.
const OVERLAY_CONVERT_CALL: &str = "Self::overlay_color_image(";

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
///
/// `upload_section_raster` joined the list when the premultiply moved into
/// `offload::execute`. It was the known exception this module used to name: a
/// section pane **retains** its `CrossSection`, so converting before the send
/// once meant carrying a `ColorImage` beside the cut for the life of the
/// session. Premultiplying the cut's own raster inside the job is what made the
/// exception unnecessary rather than merely paid for — there is one buffer, in
/// one convention, and the upload and the resume re-upload both read it.
#[test]
fn no_poller_unmultiplies_on_the_frame_thread() {
    for signature in [
        "fn poll_render_results(",
        "fn apply_render_to_pane(",
        "fn poll_overlay_render_results(",
        "pub(super) fn restore_cached_render(",
        "fn upload_section_raster(",
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
        assert!(
            !body.contains(OVERLAY_CONVERT),
            "`{signature}` converts a full-size overlay buffer on the frame \
             thread. `overlay_color_image` is a per-pixel walk of an 18.7 Mpx \
             texture whichever arm it takes; it belongs in the `offload` \
             closure that produced the pixels.",
        );
    }
}

/// The other half of the same claim: the conversion did not merely leave the
/// frame thread, it landed where the rasterization is.
///
/// Both overlay producers, because they are two sends of one response type and
/// a conversion added back to only one of them would be invisible — the
/// `RadarSites` raster covers the same viewport as any other overlay's. They
/// are no longer two of one shape, though, and each is pinned to its own:
///
/// * The **opaque** arm — the hit-map kinds and the model grid, still
///   closures — has rasterizers whose alpha convention varies by kind:
///   tiny-skia writes premultiplied, `rasterize_model_data` writes straight,
///   and both arrive through the same `prepare_rasterize` arm. So its
///   conversion must be `overlay_color_image`, the one function that reads
///   `RasterizeOutput::alpha`, and a call site that picked an egui
///   constructor itself would be the regression.
/// * The **described** arms — sites and the three polygon kinds — share one
///   deliver, `overlay_job_deliver`, and their replies' convention does not
///   vary: `offload::execute` converts inside the job at the rasterizer's own
///   declaration, the contract on `offload::JobOutput::OverlayRaster` is
///   premultiplied-always, and the per-kind
///   `..._is_byte_identical_direct_and_via_the_wire` parity tests in
///   `offload::tests` pin the bytes. The deliver therefore reads the buffer
///   through `from_rgba_premultiplied`, which computes nothing — the same
///   read-a-converted-raster shape `upload_section_raster` is *required* to
///   have below — and exactly one such read may exist, in the one shared
///   deliver rather than once per dispatch.
#[test]
fn both_overlay_rasterizers_convert_before_they_send() {
    let body = body_of(APP_FETCH, "pub(super) fn spawn_overlay_render(");
    let conversions = body.matches(OVERLAY_CONVERT_CALL).count();
    assert_eq!(
        conversions, 1,
        "`spawn_overlay_render` has {conversions} `overlay_color_image` calls \
         where its one remaining opaque arm needs exactly one. Zero means the \
         handler raster arrives unconverted, with nowhere to be converted but \
         `poll_overlay_render_results` on the frame thread; two means a \
         described arm has gone back to converting what `offload::execute` \
         already converted inside the job.",
    );
    assert!(
        !body.contains(UNMULTIPLY),
        "`spawn_overlay_render` unmultiplies somewhere. No arm may: the \
         opaque one reads `RasterizeOutput::alpha` through \
         `overlay_color_image`, and the described ones receive pixels \
         `offload::execute` already premultiplied inside the job.",
    );
    assert_eq!(
        body.matches("Self::overlay_job_deliver(").count(),
        2,
        "`spawn_overlay_render`'s described arms — the sites dispatch and the \
         polygon-kind dispatch — must both hand their reply to the one shared \
         `overlay_job_deliver`. Fewer means a described arm grew a deliver of \
         its own, which is the drift the shared builder exists to prevent.",
    );
    assert!(
        !body.contains("from_rgba_premultiplied"),
        "`spawn_overlay_render` reads a reply through \
         `from_rgba_premultiplied` inline. That read lives in \
         `overlay_job_deliver` — the one shared deliver — so an inline copy \
         is an arm that has stopped going through it.",
    );

    // The shared deliver itself: exactly one compute-nothing read, and no
    // conversion of any other shape.
    let deliver = body_of(APP_FETCH, "fn overlay_job_deliver(");
    assert_eq!(
        deliver.matches("from_rgba_premultiplied").count(),
        1,
        "`overlay_job_deliver` must read the described reply through exactly \
         one `from_rgba_premultiplied` — the compute-nothing read of the \
         wire's premultiplied-always contract. More is a second guess at a \
         convention the kind's rasterizer did not declare; fewer means the \
         described reply is being converted somewhere else, which can only be \
         the frame thread.",
    );
    assert!(
        !deliver.contains(UNMULTIPLY) && !deliver.contains(OVERLAY_CONVERT),
        "`overlay_job_deliver` converts. The wire's contract is \
         premultiplied-always (`offload::execute` converted inside the job), \
         so any conversion here is a double conversion.",
    );
}

/// The radar rasters are converted by **the job**, at the one place every
/// rasterizing arm funnels through.
///
/// The counterpart of the assertions above: they say the conversion is not on
/// the frame thread, and this says where it went instead. `execute`'s output
/// stage, rather than any of its five arms, because an arm-by-arm conversion is
/// one a sixth arm can be added without — and the two consumers that would then
/// read a straight-alpha buffer through `from_rgba_premultiplied` would draw it
/// too bright with nothing to catch them.
///
/// Read off the source for the module's reason: what is being asserted is that
/// the statement is written *inside the job*, and a job that ran on this thread
/// produces pixels indistinguishable from one that ran in a worker.
#[test]
fn the_job_converts_its_own_rasters() {
    let body = free_body_of(OFFLOAD, "pub fn execute(");
    assert!(
        body.contains("output.map(premultiplied)"),
        "`execute` no longer premultiplies what it answers with. Every radar \
         consumer reads its buffers through \
         `ColorImage::from_rgba_premultiplied`, which computes nothing — so a \
         straight-alpha raster reaching one is not an error, it is a picture \
         drawn at the wrong opacity.",
    );
    assert!(
        !body.contains(UNMULTIPLY),
        "`execute` names the unmultiply in one of its arms. It belongs in the \
         output stage after all of them, which is what stops a sixth arm from \
         being added without it.",
    );
}

/// The section raster is no longer the known exception.
///
/// It was, and the note that stood here explained why: a section pane
/// **retains** its `CrossSection` — the hover reads it and
/// `restore_section_textures` re-uploads from it after a suspend — so
/// converting before the send would have meant carrying a `ColorImage` beside
/// the cut for the life of the session, to save 8 MiB of frame thread once per
/// cut. That trade was never taken. Premultiplying the cut's *own* raster
/// inside the job removed the choice: there is one buffer, the pane retains it,
/// and both uploads read it through a constructor that computes nothing.
#[test]
fn the_section_upload_reads_a_converted_raster() {
    let body = body_of(APP_RENDER, "fn upload_section_raster(");
    assert!(
        body.contains("from_rgba_premultiplied"),
        "`upload_section_raster` no longer reads the cut as premultiplied, so \
         either the section raster has stopped arriving converted — in which \
         case `offload::execute`'s output stage has lost its `Section` arm — or \
         the upload has stopped happening.",
    );
}
