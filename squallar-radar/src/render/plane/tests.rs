//! What the polar producer refuses, and what it keeps.

use super::*;
use crate::render::codes::{Lut, PAINTABLE_CODES, r8_fidelity};
use crate::render::{moment_value_at, painted_moment_value};
use crate::types::RadarProduct;
use nexrad_model::data::{MomentData, Radial, RadialStatus};

const FIRST_GATE_M: u16 = 2125;
const GATE_M: u16 = 250;
const REFL_SCALE: f32 = 2.0;
const REFL_OFFSET: f32 = 66.0;

/// A moment block of `bytes`, at the width and decode the caller names.
fn moment(bytes: Vec<u8>, word_bits: u8, scale: f32, offset: f32) -> MomentData {
    let gates = (bytes.len() / usize::from(word_bits / 8)) as u16;
    MomentData::from_fixed_point(gates, FIRST_GATE_M, GATE_M, word_bits, scale, offset, bytes)
}

/// One radial carrying `refl` as its reflectivity moment and nothing else.
fn radial_with(azimuth_deg: f32, spacing_deg: f32, refl: Option<MomentData>) -> Radial {
    Radial::new(
        0,
        0,
        azimuth_deg,
        spacing_deg,
        RadialStatus::IntermediateRadialData,
        1,
        0.5,
        refl,
        None,
        None,
        None,
        None,
        None,
        None,
    )
}

/// A sweep of `n_radials` evenly spaced radials, each carrying `gates` codes
/// produced by `code`.
fn sweep(n_radials: usize, gates: usize, code: impl Fn(usize, usize) -> u8) -> Vec<Radial> {
    let spacing = 360.0 / n_radials as f32;
    (0..n_radials)
        .map(|r| {
            let bytes: Vec<u8> = (0..gates).map(|g| code(r, g)).collect();
            radial_with(
                r as f32 * spacing,
                spacing,
                Some(moment(bytes, 8, REFL_SCALE, REFL_OFFSET)),
            )
        })
        .collect()
}

/// A plain sweep whose codes ascend along each radial, so no two gates of one
/// radial carry the same one.
fn ascending_sweep(n_radials: usize, gates: usize) -> Vec<Radial> {
    sweep(n_radials, gates, |r, g| ((r + g) % 254 + 2) as u8)
}

// ── What the plane carries ──────────────────────────────────────────────────

/// **The plane's bytes are the wire's bytes**, gate for gate.
///
/// The claim the whole representation rests on: nothing between the moment
/// block and the plane decodes a gate, so nothing can round one.
#[test]
fn the_plane_holds_the_codes_the_wire_carried() {
    let radials = ascending_sweep(9, 40);
    let built = sweep_code_plane(&radials, RadarProduct::Reflectivity, 40)
        .expect("an eight-bit reflectivity sweep admits a plane");
    for (r, radial) in radials.iter().enumerate() {
        let moment = RadarProduct::Reflectivity
            .get_moment(radial)
            .expect("the fixture gave every radial one");
        for (g, &raw) in moment.raw_values().iter().enumerate() {
            assert_eq!(
                built.plane.code_at(0, r, g),
                Some(raw),
                "gate ({r}, {g}) is not the byte the moment block holds",
            );
        }
    }
}

/// **Reading a code back through the table paints what the raster paints**,
/// byte for byte, over every gate of the sweep.
///
/// Not a tolerance and not a sample: the raster's own pair —
/// `moment_value_at` then `painted_moment_value` then `get_color_for_value` —
/// against the plane's code through `Lut`. An unpainted gate is `(0, 0, 0, 0)`
/// on both sides, which is what the raster leaves a pixel nothing claimed.
#[test]
fn the_plane_paints_what_the_raster_paints() {
    let radials = ascending_sweep(7, 60);
    let product = RadarProduct::Reflectivity;
    let built = sweep_code_plane(&radials, product, 60).expect("the sweep admits a plane");
    let lut = Lut::build(built.plane.key());
    for (r, radial) in radials.iter().enumerate() {
        let moment = product.get_moment(radial).expect("every radial has one");
        for g in 0..moment.raw_values().len() {
            let raster = moment_value_at(moment, g)
                .and_then(painted_moment_value)
                .map(|v| {
                    if v.to_bits() == crate::render::RANGE_FOLDED_SENTINEL.to_bits() {
                        crate::palette::RANGE_FOLDED
                    } else {
                        crate::palette::get_color_for_value(product, v)
                    }
                })
                .unwrap_or((0, 0, 0, 0));
            let code = built.plane.code_at(0, r, g).expect("inside the shape");
            assert_eq!(
                lut.entry(code),
                raster,
                "gate ({r}, {g}) paints differently through the plane than through the raster",
            );
        }
    }
}

