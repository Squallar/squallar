//! The unmultiply stays off the frame thread — and the opaque overlay path
//! stays deleted.

const APP_RENDER: &str = include_str!("../app_render.rs");
const APP_FETCH: &str = include_str!("../app_fetch.rs");
const OFFLOAD: &str = include_str!("../../../rustdar-worker/src/offload.rs");

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

/// The deleted overlay converter, by the name it had.
const OVERLAY_CONVERT: &str = "overlay_color_image";

/// Every function `setup_egui_frame` reaches that used to walk a full-size
/// buffer, and no longer may.
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

/// The overlay dispatch converts nothing and closes over nothing: every arm
/// is a described job handed to the one shared deliver.
#[test]
fn every_overlay_dispatch_is_described_and_converts_nothing() {
    let body = body_of(APP_FETCH, "pub(super) fn spawn_overlay_render(");
    assert!(
        !APP_FETCH.contains(OVERLAY_CONVERT),
        "`overlay_color_image` is back in app_fetch. The page-side alpha \
         conversion died with the opaque overlay arm; `offload::execute` \
         converts inside the job, and a frame-side converter is that cost \
         landing back on the browser's one thread.",
    );
    assert!(
        !body.contains(UNMULTIPLY),
        "`spawn_overlay_render` unmultiplies somewhere. No arm may: every \
         arm receives pixels `offload::execute` already premultiplied inside \
         the job.",
    );
    assert_eq!(
        body.matches("Self::overlay_job_deliver(").count(),
        2,
        "`spawn_overlay_render`'s two dispatch sites — the handler-kind arm \
         and the sites arm — must both hand their reply to the one shared \
         `overlay_job_deliver`. Fewer means an arm grew a deliver of its \
         own, which is the drift the shared builder exists to prevent; more \
         means a third dispatch site exists that this module has never heard \
         of.",
    );
    assert!(
        !body.contains("from_rgba_premultiplied"),
        "`spawn_overlay_render` reads a reply through \
         `from_rgba_premultiplied` inline. That read lives in \
         `overlay_job_deliver` — the one shared deliver — so an inline copy \
         is an arm that has stopped going through it.",
    );

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

/// **The opaque overlay path stays deleted**, stated as properties of the
/// source so it cannot come back silently.
#[test]
fn the_opaque_overlay_path_stays_deleted() {
    let body = body_of(APP_FETCH, "pub(super) fn spawn_overlay_render(");
    assert!(
        body.contains("offload_job("),
        "control: `spawn_overlay_render` no longer calls `offload_job`, so \
         the absence checks below are reading the wrong function",
    );
    for door in ["Job::Opaque", "prepare_rasterize", "offload::offload("] {
        assert!(
            !body.contains(door),
            "`spawn_overlay_render` names `{door}` again. The opaque overlay \
             path was deleted in S5d — on wasm it was an inline gesture-end \
             rasterization on the browser's one thread — and every overlay \
             kind has a wire form, so there is nothing left that needs it.",
        );
    }

    assert!(
        OFFLOAD.contains("pub fn offload_job("),
        "control: offload.rs no longer declares `offload_job`, so the \
         absence check below is reading the wrong file",
    );
    assert!(
        !OFFLOAD.contains("pub fn offload("),
        "offload.rs declares `pub fn offload(` again — the funnel whose wasm \
         arm ran closures inline on the browser's one thread. Its deletion \
         is what makes \"no overlay can rasterize inline\" a property of the \
         API; the lost-worker fallback (`run_here`) is the only inline \
         execution wasm is meant to have.",
    );
    assert!(
        OFFLOAD.contains("#[cfg(not(target_arch = \"wasm32\"))]\n    Opaque("),
        "`Job::Opaque` is no longer declared native-only. The cfg is the \
         compile-level guarantee that no wasm dispatch can construct an \
         opaque job at all; without it, the guarantee is back to being every \
         dispatch site staying careful.",
    );
}

/// The radar rasters are converted by **the job**, at the one place every
/// rasterizing row funnels through.
#[test]
fn the_job_converts_its_own_rasters() {
    let body = free_body_of(OFFLOAD, "pub fn execute(");
    assert!(
        body.contains("straight_rasters_mut()") && body.contains("premultiply_raster(raster)"),
        "`execute` no longer premultiplies what it answers with (the \
         `straight_rasters_mut` walk left its output stage). Every radar \
         consumer reads its buffers through \
         `ColorImage::from_rgba_premultiplied`, which computes nothing — so a \
         straight-alpha raster reaching one is not an error, it is a picture \
         drawn at the wrong opacity.",
    );
    assert!(
        !body.contains(UNMULTIPLY),
        "`execute` names the unmultiply per kind. It belongs in the output \
         stage after every kind, driven by each output type's own declared \
         posture, which is what stops a new kind from being added without \
         it.",
    );
}

/// The section raster is no longer the known exception.
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
