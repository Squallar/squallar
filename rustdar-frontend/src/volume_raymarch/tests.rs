use super::*;

/// [`VOLUME_SHADER_WGSL`] with its comments removed.
///
/// Every "the shader must NOT contain X" assertion runs against this rather
/// than the raw source, because the comments in `volume.wgsl` deliberately
/// name the things the shader must not do — `textureNumLevels`,
/// `dt * length(box_size_km)` — so that a reader learns why. Scanning the
/// raw text would make those explanations trip their own guards, and the
/// fix a hurried reader would reach for is deleting the explanation.
///
/// `//` to end of line is the only comment form `volume.wgsl` uses, and
/// WGSL has no string literals for a `//` to hide inside.
fn shader_code() -> String {
    VOLUME_SHADER_WGSL
        .lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The comment-stripper actually strips something, and keeps the code.
///
/// Without this, a `shader_code` that returned an empty string would make
/// every absence assertion below pass vacuously — which is the failure mode
/// of every scan-based test and the reason they need a control.
#[test]
fn the_comment_stripper_removes_prose_and_keeps_code() {
    let code = shader_code();
    assert!(
        code.len() < VOLUME_SHADER_WGSL.len() / 2,
        "volume.wgsl is {} bytes and its code is {} — the stripper is not \
             removing the comments",
        VOLUME_SHADER_WGSL.len(),
        code.len()
    );
    assert!(
        code.contains("fn fs_raymarch(") && code.contains("textureSampleLevel("),
        "the comment stripper removed code as well as comments"
    );
    assert!(
        !code.contains("naga"),
        "a word that appears only in this file's prose survived the stripper"
    );
}

/// The quad is 48 bytes of `vec2<f32>`, and it covers all of clip space.
///
/// The size is the claim the module doc makes; the coverage is the claim
/// the blit's viewport trick rests on. A quad that covered only part of
/// clip space would blit a fraction of the offscreen into the whole pane,
/// which reads as a zoomed-in volume rather than as a broken quad.
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
///
/// A test helper rather than production code: nothing that draws needs the
/// quad grouped into triangles, but a coverage assertion has to talk about
/// triangles — a quad that names all four corners can still fail to tile
/// the rectangle.
fn quad_triangles() -> [[[f32; 2]; 3]; 2] {
    [
        [QUAD_CORNERS[0], QUAD_CORNERS[1], QUAD_CORNERS[2]],
        [QUAD_CORNERS[3], QUAD_CORNERS[4], QUAD_CORNERS[5]],
    ]
}

/// The two triangles tile clip space exactly once, with no gap and no
/// overlap.
///
/// Added after a mutation survived the test above. `QUAD_CORNERS` has six
/// negative components; deleting the minus from **four** of them leaves all
/// four clip-space corners present and the bounding box unchanged, so every
/// assertion up there still passes — while turning the pair into two
/// triangles that both cover the upper half and leave a quadrant of the
/// volume simply not drawn. (The other two are vertex 0's, and the corner
/// check does catch those, because removing either loses `[-1, -1]`
/// entirely.) Corner presence is not coverage, so assert coverage: this
/// test catches all six.
///
/// Sampled at points chosen to miss every edge: the shared diagonal is
/// `x + y = 0`, and `-1.88 + 0.19 * (i + j)` is zero only at a
/// non-integer `i + j`.
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
///
/// This is the whole of bug #2's mitigation on the Rust side, and both arms are
/// reachable — `app_state::preferred_surface_format` prefers a non-sRGB format
/// on wasm32 and prefers `Bgra8Unorm` natively, taking `capabilities.formats[0]`
/// only as a fallback, so an sRGB surface is the rare case rather than the
/// routine native one.
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
///
/// The companion to the blit test above, and for the same reason: the mirror is
/// drawn by the very pipeline whose entry point that test pins, so the two
/// answers have to be the same fact read from the two ends. What this adds is
/// that the fact is a property of *sRGB-ness*, not of a particular format.
///
/// Without it the predicate's only coverage is a fixture precondition inside an
/// `#[ignore]`d GPU test, which exercises one arm at one format. Two mutations
/// that would survive that and die here:
///
///  * dropping the negation (`format.is_srgb()`), which inverts every arm;
///  * narrowing to one format (`format != TextureFormat::Rgba8UnormSrgb`),
///    which is *correct* for `MIRROR_FORMAT` and for the fixture's own arm, and
///    wrong for a `Bgra8UnormSrgb` swapchain — the one an adapter without
///    `Bgra8Unorm` actually lands on.
///
/// The failure mode either way is a floor a little too dark or too light beside
/// a 2D pane that looks right, with no validation error to notice it by.
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
///
/// It holds bytes the raymarch has already encoded. An `Rgba8UnormSrgb`
/// target would have the hardware decode them on the way out, undoing that
/// encode — and the result is plausible, merely washed out.
#[test]
fn the_offscreen_format_is_not_srgb() {
    assert!(!OFFSCREEN_FORMAT.is_srgb());
    assert!(!LUT_FORMAT.is_srgb());
}

/// The blend state is egui's, component for component.
///
/// Written out rather than compared against a copy: `egui_wgpu` does not
/// export the value, so the only thing that can be pinned locally is the
/// literal. The measurement that actually proves the match is
/// `the_blit_matches_egui_exactly_on_both_surface_formats`, which needs a
/// GPU. The alpha component is the half worth staring at — `OneMinusDstAlpha`
/// and `One`, not the `OneMinusSrcAlpha` symmetry invites.
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
///
/// Both directions are load-bearing. A name here that the shader does not
/// declare is a pipeline that fails to create, from a call with no `Result`.
/// A name in the shader that is missing from [`ENTRY_POINTS`] is worse: it
/// is an entry point that ships to a browser having never been translated
/// to GLSL by `tests/volume_shader.rs`.
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
///
/// A binding number that drifts between the WGSL and the bind group layout
/// is a validation error at pipeline creation — from `create_render_pipeline`,
/// which returns no `Result`, so it arrives asynchronously through the
/// uncaptured-error sink instead.
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
        (1, BINDING_FLOOR_TEXTURE, "floor_texture"),
        (1, BINDING_FLOOR_SAMPLER, "floor_sampler"),
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
        bindings, 9,
        "volume.wgsl declares {bindings} bindings; this file names 9, and a \
             binding the layouts do not declare fails pipeline creation"
    );
}

