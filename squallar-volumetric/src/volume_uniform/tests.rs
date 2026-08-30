use super::*;

/// Decode the packed block back to lanes, for assertions about offsets.
fn lanes(bytes: &[u8; VOLUME_UNIFORM_BYTES]) -> [f32; VOLUME_UNIFORM_LANES] {
    let mut out = [0.0; VOLUME_UNIFORM_LANES];
    for (lane, slot) in out.iter_mut().enumerate() {
        let start = lane * 4;
        *slot = f32::from_le_bytes(
            <[u8; 4]>::try_from(&bytes[start..start + 4]).expect("four bytes per lane"),
        );
    }
    out
}

/// A uniform whose every lane is a distinct, recognisable number.
fn distinct() -> VolumeUniform {
    let mut matrix = [[0.0f32; 4]; 4];
    let mut forward = [[0.0f32; 4]; 4];
    for (column, (values, forward)) in matrix.iter_mut().zip(forward.iter_mut()).enumerate() {
        for (row, (slot, forward)) in values.iter_mut().zip(forward.iter_mut()).enumerate() {
            // Column-major, so the lane index is column * 4 + row, and the
            // value says which is which: 10 * column + row.
            *slot = (column * 10 + row) as f32;
            // The second matrix's own recognisable numbers, offset far enough
            // that a lane read out of the wrong block is unmistakable.
            *forward = (1200 + column * 10 + row) as f32;
        }
    }
    VolumeUniform {
        box_from_clip: matrix,
        eye_in_box: [101.0, 102.0, 103.0],
        box_size_km: [201.0, 202.0, 203.0],
        vertical_exaggeration: 204.0,
        grid_dims: [301, 302, 303],
        light_dir: [401.0, 402.0, 403.0],
        ambient: 404.0,
        extinction_per_km: 501.0,
        empty_index_threshold: 502.0,
        early_out_transmittance: 503.0,
        edge_soft_width: 504.0,
        gradient_shading: true,
        step_cells: 602.0,
        reconstruction_lod: 601.0,
        map_floor: true,
        iso_threshold: 104.0,
        iso_centre: 304.0,
        floor_uv: [701.0, 702.0, 703.0, 704.0],
        floor_geo: [801.0, 802.0, 803.0, 804.0],
        grid_from_box_scale: [901.0, 902.0, 903.0],
        grid_bounded: true,
        grid_from_box_offset: [1001.0, 1002.0, 1003.0],
        clip_from_box: forward,
        occluder_t_scale: 1301.0,
        ground_max_z: 1302.0,
        height_scale: 1303.0,
        height_offset: 1304.0,
        ground_box: [1401.0, 1402.0, 1403.0, 1404.0],
    }
}

/// The block is exactly 320 bytes, and the shader declares the same.
#[test]
fn the_block_is_two_mat4s_and_twelve_vec4s_on_both_sides() {
    assert_eq!(VOLUME_UNIFORM_BYTES, 2 * 64 + 12 * 16);
    assert_eq!(OFFSET_GROUND_BOX + 16, VOLUME_UNIFORM_BYTES);
    // The growth is append-only: every offset that existed before the ground
    // pass is still where it was, which is what let the block grow twice —
    // 224 to 304 at B1, 304 to 320 at B3 — without a single lane of the march
    // moving.
    assert_eq!(OFFSET_GRID_FROM_BOX_B + 16, OFFSET_CLIP_FROM_BOX);
    assert_eq!(OFFSET_CLIP_FROM_BOX + 64, OFFSET_OCCLUDER);
    assert_eq!(OFFSET_OCCLUDER + 16, OFFSET_GROUND_BOX);

    let source = include_str!("../volume.wgsl");
    let declaration = source
        .split_once("struct Volume {")
        .and_then(|(_, rest)| rest.split_once('}'))
        .map(|(body, _)| body)
        .expect("volume.wgsl no longer declares `struct Volume`");

    let mat4s = declaration.matches("mat4x4<f32>").count();
    let vec4s = declaration.matches("vec4<f32>").count();
    assert_eq!(
        (mat4s, vec4s),
        (2, 12),
        "volume.wgsl's uniform block is {mat4s} mat4x4 and {vec4s} vec4, \
             which is {} bytes, not the {VOLUME_UNIFORM_BYTES} this file packs. \
             A block smaller than the buffer is legal, so nothing would report \
             the mismatch — every member past the change would simply read the \
             wrong lane.",
        mat4s * 64 + vec4s * 16
    );
}

