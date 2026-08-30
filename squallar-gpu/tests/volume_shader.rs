//! What a WebGL2 browser is actually handed.
#![cfg(not(target_arch = "wasm32"))]

use naga::back::glsl;
use naga::proc::{BoundsCheckPolicies, BoundsCheckPolicy};
use naga::valid::{Capabilities, ValidationFlags, Validator};
use squallar_volumetric::raymarch::{
    BINDING_BLIT_SAMPLER, BINDING_BLIT_TEXTURE, BINDING_FLOOR_SAMPLER, BINDING_FLOOR_TEXTURE,
    BINDING_GRID_SAMPLER, BINDING_GRID_TEXTURE, BINDING_GROUND_TEXTURE, BINDING_JITTER_TEXTURE,
    BINDING_LUT_SAMPLER, BINDING_LUT_TEXTURE, BINDING_OCCLUDER_TEXTURE, BINDING_UNIFORM,
    ENTRY_FS_BLIT_GAMMA, ENTRY_FS_BLIT_LINEAR, ENTRY_FS_GROUND, ENTRY_FS_RAYMARCH, ENTRY_POINTS,
    ENTRY_VS_BLIT, ENTRY_VS_GROUND, ENTRY_VS_RAYMARCH, GROUND_POSTS, ShaderStage,
    VOLUME_SHADER_WGSL,
};

/// What kind of resource one bind group layout entry is.
#[derive(Clone, Copy)]
enum BindingKind {
    UniformBuffer,
    Texture,
    Sampler,
}

/// One pipeline layout: `(group, binding, kind)` in declaration order —
/// groups in ascending order, entries in binding order within each, which is
/// the order wgpu-hal walks them.
type Layout = &'static [(u32, u32, BindingKind)];

/// The raymarch's bind group layout, in the order `create_bind_group_layout`
/// declares it.
const RAYMARCH_LAYOUT: Layout = &[
    (0, BINDING_UNIFORM, BindingKind::UniformBuffer),
    (0, BINDING_GRID_TEXTURE, BindingKind::Texture),
    (0, BINDING_GRID_SAMPLER, BindingKind::Sampler),
    (0, BINDING_LUT_TEXTURE, BindingKind::Texture),
    (0, BINDING_LUT_SAMPLER, BindingKind::Sampler),
    // The jitter tile: a texture with no sampler after it, because the shader
    // reaches it with `textureLoad`. It still takes a texture counter slot,
    // which is what pushes the floor's texture to 3 below.
    (0, BINDING_JITTER_TEXTURE, BindingKind::Texture),
    // The floor rides group 1, and the counters do NOT restart: wgpu-hal
    // keeps one counter per binding type across the whole pipeline layout.
    (1, BINDING_FLOOR_TEXTURE, BindingKind::Texture),
    (1, BINDING_FLOOR_SAMPLER, BindingKind::Sampler),
    // The ground pass's two outputs, read back at group 2. Two textures and no
    // sampler after either: both are reached with `textureLoad`. They are last,
    // so the floor's slots do not move.
    (2, BINDING_OCCLUDER_TEXTURE, BindingKind::Texture),
    (2, BINDING_GROUND_TEXTURE, BindingKind::Texture),
];

/// The ground pipeline's, which is group 0 alone — its two stages read the
/// camera out of the shared uniform block and nothing else. Required as its own
/// row because `layout_for` **panics** on an entry point it has no layout for,
/// and appending the occluder to `RAYMARCH_LAYOUT` would not have supplied one.
const GROUND_LAYOUT: Layout = &[
    (0, BINDING_UNIFORM, BindingKind::UniformBuffer),
    (0, BINDING_GRID_TEXTURE, BindingKind::Texture),
    (0, BINDING_GRID_SAMPLER, BindingKind::Sampler),
    (0, BINDING_LUT_TEXTURE, BindingKind::Texture),
    (0, BINDING_LUT_SAMPLER, BindingKind::Sampler),
    (0, BINDING_JITTER_TEXTURE, BindingKind::Texture),
];

/// The blit's, whose counters restart at zero — which is the whole reason the
/// two are kept apart.
const BLIT_LAYOUT: Layout = &[
    (0, BINDING_BLIT_TEXTURE, BindingKind::Texture),
    (0, BINDING_BLIT_SAMPLER, BindingKind::Sampler),
];

/// Which layout an entry point is compiled against.
fn layout_for(entry_point: &str) -> Layout {
    match entry_point {
        ENTRY_VS_RAYMARCH | ENTRY_FS_RAYMARCH => RAYMARCH_LAYOUT,
        ENTRY_VS_GROUND | ENTRY_FS_GROUND => GROUND_LAYOUT,
        ENTRY_VS_BLIT | ENTRY_FS_BLIT_GAMMA | ENTRY_FS_BLIT_LINEAR => BLIT_LAYOUT,
        other => panic!("no pipeline layout is declared for the entry point `{other}`"),
    }
}

