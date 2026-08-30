use super::*;

/// [`VOLUME_SHADER_WGSL`] with its comments removed.
fn shader_code() -> String {
    VOLUME_SHADER_WGSL
        .lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The comment-stripper actually strips something, and keeps the code.
#[test]
fn the_comment_stripper_removes_prose_and_keeps_code() {
    let code = shader_code();
    assert!(
        !code.contains("//"),
        "a comment marker survived the stripper",
    );
    assert!(
        code.contains("fn fs_raymarch(") && code.contains("textureSampleLevel("),
        "the comment stripper removed code as well as comments"
    );
}

/// The quad is 48 bytes of `vec2<f32>`, and it covers all of clip space.
#[test]
fn the_quad_is_forty_eight_bytes_covering_all_of_clip_space() {
    assert_eq!(QUAD_BYTES, 48);
    assert_eq!(quad_bytes().len(), QUAD_BYTES);
    assert_eq!(QUAD_VERTEX_COUNT as usize % 3, 0, "not whole triangles");

    let xs: Vec<f32> = QUAD_CORNERS.iter().map(|c| c[0]).collect();
    let ys: Vec<f32> = QUAD_CORNERS.iter().map(|c| c[1]).collect();
    assert_eq!(xs.iter().cloned().fold(f32::INFINITY, f32::min), -1.0);
    assert_eq!(xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max), 1.0);
    assert_eq!(ys.iter().cloned().fold(f32::INFINITY, f32::min), -1.0);
    assert_eq!(ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max), 1.0);

    // All four corners present, so the two triangles really do tile the
    // rectangle rather than covering one half of it twice.
    for corner in [[-1.0, -1.0], [1.0, -1.0], [-1.0, 1.0], [1.0, 1.0]] {
        assert!(
            QUAD_CORNERS.contains(&corner),
            "clip-space corner {corner:?} is not in the quad, so part of \
                 the offscreen is never drawn"
        );
    }
}

/// The two triangles [`QUAD_CORNERS`] describes, in draw order.
fn quad_triangles() -> [[[f32; 2]; 3]; 2] {
    [
        [QUAD_CORNERS[0], QUAD_CORNERS[1], QUAD_CORNERS[2]],
        [QUAD_CORNERS[3], QUAD_CORNERS[4], QUAD_CORNERS[5]],
    ]
}