/// The declaration order in the WGSL is the order this file packs.
#[test]
fn the_shader_declares_the_members_in_the_order_this_file_packs_them() {
    let source = include_str!("../volume.wgsl");
    let declaration = source
        .split_once("struct Volume {")
        .and_then(|(_, rest)| rest.split_once('}'))
        .map(|(body, _)| body)
        .expect("volume.wgsl no longer declares `struct Volume`");

    let mut at = 0usize;
    for member in [
        "box_from_clip",
        "eye_in_box",
        "box_size_km",
        "grid_dims",
        "light_dir_ambient",
        "transfer",
        "flags",
        "floor_uv",
        "floor_geo",
        "grid_from_box_a",
        "grid_from_box_b",
        "clip_from_box",
        "occluder",
        "ground_box",
    ] {
        let needle = format!("{member}:");
        let found = declaration[at..].find(&needle).unwrap_or_else(|| {
            panic!(
                "volume.wgsl's uniform block does not declare `{member}` \
                     after the members before it; the shader's order no longer \
                     matches the byte offsets this file writes"
            )
        });
        at += found + needle.len();
    }
}

/// Every lane lands at its documented std140 offset.
#[test]
fn every_lane_lands_at_its_std140_offset() {
    let packed = lanes(&distinct().to_bytes());

    // Column-major: column c occupies lanes 4c..4c+4.
    assert_eq!(
        &packed[0..16],
        &[
            0.0, 1.0, 2.0, 3.0, // column 0
            10.0, 11.0, 12.0, 13.0, // column 1
            20.0, 21.0, 22.0, 23.0, // column 2
            30.0, 31.0, 32.0, 33.0, // column 3
        ],
        "box_from_clip is not written column-major; WGSL's mat4x4 and \
             std140 both are, so a transpose here rotates the camera's axes"
    );

    // The offsets themselves, as literals.
    // An array rather than a tuple: fourteen members is past the arity `Debug`
    // is implemented for, and a failure that cannot print the two sides is a
    // failure a reader cannot act on.
    assert_eq!(
        [
            OFFSET_BOX_FROM_CLIP,
            OFFSET_EYE_IN_BOX,
            OFFSET_BOX_SIZE_KM,
            OFFSET_GRID_DIMS,
            OFFSET_LIGHT_DIR_AMBIENT,
            OFFSET_TRANSFER,
            OFFSET_FLAGS,
            OFFSET_FLOOR_UV,
            OFFSET_FLOOR_GEO,
            OFFSET_GRID_FROM_BOX_A,
            OFFSET_GRID_FROM_BOX_B,
            OFFSET_CLIP_FROM_BOX,
            OFFSET_OCCLUDER,
            OFFSET_GROUND_BOX,
        ],
        [
            0, 64, 80, 96, 112, 128, 144, 160, 176, 192, 208, 224, 288, 304
        ],
        "the std140 offsets have moved. They are the layout the WGSL's \
             `struct Volume` declares, in its declaration order, and nothing \
             else in this file can tell you they are wrong."
    );

    for (offset, expected, member) in [
        (
            OFFSET_EYE_IN_BOX,
            [101.0, 102.0, 103.0, 104.0],
            "eye_in_box + iso_threshold",
        ),
        (
            OFFSET_BOX_SIZE_KM,
            [201.0, 202.0, 203.0, 204.0],
            "box_size_km + vertical_exaggeration",
        ),
        (
            OFFSET_GRID_DIMS,
            [301.0, 302.0, 303.0, 304.0],
            "grid_dims + iso_centre",
        ),
        (
            OFFSET_LIGHT_DIR_AMBIENT,
            [401.0, 402.0, 403.0, 404.0],
            "light_dir_ambient",
        ),
        (OFFSET_TRANSFER, [501.0, 502.0, 503.0, 504.0], "transfer"),
        (OFFSET_FLAGS, [1.0, 601.0, 602.0, 1.0], "flags"),
        (OFFSET_FLOOR_UV, [701.0, 702.0, 703.0, 704.0], "floor_uv"),
        (OFFSET_FLOOR_GEO, [801.0, 802.0, 803.0, 804.0], "floor_geo"),
        (
            OFFSET_GRID_FROM_BOX_A,
            [901.0, 902.0, 903.0, 1.0],
            "grid_from_box_scale + grid_bounded",
        ),
        (
            OFFSET_GRID_FROM_BOX_B,
            [1001.0, 1002.0, 1003.0, 0.0],
            "grid_from_box_offset + a reserved zero",
        ),
        (
            OFFSET_OCCLUDER,
            [1301.0, 1302.0, 1303.0, 1304.0],
            "occluder_t_scale + ground_max_z + the height affine",
        ),
        (
            OFFSET_GROUND_BOX,
            [1401.0, 1402.0, 1403.0, 1404.0],
            "ground_box",
        ),
    ] {
        let lane = offset / 4;
        assert_eq!(
            &packed[lane..lane + 4],
            &expected,
            "`{member}` is not at byte {offset}"
        );
    }
}

