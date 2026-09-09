//! The renderer and volume pins this crate still holds after the egui/wgpu
//! renderer moved to squallar-gpu.

/// A named function's body, read out of a source file this crate ships.
fn body_of(source: &'static str, signature: &str) -> &'static str {
    source
        .split_once(signature)
        .and_then(|(_, rest)| rest.split_once("\n    }"))
        .map(|(body, _)| body)
        .unwrap_or_else(|| panic!("`{signature}` is no longer a method there"))
}

/// The frame path must submit through `squallar_gpu::egui_renderer::PreparedFrame::submit`.
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
        include_str!("../../../squallar-gpu/src/egui_renderer.rs"),
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
        "squallar_volumetric::probe(",
        "squallar_volumetric::install_error_latch(",
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
/// **Scraped from `abandon_graphics_state` rather than from `present_frame`,
/// which is where this teardown used to be spelled.** It moved out whole when
/// the WebGL2 context-restore path landed and needed the identical seven
/// steps: the browser's own context loss never reaches `present_frame`'s
/// acquire at all, so the two callers reach one function. The assertion is
/// unchanged — the call must still be inside a check that a volume pane was on
/// screen — only the body it reads moved with the code.
#[test]
fn a_surface_loss_is_only_counted_when_a_volume_was_on_screen() {
    let source = include_str!("../app_render.rs");
    let body = body_of(source, "fn abandon_graphics_state(");

    // Control: the teardown is still what a lost surface reaches. A scrape of
    // a function nothing calls would pass while the arm did nothing.
    let present = body_of(source, "pub(super) fn present_frame(");
    assert!(
        present.contains("self.abandon_graphics_state("),
        "present_frame no longer reaches the teardown this test scrapes, so a \
         lost surface leaves the app holding the dead device's handles"
    );

    let call = body
        .find("note_surface_loss_with_volume(")
        .expect("the graphics teardown no longer counts surface losses against the volume view");
    let preamble = &body[..call];
    assert!(
        preamble.contains("squallar_radar::types::RenderView::Volume"),
        "the graphics teardown counts a surface loss against the volume view \
         without first checking that a volume pane was on screen"
    );
}

/// **Every path that abandons a graphics context takes the one teardown.** The
/// steps were copied nowhere when the browser's restore path landed: a copy
/// that drifted by one step would leave a handle pointing at a dead device,
/// which is a crash rather than a stale picture.
#[test]
fn the_graphics_teardown_has_exactly_one_spelling() {
    let source = include_str!("../app_render.rs");
    let teardown = body_of(source, "fn abandon_graphics_state(");
    // Split so this file cannot count itself, exactly as
    // `app/gui_seam_ratchet_tests.rs` splits its own scrape: the crate-wide
    // `self.``gui.` walk counts every file in this crate, test files included.
    let step = concat!("self.", "gui.", "clear_graphics_state()");
    assert!(
        teardown.contains(step),
        "control: the teardown no longer contains `{step}`, so the count \
         below is not reading the code it exists to guard"
    );
    let spellings = source.matches(step).count();
    assert_eq!(
        spellings, 1,
        "`{step}` is spelled {spellings} times in app_render.rs; the surface \
         loss and the context restore must reach ONE teardown, not two that \
         agree today"
    );
}

/// **Every submission tells the staging rings it happened.**
///
/// Both rings — the raster bands' and the geometry's — record their copies on
/// the frame's own encoder and submit nothing of their own, which is what took
/// two whole `queue.submit` calls a frame off this thread. The price is that
/// neither may ask for its host mapping back until that encoder is on the
/// queue, so each submission owes them one `after_submit`. A submission that
/// does not pay it runs both rings out of mapped slots
/// (`squallar_gpu::staging_ring::STAGING_RING_DEPTH` of them) and every frame
/// after that takes the route this path exists to avoid — the BAR window for
/// the geometry, a blocking `write_texture` for every band — with no picture
/// changing and nothing else going red.
///
/// The skipped-surface arm counts: that frame submits too, and this frame's
/// bands are in the command buffer it submits.
#[test]
fn every_frame_submission_hands_the_staging_rings_back_their_mappings() {
    let body = body_of(
        include_str!("../app_render.rs"),
        "pub(super) fn present_frame(",
    );

    let submits = body.match_indices("frame.submit(").count();
    let told = body.match_indices("egui_renderer.after_submit()").count();
    assert_eq!(
        submits, told,
        "present_frame submits {submits} time(s) and tells the staging rings \
         {told} time(s); a submission that never says so costs the rings their \
         slots and the frame thread the BAR window",
    );

    for (at, _) in body.match_indices("frame.submit(") {
        let after = &body[at..];
        let told_at = after
            .find("egui_renderer.after_submit()")
            .unwrap_or(usize::MAX);
        let next_submit = after[1..]
            .find("frame.submit(")
            .map_or(usize::MAX, |at| at + 1);
        assert!(
            told_at < next_submit,
            "a `frame.submit(` is followed by another before the staging \
             rings are told the first one went",
        );
    }
}