/// The two triangles tile clip space exactly once, with no gap and no
/// overlap.
#[test]
fn the_two_triangles_tile_clip_space_exactly_once() {
    /// Which side of the directed line `a -> b` the point falls on.
    fn side(a: [f32; 2], b: [f32; 2], p: [f32; 2]) -> f32 {
        (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
    }
    /// Inside, for either winding.
    fn inside(triangle: [[f32; 2]; 3], p: [f32; 2]) -> bool {
        let sides = [
            side(triangle[0], triangle[1], p),
            side(triangle[1], triangle[2], p),
            side(triangle[2], triangle[0], p),
        ];
        sides.iter().all(|&s| s >= 0.0) || sides.iter().all(|&s| s <= 0.0)
    }

    let triangles = quad_triangles();
    for i in 0..10 {
        for j in 0..10 {
            let point = [-0.95 + 0.19 * i as f32, -0.93 + 0.19 * j as f32];
            let covering = triangles.iter().filter(|t| inside(**t, point)).count();
            assert_eq!(
                covering, 1,
                "clip-space point {point:?} is covered by {covering} of the \
                     quad's two triangles. Anything but one means the volume is \
                     missing a region of the pane, or drawing one twice."
            );
        }
    }
}

/// The quad's bytes are little-endian `f32` pairs in draw order.
#[test]
fn the_quad_packs_its_corners_in_draw_order() {
    let packed = quad_bytes();
    for (vertex, corner) in QUAD_CORNERS.iter().enumerate() {
        for (axis, expected) in corner.iter().enumerate() {
            let at = (vertex * 2 + axis) * 4;
            let value =
                f32::from_le_bytes(<[u8; 4]>::try_from(&packed[at..at + 4]).expect("four bytes"));
            assert_eq!(value, *expected, "vertex {vertex} axis {axis}");
        }
    }
    assert_eq!(
        QUAD_VERTEX_LAYOUT.array_stride as usize * QUAD_VERTEX_COUNT as usize,
        QUAD_BYTES,
        "the vertex stride and the packed bytes disagree, so the second \
             triangle reads from the wrong offset"
    );
}

/// sRGB targets get the decoding blit and non-sRGB ones the pass-through.
#[test]
fn the_blit_entry_point_follows_the_surfaces_srgb_ness() {
    for format in [
        wgpu::TextureFormat::Rgba8UnormSrgb,
        wgpu::TextureFormat::Bgra8UnormSrgb,
    ] {
        assert_eq!(
            blit_entry_point_for(format),
            ENTRY_FS_BLIT_LINEAR,
            "{format:?} is an sRGB surface and did not get the decoding blit"
        );
    }
    for format in [
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureFormat::Bgra8Unorm,
    ] {
        assert_eq!(
            blit_entry_point_for(format),
            ENTRY_FS_BLIT_GAMMA,
            "{format:?} is not an sRGB surface and did not get the \
                 pass-through blit"
        );
    }
}

/// A mirror holds gamma-encoded texels exactly when its format is **not**
/// sRGB, over every format the swapchain can actually be.
#[test]
fn a_mirror_is_gamma_encoded_exactly_when_its_format_is_not_srgb() {
    for format in [
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureFormat::Bgra8Unorm,
    ] {
        assert!(
            mirror_is_gamma_encoded(format),
            "{format:?} is not an sRGB format, so egui's gamma entry point drew \
             the mirror and its texels are gamma-encoded; reporting otherwise \
             makes the shader decode a value that is already linear",
        );
    }
    for format in [
        wgpu::TextureFormat::Rgba8UnormSrgb,
        wgpu::TextureFormat::Bgra8UnormSrgb,
    ] {
        assert!(
            !mirror_is_gamma_encoded(format),
            "{format:?} is an sRGB format, so egui's linear entry point drew the \
             mirror and the hardware encoded on write; reporting it as \
             gamma-encoded makes the shader decode twice",
        );
    }
    // The mirror's own default format must agree with the predicate rather than
    // be a fifth case: `ensure_mirror` is handed the swapchain's format at
    // runtime, and `FLOOR_FORMAT` is what the GPU fixtures plant through.
    assert_eq!(
        mirror_is_gamma_encoded(FLOOR_FORMAT),
        !FLOOR_FORMAT.is_srgb(),
        "FLOOR_FORMAT has stopped agreeing with the predicate that describes it",
    );
}

/// The offscreen is not itself an sRGB format.
#[test]
fn the_offscreen_format_is_not_srgb() {
    assert!(!OFFSCREEN_FORMAT.is_srgb());
    assert!(!LUT_FORMAT.is_srgb());
}

/// The blend state is egui's, component for component.
#[test]
fn the_blend_state_is_the_one_egui_uses() {
    assert_eq!(EGUI_BLEND.color.src_factor, wgpu::BlendFactor::One);
    assert_eq!(
        EGUI_BLEND.color.dst_factor,
        wgpu::BlendFactor::OneMinusSrcAlpha
    );
    assert_eq!(EGUI_BLEND.color.operation, wgpu::BlendOperation::Add);
    assert_eq!(
        EGUI_BLEND.alpha.src_factor,
        wgpu::BlendFactor::OneMinusDstAlpha
    );
    assert_eq!(EGUI_BLEND.alpha.dst_factor, wgpu::BlendFactor::One);
    assert_eq!(EGUI_BLEND.alpha.operation, wgpu::BlendOperation::Add);
}

/// Every entry point this file names exists in the WGSL, and vice versa.
#[test]
fn the_entry_point_list_is_exactly_what_the_shader_declares() {
    for (name, stage) in ENTRY_POINTS {
        let attribute = match stage {
            ShaderStage::Vertex => "@vertex",
            ShaderStage::Fragment => "@fragment",
        };
        let declaration = format!("fn {name}(");
        let at = VOLUME_SHADER_WGSL
            .find(&declaration)
            .unwrap_or_else(|| panic!("volume.wgsl declares no `{declaration}`"));
        let preceding = VOLUME_SHADER_WGSL[..at]
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .expect("nothing precedes the entry point");
        assert_eq!(
            preceding, attribute,
            "`{name}` is listed as a {stage:?} entry point but the shader \
                 declares it under `{preceding}`"
        );
    }

    let code = shader_code();
    let declared = code.matches("@vertex").count() + code.matches("@fragment").count();
    assert_eq!(
        declared,
        ENTRY_POINTS.len(),
        "volume.wgsl declares {declared} entry points but ENTRY_POINTS \
             lists {}. An unlisted entry point is never translated to GLSL by \
             the naga test, so it reaches a browser unchecked.",
        ENTRY_POINTS.len()
    );
}

/// The shader binds exactly the group-0 slots this file declares.
#[test]
fn the_shaders_bindings_are_the_ones_the_layouts_declare() {
    for (group, binding, name) in [
        (0, BINDING_UNIFORM, "volume"),
        (0, BINDING_GRID_TEXTURE, "grid_texture"),
        (0, BINDING_GRID_SAMPLER, "grid_sampler"),
        (0, BINDING_LUT_TEXTURE, "lut_texture"),
        (0, BINDING_LUT_SAMPLER, "lut_sampler"),
        (0, BINDING_BLIT_TEXTURE, "blit_texture"),
        (0, BINDING_BLIT_SAMPLER, "blit_sampler"),
        (0, BINDING_JITTER_TEXTURE, "jitter_texture"),
        (1, BINDING_FLOOR_TEXTURE, "floor_texture"),
        (1, BINDING_FLOOR_SAMPLER, "floor_sampler"),
        (2, BINDING_OCCLUDER_TEXTURE, "occluder_texture"),
        (2, BINDING_GROUND_TEXTURE, "ground_texture"),
    ] {
        let expected = format!("@group({group}) @binding({binding}) var");
        let line = VOLUME_SHADER_WGSL
            .lines()
            .find(|line| line.starts_with(&expected))
            .unwrap_or_else(|| panic!("volume.wgsl has no `{expected}` declaration for `{name}`"));
        assert!(
            line.contains(name),
            "group {group} binding {binding} is declared as `{line}`, not as `{name}`"
        );
    }

    let bindings = shader_code().matches("@binding(").count();
    assert_eq!(
        bindings, 12,
        "volume.wgsl declares {bindings} bindings; this file names 12, and a \
             binding the layouts do not declare fails pipeline creation"
    );
}

/// The shader's tile mask and the tile's edge are the same number.
#[test]
fn the_shader_and_the_blue_noise_tile_agree() {
    assert!(
        BLUE_NOISE_EDGE.is_power_of_two(),
        "the tile's edge is {BLUE_NOISE_EDGE}, not a power of two, so masking is no longer the \
         same operation as wrapping and the shader would read outside the tile",
    );
    let expected = format!("const JITTER_TILE_MASK: i32 = {};", BLUE_NOISE_EDGE - 1);
    assert!(
        VOLUME_SHADER_WGSL.contains(&expected),
        "volume.wgsl does not declare `{expected}`; the shader's mask and \
         `blue_noise::BLUE_NOISE_EDGE` have drifted, and the march would tile the jitter at the \
         wrong period",
    );
}

/// One sampler per **sampled** texture, in each pipeline, as naga requires.
#[test]
fn each_sampled_texture_has_exactly_one_sampler() {
    let code = shader_code();
    let textures = code.matches(": texture_").count();
    let samplers = code.matches(": sampler;").count();
    assert_eq!(
        (textures, samplers),
        (7, 4),
        "volume.wgsl declares {textures} textures and {samplers} samplers; \
             naga refuses a texture sampled through two samplers in one entry \
             point"
    );
    // The three unsampled ones are the jitter tile and the ground pass's two
    // outputs, and each is unsampled *because* it is loaded: the jitter tile is
    // indexed by pixel, and the other two are read at a 1:1 texel-to-pixel
    // invariant, which is what keeps the WebGL2 float-filterability question
    // from ever arising.
    for loaded in ["jitter_texture", "occluder_texture", "ground_texture"] {
        assert!(
            code.contains(&format!("textureLoad({loaded}")),
            "nothing loads `{loaded}`; it is one of the textures that carries no sampler, so if \
             it is sampled instead the pipeline has a texture with no sampler to sample it \
             through",
        );
    }
    assert_eq!(
        code.matches("textureLoad(").count(),
        3,
        "volume.wgsl has more `textureLoad`s than the three textures bound without a sampler, \
         so one is either a texture that lost its sampler or a sampled read written as a load",
    );
}

/// The shader samples with an explicit level everywhere.
#[test]
fn every_sample_gives_an_explicit_level() {
    let implicit = shader_code().matches("textureSample(").count();
    assert_eq!(
        implicit, 0,
        "volume.wgsl calls `textureSample` {implicit} time(s); the march \
             breaks on a data-dependent condition, so implicit-LOD sampling is \
             a validation failure on every backend"
    );
    assert!(shader_code().contains("textureSampleLevel("));
}

/// `textureNumLevels` appears nowhere.
#[test]
fn the_shader_never_asks_how_many_mip_levels_there_are() {
    assert!(
        !shader_code().contains("textureNumLevels"),
        "volume.wgsl calls `textureNumLevels`, which naga gates on GLSL \
             core 130 with no ES version at all"
    );
}

/// The step ceiling is a `const`, so it folds to a literal in the loop.
#[test]
fn the_step_count_is_a_constant_the_loop_bound_names() {
    assert!(
        shader_code().contains("const RAYMARCH_STEP_CEILING: i32 = 1024;"),
        "the raymarch's step ceiling is no longer a `const` literal"
    );
    assert!(
        shader_code().contains("i < RAYMARCH_STEP_CEILING"),
        "the march's loop bound is no longer the constant"
    );
    assert!(
        shader_code().contains("(span.y - span.x) / f32(RAYMARCH_STEP_CEILING)"),
        "the dt floor against the ceiling is gone; a span that outruns \
             the ceiling would render truncated mid-box instead of coarser"
    );
    assert!(
        shader_code().contains("volume.flags.z / cells_per_t"),
        "the march no longer takes its step from the uniform's step lane"
    );
    // The host-side restatements, against the same literals rather than against
    // the constants themselves — pinning a constant to itself is the mistake
    // `every_lane_lands_at_its_std140_offset` documents.
    assert_eq!(
        (RAYMARCH_STEP_CEILING, RAYMARCH_STEP_CELLS),
        (1024, 1.0),
        "the Rust restatement of the march constants no longer matches \
             the WGSL literals this test pins"
    );
    assert_eq!(
        VolumeUniform::new([240.0, 240.0, 20.0], [128, 128, 64]).step_cells,
        RAYMARCH_STEP_CELLS,
        "the uniform's default step no longer matches the constant the \
             silhouette harness mirrors, so every instrument marches a \
             different comb than the mirror predicts"
    );
}

/// The hand-built mip is the plain box mean of BOTH channels — and that,
/// under the shader's `R_bar / G_bar`, IS the occupancy-weighted mean of
/// the index, with no special case anywhere.
#[test]
fn the_grid_mip_is_the_mean_of_each_coarse_blocks_measured_cells() {
    /// One texel of a packed plane, as `(R, G)`, through the production
    /// decoder — so a channel-order or stride error shows up here too.
    fn texel(plane: &[u8], at: usize) -> (f32, f32) {
        let cell = at * GRID_BYTES_PER_CELL as usize;
        (
            super::read_channel(plane, cell),
            super::read_channel(plane, cell + 2),
        )
    }
    /// The shader's reconstruction, in the host's arithmetic: `R_bar` over
    /// `G_bar`, back in 0..=255 index units.
    fn reconstructed(plane: &[u8], at: usize) -> Option<f32> {
        let (r, g) = texel(plane, at);
        (g != 0.0).then(|| r / g * 255.0)
    }
    /// Half an ulp of binary16's 11-bit significand, in index units. The
    /// coverage channel is exact — every reachable value is `n/8` — so this
    /// is the whole of the mip's reconstruction error. See
    /// `downsampled_grid`'s doc.
    const MIP_QUANTISATION_TOLERANCE: f32 = 255.0 / 2048.0;

    // Uniform control — every cell covered, so the coarse level is the same
    // value at full coverage.
    let (coarse, bytes) = downsampled_grid([4, 4, 2], &[7u8; 32]);
    assert_eq!(coarse, [2, 2, 1]);
    assert_eq!(bytes.len(), 4 * GRID_BYTES_PER_CELL as usize);
    for at in 0..4 {
        assert_eq!(texel(&bytes, at).1, 1.0, "every cell is covered");
        let index = reconstructed(&bytes, at).expect("covered");
        assert!(
            (index - 7.0).abs() < MIP_QUANTISATION_TOLERANCE,
            "a uniform grid must downsample to itself; coarse cell {at} \
             reconstructs to {index}, not 7",
        );
    }

    // The all-empty block: no data and no coverage. Nothing divides by zero
    // here or in the shader, whose divisor is floored.
    let (_, bytes) = downsampled_grid([4, 4, 2], &[0u8; 32]);
    assert_eq!(
        bytes,
        vec![0u8; 4 * GRID_BYTES_PER_CELL as usize],
        "an unmeasured block must stay no-data"
    );
    assert_eq!(reconstructed(&bytes, 0), None);

    // A lone measured cell: fine cell (0,0,0) of a 4x4x2 grid is in
    // coarse block (0,0,0) and nowhere else, and it keeps its own value.
    let mut fine = vec![0u8; 32];
    fine[0] = 255;
    let (_, bytes) = downsampled_grid([4, 4, 2], &fine);
    assert_eq!(
        texel(&bytes, 0),
        (0.125, 0.125),
        "an eighth of full scale in both channels; anything else is a \
         stride error"
    );
    assert_eq!(
        reconstructed(&bytes, 0),
        Some(255.0),
        "a lone measured 255 must reconstruct to its own value; the 32 an \
         eighth of it would give is the full-cube mean that erased the \
         Harvey core at coarse cell sizes"
    );
    assert_eq!(
        &bytes[GRID_BYTES_PER_CELL as usize..],
        &vec![0u8; 3 * GRID_BYTES_PER_CELL as usize][..],
        "it must not reach another block"
    );

    // A mixed block: the measured cells' own mean. Anchored on the CONTRACT
    // — the true mean of {100, 105} — with the quantisation tolerance, not
    // on whatever this implementation's rounding lands at.
    let mut fine = vec![0u8; 32];
    fine[0] = 100;
    fine[1] = 105;
    let (_, bytes) = downsampled_grid([4, 4, 2], &fine);
    assert_eq!(texel(&bytes, 0).1, 0.25, "two of eight cells are covered");
    let index = reconstructed(&bytes, 0).expect("the block is covered");
    assert!(
        (index - 102.5).abs() < MIP_QUANTISATION_TOLERANCE,
        "two measured cells among six empties must reconstruct to their own \
         mean of 102.5 (got {index}), not the full-cube 25.6",
    );

    // The bound itself, over every reachable block, THROUGH `downsampled_grid`:
    // what the shader really divides, against the true occupancy mean.
    let mut worst = 0.0f32;
    let mut worst_at = (0u32, 0u32);
    let mut cases = 0u32;
    for n in 1..=8u32 {
        // Below `n` is unreachable: a measured cell is `1..=255`, never 0.
        for sum in n..=255 * n {
            // `sum` spread as evenly as it goes, which keeps every cell inside
            // `1..=255` for every reachable pair.
            let (quotient, remainder) = (sum / n, sum % n);
            let mut fine = [0u8; 8];
            for (cell, slot) in fine.iter_mut().take(n as usize).enumerate() {
                *slot = (quotient + u32::from((cell as u32) < remainder)) as u8;
            }
            let (coarse, bytes) = downsampled_grid([2, 2, 2], &fine);
            assert_eq!(coarse, [1, 1, 1]);
            let (r, g) = texel(&bytes, 0);
            assert_eq!(g, n as f32 / 8.0, "the coverage channel is exact in f16");
            let error = r / g * 255.0 - sum as f32 / n as f32;
            if error.abs() > worst {
                worst = error.abs();
                worst_at = (n, sum);
            }
            cases += 1;
        }
    }
    assert_eq!(
        cases, 9152,
        "the exhaustiveness argument in this test's doc names a count, and \
         the count is now {cases}: the sweep no longer covers what it says it \
         covers, or covers it more than once",
    );
    assert!(
        worst < MIP_QUANTISATION_TOLERANCE,
        "the mip's worst reconstruction error is {worst} index units at \
         (n, sum) = {worst_at:?}, over the {MIP_QUANTISATION_TOLERANCE} \
         half an ulp of binary16 allows — the coarse level has stopped being \
         the occupancy mean to the tolerance the callers were told",
    );
    // Not exact, and stated as inexact rather than left to look exact: the
    // channels are 11-bit significands, so a bound of zero here would be a
    // test that cannot fail rather than a tighter promise.
    assert!(
        worst > 0.0,
        "the mip now reconstructs the occupancy mean exactly, which binary16 \
         cannot do — `downsampled_grid` is no longer storing what its doc \
         says, or this loop stopped reaching the sparse blocks",
    );

    // The block under coarse cell (1, 0, 0): fine x in 2..4, y in 0..2,
    // z in 0..2. Fill exactly that block and nothing else.
    let mut fine = vec![0u8; 32];
    for z in 0..2 {
        for y in 0..2 {
            for x in 2..4 {
                fine[(z * 4 + y) * 4 + x] = 100;
            }
        }
    }
    let (_, bytes) = downsampled_grid([4, 4, 2], &fine);
    for at in 0..4 {
        let covered = at == 1;
        assert_eq!(
            texel(&bytes, at).1,
            f32::from(u8::from(covered)),
            "the filled block must land whole in coarse cell (1,0,0); \
             anything else is a dimension-order error smearing data across \
             the mip"
        );
        if covered {
            let index = reconstructed(&bytes, at).expect("covered");
            assert!((index - 100.0).abs() < MIP_QUANTISATION_TOLERANCE);
        }
    }

    // Odd extents follow wgpu's mip arithmetic: max(n / 2, 1). The clamp
    // counts a fine cell more than once — in BOTH channels, so the ratio,
    // and with it the reconstructed index, is untouched.
    let (coarse, bytes) = downsampled_grid([3, 3, 3], &[9u8; 27]);
    assert_eq!(coarse, [1, 1, 1]);
    assert_eq!(bytes.len(), GRID_BYTES_PER_CELL as usize);
    assert_eq!(texel(&bytes, 0).1, 1.0);
    let index = reconstructed(&bytes, 0).expect("covered");
    assert!((index - 9.0).abs() < MIP_QUANTISATION_TOLERANCE);
}

/// The premultiplied plane is the index and a binary coverage beside it —
/// the texture's whole contract, in one place.
#[test]
fn the_premultiplied_plane_is_index_and_a_binary_coverage() {
    /// Half an ulp of binary16's 11-bit significand, as a relative error.
    const HALF_ULP: f32 = 1.0 / 2048.0;

    let indices: Vec<u8> = (0..=255u8).collect();
    let mut buffer = Vec::new();
    let plane = super::coverage_premultiplied_into(&mut buffer, &indices);
    assert_eq!(plane.len(), indices.len() * GRID_BYTES_PER_CELL as usize);
    for (index, texel) in indices
        .iter()
        .zip(plane.chunks_exact(GRID_BYTES_PER_CELL as usize))
    {
        let stored = super::read_channel(texel, 0);
        let wanted = f32::from(*index) / 255.0;
        assert!(
            (stored - wanted).abs() <= wanted * HALF_ULP,
            "R must be coverage x index — and index 0 is the only one \
             coverage zeroes, which leaves the index itself. Index {index} \
             is {wanted}, stored as {stored}, further off than the relative \
             half-ulp {HALF_ULP} the format promises; a fixed-point channel \
             fails this at the faint end, which is where the reconstruction \
             breaks",
        );
        assert_eq!(
            super::read_channel(texel, 2),
            if *index == squallar_radar::voxel::NO_DATA_INDEX {
                0.0
            } else {
                1.0
            },
            "coverage at index {index} is not binary on the no-data test",
        );
    }
    // The faintest measurable echo, called out because it is the value an
    // eight-bit channel loses first once the filter has weighted it: `1/255`
    // scaled by any coverage under a half rounds to zero, the shell around the
    // echo reconstructs to the no-data index, and the silhouette's reach starts
    // reading the stored value again.
    let faintest = super::read_channel(plane, GRID_BYTES_PER_CELL as usize);
    assert!(
        faintest > 0.0 && (faintest * 255.0 - 1.0).abs() < 0.01,
        "index 1 stores {faintest}, which is not 1/255 — the faintest echo \
         the encoder can be handed has to survive the channel, or every \
         boundary around it reconstructs as air",
    );
}

/// The 256-entry texel table is the conversion it replaces, **byte for byte**,
/// over the whole of its input domain.
#[test]
fn the_texel_table_is_the_conversion_it_replaces() {
    let indices: Vec<u8> = (0..=255u8).collect();
    let mut buffer = Vec::new();
    let plane = super::coverage_premultiplied_into(&mut buffer, &indices);

    let mut arithmetic = Vec::with_capacity(indices.len() * GRID_BYTES_PER_CELL as usize);
    for &index in &indices {
        let covered = index != squallar_radar::voxel::NO_DATA_INDEX;
        arithmetic.extend_from_slice(&half::f16::from_f32(f32::from(index) / 255.0).to_le_bytes());
        arithmetic
            .extend_from_slice(&half::f16::from_f32(if covered { 1.0 } else { 0.0 }).to_le_bytes());
    }

    assert_eq!(
        plane, arithmetic,
        "the texel table has stopped agreeing with the `half` conversion it \
         was built from — the widening is no longer a pure speed change, and \
         every uploaded grid differs from what the format's contract says",
    );
}

/// A widening buffer carried across uploads gives the same plane a fresh one
/// would — **including when a smaller grid follows a larger one**.
#[test]
fn a_reused_widening_buffer_is_the_plane_a_fresh_one_would_be() {
    let stride = GRID_BYTES_PER_CELL as usize;
    // Grows and shrinks interleaved — see the note above on why not
    // largest-first. All three shipped rungs appear, widest last.
    let shapes = [
        [3u32, 1, 2],    // grow from empty
        [64, 64, 32],    // grow from a non-empty smaller buffer
        [7, 5, 3],       // shrink onto a tail
        [64, 64, 48],    // grow, leaving spare capacity behind it
        [128, 64, 32],   // grow past the length but inside that capacity
        [1, 1, 1],       // shrink to the smallest extent there is
        [192, 192, 96],  // grow — the mobile rung
        [128, 128, 64],  // shrink — the wasm32 rung, onto a 13.5 MiB tail
        [256, 256, 128], // grow — the desktop rung, the widest shipped
        [64, 64, 32],    // shrink onto the full 32 MiB tail
    ];
    let mut reused = Vec::new();
    let mut high_water = 0;
    for cells in shapes {
        let count = cells[0] as usize * cells[1] as usize * cells[2] as usize;
        // Every one of the 256 input bytes appears, and the pattern is offset
        // per shape so two shapes never widen to the same bytes.
        let indices: Vec<u8> = (0..count)
            .map(|i| (i.wrapping_add(cells[2] as usize) % 256) as u8)
            .collect();

        let fresh = super::coverage_premultiplied_into(&mut Vec::new(), &indices).to_vec();
        let pooled = super::coverage_premultiplied_into(&mut reused, &indices);
        assert_eq!(
            pooled.len(),
            count * stride,
            "the {cells:?} grid's plane is not the length its extent asks for",
        );
        assert_eq!(
            pooled,
            &fresh[..],
            "the {cells:?} grid widened into a carried buffer differs from the \
             same grid widened into an empty one — the upload's bytes now \
             depend on what was uploaded before it",
        );

        high_water = high_water.max(count * stride);
        assert_eq!(
            reused.len(),
            high_water,
            "the widening buffer no longer sits at the largest shape it has \
             been asked for, so the tail this test exists to catch is not \
             actually being left behind and the case is going unchecked",
        );
    }
    // And the tail really is the earlier grid, not zeroes that would hide a
    // missing prefix bound.
    let small: Vec<u8> = vec![9u8; 8];
    super::coverage_premultiplied_into(&mut reused, &small);
    assert!(
        reused[small.len() * stride..].iter().any(|&b| b != 0),
        "the buffer's tail past the plane is all zeroes, so a version that \
         handed back the whole buffer would look correct here",
    );
}

/// The step length puts the ray direction inside the `length`.
#[test]
fn the_step_length_scales_the_direction_not_just_the_box() {
    assert!(
        shader_code().contains("return length(rd * dt * volume.box_size_km.xyz);"),
        "`step_length_km` no longer multiplies the direction by the box \
             size inside the `length`"
    );
    assert!(
        !shader_code().contains("dt * length(volume.box_size_km"),
        "the shader takes the length of the box size without the ray \
             direction, which makes opacity per step depend on nothing but the \
             box's diagonal"
    );
}

/// The sRGB blit decodes the premultiplied value, without un-premultiplying.
#[test]
fn the_srgb_blit_decodes_the_premultiplied_value_directly() {
    let body = entry_point_body(ENTRY_FS_BLIT_LINEAR);
    assert!(
        body.contains("linear_from_gamma_rgb(premultiplied_gamma.rgb)"),
        "the sRGB blit no longer decodes the premultiplied value the way \
             egui's own fs_main_linear_framebuffer does: {body}"
    );
    assert!(
        !body.contains('/'),
        "the sRGB blit divides — the only division it could want is by \
             alpha, to un-premultiply before decoding. That is the \
             colour-theoretically correct answer and it measured 60/255 away \
             from egui's own output; matching egui is the requirement: {body}"
    );
}

/// And the non-sRGB blit does not decode at all.
#[test]
fn the_non_srgb_blit_is_a_pass_through() {
    let body = entry_point_body(ENTRY_FS_BLIT_GAMMA);
    assert!(
        !body.contains("linear_from_gamma_rgb") && !body.contains("gamma_from_linear_rgb"),
        "the non-sRGB blit converts colour space. egui writes gamma-encoded \
             premultiplied colour onto that surface and blends it in gamma \
             space, which is exactly what the offscreen already holds: {body}"
    );
    assert!(body.contains("textureSampleLevel("));
}

/// The raymarch un-premultiplies before encoding and re-premultiplies after.
#[test]
fn the_raymarch_encodes_a_straight_colour_and_premultiplies_after() {
    let body = entry_point_body(ENTRY_FS_RAYMARCH);
    assert!(
        body.contains("let straight_linear = accumulated / alpha;"),
        "the raymarch no longer un-premultiplies before encoding: {body}"
    );
    assert!(
        body.contains("gamma_from_linear_rgb(straight_linear) * alpha"),
        "the raymarch no longer re-premultiplies after encoding, so the \
             offscreen holds a straight colour where egui's convention is \
             premultiplied: {body}"
    );
}

/// The transfer functions are egui's, character for character.
#[test]
fn the_transfer_functions_match_eguis_own() {
    for line in [
        "let cutoff = srgb < vec3<f32>(0.04045);",
        "let lower = srgb / vec3<f32>(12.92);",
        "let higher = pow((srgb + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));",
        "let cutoff = rgb < vec3<f32>(0.0031308);",
        "let lower = rgb * vec3<f32>(12.92);",
        "let higher = vec3<f32>(1.055) * pow(rgb, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);",
    ] {
        assert!(
            shader_code().contains(line),
            "volume.wgsl's sRGB transfer functions have diverged from \
                 egui-wgpu's egui.wgsl:44-57; this line is gone: {line}"
        );
    }
}

/// Grid byte counts — one byte per cell on the wire, four in the texture, and
/// what the *device* reserves for those four — including the overflow the
/// multiplication can hit.
#[test]
fn a_grids_byte_count_is_four_per_cell_and_the_budget_counts_the_mip() {
    assert_eq!(cell_count([256, 256, 128]), Some(8 * 1024 * 1024));
    assert_eq!(grid_bytes([256, 256, 128]), Some(32 * 1024 * 1024));
    assert_eq!(
        grid_bytes_with_mips([256, 256, 128]),
        Some(38_354_944),
        "the desktop grid is 32 MiB of premultiplied cells and then the whole \
         mip pyramid a second level buys. 38,350,848 B of that is the tile \
         model and 4,096 the page; the two devices this has been read on \
         reserved 38,351,360 B (RTX 3090, which lays the pyramid out and adds \
         512 B to every D3 image) and 37,748,736 B (lavapipe, which lays out \
         only the two levels named), so the charge is over on both",
    );
    assert_eq!(grid_bytes([128, 128, 64]), Some(4 * 1024 * 1024));
    assert_eq!(grid_bytes_with_mips([128, 128, 64]), Some(4_800_512));
    // Too small to halve on any axis: one level. The payload is still four
    // bytes; the allocation is a whole tile, because a 1x1x1 image is laid out
    // in the same 16x8 gob a 16x8 one is.
    assert_eq!(grid_bytes([1, 1, 1]), Some(4));
    assert_eq!(grid_bytes_with_mips([1, 1, 1]), Some(512 + 4096));
    for overflowing in [cell_count, grid_bytes, grid_bytes_with_mips] {
        assert_eq!(
            overflowing([u32::MAX, u32::MAX, u32::MAX]),
            None,
            "a grid whose cell count overflows `usize` must not wrap to a \
                 small number and then be compared against a slice length"
        );
    }
}

/// **A second mip level buys the whole pyramid, and the count says so.**
#[test]
fn a_second_mip_level_is_charged_as_the_whole_pyramid() {
    for cells in [
        [256u32, 256, 128],
        [512, 512, 32],
        [192, 192, 96],
        [128, 128, 64],
        [320, 320, 32],
    ] {
        let packed_two = grid_bytes(cells).unwrap() + grid_bytes(coarse_cells(cells)).unwrap();
        let charged = grid_bytes_with_mips(cells).unwrap();
        assert!(
            charged > packed_two,
            "{cells:?}: charged {charged} B for a two-level descriptor, no more \
             than the {packed_two} B its two levels pack into — so the pyramid \
             below them is uncounted again",
        );
        assert!(
            charged * 10 < packed_two * 11,
            "{cells:?}: charged {charged} B against {packed_two} B packed, more \
             than the mip tail and the tiling together can account for",
        );
    }
}

/// An omitted coarse level is one level in the descriptor — the allocation,
/// not merely the write.
#[test]
fn an_omitted_coarse_level_leaves_the_texture_with_one_level() {
    for cells in [[256, 256, 128], [192, 192, 96], [128, 128, 64]] {
        assert_eq!(grid_mip_levels(cells, CoarseLevel::Built), GRID_MIP_LEVELS);
        assert_eq!(
            grid_mip_levels(cells, CoarseLevel::Omitted),
            1,
            "the {cells:?} grid still allocates a coarse level nothing on \
             this device will sample",
        );
        // What the saving is, on the shape it is largest.
        let with = grid_bytes_with_mips(cells).expect("a shipped shape fits");
        let without = grid_bytes_at(cells, CoarseLevel::Omitted).expect("a shipped shape fits");
        let raw = grid_bytes(cells).expect("a shipped shape fits");
        assert!(
            with - without >= raw / 8,
            "{cells:?}: the second level saves {} B, under the eighth of the \
             raw field the level itself is — so the pyramid it drags with it \
             is not being counted",
            with - without,
        );
        assert!(
            with - without < raw / 6,
            "{cells:?}: the second level saves {} B, more than a 3D pyramid's \
             8/7 of a {raw} B base can account for",
            with - without,
        );
    }
    // A grid too small to halve keeps one level either way — the shape rung
    // that would need two is the one `create_texture` refuses.
    assert_eq!(grid_mip_levels([1, 1, 1], CoarseLevel::Built), 1);
    assert_eq!(grid_mip_levels([1, 1, 1], CoarseLevel::Omitted), 1);
}

/// An offscreen never has a zero axis, and a real size passes through.
#[test]
fn an_offscreen_extent_is_clamped_up_from_zero_and_left_alone_otherwise() {
    assert_eq!(offscreen_extent([0, 0]), [1, 1]);
    assert_eq!(offscreen_extent([0, 900]), [1, 900]);
    assert_eq!(offscreen_extent([1440, 0]), [1440, 1]);
    assert_eq!(offscreen_extent([1440, 900]), [1440, 900]);
}

/// A held offscreen is rebuilt for a new plan and kept for the same one.
#[test]
fn an_offscreen_is_rebuilt_only_when_its_plan_changed() {
    let held = OffscreenPlan::native([1440, 900]);
    assert!(
        offscreen_needs_rebuild(None, held),
        "nothing held must always be built"
    );
    assert!(
        !offscreen_needs_rebuild(Some(held), held),
        "an offscreen of the right size was thrown away and rebuilt"
    );
    for size in [[1441, 900], [1440, 901], [900, 1440]] {
        assert!(
            offscreen_needs_rebuild(Some(held), OffscreenPlan { size, ..held }),
            "a {size:?} pane reused a 1440x900 offscreen, so it would be \
                 blitted at the wrong scale"
        );
    }
    // **The two fields a size comparison cannot see.** A governor that moves a
    // rung leaves the pane's pixel count where it was whenever the fit was
    // already shrinking to the budget rather than dividing, and turning the
    // ground pass on decides whether three attachments exist at all — a target
    // built without them cannot grow them.
    assert!(
        offscreen_needs_rebuild(
            Some(held),
            OffscreenPlan {
                rung: ResolutionRung::Half,
                ..held
            }
        ),
        "a target built at one rung was kept for another, so the offscreen and \
         the quality the uniform reports have come apart"
    );
    assert!(
        offscreen_needs_rebuild(
            Some(held),
            OffscreenPlan {
                ground: GroundPass::On,
                ..held
            }
        ),
        "a target built with no ground attachments was kept for a pane that \
         draws ground; the ground pass would then record into nothing and the \
         march would read the 1x1 placeholder for ever"
    );
}

/// The same packing, in exact arithmetic: floor to a code, then divide.
///
/// `f64` throughout, so nothing here rounds at all — this is the definition the
/// `f32` implementation is judged against, rather than a second implementation
/// with the same hazards.
fn packing_model(v: f32) -> f64 {
    let x = f64::from(v.clamp(0.0, 1.0)) * f64::from(PACK24_CODES);
    x.floor() / f64::from(PACK24_CODES)
}

/// What an `Rgba8Unorm` attachment does to the digits between the write and
/// the read: **clamp to 0..1, then quantise to a byte.**
///
/// Load-bearing, and the reason the flooring pack is not merely tidier: a pack
/// whose arithmetic can produce a digit outside 0..1 loses it here, in the
/// texture, where no Rust arithmetic would ever show it.
fn through_unorm(digits: [f32; 3]) -> [u8; 3] {
    digits.map(|d| (d.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// **The packing floors to a code and never reorders.**
///
/// The *encode* is one-sided: `pack24` floors, so the code written is never
/// past the true value, which is what keeps the clip from letting a ray
/// through the ground. The **decode is not** one-sided, and the earlier
/// wording here said it was: `code * (1/16777215)` is one `f32` multiply and
/// rounds up on about half the code space, so a decoded `t` can sit one code
/// PAST the true one — ~0.15 m in a 240 km box, against a ~1.8 km march cell.
/// Operationally irrelevant; the absolute phrasing was not.
///
/// Judged against exact `f64` arithmetic rather than against the input value,
/// because `code / 16777215` is itself a rounded `f32` — near 1.0 a quantum
/// *is* an ulp, and an assertion written against the input would be measuring
/// `f32`'s resolution rather than the packing's.
#[test]
fn the_packing_floors_to_a_code_across_the_whole_code_space() {
    let quantum = 1.0 / f64::from(PACK24_CODES);
    // The whole path the GPU takes: float in, digits, the unorm texture, float
    // out. Anything short of the texture is not the round trip that happens.
    let round_trip = |v: f32| f64::from(unpack24_bytes(through_unorm(pack24(v))));
    // The tolerance is two codes: `x = v * 16777215.0` is one `f32` multiply,
    // so `x` may land a code either side of the exact product and the floor may
    // then pick a neighbouring code. Two codes out of 16.7 million is three
    // centimetres of displayed vertical against a ~1.8 km march cell.
    let tolerance = 2.0 * quantum;

    let mut worst = 0.0f64;
    let mut previous = f64::NEG_INFINITY;
    // Every code is 16.7 M round trips; a stride coprime with 255 and 65536
    // walks all three digits and both carries without walking all of it.
    let stride = 4373u32;
    let mut code = 0u32;
    while code <= PACK24_CODES {
        let v = code as f32 / PACK24_CODES as f32;
        // The digits `pack24` produces must be storable AS unorm digits, with
        // nothing for the texture to clamp away.
        for digit in pack24(v) {
            assert!(
                (0.0..=1.0).contains(&digit),
                "code {code} packs to a digit of {digit}, which an `Rgba8Unorm` \
                 attachment clamps — the value the march reads back would then \
                 be a whole digit away from the one written"
            );
        }
        let back = round_trip(v);
        worst = worst.max((back - packing_model(v)).abs());
        assert!(
            back >= previous,
            "the packing is not monotone: code {code} decoded to {back} after \
             a larger value {previous}"
        );
        previous = back;
        code += stride;
    }
    assert!(
        worst <= tolerance,
        "the packing's worst departure from an exact floor is {worst}, over the \
         {tolerance} two codes allow — the digits themselves are being \
         reordered or dropped, not merely rounded"
    );

    // Both carries by hand: a stride can step over them, and this is where a
    // *rounding* pack rather than a flooring one overflows a digit past what
    // the texture can hold. This is the reason `pack24` is written with `floor`.
    for code in [
        0u32, 1, 255, 256, 257, 65_535, 65_536, 65_537, 16_777_214, 16_777_215,
    ] {
        let v = code as f32 / PACK24_CODES as f32;
        let off = (round_trip(v) - packing_model(v)).abs();
        assert!(
            off <= tolerance,
            "code {code} round-trips {off} away from an exact floor, over the \
             {tolerance} two codes allow"
        );
    }

    // Saturation is safe in both directions: it is what makes an over-estimated
    // `t_scale` a no-op rather than a clip.
    assert_eq!(unpack24(pack24(2.0)), 1.0);
    assert_eq!(unpack24(pack24(-1.0)), 0.0);
}

/// **The shader's packing and this file's mirror are one arithmetic.**
///
/// Everything above exercises the RUST mirror. Nothing else pins the shader's
/// copy to it, and the occluder is the entire occlusion channel — so a
/// mutation of the WGSL is invisible to every other test in the tree. The one
/// that motivated this: dropping the high term from the low digit,
/// `floor(x - mid * 256.0)` for `floor(x - hi * 65536.0 - mid * 256.0)`, drives
/// the blue digit past 1.0 for any `t >= t_scale / 256`, the unorm write clamps
/// it to 255 on every drawn texel — **8 of the 24 bits gone** — and the GPU
/// registration oracle moves by too little to notice.
///
/// A text pin, in the style of `the_shader_and_the_ground_post_count_agree`,
/// because the two copies are in two languages and only their source can be
/// compared. It is deliberately whole lines: a partial match would accept the
/// mutation that motivated it.
#[test]
fn the_shader_and_this_files_packing_are_one_arithmetic() {
    for line in [
        // `pack24`, every line of it.
        "    let x = clamp(v, 0.0, 1.0) * 16777215.0;",
        "    let hi = floor(x * (1.0 / 65536.0));",
        "    let mid = floor((x - hi * 65536.0) * (1.0 / 256.0));",
        "    let lo = floor(x - hi * 65536.0 - mid * 256.0);",
        "    return vec3<f32>(hi, mid, lo) * (1.0 / 255.0);",
        // and `unpack24`.
        "    return dot(round(c * 255.0), vec3<f32>(65536.0, 256.0, 1.0)) * (1.0 / 16777215.0);",
    ] {
        assert!(
            VOLUME_SHADER_WGSL.contains(line),
            "volume.wgsl no longer contains `{line}`. The shader's packing has \
             drifted from this file's mirror, and every property proved of the \
             mirror above — the floor, the carries, the digits an `Rgba8Unorm` \
             can hold — is now proved of a function the GPU does not run",
        );
    }
    // And the constant both sides scale by, which is the one number that would
    // make the two disagree without either body changing.
    assert!(
        VOLUME_SHADER_WGSL.contains(&format!("* {PACK24_CODES}.0;")),
        "volume.wgsl no longer scales by {PACK24_CODES}, so its code space and \
         this file's have come apart",
    );
}
/// And the tolerance is not slack enough to hide the failure it exists to
/// catch: a pack that rounds its digits instead of flooring them drives the
/// top digit past what an `Rgba8Unorm` can hold, and the texture clamps it away.
#[test]
fn a_rounding_pack_loses_a_whole_digit_at_the_carries() {
    /// [`pack24`] with `round` where it has `floor`, which is the edit a
    /// reviewer makes when the flooring looks like an accident.
    fn rounding_pack(v: f32) -> [f32; 3] {
        let x = v.clamp(0.0, 1.0) * 16_777_215.0;
        let hi = (x * (1.0 / 65536.0)).round();
        let mid = ((x - hi * 65536.0) * (1.0 / 256.0)).round();
        let lo = (x - hi * 65536.0 - mid * 256.0).round();
        [hi / 255.0, mid / 255.0, lo / 255.0]
    }

    let quantum = 1.0 / f64::from(PACK24_CODES);
    let mut worst = 0.0f64;
    for code in [255u32, 256, 65_535, 65_536, 16_777_215] {
        let v = code as f32 / PACK24_CODES as f32;
        let back = f64::from(unpack24_bytes(through_unorm(rounding_pack(v))));
        worst = worst.max((back - packing_model(v)).abs());
    }
    assert!(
        worst > 100.0 * quantum,
        "a rounding pack was only {worst} away from an exact floor at the \
         carries, under 100 quanta — so the tolerance above would accept it and \
         the shipped `floor` is not what the test is holding"
    );
}

/// The bytes a readback yields decode the same way the shader's floats do.
#[test]
fn the_byte_decode_and_the_float_decode_are_one_function() {
    for code in [0u32, 1, 255, 256, 65_535, 65_536, 12_345_678, PACK24_CODES] {
        let bytes = [
            (code >> 16) as u8,
            ((code >> 8) & 0xff) as u8,
            (code & 0xff) as u8,
        ];
        let floats = bytes.map(|b| f32::from(b) / 255.0);
        assert_eq!(
            unpack24_bytes(bytes),
            unpack24(floats),
            "code {code} decodes differently from bytes than from the unorm \
             floats the shader sees, so a readback oracle and the march are \
             reading two different numbers"
        );
    }
}

/// **The shader's height field and this file's mirror are one arithmetic,
/// clamp included.**
///
/// The clamp is not tidiness: [`crate::uniform::VolumeUniform::t_scale_for`]
/// bounds the packing by the farthest corner of the **unit cube**, and that is
/// only an upper bound on a post while the field cannot leave the cube. The
/// amplitude arrives in a plain public lane, so a shader whose copy lost the
/// clamp would put posts outside, saturate the packing and decode them SHORT —
/// the march clipping early while the composite paints terrain at the wrong
/// depth. The host sweep below proves that of the mirror; only this proves the
/// shader still agrees with it.
#[test]
fn the_shader_and_this_files_ground_height_are_one_arithmetic() {
    for line in [
        "    let d = (uv.x - 0.5) / GROUND_RIDGE_SIGMA;",
        "    return clamp(volume.occluder.z * exp(-0.5 * d * d), 0.0, 1.0);",
    ] {
        assert!(
            VOLUME_SHADER_WGSL.contains(line),
            "volume.wgsl no longer contains `{line}`. Its height field has \
             drifted from this file's mirror, so the host oracle predicts a \
             surface the GPU does not draw — and if it is the clamp that went, \
             `t_scale_for`'s corner bound has stopped being an upper bound at \
             all",
        );
    }
    let sigma = format!("const GROUND_RIDGE_SIGMA: f32 = {GROUND_RIDGE_SIGMA};");
    assert!(
        VOLUME_SHADER_WGSL.contains(&sigma),
        "volume.wgsl does not declare `{sigma}`, so the two ridges are \
         different shapes",
    );
}

/// The vertex count and the grid the vertex stage lays out are one arithmetic.
#[test]
fn the_draw_issues_exactly_the_grids_two_triangles_a_cell() {
    let cells = GROUND_POSTS - 1;
    assert_eq!(ground_vertex_count(), 6 * cells * cells);
    // Every vertex index the draw issues must land inside the grid: the last
    // one's quad is the last cell, not one past it.
    let last_quad = (ground_vertex_count() - 1) / 6;
    assert_eq!(last_quad % cells, cells - 1);
    assert_eq!(last_quad / cells, cells - 1);
}

/// The stand-in height field is a ridge, not a step or a plane.
#[test]
fn the_stand_in_ridge_peaks_in_the_middle_and_falls_away_on_both_sides() {
    let amplitude = 0.25f32;
    assert_eq!(ground_height([0.5, 0.5], amplitude), amplitude);
    assert_eq!(ground_height([0.5, 0.5], 0.0), 0.0);
    let mut previous = 0.0f32;
    // Offsets either side of the crest, `0.5 ± d` rather than `u` and `1 - u`:
    // the pair must be exact mirrors in `f32` or the asymmetry measured is the
    // test's own inputs.
    for step in (0..=50).rev() {
        let d = 0.5 * step as f32 / 64.0;
        let h = ground_height([0.5 - d, 0.5], amplitude);
        assert!(h >= previous, "the ridge is not monotone up to its crest");
        assert_eq!(
            h,
            ground_height([0.5 + d, 0.5], amplitude),
            "the ridge is not symmetric about the box's middle at ±{d}"
        );
        previous = h;
    }
    // It must actually be near zero at the box's edges, or the "ridge" is a
    // plateau and the control below would be measuring a lift, not a shape.
    assert!(ground_height([0.0, 0.5], amplitude) < amplitude * 1e-3);
}

/// An upload whose shapes disagree is refused, and one that agrees is not.
#[test]
fn an_upload_whose_shapes_disagree_is_refused() {
    let cells = [8u32, 8, 8];
    let cell_count = 8 * 8 * 8;
    assert_eq!(upload_refusal(cells, cell_count, VOLUME_LUT_BYTES), None);

    for (indices, lut, what) in [
        (cell_count - 1, VOLUME_LUT_BYTES, "one index byte short"),
        (cell_count + 1, VOLUME_LUT_BYTES, "one index byte long"),
        (0, VOLUME_LUT_BYTES, "no indices at all"),
        (cell_count, VOLUME_LUT_BYTES - 4, "a table one entry short"),
        (cell_count, 0, "no colour table"),
    ] {
        assert!(
            upload_refusal(cells, indices, lut).is_some(),
            "an upload with {what} was accepted"
        );
    }

    assert!(
        upload_refusal([u32::MAX, u32::MAX, u32::MAX], 0, VOLUME_LUT_BYTES).is_some(),
        "a grid whose cell count overflows `usize` was accepted; that is \
             the strongest reason to refuse, not a reason to say nothing"
    );
}

/// The colour table's texture width is its entry count, from the budget.
#[test]
fn the_colour_tables_texture_is_as_wide_as_the_budget_pays_for() {
    assert_eq!(lut_texel_count(), 256);
    assert_eq!(lut_texel_count() as usize * 4, VOLUME_LUT_BYTES);
    assert!(
        shader_code().contains(&format!(
            "const LUT_ENTRIES: f32 = {}.0;",
            lut_texel_count()
        )),
        "the shader's palette size and the uploaded texture's width \
             disagree, so every colour is fetched from a fraction of a texel off"
    );
}

/// Every wgpu label this module writes is under the latch's prefix.
#[test]
fn every_label_this_module_writes_carries_the_latch_prefix() {
    let source = include_str!("../volume_raymarch.rs");
    let mut labels = 0;
    for fragment in source.split("label(\"").skip(1) {
        let (name, _) = fragment.split_once('"').expect("an unterminated label");
        // Skip the definition of `label` itself and the doc comments.
        if name.contains('{') {
            continue;
        }
        labels += 1;
        assert!(
            label(name).starts_with(LABEL_PREFIX),
            "the label helper produced `{}` for `{name}`, which the \
                 uncaptured-error latch would treat as an unrelated error",
            label(name)
        );
    }
    assert!(
        labels >= 10,
        "only {labels} labels were found; the scan is not looking where it \
             thinks it is"
    );
    assert!(
        !source.contains("label: Some(\""),
        "a wgpu descriptor in this module writes a literal label instead of \
             going through `label()`, so it may not carry the \
             `{LABEL_PREFIX}` prefix the error latch keys on"
    );
}

/// The body of one WGSL entry point, from its `{` to the matching `}`.
fn entry_point_body(name: &str) -> &'static str {
    let at = VOLUME_SHADER_WGSL
        .find(&format!("fn {name}("))
        .unwrap_or_else(|| panic!("volume.wgsl declares no `{name}`"));
    let open = VOLUME_SHADER_WGSL[at..]
        .find('{')
        .expect("an entry point with no body");
    let start = at + open;
    let mut depth = 0usize;
    for (offset, byte) in VOLUME_SHADER_WGSL[start..].bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &VOLUME_SHADER_WGSL[start..=start + offset];
                }
            }
            _ => {}
        }
    }
    panic!("`{name}`'s body is not brace-balanced")
}

