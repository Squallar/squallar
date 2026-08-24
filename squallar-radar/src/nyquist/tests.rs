use super::*;

#[test]
fn the_first_statement_of_a_cut_wins() {
    let mut table = DeclaredNyquist::empty();
    table.declare(3, 26.42);
    table.declare(3, 8.0);
    assert_eq!(table.get(3), Some(26.42));
}

#[test]
fn a_second_disagreeing_declaration_is_recorded_against_its_cut() {
    let mut table = DeclaredNyquist::empty();
    table.declare(1, 26.42);
    table.declare(2, 22.14);
    table.declare(1, 31.0);
    assert_eq!(
        table.get(1),
        Some(26.42),
        "the resolution policy is unchanged: the first statement still stands",
    );
    assert_eq!(table.contradicted().collect::<Vec<_>>(), vec![1]);
}

#[test]
fn a_repeated_agreeing_declaration_is_not_a_contradiction() {
    let mut table = DeclaredNyquist::empty();
    for _ in 0..360 {
        table.declare(4, 22.14);
    }
    assert_eq!(table.contradicted().count(), 0, "{table:?}");
}

#[test]
fn a_merge_carries_the_contradictions_of_both_volumes() {
    let mut base = DeclaredNyquist::empty();
    base.declare(1, 26.42);
    base.declare(1, 31.0);
    let mut overlay = DeclaredNyquist::empty();
    overlay.declare(2, 22.14);
    overlay.declare(2, 25.0);
    base.overlay(&overlay);
    assert_eq!(base.contradicted().collect::<Vec<_>>(), vec![1, 2]);
}

#[test]
fn a_non_finite_declaration_is_refused_rather_than_stored() {
    let mut table = DeclaredNyquist::empty();
    table.declare(1, f64::NAN);
    table.declare(2, f64::INFINITY);
    assert!(table.is_empty(), "{table:?}");
}

/// A zero leaves its cut unnamed: across 22 TDWR volumes from 10 sites over
/// three days, every cut of every volume declares `nyquist_velocity = 0`.
/// Unnamed rather than stored, because `Vny = λ·PRF/4` and no cut that
/// emitted a radial was flown at no PRF: zero is the wire spelling *not
/// populated*.
#[test]
fn a_zero_declaration_leaves_its_cut_unnamed() {
    let mut table = DeclaredNyquist::empty();
    for elevation_number in 1..=16 {
        table.declare(elevation_number, 0.0);
    }
    assert!(
        table.is_empty(),
        "a volume that declared zero on every cut must name none of them: {table:?}",
    );
    assert_eq!(table.get(1), None);
    assert_eq!(
        table.contradicted().count(),
        0,
        "a zero is an absence, not two cuts disagreeing under one key",
    );

    let mut mixed = DeclaredNyquist::empty();
    mixed.declare(2, 23.84);
    mixed.declare(2, 0.0);
    assert_eq!(mixed.get(2), Some(23.84));
    assert_eq!(mixed.contradicted().count(), 0, "{mixed:?}");
}

#[test]
fn a_negative_declaration_is_refused_at_every_door() {
    let collected: DeclaredNyquist = [(1, -23.84), (2, 23.84)].into_iter().collect();
    assert_eq!(collected.get(1), None);
    assert_eq!(collected.get(2), Some(23.84));

    let mut base: DeclaredNyquist = [(3, 23.84)].into_iter().collect();
    base.set(3, -1.0);
    base.set(4, 0.0);
    assert_eq!(
        base.get(3),
        Some(23.84),
        "the merge door must not erase a real declaration with a non-speed",
    );
    assert_eq!(base.get(4), None);
}

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
