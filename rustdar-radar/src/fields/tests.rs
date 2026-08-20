//! The registration is self-verifying: every claim [`super::FIELDS`] makes about
//! a product is checked here against the predicate that already answers it, so
//! a new product cannot be registered with a fact nobody computed.

use super::*;
use crate::derive::volume_slot;
use crate::sampler::samplable;

/// The products whose field exists at individual tilts.
///
/// The six native moments, the three per-sweep derivations, and the hydrometeor
/// classification — [`crate::hca`] computes the RPG's per-tilt product 165 from
/// one tilt's dual-pol moments, and [`crate::hhc`] is what composites it. The
/// remainder are column integrals and whole-volume composites with no per-tilt
/// rendition at all.
const TILTED: &[RadarProduct] = &[
    RadarProduct::Reflectivity,
    RadarProduct::Velocity,
    RadarProduct::SpectrumWidth,
    RadarProduct::DifferentialPhase,
    RadarProduct::CorrelationCoefficient,
    RadarProduct::DifferentialReflectivity,
    RadarProduct::StormRelativeVelocity,
    RadarProduct::SpecificDifferentialPhase,
    RadarProduct::HydrometeorClassification,
    RadarProduct::NormalizedRotation,
];

/// `spec` indexes by discriminant, so the list it is built from must be in
/// discriminant order. `palette::legend_scale_static` makes the same
/// assumption; this is the one place it is checked.
#[test]
fn the_all_list_is_in_discriminant_order() {
    assert!(
        !RadarProduct::all().is_empty(),
        "there are products to check"
    );
    for (i, &p) in RadarProduct::all().iter().enumerate() {
        assert_eq!(
            p as usize, i,
            "{p:?} sits at index {i} of `all()` but its discriminant is {}; \
             every by-discriminant index into a table built from `all()` — \
             `fields::spec` and `palette::legend_scale_static` — would read \
             another product's row",
            p as usize,
        );
    }
}

/// **The zero-migration proof.** A `FieldId` is persisted as its bare string,
/// and the strings this crate registers are the product enum's own `Serialize`
/// output — so a config file written before `FieldId` existed loads unchanged,
/// and one written after loads on a build from before.
#[test]
fn the_field_id_is_the_products_own_serde_spelling() {
    for &p in RadarProduct::all() {
        let serde_form = serde_json::to_string(&p).expect("a fieldless enum serializes");
        let bare = serde_form
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or_else(|| panic!("{p:?} serialized as {serde_form}, not a JSON string"));
        assert_eq!(
            spec(p).id.as_str(),
            bare,
            "{p:?}'s FieldId is {:?} but the enum serializes as {bare:?}. These \
             two spellings are the same bytes in the same config files: a \
             difference is a silent migration nobody wrote.",
            spec(p).id.as_str(),
        );
    }
}

/// Every product is registered exactly once, and no two share an id.
#[test]
fn every_product_has_exactly_one_entry_with_a_unique_id() {
    assert_eq!(
        products().len(),
        RadarProduct::all().len(),
        "the projection and the enum disagree on how many products there are",
    );
    let mut ids: Vec<&str> = products().iter().map(|s| s.id.as_str()).collect();
    ids.sort_unstable();
    let before = ids.len();
    ids.dedup();
    assert_eq!(
        ids.len(),
        before,
        "two radar products registered the same FieldId; one would shadow the \
         other's saved curves, thresholds and preset panes",
    );
    for &p in RadarProduct::all() {
        assert_eq!(
            spec(p).name,
            p.name(),
            "{p:?}'s projected name drifted from its registration",
        );
        assert_eq!(spec(p).code, p.code(), "{p:?}'s projected code drifted");
        assert_eq!(
            spec(p).group,
            GROUP,
            "{p:?} is filed outside the radar group"
        );
    }
}

/// `vertical` is the 3D editor's gate, so it must be exactly the predicate the
/// 3D pipeline itself asks.
#[test]
fn vertical_agrees_with_volume_slot() {
    let mut n = 0;
    for &p in RadarProduct::all() {
        assert_eq!(
            spec(p).vertical,
            volume_slot(p).is_some(),
            "{p:?} declares vertical={} but `derive::volume_slot` says {:?}. \
             The editors gate on this flag: a disagreement is either a 3D \
             editor offered for a field with no vertical extent, or one \
             refused for a field that has it.",
            spec(p).vertical,
            volume_slot(p),
        );
        n += usize::from(spec(p).vertical);
    }
    // Non-triviality floor: the flag actually separates the products.
    assert_eq!(
        n, 9,
        "nine products render in 3D (six native moments plus SRV, NROT, KDP); \
         a different count means the vertical set moved and this pin was not \
         re-cut",
    );
}

/// `tilted` is the per-tilt-field claim, and it is NOT `vertical` — the hybrid
/// classification is the pair that separates them.
#[test]
fn tilted_is_the_per_tilt_field_set() {
    for &p in RadarProduct::all() {
        assert_eq!(
            spec(p).tilted,
            TILTED.contains(&p),
            "{p:?} declares tilted={} against this test's stated set",
            spec(p).tilted,
        );
    }
    // The two flags are different questions, and this is where that is shown
    // rather than asserted: HCA has a per-tilt field and no vertical extent.
    assert!(
        spec(RadarProduct::HydrometeorClassification).tilted,
        "HCA's per-tilt classification is `crate::hca`'s product 165",
    );
    assert!(
        !spec(RadarProduct::HydrometeorClassification).vertical,
        "HCA's rendered product is the hybrid composite, one surface",
    );
    // Every samplable moment and every derivation is tilted; otherwise the set
    // above could drift into an arbitrary literal.
    for &p in RadarProduct::all() {
        if samplable(p).is_some() || volume_slot(p).is_some() {
            assert!(
                spec(p).tilted,
                "{p:?} is sampled or derived per sweep, so its field exists per tilt",
            );
        }
    }
}