/// The isosurface pair defaults to the negative sentinels that select the
/// lit-volume march.
#[test]
fn the_iso_lanes_default_to_the_lit_volume_sentinels() {
    let uniform = VolumeUniform::new([240.0, 240.0, 20.0], [128, 128, 64]);
    assert!(
        uniform.iso_threshold < 0.0 && uniform.iso_centre < 0.0,
        "the default must be the lit volume, selected by a negative \
             sentinel — zero is a real threshold",
    );
    let packed = lanes(&uniform.to_bytes());
    assert_eq!(packed[OFFSET_EYE_IN_BOX / 4 + 3], ISO_OFF);
    assert_eq!(packed[OFFSET_GRID_DIMS / 4 + 3], ISO_OFF);
    assert!(
        include_str!("../volume.wgsl").contains("volume.eye_in_box.w >= 0.0"),
        "the shader no longer selects the isosurface march on the \
             threshold lane's sign, so the sentinel selects nothing",
    );
}

/// A fresh uniform draws the grid in its own box: scale 1, offset 0, no
/// bounds test.
#[test]
fn a_fresh_uniform_draws_the_grid_in_its_own_box() {
    let uniform = VolumeUniform::new([240.0, 240.0, 20.0], [128, 128, 64]);
    assert_eq!(
        (
            uniform.grid_from_box_scale,
            uniform.grid_from_box_offset,
            uniform.grid_bounded
        ),
        (IDENTITY_GRID_FROM_BOX.0, IDENTITY_GRID_FROM_BOX.1, false),
    );

    let packed = lanes(&uniform.to_bytes());
    let a = OFFSET_GRID_FROM_BOX_A / 4;
    let b = OFFSET_GRID_FROM_BOX_B / 4;
    assert_eq!(&packed[a..a + 4], &[1.0, 1.0, 1.0, 0.0]);
    assert_eq!(&packed[b..b + 4], &[0.0, 0.0, 0.0, 0.0]);

    let shader = include_str!("../volume.wgsl");
    assert!(
        shader.contains("return p * volume.grid_from_box_a.xyz + volume.grid_from_box_b.xyz;"),
        "the shader no longer applies the affine, so a pane standing in for a \
             build would draw its held grid stretched across the requested box \
             instead of cropped to it",
    );
    assert!(
        shader.contains("volume.grid_from_box_a.w > 0.5"),
        "the shader no longer gates the bounds test on the flag: unconditional \
             it costs the identity path a discarded exit sample, and absent \
             altogether a zoomed-out pane smears the grid's rim across ground \
             the radar never reported",
    );
}

/// The shading flag is 1.0 or 0.0, and the shader's threshold sits between.
#[test]
fn the_shading_flag_is_one_or_zero() {
    let mut uniform = distinct();

    uniform.gradient_shading = true;
    assert_eq!(lanes(&uniform.to_bytes())[OFFSET_FLAGS / 4], 1.0);

    uniform.gradient_shading = false;
    assert_eq!(lanes(&uniform.to_bytes())[OFFSET_FLAGS / 4], 0.0);

    assert!(
        include_str!("../volume.wgsl").contains("volume.flags.x > 0.5"),
        "the shader no longer tests the shading flag against 0.5, so the \
             1.0/0.0 this file writes may no longer select what it selects"
    );
}

