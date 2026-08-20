use super::submission_order;
#[cfg(not(target_arch = "wasm32"))]
use super::{PreparedFrame, Renderer, ScreenDescriptor, TextureFormat, wgpu};

/// A named function's body, read out of a source file this crate ships.
fn body_of(source: &'static str, signature: &str) -> &'static str {
    source
        .split_once(signature)
        .and_then(|(_, rest)| rest.split_once("\n    }"))
        .map(|(body, _)| body)
        .unwrap_or_else(|| panic!("`{signature}` is no longer a method there"))
}

/// **`ctx.request_repaint()` from a background thread has to reach winit.**
#[test]
fn an_off_frame_repaint_request_reaches_the_event_loop() {
    let ctx = egui::Context::default();
    let woke = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&woke);
    super::install_repaint_wake(&ctx, move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });

    let asking = ctx.clone();
    std::thread::spawn(move || asking.request_repaint())
        .join()
        .expect("the requesting thread panicked");

    assert_eq!(
        woke.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a repaint asked for off-frame woke nothing, so whatever asked for it \
             is waiting for an unrelated event to draw its result"
    );
}

/// …and a *timed* request must not, or every dwell becomes a busy loop.
#[test]
fn a_timed_repaint_request_is_left_to_the_frames_own_schedule() {
    let ctx = egui::Context::default();
    let woke = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&woke);
    super::install_repaint_wake(&ctx, move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });

    ctx.request_repaint_after(std::time::Duration::from_secs(1));

    assert_eq!(
        woke.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a request to repaint in a second was spent as a request to repaint \
             now"
    );
}

/// The wiring itself: the one place a `Context` is built has to install it,
/// and what it installs has to be the caller's injected wake — nothing else.
#[test]
fn the_renderer_installs_that_wake_on_the_context_it_builds() {
    let body = body_of(include_str!("../egui_renderer.rs"), "    pub fn new(");
    let installs = body.matches("install_repaint_wake(&egui_context").count();
    assert_eq!(
        installs, 1,
        "`new` must install a repaint wake on the context it builds exactly \
             once; found {installs}: {body}"
    );
    assert!(
        body.contains("install_repaint_wake(&egui_context, wake)"),
        "the context is built with something other than the caller's injected \
             wake, so the app loop's redraw request is no longer what an \
             off-frame `ctx.request_repaint()` reaches: {body}"
    );
}

/// The callbacks' command buffers must precede egui's own.
#[test]
fn the_callbacks_command_buffers_are_submitted_before_eguis() {
    assert_eq!(
        submission_order(vec!["callback 0", "callback 1"], "egui"),
        vec!["callback 0", "callback 1", "egui"],
    );
}

/// With no callbacks, egui's buffer is still submitted, and alone.
#[test]
fn a_frame_with_no_callbacks_still_submits_eguis_own_buffer() {
    assert_eq!(submission_order(Vec::new(), "egui"), vec!["egui"]);
}

/// `update_buffers`' return must be bound and carried, not dropped.
#[test]
fn end_pass_and_upload_carries_the_callback_command_buffers() {
    let body = body_of(
        include_str!("../egui_renderer.rs"),
        "pub fn end_pass_and_upload(",
    );
    let call = body
        .find("update_buffers(")
        .expect("end_pass_and_upload no longer calls update_buffers");

    // The whole statement the call sits in — from the previous statement
    // boundary to its own `;`. Not the line: rustfmt is free to wrap the
    // binding onto a line of its own, and it does.
    let statement_start = body[..call].rfind(';').map_or(0, |semi| semi + 1);
    let statement = body[statement_start..]
        .split_once(';')
        .map(|(head, _)| head)
        .expect("the update_buffers call is not a statement");
    assert!(
        statement.contains("let user_command_buffers"),
        "update_buffers' returned command buffers are discarded again. Any \
             CallbackTrait::prepare that records into them then renders nothing, \
             silently — the return is not #[must_use]. Found: {statement:?}"
    );

    assert!(
        body.contains("user_command_buffers,"),
        "end_pass_and_upload binds the callback command buffers but does not \
             put them on the PreparedFrame it returns, so they are dropped one \
             line later instead of at the call"
    );
}

