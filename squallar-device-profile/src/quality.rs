//! The two degrade rungs the raymarch has, and how one is picked.
//!
//! Measured on an RTX 3090 over Vulkan, dense (KCRP 2017-08-26 Harvey, 45.7%
//! occupied) and sparse (KCRP 2021-08-01, 5.6%) volumes, `tests/volume_march_cost.rs`:
//!
//! | offscreen   | cloud, dense | cloud, sparse | floor, dense | floor, sparse |
//! |-------------|-------------:|--------------:|-------------:|--------------:|
//! | 1440 x 900  |     0.766 ms |      0.607 ms |     0.263 ms |      0.351 ms |
//! | 720 x 450   |     0.249 ms |      0.206 ms |     0.105 ms |      0.146 ms |
//!
//! (The sparse *floor* costs more than the dense one because nothing
//! saturates: rays cross the whole box with no early-out.)
//!
//! Resolution is a real lever at ~85% efficiency — quartering the pixel count
//! buys 3.4x, not 4x. The cost model is texture-unit bound (267-288 G dependent
//! 3D-linear fetches/s, matching the 3090's trilinear rate), so frame cost is
//! `covered px x steps x fetches/step / the device's 3D-linear rate`, which is
//! what makes extrapolating to unmeasured devices defensible.
//!
//! Shading, not steps, is the expensive knob: the central-difference gradient
//! costs seven fetches per step against one. It is therefore a second,
//! independent rung.
//!
//! Extrapolated and **not measured**: an integrated GPU at 12-23 ms and a phone
//! at 23-60 ms at 1440 x 900, but 3.5-7 and 7-18 ms at 720 x 450.
//!
//! The ladder degrades lighting before resolution:
//! `Native+On -> Native+Off -> Half+Off -> Quarter+Off`.
//!
//! `select` takes the device class *and* the platform ceiling rather than
//! reading `cfg!` inline, so every arm is reachable from one host test.

use crate::constants::{VOLUME_OFFSCREEN_BUDGET_BYTES, VOLUME_OFFSCREEN_REFERENCE_PANE_PX};

/// Bytes one offscreen pixel costs: `Rgba8Unorm`. The blit's premise is that
/// the offscreen holds sRGB-encoded premultiplied bytes, so an
/// `Rgba8UnormSrgb` target would undo the encode the raymarch just did.
///
/// **The colour target alone.** A pane drawing 3D ground carries three more
/// attachments beside it, and [`GroundPass::bytes_per_pixel`] — not this
/// constant — is what every byte figure in this module multiplies by.
pub const OFFSCREEN_BYTES_PER_PIXEL: usize = 4;

/// Bytes the ground pass's occluder attachment costs a pixel: `Rgba8Unorm`,
/// carrying the packed ray parameter in RGB and the hit flag in A.
pub const OCCLUDER_BYTES_PER_PIXEL: usize = 4;

/// Bytes the ground pass's colour attachment costs a pixel: `Rgba8Unorm`. A
/// second colour target rather than a write into the offscreen, because the
/// raymarch pass *clears* that one — anything the ground wrote there would be
/// destroyed before it was read.
pub const GROUND_COLOUR_BYTES_PER_PIXEL: usize = 4;

/// Bytes the ground pass's own depth attachment costs a pixel: `Depth32Float`.
/// It lives inside the volume crate; egui's own pass still carries no depth.
pub const GROUND_DEPTH_BYTES_PER_PIXEL: usize = 4;

/// Everything a ground-drawing pane adds to its offscreen, per pixel: 12 B on
/// top of [`OFFSCREEN_BYTES_PER_PIXEL`], so 16 in total rather than 4.
pub const GROUND_BYTES_PER_PIXEL: usize =
    OCCLUDER_BYTES_PER_PIXEL + GROUND_COLOUR_BYTES_PER_PIXEL + GROUND_DEPTH_BYTES_PER_PIXEL;

/// Whether a pane's offscreen carries the ground pass's three attachments.
///
/// It rides every byte figure in this module rather than being added on top of
/// one, because [`VolumeQuality::fit`] walks a ladder until the result *fits*:
/// charging four bytes a pixel for a target that costs sixteen would
/// over-commit every ceiling here by three times the colour target, which is a
/// budget miss no later clamp recovers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GroundPass {
    /// The colour target alone — today's picture, and every pane with no
    /// height field behind it.
    Off,
    /// The occluder, the ground colour and the ground depth beside it.
    On,
}