/// Build a binding map the way `wgpu-hal/src/gles/device.rs:1219-1243` does.
fn binding_map(layout: Layout) -> glsl::BindingMap {
    let mut samplers = 0u8;
    let mut textures = 0u8;
    let mut uniform_buffers = 0u8;
    let mut map = glsl::BindingMap::default();

    for &(group, binding, kind) in layout {
        let counter = match kind {
            BindingKind::Sampler => &mut samplers,
            BindingKind::Texture => &mut textures,
            BindingKind::UniformBuffer => &mut uniform_buffers,
        };
        map.insert(naga::ResourceBinding { group, binding }, *counter);
        *counter += 1;
    }
    map
}

/// The writer options wgpu-hal would build for a WebGL2 or native-GLES device.
fn writer_options(is_webgl: bool, layout: Layout) -> glsl::Options {
    glsl::Options {
        version: glsl::Version::Embedded {
            version: 300,
            is_webgl,
        },
        writer_flags: glsl::WriterFlags::ADJUST_COORDINATE_SPACE
            | glsl::WriterFlags::FORCE_POINT_SIZE,
        binding_map: binding_map(layout),
        zero_initialize_workgroup_memory: true,
    }
}

/// The bounds-check policies the GLES backend selects on an embedded context.
fn policies() -> BoundsCheckPolicies {
    BoundsCheckPolicies {
        index: BoundsCheckPolicy::Unchecked,
        buffer: BoundsCheckPolicy::Unchecked,
        // `ReadZeroSkipWrite` needs the TEXTURE_LEVELS feature, which is desktop
        // GL 4.3 and above. `gl.version().is_embedded` is true for every target
        // this shader reaches through the GLES backend, so `Unchecked` is the
        // arm that runs.
        image_load: BoundsCheckPolicy::Unchecked,
        binding_array: BoundsCheckPolicy::Unchecked,
    }
}

/// Parse and validate the shader once. Panics with naga's own diagnostic.
fn validated_module() -> (naga::Module, naga::valid::ModuleInfo) {
    let module = naga::front::wgsl::parse_str(VOLUME_SHADER_WGSL).unwrap_or_else(|error| {
        panic!(
            "src/volume.wgsl is not valid WGSL:\n{}",
            error.emit_to_string(VOLUME_SHADER_WGSL)
        )
    });
    let info = Validator::new(ValidationFlags::all(), Capabilities::empty())
        .validate(&module)
        .unwrap_or_else(|error| {
            panic!(
                "src/volume.wgsl does not pass naga's validator:\n{}",
                error.emit_to_string(VOLUME_SHADER_WGSL)
            )
        });
    (module, info)
}

/// Translate one entry point to GLSL ES 300.
fn translate(entry_point: &str, stage: naga::ShaderStage, is_webgl: bool) -> String {
    let (module, info) = validated_module();
    let options = writer_options(is_webgl, layout_for(entry_point));
    let pipeline_options = glsl::PipelineOptions {
        shader_stage: stage,
        entry_point: entry_point.to_owned(),
        multiview: None,
    };

    let mut glsl_source = String::new();
    let mut writer = glsl::Writer::new(
        &mut glsl_source,
        &module,
        &info,
        &options,
        &pipeline_options,
        policies(),
    )
    .unwrap_or_else(|error| panic!("`{entry_point}` cannot be set up for GLSL ES 300: {error}"));
    writer.write().unwrap_or_else(|error| {
        panic!("`{entry_point}` does not translate to GLSL ES 300: {error}")
    });
    drop(writer);
    glsl_source
}

/// naga's stage for one of ours.
fn naga_stage(stage: ShaderStage) -> naga::ShaderStage {
    match stage {
        ShaderStage::Vertex => naga::ShaderStage::Vertex,
        ShaderStage::Fragment => naga::ShaderStage::Fragment,
    }
}

/// The shader parses and passes naga's own validator.
#[test]
fn the_volume_shader_is_valid_wgsl() {
    let (module, _) = validated_module();
    assert!(
        !module.entry_points.is_empty(),
        "the module validated but declares no entry points at all"
    );
}

