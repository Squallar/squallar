//! The two degrade rungs the raymarch has, and how one is picked.
//!
//! # Why there are rungs at all, and why these two
//!
//! Spike 0a measured the offscreen raymarch on an RTX 3090 over Vulkan: 96
//! steps, a 256^3 `Rg8Unorm` grid, empty-cell skip and early-out on, gradient
//! shading on.
//!
//! | offscreen   | gpu ms |
//! |-------------|-------:|
//! | 2560 x 1440 |  1.776 |
//! | 1440 x 900  |  0.774 |
//! | 720 x 450   |  0.229 |
//!
//! Two things follow, and they are the whole design.
//!
//! **Resolution is a real lever, at about 85% efficiency.** Quartering the
//! pixel count buys 3.4x rather than the ideal 4x. The cost model behind that
//! is texture-unit bound — 267 to 288 G dependent 3D-linear fetches per second,
//! which matches the 3090's trilinear rate — so frame cost is
//! `covered px x steps x fetches/step / the device's 3D-linear rate`, with no
//! hidden ALU or bandwidth term. That is what makes extrapolating to devices
//! nobody measured defensible at all.
//!
//! **Shading, not steps, is the expensive knob.** The central-difference
//! gradient costs seven fetches per step against one, and measured 2.4x
//! (0.774 ms against 0.325 at 1440 x 900). It is therefore a *second*,
//! independent rung rather than something folded into the resolution ladder.
//!
//! Extrapolated from that model and **not measured**: an integrated GPU at
//! 12-23 ms and a phone at 23-60 ms at 1440 x 900 — unusable at full pane size
//! — but 3.5-7 and 7-18 ms at 720 x 450, which ships. Designing the resolution
//! rung in from the start is the reason the raymarch is offscreen at all: a
//! callback inside egui's own pass has no way to drop quality for a frame.
//!
//! # Re-measured after the voxel-locked march (2026-08-09)
//!
//! The march now steps one cell along the ray (up to ~2.7x the samples of the
//! 96-step version it replaced, jittered, breaking at the box exit) and the
//! bridge anchors the empty-skip at the palette's fade boundary. Measured on
//! the same RTX 3090 over a dense real volume — KCRP 2017-08-26 (Harvey),
//! 52.1% of cells occupied, `tests/volume_march_cost.rs`:
//!
//! | offscreen   | shaded, before -> after | unshaded, before -> after |
//! |-------------|------------------------:|--------------------------:|
//! | 1440 x 900  |     0.549 -> 0.454 ms   |        0.214 -> 0.250 ms  |
//! | 720 x 450   |     0.215 -> 0.169 ms   |        0.079 -> 0.103 ms  |
//!
//! Shaded got *cheaper*: the fade-anchored skip stops paying seven fetches
//! per step inside the sub-visible shell, which outweighs the extra steps.
//! Unshaded paid the steps (+17-30%).
//!
//! # Re-measured after the cloud rung (this change)
//!
//! [`GradientShading::On`] now selects the whole **cloud look**: gradient
//! lighting, the mip-blended smooth reconstruction, and half-cell steps
//! (`volume::bridge::{CLOUD_RECONSTRUCTION_LOD, CLOUD_STEP_CELLS}`). The
//! half-cell step is the expensive part — roughly twice the samples — and it
//! is what takes the jitter's per-step opacity residual below the eight-bit
//! level. `Off` is unchanged: the raw trilinear field at one-cell steps, the
//! jagged-unlit floor every instrument measures. Same RTX 3090, same
//! harness, dense (Harvey, 45.7% occupied at this box) and sparse (KCRP
//! 2021-08-01, 5.6%) volumes:
//!
//! | offscreen   | cloud, dense | cloud, sparse | floor, dense | floor, sparse |
//! |-------------|-------------:|--------------:|-------------:|--------------:|
//! | 1440 x 900  |     0.766 ms |      0.607 ms |     0.263 ms |      0.351 ms |
//! | 720 x 450   |     0.249 ms |      0.206 ms |     0.105 ms |      0.146 ms |
//!
//! (The sparse *floor* costs more than the dense one because nothing
//! saturates: rays cross the whole box with no early-out.)
//!
//! # The ladder's order: lighting degrades before resolution
//!
//! The degraded states run `Native+On -> Native+Off -> Half+Off ->
//! Quarter+Off`: a device that cannot afford the top rung gives up the cloud
//! look before it gives up pixels, and the floor is always the jagged-unlit
//! march. On the fetch-bound model the cloud rung at native size extrapolates
//! to 23-38 ms on an integrated GPU — not a frame — and even unshaded native
//! (8-13 ms) crowds a frame the rest of the application also lives in, so
//! `Integrated` lands at `Half`+`Off` (3-5 ms). The consequence worth
//! writing down: only a discrete adapter renders the cloud look today, and
//! that is a decision about honesty under extrapolated budgets, not a
//! measured refusal — an integrated part that proves faster earns its rung
//! back by measurement, the way every number in this table arrived.
//!
//! # Why the selection is a pure function of two arguments
//!
//! `select` takes both the device class *and* the platform ceiling rather than
//! reading `cfg!` inline. `cfg` arms are per-target, so a rule written with
//! `cfg!` inside it can only ever be tested on the arm the test runner was
//! built for — and the arms that matter most here are the two no CI row runs a
//! test binary for. Passing the ceiling in makes every arm reachable from one
//! host test. `volume::disposition` already uses this shape for the same
//! reason.