/// The threshold prefix is the isosurface shape's own answer, never a literal
/// kept in step by hand.
#[test]
fn the_domain_prefix_agrees_with_the_iso_shape() {
    let mut shapes = std::collections::HashSet::new();
    for &p in RadarProduct::all() {
        let expected = match crate::voxel::iso_shape(p) {
            IsoShape::Sequential => "\u{2265}",
            IsoShape::DeviationFrom { .. } => "|\u{b1}| \u{2265}",
            IsoShape::AtOrBelow => "\u{2264}",
        };
        assert_eq!(
            spec(p).domain_label_ends.0,
            expected,
            "{p:?}'s threshold prefix disagrees with its isosurface shape — the \
             slider would read as a bound it does not apply",
        );
        shapes.insert(expected);
    }
    // Non-triviality floor: all three shapes are actually exercised, so the
    // comparison cannot be green because every product shares one prefix.
    assert_eq!(
        shapes.len(),
        3,
        "the products cover only {} of the three isosurface shapes, so this \
         check no longer distinguishes them: {shapes:?}",
        shapes.len(),
    );
}

/// A product with a 3D editor states its own slider travel; one without borrows
/// its colour scale's span. Either way the domain is finite and non-empty —
/// there is no `0.0..=1.0` wildcard left to fabricate one.
#[test]
fn every_value_domain_is_finite_non_empty_and_stated_where_it_is_used() {
    for &p in RadarProduct::all() {
        let s = spec(p);
        let (lo, hi) = s.value_domain;
        assert!(
            lo.is_finite() && hi.is_finite(),
            "{p:?}'s value domain is {lo}..={hi}, which a slider cannot travel",
        );
        assert!(lo < hi, "{p:?}'s value domain is empty ({lo}..={hi})",);
        assert_eq!(
            crate::product_spec::spec(p).value_domain.is_some(),
            s.vertical,
            "{p:?} states a slider travel iff it has a 3D editor: stated={:?}, \
             vertical={}. A stated travel for a refused field is a slider \
             nobody draws; a missing one for an offered field is the wildcard \
             that WO-E9a deleted.",
            crate::product_spec::spec(p).value_domain,
            s.vertical,
        );
        if !s.vertical {
            assert_eq!(
                (lo, hi),
                (s.scale.min_value, s.scale.max_value),
                "{p:?} has no slider, so its domain is its scale's own span",
            );
        }
    }
}

/// The scale is the palette's built-once object, borrowed — not a table rebuilt
/// from values. `get_color_for_value` stays the only colouring authority.
#[test]
fn the_scale_is_the_palettes_own_borrowed_object() {
    for &p in RadarProduct::all() {
        let projected: *const _ = spec(p).scale;
        let palette: *const _ = crate::palette::legend_scale_static(p);
        assert!(
            std::ptr::eq(projected, palette),
            "{p:?}'s projected scale is a different object from the palette's \
             own: a copy is a colour table that can drift from the renderer's",
        );
        assert!(
            !spec(p).scale.thresholds.is_empty(),
            "{p:?}'s scale has no stops, so the check above compares two empty \
             tables and cannot fail",
        );
    }
}

/// A `FieldId` this build does not register resolves to nothing rather than to
/// a neighbouring product — the open-id doctrine, at the radar boundary.
#[test]
fn an_unregistered_field_id_names_no_product() {
    for &p in RadarProduct::all() {
        assert_eq!(
            product_for(&spec(p).id),
            Some(p),
            "{p:?}'s own id must resolve back to it",
        );
    }
    assert_eq!(
        product_for(&FieldId::new("NotAProductThisBuildHas")),
        None,
        "an unknown id must not resolve to a product",
    );
    // Presence control: the lookup can succeed at all.
    assert!(
        product_for(&FieldId::from_static("Reflectivity")).is_some(),
        "the lookup itself is broken if even a known id misses",
    );
}

/// Every `known::` const names a field this crate actually registers.
///
/// The open string has no compiler to catch a typo, and these consts are what
/// the UI spells instead of the product enum — so a drifted one would be a
/// selection that silently resolves to nothing rather than a build error.
#[test]
fn every_known_field_is_registered() {
    assert_eq!(
        known::ALL.len(),
        RadarProduct::all().len(),
        "the `known::` list and the registration disagree in size",
    );
    for id in &known::ALL {
        assert!(
            product_for(id).is_some(),
            "known::{} names {:?}, which this crate does not register",
            id.as_str(),
            id.as_str(),
        );
    }
    // Every registered field is reachable as a const, not merely the reverse:
    // a product added without its const would otherwise pass silently.
    for &p in RadarProduct::all() {
        assert!(
            known::ALL.contains(&spec(p).id),
            "{p:?} is registered but has no `known::` const, so the UI has no \
             spelling for it that does not name the enum",
        );
    }
    // Non-triviality floor: the consts are distinct.
    // Bound to a local first: `known::ALL` is a `const`, so referring to it
    // directly creates a temporary that dies at the end of the statement.
    let all = known::ALL;
    let mut ids: Vec<&str> = all.iter().map(|i| i.as_str()).collect();
    ids.sort_unstable();
    let before = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), before, "two `known::` consts share a spelling");
}