/// A radial shorter than the sweep's stride is padded with the code the raster
/// leaves a pixel unclaimed for, not with a measurement.
#[test]
fn a_short_radial_is_padded_with_the_unpainted_code() {
    let long = moment(vec![40u8; 30], 8, REFL_SCALE, REFL_OFFSET);
    let short = moment(vec![40u8; 12], 8, REFL_SCALE, REFL_OFFSET);
    let radials = vec![
        radial_with(0.0, 1.0, Some(long)),
        radial_with(1.0, 1.0, Some(short)),
    ];
    let built = sweep_code_plane(&radials, RadarProduct::Reflectivity, 30)
        .expect("the stride is the longer radial's");
    for g in 12..30 {
        assert_eq!(
            built.plane.code_at(1, 1, g),
            None,
            "the short radial reported a level past the chain",
        );
        assert_eq!(
            built.plane.code_at(0, 1, g),
            Some(BELOW_THRESHOLD_CODE),
            "gate {g} of the short radial was padded with a measurement",
        );
    }
}

/// A radial carrying no moment at all is the same padding, whole.
#[test]
fn a_radial_with_no_moment_is_wholly_unpainted() {
    let radials = vec![
        radial_with(
            0.0,
            1.0,
            Some(moment(vec![40u8; 8], 8, REFL_SCALE, REFL_OFFSET)),
        ),
        radial_with(1.0, 1.0, None),
    ];
    let built = sweep_code_plane(&radials, RadarProduct::Reflectivity, 8)
        .expect("one radial carrying the moment is enough");
    for g in 0..8 {
        assert_eq!(built.plane.code_at(0, 1, g), Some(BELOW_THRESHOLD_CODE));
    }
}

// ── The reach ───────────────────────────────────────────────────────────────

/// **The reach is measured off the painted gates, not off the stride.**
///
/// Three arms, and the third is the one that separates this from a length: a
/// range-folded gate is painted and must extend the reach, while a
/// below-threshold gate must not.
#[test]
fn the_reach_is_the_furthest_painted_gate() {
    // Nothing painted past gate 4.
    let near = sweep(3, 20, |_, g| if g <= 4 { 40 } else { BELOW_THRESHOLD_CODE });
    assert_eq!(
        sweep_code_plane(&near, RadarProduct::Reflectivity, 20)
            .expect("painted")
            .reach_gates,
        5,
    );

    // One range-folded gate further out, on one radial only.
    let folded = sweep(3, 20, |r, g| {
        if g <= 4 {
            40
        } else if r == 2 && g == 11 {
            RANGE_FOLDED_CODE
        } else {
            BELOW_THRESHOLD_CODE
        }
    });
    assert_eq!(
        sweep_code_plane(&folded, RadarProduct::Reflectivity, 20)
            .expect("painted")
            .reach_gates,
        12,
        "a range-folded gate is painted and has to extend the reach",
    );

    // Nothing above threshold anywhere.
    let empty = sweep(3, 20, |_, _| BELOW_THRESHOLD_CODE);
    assert_eq!(
        sweep_code_plane(&empty, RadarProduct::Reflectivity, 20),
        Err(PlaneUnavailable::NothingPainted),
    );
}

// ── What it refuses ─────────────────────────────────────────────────────────

