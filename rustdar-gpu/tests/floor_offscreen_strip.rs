//! Can a pane's map be drawn *only* into the mirror, and never onto the glass?

/// `egui_wgpu::ScissorRect::new`, restated. See the module doc.
fn scissor(clip: egui::Rect, pixels_per_point: f32, size_in_pixels: [u32; 2]) -> (u32, u32) {
    let width = size_in_pixels[0] as f32;
    let height = size_in_pixels[1] as f32;
    let min_x = (pixels_per_point * clip.min.x).round().clamp(0.0, width);
    let min_y = (pixels_per_point * clip.min.y).round().clamp(0.0, height);
    let max_x = (pixels_per_point * clip.max.x).round().clamp(min_x, width);
    let max_y = (pixels_per_point * clip.max.y).round().clamp(min_y, height);
    (
        (max_x as u32).saturating_sub(min_x as u32),
        (max_y as u32).saturating_sub(min_y as u32),
    )
}

/// Whether `egui_wgpu::Renderer::render` would draw this primitive at all.
fn would_draw(clip: egui::Rect, pixels_per_point: f32, size_in_pixels: [u32; 2]) -> bool {
    let (w, h) = scissor(clip, pixels_per_point, size_in_pixels);
    w != 0 && h != 0
}

const FRAME: egui::Vec2 = egui::vec2(800.0, 600.0);

fn raw_input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, FRAME)),
        ..Default::default()
    }
}

/// The rect a pane's map is drawn into when the pane itself is showing a
/// volume: the pane's own rect, moved a whole frame down.
fn strip_for(pane_rect: egui::Rect) -> egui::Rect {
    pane_rect.translate(egui::vec2(0.0, FRAME.y))
}

/// One frame: a pane's chrome on the glass, its map in the strip.
fn frame(ctx: &egui::Context, pane_rect: egui::Rect) -> Vec<egui::ClippedPrimitive> {
    let output = ctx.run_ui(raw_input(), |ctx| {
        // The pane itself, on the glass. Stands in for the volume and the
        // chrome over it.
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.painter()
                .rect_filled(pane_rect, 0.0, egui::Color32::DARK_BLUE);
        });

        // The same pane's map, in the strip. A bare `Ui` on its own layer with
        // `max_rect` given up front — deliberately not an `egui::Area`, which
        // would spend frame 0 invisible sizing itself.
        let strip = strip_for(pane_rect);
        let layer = egui::LayerId::new(egui::Order::Background, egui::Id::new("floor_strip"));
        let mut ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::new("floor_strip_ui"),
            egui::UiBuilder::new().layer_id(layer).max_rect(strip),
        );
        ui.set_clip_rect(strip);
        ui.painter().rect_filled(strip, 0.0, egui::Color32::GREEN);
    });
    ctx.tessellate(output.shapes, 1.0)
}

/// A pane rect that is not the whole frame, so "the strip is the pane moved
/// down" is distinguishable from "the strip is the frame moved down".
fn pane_rect() -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(40.0, 80.0), egui::vec2(400.0, 300.0))
}

#[test]
fn the_strip_is_tessellated_on_its_very_first_frame() {
    let ctx = egui::Context::default();
    // Frame 0, not frame 1. An `egui::Area` fails this assertion: it paints its
    // content through an invisible painter until it knows its own size, so the
    // floor would be blank for the first frame of every 3D mode entry — a
    // one-frame hole that only shows up on a machine slower than the developer's.
    let tris = frame(&ctx, pane_rect());

    let strip = strip_for(pane_rect());
    let in_strip: Vec<_> = tris
        .iter()
        .filter(|p| strip.contains_rect(p.clip_rect))
        .collect();
    assert_eq!(
        in_strip.len(),
        1,
        "the map did not reach the strip on frame 0; clip rects: {:?}",
        tris.iter().map(|p| p.clip_rect).collect::<Vec<_>>()
    );

    // And the vertices are really down there rather than clamped back into the
    // frame — a clamp would put the map on the glass, under the volume.
    let egui::epaint::Primitive::Mesh(mesh) = &in_strip[0].primitive else {
        panic!("the strip's primitive is not a mesh");
    };
    let lowest = mesh
        .vertices
        .iter()
        .fold(f32::MIN, |acc, v| acc.max(v.pos.y));
    assert!(
        lowest > FRAME.y,
        "egui clamped the strip back into the frame: lowest vertex y = {lowest}, frame = {}",
        FRAME.y
    );
}

