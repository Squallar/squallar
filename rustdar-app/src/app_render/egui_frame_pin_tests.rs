//! The renderer and volume pins this crate still holds after the egui/wgpu
//! renderer moved to rustdar-gpu.

/// A named function's body, read out of a source file this crate ships.
fn body_of(source: &'static str, signature: &str) -> &'static str {
    source
        .split_once(signature)
        .and_then(|(_, rest)| rest.split_once("\n    }"))
        .map(|(body, _)| body)
        .unwrap_or_else(|| panic!("`{signature}` is no longer a method there"))
}

/// The frame path must submit through `rustdar_gpu::egui_renderer::PreparedFrame::submit`.
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