/// The shader's entry points are exactly the ones the crate names.
#[test]
fn the_shaders_entry_points_are_exactly_the_ones_the_crate_lists() {
    let (module, _) = validated_module();

    let mut found: Vec<(String, naga::ShaderStage)> = module
        .entry_points
        .iter()
        .map(|entry| (entry.name.clone(), entry.stage))
        .collect();
    let mut expected: Vec<(String, naga::ShaderStage)> = ENTRY_POINTS
        .iter()
        .map(|&(name, stage)| (name.to_owned(), naga_stage(stage)))
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    expected.sort_by(|a, b| a.0.cmp(&b.0));

    assert_eq!(
        found, expected,
        "src/volume.wgsl's entry points and `ENTRY_POINTS` disagree. An entry \
         point the list omits is never translated to GLSL by this file, so it \
         reaches a WebGL2 browser having been checked by nothing."
    );
}

/// Every entry point translates, and the output is legal ES 300.
#[test]
fn every_entry_point_translates_to_legal_glsl_es_300() {
    for (name, stage) in ENTRY_POINTS {
        for is_webgl in [true, false] {
            let glsl_source = translate(name, naga_stage(stage), is_webgl);

            assert!(
                glsl_source.starts_with("#version 300 es"),
                "`{name}` (is_webgl={is_webgl}) was emitted for something other \
                 than ES 300; it begins {:?}",
                glsl_source.lines().next().unwrap_or_default()
            );
            assert!(
                !glsl_source.contains("layout(binding"),
                "`{name}` (is_webgl={is_webgl}) carries an explicit binding \
                 qualifier, which is ES 310 and above. WebGL2 rejects it, so \
                 this shader could not compile in a browser."
            );
            assert!(
                !glsl_source.contains("textureQueryLevels"),
                "`{name}` (is_webgl={is_webgl}) queries the mip level count, \
                 which has no ES version at all"
            );
            assert!(
                glsl_source.len() > 200,
                "`{name}` (is_webgl={is_webgl}) translated to {} bytes, which \
                 is too little to be a shader — the assertions above would then \
                 all pass vacuously",
                glsl_source.len()
            );
        }
    }
}

/// The browser's translation and the native GLES one are byte-identical.
#[test]
fn the_webgl_and_native_gles_translations_are_byte_identical() {
    for (name, stage) in ENTRY_POINTS {
        let webgl = translate(name, naga_stage(stage), true);
        let native = translate(name, naga_stage(stage), false);
        assert_eq!(
            webgl, native,
            "`{name}` translates differently for WebGL2 than for native GLES, \
             so the two paths are no longer the same shader and the native one \
             stops being evidence about the browser"
        );
    }
}

/// The march's step ceiling reaches the GLSL as a compile-time constant.
#[test]
fn the_step_count_reaches_the_glsl_as_a_compile_time_constant() {
    let glsl_source = translate(ENTRY_FS_RAYMARCH, naga::ShaderStage::Fragment, true);
    assert!(
        glsl_source.contains("const int RAYMARCH_STEP_CEILING = 1024;"),
        "the step ceiling is no longer a GLSL `const` at module scope"
    );
    assert!(
        !glsl_source.contains("float(RAYMARCH_STEP_CEILING)"),
        "the conversion to float did not fold, so the constant is reaching the \
         arithmetic through a runtime cast"
    );
    assert!(
        glsl_source.contains("/ 1024.0)"),
        "the dt floor is no longer divided by the folded literal 1024.0"
    );
    assert!(
        !glsl_source.contains("uniform int RAYMARCH_STEP_CEILING")
            && !glsl_source.contains("uniform highp int RAYMARCH_STEP_CEILING"),
        "the step ceiling became a uniform; the loop bound would then vary per \
         draw and no driver could unroll it"
    );
}

/// The raymarch really does sample a 3D texture, and with an explicit level.
#[test]
fn the_translated_raymarch_samples_a_3d_texture_at_an_explicit_level() {
    let glsl_source = translate(ENTRY_FS_RAYMARCH, naga::ShaderStage::Fragment, true);
    assert!(
        glsl_source.contains("sampler3D"),
        "the translated raymarch binds no 3D sampler"
    );
    assert!(
        glsl_source.contains("textureLod("),
        "the translated raymarch samples without an explicit level, which is \
         what `textureSampleLevel` exists to prevent"
    );
}