/// The reconstruction LOD rides `flags.y`, and the uniform's default is
/// the raw field.
#[test]
fn the_reconstruction_lod_rides_flags_y_and_defaults_to_the_raw_field() {
    let mut uniform = distinct();

    uniform.reconstruction_lod = 0.75;
    assert_eq!(lanes(&uniform.to_bytes())[OFFSET_FLAGS / 4 + 1], 0.75);

    assert_eq!(
        VolumeUniform::new([240.0, 240.0, 20.0], [128, 128, 64]).reconstruction_lod,
        0.0,
        "the uniform's default must be the raw trilinear field — the \
             instrument configuration — with the production softness a \
             decision in volume::bridge",
    );
    assert!(
        include_str!("../volume.wgsl").contains("volume.flags.y).r"),
        "the shader no longer samples the grid at the flags.y level, so \
             this lane has stopped selecting the reconstruction",
    );
}

/// There is no nearest sentinel any more, and the shader has no sign
/// branch to select one with.
#[test]
fn no_negative_reconstruction_sentinel_survives_in_the_lane_or_the_shader() {
    let shader = include_str!("../volume.wgsl");
    assert!(
        !shader.contains("volume.flags.y < 0.0"),
        "the shader branches on flags.y's sign again: the nearest path is \
             back, and with it the per-product reconstruction split the \
             coverage channel retired",
    );
    assert!(
        shader.contains("texel.r / max(texel.g, COVERAGE_EPSILON)"),
        "the shader no longer reconstructs the index as premultiplied over \
             coverage, which is the whole of the honesty argument",
    );
    // And nothing in the crate writes a negative level into the lane.
    let uniform = VolumeUniform::new([1.0, 1.0, 1.0], [2, 2, 2]);
    assert!(uniform.reconstruction_lod >= 0.0);
    assert!(
        crate::bridge::CLOUD_RECONSTRUCTION_LOD >= 0.0
            && (0..=40)
                .map(|tenths| crate::bridge::cloud_reconstruction_lod_for(tenths as f32 / 10.0))
                .all(|lod| lod >= 0.0),
        "a production writer produced a negative reconstruction level, which \
             the shader would now sample the grid at rather than treat as a \
             mode",
    );
}

/// Grid dimensions cross as floats, not as integers reinterpreted.
#[test]
fn the_grid_dimensions_cross_as_floats() {
    let uniform = VolumeUniform::new([240.0, 240.0, 20.0], [256, 256, 128]);
    let packed = lanes(&uniform.to_bytes());
    let lane = OFFSET_GRID_DIMS / 4;
    assert_eq!(&packed[lane..lane + 3], &[256.0, 256.0, 128.0]);
}

/// `new` produces a uniform whose defaults the shader can actually march.
#[test]
fn the_defaults_are_a_marchable_configuration() {
    let uniform = VolumeUniform::new([240.0, 240.0, 20.0], [128, 128, 64]);
    assert!(uniform.grid_dims.iter().all(|&n| n > 0));
    assert!(uniform.box_size_km.iter().all(|&km| km > 0.0));
    assert!(
        uniform.vertical_exaggeration >= 1.0,
        "the default exaggeration must be the identity stretch, and never \
             zero — the shading divides a cell extent by it",
    );
    assert!(uniform.extinction_per_km > 0.0);
    assert!((0.0..1.0).contains(&uniform.early_out_transmittance));
    assert!((0.0..=1.0).contains(&uniform.ambient));
    assert!(uniform.light_dir.iter().any(|&c| c != 0.0));
    assert_eq!(uniform.box_from_clip, IDENTITY);
    assert_eq!(uniform.clip_from_box, IDENTITY);
    assert_eq!(
        uniform.occluder_t_scale, 0.0,
        "a fresh uniform must have the occluder OFF: zero is the sentinel the \
         march tests, and a non-zero scale over a placeholder texture would \
         have it clipping every ray against ground that does not exist",
    );
}

