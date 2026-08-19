#![warn(clippy::all)]
#![forbid(unsafe_code)]

//! The wgpu boundary of rustdar.
//!
//! The one egui/wgpu renderer every target — desktop, web (wasm32 + WebGL2),
//! Android and iOS — draws through: frame prepare/submit ([`egui_renderer`]),
//! the banded texture upload path, the pane mirror, the staging ring
//! ([`staging_ring`]), and the device-request policy ([`device`]).
//!
//! What deliberately does NOT live here: the app loop, the window/surface
//! lifecycle (rustdar-frontend's `AppState` spans surface, renderer and
//! volume support and stays above), and the 3D volume stack (rustdar-volumetric
//! depends on this crate, never the reverse — a dev-dep back from here onto it
//! arrives with the GPU test suite at WO-RV and is legal because dev-deps
//! never enter the normal graph).

/// The device-request policy: which surface format, which limits, which
/// present mode — the forks that are silent when they go the wrong way.
pub mod device;
pub mod egui_renderer;
pub mod staging_ring;

/// Type alias for a reference-counted Window.
///
/// Duplicated from rustdar-frontend deliberately — two type aliases to the
/// same type are the same type, and the alternative is this crate reaching up
/// into the app crate for a name.
pub type WindowRef = std::sync::Arc<winit::window::Window>;

/// Fails the build when this crate's two `wgpu` paths are different copies; the
/// notes below say why that matters, and `tests/wgpu_guard.rs` keeps this from
/// being edited into something vacuous.
///
/// Scope is this crate only — a second wgpu reached by another member is
/// invisible here, and to any Rust check. Nothing covers that today.
const _: () = {
    /// The `wgpu` entry in this crate's `Cargo.toml`.
    type OurWgpu = ::wgpu::Instance;
    /// The copy egui-wgpu links and renders through.
    type EguiWgpu = egui_wgpu::wgpu::Instance;

    #[diagnostic::on_unimplemented(
        message = "egui-wgpu links a different copy of `wgpu` than this crate configures",
        label = "this is egui-wgpu's `wgpu`, and it is not this crate's `wgpu`",
        note = "the backend features in rustdar-gpu/Cargo.toml apply to this crate's \
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
/// aliases, so this is the real compiled-in set, not a restatement of it.
/// Deliberately written `::wgpu::` rather than the `egui_wgpu::wgpu` re-export
/// imported above: this and the guard above are the only places that name the
/// *direct* dependency.
///
/// Two failures it turns into build errors.
///
/// **The `wgpu` entry in `Cargo.toml` going away.** It carries this crate's
/// entire per-target backend selection and nothing imports it — every `wgpu::`
/// path here comes through `egui_wgpu::wgpu`, which is what keeps a single wgpu
/// in the graph. That makes the entry look dead to `cargo machete`, to
/// `cargo udeps`, and to anyone tidying the manifest. Deleting it still
/// compiles: wgpu falls back to the `std` + `wgsl` egui-wgpu asks for, with no
/// backend at all, and the app dies at `request_adapter` instead. Naming the
/// crate here also makes the dependency genuinely used, so those tools stop
/// reporting it.
///
/// **`webgpu` coming back.** Features are additive across the graph, so any
/// dependency that turns on `wgpu/default` re-enables it regardless of what this
/// crate asks for — which is how the duplicate-bindings failure got in. A build
/// that has drifted back onto WebGPU now says so here rather than in a browser.
const _: () = {
    let enabled = ::wgpu::Instance::enabled_backend_features();

    assert!(
        !enabled.contains(::wgpu::Backends::BROWSER_WEBGPU),
        "wgpu's `webgpu` feature is enabled. This build targets WebGL2 because \
         Firefox has no stable WebGPU; something re-enabled `wgpu/default`."
    );

    // Only reachable when `web` is on and `webgl` is not. Dropping `webgl` on its
    // own never gets here: it implies `wgpu/web`, which gates `wgpu::web_sys`, so
    // egui-wgpu stops compiling first with E0433 and this crate is never built.
    #[cfg(target_arch = "wasm32")]
    assert!(
        enabled.contains(::wgpu::Backends::GL),
        "no WebGL2 backend compiled in - wgpu's `webgl` feature is off. Note \
         that `gles` does not cover the browser. See the wasm32 target section \
         of this crate's Cargo.toml."
    );

    #[cfg(not(target_arch = "wasm32"))]
    assert!(
        !enabled.is_empty(),
        "no native wgpu backend compiled in. See the per-target wgpu feature \
         sections of this crate's Cargo.toml."
    );
};