use egui_wgpu::wgpu;

use crate::constants::{VOLUME_OFFSCREEN_BUDGET_BYTES, VOLUME_OFFSCREEN_REFERENCE_PANE_PX};

/// Bytes one offscreen pixel costs: `Rgba8Unorm`.
///
/// The format is fixed rather than negotiated. It is the one colour format
/// every target this build reaches can render into, and the blit's whole
/// premise is that the offscreen holds sRGB-encoded premultiplied bytes — an
/// `Rgba8UnormSrgb` target would make the hardware decode them on the way out
/// and undo the encode the raymarch just did.
pub const OFFSCREEN_BYTES_PER_PIXEL: usize = 4;

/// How far the offscreen is scaled down from the pane it will be blitted into.
///
/// Named by the *linear* scale, not the pixel count: `Half` is half the width
/// and half the height, so a quarter of the pixels and about 3.4x the speed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResolutionRung {
    /// One offscreen pixel per pane pixel.
    Native,
    /// Half the width and half the height.
    Half,
    /// A quarter of the width and a quarter of the height.
    Quarter,
}

impl ResolutionRung {
    /// Every rung, finest first. The order is the ladder `fit` walks.
    pub const LADDER: [Self; 3] = [Self::Native, Self::Half, Self::Quarter];

    /// What each pane axis is divided by.
    pub fn linear_divisor(self) -> u32 {
        match self {
            Self::Native => 1,
            Self::Half => 2,
            Self::Quarter => 4,
        }
    }

    /// The next rung down, or `None` at the bottom of the ladder.
    pub fn next_coarser(self) -> Option<Self> {
        match self {
            Self::Native => Some(Self::Half),
            Self::Half => Some(Self::Quarter),
            Self::Quarter => None,
        }
    }

    /// The coarser of two rungs.
    pub fn coarser_of(self, other: Self) -> Self {
        self.max(other)
    }
}

/// Whether the raymarch renders the cloud look: gradient lighting plus the
/// smoothed reconstruction plus half-cell steps, which the bridge sets as one
/// decision.
///
/// Off is not a cosmetic downgrade — it is the difference between one raw
/// fetch per one-cell step and seven fetches per half-cell step, measured at
/// ~2.9x on the dense volume. See the module doc's cloud-rung table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GradientShading {
    /// The cloud look. Seven fetches per contributing half-cell step.
    On,
    /// Flat: the jagged-unlit floor, one fetch per one-cell step.
    Off,
}

impl GradientShading {
    /// Whether shading is on, in the form the uniform block wants.
    pub fn is_on(self) -> bool {
        self == Self::On
    }