/// The budget-agreement proofs that bridge to this module's arithmetic — moved
/// here from the floor crate's `constants::tests` at WO-RD. The resolver lives
/// below the raymarch and must not call up into it, so every byte proof that
/// reads [`resident_grid_bytes`]/[`grid_bytes_with_mips`] lives beside the
/// arithmetic instead.
mod budget_agreement {
    use crate::budget_arms::{SHIPPED_VOLUME_LOOP_FRAMES, arms, volume_bytes};
    use egui_wgpu::wgpu;
    use squallar_device_profile::constants::{
        DESKTOP_VOLUME_GRID_CELLS, MOBILE_VOLUME_GRID_CELLS, VOLUME_LUT_BYTES,
        WASM_VOLUME_GRID_CELLS,
    };

    /// The limits a real adapter might report, which every sweep below runs.
    const REPORTED_LIMITS: [u32; 5] = [256, 512, 704, 1024, 2048];

    /// The three budget triples, whatever this target's cascade selected.
    const ALL_ARMS: [(&str, [u32; 3]); 3] = [
        ("wasm", WASM_VOLUME_GRID_CELLS),
        ("mobile", MOBILE_VOLUME_GRID_CELLS),
        ("desktop", DESKTOP_VOLUME_GRID_CELLS),
    ];

