//! What one frame's primitive list records into the command encoder.
//!
//! [`super::EguiRenderer::draw`] hands `egui_wgpu::Renderer::render` a
//! `&[ClippedPrimitive]` and it walks that slice issuing render-pass calls.
//! Every call it issues is a `wgpu-core` command pushed onto the pass's
//! `Vec<ArcRenderCommand>`, replayed into the HAL encoder by
//! `CommandEncoder::finish` and — on the GL backend — replayed a second time,
//! as real GL calls on the frame thread, by `Queue::submit`. So the length of
//! that walk is the length of the frame tail, and this counts it.
//!
//! # Why a count and not a clock
//!
//! Every other figure on this path is a clock, and the frame tail's clock is
//! already attributed: `queue.submit` is 93% of it. What no clock can say is
//! *what was submitted*. A command count is deterministic — the same scene
//! records the same number on a loaded box as on a quiet one — so a reduction
//! is provable at any load, which a 340 us noise floor makes impossible for a
//! timing.
//!
//! # The GL amplification, per figure
//!
//! `wgpu-core` drops a `set_bind_group` or `set_pipeline` whose argument
//! already holds (`StateChange::set_and_check_redundant`), so
//! [`CommandStream::bind_group_repeats`] is counted but never recorded.
//! **`set_scissor_rect`, `set_index_buffer` and `set_vertex_buffer` have no
//! such check** at either layer, so each reaches `wgpu-hal`. There a
//! `set_vertex_buffer` only dirties a mask, and the following draw's
//! `prepare_draw` turns it into one `SetVertexBuffer` where
//! `PrivateCapabilities::VERTEX_BUFFER_LAYOUT` holds (desktop GL 4.3 / ES 3.1
//! and up) and into **one `SetVertexAttribute` per vertex attribute** where it
//! does not. egui's vertex has three, and **WebGL2 is ES 3.0**, so a web leg
//! records three commands per draw where a native GL leg records one. Never
//! quote a command total without saying which.

use egui::epaint::Primitive;
use egui::{ClippedPrimitive, Rect};

/// Which side of the boundary a primitive is on, and what put it there.
///
/// `epaint`'s tessellator opens a new [`ClippedPrimitive`] only when the clip
/// rect changes, the texture changes, or a callback is involved
/// (`epaint-0.35.0/src/tessellator.rs`, `tessellate_clipped_shape`). Those
/// three are exhaustive at the point the primitives are built — so a fourth
/// bucket, [`CommandStream::split_none`], can only be filled by the `retain`
/// that runs afterwards dropping the primitive that stood between two
/// otherwise-mergeable neighbours. A non-zero reading there is a seam the
/// tessellator would have merged had it looked again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommandStream {
    /// Walks counted. The non-vacuity floor: `walks == 0` is a renderer that
    /// drew no pass, and every other figure here divides by this.
    pub walks: u64,

    /// `ClippedPrimitive`s the walk was handed.
    pub primitives: u64,
    /// Of those, `Primitive::Mesh`.
    pub meshes: u64,
    /// Of those, `Primitive::Callback`.
    pub callbacks: u64,
    /// Primitives whose scissor rect came out zero-area and were skipped —
    /// tessellated, staged into the vertex and index buffers, and then not
    /// drawn. Their bytes crossed to the GPU for nothing.
    pub skipped: u64,

    /// Primitives that opened because the clip rect changed.
    pub split_clip: u64,
    /// Primitives that opened because the texture changed under an unchanged
    /// clip rect.
    pub split_texture: u64,
    /// Primitives that opened because a callback was on one side of the seam.
    pub split_callback: u64,
    /// Primitives whose clip rect *and* texture match the primitive before
    /// them. See the type docs: a merge the tessellator left on the table.
    pub split_none: u64,

    /// `set_viewport` + `set_pipeline` + `set_bind_group(0)` triples: one
    /// before the first mesh drawn after the walk opens or a callback paints.
    /// A callback never pays one, because egui's pipeline is undrawable until
    /// a mesh binds the buffers and the texture that go with it.
    pub resets: u64,
    /// `set_scissor_rect` calls recorded: one per drawn primitive whose rect
    /// differs from the one the walk knows the pass holds, plus the
    /// unconditional full-surface set the walk closes with.
    pub scissor_sets: u64,
    /// Scissor sets the walk did **not** record because the rect already held.
    /// Not in [`Self::calls`]; it is the size of the saving, not a cost.
    /// Neither `wgpu` layer would have dropped these — see the module docs.
    pub scissor_repeats: u64,
    /// `set_bind_group(1, ..)` calls, one per drawn mesh.
    pub bind_group_sets: u64,
    /// Of those, ones whose texture was already bound. Dropped by `wgpu-core`
    /// before the encoder sees them.
    pub bind_group_repeats: u64,
    /// `set_index_buffer` + `set_vertex_buffer` calls: two per drawn mesh,
    /// always, because egui slices one buffer per mesh rather than binding it
    /// once and offsetting the draw.
    pub buffer_binds: u64,
    /// `draw_indexed` calls.
    pub draws: u64,
    /// Indices those draws covered. Over [`Self::draws`] it says whether the
    /// stream is a few large draws or many tiny ones.
    pub draw_indices: u64,
    /// `set_viewport` calls egui makes on a callback's behalf, one per painted
    /// callback, immediately before handing the pass over.
    pub callback_viewports: u64,

    /// Every call above, as egui issues it — the walk's own length. See the
    /// module docs for which of them `wgpu-core` then drops and which the GL
    /// backend multiplies.
    pub calls: u64,
}

