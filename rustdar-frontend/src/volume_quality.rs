//! The two degrade rungs the raymarch has, and how one is picked.
//!
//! # Why there are rungs at all, and why these two
//!
//! Spike 0a measured the offscreen raymarch on an RTX 3090 over Vulkan: 96
//! steps, a 256^3 `R8Unorm` grid, empty-cell skip and early-out on, gradient
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

/// Whether the raymarch shades with the central-difference gradient.
///
/// Off is not a cosmetic downgrade — it is the difference between one texture
/// fetch per step and seven, measured at 2.4x. See the module doc.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GradientShading {
    /// Shaded. Seven fetches per contributing step.
    On,
    /// Flat. One fetch per step.
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
    /// The numbers behind each row are in the module doc. In short: `Discrete`
    /// is the class the table was measured on and can afford everything;
    /// `Integrated` is extrapolated at 12-23 ms full-size, so it takes the
    /// resolution rung and keeps shading; `Virtual` and `Unknown` are unknown
    /// quantities that could be either, so they take both rungs; `Software` is
    /// known to be hopeless and takes the bottom of the ladder, where it will
    /// at least produce a picture.
    pub fn unconstrained_quality(self) -> VolumeQuality {
        match self {
            Self::Discrete => VolumeQuality {
                resolution: ResolutionRung::Native,
                shading: GradientShading::On,
            },
            Self::Integrated => VolumeQuality {
                resolution: ResolutionRung::Half,
                shading: GradientShading::On,
            },
            Self::Virtual | Self::Unknown => VolumeQuality {
                resolution: ResolutionRung::Half,
                shading: GradientShading::Off,
            },
            Self::Software => VolumeQuality::CHEAPEST,
        }
    }
}

/// The best quality this target may select, whatever the adapter claims.
///
/// The cascade shape is the one `constants::MAX_LOOP_FRAMES` documents, and for
/// the reason it documents: `cfg` arms have no ordering and no fallthrough, so
/// the `not(target_arch = "wasm32")` guard on the lower two arms is what keeps
/// wasm32 from matching two of them.
///
/// **Desktop** is uncapped: the measured table is a desktop table, and a
/// discrete GPU there should get what it paid for.
///
/// **Mobile** is capped at Half and unshaded. A phone at 1440 x 900 extrapolates
/// to 23-60 ms, which is not a frame; at 720 x 450 it is 7-18 ms with shading
/// and roughly 3-7.5 without. Shading is the cheap half of that saving and the
/// one a user is least likely to notice on a five-inch screen.
///
/// **wasm** takes the same cap, and for a different reason: the browser reports
/// `DeviceType::Other` whatever the silicon is, so a desktop browser and a
/// phone browser are indistinguishable here. Capping at what the phone can
/// survive is the only honest choice until something measures the frame.
#[cfg(target_arch = "wasm32")]
pub const PLATFORM_CEILING: VolumeQuality = VolumeQuality {
    resolution: ResolutionRung::Half,
    shading: GradientShading::Off,
};
/// See the wasm32 arm.
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const PLATFORM_CEILING: VolumeQuality = VolumeQuality {
    resolution: ResolutionRung::Half,
    shading: GradientShading::Off,
};
/// See the wasm32 arm.
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const PLATFORM_CEILING: VolumeQuality = VolumeQuality::BEST;