    /// A cell triple as a `VoxelShape`, axis order x, y, z — the frontend
    /// mirror of the floor crate's private `constants::shape_of`, restated
    /// because a private const fn does not cross a crate boundary.
    const fn shape_of(cells: [u32; 3]) -> squallar_radar::voxel::VoxelShape {
        squallar_radar::voxel::VoxelShape {
            nx: cells[0] as usize,
            ny: cells[1] as usize,
            nz: cells[2] as usize,
        }
    }

    /// The **3D volume** row of the loop table, executed.
    #[test]
    fn volume_loop_grids_fit_the_application_texture_budget() {
        for (arm, frames) in arms().into_iter().zip(SHIPPED_VOLUME_LOOP_FRAMES) {
            let total = frames * volume_bytes(&arm);
            assert!(
                total <= arm.volume_loop_bytes(),
                "{}: {} resident grids x {} B = {:.1} MiB, over the {} MiB budget",
                arm.name,
                frames,
                volume_bytes(&arm),
                total as f64 / (1024.0 * 1024.0),
                arm.volume_loop_bytes() / (1024 * 1024),
            );
        }
    }

    /// **A full 3D loop leaves room for one live 3D grid beside it.**
    #[test]
    fn a_full_3d_loop_leaves_room_for_a_live_grid_beside_it() {
        for (arm, frames) in arms().into_iter().zip(SHIPPED_VOLUME_LOOP_FRAMES) {
            let resident = frames * volume_bytes(&arm);
            assert!(
                resident + volume_bytes(&arm) <= arm.volume_loop_bytes(),
                "{}: {} resident grids + one live grid = {:.1} MiB against a {} MiB \
                 budget, so a 3D loop beside a live 3D pane makes the store evict \
                 the loop's own oldest frame and rebuild it for ever",
                arm.name,
                frames,
                (resident + volume_bytes(&arm)) as f64 / (1024.0 * 1024.0),
                arm.volume_loop_bytes() / (1024 * 1024),
            );
        }
    }