    /// The cheaper of two settings: on only when both are.
    pub fn cheaper_of(self, other: Self) -> Self {
        self.max(other)
    }
}

/// A point on both rungs at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VolumeQuality {
    /// See [`ResolutionRung`].
    pub resolution: ResolutionRung,
    /// See [`GradientShading`].
    pub shading: GradientShading,
}

impl VolumeQuality {
    /// The best this build ever offers.
    pub const BEST: Self = Self {
        resolution: ResolutionRung::Native,
        shading: GradientShading::On,
    };

    /// The cheapest this build ever offers.
    pub const CHEAPEST: Self = Self {
        resolution: ResolutionRung::Quarter,
        shading: GradientShading::Off,
    };

    /// This quality, held to a ceiling on each rung independently.
    ///
    /// Independently is the point: a ceiling that said "at most Half, no
    /// shading" must not let a discrete GPU claw back shading by being fast, and
    /// must not force a slow device up to Half if it had already chosen
    /// Quarter.
    pub fn capped_by(self, ceiling: Self) -> Self {
        Self {
            resolution: self.resolution.coarser_of(ceiling.resolution),
            shading: self.shading.cheaper_of(ceiling.shading),
        }
    }
}

/// What kind of thing is going to run the shader.
///
/// Derived from `AdapterInfo::device_type`, which is the only capability signal
/// available before anything has been rendered. It is a coarse signal and it is
/// deliberately the *only* one: the alternative is a per-frame timing loop,
/// which belongs to the pane that owns the frame, not to the pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceClass {
    /// A discrete GPU with its own memory. The class the table was measured on.
    Discrete,
    /// An integrated GPU sharing memory with the CPU.
    Integrated,
    /// A virtualised or hosted adapter — a VM, or a remote desktop.
    Virtual,
    /// A software rasteriser. Correct, and orders of magnitude too slow.
    Software,
    /// Anything the driver would not name. **This is what a browser reports**:
    /// WebGL2 exposes no device type, so every wasm build lands here whatever
    /// silicon is underneath.
    Unknown,
}

impl DeviceClass {
    /// Classify what the adapter says it is.
    ///
    /// Exhaustive on purpose: a new `DeviceType` variant should be a compile
    /// error here, not a silent fall into `Unknown`.
    pub fn from_device_type(device_type: wgpu::DeviceType) -> Self {
        match device_type {
            wgpu::DeviceType::DiscreteGpu => Self::Discrete,
            wgpu::DeviceType::IntegratedGpu => Self::Integrated,
            wgpu::DeviceType::VirtualGpu => Self::Virtual,
            wgpu::DeviceType::Cpu => Self::Software,
            wgpu::DeviceType::Other => Self::Unknown,
        }
    }

    /// What this class would pick with no platform ceiling over it.
    ///
    /// The numbers behind each row are in the module doc, and every degraded
    /// row sits on the ladder's stated order — lighting surrendered before
    /// resolution, jagged-unlit as the floor. In short: `Discrete` is the
    /// class the table was measured on and affords the cloud rung;
    /// `Integrated` extrapolates past a frame at both native rungs (cloud
    /// 23-38 ms, even unshaded 8-13), so it holds Half and the flat march;
    /// `Virtual` and `Unknown` are unknown quantities that could be either,
    /// so they take the same; `Software` is known to be hopeless and takes
    /// the bottom of the ladder, where it will at least produce a picture.
    pub fn unconstrained_quality(self) -> VolumeQuality {
        match self {
            Self::Discrete => VolumeQuality {
                resolution: ResolutionRung::Native,
                shading: GradientShading::On,
            },
            Self::Integrated | Self::Virtual | Self::Unknown => VolumeQuality {
                resolution: ResolutionRung::Half,
                shading: GradientShading::Off,
            },
            Self::Software => VolumeQuality::CHEAPEST,
        }
    }
}

