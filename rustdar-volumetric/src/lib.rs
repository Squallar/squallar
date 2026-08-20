#![warn(clippy::all)]
#![forbid(unsafe_code)]

//! The 3D volume stack: raymarch pipelines and shader ([`raymarch`]), voxel
//! staging, uniforms ([`uniform`]), blue noise ([`blue_noise`]), the
//! quality-degrade/probe policy ([`degrade`]) and the bridge egui's paint
//! callbacks march through ([`bridge`]).
//!
//! The crate root decides, before anything is created, whether a 3D volume can
//! be rendered at all: `create_texture` and `create_render_pipeline` return no
//! `Result`, their errors arrive through wgpu's uncaptured-error sink, and the
//! default sink *panics* (wgpu-29.0.4 `src/backend/wgpu_core.rs:685-688`) —
//! which on the web is a dead browser tab. Three layers guard it: this probe
//! (synchronous, allocates nothing), the uncaptured-error latch
//! ([`install_error_latch`]) and the two-strike surface-loss counter
//! ([`degrade`]).

use egui_wgpu::wgpu;

use rustdar_device_profile::constants::VOLUME_GRID_CELLS;

#[path = "volume_blue_noise.rs"]
pub mod blue_noise;
#[path = "volume_bridge.rs"]
pub mod bridge;
#[path = "volume_degrade.rs"]
pub mod degrade;
#[path = "volume_raymarch.rs"]
pub mod raymarch;
#[path = "volume_uniform.rs"]
pub mod uniform;

/// Shared per-arm fixtures for the budget agreement tests.
#[cfg(test)]
pub(crate) mod budget_arms;

pub use degrade::VolumeSupport;

/// The texel format a voxel grid is uploaded as: **coverage-premultiplied**
/// palette indices.
///
/// `R = coverage × index`, `G = coverage`, one **half float** each, where
/// coverage is 1 for a measured cell and 0 for empty air. The march samples
/// both channels `Linear` and reconstructs `index = R̄ / Ḡ`, which is the
/// coverage-weighted mean over the covered texels alone — air contributes 0 to
/// numerator and denominator alike, so it drops out of the average instead of
/// taking part in it as a value. See `volume.wgsl`'s `field_at`, and
/// `rustdar_radar::voxel`'s module doc for what that retired.
///
/// Three properties are load-bearing. `Rg16Float` is filterable under
/// `Features::empty()` — `RG16F` is texture-filterable in ES 3.0 (Table 3.13),
/// including for `TEXTURE_3D` — where `R32Float` would need
/// `FLOAT32_FILTERABLE`. The index↔value map is affine, so filtering within
/// data is linear interpolation of the physical value. And float quantisation
/// is *relative*, so the quotient's error is bounded by an ulp however small
/// `Ḡ` is; an 8-bit format's error is absolute — `|Δindex| ≤ 2q / Ḡ`, `q =
/// 1/255` — which is the whole palette one cell out from an echo edge, and
/// `R̄ = c/255` underflows to zero for every coverage under a half.
pub const VOLUME_TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg16Float;

/// The environment variable that turns the volume view off natively.
pub const VOLUME_ENV_VAR: &str = "RUSTDAR_VOLUME";

/// The smallest 3D texture worth rendering a volume into.
///
/// Not the grid size: a device reporting less than the grid can still be
/// stepped down to a coarser one. This is the floor below which there is no
/// useful volume at all — 32 cells over a 40 km half-width is 2.5 km per cell,
/// coarser than the beam.
const MIN_TEXTURE_DIMENSION_3D: u32 = 32;

/// Sampled textures a volume pipeline binds at once: the grid, its colour LUT,
/// the jitter tile and the map floor's pane mirror.
///
/// "Sampled" is wgpu's name for the binding *class*, not a claim that all four
/// are read through a sampler — the jitter tile is read with `textureLoad` and
/// carries none, and occupies a slot regardless. What this has to match is the
/// number of texture bindings the raymarch's layouts declare across both
/// groups. Held to the shader by
/// `the_probe_asks_the_adapter_for_every_binding_the_raymarch_declares`.
const REQUIRED_SAMPLED_TEXTURES: u32 = 4;

