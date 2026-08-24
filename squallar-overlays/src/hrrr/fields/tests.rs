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

/// Every scale has stops and spans a finite non-empty range.
///
/// **`is_gradient` moved out of here** when the reflectivity fields landed: it
/// is no longer the same answer for every parameter, so a blanket assertion
/// would have had to be deleted rather than tightened. It is now pinned per
/// parameter by [`the_reflectivity_bars_are_banded_and_the_others_are_not`] and
/// against the ramp itself by
/// [`a_banded_bar_paints_one_flat_colour_across_its_band`].
#[test]
fn every_scale_is_a_finite_non_empty_ramp() {
    for &p in ModelParameter::all() {
        let s = spec(p);
        assert!(
            !s.scale.thresholds.is_empty(),
            "{p:?}'s colour bar has no stops",
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

/// The five reflectivity fields draw **bands**; the two that are not
/// reflectivity draw a wash. A literal table rather than a re-derivation of
/// [`ModelParameter::is_banded`]: the point is to state what each of the seven
/// bars looks like, so that flipping the predicate is a red test and not a
/// silently different picture.
#[test]
fn the_reflectivity_bars_are_banded_and_the_others_are_not() {
    for (param, is_gradient) in [
        (ModelParameter::CompositeReflectivity, false),
        (ModelParameter::Reflectivity1km, false),
        (ModelParameter::Reflectivity4km, false),
        (ModelParameter::ReflectivityM10C, false),
        (ModelParameter::MaxReflectivity, false),
        (ModelParameter::EchoTop, true),
        (ModelParameter::VerticallyIntegratedLiquid, true),
    ] {
        let bar = |g: bool| if g { "a wash" } else { "bands" };
        assert_eq!(
            spec(param).scale.is_gradient,
            is_gradient,
            "{param:?}'s bar draws as {} and should draw as {}",
            bar(spec(param).scale.is_gradient),
            bar(is_gradient),
        );
    }

    // Floor: a pre-existing parameter still declares a wash, so "band every
    // bar" fails here rather than satisfying the table above.
    assert!(
        spec(ModelParameter::SurfaceBasedCape).scale.is_gradient,
        "CAPE's ramp interpolates, so its bar must still be a wash — without \
         this, the table above passes for a build that banded everything",
    );
    // And the count, so "band nothing" is red too.
    let banded = ModelParameter::all()
        .iter()
        .filter(|p| !spec(**p).scale.is_gradient)
        .count();
    assert_eq!(banded, 5, "exactly the five reflectivity fields are banded");
}

/// The declaration and the raster must agree. `is_gradient: false` beside a ramp
/// that interpolates is a legend explaining a picture that is not on screen, and
/// nothing else in the tree compares the two.
#[test]
fn a_banded_bar_paints_one_flat_colour_across_its_band() {
    let p = ModelParameter::CompositeReflectivity;
    assert!(!spec(p).scale.is_gradient, "the premise");

    // Two readings inside the 45-50 dBZ band paint the same colour...
    assert_eq!(p.color_for_value(45.0), p.color_for_value(49.9));
    // ...and the next band is a different colour, so "one colour for
    // everything" is not what the equality above would accept.
    assert_ne!(p.color_for_value(49.9), p.color_for_value(50.0));

    // The control: a wash does move between its stops, so the equality is a
    // property of banding rather than of how close the two probes are.
    let vil = ModelParameter::VerticallyIntegratedLiquid;
    assert!(spec(vil).scale.is_gradient, "the premise");
    assert_ne!(
        vil.color_for_value(5.0),
        vil.color_for_value(9.9),
        "VIL is a wash: two readings inside one interval must differ",
    );
}

/// The forecast composite and the observed mosaic are read side by side, so the
/// same dBZ must be the same colour on both — **and the same colour a radar
/// tilt paints it, through 70 dBZ and no further.**
///
/// **This was `the_two_reflectivity_ladders_agree`, and two was the wrong
/// number.** It held HRRR's copy equal to MRMS's while a third table in
/// `squallar-radar` — the one a radar tilt is drawn through, in the same pane —
/// sat about one 5 dBZ band away through the green-to-red region with nothing
/// comparing it to either. The shared colours are the substrate's
/// `squallar_source::product::REFLECTIVITY_SHARED_STOPS`, which both of these
/// draw from the overlay floor up and which `squallar-radar` draws whole;
/// comparing the two overlay bars against it is what makes the radar bar part
/// of the same claim, from a crate that may not name `squallar-radar`.
///
/// **The agreement is bounded at 70 on purpose, and this test says both
/// halves.** Above 70 radar shows a hail band — 75 sky-blue climbing to 95 —
/// and these two bars cap at 75 white, because a mosaic and a forecast grid do
/// not produce values up there and a bar advertising a range its raster cannot
/// reach is the worse lie. So an accidental **re-convergence** (someone
/// re-capping radar at 75 white, which is exactly what `e6091e47` did) reddens
/// on the divergence assertion below, and an accidental **widening** — a stop
/// drifting anywhere at or below 70, or either overlay bar growing a tail —
/// reddens on the agreement assertion. Neither direction passes.
///
/// The radar end of it is pinned twice more, because this crate cannot see it:
/// `squallar_radar::palette::tests::the_reflectivity_ladder_is_the_substrates_radar_one`
/// from below, and
/// `squallar_egui::ui::map::pane_render::legend_ladder_tests::
/// every_layer_that_draws_dbz_paints_the_same_ladder_through_seventy`
/// from the one crate that can hold all three at once.
#[test]
fn the_three_reflectivity_ladders_agree_through_seventy() {
    use squallar_source::product::{
        REFLECTIVITY_DIVERGENCE_DBZ, REFLECTIVITY_OVERLAY_CAP, REFLECTIVITY_OVERLAY_FLOOR,
        REFLECTIVITY_OVERLAY_STOPS, REFLECTIVITY_RADAR_STOPS, REFLECTIVITY_SHARED_STOPS,
    };

    let hrrr = spec(ModelParameter::CompositeReflectivity).scale;
    let mrms = crate::mrms::fields::spec(crate::mrms::MrmsProduct::ReflectivityComposite).scale;
    let shared: Vec<(f32, [u8; 3])> =
        REFLECTIVITY_SHARED_STOPS[REFLECTIVITY_OVERLAY_FLOOR..].to_vec();

    // ── the agreement: every stop at or below 70 dBZ, on all three ──
    for (layer, ladder) in [("HRRR", &hrrr.thresholds), ("MRMS", &mrms.thresholds)] {
        let through_seventy: Vec<(f32, [u8; 3])> = ladder
            .iter()
            .copied()
            .filter(|&(dbz, _)| dbz < REFLECTIVITY_DIVERGENCE_DBZ)
            .collect();
        assert_eq!(
            through_seventy, shared,
            "{layer}'s dBZ ladder is no longer the shared table from the \
             overlay floor through 70",
        );
    }
    assert_eq!(
        hrrr.thresholds, mrms.thresholds,
        "the two overlay dBZ ladders have drifted apart",
    );
    let radar_through_seventy: Vec<(f32, [u8; 3])> = REFLECTIVITY_RADAR_STOPS
        .iter()
        .copied()
        .filter(|&(dbz, _)| dbz < REFLECTIVITY_DIVERGENCE_DBZ)
        .skip(REFLECTIVITY_OVERLAY_FLOOR)
        .collect();
    assert_eq!(
        radar_through_seventy, shared,
        "the ladder a radar tilt draws is no longer the shared table through \
         70 dBZ, which is the drift this whole arrangement exists to stop",
    );
    assert!(
        shared.len() >= 10,
        "a shared core of {} stops is too short for this comparison to mean \
         much",
        shared.len(),
    );
    assert_eq!(
        hrrr.is_gradient, mrms.is_gradient,
        "one dBZ bar draws bands and the other a wash",
    );

    // ── the divergence: 75 dBZ, two colours, deliberately ──
    let overlay_top = *REFLECTIVITY_OVERLAY_STOPS
        .last()
        .expect("the overlay ladder is non-empty");
    assert_eq!(
        overlay_top, REFLECTIVITY_OVERLAY_CAP,
        "the overlay bars must end at their own cap",
    );
    assert_eq!(
        overlay_top.0, REFLECTIVITY_DIVERGENCE_DBZ,
        "the overlay bars must stop at the dBZ the layers part at, not climb \
         past it into a range no mosaic or forecast grid produces",
    );
    let radar_at_divergence = REFLECTIVITY_RADAR_STOPS
        .iter()
        .find(|&&(dbz, _)| dbz == REFLECTIVITY_DIVERGENCE_DBZ)
        .copied()
        .expect("radar's ladder has a stop at the divergence");
    assert_ne!(
        radar_at_divergence.1, overlay_top.1,
        "the ladders have re-converged at {REFLECTIVITY_DIVERGENCE_DBZ} dBZ. A \
         tilt shows the hail band there (sky-blue, climbing to 95) and these \
         two bars cap white; making them equal again is what silently painted \
         every hail core one flat colour in `e6091e47`.",
    );
    assert!(
        REFLECTIVITY_RADAR_STOPS
            .iter()
            .any(|&(dbz, _)| dbz > REFLECTIVITY_DIVERGENCE_DBZ),
        "radar's ladder must keep climbing above the divergence, or the \
         divergence is a repaint of one stop rather than a band",
    );
    for (layer, ladder) in [("HRRR", &hrrr.thresholds), ("MRMS", &mrms.thresholds)] {
        assert!(
            !ladder
                .iter()
                .any(|&(dbz, _)| dbz > REFLECTIVITY_DIVERGENCE_DBZ),
            "{layer}'s bar has grown a stop above {REFLECTIVITY_DIVERGENCE_DBZ} \
             dBZ. The divergence is meant to be one-sided: radar has the hail \
             band because a tilt reaches it, and these grids do not.",
        );
    }

    // The control: the overlay ladders are genuinely shorter than radar's, so a
    // slicer that forgot the floor or the cap cannot pass by accident.
    assert_ne!(
        REFLECTIVITY_OVERLAY_STOPS.len(),
        REFLECTIVITY_RADAR_STOPS.len(),
        "the overlay ladder must be shorter than the one radar draws",
    );
}
