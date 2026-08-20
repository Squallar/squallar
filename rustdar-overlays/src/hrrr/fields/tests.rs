//! The model registration checks itself against the accessors it projects.

use super::*;

/// Every parameter is registered exactly once, under its persisted spelling.
#[test]
fn every_parameter_has_one_entry_keyed_by_its_persisted_spelling() {
    assert_eq!(
        products().len(),
        ModelParameter::all().len(),
        "the projection and the parameter list disagree in size",
    );
    assert!(!products().is_empty(), "there are parameters to check");
    let mut ids: Vec<&str> = products().iter().map(|s| s.id.as_str()).collect();
    ids.sort_unstable();
    let before = ids.len();
    ids.dedup();
    assert_eq!(
        ids.len(),
        before,
        "two parameters registered the same FieldId; one would shadow the \
         other's saved pane selection",
    );
    for &p in ModelParameter::all() {
        assert_eq!(
            spec(p).id.as_str(),
            p.as_str(),
            "{p:?}'s FieldId must be the string `serialize_state` already \
             writes, or every saved pane selection silently stops resolving",
        );
        assert_eq!(spec(p).name, p.display_name(), "{p:?}'s name drifted");
        assert_eq!(
            spec(p).group,
            GROUP,
            "{p:?} is filed outside the model group"
        );
    }
}

/// No model parameter has vertical extent or a per-tilt field: they are 2 D
/// forecast grids. The 3D editors gate on `vertical`, so this is what refuses
/// them.
#[test]
fn no_model_parameter_is_vertical_or_tilted() {
    for &p in ModelParameter::all() {
        assert!(!spec(p).vertical, "{p:?} is a 2 D forecast grid");
        assert!(!spec(p).tilted, "{p:?} has no tilts");
    }
}

/// Every scale has stops, spans a finite non-empty range, and is a ramp.
#[test]
fn every_scale_is_a_finite_non_empty_ramp() {
    for &p in ModelParameter::all() {
        let s = spec(p);
        assert!(
            !s.scale.thresholds.is_empty(),
            "{p:?}'s colour bar has no stops",
        );
        assert!(
            s.scale.is_gradient,
            "{p:?} declares banded colour, but `color_for_value` interpolates",
        );
        let (lo, hi) = s.value_domain;
        assert!(
            lo.is_finite() && hi.is_finite(),
            "{p:?}'s domain is {lo}..={hi}",
        );
        assert!(lo < hi, "{p:?}'s domain is empty ({lo}..={hi})");
        assert_eq!(
            (lo, hi),
            (s.scale.min_value, s.scale.max_value),
            "{p:?} has no slider, so its domain is its scale's own span",
        );
    }
}

/// The stops are ascending, which is what makes `min_value`/`max_value` the
/// ends rather than two arbitrary entries.
#[test]
fn every_scales_stops_ascend() {
    for &p in ModelParameter::all() {
        let stops = &spec(p).scale.thresholds;
        assert!(
            stops.len() >= 2,
            "{p:?} has {} stop(s); a one-stop bar makes the ordering check \
             below vacuous",
            stops.len(),
        );
        for w in stops.windows(2) {
            assert!(
                w[0].0 < w[1].0,
                "{p:?}'s stops are not ascending: {} then {}",
                w[0].0,
                w[1].0,
            );
        }
    }
}
