//! What a WebGL2 browser is actually handed.
//!
//! `src/volume.wgsl` is WGSL, and every target but one compiles it directly.
//! The browser does not: wgpu's WebGL2 backend runs it through naga's GLSL
//! backend first, and what the driver sees is that output. This file produces
//! it, for every entry point, under the options wgpu-hal itself uses — so that
//! a translation failure is a red CI row rather than a blank pane in Firefox.
//!
//! # The options matter more than the test does
//!
//! `glsl::Options::default()` is **ES 310** and emits `layout(binding = …)`,
//! which WebGL2 forbids outright. A test run against the defaults would pass
//! happily while proving nothing about the target it exists for. So the options
//! here are transcribed from `wgpu-hal-29.0.4/src/gles/device.rs`:
//!
//! * `Version::Embedded { version: 300, is_webgl }` — `adapter.rs:279-298`
//!   derives the version from the context's own `SHADING_LANGUAGE_VERSION` and
//!   sets `is_webgl` from `cfg!(any(webgl, Emscripten))`. WebGL2's is ES 3.00.
//! * `ADJUST_COORDINATE_SPACE | FORCE_POINT_SIZE` — `device.rs:1183-1198`. The
//!   other two flags are driven by private capabilities a browser does not
//!   have.
//! * a `binding_map` built with **per-binding-type counters**, one pipeline
//!   layout at a time — `device.rs:1199-1243`. Counters restart at zero for
//!   each pipeline, which is why the raymarch and the blit are translated
//!   against separate maps rather than one combined one.
//! * `zero_initialize_workgroup_memory: true` — `device.rs:1259`.
//! * `BoundsCheckPolicies` all `Unchecked` — `device.rs:253-268`. The
//!   `ReadZeroSkipWrite` arm is selected only for desktop GL 4.3 and above,
//!   because the image bounds check needs `TEXTURE_LEVELS`; it is unreachable
//!   on ES.
//!
//! # What this does and does not establish
//!
//! It establishes that the generated GLSL is **legal ES 300**: naga's own
//! validator accepted the module, the backend emitted every entry point, and
//! the output carries none of the constructs ES 300 lacks.
//!
//! It does **not** establish that the program links in a real browser. Nothing
//! in this repository does — spike 0a could not test it, because the machine it
//! ran on has no display and a software-rasteriser result would have meant
//! nothing. A driver may still refuse a program naga emitted correctly, which
//! is what `volume::install_error_latch` and `volume::degrade` are for.
//!
//! Gated off wasm32 because the dev-dependency is: a `#[cfg]`-empty test file
//! with an unused dependency fails the wasm clippy row.
#![cfg(not(target_arch = "wasm32"))]

use naga::back::glsl;
use naga::proc::{BoundsCheckPolicies, BoundsCheckPolicy};
use naga::valid::{Capabilities, ValidationFlags, Validator};
use rustdar_frontend::volume::raymarch::{
    BINDING_BLIT_SAMPLER, BINDING_BLIT_TEXTURE, BINDING_FLOOR_SAMPLER, BINDING_FLOOR_TEXTURE,
    BINDING_GRID_SAMPLER, BINDING_GRID_TEXTURE, BINDING_LUT_SAMPLER, BINDING_LUT_TEXTURE,
    BINDING_UNIFORM, ENTRY_FS_BLIT_GAMMA, ENTRY_FS_BLIT_LINEAR, ENTRY_FS_RAYMARCH, ENTRY_POINTS,
    ENTRY_VS_BLIT, ENTRY_VS_RAYMARCH, ShaderStage, VOLUME_SHADER_WGSL,
};

/// What kind of resource one bind group layout entry is.
///
/// Only the three kinds the volume pipelines use. wgpu-hal keeps a counter per
/// kind and hands each binding the counter's value before incrementing it, so
/// the *kind* is what decides the slot, not the binding number.
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
///
/// One counter per binding *type*, shared across every group in the pipeline
/// layout, incremented after each entry. Writing `binding` into the map instead
/// would be the plausible mistake and would be right only by coincidence — here
/// it happens to differ for the LUT's texture and sampler, which is exactly why
/// the raymarch layout is worth transcribing rather than guessing.
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
///
/// This is the check that would catch `textureSample` under the march's
/// data-dependent break: implicit-LOD sampling in non-uniform control flow is
/// `FunctionError::NonUniformControlFlow`, and it fails here rather than on one
/// unlucky driver.
#[test]
fn the_volume_shader_is_valid_wgsl() {
    let (module, _) = validated_module();
    assert!(
        !module.entry_points.is_empty(),
        "the module validated but declares no entry points at all"
    );
}

/// The shader's entry points are exactly the ones the crate names.
///
/// Stronger than the source scan in `volume_raymarch.rs`, because this asks
/// naga rather than a substring search — and it is the list this file iterates,
/// so an entry point missing from it is one that never gets translated.
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
///
/// Three constructs are checked by name, and each is a real ES 300 gap rather
/// than a style preference:
///
/// * `layout(binding` — explicit binding points are ES 310 and desktop GL 4.2.
///   WebGL2 rejects the qualifier outright, so a single occurrence is a shader
///   that cannot compile in a browser.
/// * `textureQueryLevels` — what `textureNumLevels` becomes. Gated on GLSL core
///   130 with no ES version at all.
/// * `#version 300 es` — the header, asserted so that a future options change
///   that silently reverted to ES 310 cannot pass by emitting nothing else new.
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
///
/// `is_webgl` is set from `cfg!(any(webgl, Emscripten))` inside wgpu-hal, so
/// the desktop Linux and Android GLES paths compile the *same* WGSL with the
/// flag off while the browser compiles it with the flag on. Identical output
/// means a bug found on one is a bug found on all three, and — more usefully —
/// that testing the native GLES path locally is testing the browser's.
///
/// If this ever fails it is not a defect: it means the shader has grown a
/// construct naga treats differently in a browser, and the browser's arm then
/// needs its own coverage.
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
///
/// Corrects an assumption worth writing down, because it was in the brief this
/// work came from: naga does **not** delete constant declarations and inline
/// them. It emits `const int RAYMARCH_STEP_CEILING = 1024;` at module scope and
/// names it in the loop bound, folding only where a conversion forces it —
/// `f32(RAYMARCH_STEP_CEILING)` becomes the literal `1024.0` inside the `dt`
/// floor. Both are compile-time constant expressions to an ES 300 driver,
/// which is the property that matters: the bound cannot vary per draw. (The
/// march *breaks* at the box exit long before the ceiling on every shipped
/// grid; the data-dependent break is legal where a data-dependent bound is
/// not.)
///
/// The failure this rules out is the ceiling becoming a uniform. That
/// compiles, looks identical, and makes the march's worst case invisible to
/// the driver — on the target where fill rate is the whole risk.
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
///
/// A control on the three assertions above: they are all absences, and an
/// absence is satisfied by an empty shader. This is the presence that says the
/// thing being translated is the raymarch rather than a stub — `sampler3D` and
/// `textureLod` are what `texture_3d` and `textureSampleLevel` become.
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
///
/// wgpu-hal counts per binding *type*, so the LUT's texture is slot 1 while its
/// sampler is slot 1 as well — two counters, not one. Writing the binding
/// number into the map is the plausible mistake, and on the blit's layout it
/// would even look right.
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
        slot(1, BINDING_FLOOR_TEXTURE),
        2,
        "the floor's texture must continue the pipeline-wide texture counter \
         across the group boundary, not restart it"
    );
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
