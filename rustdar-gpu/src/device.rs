//! The device-request policy: the three silent forks (surface format, limit
//! set, present mode) and the one `request_device` every target shares.
//!
//! Moved here from rustdar-app's `app_state.rs` at WO-RG. What stayed
//! behind: `AppState` itself (it spans surface, renderer and volume support),
//! `request_adapter` (winit-coupled — it needs the surface), and the volume
//! probe/latch (volumetric items this crate must not name; volumetric→gpu is
//! the dependency direction).

use egui_wgpu::wgpu;

/// Whether this build is the browser build.
///
/// The three device decisions below fork on this value rather than on `#[cfg]`
/// attributes, so both arms of each are compiled and callable from a single host
/// test binary — the same shape the app side's `volume::disposition(rendered,
/// debug_build)` already uses. `cfg!` expands to a literal `true` or `false`, so
/// nothing is decided at runtime and the arm this target does not take still
/// optimises away.
///
/// It matters here more than most places. Every fork below is *silent* when it
/// goes the wrong way: an sRGB swapchain in a browser washes the colours out
/// with no validation error, WebGL2 limits requested natively cost texture size
/// with no message, and `AutoVsync` in a browser negotiates something nobody
/// asked for. None of the three produce a `Result` to check, and until these
/// tests existed (they arrived with `app_state.rs`'s test module and travelled
/// here at WO-RG), all six arms were unexercised and three of them were
/// unreachable from a host build.
///
/// This line is now the only thing in the file a host build cannot check, which
/// is what `the_web_fork_is_the_wasm32_arch_and_nothing_else` scrapes.
const WEB: bool = cfg!(target_arch = "wasm32");

/// Selects the best surface format from available capabilities
pub fn select_surface_format(capabilities: &wgpu::SurfaceCapabilities) -> wgpu::TextureFormat {
    preferred_surface_format(&capabilities.formats, WEB)
}

/// The format choice itself, over the format list rather than over a
/// `SurfaceCapabilities` only a live adapter can produce.
///
/// WebGL2 presents the canvas through a plain, non-sRGB default framebuffer.
/// Configuring an sRGB swapchain on top of that makes the browser apply the
/// transfer function a second time over the one egui has already baked into its
/// vertex colours; the failure is washed-out output, not a validation error, so
/// nothing reports it. Native has a real sRGB-capable swapchain and keeps the
/// `Bgra8Unorm` preference untouched.
fn preferred_surface_format(formats: &[wgpu::TextureFormat], web: bool) -> wgpu::TextureFormat {
    let Some(&first) = formats.first() else {
        // Fallback to a common format
        return wgpu::TextureFormat::Rgba8UnormSrgb;
    };
    if web && let Some(&format) = formats.iter().find(|f| !f.is_srgb()) {
        return format;
    }
    formats
        .iter()
        .copied()
        .find(|&format| format == wgpu::TextureFormat::Bgra8Unorm)
        .unwrap_or(first)
}

/// The limit set to request from the adapter.
///
/// Native asks for the adapter's real limits so desktop GPUs can use textures
/// far larger than any portable floor. WebGL2 cannot express most of wgpu's
/// limit set at all, so requesting the adapter's limits verbatim there fails the
/// device request outright. The web arm starts from the WebGL2 downlevel
/// defaults and lifts *only* the resolution back to what the adapter actually
/// reports — `max_texture_dimension_2d` is the one limit the overlay planner
/// reads, and pinning it to the 2048 spec floor would cost resolution on every
/// browser that offers more.
///
/// Takes the adapter's `Limits` rather than the `Adapter`, because what comes
/// out of here is the floor the whole 3D volume view is held to:
/// `AppState::new` requests these limits (through [`request_device`]), the
/// device grants exactly them, and the volume `probe` then reads them back off
/// the device. `limits_shortfall` documents being testable against
/// `downlevel_webgl2_defaults()` and nothing connected the two;
/// `the_web_limits_this_app_requests_clear_the_volume_probes_floor` does.
///
/// `pub` because that cross-crate pin composes this fn with the probe's floor
/// from outside — it lives beside the probe, which this crate must not name.
pub fn device_limits(adapter: wgpu::Limits, web: bool) -> wgpu::Limits {
    if web {
        wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter)
    } else {
        adapter
    }
}