/// One sampler per texture, in each pipeline, as naga requires.
///
/// `Error::ImageMultipleSamplers` is a real naga error, not a convention:
/// a texture sampled through two samplers in one entry point does not
/// translate to GLSL at all, because GLSL's `sampler3D` fuses the two.
#[test]
fn each_texture_has_exactly_one_sampler() {
    let code = shader_code();
    let textures = code.matches(": texture_").count();
    let samplers = code.matches(": sampler;").count();
    assert_eq!(
        (textures, samplers),
        (4, 4),
        "volume.wgsl declares {textures} textures and {samplers} samplers; \
             naga refuses a texture sampled through two samplers in one entry \
             point"
    );
}

/// The shader samples with an explicit level everywhere.
///
/// Implicit-LOD sampling under the march's data-dependent break is
/// `FunctionError::NonUniformControlFlow` — a hard validator failure on
/// every target, not a driver quirk. `textureSample` compiles in a shader
/// with no branching, so this is exactly the edit that would pass review.
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
///
/// naga gates it on GLSL core 130 with no ES version at all, so it is
/// unreachable on WebGL2 forever — and the failure would be at translation
/// time on the browser only, i.e. on the target CI covers least.
#[test]
fn the_shader_never_asks_how_many_mip_levels_there_are() {
    assert!(
        !shader_code().contains("textureNumLevels"),
        "volume.wgsl calls `textureNumLevels`, which naga gates on GLSL \
             core 130 with no ES version at all"
    );
}

