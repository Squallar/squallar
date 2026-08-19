//! The unmultiply stays off the frame thread — and the opaque overlay path
//! stays deleted.
//!
//! Read off the source, for the reason `frame_build_order_tests` gives: what is
//! being asserted is *where a statement is written*, and no runtime observation
//! distinguishes a conversion that ran on the frame thread from one that ran on
//! the rasterizer's — both produce the same pixels, which is the whole point.
//!
//! Every overlay raster is a **described job** now. Nothing converts on
//! arrival: `offload::execute` premultiplies inside the job (the model grid's
//! straight-alpha palette included), so what every consumer holds is already
//! egui's own bytes and reading them through `from_rgba_premultiplied`
//! computes nothing. The assertions below look for a *missing* unmultiply in
//! `app_render`, exactly one compute-nothing read in the one shared overlay
//! deliver — and, since S5d, for the **absence of the opaque path itself**:
//! no `offload(name, closure)` funnel, no closure arm in the overlay
//! dispatch, and an `Opaque` job variant that does not exist on wasm at all.

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

/// The deleted overlay converter, by the name it had: the function that read
/// `RasterizeOutput::alpha` on the frame side and picked an egui constructor
/// from it. It died with the opaque closure arm — the convention is resolved
/// inside the job now — and its name coming back anywhere in `app_fetch` is
/// the page-side conversion coming back with it.
const OVERLAY_CONVERT: &str = "overlay_color_image";

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

/// The overlay dispatch converts nothing and closes over nothing: every arm
/// is a described job handed to the one shared deliver.
///
/// * No arm may convert. `offload::execute` converts inside the job at the
///   rasterizer's own declaration — the model grid's straight alpha included
///   — the reply contract (`RasterizeOutput::straight_rasters_mut`) is
///   premultiplied-always, and the per-kind
///   `..._is_byte_identical_direct_and_via_the_wire` parity tests in
///   `offload::tests` pin the bytes. The deliver therefore reads the buffer
///   through `from_rgba_premultiplied`, which computes nothing — the same
///   read-a-converted-raster shape `upload_section_raster` is *required* to
///   have below — and exactly one such read may exist, in the one shared
///   deliver rather than once per dispatch.
/// * Both dispatch sites — the handler-kind arm (all six handler-backed
///   kinds, the model grid among them) and the sites arm — must hand their
///   reply to that one `overlay_job_deliver`, or a drifting copy grows.
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

/// **The opaque overlay path stays deleted** — S5d's guard, stated as
/// properties of the source so it cannot come back silently.
///
/// Three doors are pinned shut, each beside a presence control on the same
/// haystack so an absence cannot pass vacuously on a moved or renamed file
/// (a scrape that greps an empty string finds nothing and means nothing):
///
/// 1. The dispatch: `spawn_overlay_render` names no `Job::Opaque`, no bare
///    `offload(` funnel and no `prepare_rasterize` — the trait method is
///    deleted, so this is a tripwire for it being re-grown.
/// 2. The funnel: `offload.rs` has no `pub fn offload(` — the function whose
///    wasm arm ran any closure inline on the browser's one thread. The only
///    inline execution left there is `run_here`, the lost-worker fallback
///    every described job shares.
/// 3. The type: `Job::Opaque` is declared under
///    `#[cfg(not(target_arch = "wasm32"))]`, so on wasm the variant does not
///    exist and a dispatch that routed an overlay through a closure would
///    not compile — the compile-level half of the guarantee, witnessed here
///    so the cfg cannot be quietly dropped.
#[test]
fn the_opaque_overlay_path_stays_deleted() {
    let body = body_of(APP_FETCH, "pub(super) fn spawn_overlay_render(");
    // Presence control: the scrape is reading the real dispatch.
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

    // Presence control for the funnel scrape.
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
///
/// The counterpart of the assertions above: they say the conversion is not on
/// the frame thread, and this says where it went instead. `execute`'s output
/// stage — the `straight_rasters_mut` walk over what the row's own output
/// type declares (required, no default, since WO-M7c) — rather than any
/// per-kind arm, because an arm-by-arm conversion is one a new kind can be
/// added without, and the two consumers that would then read a
/// straight-alpha buffer through `from_rgba_premultiplied` would draw it
/// too bright with nothing to catch them.
///
/// Read off the source for the module's reason: what is being asserted is that
/// the statement is written *inside the job*, and a job that ran on this thread
/// produces pixels indistinguishable from one that ran in a worker.
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
