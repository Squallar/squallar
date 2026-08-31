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
/// every fill at the wrong gamma, which the adapter-backed parity suite
/// catches — this is the same claim without a device, so a refactor that
/// drops the branch reddens on every board rather than only on the GPU one.
#[test]
fn the_entry_point_is_keyed_off_the_targets_srgb_ness() {
    let source = include_str!("../tile_mesh.rs");
    let body = source
        .split_once("let entry_point = if attachments.color_format.is_srgb() {")
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

/// Both entry points, and only those two, exist in the shader — so the choice
/// above cannot name one that silently is not there.
#[test]
fn the_shader_declares_both_entry_points_egui_declares() {
    let wgsl = include_str!("../tile_mesh.wgsl");
    for entry in ["fs_main_linear_framebuffer", "fs_main_gamma_framebuffer"] {
        assert!(
            wgsl.contains(&format!("fn {entry}(")),
            "the shader has no `{entry}`"
        );
    }
}
