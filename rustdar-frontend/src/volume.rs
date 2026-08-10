//! Deciding, before anything is created, whether a 3D volume can be rendered.
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

use crate::constants::VOLUME_GRID_CELLS;

#[path = "volume_bridge.rs"]
pub mod bridge;
#[path = "volume_degrade.rs"]
pub mod degrade;
#[path = "volume_floor.rs"]
pub mod floor;
#[path = "volume_quality.rs"]
pub mod quality;
#[path = "volume_raymarch.rs"]
pub mod raymarch;
#[path = "volume_uniform.rs"]
pub mod uniform;

pub use degrade::VolumeSupport;

/// The texel format a voxel grid is uploaded as: **coverage-premultiplied**
/// palette indices.
///
/// `R = coverage × index`, `G = coverage`, one byte each, where coverage is 1
/// for a measured cell and 0 for empty air. The march samples both channels
/// `Linear` and reconstructs `index = R̄ / Ḡ`, which is the coverage-weighted
/// mean over the covered texels alone — air contributes 0 to numerator and
/// denominator alike, so it drops out of the average instead of taking part in
/// it as a value. See `volume.wgsl`'s `field_at`, and
/// `rustdar_radar::voxel`'s module doc for what that retired.
///
/// Two properties are load-bearing and neither survives changing this format
/// casually, which is why [`format_shortfall`] checks for both:
///
/// * **Filterable under `Features::empty()`.** The whole design rests on the
///   hardware doing the two filtered means under one set of weights.
///   `Rg8Unorm` carries `FILTERABLE` on the GLES3/WebGL2 downlevel path —
///   `RG8` is in ES 3.0's required *texture-filterable* colour formats
///   (Table 3.13), including for `TEXTURE_3D` — where `R32Float` would need
///   `FLOAT32_FILTERABLE`.
/// * **Affine index↔value.** Filtering *within* data is then exactly linear
///   interpolation of the physical value, which is what makes the ratio a
///   meaningful reconstruction rather than a blend of labels.
///
/// The cost of the second channel is one byte per cell — still one texture
/// fetch per march step, and the memory is budgeted in
/// `constants::VOLUME_TEXTURE_BUDGET_BYTES`.
pub const VOLUME_TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg8Unorm;

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

/// Sampled textures a volume pipeline binds at once: the grid and its colour LUT.
const REQUIRED_SAMPLED_TEXTURES: u32 = 2;

/// Samplers a volume pipeline binds at once.
///
/// One per texture, and it has to be one *per* texture rather than one shared:
/// naga rejects a texture sampled through two samplers in one entry point
/// (`Error::ImageMultipleSamplers`), and the grid wants `Linear` while an
/// exact-index LUT lookup wants `Nearest`.
const REQUIRED_SAMPLERS: u32 = 2;

/// Bytes of uniform data one volume draw needs bound at once.
///
/// The raymarch's uniform block is one `mat4x4<f32>` plus six `vec4<f32>` — 160
/// bytes — and this is the next std140-friendly bound above it. Well under the
/// 16 KiB WebGL2 itself guarantees; the check exists to catch a device that
/// reports something absurd, not to be tight.
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
/// `pub(crate)` so `app_state` can hold the limits it *actually requests* to
/// this floor rather than a hand-built approximation of them. That claim was
/// prose in the sentence above until
/// `the_web_limits_this_app_requests_clear_the_volume_probes_floor` connected
/// the two functions.
pub(crate) fn limits_shortfall(limits: &wgpu::Limits) -> Option<String> {
    let grid_axis = VOLUME_GRID_CELLS.iter().copied().max().unwrap_or(0);
    // The grid must fit as well as the floor, so that a device between the two is
    // reported honestly rather than failing later inside a callback. The web arm
    // of `device_limits` lifts this limit via `using_resolution`, so on a capable
    // browser it is the adapter's real figure rather than the 256 floor.
    let needed_3d = grid_axis.max(MIN_TEXTURE_DIMENSION_3D);

    // Widened to `u64` because `max_uniform_buffer_binding_size` is one and the
    // other three are `u32`; comparing each in its own width would need four
    // near-identical branches instead of one table.
    for (actual, needed, what) in [
        (
            u64::from(limits.max_texture_dimension_3d),
            u64::from(needed_3d),
            "3D textures large enough to hold a volume",
        ),
        (
            u64::from(limits.max_sampled_textures_per_shader_stage),
            u64::from(REQUIRED_SAMPLED_TEXTURES),
            "two sampled textures in one shader stage",
        ),
        (
            u64::from(limits.max_samplers_per_shader_stage),
            u64::from(REQUIRED_SAMPLERS),
            "two samplers in one shader stage",
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
/// the *stated reason* `Rg8Unorm` was chosen over `R32Float`, and without it a
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
    use crate::constants::WEBGL2_MAX_TEXTURE_DIMENSION_3D;

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
    /// `FILTERABLE` in particular: it is the stated reason `Rg8Unorm` was chosen,
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

    /// `AppState::new` must actually install the latch and run the probe.
    ///
    /// Neither is enforced by the type system: `volume_support` could be filled
    /// in with a literal `Supported` and `install_error_latch` deleted outright,
    /// and everything would still compile and pass. What would be lost is the
    /// entire second layer of defence — errors back to panicking, on a device
    /// nobody checked. `AppState::new` needs a window and a surface, so reading
    /// the source is the only handle there is.
    #[test]
    fn app_state_probes_the_device_and_installs_the_latch() {
        let body = include_str!("app_state.rs")
            .split_once("pub async fn new(")
            .and_then(|(_, rest)| rest.split_once("\n    }"))
            .map(|(body, _)| body)
            .expect("AppState::new is no longer a method there");

        for call in ["volume::probe(", "volume::install_error_latch("] {
            assert!(
                body.contains(call),
                "AppState::new no longer calls `{call}`, so the volume view's \
                 pre-check or its error latch is gone"
            );
        }
    }

    /// A lost surface only counts against the volume when one was on screen.
    ///
    /// The gate is the property, not the call: counting every surface loss would
    /// retire 3D after two unplugged monitors on a machine whose GPU never
    /// complained. `present_frame` needs a real swapchain, so this reads source.
    #[test]
    fn a_surface_loss_is_only_counted_when_a_volume_was_on_screen() {
        let body = include_str!("app_render.rs")
            .split_once("pub(super) fn present_frame(")
            .and_then(|(_, rest)| rest.split_once("\n    }"))
            .map(|(body, _)| body)
            .expect("present_frame is no longer a method there");

        let call = body
            .find("note_surface_loss_with_volume(")
            .expect("present_frame no longer counts surface losses against the volume view");
        let preamble = &body[..call];
        assert!(
            preamble.contains("PaneKind::Volume"),
            "present_frame counts a surface loss against the volume view without \
             first checking that a volume pane was on screen"
        );
    }

    /// The probe agrees with a real adapter, and installing the latch is safe.
    ///
    /// The probe's two halves are unit-tested against synthetic limits above;
    /// what only a device can show is that `get_texture_format_features` and
    /// `on_uncaptured_error` behave as assumed on real hardware — in particular
    /// that `Rg8Unorm` really is bindable and filterable under
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
    /// cargo test -p rustdar-frontend --lib \
    ///     volume::tests::a_real_adapter_supports_the_volume_format \
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
             which is the premise the Rg8Unorm choice rests on. Features: \
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