/// Samplers a volume pipeline binds at once: the grid's, the LUT's, the floor's.
///
/// One per texture *that is sampled*: naga rejects a texture sampled through
/// two samplers in one entry point (`Error::ImageMultipleSamplers`), and the
/// grid wants `Linear` while an exact-index LUT lookup wants `Nearest`. One
/// fewer than [`REQUIRED_SAMPLED_TEXTURES`] because the jitter tile is read
/// with `textureLoad` and carries no sampler at all.
const REQUIRED_SAMPLERS: u32 = 3;

/// Bytes of uniform data one volume draw needs bound at once.
///
/// The raymarch's uniform block is one `mat4x4<f32>` plus ten `vec4<f32>` — 224
/// bytes, [`uniform::VOLUME_UNIFORM_BYTES`] — and this is the next
/// std140-friendly bound above it. `u64` because
/// `Limits::max_uniform_buffer_binding_size` is, unlike the three counts above.
const REQUIRED_UNIFORM_BINDING_SIZE: u64 = 256;

/// Whether this device can render a 3D volume, decided before anything is made.
///
/// Order goes cheapest and most-likely-to-be-deliberate first: the escape
/// hatch, then limits, then format features. Nothing here allocates.
pub fn probe(adapter: &wgpu::Adapter, limits: &wgpu::Limits) -> VolumeSupport {
    if let Some(off) = disabled_by_environment() {
        return off;
    }
    if let Some(why) = limits_shortfall(limits) {
        return VolumeSupport::Unavailable(why);
    }
    if let Some(why) = format_shortfall(&adapter.get_texture_format_features(VOLUME_TEXTURE_FORMAT))
    {
        return VolumeSupport::Unavailable(why);
    }
    VolumeSupport::Supported
}

/// The probe's answer, overridden by anything that has already gone wrong.
///
/// A device that lost its context twice is not made capable again by passing a
/// limits check, and the probe cannot know about it. Call this rather than
/// reading `AppState::volume_support` directly.
pub fn support(probed: &VolumeSupport) -> VolumeSupport {
    prefer_recorded_failure(degrade::recorded_failure(), probed)
}

/// The precedence rule, separated from the process-global state it reads.
///
/// Split for testability: the statics in [`degrade`] are process-global and
/// never reset, so a test that drove them would be at the mercy of every other
/// test in the binary.
fn prefer_recorded_failure(
    recorded: Option<VolumeSupport>,
    probed: &VolumeSupport,
) -> VolumeSupport {
    recorded.unwrap_or_else(|| probed.clone())
}

/// `RUSTDAR_VOLUME=off`, natively.
#[cfg(not(target_arch = "wasm32"))]
fn disabled_by_environment() -> Option<VolumeSupport> {
    override_from_env_value(std::env::var(VOLUME_ENV_VAR).ok().as_deref())
}

/// A browser has no environment to read, so there is nothing to consult.
#[cfg(target_arch = "wasm32")]
fn disabled_by_environment() -> Option<VolumeSupport> {
    None
}

/// What a `RUSTDAR_VOLUME` value means.
///
/// Takes the value rather than reading the environment so it is testable. Only
/// an explicit `off` disables; anything else — an empty string, a typo, or `on`
/// — leaves the probe to decide.
#[cfg(not(target_arch = "wasm32"))]
fn override_from_env_value(value: Option<&str>) -> Option<VolumeSupport> {
    let value = value?.trim();
    value.eq_ignore_ascii_case("off").then(|| {
        VolumeSupport::Unavailable(format!(
            "The 3D volume view is switched off by {VOLUME_ENV_VAR}={value}."
        ))
    })
}