/// The binding map is built the way wgpu-hal builds it.
#[test]
fn the_binding_map_counts_per_binding_type_the_way_wgpu_hal_does() {
    let raymarch = binding_map(RAYMARCH_LAYOUT);
    let slot = |group: u32, binding: u32| {
        *raymarch
            .get(&naga::ResourceBinding { group, binding })
            .unwrap_or_else(|| panic!("group {group} binding {binding} is missing from the map"))
    };
    assert_eq!(slot(0, BINDING_UNIFORM), 0);
    assert_eq!(slot(0, BINDING_GRID_TEXTURE), 0);
    assert_eq!(slot(0, BINDING_GRID_SAMPLER), 0);
    assert_eq!(
        slot(0, BINDING_LUT_TEXTURE),
        1,
        "the second texture did not take the texture counter's second slot"
    );
    assert_eq!(
        slot(0, BINDING_LUT_SAMPLER),
        1,
        "the second sampler did not take the sampler counter's second slot"
    );
    assert_ne!(
        slot(0, BINDING_LUT_TEXTURE),
        BINDING_LUT_TEXTURE as u8,
        "the map is writing binding numbers rather than per-type counters; on \
         this layout those differ, which is what makes the difference visible"
    );
    assert_eq!(
        slot(0, BINDING_JITTER_TEXTURE),
        2,
        "the jitter tile did not take the texture counter's third slot; it carries no sampler, \
         but it is still a texture and still consumes a texture slot"
    );
    assert_eq!(
        slot(1, BINDING_FLOOR_TEXTURE),
        3,
        "the floor's texture must continue the pipeline-wide texture counter \
         across the group boundary, not restart it"
    );
    // Two, not three: the jitter tile added a texture without adding a
    // sampler, so the two counters have diverged. That divergence is the
    // sharpest thing this test now checks — a map that keyed off binding
    // numbers, or that counted both types together, gets this wrong.
    assert_eq!(slot(1, BINDING_FLOOR_SAMPLER), 2);
    // Group 2 continues the same counter again, and adds no sampler at all —
    // which is why the floor's sampler slot above did not move when the ground
    // pass landed.
    assert_eq!(
        slot(2, BINDING_OCCLUDER_TEXTURE),
        4,
        "the occluder must take the texture counter's fifth slot, after the \
         floor's fourth — a group boundary does not restart it"
    );
    assert_eq!(slot(2, BINDING_GROUND_TEXTURE), 5);

    let blit = binding_map(BLIT_LAYOUT);
    for binding in [BINDING_BLIT_TEXTURE, BINDING_BLIT_SAMPLER] {
        assert_eq!(
            blit.get(&naga::ResourceBinding { group: 0, binding }),
            Some(&0),
            "the blit's counters do not restart at zero; wgpu-hal builds one \
             map per pipeline layout, not one per device"
        );
    }
}

/// **No entry point emits a shadow sampler, in either translation mode.**
///
/// The single highest-value assertion in this file. Occlusion travels as the
/// march's own ray parameter packed into an `Rgba8Unorm` *because a depth
/// texture cannot be read at all* on this build's web and Android-GLES targets:
/// naga hard-errors on `textureLoad` from depth, and `textureSampleLevel` on one
/// maps to `sampler2DShadow` and emits a `textureLod` overload that does not
/// exist in GLSL ES 3.00 — which fails at *driver* compile, silently, on a
/// target no Rust test executes. This is the mechanical tripwire that stops the
/// depth route being reintroduced by someone who does not have that finding.
#[test]
fn no_entry_point_emits_a_shadow_sampler() {
    for (name, stage) in ENTRY_POINTS {
        for is_webgl in [true, false] {
            let glsl_source = translate(name, naga_stage(stage), is_webgl);
            for banned in [
                "sampler2DShadow",
                "sampler2DArrayShadow",
                "samplerCubeShadow",
            ] {
                assert!(
                    !glsl_source.contains(banned),
                    "`{name}` (is_webgl={is_webgl}) emits `{banned}`. Something \
                     in this shader is reading a depth texture, and the ES 3.00 \
                     `textureLod` overload for one does not exist — the driver \
                     rejects it at compile time in a browser, where no Rust test \
                     runs. Occlusion travels as a packed ray parameter in an \
                     `Rgba8Unorm` precisely so this never appears."
                );
            }
        }
    }
}

/// And the tripwire is not vacuous: the very construct it forbids does emit
/// the sampler, translated through this same writer with this same policy.
#[test]
fn the_shadow_sampler_tripwire_notices_a_depth_texture() {
    // The design that was rejected, in miniature: read the occluder as depth.
    const DEPTH_READ: &str = "\
@group(0) @binding(0) var occluder_depth: texture_depth_2d;
@group(0) @binding(1) var occluder_sampler: sampler_comparison;

@fragment
fn fs_depth(@builtin(position) at: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = at.xy * 0.001;
    return vec4<f32>(textureSampleCompareLevel(occluder_depth, occluder_sampler, uv, 0.5));
}
";
    let module = naga::front::wgsl::parse_str(DEPTH_READ).expect("the counter-example is valid");
    let info = Validator::new(ValidationFlags::all(), Capabilities::empty())
        .validate(&module)
        .expect("the counter-example validates");
    const DEPTH_LAYOUT: Layout = &[(0, 0, BindingKind::Texture), (0, 1, BindingKind::Sampler)];
    let options = writer_options(true, DEPTH_LAYOUT);
    let pipeline_options = glsl::PipelineOptions {
        shader_stage: naga::ShaderStage::Fragment,
        entry_point: "fs_depth".to_owned(),
        multiview: None,
    };
    let mut glsl_source = String::new();
    let mut writer = glsl::Writer::new(
        &mut glsl_source,
        &module,
        &info,
        &options,
        &pipeline_options,
        policies(),
    )
    .expect("the counter-example can be set up for GLSL ES 300");
    writer.write().expect("the counter-example translates");
    drop(writer);
    assert!(
        glsl_source.contains("sampler2DShadow"),
        "reading a depth texture did NOT emit `sampler2DShadow`, so the \
         assertion above proves nothing about the shipped shader. naga's \
         mapping has changed and the whole occluder-as-packed-`t` argument \
         needs re-deriving, not this test relaxing"
    );
}

