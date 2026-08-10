use super::*;

/// Disabled means "use the vector the RPG published", not "use zero".
#[test]
fn a_disabled_override_yields_no_sample() {
    let o = StormMotionOverride::default();
    assert!(!o.enabled, "the RPG's own SCIT average is the default");
    assert!(o.sample().is_none());
    let on = StormMotionOverride { enabled: true, ..o };
    let s = on.sample().expect("enabled");
    assert_eq!(s.motion.speed_kt, o.speed_kt);
    assert_eq!(s.motion.direction_deg, o.direction_deg);
    assert!(!s.motion.is_scit_average, "a typed vector is not the RPG's");
}

/// `DragValue` parses "nan" and "inf", and `f32::clamp` propagates NaN.
/// A NaN reaching the dispatcher renders an all-NaN field *and*, because
/// `NaN != NaN`, makes its change detector fire every frame — an unbounded
/// re-render of every storm-relative pane that never settles.
#[test]
fn a_non_finite_override_is_refused_rather_than_propagated() {
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let speed = StormMotionOverride {
            enabled: true,
            speed_kt: bad,
            direction_deg: 240.0,
        };
        assert!(speed.sample().is_none(), "speed {bad}");
        let dir = StormMotionOverride {
            enabled: true,
            speed_kt: 30.0,
            direction_deg: bad,
        };
        assert!(dir.sample().is_none(), "direction {bad}");
    }
    // The counterweight: ordinary values still pass, so "reject everything"
    // is not how the test above is satisfied.
    let ok = StormMotionOverride {
        enabled: true,
        speed_kt: 0.0,
        direction_deg: 0.0,
    };
    assert!(ok.sample().is_some(), "zero is a legitimate vector");
}

/// Two equal overrides must produce equal samples, or the dispatcher's
/// change detector re-renders every frame even without a NaN.
#[test]
fn equal_overrides_produce_equal_samples() {
    let a = StormMotionOverride {
        enabled: true,
        speed_kt: 31.5,
        direction_deg: 287.5,
    };
    let b = a;
    assert_eq!(a.sample(), b.sample());
    let c = StormMotionOverride {
        speed_kt: 31.6,
        ..a
    };
    assert_ne!(a.sample(), c.sample());
}

/// The widget's ceiling is the one `DERIVED_OFFSET` was sized against. If
/// this drifts upward, the worst-case derived value starts clamping and
/// paints as data at the clamp instead of at its real magnitude.
#[test]
fn the_speed_ceiling_is_the_one_the_encoding_was_sized_for() {
    assert_eq!(rustdar_radar::srm::MAX_OVERRIDE_SPEED_KT, 200.0);
}