/// Which limit, if any, rules the volume view out.
///
/// Pure, and takes the whole `Limits` so it can be exercised against synthetic
/// ones — including `Limits::downlevel_webgl2_defaults()`, the floor the web
/// build is actually held to. `pub` because that floor is pinned from outside
/// this module.
pub fn limits_shortfall(limits: &wgpu::Limits) -> Option<String> {
    let grid_axis = VOLUME_GRID_CELLS.iter().copied().max().unwrap_or(0);
    // The grid must fit as well as the floor, so that a device between the two is
    // The grid must fit as well as the floor, so that a device between the two is
    // reported honestly rather than failing later inside a callback. The web arm
    // of `device_limits` lifts this limit via `using_resolution`.
    let needed_3d = grid_axis.max(MIN_TEXTURE_DIMENSION_3D);

    // Widened to `u64` because `max_uniform_buffer_binding_size` is one and the
    // other three are `u32`. `what` names the resource and never counts it.
    for (actual, needed, what) in [
        (
            u64::from(limits.max_texture_dimension_3d),
            u64::from(needed_3d),
            "3D textures large enough to hold a volume",
        ),
        (
            u64::from(limits.max_sampled_textures_per_shader_stage),
            u64::from(REQUIRED_SAMPLED_TEXTURES),
            "sampled textures in one shader stage",
        ),
        (
            u64::from(limits.max_samplers_per_shader_stage),
            u64::from(REQUIRED_SAMPLERS),
            "samplers in one shader stage",
        ),
        (
            limits.max_uniform_buffer_binding_size,
            REQUIRED_UNIFORM_BINDING_SIZE,
            "a uniform block for the camera",
        ),
    ] {
        if actual < needed {
            return Some(format!(
                "The 3D volume view needs {what}: this graphics device reports \
                 {actual} where {needed} is required."
            ));
        }
    }
    None
}

/// Whether the adapter can bind and filter the voxel grid's format.
///
/// Both halves are load-bearing and neither is implied by the other.
/// `TEXTURE_BINDING` makes the grid samplable at all; without `FILTERABLE` a
/// `Linear` sampler is a validation error rather than a fallback to `Nearest`,
/// and `R̄ / Ḡ` is meaningless unless the hardware takes both means under one
/// set of weights.
fn format_shortfall(features: &wgpu::TextureFormatFeatures) -> Option<String> {
    if !features
        .allowed_usages
        .contains(wgpu::TextureUsages::TEXTURE_BINDING)
    {
        return Some(
            "The 3D volume view needs to sample a two-channel texture: this \
             graphics device cannot bind one."
                .to_owned(),
        );
    }
    if !features
        .flags
        .contains(wgpu::TextureFormatFeatureFlags::FILTERABLE)
    {
        return Some(
            "The 3D volume view needs smooth interpolation between radar cells: \
             this graphics device cannot filter a two-channel texture."
                .to_owned(),
        );
    }
    None
}

/// Route uncaptured device errors past wgpu's panicking default.
///
/// `Device::on_uncaptured_error` installs **one** handler for the whole device,
/// replacing wgpu's panicking `default_error_handler` (wgpu-29.0.4
/// `src/backend/wgpu_core.rs:685-688`) for *every* wgpu call in the
/// application. So anything without a [`degrade::VOLUME_LABEL_PREFIX`] label
/// re-panics under `debug_assertions` and is logged in release.
///
/// Consequence: every wgpu resource the volume view creates **must** carry a
/// `rustdar.volume`-prefixed label, or its errors panic the debug build.
pub fn install_error_latch(device: &wgpu::Device) {
    device.on_uncaptured_error(std::sync::Arc::new(|error: wgpu::Error| {
        let rendered = error.to_string();
        match disposition(&rendered, cfg!(debug_assertions)) {
            ErrorDisposition::LatchVolumeFailure => {
                log::error!("3D volume view: the graphics driver rejected a resource: {rendered}");
                degrade::latch_volume_device_error();
            }
            ErrorDisposition::Repanic => panic!(
                "wgpu error unrelated to the volume view, re-raised because \
                 installing an uncaptured-error handler replaced wgpu's own \
                 panicking default: {rendered}"
            ),
            ErrorDisposition::Log => log::error!("wgpu error: {rendered}"),
        }
    }));
}

