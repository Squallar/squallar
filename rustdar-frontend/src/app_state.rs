use egui_wgpu::wgpu;

use crate::egui_renderer;
use crate::volume;

/// Minimum window dimension (width or height) in pixels
const MIN_SIZE: u32 = 1;

/// Whether this build is the browser build.
///
/// The three device decisions below fork on this value rather than on `#[cfg]`
/// attributes, so both arms of each are compiled and callable from a single host
/// test binary — the shape `volume::disposition(rendered, debug_build)` already
/// uses. `cfg!` expands to a literal `true` or `false`, so nothing is decided at
/// runtime and the arm this target does not take still optimises away.
///
/// It matters here more than most places. Every fork below is *silent* when it
/// goes the wrong way: an sRGB swapchain in a browser washes the colours out
/// with no validation error, WebGL2 limits requested natively cost texture size
/// with no message, and `AutoVsync` in a browser negotiates something nobody
/// asked for. None of the three produce a `Result` to check, and until this
/// commit `app_state.rs` had no test module at all, so all six arms were
/// unexercised and three of them were unreachable from a host build.
///
/// This line is now the only thing in the file a host build cannot check, which
/// is what `the_web_fork_is_the_wasm32_arch_and_nothing_else` scrapes.
const WEB: bool = cfg!(target_arch = "wasm32");

