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
///
/// Distinctness is the point: a `to_bytes` that swapped two `vec4`s, or
/// transposed the matrix, or wrote the light direction into the transfer
/// slot would still round-trip through a decoder that mirrored it. Only
/// absolute positions with unique values catch that.
fn distinct() -> VolumeUniform {
    let mut matrix = [[0.0f32; 4]; 4];
    for (column, values) in matrix.iter_mut().enumerate() {
        for (row, slot) in values.iter_mut().enumerate() {
            // Column-major, so the lane index is column * 4 + row, and the
            // value says which is which: 10 * column + row.
            *slot = (column * 10 + row) as f32;
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
    }
}

/// The block is exactly 192 bytes, and the shader declares the same.
///
/// Both halves matter: the Rust side could be 192 while the WGSL grew a
/// member, and then every lane after the new one is read from the wrong
/// place with no error at all — a uniform buffer larger than the shader's
/// block is legal.
#[test]
fn the_block_is_a_mat4_and_eight_vec4s_on_both_sides() {
    assert_eq!(VOLUME_UNIFORM_BYTES, 64 + 8 * 16);
    assert_eq!(OFFSET_FLOOR_GEO + 16, VOLUME_UNIFORM_BYTES);

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
        (1, 8),
        "volume.wgsl's uniform block is {mat4s} mat4x4 and {vec4s} vec4, \
             which is {} bytes, not the {VOLUME_UNIFORM_BYTES} this file packs. \
             A block smaller than the buffer is legal, so nothing would report \
             the mismatch — every member past the change would simply read the \
             wrong lane.",
        mat4s * 64 + vec4s * 16
    );
}

/// The declaration order in the WGSL is the order this file packs.
///
/// Reordering two `vec4<f32>` members in the shader is a one-line edit that
/// leaves the block the same size and every test above green, while the
/// camera reads the box size and the box size reads the camera.
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
    //
    // Everything below indexes with `offset / 4` using the very constants
    // `to_bytes` writes at, so on its own it cannot see a transposition:
    // swap `OFFSET_BOX_SIZE_KM` and `OFFSET_GRID_DIMS` and the writer and
    // the reader move together. A review proved it — all 103 host tests
    // passed, and only the `#[ignore]`d GPU test noticed — which at the time
    // ran nowhere but by hand. It runs in CI now, on lavapipe, but a
    // transposition should not need a render to be caught at all.
    // The realistic route in is someone reordering `struct
    // Volume` in the WGSL and transposing two offsets to match; the shader
    // then reads the box size out of the grid-dims slot, which is wrong
    // step lengths and wrong gradient spacing, i.e. a merely hazy volume.
    //
    // So the offsets are pinned to literals here, and the loop below is
    // what says each member reaches the offset it names.
    assert_eq!(
        (
            OFFSET_BOX_FROM_CLIP,
            OFFSET_EYE_IN_BOX,
            OFFSET_BOX_SIZE_KM,
            OFFSET_GRID_DIMS,
            OFFSET_LIGHT_DIR_AMBIENT,
            OFFSET_TRANSFER,
            OFFSET_FLAGS,
            OFFSET_FLOOR_UV,
            OFFSET_FLOOR_GEO,
        ),
        (0, 64, 80, 96, 112, 128, 144, 160, 176),
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
    ] {
        let lane = offset / 4;
        assert_eq!(
            &packed[lane..lane + 4],
            &expected,
            "`{member}` is not at byte {offset}"
        );
    }
}