    /// A 3D loop holds exactly what it marches: the frame list **is** the resident
    /// set.
    #[test]
    fn the_3d_loop_holds_exactly_what_it_marches() {
        for (arm, frames) in arms().into_iter().zip(SHIPPED_VOLUME_LOOP_FRAMES) {
            assert!(frames >= 2, "{}: a one-frame loop is not a loop", arm.name,);
            // The frame count is the tighter of two bounds, computed rather
            // than restated, and it is an *equality* — a list shorter than both
            // bounds allow is history thrown away for nothing, and a longer one
            // is the treadmill this loop kind cannot afford.
            let admits =
                arm.volume_loop_bytes().saturating_sub(volume_bytes(&arm)) / volume_bytes(&arm);
            assert_eq!(
                frames,
                admits.min(arm.loop_render_budget),
                "{}: the budget admits {admits} grids and the loop render budget is \
                 {}, so the frame list should be their minimum, not {}",
                arm.name,
                arm.loop_render_budget,
                frames,
            );
        }
    }

    /// Every arm is held to its own volume budget, exactly as
    /// `one_loop_at_the_floor_gets_the_whole_span_budget` holds it to its
    /// loop budget.
    #[test]
    fn the_volume_grid_fits_the_target_texture_budget() {
        for arm in arms() {
            let total = volume_bytes(&arm);
            assert!(
                total <= arm.volume_texture_bytes,
                "{}: a {:?} grid plus a {VOLUME_LUT_BYTES} B table is {total} B, \
                     over the {} B budget",
                arm.name,
                arm.grid_cells,
                arm.volume_texture_bytes,
            );
        }
    }

