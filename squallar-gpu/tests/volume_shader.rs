//! What a WebGL2 browser is actually handed.
#![cfg(not(target_arch = "wasm32"))]

use naga::back::glsl;
use naga::proc::{BoundsCheckPolicies, BoundsCheckPolicy};
use naga::valid::{Capabilities, ValidationFlags, Validator};
use squallar_volumetric::raymarch::{
    BINDING_BLIT_SAMPLER, BINDING_BLIT_TEXTURE, BINDING_FLOOR_SAMPLER, BINDING_FLOOR_TEXTURE,
    BINDING_GRID_SAMPLER, BINDING_GRID_TEXTURE, BINDING_JITTER_TEXTURE, BINDING_LUT_SAMPLER,
    BINDING_LUT_TEXTURE, BINDING_UNIFORM, ENTRY_FS_BLIT_GAMMA, ENTRY_FS_BLIT_LINEAR,
    ENTRY_FS_RAYMARCH, ENTRY_POINTS, ENTRY_VS_BLIT, ENTRY_VS_RAYMARCH, ShaderStage,
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
