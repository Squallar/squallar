#![warn(clippy::all)]
#![forbid(unsafe_code)]

//! The 3D volume stack: raymarch pipelines and shader ([`raymarch`]), voxel
//! staging, uniforms ([`uniform`]), blue noise ([`blue_noise`]), the
//! quality-degrade/probe policy ([`degrade`]) and the bridge egui's paint
//! callbacks march through ([`bridge`]).
//!
//! The crate root itself decides, before anything is created, whether a 3D
//! volume can be rendered at all.
//!
//! The volume view's failure mode is worse than a missing feature: the calls that
//! would fail — `create_texture`, `create_render_pipeline` — return no `Result`,
//! their errors arrive asynchronously through wgpu's uncaptured-error sink, and
//! the default sink *panics* (wgpu-29.0.4
//! `src/backend/wgpu_core.rs:685-688`). On the web a panic aborts the whole
//! module, which is a dead browser tab.
//!
//! So there are three layers, in order of how much they cost:
//!
//! 1. **This probe.** Synchronous, before a single volume resource exists, purely
//!    from limits the device already reports and format features the adapter
//!    already knows. Nothing is allocated, so nothing can fail.
//! 2. **The uncaptured-error latch** ([`install_error_latch`]), for what the probe
//!    cannot see — a shader a driver refuses despite every limit being satisfied.
//! 3. **The two-strike surface-loss counter** ([`degrade`]), for the case where
//!    the failure is a dead graphics context rather than an error at all.
//!
//! Only the first is in this module's own hands. The other two are recovery, and
//! their state deliberately lives outside `AppState` — see [`degrade`].

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

/// Shared per-arm fixtures for the budget agreement tests that ride with the
/// raymarch. The frontend keeps its own twin for the tests that stayed
/// app-side (WO-RD's rule: a test helper does not cross a crate boundary).
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
/// Three properties are load-bearing and none survives changing this format
/// casually. [`format_shortfall`] checks the first at runtime; the third is
/// why this is a float format and not the `Rg8Unorm` it began as.
///
/// * **Filterable under `Features::empty()`.** The whole design rests on the
///   hardware doing the two filtered means under one set of weights.
///   `Rg16Float` carries `FILTERABLE` on the GLES3/WebGL2 downlevel path —
///   `RG16F` is texture-filterable in ES 3.0 (Table 3.13), including for
///   `TEXTURE_3D`, and wgpu's GL backend reports it unconditionally
///   (`wgpu-hal` 29 `gles/adapter.rs`: `Tf::Rg16Float => filterable | ...`)
///   — where `R32Float` would need `FLOAT32_FILTERABLE`.
/// * **Affine index↔value.** Filtering *within* data is then exactly linear
///   interpolation of the physical value, which is what makes the ratio a
///   meaningful reconstruction rather than a blend of labels.
/// * **Filter error that scales with the sample, not with the format.** This
///   is the one that costs the second byte per channel, and it is not a
///   refinement — an eight-bit format makes the reconstruction *wrong*, not
///   merely coarse.
///
/// # Why `Rg8Unorm` cannot carry this reconstruction
///
/// A sampler is permitted to compute a filtered `unorm` result in the source
/// format's own fixed point, and real ones do: Mesa's lavapipe returns `R̄`
/// and `Ḡ` rounded to exact multiples of 1/255, and 8-bit filtering is the
/// norm on the GLES3 hardware this format was picked to serve. The error is
/// then **absolute** — up to one 1/255 quantum on each channel — while the
/// reconstruction divides by `Ḡ`, so it arrives at the index multiplied by
/// `1/Ḡ`:
///
/// ```text
/// |Δindex|  ≤  (q + index·q) / Ḡ  ≤  2q / Ḡ,   q = 1/255
/// ```
///
/// At full coverage that is invisible. One cell out from an echo edge, where
/// `Ḡ` is a few 255ths, it is the whole palette — and the shell around every
/// echo is exactly where this feature exists to be honest. Measured on
/// lavapipe against a 4-texel fixture, sampled directly with no march in the
/// way:
///
/// | stored index | `Ḡ` | `Rg8Unorm` reconstructs | `Rg16Float` reconstructs |
/// |-------------:|------:|------------------------:|-------------------------:|
/// |          147 |  1/255 |                     255 |                   147.05 |
/// |          147 |  3/255 |                     170 |                   147.05 |
/// |           64 |  3/255 |                      85 |                    64.00 |
/// |           64 |  5/255 |                      51 |                    64.00 |
/// |            1 | <1/2   |                       0 |                     1.00 |
///
/// The 147 and 64 rows are the KLOT NROT green arcs coming straight back: a
/// volume whose only data index is 147 reconstructs to 51-85 in the boundary
/// shell, which is inside an under-band the field never occupied. The index-1
/// row is worse in a quieter way — `R̄ = c/255` rounds to **zero** for every
/// coverage under a half, so a faint echo's premultiplied channel underflows
/// outright, the shell reconstructs to the no-data index, and the silhouette's
/// reach starts depending on the stored value again. That is the precise
/// defect premultiplication retired.
///
/// No shader arithmetic recovers this. The sampler has already destroyed the
/// information by the time `field_at` sees it; `max(Ḡ, ε)` guards a division
/// by zero and nothing else, and a coverage floor can only choose how much of
/// the shell to discard, not make the discarded part honest.
///
/// A float format fixes it because float quantisation is **relative**: `Δ R̄ /
/// R̄` and `Δ Ḡ / Ḡ` are both bounded by half an ulp whatever the magnitude,
/// so the quotient's error is bounded by an ulp and does not know how small
/// `Ḡ` is. `Rg16Float`'s 11-bit significand puts the reconstructed index
/// within ~0.25 of a palette entry at *every* coverage — measured identical to
/// four decimal places on an RTX 3090 and on lavapipe, which is the whole
/// point.
///
/// The cost is two bytes per cell rather than one — still one texture fetch
/// per march step, and the memory is budgeted in
/// `constants::VOLUME_TEXTURE_BUDGET_BYTES`, whose three arms were doubled to
/// take it.
pub const VOLUME_TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg16Float;