/// The quality to render a volume at on this adapter.
///
/// `ceiling` is a parameter rather than [`PLATFORM_CEILING`] read inline — see
/// the module doc for why.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A budget far past anything the ladder can ask for, so a test that means
    /// to exercise the rungs is not accidentally exercising the clamp.
    const UNLIMITED: usize = usize::MAX;

    /// Each rung divides both axes by its own linear divisor.
    #[test]
    fn a_rung_divides_both_axes_by_its_linear_divisor() {
        for (rung, expected) in [
            (ResolutionRung::Native, [1440, 900]),
            (ResolutionRung::Half, [720, 450]),
            (ResolutionRung::Quarter, [360, 225]),
        ] {
            let fitted = VolumeQuality {
                resolution: rung,
                shading: GradientShading::On,
            }
            .fit([1440, 900], UNLIMITED);
            assert_eq!(fitted.size, expected, "{rung:?} scaled the pane wrongly");
            assert_eq!(fitted.quality.resolution, rung, "{rung:?} was not kept");
        }
    }

    /// The measured table's own sizes are reachable, which is the point of it.
    ///
    /// 1440 x 900 at `Half` is 720 x 450 — the two rows the extrapolation to a
    /// phone rests on. If the divisors ever stopped producing them the numbers
    /// in the module doc would describe something the code cannot select.
    #[test]
    fn the_measured_rows_are_reachable_from_the_ladder() {
        let native = VolumeQuality::BEST.fit([2560, 1440], UNLIMITED);
        assert_eq!(native.size, [2560, 1440]);
        assert_eq!(
            VolumeQuality::BEST.fit([1440, 900], UNLIMITED).size,
            [1440, 900]
        );
        assert_eq!(
            VolumeQuality {
                resolution: ResolutionRung::Half,
                shading: GradientShading::On,
            }
            .fit([1440, 900], UNLIMITED)
            .size,
            [720, 450]
        );
    }

    /// A pane no rung can round away to nothing.
    ///
    /// `wgpu` refuses a zero-extent texture, and it refuses it inside the
    /// callback where `create_texture` returns no `Result` — so a plain integer
    /// divide here is a panic on a one-pixel pane, which a user reaches by
    /// dragging a splitter.
    #[test]
    fn a_tiny_pane_never_rounds_to_a_zero_sized_texture() {
        for pane in [[1, 1], [1, 900], [3, 2], [7, 5]] {
            for rung in ResolutionRung::LADDER {
                let fitted = VolumeQuality {
                    resolution: rung,
                    shading: GradientShading::Off,
                }
                .fit(pane, UNLIMITED);
                assert!(
                    fitted.size[0] >= 1 && fitted.size[1] >= 1,
                    "{rung:?} scaled {pane:?} to {:?}",
                    fitted.size
                );
            }
        }
    }

    /// Over budget at one rung, the fit steps down and says which rung it used.
    #[test]
    fn a_pane_over_budget_steps_down_a_rung_and_reports_it() {
        // 2560 x 1440 at Native is 14.06 MiB; at Half it is 3.52 MiB.
        let budget = 8 * 1024 * 1024;
        let fitted = VolumeQuality::BEST.fit([2560, 1440], budget);

        assert_eq!(fitted.quality.resolution, ResolutionRung::Half);
        assert_eq!(fitted.size, [1280, 720]);
        assert!(fitted.bytes() <= budget);
        assert_eq!(
            fitted.quality.shading,
            GradientShading::On,
            "the budget is a memory bound on the resolution rung; it must not \
             quietly turn shading off as well, or the reported quality stops \
             describing what was drawn"
        );
    }

    /// A pane too large even at the bottom rung is shrunk, not refused.
    #[test]
    fn a_pane_over_budget_at_every_rung_is_shrunk_proportionally() {
        // 7680 x 4320 at Quarter is 1920 x 1080 = 7.91 MiB, against 2 MiB.
        let budget = 2 * 1024 * 1024;
        let fitted = VolumeQuality::BEST.fit([7680, 4320], budget);

        assert_eq!(fitted.quality.resolution, ResolutionRung::Quarter);
        assert!(
            fitted.bytes() <= budget,
            "the bottom rung returned {:?} = {} B against a {budget} B budget",
            fitted.size,
            fitted.bytes()
        );

        let asked = 1920.0 / 1080.0;
        let got = f64::from(fitted.size[0]) / f64::from(fitted.size[1]);
        assert!(
            (asked - got).abs() < 0.01,
            "the shrink distorted the aspect ratio: {asked} became {got}"
        );
    }

    /// Whatever the pane, whatever the rung, the result fits the budget.
    ///
    /// The property, rather than the three cases above: this is what makes the
    /// budget constant a bound rather than a suggestion.
    #[test]
    fn no_pane_and_no_rung_can_exceed_the_budget() {
        let budget = 4 * 1024 * 1024;
        for pane in [
            [1, 1],
            [640, 480],
            [1440, 900],
            [2560, 1440],
            [3840, 2160],
            [7680, 4320],
            [16384, 16384],
        ] {
            for rung in ResolutionRung::LADDER {
                let fitted = VolumeQuality {
                    resolution: rung,
                    shading: GradientShading::On,
                }
                .fit(pane, budget);
                assert!(
                    fitted.bytes() <= budget,
                    "{rung:?} on a {pane:?} pane produced {:?} = {} B, over the \
                     {budget} B budget",
                    fitted.size,
                    fitted.bytes()
                );
                assert!(fitted.size[0] >= 1 && fitted.size[1] >= 1);
            }
        }
    }

    /// A fit that did not have to degrade returns the rung it was given.
    #[test]
    fn a_fit_within_budget_leaves_the_quality_alone() {
        let quality = VolumeQuality {
            resolution: ResolutionRung::Half,
            shading: GradientShading::Off,
        };
        let fitted = quality.fit([1440, 900], UNLIMITED);
        assert_eq!(fitted.quality, quality);
    }

    /// Each device class maps to the row the module doc gives it.
    #[test]
    fn every_device_class_selects_the_quality_its_row_documents() {
        for (class, resolution, shading) in [
            (
                DeviceClass::Discrete,
                ResolutionRung::Native,
                GradientShading::On,
            ),
            (
                DeviceClass::Integrated,
                ResolutionRung::Half,
                GradientShading::On,
            ),
            (
                DeviceClass::Virtual,
                ResolutionRung::Half,
                GradientShading::Off,
            ),
            (
                DeviceClass::Unknown,
                ResolutionRung::Half,
                GradientShading::Off,
            ),
            (
                DeviceClass::Software,
                ResolutionRung::Quarter,
                GradientShading::Off,
            ),
        ] {
            assert_eq!(
                select(class, VolumeQuality::BEST),
                VolumeQuality {
                    resolution,
                    shading
                },
                "{class:?} no longer selects what its row documents"
            );
        }
    }

    /// Every `DeviceType` classifies, and no two collapse that must not.
    ///
    /// `Cpu` mapping to anything but `Software` is the one that matters: a
    /// software rasteriser given the discrete GPU's quality is a frame time in
    /// seconds, and a browser falling back to SwiftShader is a real path.
    #[test]
    fn every_adapter_device_type_maps_to_its_own_class() {
        for (device_type, expected) in [
            (wgpu::DeviceType::DiscreteGpu, DeviceClass::Discrete),
            (wgpu::DeviceType::IntegratedGpu, DeviceClass::Integrated),
            (wgpu::DeviceType::VirtualGpu, DeviceClass::Virtual),
            (wgpu::DeviceType::Cpu, DeviceClass::Software),
            (wgpu::DeviceType::Other, DeviceClass::Unknown),
        ] {
            assert_eq!(
                DeviceClass::from_device_type(device_type),
                expected,
                "{device_type:?} no longer classifies as {expected:?}"
            );
        }
    }

    /// The mobile and wasm ceiling holds a discrete GPU down on both rungs.
    ///
    /// This is the arm no test binary on any CI row would otherwise reach: a
    /// phone reports `IntegratedGpu` and a browser reports `Other`, but an
    /// Android tablet with a fast GPU can report `DiscreteGpu`, and without the
    /// ceiling it would select the desktop's full-size shaded march.
    #[test]
    fn the_handheld_ceiling_caps_even_a_discrete_adapter() {
        let handheld = VolumeQuality {
            resolution: ResolutionRung::Half,
            shading: GradientShading::Off,
        };
        assert_eq!(
            select(DeviceClass::Discrete, handheld),
            handheld,
            "a discrete adapter escaped the handheld ceiling"
        );
    }

    /// A ceiling never *raises* a device that had already chosen less.
    ///
    /// The mistake this catches is writing `capped_by` as an assignment rather
    /// than a `max`: a software rasteriser under the desktop ceiling would then
    /// be promoted to the full-size shaded march.
    #[test]
    fn a_ceiling_never_raises_a_device_that_chose_less() {
        assert_eq!(
            select(DeviceClass::Software, VolumeQuality::BEST),
            VolumeQuality::CHEAPEST,
            "the desktop ceiling promoted a software rasteriser"
        );
        assert_eq!(
            select(DeviceClass::Integrated, VolumeQuality::BEST).resolution,
            ResolutionRung::Half,
            "the desktop ceiling promoted an integrated GPU to Native"
        );
    }

    /// The two rungs cap independently of each other.
    ///
    /// Folding them into one ordered "quality level" is the tempting
    /// simplification, and it is wrong: shading is the 2.4x knob and resolution
    /// is the 3.4x one, and a device can want one without the other.
    #[test]
    fn the_two_rungs_are_capped_independently() {
        let shaded_but_small = VolumeQuality {
            resolution: ResolutionRung::Quarter,
            shading: GradientShading::On,
        };
        let large_but_flat = VolumeQuality {
            resolution: ResolutionRung::Native,
            shading: GradientShading::Off,
        };
        assert_eq!(
            shaded_but_small.capped_by(large_but_flat),
            VolumeQuality::CHEAPEST,
            "capping took one rung from each side instead of the cheaper of both"
        );
    }

    /// `is_on` is the bridge into the uniform block's `flags.x`.
    #[test]
    fn the_shading_rung_reports_itself_as_a_flag() {
        assert!(GradientShading::On.is_on());
        assert!(!GradientShading::Off.is_on());
    }

    /// The ladder is ordered finest-first and `next_coarser` walks it.
    ///
    /// `fit` depends on both: it walks with `next_coarser` and the budget tests
    /// index `LADDER`. A `LADDER` in the other order would leave the tests
    /// asserting the same things about the wrong rungs.
    #[test]
    fn the_ladder_runs_finest_to_coarsest_and_next_coarser_walks_it() {
        assert_eq!(
            ResolutionRung::LADDER,
            [
                ResolutionRung::Native,
                ResolutionRung::Half,
                ResolutionRung::Quarter
            ]
        );
        let mut walked = vec![ResolutionRung::LADDER[0]];
        while let Some(next) = walked.last().expect("never empty").next_coarser() {
            walked.push(next);
        }
        assert_eq!(walked, ResolutionRung::LADDER.to_vec());

        let divisors = ResolutionRung::LADDER.map(ResolutionRung::linear_divisor);
        assert_eq!(divisors, [1, 2, 4]);
        assert!(
            divisors.windows(2).all(|pair| pair[0] < pair[1]),
            "the divisors {divisors:?} do not increase down the ladder, so \
             stepping down would not reduce anything and `fit` would loop to \
             the bottom rung without ever getting cheaper"
        );
    }

    /// This target's own ceiling is one of the ladder's points.
    ///
    /// Whichever arm this build compiled is the one checked, exactly as
    /// `constants`' budget tests work. What it catches is a ceiling written as
    /// a quality nothing can select.
    #[test]
    fn this_targets_ceiling_is_a_point_on_both_ladders() {
        assert!(ResolutionRung::LADDER.contains(&PLATFORM_CEILING.resolution));
        assert_eq!(
            select(DeviceClass::Discrete, PLATFORM_CEILING),
            PLATFORM_CEILING,
            "the fastest device this target admits cannot reach its own \
             ceiling, so the ceiling names a quality nothing selects"
        );
    }
}