/// **The `t` scale is an over-estimate, and the whole mesh fits inside it.**
///
/// A `t` clamped to 1 by the packing decodes to the scale, so the scale must be
/// past the far side of the box for the march's `min` to be a no-op rather than
/// a wall. Checked over the posts themselves, not argued from the corners.
#[test]
fn the_t_scale_covers_every_post_of_the_grid() {
    // Eyes outside the box, inside it, and on a face — the third is the one an
    // eye-relative derivation is most likely to get wrong.
    for eye in [
        [0.5f32, -1.9, 1.2],
        [3.0, 3.0, 3.0],
        [0.5, 0.5, 0.5],
        [0.0, 0.0, 0.0],
        [-0.2, 1.1, 0.03],
    ] {
        let scale = VolumeUniform::t_scale_for(eye);
        assert!(scale > 0.0 && scale.is_finite());
        let posts = 64;
        let mut worst_ratio = 0.0f32;
        for j in 0..=posts {
            for i in 0..=posts {
                let uv = [i as f32 / posts as f32, j as f32 / posts as f32];
                // Every box z a decoded height sample can produce, **including
                // ones past the top of the box**: the affine is two plain `f32`
                // lanes a caller can set to anything, a field really can reach
                // above a box floored 100 m under its minimum, and the shader's
                // decode clamps into the cube so that the corner bound below
                // stays sound. Stopping this sweep at 1.0 would test only the
                // precondition, not the clamp that enforces it.
                for raw_z in [0.0f32, 0.25, 0.5, 1.0, 1.2, 1.5, 4.0, 1e6] {
                    let p = [uv[0], uv[1], raw_z.clamp(0.0, 1.0)];
                    let t = ((p[0] - eye[0]).powi(2)
                        + (p[1] - eye[1]).powi(2)
                        + (p[2] - eye[2]).powi(2))
                    .sqrt();
                    worst_ratio = worst_ratio.max(t / scale);
                }
            }
        }
        assert!(
            worst_ratio < 1.0,
            "a post is {worst_ratio} of the way to the {scale} scale from an \
             eye at {eye:?}; a post at or past the scale saturates the packing \
             and decodes SHORT of where it is, which clips the march early",
        );

        // And the slack is the margin, not luck. The bound is the farthest
        // *corner* — the unit cube is convex and `|p - eye|` is convex, so its
        // maximum over the cube is at a vertex, and every post is inside the
        // cube by construction. A post need not reach that corner, which is
        // why the tightness is asserted here and not on the posts above.
        let farthest_corner = (0..8u32)
            .map(|corner| {
                let p = [
                    (corner & 1) as f32,
                    ((corner >> 1) & 1) as f32,
                    ((corner >> 2) & 1) as f32,
                ];
                ((p[0] - eye[0]).powi(2) + (p[1] - eye[1]).powi(2) + (p[2] - eye[2]).powi(2)).sqrt()
            })
            .fold(0.0f32, f32::max);
        let margin = scale / farthest_corner;
        assert!(
            (margin - T_SCALE_MARGIN).abs() < 1e-5,
            "the scale is {margin} times the farthest corner from an eye at \
             {eye:?}, not the {T_SCALE_MARGIN} the constant names — either the \
             derivation stopped being corner-based, or the margin is slack \
             nobody chose and the packing is spending digits on empty space",
        );
    }
}

/// **The two coupled occluder lanes are checked where a uniform reaches the
/// GPU, and the check is not vacuous.**
#[test]
fn an_occluder_scale_from_another_eye_is_recognised_as_one() {
    let mut uniform = VolumeUniform::new([240.0, 240.0, 20.0], [8, 8, 8]);
    uniform.eye_in_box = [0.5, -1.9, 1.2];
    assert!(
        uniform.occluder_is_aimed_at_its_own_eye(),
        "the occluder is off, which is the zero sentinel, and off must count as \
         aimed or every pane without a ground pass would trip the check",
    );

    uniform.aim_occluder(0.25, 1.0 / 65_535.0, 0.0);
    assert!(
        uniform.occluder_is_aimed_at_its_own_eye(),
        "a scale set through `aim_occluder` is not recognised as this eye's own",
    );
    assert!(uniform.occluder_t_scale > 0.0);

    // The eye moves and the scale does not — the exact shape of the defect,
    // and the reason the two are checked against each other at all.
    uniform.eye_in_box = [0.5, 4.0, 3.0];
    assert!(
        !uniform.occluder_is_aimed_at_its_own_eye(),
        "a scale left over from another eye read as aimed, so the check at \
         `write_uniform` would pass a frame whose every ray clips at the wrong \
         depth",
    );
    uniform.aim_occluder(0.25, 1.0 / 65_535.0, 0.0);
    assert!(uniform.occluder_is_aimed_at_its_own_eye());
}