/// What to do with an uncaptured device error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ErrorDisposition {
    /// The volume view's own. Latch it and carry on.
    LatchVolumeFailure,
    /// Not the volume's, and this build re-raises it, as wgpu's default did.
    Repanic,
    /// Not the volume's, and this build logs it rather than aborting.
    Log,
}

/// The handler's decision, separated from the handler.
///
/// `debug_build` is a parameter rather than `cfg!(debug_assertions)` read
/// inline, so that both arms are reachable from one test binary.
fn disposition(rendered: &str, debug_build: bool) -> ErrorDisposition {
    if degrade::error_belongs_to_volume(rendered) {
        ErrorDisposition::LatchVolumeFailure
    } else if debug_build {
        ErrorDisposition::Repanic
    } else {
        ErrorDisposition::Log
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdar_device_profile::constants::WEBGL2_MAX_TEXTURE_DIMENSION_3D;

    /// What the raymarch's two bind groups declare, counted out of the shader.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
    struct DeclaredBindings {
        /// Counts against `max_sampled_textures_per_shader_stage`.
        sampled_textures: u32,
        /// Counts against `max_samplers_per_shader_stage`.
        samplers: u32,
        /// Uniform blocks. A *count*, where the probe's limit is a size.
        uniform_buffers: u32,
    }

    /// Tally every `@group(...) @binding(...)` the raymarch declares, by class.
    ///
    /// **Exhaustive on purpose.** A binding whose class none of the arms below
    /// recognise is an `Err`, not a skip: a storage buffer or storage texture
    /// would be checked against limits [`limits_shortfall`] never reads. The
    /// blit's own pair is skipped — it belongs to a third layout and a
    /// different pipeline. Text rather than a naga parse because this must fail
    /// when the *source* gains a line.
    fn declared_by_the_raymarch(wgsl: &str) -> Result<DeclaredBindings, String> {
        use crate::raymarch::{BINDING_BLIT_SAMPLER, BINDING_BLIT_TEXTURE};

        let mut tally = DeclaredBindings::default();
        for line in wgsl.lines() {
            let line = line.split("//").next().unwrap_or_default().trim();
            let Some(rest) = line.strip_prefix("@group(") else {
                continue;
            };
            let malformed = || format!("cannot read a group and binding out of {line:?}");
            let (group, rest) = rest.split_once(')').ok_or_else(malformed)?;
            let rest = rest
                .trim_start()
                .strip_prefix("@binding(")
                .ok_or_else(malformed)?;
            let (binding, declaration) = rest.split_once(')').ok_or_else(malformed)?;
            let group: u32 = group.trim().parse().map_err(|_| malformed())?;
            let binding: u32 = binding.trim().parse().map_err(|_| malformed())?;

            if group == 0 && matches!(binding, BINDING_BLIT_TEXTURE | BINDING_BLIT_SAMPLER) {
                continue;
            }

            let declaration = declaration.trim().strip_prefix("var").ok_or_else(|| {
                format!(
                    "{line:?} carries a binding attribute with no `var` after it on the same \
                         line, so its class cannot be told and the limit it costs cannot be \
                         counted; keep each binding on one line",
                )
            })?;

            // `var<uniform>` and `var<storage>` carry their class in the address
            // space; a handle binding carries it in the type after the colon.
            if let Some(space) = declaration.strip_prefix('<') {
                let space = space.split_once('>').ok_or_else(malformed)?.0;
                match space.split(',').next().unwrap_or_default().trim() {
                    "uniform" => tally.uniform_buffers += 1,
                    other => {
                        return Err(format!(
                            "{line:?} binds a `{other}` buffer, which is checked against a limit \
                             the volume probe does not read — add it to `limits_shortfall` \
                             before adding it to the shader",
                        ));
                    }
                }
                continue;
            }

            let ty = declaration
                .split_once(':')
                .ok_or_else(malformed)?
                .1
                .trim()
                .trim_end_matches(';')
                .trim();
            let head = ty.split('<').next().unwrap_or_default();
            match head {
                "sampler" | "sampler_comparison" => tally.samplers += 1,
                // Ordered before the general texture arm: a storage texture is a
                // `texture_` that counts against a different limit entirely.
                _ if head.starts_with("texture_storage") => {
                    return Err(format!(
                        "{line:?} binds a storage texture, which counts against \
                         `max_storage_textures_per_shader_stage` — a limit the volume probe \
                         does not read",
                    ));
                }
                _ if head.starts_with("texture_") => tally.sampled_textures += 1,
                other => {
                    return Err(format!(
                        "{line:?} binds a `{other}`, which the volume probe has no arm for; \
                         decide which limit it costs before the shader depends on it",
                    ));
                }
            }
        }
        Ok(tally)
    }

    /// Every count the probe asks the adapter for is the shader's own.
    #[test]
    fn the_probe_asks_the_adapter_for_every_binding_the_raymarch_declares() {
        use crate::raymarch::VOLUME_SHADER_WGSL;

        let declared = declared_by_the_raymarch(VOLUME_SHADER_WGSL)
            .unwrap_or_else(|why| panic!("the raymarch's bindings do not read: {why}"));
        assert_eq!(
            declared,
            DeclaredBindings {
                sampled_textures: REQUIRED_SAMPLED_TEXTURES,
                samplers: REQUIRED_SAMPLERS,
                uniform_buffers: 1,
            },
            "the raymarch declares {declared:?} and the probe asks the adapter for \
             {REQUIRED_SAMPLED_TEXTURES} textures and {REQUIRED_SAMPLERS} samplers; an adapter \
             between those numbers passes this probe and then fails pipeline creation into the \
             sink that panics",
        );
        assert!(
            uniform::VOLUME_UNIFORM_BYTES as u64 <= REQUIRED_UNIFORM_BINDING_SIZE,
            "the uniform block is {} bytes and the probe only asks the adapter for \
             {REQUIRED_UNIFORM_BINDING_SIZE}",
            uniform::VOLUME_UNIFORM_BYTES,
        );
    }

    /// And the tally moves when the shader does — for a sampler specifically.
    #[test]
    fn one_more_binding_in_the_shader_is_one_more_in_the_tally() {
        use crate::raymarch::VOLUME_SHADER_WGSL;

        let shipped = declared_by_the_raymarch(VOLUME_SHADER_WGSL).expect("the shipped shader");
        for (added, expected) in [
            (
                "@group(1) @binding(2) var extra_sampler: sampler;",
                DeclaredBindings {
                    samplers: shipped.samplers + 1,
                    ..shipped
                },
            ),
            (
                "@group(1) @binding(2) var extra_texture: texture_2d<f32>;",
                DeclaredBindings {
                    sampled_textures: shipped.sampled_textures + 1,
                    ..shipped
                },
            ),
            (
                "@group(1) @binding(2) var<uniform> extra: Volume;",
                DeclaredBindings {
                    uniform_buffers: shipped.uniform_buffers + 1,
                    ..shipped
                },
            ),
        ] {
            let grown = format!("{VOLUME_SHADER_WGSL}\n{added}\n");
            assert_eq!(
                declared_by_the_raymarch(&grown).expect("a grown shader still reads"),
                expected,
                "adding {added:?} did not move the tally, so the probe's constants could go \
                 stale against it exactly as they have twice",
            );
        }
    }

    /// A binding class the probe cannot price is refused, not skipped.
    #[test]
    fn a_binding_the_probe_prices_no_limit_for_fails_the_scan() {
        use crate::raymarch::VOLUME_SHADER_WGSL;

        for unpriced in [
            "@group(1) @binding(2) var<storage, read> extra: array<f32>;",
            "@group(1) @binding(2) var extra: texture_storage_2d<rgba8unorm, write>;",
        ] {
            let grown = format!("{VOLUME_SHADER_WGSL}\n{unpriced}\n");
            let why = declared_by_the_raymarch(&grown).expect_err(
                "an unpriced binding class was tallied as though the probe checked for it",
            );
            assert!(
                why.contains("limit"),
                "the refusal does not say which limit is missing: {why:?}",
            );
        }
    }

    /// The blit's pair is not charged to the raymarch.
    #[test]
    fn the_blits_own_pair_is_left_out_of_the_raymarchs_tally() {
        use crate::raymarch::{BINDING_BLIT_SAMPLER, BINDING_BLIT_TEXTURE, VOLUME_SHADER_WGSL};

        let shipped = declared_by_the_raymarch(VOLUME_SHADER_WGSL).expect("the shipped shader");
        let with_blit = format!(
            "{VOLUME_SHADER_WGSL}\n\
             @group(0) @binding({BINDING_BLIT_TEXTURE}) var again: texture_2d<f32>;\n\
             @group(0) @binding({BINDING_BLIT_SAMPLER}) var again_sampler: sampler;\n",
        );
        assert_eq!(
            declared_by_the_raymarch(&with_blit).expect("the blit's bindings still read"),
            shipped,
            "the blit's pair is being charged to the raymarch's layouts",
        );
    }

    /// One limit lowered below what the probe requires.
    type LowerOneLimit = fn(&mut wgpu::Limits);

    /// The WebGL2 floor — the least capable device this build targets — passes.
    #[test]
    fn the_guaranteed_webgl2_limits_are_enough_for_a_volume() {
        assert_eq!(
            limits_shortfall(&wgpu::Limits::downlevel_webgl2_defaults()),
            None,
            "the probe rejects the WebGL2 guarantee itself, so no conforming \
             browser could ever render a volume"
        );
    }

    /// And so does the unlifted 256-cell 3D floor, specifically.
    /// `device_limits`' web arm calls `using_resolution`, which raises
    /// `max_texture_dimension_3d` to whatever the adapter reports, so in
    /// practice this is usually higher.
    #[test]
    fn the_grid_fits_the_unlifted_3d_texture_floor() {
        let mut floor = wgpu::Limits::downlevel_webgl2_defaults();
        floor.max_texture_dimension_3d = WEBGL2_MAX_TEXTURE_DIMENSION_3D;
        assert_eq!(limits_shortfall(&floor), None);
        assert!(
            VOLUME_GRID_CELLS
                .iter()
                .all(|&n| n <= WEBGL2_MAX_TEXTURE_DIMENSION_3D)
        );
    }

    /// Every threshold is load-bearing: lowering any one of them alone refuses.
    #[test]
    fn each_limit_the_probe_names_can_refuse_on_its_own() {
        let ok = wgpu::Limits::downlevel_webgl2_defaults();
        let lowered: [(&str, LowerOneLimit); 4] = [
            ("max_texture_dimension_3d", |l| {
                l.max_texture_dimension_3d = MIN_TEXTURE_DIMENSION_3D - 1;
            }),
            ("max_sampled_textures_per_shader_stage", |l| {
                l.max_sampled_textures_per_shader_stage = REQUIRED_SAMPLED_TEXTURES - 1;
            }),
            ("max_samplers_per_shader_stage", |l| {
                l.max_samplers_per_shader_stage = REQUIRED_SAMPLERS - 1;
            }),
            ("max_uniform_buffer_binding_size", |l| {
                l.max_uniform_buffer_binding_size = REQUIRED_UNIFORM_BINDING_SIZE - 1;
            }),
        ];

        for (limit, lower) in lowered {
            let mut limits = ok.clone();
            lower(&mut limits);
            let why = limits_shortfall(&limits).unwrap_or_else(|| {
                panic!("the probe accepts a device whose {limit} is below what it requires")
            });
            assert!(
                why.ends_with('.') && why.contains("3D volume view"),
                "the reason for refusing on {limit} is not a user-readable \
                 sentence: {why:?}"
            );
        }
    }

    /// A grid that outgrows a device's 3D limit is refused, not silently clamped.
    /// The threshold the probe applies is `max(grid axis, 32)`, so a device
    /// between the two is caught here rather than inside a callback where there
    /// is no `Result`.
    #[test]
    fn a_device_that_cannot_hold_the_grid_is_refused() {
        let grid_axis = VOLUME_GRID_CELLS.iter().copied().max().unwrap();
        let mut limits = wgpu::Limits::downlevel_webgl2_defaults();
        limits.max_texture_dimension_3d = grid_axis - 1;
        assert!(
            limits_shortfall(&limits).is_some(),
            "a device that cannot hold the {grid_axis}-cell grid was accepted"
        );

        limits.max_texture_dimension_3d = grid_axis;
        assert_eq!(limits_shortfall(&limits), None);
    }

    /// The two format-feature halves refuse independently.
    #[test]
    fn both_format_features_are_required_separately() {
        let usable = wgpu::TextureFormatFeatures {
            allowed_usages: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            flags: wgpu::TextureFormatFeatureFlags::FILTERABLE,
        };
        assert_eq!(format_shortfall(&usable), None);

        let unbindable = wgpu::TextureFormatFeatures {
            allowed_usages: wgpu::TextureUsages::COPY_DST,
            ..usable
        };
        assert!(
            format_shortfall(&unbindable).is_some_and(|why| why.contains("cannot bind")),
            "a format that cannot be sampled was accepted"
        );

        let unfilterable = wgpu::TextureFormatFeatures {
            flags: wgpu::TextureFormatFeatureFlags::empty(),
            ..usable
        };
        assert!(
            format_shortfall(&unfilterable).is_some_and(|why| why.contains("cannot filter")),
            "a format that cannot be filtered was accepted, which makes the \
             Linear sampler a validation error rather than a fallback"
        );
    }

    /// Only an explicit `off` switches the view off.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn the_environment_override_needs_an_explicit_off() {
        for value in ["off", "OFF", " off ", "Off"] {
            let state = override_from_env_value(Some(value))
                .unwrap_or_else(|| panic!("{VOLUME_ENV_VAR}={value:?} did not switch 3D off"));
            assert!(!state.is_supported());
            assert!(
                state
                    .reason()
                    .is_some_and(|why| why.contains(VOLUME_ENV_VAR)),
                "the reason must name the variable, so a user who set it can \
                 find it again: {state:?}"
            );
        }
    }

    /// Anything else leaves the decision to the probe.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn an_unrecognised_environment_value_does_not_switch_anything_off() {
        for value in [
            None,
            Some(""),
            Some("  "),
            Some("on"),
            Some("1"),
            Some("of"),
        ] {
            assert_eq!(
                override_from_env_value(value),
                None,
                "{VOLUME_ENV_VAR}={value:?} switched 3D off, which no value but \
                 `off` may do"
            );
        }
    }

    /// A recorded failure outranks a probe that says the device is fine.
    /// A probe that runs at construction cannot know about a context that died
    /// afterwards. Driven through the pure rule rather than the statics: the
    /// degrade counters are deliberately never reset, so no test may depend on
    /// their value.
    #[test]
    fn a_recorded_failure_outranks_the_probes_answer() {
        let probed_fine = VolumeSupport::Supported;
        let probed_refused = VolumeSupport::Unavailable("probe said no.".to_owned());
        let recorded = VolumeSupport::Unavailable("the device already died.".to_owned());

        assert_eq!(
            prefer_recorded_failure(None, &probed_fine),
            VolumeSupport::Supported,
            "nothing recorded must leave the probe's answer alone"
        );
        assert_eq!(
            prefer_recorded_failure(None, &probed_refused),
            probed_refused
        );
        assert_eq!(
            prefer_recorded_failure(Some(recorded.clone()), &probed_fine),
            recorded,
            "a device that has already failed was reported as usable because the \
             probe, which ran before the failure, said so"
        );
    }

    /// The volume's own errors are latched, never re-raised, in either build.
    #[test]
    fn a_volume_error_is_latched_in_debug_and_release_alike() {
        let volume_error = "In Device::create_render_pipeline, label = 'rustdar.volume.raymarch'";
        for debug_build in [true, false] {
            assert_eq!(
                disposition(volume_error, debug_build),
                ErrorDisposition::LatchVolumeFailure,
                "a volume error was not latched with debug_assertions={debug_build}"
            );
        }
    }

    /// An unrelated error still panics the debug build, as wgpu's default did.
    #[test]
    fn an_unrelated_error_still_aborts_a_debug_build() {
        for rendered in [
            "In Device::create_texture, label = 'egui sampler'",
            "In Queue::write_buffer",
            "Out of Memory",
        ] {
            assert_eq!(
                disposition(rendered, true),
                ErrorDisposition::Repanic,
                "an unrelated wgpu error would be swallowed by a debug build: \
                 {rendered:?}"
            );
        }
    }

    /// In release it is logged instead, because the user's app is worth more.
    #[test]
    fn an_unrelated_error_is_logged_rather_than_fatal_in_release() {
        assert_eq!(
            disposition("In Queue::write_buffer", false),
            ErrorDisposition::Log
        );
    }

    /// The limits the app *requests* clear the floor the volume probe applies.
    /// The app side's `AppState::new` requests these limits (through
    /// `rustdar_gpu::device::request_device`), the device grants exactly them,
    /// and [`probe`] reads them back off the device — so this is the real path,
    /// not a restatement.
    #[test]
    fn the_web_limits_this_app_requests_clear_the_volume_probes_floor() {
        use rustdar_gpu::device::device_limits;

        // The least capable browser this build targets: an adapter reporting
        // exactly the WebGL2 guarantee and not a pixel more.
        let barest = wgpu::Limits::downlevel_webgl2_defaults();
        assert_eq!(
            limits_shortfall(&device_limits(barest, true)),
            None,
            "the volume probe rejects the very limits this app asks a browser \
             for, so the 3D view could never be available in one"
        );

        // And on a capable browser, where `using_resolution` lifts the 3D
        // texture bound well past the grid.
        assert_eq!(
            limits_shortfall(&device_limits(wgpu::Limits::default(), true)),
            None
        );

        // Native asks for the adapter's own limits, so any adapter that could
        // run the app at all clears the floor too.
        for adapter in [wgpu::Limits::default(), wgpu::Limits::downlevel_defaults()] {
            assert_eq!(limits_shortfall(&device_limits(adapter, false)), None);
        }
    }

    /// The probe agrees with a real adapter, and installing the latch is safe.
    /// The probe's two halves are unit-tested against synthetic limits above;
    /// what only a device can show is that `Rg16Float` really is bindable and
    /// filterable under `Features::empty()`, the premise the format choice
    /// rests on. Ignored by default — CI's `gpu` job in `test.yaml` names this
    /// test explicitly, so renaming it means editing that job. Passes on Mesa's
    /// lavapipe.
    #[test]
    #[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_real_adapter_supports_the_volume_format() {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .expect("no wgpu adapter; this test is ignored by default for that reason");

        let features = adapter.get_texture_format_features(VOLUME_TEXTURE_FORMAT);
        assert_eq!(
            format_shortfall(&features),
            None,
            "a real adapter cannot bind or filter {VOLUME_TEXTURE_FORMAT:?}, \
             which is the premise the Rg16Float choice rests on. Features: \
             {features:?}"
        );

        let (device, _queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: None,
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                memory_hints: Default::default(),
                experimental_features: Default::default(),
                trace: Default::default(),
            }))
            .expect("could not create a device on an adapter that was found");

        assert_eq!(
            probe(&adapter, &device.limits()),
            VolumeSupport::Supported,
            "a real adapter fails the volume probe"
        );

        // Installing the latch must not itself trip anything: the handler
        // re-panics on unrelated errors under `debug_assertions`.
        install_error_latch(&device);
    }
}