/// How the surface presents.
///
/// `Fifo` is the only present mode WebGL2 actually has — the browser paces
/// presentation through `requestAnimationFrame` and wgpu's other modes have
/// nothing to map onto. Naming it explicitly keeps the web build off
/// `AutoVsync`'s negotiation, which has no meaningful choice to make here.
const fn present_mode(web: bool) -> wgpu::PresentMode {
    if web {
        wgpu::PresentMode::Fifo
    } else {
        wgpu::PresentMode::AutoVsync
    }
}

/// What this build asks the surface for. See `present_mode` above (private —
/// a doc link from a pub const to it would be rustdoc's one warning here).
pub const PRESENT_MODE: wgpu::PresentMode = present_mode(WEB);

/// Request the device this build wants from an already-chosen adapter.
///
/// `extra_features` is whatever features the caller asks for **beyond**
/// `Features::empty()` — the app side computes
/// `adapter.features() & STAGING_RING_FEATURE` and hands the mask in, so the
/// staging-ring coupling stays out of the device fn and this is the same
/// `request_device` on every target rather than a `cfg`ed pair. That one
/// feature is what lets a voxel grid's 32 MiB plane be staged through host
/// memory and pulled across PCIe by DMA instead of being pushed across it by
/// the frame thread, and on the desktop shape that is the difference between
/// 17.6 ms and 2.0 ms inside `prepare`.
///
/// Nothing else changes shape when it is on. wgpu warns that the feature is "a
/// massive performance footgun on a discrete GPU", and it is — for an
/// application that then maps its vertex, index or uniform buffers. Buffer
/// placement is decided per buffer from that buffer's own
/// `MAP_READ`/`MAP_WRITE` bits, and the only buffers in this workspace's render
/// stack that name either are the staging ring's.
///
/// WebGL2 offers none of this and takes `Features::empty()`, which is also the
/// arm with nothing to gain: there is no BAR window in a browser for the old
/// path to have been slow across.
pub async fn request_device(
    adapter: &wgpu::Adapter,
    extra_features: wgpu::Features,
) -> (wgpu::Device, wgpu::Queue) {
    // Native takes the adapter's actual limits so it is not held to a
    // portable floor; the web arm reconciles them with what WebGL2 can
    // express. See `device_limits`.
    let limits = device_limits(adapter.limits(), WEB);
    adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: None,
            required_features: extra_features,
            required_limits: limits,
            memory_hints: Default::default(),
            experimental_features: Default::default(),
            trace: Default::default(),
        })
        .await
        .expect("Failed to create device")
}