/// **Aiming the occluder puts the flat lid out, and the two cannot both be on.**
///
/// The mesh IS the ground. Holding both painted the lid behind the march at
/// full opacity wherever a ray crossed `z = 0` without meeting the mesh — B1
/// measured 76, 74 and 33 such pixels at the three below-floor cameras, at
/// alpha above 200. `aim_occluder` is the one blessed way to turn a ground pass
/// on, so clearing `map_floor` there is what makes the pair unbuildable in the
/// order production builds it in.
#[test]
fn aiming_the_occluder_puts_the_map_lid_out() {
    let mut uniform = VolumeUniform::new([240.0, 240.0, 20.0], [8, 8, 8]);
    uniform.eye_in_box = [0.5, -1.9, 1.2];
    uniform.map_floor = true;
    assert!(
        uniform.ground_is_one_surface(),
        "a lid with no ground pass is one surface, and this must say so or the predicate would refuse every pane shipping today",
    );

    uniform.aim_occluder(0.25, 1.0 / 65_535.0, 0.0);
    assert!(
        !uniform.map_floor,
        "aiming the occluder left the lid on; the frame would hold two grounds",
    );
    assert!(uniform.ground_is_one_surface());

    // The non-triviality half: the predicate really is reading the pair rather
    // than being true of everything. Setting the lid back on AFTER the aim is
    // the one order that reaches the defect, and it is what the predicate is
    // for.
    uniform.map_floor = true;
    assert!(
        !uniform.ground_is_one_surface(),
        "a lid set back on after the aim read as one surface, so nothing would notice a caller building the pair in that order",
    );
}

/// The height affine is one derivation, and it is the field's own encoding
/// carried into the drawn box's own z range.
#[test]
fn the_height_affine_turns_a_raw_sample_into_the_box_it_stands_in() {
    // A real box: floor at sea level, top at 18 km MSL, and the shipped
    // `HeightField` encoding.
    let (scale, offset) = VolumeUniform::height_affine(-500.0, 0.25, (0.0, 18.0))
        .expect("a box with vertical extent");
    let box_z = |raw: u16| f64::from(raw) * f64::from(scale) + f64::from(offset);

    // Sea level is raw 2000 (`-500 + 2000 * 0.25 == 0`), which must land on the
    // box floor exactly.
    assert!(
        box_z(2000).abs() < 1e-6,
        "sea level does not land on a box floored at 0 km MSL: {}",
        box_z(2000),
    );
    // Denver, 1609 m: raw 8436, and 1.609 / 18 of the way up the box.
    let denver = ((1609.0 + 500.0) / 0.25) as u16;
    assert!(
        (box_z(denver) - 1.609 / 18.0).abs() < 1e-4,
        "1609 m lands at box z {}, not {}",
        box_z(denver),
        1.609 / 18.0,
    );
    // A box that does not start at sea level shifts, and does not rescale.
    let (_, raised) = VolumeUniform::height_affine(-500.0, 0.25, (1.0, 18.0))
        .expect("a box with vertical extent");
    assert!(
        (f64::from(raised) - (-0.5 - 1.0) / 17.0).abs() < 1e-6,
        "a base of 1 km MSL did not move the encoding's own zero with it",
    );

    assert_eq!(
        VolumeUniform::height_affine(-500.0, 0.25, (18.0, 18.0)),
        None,
        "a box with no vertical extent must be refused rather than divided by; the quotient reaches the GPU as an infinity and the matrix after it as NaN",
    );
}