/// The environment variable that turns the volume view off natively.
///
/// Mirrors the `WGPU_BACKEND` convention `app::instance_descriptor` already
/// relies on: an escape hatch for a user whose driver misbehaves in a way none of
/// the three layers catch, without needing a rebuild or a config edit. There is
/// no browser equivalent, because a browser has no environment to read.
pub const VOLUME_ENV_VAR: &str = "RUSTDAR_VOLUME";

/// The smallest 3D texture worth rendering a volume into.
///
/// Not the grid size: a device reporting less than the grid can still be stepped
/// down to a coarser one, and the runtime grid ladder is where that belongs. This
/// is the floor below which there is no useful volume at all — 32 cells over a
/// 40 km half-width is 2.5 km per cell, coarser than the beam.
const MIN_TEXTURE_DIMENSION_3D: u32 = 32;

/// Sampled textures a volume pipeline binds at once: the grid, its colour LUT,
/// the jitter tile and the map floor's pane mirror.
///
/// "Sampled" is wgpu's name for the binding *class*, not a claim that all four
/// are read through a sampler — the jitter tile is read with `textureLoad` and
/// carries none, and it occupies one of these slots regardless. What this has
/// to match is the number of texture bindings the raymarch's layouts declare
/// across both groups, because that is what the adapter is being asked for.
///
/// It read 2 until 2026-08-12, and was already wrong before the jitter tile
/// existed: the map floor's mirror had made it 3, so an adapter reporting
/// exactly 2 passed this probe and then failed pipeline creation — which is
/// the failure the probe exists to turn into a clean refusal.
///
/// Held to the shader by
/// `the_probe_asks_the_adapter_for_every_binding_the_raymarch_declares`, which
/// counts it rather than trusting it.
const REQUIRED_SAMPLED_TEXTURES: u32 = 4;

