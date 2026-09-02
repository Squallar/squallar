#![warn(clippy::all)]
#![forbid(unsafe_code)]

//! The wgpu boundary of squallar.
//!
//! The one egui/wgpu renderer every target — desktop, web (wasm32, WebGPU with
//! a WebGL2 fallback), Android and iOS — draws through: frame prepare/submit
//! ([`egui_renderer`]), the banded texture upload path, the pane mirror, the
//! staging ring ([`staging_ring`]), and the device-request policy ([`device`]).

/// Heap arithmetic behind the measured GPU capacity: pure, so the `unsafe`
/// readers can stay out of this crate.
pub mod capacity;
/// The device-request policy: which surface format, which limits, which
/// present mode.
pub mod device;
pub mod egui_renderer;
/// GPU pass timing through timestamp queries — the opt-in probe behind the
/// `gpu passes:` telemetry line.
pub mod gpu_probe;
/// The out-of-memory count the device's error sink raises and the frame loop
/// drains — one pressure event per frame, whatever the count.
pub mod pressure;
pub mod staging_ring;
/// A vector tile's tessellated fills, uploaded once and placed by a uniform —
/// the renderer half of [`squallar_egui::tile_mesh`].
pub mod tile_mesh;

/// Type alias for a reference-counted Window.
pub type WindowRef = std::sync::Arc<winit::window::Window>;

/// Fails the build when this crate's two `wgpu` paths are different copies.
/// Scope is this crate only — a second wgpu reached by another member is
/// invisible here, and to any Rust check.
const _: () = {
    type OurWgpu = ::wgpu::Instance;
    type EguiWgpu = egui_wgpu::wgpu::Instance;

    #[diagnostic::on_unimplemented(
        message = "egui-wgpu links a different copy of `wgpu` than this crate configures",
        label = "this is egui-wgpu's `wgpu`, and it is not this crate's `wgpu`",
        note = "the backend features in squallar-gpu/Cargo.toml apply to this crate's \
                copy, but rendering goes through egui-wgpu's; split, they configure nothing.",
        note = "egui-wgpu pins a wgpu major, so wgpu cannot move alone: bump egui, \
                egui-wgpu, egui-winit, walkers and wgpu together, and expect walkers to \
                gate it - it pins an exact egui minor. `cargo tree -i wgpu` lists the \
                copies that are in the graph now."
    )]
    trait IsOurWgpu {}

    impl IsOurWgpu for OurWgpu {}

    fn assert_is_our_wgpu<T: IsOurWgpu>() {}

    let _: fn() = assert_is_our_wgpu::<EguiWgpu>;
};

/// Check at compile time that the manifest's backend selection survived.
///
/// `Instance::enabled_backend_features` is a `const fn` over wgpu's own cfg
/// aliases, so this is the real compiled-in set. The `wgpu` manifest entry
/// carries this crate's per-target backend selection and is imported nowhere
/// else, so naming `::wgpu::` here is what keeps it from looking dead.
///
/// This asserts the configuration this build INTENDS, in both directions:
/// wasm32 must carry WebGPU and WebGL2 together, and no other target may carry
/// WebGPU at all.
const _: () = {
    let enabled = ::wgpu::Instance::enabled_backend_features();

    // The browser wants BOTH, and neither is optional. WebGPU alone would strand
    // every browser that has no usable WebGPU adapter — which on the platform
    // this is gated on is every one of them, since Firefox has not shipped it on
    // Linux. WebGL2 alone would strand the adapters WebGL2 has given up on: a
    // Chromium whose driver is blocklisted answers WebGL2 with SwiftShader.
    // `squallar_app::app::create_instance` is what chooses between them at
    // startup, and it can only choose from what is compiled in.
    #[cfg(target_arch = "wasm32")]
    assert!(
        enabled.contains(::wgpu::Backends::BROWSER_WEBGPU),
        "no WebGPU backend compiled in - wgpu's `webgpu` feature is off. The \
         browser build asks for it alongside WebGL2 and falls back when no \
         adapter answers; without it there is nothing to fall back FROM, and \
         a blocklisted-driver Chromium stays on SwiftShader. See the wasm32 \
         target section of this crate's Cargo.toml."
    );

    // Only reachable when `web` is on and `webgl` is not: dropping `webgl` alone
    // makes egui-wgpu fail first with E0433.
    #[cfg(target_arch = "wasm32")]
    assert!(
        enabled.contains(::wgpu::Backends::GL),
        "no WebGL2 backend compiled in - wgpu's `webgl` feature is off. Note \
         that `gles` does not cover the browser. It is also what actually runs \
         on Firefox/Linux today, WebGPU being unshipped there. See the wasm32 \
         target section of this crate's Cargo.toml."
    );

    // Native has no browser API to bind, so `webgpu` there is a feature nothing
    // could dispatch through: `wgpu`'s own `cfg(webgpu)` alias is wasm32-only.
    // Requesting it anywhere but the wasm32 section means `wgpu/default` came
    // back.
    #[cfg(not(target_arch = "wasm32"))]
    assert!(
        !enabled.contains(::wgpu::Backends::BROWSER_WEBGPU),
        "wgpu's `webgpu` feature reached a native target, where there is no \
         browser to reach. See the per-target wgpu feature sections of this \
         crate's Cargo.toml."
    );

    #[cfg(not(target_arch = "wasm32"))]
    assert!(
        !enabled.is_empty(),
        "no native wgpu backend compiled in. See the per-target wgpu feature \
         sections of this crate's Cargo.toml."
    );
};