/// The shader reads the scale off the lane this file writes it to, and treats
/// zero as "no ground pass".
///
/// **Anchored inside `ground_covered`, not on a bare `contains` of the
/// comparison.** B2 gave the composite two more readers of the same lane — the
/// coverage and the arm — so a whole-file `contains` went from one occurrence
/// to three and stopped fencing its own defect: measured, deleting
/// `volume.occluder.x > 0.0 && ` from `ground_covered` left this test and the
/// entire default `cargo test` row green, caught only by the `#[ignore]`d GPU
/// suites the default row skips. The gate a default row cannot fail is not a
/// gate.
#[test]
fn the_shader_gates_the_occluder_on_the_scale_lane_being_positive() {
    let shader = include_str!("../volume.wgsl");
    let covered = shader
        .split_once("fn ground_covered(")
        .and_then(|(_, rest)| rest.split_once('}').map(|(body, _)| body))
        .expect(
            "the march no longer decides ground coverage in `ground_covered`; \
             re-anchor this on wherever that moved to rather than widening it \
             back to a whole-file search",
        );
    assert!(
        covered.contains("volume.occluder.x > 0.0"),
        "`ground_covered` no longer gates the occluder read on a positive \
         scale, so every pane would read the group-2 placeholder as though a \
         ground pass had run. The composite's coverage and arm read the same \
         lane, so a whole-file search for it stays green through exactly this \
         deletion — the body is what has to carry the test. Body was: \
         {covered}",
    );
    assert!(
        shader.contains("span.y = min(span.y, ground_t);"),
        "the march no longer clips its span against the ground, so the ground \
         pass draws an occluder nothing consumes",
    );
}

/// The default light really does come from above and from the left.
#[test]
fn the_default_light_comes_from_above_and_over_the_left_shoulder() {
    let [x, y, z] = DEFAULT_LIGHT_DIR;
    assert!(
        z > 0.0,
        "the default light shines from below (z = {z}), so an overshooting \
             top would be shaded like a dent"
    );
    assert!(
        x < 0.0 && y < 0.0,
        "the default light no longer comes over the viewer's left shoulder \
             (x = {x}, y = {y})"
    );
    // Not normalised — the shader does that — but it must not be so short
    // that it is indistinguishable from the zero vector after normalising.
    let magnitude = (x * x + y * y + z * z).sqrt();
    assert!(
        magnitude > 0.5,
        "the default light vector is {magnitude} long"
    );
}

/// The empty-cell threshold selects index 0 and nothing else.
#[test]
fn the_empty_threshold_selects_exactly_palette_index_zero() {
    let threshold = DEFAULT_EMPTY_INDEX_THRESHOLD;
    assert!(
        0.0 < threshold && threshold < 1.0 / 255.0,
        "an empty-cell threshold of {threshold} does not separate palette \
             index 0 from index 1"
    );
}

/// The shader's palette size is the one the LUT budget pays for.
#[test]
fn the_shader_and_the_lut_constant_agree() {
    let expected = format!("const LUT_ENTRIES: f32 = {LUT_ENTRIES}.0;");
    assert!(
        include_str!("../volume.wgsl").contains(&expected),
        "volume.wgsl does not declare `{expected}`, so its palette \
             coordinate no longer matches the {VOLUME_LUT_BYTES}-byte table \
             `constants::VOLUME_LUT_BYTES` sizes"
    );
}

/// The shader's kilometres-per-degree is the radar crate's, to the bit.
#[test]
fn the_shaders_km_per_degree_is_the_radar_crates_own() {
    const DECL: &str = "const KM_PER_DEGREE_LAT: f32 = ";
    let source = include_str!("../volume.wgsl");
    let tail = source
        .split_once(DECL)
        .expect("volume.wgsl no longer declares `KM_PER_DEGREE_LAT`")
        .1;
    let literal = tail
        .split_once(';')
        .expect("volume.wgsl's KM_PER_DEGREE_LAT declaration is unterminated")
        .0
        .trim();
    let shader: f32 = literal
        .parse()
        .unwrap_or_else(|_| panic!("volume.wgsl's KM_PER_DEGREE_LAT `{literal}` is not a number"));

    let expected = squallar_geo::KM_PER_DEGREE_LAT as f32;
    assert_eq!(
        shader.to_bits(),
        expected.to_bits(),
        "volume.wgsl says {shader} km per degree and \
             `squallar_geo::KM_PER_DEGREE_LAT` says {expected}, so \
             the volume floor's geography and the radar data drawn over it \
             are on different spheres",
    );
}