/// Samplers a volume pipeline binds at once: the grid's, the LUT's, the floor's.
///
/// One per texture *that is sampled*, and it has to be one per rather than one
/// shared: naga rejects a texture sampled through two samplers in one entry
/// point (`Error::ImageMultipleSamplers`), and the grid wants `Linear` while an
/// exact-index LUT lookup wants `Nearest`. It is one fewer than
/// [`REQUIRED_SAMPLED_TEXTURES`] because the jitter tile is read with
/// `textureLoad` and carries no sampler at all.
///
/// It read 2 until 2026-08-13 — the same defect as the one above, one constant
/// along and found one day later, because the fix that corrected the textures
/// derived only the textures. Both are now counted by the one scan in
/// `the_probe_asks_the_adapter_for_every_binding_the_raymarch_declares`, and a
/// binding class that scan does not recognise fails it rather than passing
/// silently, so the third constant cannot go stale the way this one did.
const REQUIRED_SAMPLERS: u32 = 3;

/// Bytes of uniform data one volume draw needs bound at once.
///
/// The raymarch's uniform block is one `mat4x4<f32>` plus ten `vec4<f32>` — 224
/// bytes, [`uniform::VOLUME_UNIFORM_BYTES`] — and this is the next
/// std140-friendly bound above it. Well under the 16 KiB WebGL2 itself
/// guarantees; the check exists to catch a device that reports something
/// absurd, not to be tight.
///
/// The headroom is 32 bytes — two `vec4` lanes — not the 96 the shader's own
/// comment claimed until 2026-08-13. Small enough that growing the block is a
/// change to this number too, which is why
/// `the_probe_asks_the_adapter_for_every_binding_the_raymarch_declares` asserts
/// the block still fits rather than leaving the two figures to drift.
///
/// `u64` because `Limits::max_uniform_buffer_binding_size` is, unlike the three
/// counts above.
const REQUIRED_UNIFORM_BINDING_SIZE: u64 = 256;

/// Whether this device can render a 3D volume, decided before anything is made.
///
/// Order matters only for which reason a user sees first, and it goes cheapest
/// and most-likely-to-be-deliberate first: the escape hatch, then limits, then
/// format features. Nothing here allocates or compiles anything.
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
/// limits check, and the probe cannot know about it because it runs before the
/// event and on a freshly rebuilt `AppState`. Call this rather than reading
/// `AppState::volume_support` directly.
pub fn support(probed: &VolumeSupport) -> VolumeSupport {
    prefer_recorded_failure(degrade::recorded_failure(), probed)
}