    /// The sibling of `the_budget_is_not_slack_enough_to_hide_a_doubling`, and for
    /// the same reason: a ceiling several times the real figure passes the check
    /// above while permitting any axis to be silently doubled.
    #[test]
    fn the_volume_budget_is_not_slack_enough_to_hide_a_doubling() {
        for arm in arms() {
            let total = volume_bytes(&arm);
            assert!(
                total * 2 > arm.volume_texture_bytes,
                "{}: budget {} B is more than twice the actual {total} B — it \
                     would not catch a doubled grid axis",
                arm.name,
                arm.volume_texture_bytes,
            );
        }
    }

    /// **The shape the frontend requests never costs more than the budget it was
    /// computed against — on any device.**
    #[test]
    fn the_requested_shape_never_outgrows_the_budget_it_was_computed_against() {
        for (name, budget) in ALL_ARMS {
            let budget_cells = budget.iter().map(|&n| n as usize).product::<usize>();
            let budget_bytes = crate::raymarch::grid_bytes_with_mips(budget)
                .expect("a shipped budget cannot overflow");
            for limit in REPORTED_LIMITS {
                let shape =
                    squallar_radar::voxel::shape_for_budget(shape_of(budget), limit as usize);
                let cells = [shape.nx as u32, shape.ny as u32, shape.nz as u32];
                assert!(
                    shape.cells() <= budget_cells,
                    "{name} on a {limit}-reporting device: {cells:?} is {} cells \
                     against the {budget_cells} this target budgeted for",
                    shape.cells(),
                );
                let bytes = crate::raymarch::grid_bytes_with_mips(cells)
                    .expect("a derived shape cannot overflow");
                assert!(
                    bytes <= budget_bytes,
                    "{name} on a {limit}-reporting device: {cells:?} costs \
                     {bytes} B of texture against the {budget_bytes} B \
                     {budget:?} was budgeted at",
                );
            }
        }
    }