/// The block has no reserved lanes left, and the last two to go — the
/// isosurface pair — default to the negative sentinels that select the
/// lit-volume march.
///
/// This test is the free-lane registry's tombstone: `eye_in_box.w` and
/// `grid_dims.w` were the two reserved-zero lanes (after `box_size_km.w`
/// and the three upper flags lanes took the exaggeration, the
/// reconstruction level, the march step and the floor switch), and the
/// view-mode work took both — `iso_threshold` and `iso_centre`. A seventh
/// member is now a 176-byte block and a WGSL struct change, on both
/// sides, with every layout test above moving together.
///
/// The sentinel half matters most: negative — not zero — selects the lit
/// volume, because an index-0 threshold is a real isosurface
/// configuration ("the surface of any data"). A default of 0.0 here would
/// put every existing pane into isosurface mode at the no-data boundary.
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
///
/// The default half is the load-bearing one: 0 is the bit-exact
/// instrument configuration the silhouette harness measures through —
/// the coarse mip's filter weight is exactly zero there — and any other
/// default would move alpha at every boundary of every mask.
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
///
/// This lane used to carry a negative sentinel selecting a
/// nearest-neighbour snap, for the seven products whose no-data boundary a
/// plain `R8Unorm` filter could not be trusted across. The texture is
/// coverage-premultiplied `Rg16Float` now: the shader divides the
/// premultiplied index by the coverage, so a filtered sample beside air
/// lands inside the convex hull of the stored indices and every product
/// takes the one filtering path. Both halves are asserted because either
/// alone would pass while the other regressed — a reinstated branch with no
/// writer is dead code, and a reinstated writer with no branch silently
/// samples at a negative LOD.
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
        crate::volume::bridge::CLOUD_RECONSTRUCTION_LOD >= 0.0
            && (0..=40)
                .map(
                    |tenths| crate::volume::bridge::cloud_reconstruction_lod_for(
                        tenths as f32 / 10.0
                    )
                )
                .all(|lod| lod >= 0.0),
        "a production writer produced a negative reconstruction level, which \
             the shader would now sample the grid at rather than treat as a \
             mode",
    );
}

/// Grid dimensions cross as floats, not as integers reinterpreted.
///
/// `grid_dims` is the one member whose Rust type is an integer, and the
/// mistake with teeth is writing `n.to_le_bytes()` for a `u32`: 256 then
/// arrives as 3.6e-43 and the gradient's voxel step becomes astronomically
/// large, which reads as a completely unshaded volume rather than as an
/// error.
#[test]
fn the_grid_dimensions_cross_as_floats() {
    let uniform = VolumeUniform::new([240.0, 240.0, 20.0], [256, 256, 128]);
    let packed = lanes(&uniform.to_bytes());
    let lane = OFFSET_GRID_DIMS / 4;
    assert_eq!(&packed[lane..lane + 3], &[256.0, 256.0, 128.0]);
}

/// `new` produces a uniform whose defaults the shader can actually march.
///
/// Each of these is a value that makes the raymarch degenerate rather than
/// merely ugly: a zero axis divides by zero in the gradient's voxel step, a
/// non-positive extinction makes every cell perfectly transparent, and an
/// early-out at or above 1 stops the march on its first sample.
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
}

/// The default light really does come from above and from the left.
///
/// Added after both minus signs in `DEFAULT_LIGHT_DIR` survived a mutation
/// pass: `the_defaults_are_a_marchable_configuration` only asks that the
/// vector is not all zeroes, which a light shining up from underneath the
/// storm satisfies. That is not a crash and not a NaN — it is a volume
/// whose overshooting tops read as dents, which is the failure this
/// convention exists to avoid.
///
/// Box space is z-up, and x/y run east and north, so "up and over the
/// viewer's left shoulder" is `z > 0` with `x < 0` and `y < 0`.
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
///
/// The shader skips a cell when `index > threshold` is false, and an
/// An eight-bit unorm fetch of palette entry `n` returns `n / 255`. So the
/// threshold
/// has to sit strictly between 0 and 1/255 — and it has to be *stated* as
/// that rather than as a small number, because WP-C's whole no-data
/// decision is that index 0 is the bottom of the ramp.
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
///
/// `VOLUME_LUT_BYTES` sizes the upload; `LUT_ENTRIES` in the shader turns a
/// fetched index into a texture coordinate. If they disagree the volume is
/// painted with a table shifted by a fraction of a texel — every colour
/// slightly wrong, nothing obviously broken.
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