/// The precedence rule, separated from the process-global state it reads.
///
/// Split for testability, and not gratuitously: the statics in [`degrade`] are
/// process-global and never reset, so a test that drove them would be at the
/// mercy of every other test in the binary — as the first version of this file's
/// suite discovered, by failing whenever the degrade module's own global test
/// happened to run first.
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
/// Takes the value rather than reading the environment so it is testable: env
/// vars are process-global and `cargo test` runs tests in parallel threads, so a
/// test that set one would race every other test in the binary.
///
/// Only an explicit `off` disables. Anything else — including an empty string, a
/// typo, or `on` — leaves the probe to decide, because silently disabling 3D on a
/// misspelling is worse than ignoring one.
///
/// Native-only, like its caller: on wasm32 there is nothing to read and an
/// ungated copy is dead code, which the wasm clippy row fails on.
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
/// ones — including `Limits::downlevel_webgl2_defaults()`, which is the floor the
/// web build is actually held to.
///
/// `pub` because the floor is pinned from outside this module by two external
/// surfaces: `the_web_limits_this_app_requests_clear_the_volume_probes_floor`
/// (below) composes it with `rustdar_gpu::device::device_limits` — so the
/// limits the app side *actually requests* are held to this floor rather than
/// a hand-built approximation of them — and the app side's `AppState::new`
/// scrape pin holds `probe` (this function's one production caller) in that
/// request path.
pub fn limits_shortfall(limits: &wgpu::Limits) -> Option<String> {
    let grid_axis = VOLUME_GRID_CELLS.iter().copied().max().unwrap_or(0);
    // The grid must fit as well as the floor, so that a device between the two is
    // reported honestly rather than failing later inside a callback. The web arm
    // of `device_limits` lifts this limit via `using_resolution`, so on a capable
    // browser it is the adapter's real figure rather than the 256 floor.
    let needed_3d = grid_axis.max(MIN_TEXTURE_DIMENSION_3D);

    // Widened to `u64` because `max_uniform_buffer_binding_size` is one and the
    // other three are `u32`; comparing each in its own width would need four
    // near-identical branches instead of one table.
    //
    // `what` names the resource and never counts it. Two of these read "four
    // sampled textures" and "two samplers" until 2026-08-13, which made the
    // spelled-out numeral a third place to update when a binding was added —
    // and the sentence already prints `needed`, so the numeral was only ever a
    // chance to disagree with it.
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
/// `TEXTURE_BINDING` is what makes the grid samplable at all. `FILTERABLE` is
/// the *stated reason* `Rg16Float` was chosen over `R32Float`, and without it a
/// `Linear` sampler is a validation error rather than a fallback to `Nearest` —
/// so a device that cannot filter it is not a device that renders a blockier
/// volume, it is a device that renders nothing. It is also the premise the
/// coverage-premultiplied reconstruction rests on outright: `R̄ / Ḡ` is
/// meaningless without the hardware taking both means under one set of
/// weights.
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
/// # The trade this makes, stated because it is a real one
///
/// `Device::on_uncaptured_error` installs **one** handler for the whole device,
/// replacing wgpu's default — and that default is
/// `default_error_handler`, which panics (wgpu-29.0.4
/// `src/backend/wgpu_core.rs:685-688`). So installing anything here takes over
/// error reporting for *every* wgpu call in the application, not only the
/// volume's.
///
/// Swallowing an unrelated validation error would therefore be a genuine
/// regression: a bug anywhere else in the renderer that used to abort loudly with
/// a description would become a log line nobody reads. That is why anything
/// without a [`degrade::VOLUME_LABEL_PREFIX`] label **re-panics under
/// `debug_assertions`**, restoring the default's behaviour for the builds where a
/// developer is watching. Release builds log instead, because aborting a user's
/// radar viewer over a validation error it might have survived is the worse of
/// the two failures — and on the web it is a dead tab.
///
/// The consequence to keep in mind: every wgpu resource the volume view creates
/// **must** carry a `rustdar.volume`-prefixed label, or its errors are treated as
/// unrelated and panic the debug build.
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
    /// The volume view's own. Latch it and carry on — the whole point of the
    /// handler is that this one must not abort the application.
    LatchVolumeFailure,
    /// Not the volume's, and this build re-raises it, restoring exactly what
    /// wgpu's default handler would have done.
    Repanic,
    /// Not the volume's, and this build logs it rather than aborting a user's
    /// radar viewer over an error it might have survived.
    Log,
}