/// Selects the best surface format from available capabilities
fn select_surface_format(capabilities: &wgpu::SurfaceCapabilities) -> wgpu::TextureFormat {
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
/// `AppState::new` requests these limits, the device grants exactly them, and
/// `volume::probe` then reads them back off the device.
/// `volume::limits_shortfall` documents being testable against
/// `downlevel_webgl2_defaults()` and nothing connected the two;
/// `the_web_limits_this_app_requests_clear_the_volume_probes_floor` does.
fn device_limits(adapter: wgpu::Limits, web: bool) -> wgpu::Limits {
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

/// What this build asks the surface for. See [`present_mode`].
const PRESENT_MODE: wgpu::PresentMode = present_mode(WEB);

pub struct AppState {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub surface_config: wgpu::SurfaceConfiguration,
    pub surface: wgpu::Surface<'static>,
    pub egui_renderer: egui_renderer::EguiRenderer,
    /// The adapter the device came from, which used to be dropped here.
    ///
    /// It answers questions the device cannot: `get_texture_format_features` for
    /// any format the app might later want, and `get_capabilities` for a surface
    /// that is reconfigured after the fact. Both are needed by the 3D volume
    /// view, and re-requesting an adapter to ask is not equivalent — a second
    /// `request_adapter` may legitimately return a *different* one.
    pub adapter: wgpu::Adapter,
    /// What [`crate::volume::probe`] concluded about this device, before
    /// anything was created on it.
    ///
    /// Read it through [`crate::volume::support`] rather than directly:
    /// failures recorded since the probe ran — a rejected resource, a twice-lost
    /// surface — outrank it, and they deliberately live outside this struct
    /// because a lost surface destroys this struct. See `volume::degrade`.
    pub volume_support: volume::VolumeSupport,
    max_surface_dimension: u32,
}

impl AppState {
    pub async fn new(
        instance: &wgpu::Instance,
        surface: wgpu::Surface<'static>,
        // The `Arc`, not a bare `&Window`: `EguiRenderer::new` keeps a handle
        // so egui's own repaint requests can reach the event loop — see
        // `egui_renderer::install_repaint_wake`.
        window: &crate::WindowRef,
        width: u32,
        height: u32,
    ) -> Self {
        let power_pref = wgpu::PowerPreference::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: power_pref,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
            })
            .await
            .expect("Failed to find an appropriate adapter");

        let features = wgpu::Features::empty();
        // Native takes the adapter's actual limits so it is not held to a
        // portable floor; the web arm reconciles them with what WebGL2 can
        // express. See `device_limits`.
        let limits = device_limits(adapter.limits(), WEB);
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: None,
                required_features: features,
                required_limits: limits,
                memory_hints: Default::default(),
                experimental_features: Default::default(),
                trace: Default::default(),
            })
            .await
            .expect("Failed to create device");

        // Before a single volume resource exists, and before anything can fail
        // asynchronously: purely limits the device already reports and format
        // features the adapter already knows. Note `required_features` stays
        // `Features::empty()` and `device_limits` is untouched — an uncompressed
        // 3D texture needs no feature, and the web arm's `using_resolution`
        // already lifts `max_texture_dimension_3d`.
        let volume_support = volume::probe(&adapter, &device.limits());
        if let Some(why) = volume_support.reason() {
            log::info!("3D volume view unavailable: {why}");
        }
        // Installed unconditionally, including when the probe already said no:
        // the handler's other job is to keep wgpu's panicking default from
        // taking a browser tab down over an error a release build could survive.
        // Read the trade in `volume::install_error_latch` before moving this.
        volume::install_error_latch(&device);

        let swapchain_capabilities = surface.get_capabilities(&adapter);
        let swapchain_format = select_surface_format(&swapchain_capabilities);

        // Get the maximum texture dimension - wgpu requires surface dimensions to respect this
        let max_surface_dimension = device.limits().max_texture_dimension_2d;

        // Clamp surface dimensions to the device's texture dimension limit
        let width = width.clamp(MIN_SIZE, max_surface_dimension);
        let height = height.clamp(MIN_SIZE, max_surface_dimension);

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: swapchain_format,
            width,
            height,
            present_mode: PRESENT_MODE,
            desired_maximum_frame_latency: 2,
            alpha_mode: swapchain_capabilities.alpha_modes[0],
            view_formats: vec![],
        };

        surface.configure(&device, &surface_config);

        let egui_renderer =
            egui_renderer::EguiRenderer::new(&device, surface_config.format, None, 1, window);

        Self {
            device,
            queue,
            surface,
            surface_config,
            egui_renderer,
            adapter,
            volume_support,
            max_surface_dimension,
        }
    }

    pub fn resize_surface(&mut self, width: u32, height: u32) {
        // Clamp to device's maximum texture dimension (required by wgpu)
        let width = width.clamp(MIN_SIZE, self.max_surface_dimension);
        let height = height.clamp(MIN_SIZE, self.max_surface_dimension);

        if width != self.surface_config.width || height != self.surface_config.height {
            log::debug!("Resizing surface to {}x{}", width, height);
            self.surface_config.width = width;
            self.surface_config.height = height;
            self.surface.configure(&self.device, &self.surface_config);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use wgpu::TextureFormat::{Bgra8Unorm, Bgra8UnormSrgb, Rgba8Unorm, Rgba8UnormSrgb};

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

    /// The limits this app *requests* clear the floor the volume probe applies.
    ///
    /// `volume::limits_shortfall`'s doc says it is testable against
    /// `downlevel_webgl2_defaults()`, and `volume.rs` does test it against that
    /// — but nothing tied the figure the probe was exercised with to the figure
    /// the device request actually produces. They are the same only because
    /// `device_limits` happens to start from the same call, which is precisely
    /// the kind of "obviously the same" that this campaign has already paid for
    /// once. `AppState::new` requests these limits, the device grants exactly
    /// them, and `volume::probe` reads them back off the device — so this is the
    /// real path, not a restatement.
    #[test]
    fn the_web_limits_this_app_requests_clear_the_volume_probes_floor() {
        // The least capable browser this build targets: an adapter reporting
        // exactly the WebGL2 guarantee and not a pixel more.
        let barest = wgpu::Limits::downlevel_webgl2_defaults();
        assert_eq!(
            crate::volume::limits_shortfall(&device_limits(barest, true)),
            None,
            "the volume probe rejects the very limits this app asks a browser \
             for, so the 3D view could never be available in one"
        );

        // And on a capable browser, where `using_resolution` lifts the 3D
        // texture bound well past the grid.
        assert_eq!(
            crate::volume::limits_shortfall(&device_limits(wgpu::Limits::default(), true)),
            None
        );

        // Native asks for the adapter's own limits, so any adapter that could
        // run the app at all clears the floor too.
        for adapter in [wgpu::Limits::default(), wgpu::Limits::downlevel_defaults()] {
            assert_eq!(
                crate::volume::limits_shortfall(&device_limits(adapter, false)),
                None
            );
        }
    }

    /// Both present modes, from one host binary.
    #[test]
    fn the_web_surface_asks_for_fifo_and_native_for_autovsync() {
        assert_eq!(present_mode(true), wgpu::PresentMode::Fifo);
        assert_eq!(present_mode(false), wgpu::PresentMode::AutoVsync);
        assert_eq!(PRESENT_MODE, present_mode(WEB));
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
        let source = include_str!("app_state.rs");
        // Only the shipped half of the file. The assertions below quote the very
        // strings they look for, so scanning the whole file would find them in
        // this test's own source and pass no matter what the code did.
        let (code, _) = source
            .split_once("#[cfg(test)]")
            .expect("app_state.rs no longer has a test module");

        // Every needle is counted before it is read. One occurrence is the
        // claim; a second would mean whichever came first is what got checked,
        // and a decoy in a doc comment or a string literal would be a second.
        let unique = |needle: &str| {
            let n = code.matches(needle).count();
            assert_eq!(
                n, 1,
                "expected exactly one `{needle}` in app_state.rs, found {n}"
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