/// The step ceiling is a `const`, so it folds to a literal in the loop.
///
/// The ceiling is the loop bound — a naga requirement, since the *real*
/// termination is the data-dependent break at the box exit, and a
/// non-constant bound plus that break is exactly the shape WebGL2 drivers
/// refuse. The step length itself arrives per frame in `flags.z`, so
/// there is deliberately no `(span.y - span.x) / STEPS` here to pin — the
/// dt *floor* against the ceiling is pinned instead, because deleting it
/// would truncate any span that outruns the ceiling mid-box.
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
    // The host-side restatements, against the same literals rather than
    // against the constants themselves — pinning a constant to itself is
    // the mistake `every_lane_lands_at_its_std140_offset` documents. The
    // step-cells half now pins the *uniform default*, which is what the
    // silhouette harness's mirror marches at.
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
///
/// This is the property the coverage channel bought. The previous version
/// of this function excluded no-data zeros from the mean by hand, and it
/// had to: index 0 is *no data*, not a measurement of zero, and averaging
/// it in erased the Harvey eyewall at the default box's 1.8 km cells
/// (-41% of >=50 dBZ pixels, -81% of >=30 dBZ). Premultiplied, a fine cell
/// is `(c*x, c)`; the box mean is `(sum(c*x)/8, sum(c)/8)`; the ratio is
/// `sum(c*x)/sum(c)`. So the exclusion is the arithmetic, not a branch —
/// and the coarse texel *additionally* carries the block's occupancy,
/// which the one-channel level had nowhere to put.
///
/// Five properties, each with a mutation it closes:
///
/// * A uniform fully-covered grid downsamples to itself — a stride error
///   mixing neighbouring blocks cannot be seen on a uniform grid, so this
///   is the control, not the test.
/// * **A lone 255 among seven empties reconstructs to 255.** The
///   data-honesty half: the coarse texel is `(32, 32)` — an eighth of
///   each — and `32/32` is 255/255 in unorm, the lone cell's own value.
///   The old full-cube mean landed at 32 and is what erased the core.
/// * A mixed block reconstructs to its measured cells' own mean: 100 and
///   105 among six empties store `(26, 64)`, which reconstructs near their
///   102.5 rather than near the full-cube 26. The assertion is anchored on
///   102.5 — the contract — with the quantisation tolerance stated, not on
///   the 103.594 this implementation happens to produce; see below.
/// * **The quantisation bound.** Both channels round to u8 before the
///   shader divides and the divisor steps in units of 255/8, so the stored
///   ratio is not the occupancy mean exactly. The error is under 4 index
///   units over every reachable `(n, Σx)` and worst at a single measured
///   cell — `n = 1, x = 4` stores `(1, 32)` and reads back 7.97. Pinning
///   the bound rather than the sample keeps the test honest about which of
///   the two numbers is the promise.
/// * The block that is averaged is the one under the coarse cell —
///   checked with a value planted in a *different* block, which is what a
///   transposed dimension order pushes into the wrong coarse cell.
/// * An all-empty block stays `(0, 0)`: no data, and zero coverage, which
///   is what keeps the shader's floored divisor from inventing an index.
#[test]
fn the_grid_mip_is_the_rounded_mean_of_each_coarse_blocks_measured_cells() {
    /// The grid's own index plane, widened the way `upload_volume` widens
    /// it — through the production function, so this cannot drift from it.
    fn premultiplied(indices: &[u8]) -> Vec<u8> {
        super::coverage_premultiplied(indices)
    }
    /// The shader's reconstruction, in the host's arithmetic: `R_bar` over
    /// `G_bar`, back in 0..=255 index units.
    fn reconstructed(texel: [u8; 2]) -> Option<f32> {
        (texel[1] != 0).then(|| f32::from(texel[0]) / f32::from(texel[1]) * 255.0)
    }

    // Uniform control — every cell covered, so the coarse level is the same
    // value at full coverage.
    let (coarse, bytes) = downsampled_grid([4, 4, 2], &premultiplied(&[7u8; 32]));
    assert_eq!(coarse, [2, 2, 1]);
    assert_eq!(bytes, vec![7, 255, 7, 255, 7, 255, 7, 255]);

    // The all-empty block: no data and no coverage. Nothing divides by zero
    // here or in the shader, whose divisor is floored.
    let (_, bytes) = downsampled_grid([4, 4, 2], &premultiplied(&[0u8; 32]));
    assert_eq!(bytes, vec![0u8; 8], "an unmeasured block must stay no-data");
    assert_eq!(reconstructed([bytes[0], bytes[1]]), None);

    // A lone measured cell: fine cell (0,0,0) of a 4x4x2 grid is in
    // coarse block (0,0,0) and nowhere else, and it keeps its own value.
    let mut fine = vec![0u8; 32];
    fine[0] = 255;
    let (_, bytes) = downsampled_grid([4, 4, 2], &premultiplied(&fine));
    assert_eq!(
        &bytes[..2],
        &[32, 32],
        "an eighth of 255 in both channels; anything else is a stride error"
    );
    assert_eq!(
        reconstructed([bytes[0], bytes[1]]),
        Some(255.0),
        "a lone measured 255 must reconstruct to its own value; 32 is the \
             full-cube mean that erased the Harvey core at coarse cell sizes"
    );
    assert_eq!(&bytes[2..], &[0u8; 6], "it must not reach another block");

    // A mixed block: the measured cells' own mean. Anchored on the CONTRACT
    // — the true mean of {100, 105} — with the quantisation tolerance, not
    // on whatever this implementation's rounding lands at.
    const MIP_QUANTISATION_TOLERANCE: f32 = 4.0;
    let mut fine = vec![0u8; 32];
    fine[0] = 100;
    fine[1] = 105;
    let (_, bytes) = downsampled_grid([4, 4, 2], &premultiplied(&fine));
    assert_eq!(&bytes[..2], &[26, 64]);
    let index = reconstructed([bytes[0], bytes[1]]).expect("the block is covered");
    assert!(
        (index - 102.5).abs() < MIP_QUANTISATION_TOLERANCE,
        "two measured cells among six empties must reconstruct to their own \
             mean of 102.5 (got {index}), not the full-cube 26",
    );

    // The bound itself, over every reachable block: `round8(sum) / round8(255n)`
    // against the true mean. Under 4 index units, worst at one measured cell.
    let round8 = |total: u32| ((total + 4) / 8) as u8;
    let mut worst = 0.0f32;
    let mut worst_at = (0u32, 0u32);
    for n in 1..=8u32 {
        let divisor = f32::from(round8(255 * n));
        for sum in 0..=255 * n {
            let error = f32::from(round8(sum)) / divisor * 255.0 - sum as f32 / n as f32;
            if error.abs() > worst {
                worst = error.abs();
                worst_at = (n, sum);
            }
        }
    }
    assert!(
        worst < MIP_QUANTISATION_TOLERANCE,
        "the mip's worst reconstruction error is {worst} index units at \
             (n, sum) = {worst_at:?}, over the {MIP_QUANTISATION_TOLERANCE} the \
             doc promises — the coarse level has stopped being the occupancy \
             mean to the tolerance the callers were told",
    );
    // The old hand-written `round(sum / n)` was exact to +-0.5, so this bound
    // is a real regression on sparse blocks and is stated as one rather than
    // hidden behind a loose assertion.
    assert!(
        worst > 0.5,
        "the quantised reconstruction is now as tight as the hand mean it \
             replaced; `downsampled_grid`'s doc claims otherwise and one of \
             the two is wrong",
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
    let (_, bytes) = downsampled_grid([4, 4, 2], &premultiplied(&fine));
    assert_eq!(
        bytes,
        vec![0, 0, 100, 255, 0, 0, 0, 0],
        "the filled block must land whole in coarse cell (1,0,0); anything \
             else is a dimension-order error smearing data across the mip"
    );

    // Odd extents follow wgpu's mip arithmetic: max(n / 2, 1). The clamp
    // counts a fine cell more than once — in BOTH channels, so the ratio,
    // and with it the reconstructed index, is untouched.
    let (coarse, bytes) = downsampled_grid([3, 3, 3], &premultiplied(&[9u8; 27]));
    assert_eq!(coarse, [1, 1, 1]);
    assert_eq!(bytes, vec![9, 255]);
    assert_eq!(reconstructed([bytes[0], bytes[1]]), Some(9.0));
}

/// The premultiplied plane is the index byte and a binary coverage beside
/// it — the texture's whole contract, in one place.
///
/// Coverage is `index != NO_DATA_INDEX` and nothing else, which is what
/// licenses the wire format to carry one byte per cell:
/// `rustdar_radar::voxel::ramp_index` clamps every finite measurement to
/// `1..=255`, so the second channel is a function of the first and storing
/// it would be redundancy rather than information.
#[test]
fn the_premultiplied_plane_is_index_and_a_binary_coverage() {
    let indices: Vec<u8> = (0..=255u8).collect();
    let plane = super::coverage_premultiplied(&indices);
    assert_eq!(plane.len(), indices.len() * GRID_BYTES_PER_CELL as usize);
    for (index, pair) in indices.iter().zip(plane.chunks_exact(2)) {
        assert_eq!(
            pair[0], *index,
            "R must be coverage x index, and index 0 \
             is the only one coverage zeroes — which leaves the byte itself"
        );
        assert_eq!(
            pair[1],
            if *index == rustdar_radar::voxel::NO_DATA_INDEX {
                0
            } else {
                u8::MAX
            },
            "coverage at index {index} is not binary on the no-data test",
        );
    }
}

/// The step length puts the ray direction inside the `length`.
///
/// This is spike 0a's first bug and it is worth the source scan, because
/// `dt * length(box_size_km)` compiles, reads plausibly, and on the
/// 240 x 240 x 20 km box makes a vertical ray roughly twelve times more
/// opaque per step than a horizontal one — which looks like haze.
///
/// `a_vertical_and_a_horizontal_ray_agree_on_opacity_per_kilometre` is the
/// property test; this is the one that runs without a GPU.
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

/// The anisotropy the guard above exists to prevent, stated as numbers.
///
/// A source scan pins the text; this pins the *reason*, so a future reader
/// who wants to simplify the shader can see what it costs. Both figures are
/// worth having: the absolute one says how far off a vertical ray is, and
/// the relative one is why the result reads as haze rather than as a bug —
/// the whole image gets denser together, so nothing looks inconsistent.
///
/// The box is the one the volume actually uses: 240 km across, 20 km deep.
#[test]
fn the_wrong_step_length_is_seventeen_times_off_and_twelve_times_anisotropic() {
    let box_size_km = [240.0f64, 240.0, 20.0];
    // The wrong formula, `dt * length(box_size_km)`, gives every direction
    // the box's diagonal.
    let wrong = box_size_km.iter().map(|km| km * km).sum::<f64>().sqrt();

    // The right one gives each axis-aligned ray that axis' own extent.
    let vertical = box_size_km[2];
    let horizontal = box_size_km[0];

    let vertical_inflation = wrong / vertical;
    let horizontal_inflation = wrong / horizontal;
    assert!(
        (16.5..17.5).contains(&vertical_inflation),
        "a vertical step would be {vertical_inflation:.1}x too long, not \
             the ~17x the shader's comment claims"
    );
    assert!(
        (1.3..1.5).contains(&horizontal_inflation),
        "a horizontal step would be {horizontal_inflation:.1}x too long, \
             not the ~1.4x the shader's comment claims"
    );

    let anisotropy = vertical_inflation / horizontal_inflation;
    assert!(
        (11.5..12.5).contains(&anisotropy),
        "the bug would leave a vertical ray {anisotropy:.1}x more opaque \
             relative to a horizontal one, not the ~12x claimed"
    );
    assert!(
        (anisotropy - horizontal / vertical).abs() < 1e-9,
        "the relative distortion is exactly the box's aspect ratio, and \
             this arithmetic no longer says so"
    );
}

/// The sRGB blit decodes the premultiplied value, without un-premultiplying.
///
/// Spike 0a's second finding, and the counter-intuitive one: the principled
/// version measured 60/255 off against egui's own `rect_filled`, and
/// decoding the premultiplied value directly took the delta to 0. A future
/// reader who "fixes" this is making the output wrong, so pin it.
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
///
/// The other half of the colour rule, and the half that *is* principled:
/// encoding an already-premultiplied value is wrong at every alpha but 1.
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
///
/// Rewriting either — a different cutoff, a 2.2 exponent instead of 2.4 —
/// produces output that is wrong by a few counts everywhere, which reads as
/// a slightly different theme rather than as a bug.
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

/// Grid byte counts — one byte per cell on the wire, two in the texture,
/// plus the coarse level — including the overflow the multiplication can hit.
///
/// The three figures are deliberately separate. `cell_count` is what
/// `upload_refusal` measures the caller's index plane against; `grid_bytes`
/// sizes the one-level upload; `grid_bytes_with_mips` is what the memory
/// budget in `constants` is a claim about. Collapsing any two of them was
/// how the coarse level came to be missing from the budget entirely.
#[test]
fn a_grids_byte_count_is_two_per_cell_and_the_budget_counts_the_mip() {
    assert_eq!(cell_count([256, 256, 128]), Some(8 * 1024 * 1024));
    assert_eq!(grid_bytes([256, 256, 128]), Some(16 * 1024 * 1024));
    assert_eq!(
        grid_bytes_with_mips([256, 256, 128]),
        Some(18 * 1024 * 1024),
        "the desktop grid is 16 MiB of premultiplied cells over a 2 MiB \
             coarse level, which is the figure the budget table states"
    );
    assert_eq!(grid_bytes([128, 128, 64]), Some(2 * 1024 * 1024));
    assert_eq!(
        grid_bytes_with_mips([128, 128, 64]),
        Some(2 * 1024 * 1024 + 256 * 1024)
    );
    // Too small to halve on any axis: one level, so the two figures agree.
    assert_eq!(grid_bytes([1, 1, 1]), Some(2));
    assert_eq!(grid_bytes_with_mips([1, 1, 1]), Some(2));
    for overflowing in [cell_count, grid_bytes, grid_bytes_with_mips] {
        assert_eq!(
            overflowing([u32::MAX, u32::MAX, u32::MAX]),
            None,
            "a grid whose cell count overflows `usize` must not wrap to a \
                 small number and then be compared against a slice length"
        );
    }
}

/// An offscreen never has a zero axis, and a real size passes through.
///
/// Both halves: clamping unconditionally to 1 would be as wrong as not
/// clamping at all, and `create_texture` — where this lands — returns no
/// `Result` for either.
#[test]
fn an_offscreen_extent_is_clamped_up_from_zero_and_left_alone_otherwise() {
    assert_eq!(offscreen_extent([0, 0]), [1, 1]);
    assert_eq!(offscreen_extent([0, 900]), [1, 900]);
    assert_eq!(offscreen_extent([1440, 0]), [1440, 1]);
    assert_eq!(offscreen_extent([1440, 900]), [1440, 900]);
}

/// A held offscreen is rebuilt for a new size and kept for the same one.
///
/// The mistake this catches is the comparison inverted: a pane-sized
/// texture reallocated on every frame is invisible in a screenshot and
/// reads as a driver problem rather than as an application one.
#[test]
fn an_offscreen_is_rebuilt_only_when_its_size_changed() {
    assert!(
        offscreen_needs_rebuild(None, [1440, 900]),
        "nothing held must always be built"
    );
    assert!(
        !offscreen_needs_rebuild(Some([1440, 900]), [1440, 900]),
        "an offscreen of the right size was thrown away and rebuilt"
    );
    for changed in [[1441, 900], [1440, 901], [900, 1440]] {
        assert!(
            offscreen_needs_rebuild(Some([1440, 900]), changed),
            "a {changed:?} pane reused a 1440x900 offscreen, so it would be \
                 blitted at the wrong scale"
        );
    }
}

/// An upload whose shapes disagree is refused, and one that agrees is not.
///
/// The three ways to get this wrong are all here: too few index bytes, too
/// many, and a colour table of the wrong length. `write_texture` is a
/// validation error for the first and **silently ignores the tail** for the
/// second, which uploads a plausible volume shifted by a slice.
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
///
/// `install_error_latch` re-panics on any uncaptured error whose message
/// does not carry `rustdar.volume`, under `debug_assertions`. So a resource
/// created here without the prefix converts a survivable driver refusal
/// into an abort — on the target where an abort is a dead browser tab.
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