/// **The march clips against the ground before it derives its step.**
///
/// A source-order rule, because it cannot be checked any other way: `jitter`
/// and `dt` are both computed *from* `span`, so a clamp applied after them is a
/// depth test rather than a clip — it looks right from one angle and wrong from
/// the next, a ray entering above a ridge and leaving over a valley still
/// accumulating underground samples.
fn clip_precedes_the_step(shader: &str) -> Result<(), String> {
    let source = without_comments(shader);
    let at = |needle: &str| {
        source
            .find(needle)
            .ok_or_else(|| format!("the march no longer contains `{needle}`"))
    };
    let clip = at("span.y = min(")?;
    for after in ["let jitter", "let dt ="] {
        let derived = at(after)?;
        if clip >= derived {
            return Err(format!(
                "`span.y = min(` is at {clip} and `{after}` at {derived}: the \
                 ground clamp lands after the step is derived from `span`, so \
                 the march depth-tests against the ground instead of clipping \
                 to it"
            ));
        }
    }
    Ok(())
}

#[test]
fn the_ground_clamp_precedes_the_jitter_and_the_step() {
    if let Err(why) = clip_precedes_the_step(VOLUME_SHADER_WGSL) {
        panic!("{why}");
    }
}

/// **The order rule's own mutants.** Each is a way the clamp stops being a
/// clip. Re-anchor rather than delete.
#[test]
fn the_order_rule_rejects_every_way_the_clamp_can_slip_past_the_step() {
    const CLIP: &str = "    if ground_t >= 0.0 {\n        span.y = min(span.y, ground_t);\n    }\n";
    // (name, from, to, must be rejected)
    let mutants: [(&str, &str, &str, bool); 3] = [
        (
            "the clamp deleted outright, which is the plain depth-test build",
            CLIP,
            "",
            true,
        ),
        (
            "the clamp moved below the step, so `dt` is derived from the \
             unclipped span",
            CLIP,
            "",
            true,
        ),
        (
            "CONTROL: the clamp spelled with the operands the other way round, \
             which is the same shader",
            "span.y = min(span.y, ground_t);",
            "span.y = min(ground_t, span.y);",
            false,
        ),
    ];

    for (index, (name, from, to, must_reject)) in mutants.into_iter().enumerate() {
        assert!(
            VOLUME_SHADER_WGSL.contains(from),
            "{name}: the anchor is gone, so this mutant is not being applied to \
             anything — re-anchor it rather than deleting it",
        );
        let mut mutated = VOLUME_SHADER_WGSL.replacen(from, to, 1);
        // The second mutant is the first one's deletion plus a re-insertion
        // after the step, which is the edit a reviewer would actually make.
        if index == 1 {
            const ANCHOR: &str = "    var t = span.x + jitter * dt;";
            assert!(
                mutated.contains(ANCHOR),
                "{name}: the re-insertion point is gone — re-anchor it",
            );
            mutated = mutated.replacen(ANCHOR, &format!("{CLIP}{ANCHOR}"), 1);
        }
        match (clip_precedes_the_step(&mutated), must_reject) {
            (Err(_), true) | (Ok(()), false) => {}
            (Ok(()), true) => panic!("{name}: the rule accepted it"),
            (Err(why), false) => panic!("{name}: the rule rejected a correct shader: {why}"),
        }
    }
}