impl GroundPass {
    /// Bytes one pixel of this pane's offscreen costs, every attachment counted.
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Off => OFFSCREEN_BYTES_PER_PIXEL,
            Self::On => OFFSCREEN_BYTES_PER_PIXEL + GROUND_BYTES_PER_PIXEL,
        }
    }
}

/// How far the offscreen is scaled down from the pane it will be blitted into.
/// Named by the *linear* scale: `Half` is half the width and half the height,
/// so a quarter of the pixels and about 3.4x the speed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResolutionRung {
    Native,
    Half,
    Quarter,
}

impl ResolutionRung {
    /// Every rung, finest first; the order the ladder `fit` walks.
    pub const LADDER: [Self; 3] = [Self::Native, Self::Half, Self::Quarter];

    pub fn linear_divisor(self) -> u32 {
        match self {
            Self::Native => 1,
            Self::Half => 2,
            Self::Quarter => 4,
        }
    }

    pub fn next_coarser(self) -> Option<Self> {
        match self {
            Self::Native => Some(Self::Half),
            Self::Half => Some(Self::Quarter),
            Self::Quarter => None,
        }
    }

    pub fn coarser_of(self, other: Self) -> Self {
        self.max(other)
    }
}

/// Whether the raymarch renders the cloud look: gradient lighting, the smoothed
/// reconstruction and half-cell steps, which the bridge sets as one decision.
/// Off is one raw fetch per one-cell step against seven per half-cell step,
/// measured at ~2.9x on the dense volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GradientShading {
    /// The cloud look. Seven fetches per contributing half-cell step.
    On,
    /// Flat: the jagged-unlit floor, one fetch per one-cell step.
    Off,
}

impl GradientShading {
    pub fn is_on(self) -> bool {
        self == Self::On
    }