    /// `voxel::HORIZONTAL_AXIS_MULTIPLE` is the copy alignment expressed in cells,
    /// and this is the only crate that can say so.
    #[test]
    fn the_horizontal_axis_multiple_is_the_copy_alignment_in_cells() {
        assert_eq!(
            squallar_radar::voxel::HORIZONTAL_AXIS_MULTIPLE,
            wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize
                / crate::raymarch::GRID_BYTES_PER_CELL as usize,
        );
        // And that the two it is a quotient of are what the doc says, so a change
        // to either fails by name rather than by cancelling out.
        assert_eq!(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 256);
        assert_eq!(crate::raymarch::GRID_BYTES_PER_CELL, 4);
    }

    /// `voxel::VERTICAL_AXIS_MULTIPLE` is the depth block a 3D texture is laid out
    /// in, and this is the only crate that can say so.
    #[test]
    fn the_vertical_axis_multiple_is_the_texture_depth_block() {
        use crate::raymarch::{CoarseLevel, grid_bytes, grid_bytes_at};
        let multiple = squallar_radar::voxel::VERTICAL_AXIS_MULTIPLE;
        // One level, so this is mip 0's own layout with nothing else folded in.
        let padding = |nz: usize| {
            let cells = [320, 320, nz as u32];
            grid_bytes_at(cells, CoarseLevel::Omitted).expect("a swept shape fits")
                - grid_bytes(cells).expect("a swept shape fits")
        };
        let block = 320 * 320 * (multiple - 1) * crate::raymarch::GRID_BYTES_PER_CELL as usize;
        for k in 1..=4 {
            assert!(
                padding(multiple * k) < block,
                "{} layers is a multiple of {multiple} and is still being padded by \
                 {} B, so the multiple does not describe the layout",
                multiple * k,
                padding(multiple * k),
            );
            assert!(
                padding(multiple * k + 1) >= block,
                "{} layers is one over the multiple and is padded by only {} B, so \
                 rounding the vertical down to {multiple} buys nothing",
                multiple * k + 1,
                padding(multiple * k + 1),
            );
        }
        // Both rungs the vertical is chosen at already sit on it, which is what
        // makes the rounding a constraint on the *leftover* alone.
        assert_eq!(squallar_radar::voxel::NZ_PREFERRED % multiple, 0);
        assert_eq!(squallar_radar::voxel::NZ_MIN % multiple, 0);
    }

