//! What can be checked without an adapter. The picture itself is
//! `tests/tile_mesh_gpu.rs`, in the hardware quarantine.

use super::*;

/// The uniform block's byte layout is the WGSL `Locals` declaration's, lane
/// for lane. A shader reads this by offset, so a reordered field here is a
/// tile drawn at someone else's scale rather than a compile error.
#[test]
fn the_locals_block_lays_its_lanes_out_where_the_shader_reads_them() {
    let bytes = locals_bytes(
        [3440.0, 1440.0],
        Placement {
            scale: 0.0625,
            translation: [-11.5, 7.25],
        },
        true,
    );
    assert_eq!(bytes.len(), LOCALS_BYTES as usize);
    let lane = |i: usize| f32::from_ne_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
    assert_eq!([lane(0), lane(1)], [3440.0, 1440.0], "screen_size");
    assert_eq!([lane(2), lane(3)], [-11.5, 7.25], "translation");
    assert_eq!(lane(4), 0.0625, "scale");
    assert_eq!(
        u32::from_ne_bytes(bytes[20..24].try_into().unwrap()),
        1,
        "dithering"
    );
}

/// Dithering rides across as the bit it is: `false` must reach the shader as
/// zero, or every fill is dithered on an install where egui's are not.
#[test]
fn dithering_off_reaches_the_shader_as_zero() {
    let bytes = locals_bytes(
        [1.0, 1.0],
        Placement {
            scale: 1.0,
            translation: [0.0, 0.0],
        },
        false,
    );
    assert_eq!(u32::from_ne_bytes(bytes[20..24].try_into().unwrap()), 0);
}

/// Ring slots are spaced by the adapter's uniform alignment, never by less:
/// a dynamic offset that is not a multiple of it is a validation error at
/// draw time, on the device rather than here.
#[test]
fn ring_slots_are_spaced_by_the_adapters_alignment() {
    for alignment in [16u32, 32, 64, 256] {
        let stride = align_up(LOCALS_BYTES as u32, alignment);
        assert!(stride >= LOCALS_BYTES as u32);
        assert_eq!(stride % alignment, 0, "stride {stride} at {alignment}");
    }
    // A zero alignment is not a division by zero. Adapters report a positive
    // number; this is the arithmetic saying so rather than trusting it.
    assert_eq!(align_up(LOCALS_BYTES as u32, 0), LOCALS_BYTES as u32);
}

/// **The fragment entry point is chosen off the target's sRGB-ness, the way
/// egui chooses its own.** A pipeline built for the other convention draws
/// every fill and every stroke at the wrong gamma, which the adapter-backed
/// parity suite catches — this is the same claim without a device, so a
/// refactor that drops the branch reddens on every board rather than only on
/// the GPU one. The choice lives inside `build_pipeline`, which is what makes
/// it one answer for both pipelines rather than two that could diverge.
#[test]
fn the_entry_point_is_keyed_off_the_targets_srgb_ness() {
    let source = include_str!("../tile_mesh.rs");
    let body = source
        .split_once("let fragment_entry = if attachments.color_format.is_srgb() {")
        .map(|(_, rest)| rest.split_once("};").expect("the choice is a block").0)
        .expect("the entry-point choice no longer keys off `is_srgb`");
    let (srgb, plain) = body
        .split_once("} else {")
        .expect("the choice has two arms");
    assert!(
        srgb.contains("fs_main_linear_framebuffer"),
        "an sRGB target must take egui's linear-framebuffer entry point"
    );
    assert!(
        plain.contains("fs_main_gamma_framebuffer"),
        "a non-sRGB target must take egui's gamma-framebuffer entry point"
    );
}

/// Every entry point the two pipelines name exists in the shader — so a
/// choice above cannot name one that silently is not there.
#[test]
fn the_shader_declares_every_entry_point_the_pipelines_name() {
    let wgsl = include_str!("../tile_mesh.wgsl");
    for entry in [
        "fs_main_linear_framebuffer",
        "fs_main_gamma_framebuffer",
        "vs_main",
        "vs_stroke",
    ] {
        assert!(
            wgsl.contains(&format!("fn {entry}(")),
            "the shader has no `{entry}`"
        );
    }
}

/// **The stroke vertex adds its offset after the placement, never inside it.**
///
/// The whole design is that the offset is a *screen-point* quantity: scaling
/// it with the tile would make a road breathe through a zoom sweep, which is
/// exactly the defect `mvt::render_line`'s own comment records upstream having
/// shipped. A source-text assertion rather than a rendered one because the
/// rendered gate (`tests/tile_mesh_gpu.rs`) needs an adapter and this does
/// not, so a tree that folds the offset into the multiply reddens on every
/// board rather than only the ones with a GPU.
#[test]
fn the_stroke_shader_adds_the_offset_outside_the_scale() {
    let wgsl = include_str!("../tile_mesh.wgsl");
    assert!(
        wgsl.contains("r_locals.scale * vec2<f32>(a_pos) + r_locals.translation + a_offset"),
        "the stroke vertex shader no longer spells the placement as \
         `scale * pos + translation + offset`; an offset inside the scale is \
         a road that changes width with the tile side"
    );
}

