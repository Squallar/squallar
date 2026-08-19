//! The renderer and volume pins that stayed behind when the egui/wgpu
//! renderer moved to rustdar-gpu (WO-RG) and the 3D stack to
//! rustdar-volumetric (WO-RV): each one scrapes a file THIS crate owns — the
//! frame path and surface-loss gate in `app_render.rs`, the one production
//! `EguiRenderer::new` call, the wake closure and the probe/latch calls in
//! `app_state.rs` — so they live beside their subjects rather than in the
//! crate whose type they mention.

/// A named function's body, read out of a source file this crate ships.
///
/// `present_frame` and `AppState::new` both need a real `Window`, a wgpu
/// device and a swapchain, so no host test can run either. Reading the source
/// is the only handle there is — copied from the renderer's own test module,
/// which moved to rustdar-gpu with it.
fn body_of(source: &'static str, signature: &str) -> &'static str {
    source
        .split_once(signature)
        .and_then(|(_, rest)| rest.split_once("\n    }"))
        .map(|(body, _)| body)
        .unwrap_or_else(|| panic!("`{signature}` is no longer a method there"))
}

/// The frame path must submit through `rustdar_gpu::egui_renderer::PreparedFrame::submit`.
///
/// `submit` takes the encoder by value, so it is impossible to submit egui's
/// buffer *through it* without the callbacks' — but that only closes the door
/// on the type level for callers that use it. A caller can still write
/// `queue.submit(Some(encoder.finish()))` itself, which is exactly the
/// pre-fix code and compiles clean. There are two submit sites (the frame
/// that acquired a surface and the frame that did not) and both matter: a
/// callback that recorded work for a frame nobody draws still has to be
/// flushed rather than leaked.
#[test]
fn the_frame_path_submits_only_through_prepared_frame() {
    let body = body_of(
        include_str!("../app_render.rs"),
        "pub(super) fn present_frame(",
    );

    let submits = body.matches("frame.submit(").count();
    assert_eq!(
        submits, 2,
        "present_frame should submit through PreparedFrame::submit exactly \
             twice — once for the frame that got a surface and once for the \
             frame that did not — found {submits}"
    );
    assert!(
        !body.contains("encoder.finish()"),
        "present_frame finishes the encoder itself instead of handing it to \
             PreparedFrame::submit, which skips the paint callbacks' command \
             buffers entirely"
    );
}

/// The pass `draw` opens must be the pass `attachment_config` describes.
///
/// `draw` hard-codes `depth_stencil_attachment: None` and
/// `resolve_target: None`, while `new` accepts *any* depth format and sample
/// count and forwards them to egui's own pipeline. Those two are already one
/// call-site edit away from disagreeing, and the failure mode is a pipeline
/// that declares depth (or MSAA) for a pass that has neither: a validation
/// error at draw time, from a `create_render_pipeline` that returns no
/// `Result`. Publishing `attachment_config()` makes the disagreement
/// reachable by anything building a pipeline, so pin both halves.
///
/// The renderer half is scraped CROSS-CRATE (the arch_ratchets ALLOWED-re-key
/// precedent): `draw` moved to rustdar-gpu at WO-RG, while the only production
/// construction stayed here in `app_state.rs` — this test is the one place
/// both halves can be read side by side.
#[test]
fn the_pass_draw_opens_matches_what_attachment_config_promises() {
    let draw = body_of(
        include_str!("../../../rustdar-gpu/src/egui_renderer.rs"),
        "    pub fn draw(",
    );
    assert!(
        draw.contains("depth_stencil_attachment: None"),
        "draw now attaches a depth buffer, so `AttachmentConfig::depth_format` \
             must stop being able to disagree with it"
    );
    assert!(
        draw.contains("resolve_target: None"),
        "draw now resolves MSAA, so a single-sampled `msaa_samples` no longer \
             describes this pass"
    );

    // The only production construction, and what makes the two consistent.
    let state = include_str!("../app_state.rs");
    let call = state
        .split_once("EguiRenderer::new(")
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.split_once(')'))
        .map(|(args, _)| args)
        .expect("app_state no longer constructs an EguiRenderer");
    assert!(
        call.contains("None") && call.contains(", 1,"),
        "app_state constructs the EguiRenderer with `{call}` — a depth format \
             or a sample count that `draw`'s render pass does not provide, so \
             egui's own pipeline no longer matches its own pass"
    );
}