#[test]
fn the_screen_pass_drops_the_strip_and_the_mirror_pass_keeps_it() {
    let ctx = egui::Context::default();
    let tris = frame(&ctx, pane_rect());
    let strip = strip_for(pane_rect());

    let pixels_per_point = 2.0;
    let frame_px = [
        (FRAME.x * pixels_per_point) as u32,
        (FRAME.y * pixels_per_point) as u32,
    ];
    // The mirror is the frame plus however far the lowest strip reaches.
    let mirror_px = [frame_px[0], (strip.max.y * pixels_per_point) as u32];

    let on_glass = tris
        .iter()
        .find(|p| !strip.contains_rect(p.clip_rect))
        .expect("nothing was drawn on the glass");
    let in_strip = tris
        .iter()
        .find(|p| strip.contains_rect(p.clip_rect))
        .expect("nothing was drawn in the strip");

    assert!(
        !would_draw(in_strip.clip_rect, pixels_per_point, frame_px),
        "the map drew on the glass: it would cover the volume"
    );
    assert!(
        would_draw(in_strip.clip_rect, pixels_per_point, mirror_px),
        "the map missed the mirror: the floor would be blank"
    );
    assert!(
        would_draw(on_glass.clip_rect, pixels_per_point, frame_px),
        "the pane's own content stopped being drawn"
    );
}

#[test]
fn growing_the_mirror_leaves_the_frames_own_geometry_where_it_was() {
    let ctx = egui::Context::default();
    let tris = frame(&ctx, pane_rect());
    let strip = strip_for(pane_rect());

    let pixels_per_point = 2.0;
    let frame_px = [
        (FRAME.x * pixels_per_point) as u32,
        (FRAME.y * pixels_per_point) as u32,
    ];
    let mirror_px = [frame_px[0], (strip.max.y * pixels_per_point) as u32];

    // Every primitive that is not in the strip scissors identically against the
    // taller attachment. This is what lets a 2D pane go on sourcing a floor at
    // its own rect while a 3D pane sources one from the strip: adding the strip
    // is not a change to anything above it.
    for primitive in tris.iter().filter(|p| !strip.contains_rect(p.clip_rect)) {
        assert_eq!(
            scissor(primitive.clip_rect, pixels_per_point, frame_px),
            scissor(primitive.clip_rect, pixels_per_point, mirror_px),
            "growing the mirror moved {:?}",
            primitive.clip_rect
        );
    }
}

#[test]
fn a_pointer_over_the_pane_never_reaches_the_map_in_the_strip() {
    let ctx = egui::Context::default();
    let pane = pane_rect();
    let strip = strip_for(pane);

    // The pointer is put exactly where the map's own content sits *within the
    // strip* — i.e. the position the map would be hovered at if the strip were
    // on screen. In 3D mode this press belongs to the orbit camera, and the map
    // underneath it must not also pan.
    let mut input = raw_input();
    input.events.push(egui::Event::PointerMoved(strip.center()));

    let mut map_response = None;
    let _ = ctx.run_ui(input, |ctx| {
        let layer = egui::LayerId::new(egui::Order::Background, egui::Id::new("floor_strip"));
        let mut ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::new("floor_strip_ui"),
            egui::UiBuilder::new().layer_id(layer).max_rect(strip),
        );
        ui.set_clip_rect(strip);
        map_response = Some(ui.allocate_rect(strip, egui::Sense::click_and_drag()));
    });

    let response = map_response.expect("the strip's map was never allocated");
    assert!(
        !response.hovered(),
        "the off-screen map claimed the pointer; in 3D mode it would pan under the orbit"
    );
    assert!(
        !response.dragged(),
        "the off-screen map took a drag that belongs to the camera"
    );
}