/// The stroke vertex layout the pipeline declares is the one the flattener
/// writes: three attributes at 0, 4 and 12, in a stride of
/// [`stroke::STROKE_VERTEX_BYTES`].
///
/// A layout and a writer that disagree produce a picture rather than an
/// error, which is why this is stated twice and compared.
#[test]
fn the_stroke_vertex_layout_is_the_one_the_flattener_writes() {
    assert_eq!(
        stroke::STROKE_VERTEX_BYTES,
        wgpu::VertexFormat::Sint16x2.size()
            + wgpu::VertexFormat::Float32x2.size()
            + wgpu::VertexFormat::Uint32.size(),
        "the stride is not the three attributes end to end, so either the \
         flattener is padding or the pipeline is reading past a vertex"
    );
    assert_eq!(stroke::STROKE_INDEX_BYTES, 2, "a u16 index");
    // The `u16` index space is what bounds a run, and the two must agree or a
    // run can address a vertex the index cannot name.
    assert_eq!(stroke::STROKE_RUN_VERTICES, u32::from(u16::MAX) + 1);
}

/// A pass's placements are one write. Sixty-two draws — scene D's per-pass
/// count on 2026-09-04 — lay sixty-two padded slots into the batch, nothing
/// reaches the ring while they are gathered, and the flush is one write of
/// the whole range with each placement at its own slot's offset.
#[test]
fn a_pass_of_placements_reaches_the_ring_as_one_write() {
    const DRAWS: u32 = 62;
    const STRIDE: u32 = 256;
    let mut batch = PlacementBatch::new(STRIDE);
    let locals = |i: u32| {
        locals_bytes(
            [1920.0, 1080.0],
            Placement {
                scale: 0.5,
                translation: [i as f32, -(i as f32)],
            },
            false,
        )
    };
    let mut writes: Vec<(u64, Vec<u8>)> = Vec::new();
    for i in 0..DRAWS {
        let slot = batch.push(locals(i), |offset, bytes| {
            writes.push((offset, bytes.to_vec()))
        });
        assert_eq!(slot, i, "slots are handed out in order");
    }
    assert!(
        writes.is_empty(),
        "a placement was written before the pass was gathered"
    );

    assert!(batch.flush(|offset, bytes| writes.push((offset, bytes.to_vec()))));
    assert_eq!(writes.len(), 1, "the pass was not one write");
    let (offset, bytes) = &writes[0];
    assert_eq!(*offset, 0);
    assert_eq!(
        bytes.len(),
        (DRAWS * STRIDE) as usize,
        "every slot is padded to the stride"
    );
    for i in 0..DRAWS {
        let at = (i * STRIDE) as usize;
        assert_eq!(
            &bytes[at..at + LOCALS_BYTES as usize],
            &locals(i)[..],
            "placement {i} is not at its slot's offset"
        );
    }
    assert!(
        !batch.flush(|_, _| panic!("a second flush of the same pass wrote again")),
        "the batch was not emptied by its flush"
    );
}

/// The ring wraps inside a pass: the range gathered so far is no longer
/// contiguous with slot zero, so it goes out at the wrap and the rest of the
/// pass starts a new range at zero. Two writes, both at the offsets the slots
/// name — never one write that runs off the end of the ring.
#[test]
fn a_wrap_inside_a_pass_splits_it_into_two_contiguous_writes() {
    const STRIDE: u32 = 64;
    let mut batch = PlacementBatch::new(STRIDE);
    batch.cursor = RING_SLOTS - 2;
    let locals = locals_bytes(
        [1.0, 1.0],
        Placement {
            scale: 1.0,
            translation: [0.0, 0.0],
        },
        true,
    );
    let mut writes: Vec<(u64, usize)> = Vec::new();
    let slots: Vec<u32> = (0..3)
        .map(|_| batch.push(locals, |offset, bytes| writes.push((offset, bytes.len()))))
        .collect();
    assert_eq!(slots, [RING_SLOTS - 2, RING_SLOTS - 1, 0]);
    assert_eq!(
        writes,
        [(
            u64::from(RING_SLOTS - 2) * u64::from(STRIDE),
            2 * STRIDE as usize
        )],
        "the wrap did not write the range gathered before it"
    );
    assert!(batch.flush(|offset, bytes| writes.push((offset, bytes.len()))));
    assert_eq!(
        writes[1],
        (0, STRIDE as usize),
        "the rest of the pass does not start at slot zero"
    );
    let end = writes[0].0 + writes[0].1 as u64;
    assert_eq!(
        end,
        u64::from(RING_SLOTS) * u64::from(STRIDE),
        "the first write does not end at the ring's end"
    );
}