impl CommandStream {
    /// The calls that survive `wgpu-core`'s redundancy checks and reach the
    /// HAL encoder. [`Self::calls`] less the bind-group repeats it drops.
    pub fn recorded(&self) -> u64 {
        self.calls.saturating_sub(self.bind_group_repeats)
    }
}

/// Count what `egui_wgpu::Renderer::render` will record for `primitives`.
///
/// Walks the same slice, in the same order, taking the same two decisions:
/// a zero-area scissor skips the primitive, and a painted callback forces the
/// next primitive to re-establish egui's state. `scissor` is the vendored
/// renderer's own rounding, borrowed rather than re-derived so the skip
/// decision here cannot disagree with the one there.
pub fn census(
    primitives: &[ClippedPrimitive],
    pixels_per_point: f32,
    size_in_pixels: [u32; 2],
) -> CommandStream {
    let mut c = CommandStream {
        walks: 1,
        primitives: primitives.len() as u64,
        ..CommandStream::default()
    };

    // What the previous primitive in the list was, for the split attribution.
    let mut prev: Option<(Rect, Option<egui::TextureId>)> = None;
    // What the pass currently holds, for the redundancy attribution.
    let mut bound_scissor: Option<[u32; 4]> = None;
    let mut bound_texture: Option<egui::TextureId> = None;
    let mut needs_reset = true;

    for clipped in primitives {
        let texture = match &clipped.primitive {
            Primitive::Mesh(mesh) => {
                c.meshes += 1;
                Some(mesh.texture_id)
            }
            Primitive::Callback(_) => {
                c.callbacks += 1;
                None
            }
        };

        match (prev, texture) {
            (None, _) => c.split_clip += 1,
            (Some((_, None)), _) | (Some(_), None) => c.split_callback += 1,
            (Some((prev_clip, Some(prev_tex))), Some(tex)) => {
                if prev_clip != clipped.clip_rect {
                    c.split_clip += 1;
                } else if prev_tex != tex {
                    c.split_texture += 1;
                } else {
                    c.split_none += 1;
                }
            }
        }
        prev = Some((clipped.clip_rect, texture));

        let rect = scissor(clipped.clip_rect, pixels_per_point, size_in_pixels);
        if rect[2] == 0 || rect[3] == 0 {
            c.skipped += 1;
            continue;
        }
        if bound_scissor == Some(rect) {
            c.scissor_repeats += 1;
        } else {
            c.scissor_sets += 1;
            bound_scissor = Some(rect);
        }

        match &clipped.primitive {
            Primitive::Mesh(mesh) => {
                if needs_reset {
                    c.resets += 1;
                    // A reset re-establishes egui's own pipeline and uniform
                    // bind group, so whatever a callback left bound is gone.
                    bound_texture = None;
                    needs_reset = false;
                }
                c.bind_group_sets += 1;
                if bound_texture == Some(mesh.texture_id) {
                    c.bind_group_repeats += 1;
                }
                bound_texture = Some(mesh.texture_id);
                c.buffer_binds += 2;
                c.draws += 1;
                c.draw_indices += mesh.indices.len() as u64;
            }
            Primitive::Callback(callback) => {
                // egui skips a callback whose rect is degenerate in pixels,
                // and only a painted one costs a viewport, forces a reset or
                // takes the scissor out of the walk's knowledge.
                let viewport = scissor(callback.rect, pixels_per_point, size_in_pixels);
                if viewport[2] > 0 && viewport[3] > 0 {
                    c.callback_viewports += 1;
                    needs_reset = true;
                    bound_scissor = None;
                }
            }
        }
    }

    // The walk closes by restoring a full-surface scissor, unconditionally:
    // that is the pass's exit contract, not a draw's setup.
    c.scissor_sets += 1;

    c.calls = c.resets * 3
        + c.scissor_sets
        + c.bind_group_sets
        + c.buffer_binds
        + c.draws
        + c.callback_viewports;
    c
}

