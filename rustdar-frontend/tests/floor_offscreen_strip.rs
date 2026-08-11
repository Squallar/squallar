//! Can a pane's map be drawn *only* into the mirror, and never onto the glass?
//!
//! This is the load-bearing question under "3D is a render mode of a map pane
//! rather than a pane kind of its own". Today a 3D pane borrows another pane's
//! render for its floor (`VolumePane::source_pane`), and the mirror pass copies
//! that pane's rect. If the 3D view *is* the pane, the pane's rect is occupied
//! by the volume, so the pane no longer emits map primitives for the mirror to
//! copy — and the floor has nowhere to come from.
//!
//! The answer is that the map is drawn into an **off-screen strip**: a rect in
//! egui's own coordinate space that lies *below* the frame. Two passes then see
//! the same tessellated geometry differently, because they are handed different
//! attachments:
//!
//! * the **screen** pass, whose attachment is the frame, scissors the strip to
//!   zero height and skips it (`egui_wgpu::Renderer::render`, renderer.rs:515,
//!   skips a zero-size scissor while still advancing the mesh iterators — the
//!   same property the existing mirror filter is built on);
//! * the **mirror** pass, whose attachment is the frame *plus* the strip, has
//!   texels down there and draws it.
//!
//! No second `walkers::Map`, no synthetic projector, no per-layer floor
//! compositor, and no hand-written rasteriser — the thing that was deleted once
//! already and must not come back. The pane draws its map exactly once, with
//! exactly the primitives, tile requests and layer ordering it draws today; only
//! *where* it lands moves.
//!
//! # What each assertion is protecting
//!
//! The design has four premises, and three of them are the kind that are
//! obviously true right up until egui changes its mind:
//!
//! 1. **egui does not cull geometry outside the screen rect.** It does not, but
//!    an `egui::Area` *does* spend its first frame invisible working out its own
//!    size, which would blank the floor for a frame every time a pane entered 3D
//!    mode. A bare `Ui` on its own layer with the rect given up front has no
//!    sizing pass, and [`the_strip_is_tessellated_on_its_very_first_frame`] is
//!    what pins the difference — it asserts on frame **0**, deliberately.
//! 2. **The screen pass drops it and the mirror pass keeps it**, purely from the
//!    attachment size. See [`scissor`].
//! 3. **Growing the mirror does not move the frame's own geometry**, so every
//!    existing floor stays registered where it is. This is the same statement
//!    the adaptive rung rests on (`egui_renderer::mirror`'s module doc): egui's
//!    vertex shader divides by `size_in_pixels / pixels_per_point`, so a taller
//!    attachment at the same scale simply has more texels underneath.
//! 4. **A hidden map cannot swallow input.** The pointer is in frame
//!    coordinates and the strip is not in the frame, so this is structural
//!    rather than a suppression flag someone has to remember to set.
//!
//! # Why the scissor arithmetic is restated rather than called
//!
//! `egui_wgpu::ScissorRect` is private. So [`scissor`] restates it, on the
//! precedent `tests/floor_alignment.rs` sets for `volume.wgsl`'s three lines of
//! mapping: the restatement is honest because the *decision* it feeds — drawn or
//! skipped — is asserted in both directions against real tessellator output.

/// `egui_wgpu::ScissorRect::new`, restated. See the module doc.
///
/// Returns the scissor's width and height in texels. `egui_wgpu` skips the
/// primitive when **either** is zero (`renderer.rs:515`), which is the whole
/// mechanism: the same clip rect is a real scissor against one attachment and
/// nothing at all against a smaller one.
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
///
/// A uniform translation rather than a packing, so two 3D panes can never
/// collide and the mirror is bounded at twice the frame however many there are.
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