    pub fn cheaper_of(self, other: Self) -> Self {
        self.max(other)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VolumeQuality {
    pub resolution: ResolutionRung,
    pub shading: GradientShading,
}

impl VolumeQuality {
    pub const BEST: Self = Self {
        resolution: ResolutionRung::Native,
        shading: GradientShading::On,
    };

    pub const CHEAPEST: Self = Self {
        resolution: ResolutionRung::Quarter,
        shading: GradientShading::Off,
    };

    /// This quality, held to a ceiling on each rung independently — a ceiling
    /// must neither restore shading nor force a device *up* to a finer rung.
    pub fn capped_by(self, ceiling: Self) -> Self {
        Self {
            resolution: self.resolution.coarser_of(ceiling.resolution),
            shading: self.shading.cheaper_of(ceiling.shading),
        }
    }
}

/// What kind of thing is going to run the shader. Derived from
/// `AdapterInfo::device_type`, the only capability signal available before
/// anything has been rendered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceClass {
    /// A discrete GPU with its own memory; the class the table was measured on.
    Discrete,
    Integrated,
    Virtual,
    /// A software rasteriser. Correct, and orders of magnitude too slow.
    Software,
    /// Anything the driver would not name. **This is what a browser reports**:
    /// WebGL2 exposes no device type, so every wasm build lands here whatever
    /// silicon is underneath.
    Unknown,
}

impl DeviceClass {
    /// What this class would pick with no platform ceiling over it.
    /// `Integrated` extrapolates past a frame at both native rungs (cloud
    /// 23-38 ms, unshaded 8-13), so it holds Half and the flat march;
    /// `Virtual` and `Unknown` take the same; `Software` takes the bottom.
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
/// wasm is capped at Half and unshaded: the browser reports `DeviceType::Other`
/// whatever the silicon is, so a desktop browser and a phone browser are
/// indistinguishable here.
pub const WASM_PLATFORM_CEILING: VolumeQuality = VolumeQuality {
    resolution: ResolutionRung::Half,
    shading: GradientShading::Off,
};

/// The mobile arm: a phone at 1440 x 900 extrapolates to 23-60 ms, which is not
/// a frame; at 720 x 450 it is 7-18 ms shaded and roughly 3-7.5 unshaded.
pub const MOBILE_PLATFORM_CEILING: VolumeQuality = VolumeQuality {
    resolution: ResolutionRung::Half,
    shading: GradientShading::Off,
};

/// The desktop arm — uncapped; the measured table is a desktop table.
pub const DESKTOP_PLATFORM_CEILING: VolumeQuality = VolumeQuality::BEST;

/// The best quality this target may select, whatever the adapter claims.
/// `cfg` arms have no ordering and no fallthrough, so the
/// `not(target_arch = "wasm32")` guard is what keeps wasm32 from matching two.
#[cfg(target_arch = "wasm32")]
pub const PLATFORM_CEILING: VolumeQuality = WASM_PLATFORM_CEILING;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const PLATFORM_CEILING: VolumeQuality = MOBILE_PLATFORM_CEILING;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const PLATFORM_CEILING: VolumeQuality = DESKTOP_PLATFORM_CEILING;

/// The quality to render a volume at on this adapter. Called once per renderer;
/// the result is fixed for its life. What varies per frame is the pane's size,
/// which [`VolumeQuality::fit`] applies on top and may step the rung down again.
pub fn select(class: DeviceClass, ceiling: VolumeQuality) -> VolumeQuality {
    class.unconstrained_quality().capped_by(ceiling)
}

/// An offscreen size, and the quality that actually produced it — which may not
/// be the one that went in, since the budget can force a coarser rung.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FittedOffscreen {
    pub size: [u32; 2],
    pub quality: VolumeQuality,
    /// Which attachment set this size was fitted against. Carried rather than
    /// re-decided, so [`FittedOffscreen::bytes`] cannot price a target the fit
    /// did not.
    pub ground: GroundPass,
}

impl FittedOffscreen {
    pub fn bytes(&self) -> usize {
        offscreen_bytes(self.size, self.ground)
    }
}

/// Bytes an offscreen of `size` occupies, every attachment `ground` implies
/// included.
pub fn offscreen_bytes(size: [u32; 2], ground: GroundPass) -> usize {
    size[0] as usize * size[1] as usize * ground.bytes_per_pixel()
}

impl VolumeQuality {
    /// The offscreen size for a pane, stepping down the ladder until it fits.
    /// Total by construction: always at least 1 x 1. At the bottom rung an
    /// oversized pane is scaled proportionally rather than refused, so the
    /// result can exceed the budget only for a budget under one pixel.
    pub fn fit(
        self,
        pane_px: [u32; 2],
        budget_bytes: usize,
        ground: GroundPass,
    ) -> FittedOffscreen {
        let mut resolution = self.resolution;
        loop {
            let size = scale_pane(pane_px, resolution);
            let quality = Self { resolution, ..self };
            if offscreen_bytes(size, ground) <= budget_bytes {
                return FittedOffscreen {
                    size,
                    quality,
                    ground,
                };
            }
            match resolution.next_coarser() {
                Some(coarser) => resolution = coarser,
                None => {
                    return FittedOffscreen {
                        size: shrink_into_budget(size, budget_bytes, ground),
                        quality,
                        ground,
                    };
                }
            }
        }
    }
}

/// A pane divided by a rung, never rounded away to nothing: `wgpu` rejects a
/// zero extent from inside a callback, where there is no `Result` to check.
fn scale_pane(pane_px: [u32; 2], rung: ResolutionRung) -> [u32; 2] {
    let divisor = rung.linear_divisor();
    [
        pane_px[0].div_ceil(divisor).max(1),
        pane_px[1].div_ceil(divisor).max(1),
    ]
}

fn shrink_into_budget(size: [u32; 2], budget_bytes: usize, ground: GroundPass) -> [u32; 2] {
    let affordable_pixels = budget_bytes / ground.bytes_per_pixel();
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

/// The offscreen this target's budget was sized against, for the budget tests.
/// The production path takes neither: a pane's offscreen is fitted against the
/// resolved `Budgets::offscreen_bytes` the painter was handed.
pub fn reference_offscreen() -> FittedOffscreen {
    reference_offscreen_with(GroundPass::Off)
}

/// [`reference_offscreen`] for a pane that draws 3D ground: four times the
/// bytes a pixel, and therefore fitted smaller out of the same budget.
pub fn reference_offscreen_with(ground: GroundPass) -> FittedOffscreen {
    PLATFORM_CEILING.fit(
        VOLUME_OFFSCREEN_REFERENCE_PANE_PX,
        VOLUME_OFFSCREEN_BUDGET_BYTES,
        ground,
    )
}

#[path = "quality/tests.rs"]
#[cfg(test)]
mod tests;