// `the_frame_path_submits_only_through_prepared_frame` pins rustdar-app's
// `present_frame` and stayed behind at WO-RG: it lives in the app side's
// `app_render/egui_frame_pin_tests.rs`, beside the file it scrapes.

/// `attachment_config` must report the pass, not a guess at it.
#[test]
fn attachment_config_is_built_from_new_s_own_parameters() {
    let body = body_of(include_str!("../egui_renderer.rs"), "    pub fn new(");
    for (field, parameter) in [
        ("color_format", "output_color_format"),
        ("depth_format", "output_depth_format"),
        ("msaa_samples", "msaa_samples"),
    ] {
        // Field-init shorthand where the two names coincide, which is what
        // clippy asks for and what `msaa_samples` therefore has to be.
        let written = format!("{field}: {parameter}");
        let shorthand = format!("{field},");
        assert!(
            body.contains(&written) || (field == parameter && body.contains(&shorthand)),
            "AttachmentConfig::{field} is not initialised from `new`'s \
                 `{parameter}` parameter, so `attachment_config()` describes \
                 something other than the pass egui was configured for"
        );
    }
}

// `the_pass_draw_opens_matches_what_attachment_config_promises` scrapes BOTH
// this crate's `egui_renderer.rs` and rustdar-app's `app_state.rs` (the
// only production construction), so it stayed on the app side at WO-RG — in
// `app_render/egui_frame_pin_tests.rs`, where the same-crate half lives.

/// A callback's own command buffer reaches the queue, on a real device.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
#[cfg(not(target_arch = "wasm32"))]
fn a_paint_callbacks_own_command_buffer_reaches_the_queue() {
    /// Anything but zero, so a buffer that was never written is telling.
    const SENTINEL: u32 = 0xC0FF_EE01;

    /// Copies [`SENTINEL`] from `source` into `landing` — in a command buffer
    /// of its own, which is the mechanism under test. Recording into the
    /// `egui_encoder` argument instead would pass even with the defect
    /// present, because that encoder was always submitted.
    struct SentinelCallback {
        source: wgpu::Buffer,
        landing: wgpu::Buffer,
    }

    impl egui_wgpu::CallbackTrait for SentinelCallback {
        fn prepare(
            &self,
            device: &wgpu::Device,
            _queue: &wgpu::Queue,
            _screen_descriptor: &ScreenDescriptor,
            _egui_encoder: &mut wgpu::CommandEncoder,
            _resources: &mut egui_wgpu::CallbackResources,
        ) -> Vec<wgpu::CommandBuffer> {
            let mut own = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rustdar.volume.test.sentinel"),
            });
            own.copy_buffer_to_buffer(&self.source, 0, &self.landing, 0, 4);
            vec![own.finish()]
        }

        fn paint(
            &self,
            _info: egui::epaint::PaintCallbackInfo,
            _pass: &mut wgpu::RenderPass<'static>,
            _resources: &egui_wgpu::CallbackResources,
        ) {
            // Nothing to draw: this test never records egui's render pass.
        }
    }

    // Same constructor the app uses, so `WGPU_BACKEND` selects the backend
    // here too.
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .expect("no wgpu adapter; this test is ignored by default for that reason");
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default()))
        .expect("could not create a device on an adapter that was found");

    let buffer = |label: &str, usage| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: 4,
            usage,
            mapped_at_creation: false,
        })
    };
    let source = buffer(
        "sentinel source",
        wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
    );
    let landing = buffer(
        "sentinel landing",
        wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
    );
    let readback = buffer(
        "sentinel readback",
        wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
    );
    queue.write_buffer(&source, 0, &SENTINEL.to_le_bytes());

    let mut renderer = Renderer::new(
        &device,
        TextureFormat::Rgba8Unorm,
        egui_wgpu::RendererOptions::default(),
    );
    let screen_descriptor = ScreenDescriptor {
        size_in_pixels: [64, 64],
        pixels_per_point: 1.0,
    };
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(64.0, 64.0));
    let tris = vec![egui::ClippedPrimitive {
        clip_rect: rect,
        primitive: egui::epaint::Primitive::Callback(egui_wgpu::Callback::new_paint_callback(
            rect,
            SentinelCallback {
                source,
                landing: landing.clone(),
            },
        )),
    }];

    // The two production lines this test can reach: capture, then submit.
    let mut encoder = device.create_command_encoder(&Default::default());
    let user_command_buffers =
        renderer.update_buffers(&device, &queue, &mut encoder, &tris, &screen_descriptor);
    assert_eq!(
        user_command_buffers.len(),
        1,
        "egui did not gather the callback's command buffer at all, so this \
             test cannot say anything about submission"
    );
    let mut frame = PreparedFrame {
        tris,
        screen_descriptor,
        textures_to_free: Vec::new(),
        user_command_buffers,
        repaint_delay: std::time::Duration::MAX,
    };
    frame.submit(&queue, encoder);

    let mut readback_encoder = device.create_command_encoder(&Default::default());
    readback_encoder.copy_buffer_to_buffer(&landing, 0, &readback, 0, 4);
    queue.submit(Some(readback_encoder.finish()));
    readback.slice(..).map_async(wgpu::MapMode::Read, |r| {
        r.expect("mapping the readback buffer failed");
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("polling the device failed");

    let mapped = readback.slice(..).get_mapped_range();
    let landed = u32::from_le_bytes(
        <[u8; 4]>::try_from(&mapped[..4]).expect("the readback buffer is 4 bytes"),
    );
    assert_eq!(
        landed, SENTINEL,
        "the callback's command buffer never executed. egui returns it from \
             update_buffers and that return is not #[must_use], so dropping it \
             leaves a callback rendering nothing with no error anywhere."
    );
}

/// `begin_frame`'s body, read out of this file's own source.
fn begin_frame_body() -> &'static str {
    body_of(include_str!("../egui_renderer.rs"), "pub fn begin_frame(")
}