/// The vendored renderer's clip-rect-to-scissor rounding, as `[x, y, w, h]`.
/// Borrowed from `egui_wgpu` rather than restated: this decides which
/// primitives the census calls skipped, and a private copy that rounded
/// differently would report a skip the renderer does not take.
fn scissor(clip: Rect, pixels_per_point: f32, size_in_pixels: [u32; 2]) -> [u32; 4] {
    egui_wgpu::scissor_rect_in_pixels(&clip, pixels_per_point, size_in_pixels)
}

/// The renderer's single-writer ledger: running totals over every walk, and
/// the most recent walk on its own.
///
/// Both are kept because they answer different questions and neither can be
/// got from the other. The totals are what a windowed subtraction reads
/// (`(calls_b - calls_a) / (walks_b - walks_a)`); the last walk is what
/// "a scene-D frame records N commands" means, and averaging a window that
/// spans a layout change answers about a scene that was never on screen.
#[derive(Default)]
pub(super) struct CommandStreamLedger {
    totals: CommandStream,
    last: CommandStream,
}

impl CommandStreamLedger {
    /// Fold one walk in.
    pub(super) fn note(&mut self, walk: CommandStream) {
        let t = &mut self.totals;
        t.walks += walk.walks;
        t.primitives += walk.primitives;
        t.meshes += walk.meshes;
        t.callbacks += walk.callbacks;
        t.skipped += walk.skipped;
        t.split_clip += walk.split_clip;
        t.split_texture += walk.split_texture;
        t.split_callback += walk.split_callback;
        t.split_none += walk.split_none;
        t.resets += walk.resets;
        t.scissor_sets += walk.scissor_sets;
        t.scissor_repeats += walk.scissor_repeats;
        t.bind_group_sets += walk.bind_group_sets;
        t.bind_group_repeats += walk.bind_group_repeats;
        t.buffer_binds += walk.buffer_binds;
        t.draws += walk.draws;
        t.draw_indices += walk.draw_indices;
        t.callback_viewports += walk.callback_viewports;
        t.calls += walk.calls;
        self.last = walk;
    }

    /// Running totals over every walk.
    pub(super) fn totals(&self) -> CommandStream {
        self.totals
    }

    /// The most recent walk alone; all zero before the first.
    pub(super) fn last(&self) -> CommandStream {
        self.last
    }
}

#[cfg(test)]
#[path = "command_stream/tests.rs"]
mod tests;