    /// The two grid-byte invariants the floor crate's `check_invariants` had to
    /// give up at WO-RD — the byte figure is this module's arithmetic and the
    /// resolver must not call up into it — swept at every promotion a bracket
    /// can reach rather than only at the three shipped floors: the wasm
    /// bracket's ceiling pairs the mobile grid with the wasm pool floor, a pair
    /// no shipped arm exhibits.
    #[test]
    fn every_reachable_grid_fits_its_budgets_in_bytes() {
        use crate::budget_arms::shipped_profile;
        use squallar_device_profile::budget::{BudgetLimits, DeviceProfile, resolve};
        use squallar_device_profile::constants::MIN_LOOP_FRAMES_PER_PANE;
        use squallar_device_profile::quality::DeviceClass;

        for limits in BudgetLimits::SHIPPED {
            // Unknown at the guarantee resolves the floor, Integrated the
            // step, Discrete the ceiling — all three rungs of every bracket.
            for class in [
                DeviceClass::Unknown,
                DeviceClass::Integrated,
                DeviceClass::Discrete,
            ] {
                let b = resolve(&DeviceProfile {
                    class,
                    ..shipped_profile(limits)
                });
                let grid = crate::raymarch::resident_grid_bytes(b.grid_cells)
                    .expect("a bracketed grid cannot overflow");
                // The grid fits its own budget, in bytes as well as in cells.
                assert!(
                    grid <= b.volume_texture_bytes,
                    "{} / {class:?}: a {:?} grid is {grid} B against a {} B budget",
                    b.name,
                    b.grid_cells,
                    b.volume_texture_bytes,
                );
                // One live 3D grid beside a loop at the frame minimum still
                // fits the store floor — the eviction-treadmill guard: a grid
                // that grew without the store growing under it means
                // `enforce_budget` evicts the loop's own frame 0, which the
                // dispatcher re-plans at ~89 ms of resample, for ever.
                let live_beside_a_loop = (MIN_LOOP_FRAMES_PER_PANE + 1) * grid;
                assert!(
                    live_beside_a_loop <= b.volume_loop_bytes(),
                    "{} / {class:?}: a loop at the {}-frame minimum plus one live \
                     grid is {} MiB of {:?} grids against a {} MiB store floor",
                    b.name,
                    MIN_LOOP_FRAMES_PER_PANE,
                    live_beside_a_loop / (1024 * 1024),
                    b.grid_cells,
                    b.volume_loop_bytes() / (1024 * 1024),
                );
            }
        }
    }

    /// A pixel costs what the offscreen's format actually costs.
    #[test]
    fn a_pixel_costs_what_the_offscreen_format_costs() {
        use squallar_device_profile::quality::OFFSCREEN_BYTES_PER_PIXEL;
        let format_bytes = crate::raymarch::OFFSCREEN_FORMAT
            .block_copy_size(None)
            .expect("the offscreen format has no single-aspect copy size");
        assert_eq!(
            OFFSCREEN_BYTES_PER_PIXEL,
            format_bytes as usize,
            "an offscreen pixel is budgeted at {OFFSCREEN_BYTES_PER_PIXEL} B \
             but {:?} costs {format_bytes} B",
            crate::raymarch::OFFSCREEN_FORMAT
        );
    }
}