/// Both input rewrites must precede `begin_pass`, and only this file says so.
#[test]
fn the_input_rewrites_run_before_begin_pass() {
    let body = begin_frame_body();
    let begin_pass = body
        .find("begin_pass(")
        .expect("begin_frame no longer starts a pass");

    for call in ["normalize_touch_devices(", "normalize_wheel_units("] {
        let at = body
            .find(call)
            .unwrap_or_else(|| panic!("begin_frame no longer calls {call}"));
        assert!(
            at < begin_pass,
            "{call} runs after begin_pass, so egui has already bucketed \
                 this frame's events and the rewrite lands a frame late"
        );
    }
}

/// The wheel rewrite must be *reachable*, and reachable on the web only.
#[test]
fn the_wheel_rewrite_is_gated_on_wasm32_and_nothing_else() {
    let body = begin_frame_body();
    let at = body
        .find("normalize_wheel_units(")
        .expect("begin_frame no longer calls normalize_wheel_units");

    // Back up to the start of the call's own line, so the search lands on
    // the attribute above it rather than on the call's indentation.
    let line_start = body[..at].rfind('\n').map_or(0, |nl| nl + 1);
    let guard = body[..line_start]
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .expect("nothing at all precedes the wheel rewrite");

    assert_eq!(
        guard, r#"#[cfg(target_arch = "wasm32")]"#,
        "the wheel rewrite must sit directly under that cfg and no other \
             guard; found {guard:?}"
    );
}

/// Both theme paths turn label text-selection off, and keep it off.
#[test]
fn both_theme_paths_turn_label_text_selection_off() {
    for order in [[true, false], [false, true]] {
        let ctx = egui::Context::default();
        for use_dark in order {
            super::apply_theme_to_context(&ctx, use_dark);
            assert_eq!(
                ctx.global_style().visuals.dark_mode,
                use_dark,
                "the palette half of apply_theme stopped applying"
            );
            for theme in [egui::Theme::Dark, egui::Theme::Light] {
                assert!(
                    !ctx.style_of(theme).interaction.selectable_labels,
                    "labels are text-selectable in the {theme:?} style after \
                     applying the {} theme (flip order {order:?})",
                    if use_dark { "dark" } else { "light" },
                );
            }
        }
    }
}