/// The per-target quality ceilings, named **outside** the `cfg` cascade so all
/// three are reachable from any target's tests.
///
/// This is the shape `constants::WASM_VOLUME_GRID_CELLS` already uses, and it
/// is here for the reason that commit gives: a `cfg`-selected constant can only
/// be checked by the target that compiles it, and this workspace runs
/// `cargo test` on exactly one of three. Spelt as literals inside the cascade,
/// two of the three could be edited freely — changing the wasm ceiling to
/// [`VolumeQuality::BEST`] failed zero host tests, which is a browser silently
/// promoted to the full-size shaded march on the target with the least
/// headroom and the least coverage.
///
/// **wasm** is capped at Half and unshaded because the browser reports
/// `DeviceType::Other` whatever the silicon is, so a desktop browser and a
/// phone browser are indistinguishable here. Capping at what the phone can
/// survive is the only honest choice until something measures the frame.
pub const WASM_PLATFORM_CEILING: VolumeQuality = VolumeQuality {
    resolution: ResolutionRung::Half,
    shading: GradientShading::Off,
};

/// The mobile arm. See [`WASM_PLATFORM_CEILING`].
///
/// The same cap, arrived at by measurement rather than by ignorance: a phone at
/// 1440 x 900 extrapolates to 23-60 ms, which is not a frame; at 720 x 450 it is
/// 7-18 ms with shading and roughly 3-7.5 without. Shading is the cheap half of
/// that saving and the one a user is least likely to notice on a five-inch
/// screen.
pub const MOBILE_PLATFORM_CEILING: VolumeQuality = VolumeQuality {
    resolution: ResolutionRung::Half,
    shading: GradientShading::Off,
};

/// The desktop arm. See [`WASM_PLATFORM_CEILING`].
///
/// Uncapped: the measured table is a desktop table, and a discrete GPU there
/// should get what it paid for.
pub const DESKTOP_PLATFORM_CEILING: VolumeQuality = VolumeQuality::BEST;

/// The best quality this target may select, whatever the adapter claims.
///
/// The cascade shape is the one `constants::MAX_LOOP_FRAMES` documents, and for
/// the reason it documents: `cfg` arms have no ordering and no fallthrough, so
/// the `not(target_arch = "wasm32")` guard on the lower two arms is what keeps
/// wasm32 from matching two of them.
///
/// The arms *select between* the three named constants above rather than
/// repeating their literals, so the selection is the only thing here a host
/// build cannot check — which is the one thing no other target can check on
/// this one's behalf.
#[cfg(target_arch = "wasm32")]
pub const PLATFORM_CEILING: VolumeQuality = WASM_PLATFORM_CEILING;
/// See the wasm32 arm.
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const PLATFORM_CEILING: VolumeQuality = MOBILE_PLATFORM_CEILING;
/// See the wasm32 arm.
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const PLATFORM_CEILING: VolumeQuality = DESKTOP_PLATFORM_CEILING;

/// The quality to render a volume at on this adapter.
///
/// `ceiling` is a parameter rather than [`PLATFORM_CEILING`] read inline — see
/// the module doc for why.
///
/// Called once per renderer, from `App::install_volume_bridge`, as
/// `select(DeviceClass::from_device_type(adapter.get_info().device_type),
/// PLATFORM_CEILING)`. The result is fixed for the life of that renderer — a
/// device does not change class — and what varies per frame is the pane's size,
/// which [`VolumeQuality::fit_to_budget`] applies on top and which may step the
/// resolution rung down again.
///
/// It had no production caller while WP-I existed on its own, and every arm was
/// pinned by tests for that reason. The arms that had never run outside a test
/// until this shipped are `Virtual` and `Unknown` — which is what a browser
/// reports for every adapter it exposes, so the web build takes one of them on
/// every device.
pub fn select(class: DeviceClass, ceiling: VolumeQuality) -> VolumeQuality {
    class.unconstrained_quality().capped_by(ceiling)
}