/// The wake `AppState::new` injects must end in this crate's redraw request.
///
/// The other half of the renderer's own
/// `the_renderer_installs_that_wake_on_the_context_it_builds`: WO-RG inverted
/// the wake — `EguiRenderer::new` now installs whatever closure the caller
/// hands it, so "the wake reaches `notify_redraw`" stopped being the
/// renderer's property and became this call site's. Losing the closure's
/// window capture reproduces the pre-fix "panel close shudders" / "tile never
/// appears" class: every off-frame `ctx.request_repaint()` becomes a no-op on
/// a loop parked in `ControlFlow::Wait`.
///
/// A source probe because `AppState::new` needs a real window and adapter.
#[test]
fn the_wake_app_state_builds_ends_in_a_redraw_request() {
    let source = include_str!("../app_state.rs");
    let wakes = source.matches("let wake = {").count();
    assert_eq!(
        wakes, 1,
        "expected exactly one named wake binding in app_state.rs, found \
             {wakes} — the pass/attachment pin scrapes the construction call's \
             argument list, so the wake must stay a named binding"
    );
    let binding = source
        .split_once("let wake = {")
        .and_then(|(_, rest)| rest.split_once("};"))
        .map(|(body, _)| body)
        .expect("the wake binding is no longer a block");
    assert!(
        binding.contains("notify_redraw("),
        "the wake no longer ends in a redraw request, so it produces a loop \
             iteration rather than the frame egui asked for: {binding}"
    );
    assert!(
        binding.contains("window.clone()"),
        "the wake no longer captures the window, so it has nothing to ask for \
             a redraw: {binding}"
    );
}

/// `AppState::new` must actually install the latch and run the probe.
///
/// Neither is enforced by the type system: `volume_support` could be filled
/// in with a literal `Supported` and `install_error_latch` deleted outright,
/// and everything would still compile and pass. What would be lost is the
/// entire second layer of defence — errors back to panicking, on a device
/// nobody checked. `AppState::new` needs a window and a surface, so reading
/// the source is the only handle there is.
///
/// Moved here from rustdar-volumetric's crate root at WO-RV (it scrapes a
/// file THIS crate owns), needles re-keyed to the cross-crate spellings the
/// re-point left behind.
#[test]
fn app_state_probes_the_device_and_installs_the_latch() {
    let body = body_of(include_str!("../app_state.rs"), "pub async fn new(");

    for call in [
        "rustdar_volumetric::probe(",
        "rustdar_volumetric::install_error_latch(",
    ] {
        assert!(
            body.contains(call),
            "AppState::new no longer calls `{call}`, so the volume view's \
             pre-check or its error latch is gone"
        );
    }
}

/// A lost surface only counts against the volume when one was on screen.
///
/// The gate is the property, not the call: counting every surface loss would
/// retire 3D after two unplugged monitors on a machine whose GPU never
/// complained. `present_frame` needs a real swapchain, so this reads source.
///
/// Moved here from rustdar-volumetric's crate root at WO-RV, with the pins
/// above, for the same reason: the file it pins is this crate's.
#[test]
fn a_surface_loss_is_only_counted_when_a_volume_was_on_screen() {
    let body = body_of(
        include_str!("../app_render.rs"),
        "pub(super) fn present_frame(",
    );

    let call = body
        .find("note_surface_loss_with_volume(")
        .expect("present_frame no longer counts surface losses against the volume view");
    let preamble = &body[..call];
    assert!(
        preamble.contains("rustdar_radar::types::RenderView::Volume"),
        "present_frame counts a surface loss against the volume view without \
         first checking that a volume pane was on screen"
    );
}