/// A sixteen-bit moment has no byte plane, whatever its product admits.
///
/// The conjunct that matters: reflectivity is `WireByteExact`, so
/// `CodePlane::build` would have accepted this payload — `r8_fidelity` answers
/// which widths a product *admits* and this sweep's width is the narrower
/// question. Truncating the words would have been a silent quantiser on the
/// one product the representation exists for.
#[test]
fn a_sixteen_bit_moment_is_refused_even_where_the_product_admits_eight() {
    assert!(
        r8_fidelity(RadarProduct::Reflectivity).admits_eight_bit_wire(),
        "the premise: this product is admitted at eight bits",
    );
    let bytes: Vec<u8> = (0..64u16).flat_map(|v| (v + 2).to_be_bytes()).collect();
    let radials = vec![radial_with(
        0.0,
        1.0,
        Some(moment(bytes, 16, REFL_SCALE, REFL_OFFSET)),
    )];
    assert_eq!(
        sweep_code_plane(&radials, RadarProduct::Reflectivity, 64),
        Err(PlaneUnavailable::WideWireWord { word_bits: 16 }),
    );
}

/// Two radials that decode differently cannot share one table.
#[test]
fn a_sweep_whose_radials_decode_differently_is_refused() {
    let radials = vec![
        radial_with(0.0, 1.0, Some(moment(vec![40u8; 8], 8, 2.0, 66.0))),
        radial_with(1.0, 1.0, Some(moment(vec![40u8; 8], 8, 2.0, 65.0))),
    ];
    assert_eq!(
        sweep_code_plane(&radials, RadarProduct::Reflectivity, 8),
        Err(PlaneUnavailable::MixedDecode),
    );
}

/// The raw-word encoding has no code that means "no gate", so a sweep in it
/// cannot be padded and is refused.
#[test]
fn the_raw_word_encoding_is_refused() {
    let radials = vec![radial_with(
        0.0,
        1.0,
        Some(moment(vec![40u8; 8], 8, 0.0, 0.0)),
    )];
    assert_eq!(
        sweep_code_plane(&radials, RadarProduct::Reflectivity, 8),
        Err(PlaneUnavailable::RawWordEncoding),
    );
}

/// A non-finite decode would bake a fully unpainted table over gates the
/// raster still draws.
#[test]
fn a_non_finite_decode_is_refused() {
    let radials = vec![radial_with(
        0.0,
        1.0,
        Some(moment(vec![40u8; 8], 8, f32::NAN, 66.0)),
    )];
    assert!(matches!(
        sweep_code_plane(&radials, RadarProduct::Reflectivity, 8),
        Err(PlaneUnavailable::NonFiniteDecode { .. }),
    ));
}

/// A sweep carrying nothing of the product asked for.
#[test]
fn a_sweep_with_no_moment_is_refused() {
    let radials = vec![radial_with(0.0, 1.0, None)];
    assert_eq!(
        sweep_code_plane(&radials, RadarProduct::Reflectivity, 8),
        Err(PlaneUnavailable::NoMoment),
    );
}

