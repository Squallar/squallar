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
            GradientShading::Off,
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

/// Lighting degrades before resolution: no class keeps the cloud rung
/// after giving up pixels.
#[test]
fn no_class_that_gave_up_resolution_keeps_the_cloud_rung() {
    for class in [
        DeviceClass::Discrete,
        DeviceClass::Integrated,
        DeviceClass::Virtual,
        DeviceClass::Software,
        DeviceClass::Unknown,
    ] {
        let chosen = class.unconstrained_quality();
        if chosen.resolution != ResolutionRung::Native {
            assert_eq!(
                chosen.shading,
                GradientShading::Off,
                "{class:?} coarsened its offscreen and kept the cloud rung; \
                     the ladder surrenders lighting before resolution",
            );
        }
    }
}

/// All three platform ceilings, checked from whichever target compiles this.
#[test]
fn all_three_platform_ceilings_are_the_ones_documented() {
    let handheld = VolumeQuality {
        resolution: ResolutionRung::Half,
        shading: GradientShading::Off,
    };
    assert_eq!(WASM_PLATFORM_CEILING, handheld);
    assert_eq!(MOBILE_PLATFORM_CEILING, handheld);
    assert_eq!(DESKTOP_PLATFORM_CEILING, VolumeQuality::BEST);
}

/// Both handheld ceilings really cap, and the desktop one really does not.
#[test]
fn the_handheld_ceilings_cap_a_discrete_adapter_and_the_desktop_one_does_not() {
    for (target, ceiling) in [
        ("wasm", WASM_PLATFORM_CEILING),
        ("mobile", MOBILE_PLATFORM_CEILING),
    ] {
        assert_ne!(
            ceiling,
            VolumeQuality::BEST,
            "the {target} ceiling is the best quality this build offers, so \
                 it caps nothing"
        );
        let chosen = select(DeviceClass::Discrete, ceiling);
        assert_eq!(
            chosen, ceiling,
            "a discrete adapter escaped the {target} ceiling"
        );
        assert_ne!(
            chosen,
            VolumeQuality::BEST,
            "a discrete adapter on {target} still selects the desktop's \
                 full-size shaded march"
        );
    }

    assert_eq!(
        select(DeviceClass::Discrete, DESKTOP_PLATFORM_CEILING),
        VolumeQuality::BEST,
        "the desktop ceiling holds a discrete GPU below what the measured \
             table says it can do"
    );
}

/// Every device class is held to every ceiling, on both rungs.
#[test]
fn no_device_class_escapes_any_platform_ceiling() {
    for (target, ceiling) in [
        ("wasm", WASM_PLATFORM_CEILING),
        ("mobile", MOBILE_PLATFORM_CEILING),
        ("desktop", DESKTOP_PLATFORM_CEILING),
    ] {
        for class in [
            DeviceClass::Discrete,
            DeviceClass::Integrated,
            DeviceClass::Virtual,
            DeviceClass::Software,
            DeviceClass::Unknown,
        ] {
            let chosen = select(class, ceiling);
            assert!(
                chosen.resolution >= ceiling.resolution,
                "{class:?} selects {:?} on {target}, finer than the \
                     {:?} ceiling",
                chosen.resolution,
                ceiling.resolution
            );
            assert!(
                chosen.shading >= ceiling.shading,
                "{class:?} selects {:?} shading on {target}, richer than \
                     the {:?} ceiling",
                chosen.shading,
                ceiling.shading
            );
        }
    }
}

/// A ceiling never *raises* a device that had already chosen less.
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

/// What a `cfg` cascade's arm is defined as, read out of the source.
fn cascade_arm(source: &str, cfg: &str, name: &str) -> String {
    let definition = format!("#[cfg({cfg})]\npub const {name}");
    let occurrences = source.matches(&definition).count();
    assert_eq!(
        occurrences, 1,
        "expected exactly one `{name}` definition under `#[cfg({cfg})]`, \
             found {occurrences}"
    );
    let at = source.find(&definition).expect("just counted one");
    let (declaration, _) = source[at + definition.len()..]
        .split_once(';')
        .expect("a const definition with no semicolon");
    declaration
        .split_once('=')
        .expect("a const definition with no initialiser")
        .1
        .trim()
        .to_owned()
}

/// The three `cfg` arms, in the order the cascades write them.
const CASCADE_ARMS: [(&str, &str); 3] = [
    (r#"target_arch = "wasm32""#, "WASM"),
    (r#"all(not(target_arch = "wasm32"), mobile)"#, "MOBILE"),
    (
        r#"all(not(target_arch = "wasm32"), not(mobile))"#,
        "DESKTOP",
    ),
];

/// Each ceiling arm selects **its own** class's constant.
#[test]
fn each_ceiling_arm_selects_its_own_classs_constant() {
    let source = include_str!("../quality.rs");
    for (cfg, class) in CASCADE_ARMS {
        let expected = format!("{class}_PLATFORM_CEILING");
        assert_eq!(
            cascade_arm(source, cfg, "PLATFORM_CEILING"),
            expected,
            "the `#[cfg({cfg})]` arm of PLATFORM_CEILING does not select \
                 `{expected}`. A cascade arm pointing at another class's \
                 constant compiles, passes every host test, and passes the \
                 wasm `--all-targets` check — it is only visible here."
        );
    }
}

/// The compiled cascade selects one of the three named ceilings.
#[test]
fn the_compiled_cascade_selects_one_of_the_named_ceilings() {
    assert!(
        [
            WASM_PLATFORM_CEILING,
            MOBILE_PLATFORM_CEILING,
            DESKTOP_PLATFORM_CEILING,
        ]
        .contains(&PLATFORM_CEILING),
        "PLATFORM_CEILING is {PLATFORM_CEILING:?}, which is none of the \
             three named arms — so the cascade has grown a literal of its own, \
             and the unconditional tests above no longer describe it"
    );
}

/// `select(Discrete, ceiling)` returns the ceiling for *every* ceiling.
#[test]
fn a_discrete_adapter_reaches_whatever_ceiling_it_is_given() {
    assert_eq!(
        DeviceClass::Discrete.unconstrained_quality(),
        VolumeQuality::BEST,
        "Discrete no longer sits at the top of both ladders"
    );
    for ceiling in [
        VolumeQuality::BEST,
        VolumeQuality::CHEAPEST,
        WASM_PLATFORM_CEILING,
        VolumeQuality {
            resolution: ResolutionRung::Native,
            shading: GradientShading::Off,
        },
        VolumeQuality {
            resolution: ResolutionRung::Quarter,
            shading: GradientShading::On,
        },
    ] {
        assert_eq!(
            select(DeviceClass::Discrete, ceiling),
            ceiling,
            "a discrete adapter did not reach the {ceiling:?} ceiling"
        );
    }
}