/// Classify what the adapter says it is.
///
/// Exhaustive on purpose: a new `DeviceType` variant should be a compile
/// error here, not a silent fall into `Unknown`.
///
/// A free fn rather than an inherent method on `DeviceClass`: the class lives
/// down in rustdar-device-profile (WO-RD), an inherent impl can only live in
/// the defining crate, and the floor's charter forbids it a wgpu dependency —
/// so the one wgpu-touching line lives here, at the wgpu boundary, beside the
/// request policy (re-homed from app.rs at WO-RG).
pub fn device_class_of(
    device_type: wgpu::DeviceType,
) -> rustdar_device_profile::quality::DeviceClass {
    use rustdar_device_profile::quality::DeviceClass;
    match device_type {
        wgpu::DeviceType::DiscreteGpu => DeviceClass::Discrete,
        wgpu::DeviceType::IntegratedGpu => DeviceClass::Integrated,
        wgpu::DeviceType::VirtualGpu => DeviceClass::Virtual,
        wgpu::DeviceType::Cpu => DeviceClass::Software,
        wgpu::DeviceType::Other => DeviceClass::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use egui_wgpu::wgpu::TextureFormat::{Bgra8Unorm, Bgra8UnormSrgb, Rgba8Unorm, Rgba8UnormSrgb};

    /// What a browser surface typically offers, in the order wgpu reports it.
    /// Both an sRGB and a non-sRGB view of the same underlying format, which is
    /// exactly the choice the web arm exists to make.
    const BROWSER: [wgpu::TextureFormat; 2] = [Bgra8UnormSrgb, Bgra8Unorm];

    /// What a desktop Vulkan surface typically offers.
    const DESKTOP: [wgpu::TextureFormat; 4] =
        [Bgra8UnormSrgb, Bgra8Unorm, Rgba8UnormSrgb, Rgba8Unorm];

    /// The web arm never configures an sRGB swapchain when a non-sRGB view of
    /// the surface exists.
    ///
    /// This is the whole point of the fork and it fails *silently*: WebGL2's
    /// default framebuffer is already non-sRGB, so an sRGB swapchain applies the
    /// transfer function a second time over the one egui baked into its vertex
    /// colours. The output is washed out and no validation error is raised, so
    /// nothing but an eye catches it — and nothing did, because this arm is
    /// compiled only by a target this workspace does not test.
    #[test]
    fn the_web_arm_never_picks_an_srgb_format_when_a_plain_one_exists() {
        for formats in [BROWSER.as_slice(), DESKTOP.as_slice(), &[Rgba8Unorm]] {
            let chosen = preferred_surface_format(formats, true);
            assert!(
                !chosen.is_srgb(),
                "the web arm chose {chosen:?} out of {formats:?}"
            );
            assert!(formats.contains(&chosen));
        }
        // And it takes the *first* such format, so the surface's own ordering
        // is respected rather than a preference of ours being imposed.
        assert_eq!(
            preferred_surface_format(&[Rgba8Unorm, Bgra8Unorm], true),
            Rgba8Unorm
        );
        assert_eq!(
            preferred_surface_format(&[Bgra8Unorm, Rgba8Unorm], true),
            Bgra8Unorm
        );
    }

    /// The native arm keeps its `Bgra8Unorm` preference, and the two arms really
    /// do diverge.
    ///
    /// A lift that quietly collapsed both arms onto one behaviour would pass
    /// every assertion above, so the divergence itself is asserted: on a format
    /// list where the two disagree, they must disagree.
    #[test]
    fn the_native_arm_prefers_bgra8unorm_and_the_two_arms_diverge() {
        assert_eq!(preferred_surface_format(&DESKTOP, false), Bgra8Unorm);
        assert_eq!(preferred_surface_format(&BROWSER, false), Bgra8Unorm);

        // A list where the web rule and the native rule cannot both be
        // satisfied: the first non-sRGB entry is not `Bgra8Unorm`.
        let split = [Rgba8UnormSrgb, Rgba8Unorm, Bgra8Unorm];
        assert_eq!(preferred_surface_format(&split, true), Rgba8Unorm);
        assert_eq!(preferred_surface_format(&split, false), Bgra8Unorm);
        assert_ne!(
            preferred_surface_format(&split, true),
            preferred_surface_format(&split, false),
            "the two arms have collapsed onto one behaviour"
        );
    }

    /// With no `Bgra8Unorm` on offer the native arm takes the surface's first
    /// choice rather than inventing one, and an empty list falls back.
    ///
    /// The empty case is not hypothetical padding: the old code indexed
    /// `formats[0]` and needed the emptiness check ahead of it to avoid a panic
    /// during startup, on a path with no `Result`.
    #[test]
    fn a_surface_offering_nothing_useful_still_yields_a_format() {
        assert_eq!(
            preferred_surface_format(&[Rgba8UnormSrgb], false),
            Rgba8UnormSrgb
        );
        // All-sRGB, so the web arm's search finds nothing and it falls through
        // to the same rule native uses.
        assert_eq!(
            preferred_surface_format(&[Rgba8UnormSrgb], true),
            Rgba8UnormSrgb
        );
        for web in [true, false] {
            assert_eq!(preferred_surface_format(&[], web), Rgba8UnormSrgb, "{web}");
        }
    }

    /// The native arm asks for exactly what the adapter reports, unchanged.
    #[test]
    fn the_native_arm_requests_the_adapters_own_limits() {
        let adapter = wgpu::Limits::default();
        assert_eq!(device_limits(adapter.clone(), false), adapter);
    }

    /// The web arm asks for the WebGL2 downlevel set with the resolution lifted,
    /// and nothing else lifted.
    ///
    /// Requesting the adapter's limits verbatim on WebGL2 fails the device
    /// request outright, so "did it actually clamp" is the load-bearing half;
    /// `using_resolution` being the *only* lift is the other, since anything
    /// else raised would be a limit WebGL2 cannot express.
    #[test]
    fn the_web_arm_clamps_to_webgl2_and_lifts_only_the_resolution() {
        let floor = wgpu::Limits::downlevel_webgl2_defaults();
        // A generous adapter — `Limits::default()` is the full WebGPU set and is
        // far above the WebGL2 floor in every dimension.
        let adapter = wgpu::Limits::default();
        let asked = device_limits(adapter.clone(), true);

        assert_ne!(
            asked, adapter,
            "the web arm passed the adapter's limits through"
        );
        assert_eq!(asked, floor.clone().using_resolution(adapter.clone()));

        // Resolution lifted...
        assert_eq!(
            asked.max_texture_dimension_1d,
            adapter.max_texture_dimension_1d
        );
        assert_eq!(
            asked.max_texture_dimension_2d,
            adapter.max_texture_dimension_2d
        );
        assert_eq!(
            asked.max_texture_dimension_3d,
            adapter.max_texture_dimension_3d
        );
        assert!(asked.max_texture_dimension_2d > floor.max_texture_dimension_2d);

        // ...and nothing else. A sample of limits WebGL2 genuinely cannot
        // express: each stays at the downlevel figure.
        assert_eq!(
            asked.max_storage_buffers_per_shader_stage,
            floor.max_storage_buffers_per_shader_stage
        );
        assert_eq!(
            asked.max_compute_workgroup_size_x,
            floor.max_compute_workgroup_size_x
        );
        assert_eq!(asked.max_bind_groups, floor.max_bind_groups);
    }

    /// Both present modes, from one host binary.
    #[test]
    fn the_web_surface_asks_for_fifo_and_native_for_autovsync() {
        assert_eq!(present_mode(true), wgpu::PresentMode::Fifo);
        assert_eq!(present_mode(false), wgpu::PresentMode::AutoVsync);
        assert_eq!(PRESENT_MODE, present_mode(WEB));
    }

    /// Every `DeviceType` classifies, and no two collapse that must not.
    ///
    /// `Cpu` mapping to anything but `Software` is the one that matters: a
    /// software rasteriser given the discrete GPU's quality is a frame time in
    /// seconds, and a browser falling back to SwiftShader is a real path.
    /// (Travelled from the floor crate's quality tests at WO-RD to app.rs,
    /// and from there to here with the classifier at WO-RG.)
    #[test]
    fn every_adapter_device_type_maps_to_its_own_class() {
        use rustdar_device_profile::quality::DeviceClass;
        for (device_type, expected) in [
            (wgpu::DeviceType::DiscreteGpu, DeviceClass::Discrete),
            (wgpu::DeviceType::IntegratedGpu, DeviceClass::Integrated),
            (wgpu::DeviceType::VirtualGpu, DeviceClass::Virtual),
            (wgpu::DeviceType::Cpu, DeviceClass::Software),
            (wgpu::DeviceType::Other, DeviceClass::Unknown),
        ] {
            assert_eq!(
                device_class_of(device_type),
                expected,
                "{device_type:?} no longer classifies as {expected:?}"
            );
        }
    }

    /// Everything above exercises both arms; this pins which one this build
    /// takes, and it is the only claim in the file a host `cargo test` cannot
    /// make by running code.
    ///
    /// Scraped rather than asserted because a `cfg!` that has been pointed at
    /// another arch, or replaced by `false`, or by `cfg!(target_family =
    /// "wasm")` — which is also true for WASI, a target this build does not mean
    /// — evaluates to the same thing on this host and to something different in
    /// a browser. The three call sites are checked too: a lifted function that
    /// nothing passes `WEB` to is a fork that has quietly stopped forking.
    #[test]
    fn the_web_fork_is_the_wasm32_arch_and_nothing_else() {
        let source = include_str!("device.rs");
        // Only the shipped half of the file. The assertions below quote the very
        // strings they look for, so scanning the whole file would find them in
        // this test's own source and pass no matter what the code did.
        let (code, _) = source
            .split_once("#[cfg(test)]")
            .expect("device.rs no longer has a test module");

        // Every needle is counted before it is read. One occurrence is the
        // claim; a second would mean whichever came first is what got checked,
        // and a decoy in a doc comment or a string literal would be a second.
        let unique = |needle: &str| {
            let n = code.matches(needle).count();
            assert_eq!(
                n, 1,
                "expected exactly one `{needle}` in device.rs, found {n}"
            );
        };

        unique("const WEB: bool =");
        let definition = code
            .split_once("const WEB: bool =")
            .and_then(|(_, rest)| rest.split_once(';'))
            .map(|(value, _)| value.trim())
            .expect("`WEB` is no longer defined here");
        assert_eq!(
            definition, r#"cfg!(target_arch = "wasm32")"#,
            "`WEB` is defined as `{definition}`. Every fork in this file reads \
             it, and all of them are silent when they go the wrong way."
        );

        for call in [
            "preferred_surface_format(&capabilities.formats, WEB)",
            "device_limits(adapter.limits(), WEB)",
            "present_mode(WEB)",
        ] {
            unique(call);
        }
    }
}