/// **The nine products an eight-bit plane cannot carry never come back as
/// one**, and where the sweep carries their field the refusal is the fidelity
/// verdict rather than the absence of data.
///
/// The split is the whole test. Five of the nine read a moment slot, so a
/// sweep carrying reflectivity, velocity and differential phase reaches
/// `CodePlane::build`'s own verdict for them — and a bare
/// `is_err()` over all nine would have been satisfied by `NoMoment` for every
/// one, which is a refusal that says nothing about fidelity. The other four
/// are computed from a whole volume and have no slot at all; for those the
/// domain verdict is asserted directly, because this producer cannot reach it.
///
/// Both group sizes are pinned, so a product that moved between them shows up
/// here rather than sliding quietly into the vacuous half.
#[test]
fn the_lossy_products_are_refused_at_the_producer() {
    let refused = [
        RadarProduct::NormalizedRotation,
        RadarProduct::StormRelativeVelocity,
        RadarProduct::SpecificDifferentialPhase,
        RadarProduct::VerticallyIntegratedLiquid,
        RadarProduct::PrecipitationRate,
        RadarProduct::EchoTops,
        RadarProduct::EchoTopsInterpolated,
        RadarProduct::VilDensity,
        RadarProduct::DifferentialPhase,
    ];
    assert_eq!(refused.len(), 9, "nine refused, eight admitted");
    let _ledger = crate::render::codes::hold_refusal_ledger();
    let radials = a_three_moment_sweep(4, 12);
    let (mut with_field, mut without_field) = (Vec::new(), Vec::new());
    for product in refused {
        let answer = sweep_code_plane(&radials, product, 12);
        assert!(
            answer.is_err(),
            "{product:?} was given a plane an R8 code cannot carry",
        );
        if product.get_moment(&radials[0]).is_some() {
            with_field.push(format!("{product:?}"));
            assert_ne!(
                answer,
                Err(PlaneUnavailable::NoMoment),
                "{product:?} carries a moment on this sweep, so its refusal must \
                 be about what a byte can hold and not about missing data",
            );
        } else {
            without_field.push(format!("{product:?}"));
            assert_eq!(answer, Err(PlaneUnavailable::NoMoment));
            // The verdict this producer cannot reach, asserted where it lives:
            // these five are computed over a whole volume and have no wire
            // codes for a sweep walk to find.
            assert!(
                !r8_fidelity(product).admits_eight_bit_wire(),
                "{product:?} has no moment slot here, so nothing but its domain \
                 verdict keeps it off a plane",
            );
        }
    }
    // Named rather than counted: a product that moved between the arm this
    // sweep exercises and the arm it cannot is what would quietly make the
    // first half vacuous, and a name says which one did.
    assert_eq!(
        with_field,
        [
            "NormalizedRotation",
            "StormRelativeVelocity",
            "EchoTopsInterpolated",
            "DifferentialPhase",
        ],
        "the refused products this sweep can actually put through the fidelity \
         verdict are not the ones they were",
    );
    assert_eq!(
        without_field,
        [
            "SpecificDifferentialPhase",
            "VerticallyIntegratedLiquid",
            "PrecipitationRate",
            "EchoTops",
            "VilDensity",
        ],
    );
}

/// A sweep whose radials carry reflectivity and velocity at eight bits and
/// differential phase at sixteen — every moment slot the nine refused products
/// read between them.
fn a_three_moment_sweep(n_radials: usize, gates: usize) -> Vec<Radial> {
    let spacing = 360.0 / n_radials as f32;
    (0..n_radials)
        .map(|r| {
            let eight: Vec<u8> = (0..gates).map(|g| ((r + g) % 254 + 2) as u8).collect();
            let wide: Vec<u8> = (0..gates as u16)
                .flat_map(|g| (g + 2).to_be_bytes())
                .collect();
            Radial::new(
                0,
                r as u16,
                r as f32 * spacing,
                spacing,
                RadialStatus::IntermediateRadialData,
                1,
                0.5,
                Some(moment(eight.clone(), 8, REFL_SCALE, REFL_OFFSET)),
                Some(moment(eight, 8, 2.0, 129.0)),
                None,
                None,
                Some(moment(wide, 16, 2.8361, 2.0)),
                None,
                None,
            )
        })
        .collect()
}

/// A plane may not hold more distinct measurements than a byte can address,
/// and the producer's own admission is the wire byte's 254.
#[test]
fn the_admitted_products_fit_the_paintable_codes() {
    let radials = ascending_sweep(4, 300);
    let built = sweep_code_plane(&radials, RadarProduct::Reflectivity, 300)
        .expect("reflectivity admits a plane");
    let (bytes, _, _) = built.plane.level(0).expect("level 0");
    let distinct: std::collections::BTreeSet<u8> = bytes
        .iter()
        .copied()
        .filter(|&c| c != BELOW_THRESHOLD_CODE && c != RANGE_FOLDED_CODE)
        .collect();
    assert!(
        distinct.len() <= PAINTABLE_CODES,
        "a plane held {} distinct measurements against {PAINTABLE_CODES} paintable codes",
        distinct.len(),
    );
}