/// **Every group-2 fetch is clamped into the texture it reads.**
///
/// A source rule, because the target it protects is one no Rust test executes.
/// `fs_raymarch` calls `occluder_at` **unconditionally**, before the
/// `volume.occluder.x > 0.0` sentinel — and production binds `GroundPass::Off`,
/// so group 2 is the **1x1** placeholder while `clip_position.xy` runs to the
/// full pane. On the GLES arm `image_load` is `BoundsCheckPolicy::Unchecked`
/// (see `policies()` above), so an unclamped `texelFetch` there is undefined on
/// every pixel of every 3D frame in a browser. Deleting the clamp is
/// byte-identical on a desktop Vulkan adapter, which is why nothing but this
/// notices.
fn group_two_fetches_are_clamped(shader: &str) -> Result<(), String> {
    let source = without_comments(shader);
    for name in ["occluder_at", "ground_colour_at"] {
        let at = source.find(&format!("fn {name}(")).ok_or_else(|| {
            format!("`{name}` is gone; re-anchor this rule rather than deleting it")
        })?;
        let body = &source[at..];
        let body = &body[..body
            .find("\n}")
            .ok_or_else(|| format!("`{name}` has no closing brace"))?];
        if !body.contains("textureLoad(") {
            return Err(format!("`{name}` no longer loads a texel at all"));
        }
        if !body.contains("clamp(") {
            return Err(format!(
                "`{name}` reaches a texel with no `clamp`. The march calls it \
                 before it knows whether a ground pass ran, and the placeholder \
                 bound when none did is 1x1 — on GLES that is an unchecked \
                 out-of-range `texelFetch` on every pixel of every frame",
            ));
        }
        if !body.contains("textureDimensions(") {
            return Err(format!(
                "`{name}` clamps against something other than the texture's own \
                 dimensions, so the bound is not the one the fetch needs",
            ));
        }
    }
    // And the fetch really is unguarded, which is *why* the clamp carries the
    // whole weight. If a sentinel is ever put in front of it this rule should
    // be re-derived rather than kept out of habit.
    if !source.contains("let occluder = occluder_at(in.clip_position.xy);") {
        return Err(
            "the march no longer fetches the occluder at the top of `fs_raymarch`; \
             re-derive this rule against wherever it moved to"
                .into(),
        );
    }
    Ok(())
}

#[test]
fn every_group_two_fetch_is_clamped_into_its_own_texture() {
    if let Err(why) = group_two_fetches_are_clamped(VOLUME_SHADER_WGSL) {
        panic!("{why}");
    }
}

/// **The clamp rule's own mutants.** Re-anchor rather than delete.
#[test]
fn the_clamp_rule_rejects_an_unguarded_texel_fetch() {
    // (name, from, to, must be rejected)
    let mutants: [(&str, &str, &str, bool); 3] = [
        (
            "the occluder's clamp deleted, which is byte-identical on a desktop \
             adapter and undefined on GLES",
            "    let at = clamp(vec2<i32>(px), vec2<i32>(0), dims - vec2<i32>(1));\n    return textureLoad(occluder_texture, at, 0);",
            "    return textureLoad(occluder_texture, vec2<i32>(px), 0);",
            true,
        ),
        (
            "the ground colour's clamp deleted",
            "    let at = clamp(vec2<i32>(px), vec2<i32>(0), dims - vec2<i32>(1));\n    return textureLoad(ground_texture, at, 0);",
            "    return textureLoad(ground_texture, vec2<i32>(px), 0);",
            true,
        ),
        (
            "CONTROL: the clamp's bounds spelled with the same values in a \
             different order, which is the same shader",
            "clamp(vec2<i32>(px), vec2<i32>(0), dims - vec2<i32>(1))",
            "clamp(vec2<i32>(px), vec2<i32>(0, 0), dims - vec2<i32>(1, 1))",
            false,
        ),
    ];

    for (name, from, to, must_reject) in mutants {
        assert!(
            VOLUME_SHADER_WGSL.contains(from),
            "{name}: the anchor is gone, so this mutant is not being applied to \
             anything — re-anchor it rather than deleting it",
        );
        let mutated = VOLUME_SHADER_WGSL.replacen(from, to, 1);
        match (group_two_fetches_are_clamped(&mutated), must_reject) {
            (Err(_), true) | (Ok(()), false) => {}
            (Ok(()), true) => panic!("{name}: the rule accepted an unguarded fetch"),
            (Err(why), false) => panic!("{name}: the rule rejected a correct shader: {why}"),
        }
    }
}
/// The ground grid's post count is one number, not two that agree by
/// inspection: the draw's vertex count is derived from the Rust one.
#[test]
fn the_shader_and_the_ground_post_count_agree() {
    let expected = format!("const GROUND_POSTS: u32 = {GROUND_POSTS}u;");
    assert!(
        VOLUME_SHADER_WGSL.contains(&expected),
        "volume.wgsl does not declare `{expected}`, so the grid the vertex \
         stage lays out and the vertex count the draw issues describe different \
         meshes — the tail of the grid would simply not be drawn, or the last \
         row would wrap"
    );
}