/// A primitive is dropped from the mirror by *clamping* its clip rect, and
/// clamped to the source pane it belongs to when it belongs to one.
#[test]
fn the_mirror_filter_clamps_rather_than_removes() {
    use super::clamp_to_sources;
    let pane = egui::Rect::from_min_max(egui::pos2(100.0, 50.0), egui::pos2(500.0, 450.0));
    let other = egui::Rect::from_min_max(egui::pos2(600.0, 50.0), egui::pos2(900.0, 450.0));

    // Inside the pane: kept as it is.
    let inside = egui::Rect::from_min_max(egui::pos2(120.0, 60.0), egui::pos2(300.0, 200.0));
    assert_eq!(clamp_to_sources(inside, &[pane, other]), inside);

    // Straddling the pane's edge: narrowed to the pane, so the part of a
    // widget that hangs outside its map does not land on the floor.
    let straddling = egui::Rect::from_min_max(egui::pos2(400.0, 400.0), egui::pos2(700.0, 700.0));
    assert_eq!(
        clamp_to_sources(straddling, &[pane]),
        egui::Rect::from_min_max(egui::pos2(400.0, 400.0), egui::pos2(500.0, 450.0)),
    );

    // Belonging to no source — the sidebar, the top bar, another pane's
    // chrome, a 3D pane itself. Zero size, which `render` skips.
    let elsewhere = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(50.0, 40.0));
    let dropped = clamp_to_sources(elsewhere, &[pane, other]);
    assert!(
        dropped.width() == 0.0 || dropped.height() == 0.0,
        "a primitive outside every source pane must clamp to nothing, got {dropped:?}",
    );

    // No sources at all is the same answer, not a pass-through: an empty
    // guest list must mirror an empty frame rather than the whole one.
    assert!(clamp_to_sources(inside, &[]).width() == 0.0);
}

/// The mirror pass keeps the ordering that makes it correct at all.
#[test]
fn the_mirror_pass_submits_between_the_two_uploads_and_runs_before_prepare() {
    let source = include_str!("../egui_renderer.rs");

    let mirror = body_of(source, "fn render_mirror(");
    let upload = mirror
        .find("update_buffers(")
        .expect("render_mirror no longer stages the geometry it draws");
    let submit = mirror
        .find("queue.submit(")
        .expect("render_mirror no longer submits, so its staging never lands");
    let pass = mirror
        .find("begin_render_pass(")
        .expect("render_mirror no longer opens a pass");
    assert!(
        upload < pass && pass < submit,
        "render_mirror must stage, then draw, then submit — got upload at \
         {upload}, pass at {pass}, submit at {submit}",
    );
    assert!(
        mirror.contains("Primitive::Mesh(egui::epaint::Mesh::default())"),
        "render_mirror no longer swaps callbacks out before staging, so every \
         CallbackTrait::prepare — the volume raymarch included — now runs twice \
         a frame",
    );
    assert!(
        mirror.contains("LoadOp::Clear(wgpu::Color::TRANSPARENT)"),
        "the mirror must clear transparent: the shader reads zero alpha as \
         'the source pane is not showing this ground', and any other clear \
         carpets the floor",
    );

    let outer = body_of(source, "pub fn end_pass_and_upload(");
    let call = outer
        .find("self.render_mirror(")
        .expect("end_pass_and_upload no longer draws the mirror");
    let buffers = outer
        .find("update_buffers(")
        .expect("end_pass_and_upload no longer calls update_buffers");
    assert!(
        call < buffers,
        "the mirror pass must run before update_buffers dispatches the paint \
         callbacks' prepare — that is what samples it. Reversed, the floor is \
         one frame behind the pane it mirrors on every pan.",
    );
}
