use super::*;

/// A number that arrives twice — once per radial of the same cut — must not
/// depend on which radial the walk reached last. `declare` is first-wins, and
/// this is the property that makes a partial walk of a sweep as good as a
/// whole one.
#[test]
fn the_first_statement_of_a_cut_wins() {
    let mut table = DeclaredNyquist::empty();
    table.declare(3, 26.42);
    table.declare(3, 8.0);
    assert_eq!(table.get(3), Some(26.42));
}

/// A non-finite declaration is dropped rather than stored. Stored, it would
/// reach the guard as a comparison that is false in both directions — a guard
/// that is off while looking armed — which is strictly worse than the absence
/// that makes the sampler estimate.
#[test]
fn a_non_finite_declaration_is_refused_rather_than_stored() {
    let mut table = DeclaredNyquist::empty();
    table.declare(1, f64::NAN);
    table.declare(2, f64::INFINITY);
    assert!(table.is_empty(), "{table:?}");
}

/// The merge direction the current merged volume needs: the in-flight
/// overlay's statement replaces the complete base's for a cut it has resealed,
/// and cuts it has not reached keep the base's.
#[test]
fn an_overlay_replaces_only_the_cuts_it_names() {
    let mut base: DeclaredNyquist = [(1, 11.0), (2, 12.0), (3, 13.0)].into_iter().collect();
    let overlay: DeclaredNyquist = [(2, 22.0)].into_iter().collect();
    base.overlay(&overlay);
    assert_eq!(base.get(1), Some(11.0));
    assert_eq!(base.get(2), Some(22.0), "the resealed cut did not update");
    assert_eq!(base.get(3), Some(13.0));
    assert_eq!(base.len(), 3);
}

/// A bare `Scan` converts into a volume that declares nothing, which is what
/// keeps every caller holding only model types from having to spell an empty
/// table at the call site.
#[test]
fn a_bare_scan_becomes_a_volume_that_declares_nothing() {
    let scan = Scan::new(
        nexrad_model::data::VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            nexrad_model::data::PulseWidth::Short,
            false,
            0,
            false,
            0,
            false,
            false,
            0,
            false,
            false,
            Vec::new(),
        ),
        Vec::new(),
    );
    let volume: Volume<'_> = (&scan).into();
    assert!(volume.declared_nyquist().is_empty());
    assert!(volume.declared_nyquist().get(1).is_none());
}
