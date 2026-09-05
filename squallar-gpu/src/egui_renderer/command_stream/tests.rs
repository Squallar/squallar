//! What the census must count, held against hand-built primitive lists whose
//! recorded stream can be worked out on paper.

use egui::epaint::{Mesh, Primitive, Vertex};
use egui::{ClippedPrimitive, Color32, Pos2, Rect, TextureId};

use super::{CommandStream, census};

/// A 1920x1080 surface at one pixel per point — the geometry every case here
/// is reasoned about in.
const SURFACE: [u32; 2] = [1920, 1080];
const PPP: f32 = 1.0;

fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
    Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(x1, y1))
}

/// A mesh of `quads` quads on `texture`: four vertices and six indices each,
/// which is what a glyph or a filled rect tessellates to.
fn mesh(texture: TextureId, quads: u32) -> Primitive {
    let mut m = Mesh {
        texture_id: texture,
        ..Mesh::default()
    };
    for q in 0..quads {
        let base = q * 4;
        for v in 0..4u32 {
            m.vertices.push(Vertex {
                pos: Pos2::new(v as f32, q as f32),
                uv: Pos2::ZERO,
                color: Color32::WHITE,
            });
        }
        m.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    Primitive::Mesh(m)
}

fn clipped(clip: Rect, primitive: Primitive) -> ClippedPrimitive {
    ClippedPrimitive {
        clip_rect: clip,
        primitive,
    }
}

/// A paint callback over `rect`, of the shape `egui_wgpu` downcasts.
fn callback(at: Rect) -> Primitive {
    struct Nothing;
    impl egui_wgpu::CallbackTrait for Nothing {
        fn paint(
            &self,
            _: egui::PaintCallbackInfo,
            _: &mut egui_wgpu::wgpu::RenderPass<'static>,
            _: &egui_wgpu::CallbackResources,
        ) {
        }
    }
    Primitive::Callback(egui::epaint::PaintCallback {
        rect: at,
        callback: egui_wgpu::Callback::new_paint_callback(at, Nothing).callback,
    })
}

/// **One mesh records the six calls `render` makes for one mesh.**
///
/// The arithmetic on paper: the reset triple, the primitive's own scissor, its
/// texture bind group, its index and vertex binds, its draw, and the walk's
/// closing scissor. 3 + 1 + 1 + 2 + 1 + 1 = 9 calls for a one-mesh frame, of
/// which the closing scissor is unconditional overhead. Pinned as a value, not
/// a direction: a change that records a tenth call for one mesh is a change
/// the frame tail pays for on every primitive of every frame.
#[test]
fn one_mesh_records_nine_calls() {
    let c = census(
        &[clipped(
            rect(0.0, 0.0, 100.0, 100.0),
            mesh(TextureId::default(), 1),
        )],
        PPP,
        SURFACE,
    );
    assert_eq!(
        (c.primitives, c.meshes, c.callbacks, c.draws),
        (1, 1, 0, 1),
        "the primitive census miscounted a single mesh"
    );
    assert_eq!(
        (c.resets, c.scissor_sets, c.bind_group_sets, c.buffer_binds),
        (1, 2, 1, 2),
        "the per-mesh calls are not what `Renderer::render` issues for one mesh"
    );
    assert_eq!(c.calls, 9, "the walk's length moved for a one-mesh frame");
    assert_eq!(c.draw_indices, 6, "one quad is six indices");
}

/// **A callback pays no reset, and consecutive ones share one scissor.**
///
/// The two cuts this census exists to prove, on the shape that produces them
/// in bulk: the ground path issues one egui callback per tile-mesh run, so a
/// frame carrying the basemap is mostly a run of callbacks under one pane clip
/// rect. Upstream `render` re-established egui's viewport, pipeline and
/// uniform bind group before each of them — three commands a callback cannot
/// use, because egui's pipeline is undrawable until a mesh binds the buffers
/// and texture that go with it — and re-recorded the pane's scissor for each,
/// which neither `wgpu` layer drops.
///
/// Four callbacks now record five scissors and four viewports, and **no
/// resets at all**. Nine calls where the upstream walk recorded twenty-one.
/// The five scissors rather than one are not slack: a callback owns the pass
/// while it paints and may set a scissor of its own, so the walk drops what it
/// knows exactly where it raises `needs_reset`.
#[test]
fn a_callback_pays_no_reset_and_leaves_the_scissor_unknown() {
    let pane = rect(0.0, 40.0, 1920.0, 1080.0);
    let list: Vec<_> = (0..4).map(|_| clipped(pane, callback(pane))).collect();
    let c = census(&list, PPP, SURFACE);

    assert_eq!((c.callbacks, c.draws), (4, 0), "four callbacks, no meshes");
    assert_eq!(
        c.callback_viewports, 4,
        "every callback with a positive rect gets egui's courtesy viewport"
    );
    assert_eq!(
        c.resets, 0,
        "a callback was charged a reset it cannot use; that is 3 commands per \
         tile-mesh run back on the frame thread's replay"
    );
    assert_eq!(
        (c.scissor_sets, c.scissor_repeats),
        (5, 0),
        "each callback re-establishes the scissor because it may have changed \
         it, plus the walk's closing full-surface set"
    );
    assert_eq!(c.calls, 9, "the walk's length moved for four callbacks");
}

/// **A mesh after a callback pays the reset the callback deferred.**
///
/// Deferring the triple is only sound if it is still paid before anything
/// draws with it. This is the other half of the pair above: the callback
/// records none, and the mesh behind it records one — not zero, which would
/// draw egui's geometry through whatever pipeline the callback left bound.
#[test]
fn a_mesh_behind_a_callback_still_pays_the_reset() {
    let pane = rect(0.0, 40.0, 1920.0, 1080.0);
    let c = census(
        &[
            clipped(pane, mesh(TextureId::default(), 1)),
            clipped(pane, callback(pane)),
            clipped(pane, mesh(TextureId::default(), 1)),
        ],
        PPP,
        SURFACE,
    );
    assert_eq!(
        c.resets, 2,
        "the walk opens with one and the mesh behind the callback pays the \
         second; anything less draws egui geometry through a foreign pipeline"
    );
}

/// **A zero-area scissor is a primitive that was staged and never drawn.**
///
/// `render` skips it, advancing the buffer iterators without recording
/// anything, so its vertices and indices crossed to the GPU for nothing. The
/// census has to see it as skipped rather than drawn, or a frame full of
/// clipped-away work would read as a frame doing work.
#[test]
fn a_clip_rect_outside_the_surface_is_counted_skipped_not_drawn() {
    let c = census(
        &[
            clipped(rect(0.0, 0.0, 10.0, 10.0), mesh(TextureId::default(), 1)),
            // Entirely below the surface: clamps to zero height.
            clipped(
                rect(0.0, 2000.0, 10.0, 2010.0),
                mesh(TextureId::default(), 1),
            ),
        ],
        PPP,
        SURFACE,
    );
    assert_eq!((c.primitives, c.meshes), (2, 2));
    assert_eq!(
        c.skipped, 1,
        "the off-surface primitive was not seen skipped"
    );
    assert_eq!(c.draws, 1, "the off-surface primitive was drawn anyway");
    assert_eq!(
        c.buffer_binds, 2,
        "a skipped primitive must not be charged buffer binds it never makes"
    );
}

/// **Each primitive is attributed to the thing that opened it.**
///
/// `epaint`'s tessellator opens a new primitive on a clip-rect change, a
/// texture change, or a callback, and on nothing else. The census has to split
/// those three apart, because they have different remedies: a clip change is a
/// layout question, a texture change is an atlas question, and a callback is a
/// draw-path question.
#[test]
fn a_primitive_is_attributed_to_the_change_that_opened_it() {
    let a = rect(0.0, 0.0, 100.0, 100.0);
    let b = rect(200.0, 0.0, 300.0, 100.0);
    let font = TextureId::default();
    let user = TextureId::User(7);
    let c = census(
        &[
            clipped(a, mesh(font, 1)),
            // Same clip, different texture.
            clipped(a, mesh(user, 1)),
            // Different clip.
            clipped(b, mesh(user, 1)),
            // A callback on one side of the seam.
            clipped(b, callback(b)),
            clipped(b, mesh(user, 1)),
        ],
        PPP,
        SURFACE,
    );
    assert_eq!(
        (
            c.split_clip,
            c.split_texture,
            c.split_callback,
            c.split_none
        ),
        (2, 1, 2, 0),
        "the split attribution does not add up to the list that was built"
    );
    assert_eq!(
        c.split_clip + c.split_texture + c.split_callback + c.split_none,
        c.primitives,
        "every primitive must be attributed to exactly one cause"
    );
}

/// **A seam the tessellator could have merged is counted, and is normally zero.**
///
/// `tessellate_clipped_shape` merges into the previous primitive whenever the
/// clip rect and texture both match, so `split_none` cannot be filled at the
/// point primitives are built. It can only be filled afterwards, by the
/// `retain` that drops empty meshes and non-positive clip rects leaving two
/// mergeable neighbours adjacent. A non-zero reading is therefore a real merge
/// left on the table, and this pins that the bucket can hold one.
#[test]
fn two_mergeable_neighbours_are_counted_as_a_missed_merge() {
    let a = rect(0.0, 0.0, 100.0, 100.0);
    let font = TextureId::default();
    let c = census(
        &[clipped(a, mesh(font, 1)), clipped(a, mesh(font, 1))],
        PPP,
        SURFACE,
    );
    assert_eq!(c.split_none, 1, "an adjacent mergeable pair went unnoticed");
}

/// **A scissor repeat is a saving; a bind-group repeat was already free.**
///
/// The two look alike and are not. `wgpu-core` drops a `set_bind_group` whose
/// argument already holds (`StateChange::set_and_check_redundant`), so the
/// bind-group repeats never reached the encoder and removing them would buy
/// nothing. It drops no scissor at either layer, so every scissor repeat
/// upstream issued became a `glScissor` on the frame thread — and those are
/// what this walk now declines to record.
///
/// Three primitives under one clip rect on one texture therefore record **one**
/// scissor, not three, while still making three bind-group calls of which two
/// are dropped for free below.
#[test]
fn a_scissor_repeat_is_declined_where_a_bind_group_repeat_was_already_free() {
    let a = rect(0.0, 0.0, 100.0, 100.0);
    let font = TextureId::default();
    let c = census(
        &[
            clipped(a, mesh(font, 1)),
            clipped(a, mesh(font, 1)),
            clipped(a, mesh(font, 1)),
        ],
        PPP,
        SURFACE,
    );
    assert_eq!(
        (c.scissor_sets, c.scissor_repeats),
        (2, 2),
        "the run's one rect plus the closing full-surface set, and the two \
         repeats the walk declined to record"
    );
    assert_eq!(
        c.bind_group_repeats, 2,
        "three primitives on one texture still make three bind-group calls"
    );
    assert_eq!(
        c.recorded(),
        c.calls - 2,
        "`recorded` must drop exactly the bind-group repeats `wgpu-core` drops"
    );
}

/// **A reset invalidates the bound texture, so the bind after it is not a repeat.**
///
/// The reset re-establishes egui's pipeline and its own uniform bind group;
/// what the callback left in slot 1 is gone. A census that carried the texture
/// across a reset would report a repeat `wgpu-core` does not drop, and the
/// `recorded` figure would understate the stream.
#[test]
fn a_reset_clears_the_bound_texture() {
    let a = rect(0.0, 0.0, 100.0, 100.0);
    let font = TextureId::default();
    let c = census(
        &[
            clipped(a, mesh(font, 1)),
            clipped(a, callback(a)),
            clipped(a, mesh(font, 1)),
        ],
        PPP,
        SURFACE,
    );
    assert_eq!(
        c.bind_group_repeats, 0,
        "the bind after a callback's reset was counted as redundant"
    );
}

/// **The ledger's totals and its last walk are separate readings.**
///
/// A windowed rate divides the totals; "a scene-D frame records N" is the last
/// walk. Folding one into the other is how a figure taken across a layout
/// change gets quoted as a figure about a scene that was never on screen.
#[test]
fn the_ledger_keeps_the_totals_and_the_last_walk_apart() {
    let mut ledger = super::CommandStreamLedger::default();
    assert_eq!(ledger.totals(), CommandStream::default());
    assert_eq!(
        ledger.last().walks,
        0,
        "an unwalked ledger has no last walk"
    );

    let a = rect(0.0, 0.0, 100.0, 100.0);
    let one = census(&[clipped(a, mesh(TextureId::default(), 1))], PPP, SURFACE);
    let two = census(
        &[
            clipped(a, mesh(TextureId::default(), 1)),
            clipped(
                rect(200.0, 0.0, 300.0, 100.0),
                mesh(TextureId::default(), 1),
            ),
        ],
        PPP,
        SURFACE,
    );
    ledger.note(one);
    ledger.note(two);

    assert_eq!(ledger.totals().walks, 2);
    assert_eq!(
        ledger.totals().calls,
        one.calls + two.calls,
        "the totals are not the sum of the walks folded in"
    );
    assert_eq!(
        ledger.last(),
        two,
        "the last walk is not the walk last folded in"
    );
}

/// **The census's skip decision is the renderer's own, not a copy of it.**
///
/// `census` asks `egui_wgpu::scissor_rect_in_pixels` — the vendored renderer's
/// exported rounding — rather than rounding for itself, so the two cannot
/// disagree about which primitives were drawn. This holds it to that: a clip
/// rect that rounds to a one-pixel scissor must be drawn by both, and one that
/// rounds to zero must be skipped by both.
#[test]
fn the_census_takes_the_renderers_own_rounding() {
    // 0.4 point tall at 1 ppp rounds to zero height; 0.6 rounds to one.
    let vanishes = rect(0.0, 0.0, 10.0, 0.4);
    let survives = rect(0.0, 0.0, 10.0, 0.6);
    assert_eq!(
        egui_wgpu::scissor_rect_in_pixels(&vanishes, PPP, SURFACE)[3],
        0
    );
    assert_eq!(
        egui_wgpu::scissor_rect_in_pixels(&survives, PPP, SURFACE)[3],
        1
    );

    let c = census(
        &[
            clipped(vanishes, mesh(TextureId::default(), 1)),
            clipped(survives, mesh(TextureId::default(), 1)),
        ],
        PPP,
        SURFACE,
    );
    assert_eq!((c.skipped, c.draws), (1, 1));
}

/// **The census models the walk this workspace ships, not the one it forked from.**
///
/// `census` is a second implementation of `egui_wgpu::Renderer::render`'s
/// bookkeeping — it has to be, because `render` records into a `RenderPass`
/// that cannot be asked what it was told. Two implementations of one rule
/// drift, and a drifted census is worse than none: it would report a saving
/// the encoder never got. The vendored copy is in this workspace's own tree,
/// so the rules can at least be read back out of it, and this holds each of
/// the three the census depends on.
///
/// A rewrite that keeps the behaviour and moves the text will redden this.
/// That is the intended cost: re-reading `census` beside the new `render` is
/// exactly the check that has to happen, and re-pinning the text without doing
/// it is a choice somebody makes deliberately.
#[test]
fn the_vendored_walks_three_rules_are_the_ones_the_census_models() {
    const RENDER: &str = include_str!("../../../../vendor/egui-wgpu/src/renderer.rs");
    let walk = RENDER
        .split_once("    pub fn render(")
        .and_then(|(_, rest)| rest.split_once("\n    /// Should be called before"))
        .map(|(body, _)| body)
        .expect("`Renderer::render` is no longer where the census reads it from");

    // 1. The reset is inside the mesh arm, so a callback never pays one.
    let mesh_arm = walk
        .split_once("Primitive::Mesh(mesh) => {")
        .map(|(_, rest)| rest)
        .expect("`render`'s mesh arm moved");
    let callback_arm = walk
        .split_once("Primitive::Callback(callback) => {")
        .map(|(_, rest)| rest)
        .expect("`render`'s callback arm moved");
    assert!(
        mesh_arm.contains("if needs_reset {") && !callback_arm.contains("if needs_reset {"),
        "the reset triple is no longer paid by the mesh arm alone; `census` \
         charges resets only to drawn meshes and would now under-count them"
    );

    // 2. The scissor is recorded only when it differs from the one held.
    assert!(
        walk.contains("if scissor != Some(rect) {"),
        "`render` no longer declines a scissor the pass already holds; \
         `census` counts those as `scissor_repeats` it did not record"
    );

    // 3. A painted callback drops that knowledge, because it may have set one.
    assert!(
        callback_arm.contains("scissor = None;"),
        "a painted callback no longer clears the remembered scissor; `census` \
         assumes it does, and both would then skip a scissor the callback \
         changed underneath them"
    );
}
