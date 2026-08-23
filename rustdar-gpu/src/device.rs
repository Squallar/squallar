//! The device-request policy: the three silent forks (surface format, limit
//! set, present mode) and the one `request_device` every target shares.

use egui_wgpu::wgpu;

/// Whether this build is the browser build.
///
/// The device decisions below fork on this value rather than on `#[cfg]`, so
/// both arms of each are compiled and callable from one host test binary.
/// Every one of those forks is silent when it goes the wrong way — none
/// produces a `Result` to check.
const WEB: bool = cfg!(target_arch = "wasm32");

pub fn select_surface_format(capabilities: &wgpu::SurfaceCapabilities) -> wgpu::TextureFormat {
    preferred_surface_format(&capabilities.formats, WEB)
}

/// WebGL2 presents the canvas through a plain, non-sRGB default framebuffer, so
/// an sRGB swapchain applies the transfer function a second time over the one
/// egui baked into its vertex colours — washed-out output, no validation error.
/// Native keeps the `Bgra8Unorm` preference.
fn preferred_surface_format(formats: &[wgpu::TextureFormat], web: bool) -> wgpu::TextureFormat {
    let Some(&first) = formats.first() else {
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
/// Native asks for the adapter's real limits. WebGL2 cannot express most of
/// wgpu's limit set, and requesting the adapter's limits verbatim there fails
/// the device request outright, so the web arm starts from the WebGL2 downlevel
/// defaults and lifts *only* the resolution back to what the adapter reports.
///
/// **The resolution is requested, not accepted**, and on WebGPU that is the
/// difference between two ceilings rather than a formality: a device gets
/// `Limits::default()`'s `max_texture_dimension_2d` of 8192 unless it asks for
/// more, and Firefox's WebGL2 reports 32768 on a real driver here. Taking the
/// default would have lowered the cap on the browser that governs, on the run
/// that added WebGPU. `using_resolution` copies the 1D/2D/3D trio verbatim from
/// the adapter, so what is asked for is what the adapter said it has — and
/// `the_web_arm_asks_for_a_resolution_no_default_would_have_given` pins that it
/// is above the default rather than merely equal to it.
///
/// The rest of the set stays at the WebGL2 floor on both browser APIs. That is
/// deliberate: it is not a WebGPU limitation, it is what keeps one web
/// behaviour instead of two. Nothing on the web path uses a storage buffer or a
/// compute pass to lose by it.
pub fn device_limits(adapter: wgpu::Limits, web: bool) -> wgpu::Limits {
    if web {
        wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter)
    } else {
        adapter
    }
}

/// `Fifo` is the only present mode WebGL2 has — the browser paces presentation
/// through `requestAnimationFrame`.
const fn present_mode(web: bool) -> wgpu::PresentMode {
    if web {
        wgpu::PresentMode::Fifo
    } else {
        wgpu::PresentMode::AutoVsync
    }
}

pub const PRESENT_MODE: wgpu::PresentMode = present_mode(WEB);

/// Request the device this build wants from an already-chosen adapter.
///
/// `extra_features` is what the caller asks for beyond `Features::empty()`; the
/// app side passes `adapter.features() & STAGING_RING_FEATURE`. That feature
/// lets a voxel grid's 32 MiB plane be DMA'd rather than pushed by the frame
/// thread — measured 17.6 ms → 2.0 ms inside `prepare` on the desktop shape.
/// WebGL2 takes `Features::empty()`.
pub async fn request_device(
    adapter: &wgpu::Adapter,
    extra_features: wgpu::Features,
) -> (wgpu::Device, wgpu::Queue) {
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

/// Classify what the adapter says it is. Exhaustive on purpose: a new
/// `DeviceType` variant should be a compile error, not a fall into `Unknown`.
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

    const BROWSER: [wgpu::TextureFormat; 2] = [Bgra8UnormSrgb, Bgra8Unorm];

    const DESKTOP: [wgpu::TextureFormat; 4] =
        [Bgra8UnormSrgb, Bgra8Unorm, Rgba8UnormSrgb, Rgba8Unorm];

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
        // It takes the *first* such format: the surface's ordering is respected.
        assert_eq!(
            preferred_surface_format(&[Rgba8Unorm, Bgra8Unorm], true),
            Rgba8Unorm
        );
        assert_eq!(
            preferred_surface_format(&[Bgra8Unorm, Rgba8Unorm], true),
            Bgra8Unorm
        );
    }

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

    #[test]
    fn a_surface_offering_nothing_useful_still_yields_a_format() {
        assert_eq!(
            preferred_surface_format(&[Rgba8UnormSrgb], false),
            Rgba8UnormSrgb
        );
        // All-sRGB: the web arm's search finds nothing and falls through.
        assert_eq!(
            preferred_surface_format(&[Rgba8UnormSrgb], true),
            Rgba8UnormSrgb
        );
        for web in [true, false] {
            assert_eq!(preferred_surface_format(&[], web), Rgba8UnormSrgb, "{web}");
        }
    }

    #[test]
    fn the_native_arm_requests_the_adapters_own_limits() {
        let adapter = wgpu::Limits::default();
        assert_eq!(device_limits(adapter.clone(), false), adapter);
    }

    #[test]
    fn the_web_arm_clamps_to_webgl2_and_lifts_only_the_resolution() {
        let floor = wgpu::Limits::downlevel_webgl2_defaults();
        // `Limits::default()` is the full WebGPU set, far above the WebGL2 floor.
        let adapter = wgpu::Limits::default();
        let asked = device_limits(adapter.clone(), true);

        assert_ne!(
            asked, adapter,
            "the web arm passed the adapter's limits through"
        );
        assert_eq!(asked, floor.clone().using_resolution(adapter.clone()));

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

    /// The one figure the web arm lifts is lifted *above* what a device gets for
    /// free, on the adapter shapes both browser APIs actually report.
    ///
    /// The equality above is not enough on its own: `using_resolution(adapter)`
    /// and "accept `Limits::default()`" agree exactly when the adapter reports
    /// 8192, and that is Chromium-on-SwiftShader's number. A pin taken there
    /// would pass with the request removed.
    #[test]
    fn the_web_arm_asks_for_a_resolution_no_default_would_have_given() {
        // What a WebGPU device is handed when nothing asks for more.
        let webgpu_default = wgpu::Limits::default().max_texture_dimension_2d;
        assert_eq!(webgpu_default, 8192, "wgpu's default 2D ceiling moved");

        // Measured 2026-08-22 on this box: Firefox's WebGL2 on a real driver.
        // Chromium's WebGL2 there is SwiftShader and reports the default itself,
        // which is exactly why it cannot be the fixture.
        const FIREFOX_REAL_DRIVER: u32 = 32768;

        let adapter = wgpu::Limits {
            max_texture_dimension_2d: FIREFOX_REAL_DRIVER,
            ..wgpu::Limits::default()
        };
        let asked = device_limits(adapter, true);
        assert_eq!(asked.max_texture_dimension_2d, FIREFOX_REAL_DRIVER);
        assert!(
            asked.max_texture_dimension_2d > webgpu_default,
            "the web arm asked for {} px, which is what a device gets without \
             asking. The adapter's resolution is no longer being requested, and \
             every browser above the default silently lost the difference.",
            asked.max_texture_dimension_2d,
        );
    }

    #[test]
    fn the_web_surface_asks_for_fifo_and_native_for_autovsync() {
        assert_eq!(present_mode(true), wgpu::PresentMode::Fifo);
        assert_eq!(present_mode(false), wgpu::PresentMode::AutoVsync);
        assert_eq!(PRESENT_MODE, present_mode(WEB));
    }

    /// `Cpu` mapping to anything but `Software` is the one that matters — a
    /// browser falling back to SwiftShader is a real path.
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

    /// Which arm this build takes — the only claim in the file a host
    /// `cargo test` cannot make by running code.
    ///
    /// Scraped rather than asserted: `cfg!(target_family = "wasm")` (true for
    /// WASI too) evaluates the same on this host and differently in a browser.
    #[test]
    fn the_web_fork_is_the_wasm32_arch_and_nothing_else() {
        let source = include_str!("device.rs");
        // Only the shipped half: the assertions below quote the strings they
        // look for, so scanning the whole file would find them in this test.
        let (code, _) = source
            .split_once("#[cfg(test)]")
            .expect("device.rs no longer has a test module");

        // Every needle is counted before it is read: a second occurrence (a
        // decoy in a comment or a literal) would mean the wrong one got checked.
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