/// The ground pass writes two colour targets, and the second one is not
/// optional: the raymarch pass clears the offscreen, so a ground colour written
/// there would be destroyed before anything read it.
#[test]
fn the_ground_pass_writes_both_an_occluder_and_a_colour() {
    let source = without_comments(VOLUME_SHADER_WGSL);
    for location in ["@location(0) occluder:", "@location(1) colour:"] {
        assert!(
            source.contains(location),
            "the ground pass no longer declares `{location}`. With one target \
             there is no path for the ground's colour to reach the screen at \
             all, and the occluder control cannot see that — a correctly \
             clipped volume over an empty background differs in exactly the \
             direction it asserts"
        );
    }
}

/// **`GRADIENT_EPSILON` says what it is measured in, and what it is worth in
/// a box the user can actually be in.**
#[test]
fn the_gradient_epsilon_states_its_metric_and_what_it_is_worth() {
    let decl = VOLUME_SHADER_WGSL
        .find("const GRADIENT_EPSILON")
        .expect("the constant is no longer where this test looks for it");
    // The run of comment lines immediately above the declaration: everything
    // since the last blank line.
    let doc = VOLUME_SHADER_WGSL[..decl]
        .rsplit("\n\n")
        .next()
        .expect("rsplit always yields at least one piece");
    assert!(
        doc.lines().all(|line| line.trim_start().starts_with("//")),
        "the block above the constant is not all comment; this test is reading \
         the wrong thing",
    );

    for claim in [
        // The metric, both halves of it.
        "palette index",
        "per displayed kilometre",
        // The box the figure is quoted at, so it is a real configuration
        // rather than an abstraction.
        "460 km",
        // And a ratio against the constant, which is what makes it judgeable.
        "GRADIENT_EPSILON",
    ] {
        assert!(
            doc.contains(claim),
            "GRADIENT_EPSILON's doc no longer says {claim:?}, so the value \
             cannot be judged from it: a reader sees 1e-6 against a magnitude \
             whose units are set two functions away",
        );
    }
}