/// An offscreen size, and the quality that actually produced it.
///
/// The quality comes back because it may not be the one that went in: the
/// budget can force a coarser rung, and the caller has to write the rung it
/// *got* into the uniform block rather than the one it asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FittedOffscreen {
    /// Width and height in texels. Never zero on either axis.
    pub size: [u32; 2],
    /// The quality this size was reached at.
    pub quality: VolumeQuality,
}

impl FittedOffscreen {
    /// What the texture will cost.
    pub fn bytes(&self) -> usize {
        offscreen_bytes(self.size)
    }
}

/// Bytes an offscreen of this size occupies.
pub fn offscreen_bytes(size: [u32; 2]) -> usize {
    size[0] as usize * size[1] as usize * OFFSCREEN_BYTES_PER_PIXEL
}

impl VolumeQuality {
    /// The offscreen size for a pane, stepping down the ladder until it fits.
    ///
    /// Total by construction: it always returns a size of at least 1 x 1, and
    /// the budget is honoured at every rung above the bottom. At the bottom, if
    /// a pane is still too large — an 8K display against the mobile budget — the
    /// size is scaled proportionally rather than refused, because a blurry
    /// volume is a better answer than a pane that says nothing.
    ///
    /// The only case where the result can exceed the budget is a budget too
    /// small to pay for a single pixel, which the compile-time assertions on
    /// `VOLUME_OFFSCREEN_BUDGET_BYTES` rule out.
    pub fn fit(self, pane_px: [u32; 2], budget_bytes: usize) -> FittedOffscreen {
        let mut resolution = self.resolution;
        loop {
            let size = scale_pane(pane_px, resolution);
            let quality = Self { resolution, ..self };
            if offscreen_bytes(size) <= budget_bytes {
                return FittedOffscreen { size, quality };
            }
            match resolution.next_coarser() {
                Some(coarser) => resolution = coarser,
                None => {
                    return FittedOffscreen {
                        size: shrink_into_budget(size, budget_bytes),
                        quality,
                    };
                }
            }
        }
    }

    /// The offscreen size for a pane against this target's own budget.
    pub fn fit_to_budget(self, pane_px: [u32; 2]) -> FittedOffscreen {
        self.fit(pane_px, VOLUME_OFFSCREEN_BUDGET_BYTES)
    }
}

/// A pane divided by a rung, never rounded away to nothing.
///
/// `div_ceil` rather than `/`: a pane one pixel wide must still produce a
/// texture one pixel wide, and `wgpu` rejects a zero extent outright — from
/// inside a callback, where there is no `Result` to check.
fn scale_pane(pane_px: [u32; 2], rung: ResolutionRung) -> [u32; 2] {
    let divisor = rung.linear_divisor();
    [
        pane_px[0].div_ceil(divisor).max(1),
        pane_px[1].div_ceil(divisor).max(1),
    ]
}

/// Scale a size down proportionally until it fits, preserving aspect ratio.
fn shrink_into_budget(size: [u32; 2], budget_bytes: usize) -> [u32; 2] {
    let affordable_pixels = budget_bytes / OFFSCREEN_BYTES_PER_PIXEL;
    let pixels = size[0] as f64 * size[1] as f64;
    if pixels <= affordable_pixels as f64 {
        return size;
    }
    // Both axes shrink by the same factor, so the area shrinks by its square.
    let factor = (affordable_pixels as f64 / pixels).sqrt();
    [
        ((size[0] as f64 * factor).floor() as u32).max(1),
        ((size[1] as f64 * factor).floor() as u32).max(1),
    ]
}

/// The offscreen this target's budget was sized against.
///
/// Exists so `constants`' budget tests have a concrete number to check, the way
/// `VOLUME_GRID_CELLS` gives the grid budget one. The reference pane is a
/// constant; what differs per target is the ceiling applied to it.
pub fn reference_offscreen() -> FittedOffscreen {
    PLATFORM_CEILING.fit(
        VOLUME_OFFSCREEN_REFERENCE_PANE_PX,
        VOLUME_OFFSCREEN_BUDGET_BYTES,
    )
}

#[path = "volume_quality/tests.rs"]
#[cfg(test)]
mod tests;