/// The handler's decision, separated from the handler.
///
/// `debug_build` is a parameter rather than `cfg!(debug_assertions)` read
/// inline, so that both arms are reachable from one test binary. Testing only
/// the arm the test runner happened to be compiled for would leave the release
/// behaviour — the one that runs on users' machines — entirely unexercised.
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
    ///
    /// One tally rather than one count per constant, because the drift being
    /// prevented is not "the texture number is wrong". It is "a binding was
    /// added and *some* number went stale" — which happened twice, to two
    /// different constants, and the second time to the constant standing next
    /// to the one that had just been fixed by a count that covered only itself.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
    struct DeclaredBindings {
        /// Counts against `max_sampled_textures_per_shader_stage`.
        sampled_textures: u32,
        /// Counts against `max_samplers_per_shader_stage`.
        samplers: u32,
        /// Uniform blocks. A *count*, where the probe's limit is a size — so
        /// what this pins is the premise that there is exactly one block for
        /// [`REQUIRED_UNIFORM_BINDING_SIZE`] to be about.
        uniform_buffers: u32,
    }

    /// Tally every `@group(...) @binding(...)` the raymarch declares, by class.
    ///
    /// **Exhaustive on purpose.** A binding whose class none of the arms below
    /// recognise is an `Err`, not a skip, because the whole failure being
    /// guarded against is a binding the probe does not know it needs: a storage
    /// buffer or a storage texture would be checked against limits
    /// [`limits_shortfall`] never reads, and silently ignoring it would rebuild
    /// the exact hole this closes. Each error names the line and the limit that
    /// would have to be added to the probe before the shader may depend on it.
    ///
    /// The blit's own pair is skipped: it belongs to a third layout and a
    /// different pipeline, so it is never bound alongside these and does not
    /// add to what one raymarch draw asks of the adapter.
    ///
    /// Text rather than a naga parse because this must fail when the *source*
    /// gains a line, including one no pipeline has been built from yet.
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
            // space; a handle binding — texture or sampler — carries it in the
            // type after the colon.
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
    ///
    /// Both of these constants have drifted, a day apart, in the same way:
    /// [`REQUIRED_SAMPLED_TEXTURES`] sat at 2 while the map floor's mirror had
    /// made it 3, and [`REQUIRED_SAMPLERS`] sat at 2 while the floor's sampler
    /// had made it 3. The failure either one causes is precisely the one the
    /// probe exists to prevent: an adapter reporting a figure between the two
    /// passes, and then `create_render_pipeline` fails asynchronously into the
    /// uncaptured-error sink, which panics under `debug_assertions`.
    ///
    /// So this asserts the whole tally at once. A binding added to
    /// `volume.wgsl` fails this test whichever class it belongs to, and a class
    /// with no arm fails it too rather than passing unnoticed — which is what
    /// the texture-only count that preceded it did to the sampler.
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
    ///
    /// Without this, the test above passes either because the scan counts or
    /// because it counts nothing and the constants happen to match zero. What
    /// it proves is the property the sampler constant lacked for a day: adding
    /// `var floor_sampler: sampler;` to the shader and nothing else makes the
    /// assertion above fail.
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
    ///
    /// This is the half that makes one mechanism cover the next constant as
    /// well as these two. A storage buffer or storage texture is checked by
    /// wgpu against limits [`limits_shortfall`] never reads, so counting only
    /// the classes already known would let one in with the probe silent — the
    /// same shape of hole, one class over.
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
    ///
    /// They share `@group(0)` in the source but belong to a third layout and a
    /// different pipeline, so they are never bound alongside the raymarch's and
    /// counting them would raise the probe's floor above what any volume draw
    /// actually needs — refusing devices that can render one perfectly well.
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
    ///
    /// This is the load-bearing claim of the whole probe: if the guaranteed
    /// WebGL2 limits did *not* satisfy it, the volume view would be unavailable
    /// on a conforming browser by construction and the thresholds would be
    /// wrong rather than the device.
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
    ///
    /// `device_limits`' web arm calls `using_resolution`, which raises
    /// `max_texture_dimension_3d` to whatever the adapter reports — so in
    /// practice this is usually higher. The point of asserting the *unlifted*
    /// value is that the grid needs no runtime step-down on a device that
    /// reports exactly the guarantee, which is what the grid was sized for.
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
    ///
    /// Without this the probe could check four limits and *depend* on one, and
    /// three of the four would be decoration that a refactor could delete with
    /// every test still green.
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
    ///
    /// The threshold the probe applies is `max(grid axis, 32)`, so a device
    /// between the two is caught here rather than inside a callback where there is
    /// no `Result`. Sized off the constant so this keeps meaning the same thing
    /// when the grid changes.
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
    ///
    /// `FILTERABLE` in particular: it is the stated reason `Rg16Float` was chosen,
    /// and a device without it cannot use a `Linear` sampler at all — so treating
    /// it as optional would produce a validation error rather than a blockier
    /// volume.
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
    ///
    /// Including a typo. Silently disabling 3D because someone wrote `of` would
    /// be indistinguishable, to the user, from the feature being broken.
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
    ///
    /// This is the whole point of `support` existing rather than callers reading
    /// `AppState::volume_support`: a probe that runs at construction cannot know
    /// about a context that died afterwards, and on a rebuilt `AppState` it will
    /// cheerfully say the device is fine again.
    ///
    /// Driven through the pure rule rather than the statics. Calling `support`
    /// here is what the first version did, and it failed whenever the degrade
    /// module's own global-counter test ran first in the same process — the
    /// counters are deliberately never reset, so no test may depend on their
    /// value.
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
    ///
    /// This is the layer's whole reason for existing: the calls that produce
    /// these errors return no `Result`, and wgpu's default response is to panic
    /// — which on the web is a dead browser tab.
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
    ///
    /// Installing *any* uncaptured-error handler replaces
    /// `default_error_handler`, which panics (wgpu-29.0.4
    /// `src/backend/wgpu_core.rs:685-688`), for the whole device — not just for
    /// the volume. Without this arm, adding the volume view would silently
    /// downgrade every validation error anywhere in the renderer from a loud
    /// abort with a description to a log line nobody reads. That is a real
    /// regression and it is the reason the handler is allowed to exist at all.
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
    ///
    /// The other half of the same trade, and the arm a `cfg!` read inline would
    /// leave untested on every CI row that builds debug.
    #[test]
    fn an_unrelated_error_is_logged_rather_than_fatal_in_release() {
        assert_eq!(
            disposition("In Queue::write_buffer", false),
            ErrorDisposition::Log
        );
    }

    // The two pins on the app side's *use* of this crate —
    // `app_state_probes_the_device_and_installs_the_latch` and
    // `a_surface_loss_is_only_counted_when_a_volume_was_on_screen` — moved to
    // rustdar-frontend's `app_render::egui_frame_pin_tests` at WO-RV: they
    // scrape frontend files, and the file a test pins is the crate it lives in.

    /// The limits the app *requests* clear the floor the volume probe applies.
    ///
    /// [`limits_shortfall`]'s doc says it is testable against
    /// `downlevel_webgl2_defaults()`, and this crate root does test it against
    /// that — but nothing tied the figure the probe was exercised with to the
    /// figure the device request actually produces. They are the same only
    /// because `device_limits` happens to start from the same call, which is
    /// precisely the kind of "obviously the same" that this campaign has
    /// already paid for once. The app side's `AppState::new` requests these
    /// limits (through `rustdar_gpu::device::request_device`), the device
    /// grants exactly them, and [`probe`] reads them back off the device — so
    /// this is the real path, not a restatement. Moved here from the app
    /// side's `app_state` tests at WO-RV: this crate sees both functions;
    /// the frontend now sees neither privately.
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
    ///
    /// The probe's two halves are unit-tested against synthetic limits above;
    /// what only a device can show is that `get_texture_format_features` and
    /// `on_uncaptured_error` behave as assumed on real hardware — in particular
    /// that `Rg16Float` really is bindable and filterable under
    /// `Features::empty()`, which is the premise the whole format choice — and
    /// with it the coverage-premultiplied reconstruction — rests on.
    ///
    /// Needs a real adapter, so it is ignored by default — but CI opts in, and
    /// the `gpu` job in `test.yaml` names this test explicitly. Renaming it
    /// means editing that job; the step asserts its own test count, so a stale
    /// name fails the row rather than silently running nothing.
    ///
    /// Passes on Mesa's lavapipe, which is what lets that row exist on a runner
    /// with no graphics hardware. Locally:
    ///
    /// ```text
    /// cargo test -p rustdar-volumetric --lib \
    ///     tests::a_real_adapter_supports_the_volume_format \
    ///     -- --ignored --exact --nocapture
    /// ```
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

        // Installing the latch must not itself trip anything. Nothing after this
        // point may provoke an unrelated wgpu error: the handler re-panics on
        // those under `debug_assertions`, which is the whole point of it.
        install_error_latch(&device);
    }
}