/// **The floor composite's arm is decided by the frame, never by the ray.**
fn without_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    loop {
        let line = rest.find("//");
        let block = rest.find("/*");
        let Some(at) = line.into_iter().chain(block).min() else {
            break;
        };
        out.push_str(&rest[..at]);
        out.push(' ');
        let is_line = line == Some(at);
        let closer = if is_line { "\n" } else { "*/" };
        let after = &rest[at + 2..];
        match after.find(closer) {
            // The newline stays: it terminates the *next* line comment.
            Some(end) if is_line => rest = &after[end..],
            Some(end) => rest = &after[end + closer.len()..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Every identifier in `src`, deduplicated, in source order.
fn identifiers(src: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for word in src.split(|c: char| !c.is_alphanumeric() && c != '_') {
        if word.is_empty() || word.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        if !names.iter().any(|n| n == word) {
            names.push(word.to_string());
        }
    }
    names
}

/// The rule itself, over any shader source, so it can be aimed at a mutant.
fn frame_uniform_arm(shader: &str) -> Result<(), String> {
    let source = without_comments(shader);

    const BINDING: &str = "let eye_above_plane =";
    let at = source
        .find(BINDING)
        .ok_or("the composite no longer binds its arm to a name this can read")?;
    if source.matches(BINDING).count() != 1 {
        return Err(
            "the arm is bound in more than one place, so this test cannot say \
             which one the composite reads"
                .into(),
        );
    }
    // The whole composite, not the binding alone. Both edits that got past the
    // first version of this rule left the binding untouched and put the per-ray
    // discriminant back in a **branch condition**: `floor_t > span.x &&
    // eye_above_plane` is the pre-fix arm exactly for every above-plane camera,
    // and the same edit on the second arm deletes the map floor outright for
    // every camera under the plane. Both were valid WGSL, both translated to
    // legal GLSL ES 300, and both passed all 714 tests.
    let composite = &source[at..];
    let composite = &composite[..composite
        .find("let alpha =")
        .ok_or("the composite no longer ends at the alpha it feeds")?];

    let expression = composite[BINDING.len()..]
        .split(';')
        .next()
        .expect("split always yields at least one piece")
        .trim();

    // The eye's height, and nothing else. `floor_fade` is scaled by the same
    // number, and if the two disagree about which side of the plane the camera
    // is on, the fade and the order it scales are describing different frames.
    const FRAME_UNIFORM: [&str; 4] = ["eye", "z", "volume", "eye_in_box"];
    for name in identifiers(expression) {
        if !FRAME_UNIFORM.contains(&name.as_str()) {
            return Err(format!(
                "the composite's arm is `{expression}`, which reads `{name}`. The \
                 arm may read the eye's height and nothing else — anything varying \
                 per pixel is how one frame came to composite 193 pixels behind the \
                 volume and 1172 in front of it. If `{name}` really is \
                 frame-uniform, add it to `FRAME_UNIFORM` deliberately",
            ));
        }
    }
    if !expression.contains("eye_in_box.z") && !expression.contains("eye.z") {
        return Err(format!(
            "the composite's arm is `{expression}`, which does not read the eye's \
             height at all",
        ));
    }

    // Both branch conditions, not just the first. That one was checked before
    // by a literal `&& eye_above_plane {`, which also rejected the identical
    // `eye_above_plane && floor_t >= 0.0` with the message "nothing branches on
    // `eye_above_plane`" — brittle in the direction that fails a correct shader,
    // and blind in the direction that passes a broken one.
    let conditions: Vec<&str> = composite
        .match_indices("if ")
        .map(|(at, _)| {
            let after = &composite[at + 3..];
            after.find('{').map_or(after, |end| &after[..end]).trim()
        })
        .collect();
    if conditions.len() != 2 {
        return Err(format!(
            "the composite is no longer the two arms this rule knows how to read: \
             {conditions:?}",
        ));
    }

    // The second arm is the `else`, so the frame's verdict reaches it by the
    // first not having been taken rather than by being named again. What both
    // must obey is the whitelist: `floor_t` belongs here, because "does this ray
    // meet the plane at all" is genuinely per-ray and always was, and so does
    // `floor_fade`, the same frame-uniform height read once more. What may not
    // appear in either is anything that decides *which arm* — which is what
    // `span` was.
    if !conditions[0].contains("eye_above_plane") {
        return Err(format!(
            "the arm is chosen by `{}`, which does not read the frame's own \
             verdict — a binding nothing branches on proves nothing about what \
             the composite does",
            conditions[0],
        ));
    }
    const ARM_CONDITION: [&str; 3] = ["floor_t", "eye_above_plane", "floor_fade"];
    for condition in &conditions {
        for name in identifiers(condition) {
            if !ARM_CONDITION.contains(&name.as_str()) {
                return Err(format!(
                    "the arm is chosen by `{condition}`, which reads `{name}`. \
                     Which side of the plane the camera is on is a property of the \
                     frame; asking it of a per-pixel quantity is the defect this \
                     pins, and it does not matter whether that quantity is spelled \
                     `span.x` or bound to a local first",
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn the_floor_composites_arm_is_a_property_of_the_frame() {
    if let Err(why) = frame_uniform_arm(VOLUME_SHADER_WGSL) {
        panic!("{why}");
    }
}

/// **The rule's own mutants.** Every source below is a way the defect comes
/// back, and every one of them passed this test — and the other 713 — when the
/// rule was a blacklist of per-pixel names over the binding expression alone.
#[test]
fn the_arm_rule_rejects_every_way_the_defect_can_come_back() {
    // (name, from, to, must be rejected)
    let mutants: [(&str, &str, &str, bool); 5] = [
        (
            "the pre-fix per-ray discriminant, back in the first arm",
            "if floor_t >= 0.0 && eye_above_plane {",
            "if floor_t > span.x && eye_above_plane {",
            true,
        ),
        (
            "the same edit on the else arm, which deletes the floor below the plane",
            "} else if floor_t >= 0.0 && floor_fade > 0.0 {",
            "} else if floor_t > span.x && floor_fade > 0.0 {",
            true,
        ),
        (
            "a live per-pixel term hidden behind a semicolon inside a comment",
            "let eye_above_plane = eye.z >= 0.0;",
            "let eye_above_plane = eye.z >= 0.0 // one number; the frame's own\n        && floor_t > span.x;",
            true,
        ),
        (
            "the box entry aliased to a local, so no forbidden name appears",
            "if floor_t >= 0.0 && eye_above_plane {",
            "let entry = span.x;\n    if floor_t > entry && eye_above_plane {",
            true,
        ),
        (
            "CONTROL: the operands swapped, which is the same shader",
            "if floor_t >= 0.0 && eye_above_plane {",
            "if eye_above_plane && floor_t >= 0.0 {",
            false,
        ),
    ];

    for (name, from, to, must_reject) in mutants {
        assert!(
            VOLUME_SHADER_WGSL.contains(from),
            "{name}: the anchor `{from}` is gone, so this mutant is not being \
             applied to anything — re-anchor it rather than deleting it",
        );
        let mutated = VOLUME_SHADER_WGSL.replacen(from, to, 1);
        match (frame_uniform_arm(&mutated), must_reject) {
            (Err(_), true) | (Ok(()), false) => {}
            (Ok(()), true) => panic!(
                "{name}: the rule accepted a shader that decides the floor's arm \
                 per pixel",
            ),
            (Err(why), false) => panic!("{name}: the rule rejected a correct shader: {why}"),
        }
    }
}
