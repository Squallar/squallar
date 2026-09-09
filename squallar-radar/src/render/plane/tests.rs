//! What the polar producer refuses, and what it keeps.

use super::*;
use crate::render::codes::{Lut, PAINTABLE_CODES, RANGE_FOLDED_CODE, r8_fidelity};
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
    let lut = built.plane.lut();
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

/// **The eleven products an eight-bit plane cannot carry never come back as
/// one**, and where the sweep carries their field the refusal is the fidelity
/// verdict rather than the absence of data.
///
/// The split is the whole test. Six of the eleven read a moment slot, so a
/// sweep carrying reflectivity, velocity and differential phase reaches
/// `CodePlane::build`'s own verdict for them — and a bare
/// `is_err()` over all eleven would have been satisfied by `NoMoment` for
/// every one, which is a refusal that says nothing about fidelity. The other
/// five are computed from a whole volume and have no slot at all; for those
/// the domain verdict is asserted directly, because this producer cannot reach
/// it.
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
        RadarProduct::ProbabilityOfSevereHail,
        RadarProduct::MaxExpectedHailSize,
    ];
    assert_eq!(refused.len(), 11, "eleven refused, six admitted");
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
            // The hail pair reads the reflectivity slot, so this sweep really
            // does put their verdict through `CodePlane::build` rather than
            // stopping at `NoMoment` — which is what makes their move to the
            // refused set observable here at all.
            "ProbabilityOfSevereHail",
            "MaxExpectedHailSize",
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
/// differential phase at sixteen — every moment slot the eleven refused
/// products read between them.
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

// ── What a code means as a number ───────────────────────────────────────────

/// `(scale, offset)` pairs a real moment block has carried, plus the shapes
/// that break naive arithmetic — the same hostile set `codes::tests` measures
/// the colour table against, because the two tables are one decode.
const DECODES: &[(f32, f32)] = &[
    (2.0, 66.0),
    (2.0, 129.0),
    (16.0, 128.0),
    (25.3, 128.5),
    (1.0, 0.0),
    (1000.0, 0.0),
    (-2.0, 66.0),
    (0.0, 0.0),
    (f32::NAN, 0.0),
    (f32::INFINITY, 0.0),
];

/// **The value table is the fill loop's own decode, code for code, on the
/// bits.**
///
/// The whole claim a readout over a code plane rests on: `Lut::value_of` is
/// `moment_value_at` followed by `painted_moment_value` — the exact pair the
/// raster's fill loop runs per gate — evaluated once per code instead of once
/// per gate. So a hover that indexes this table reads back the number the
/// raster's own value grid held, and the read-back is an indexing rather than
/// a rounding.
///
/// **On the bits and not on `==`.** Two of the things this decode produces are
/// NaNs that mean different things, and `f32` equality neither distinguishes
/// them nor separates `-0.0` from `0.0`. The wire carries the bits, so the
/// bits are what is compared.
///
/// The reference side reads a **real moment block** rather than a second
/// spelling of the arithmetic: the raw byte goes through `MomentData`'s own
/// decode, which is the authority both tables exist to mirror.
///
/// TAMPER: give `value_of` the plain `f32::NAN` for a below-threshold gate
/// instead of the unpainted marker, or drop its `scale == 0.0` arm, and this
/// goes red.
#[test]
fn the_value_table_is_the_fill_loops_decode_code_for_code() {
    let mut checked = 0usize;
    for &product in RadarProduct::all() {
        for &(scale, offset) in DECODES {
            let key = LutKey {
                product,
                scale,
                offset,
            };
            let table = Lut::value_table(key);
            assert_eq!(
                table.len(),
                256,
                "the table addresses every code a byte holds"
            );
            for code in 0..=u8::MAX {
                let block = moment(vec![code], 8, scale, offset);
                let expected = moment_value_at(&block, 0)
                    .and_then(painted_moment_value)
                    .unwrap_or(crate::render::polar::UNPAINTED);
                assert_eq!(
                    table[usize::from(code)].to_bits(),
                    expected.to_bits(),
                    "code {code} at scale {scale} offset {offset}: the value table and the \
                     fill loop's decode disagree, so a readout over a plane would answer a \
                     number the raster never painted",
                );
                assert_eq!(
                    Lut::value_of(key, code).to_bits(),
                    table[usize::from(code)].to_bits(),
                    "the table and the per-code answer must be one function",
                );
                checked += 1;
            }
        }
    }
    assert_eq!(
        checked,
        RadarProduct::all().len() * DECODES.len() * 256,
        "the walk did not cover the product x decode x code space it claims",
    );
}

/// **The two NaNs the decode produces stay distinct through the table and out
/// onto the wire**, which `PolarField::at` cannot show because it answers
/// `None` for both.
///
/// A below-threshold gate is one the radar measured nothing in; a range-folded
/// one is a gate the radar has a return from and cannot place. `at` collapses
/// them — that is what it is for — so the separation has to be asserted where
/// the bits are still visible, which is `to_bytes`.
///
/// TAMPER: table a below-threshold code as `painted_moment_value`'s
/// range-folded answer, or write `f32::NAN` for the fold, and the inequality
/// below goes red while every `at` in the suite stays green.
#[test]
fn the_unpainted_marker_and_the_fold_sentinel_stay_two_patterns_on_the_wire() {
    let key = LutKey {
        product: RadarProduct::Reflectivity,
        scale: REFL_SCALE,
        offset: REFL_OFFSET,
    };
    let table = Lut::value_table(key);
    let folded = painted_moment_value(nexrad_model::data::MomentValue::RangeFolded)
        .expect("a range-folded gate is painted");
    assert_eq!(
        table[usize::from(BELOW_THRESHOLD_CODE)].to_bits(),
        crate::render::polar::UNPAINTED.to_bits(),
    );
    assert_eq!(
        table[usize::from(RANGE_FOLDED_CODE)].to_bits(),
        folded.to_bits(),
    );
    assert_ne!(
        crate::render::polar::UNPAINTED.to_bits(),
        folded.to_bits(),
        "the premise: the two markers are two bit patterns, or nothing below can fail",
    );

    // One radial, two gates: the first below threshold, the second folded.
    let geometry = crate::render::polar::PolarGeometry::from_parts(
        vec![crate::render::polar::Wedge {
            azimuth_deg: 0.0,
            half_width_deg: 0.5,
        }],
        1.0,
        0.25,
        Some(0.5),
        2,
    );
    let field = crate::render::polar::PolarField::from_code_table(
        geometry,
        vec![BELOW_THRESHOLD_CODE, RANGE_FOLDED_CODE],
        table,
    )
    .expect("two codes of a one-radial sweep");

    // `at` cannot separate them, which is why the wire is asked instead.
    for gate in 0..2 {
        assert_eq!(
            field.at(crate::render::polar::GateAt { radial: 0, gate }),
            None,
            "gate {gate}: both markers read as unpainted through `at`",
        );
    }

    let bytes = field.to_bytes();
    let value_at = |gate: usize| {
        let start = bytes.len() - 8 + gate * 4;
        u32::from_le_bytes(bytes[start..start + 4].try_into().expect("four bytes"))
    };
    assert_eq!(value_at(0), crate::render::polar::UNPAINTED.to_bits());
    assert_eq!(value_at(1), folded.to_bits());
    assert_ne!(
        value_at(0),
        value_at(1),
        "the wide wire form collapsed the two markers into one pattern",
    );
}

/// **A code the table does not name is refused, not indexed.**
///
/// `from_code_table` is handed three parts that arrive separately, and a code
/// past the table's end is one sweep's codes read against another sweep's
/// table. Refused at the door rather than answered out of bounds on the thread
/// that reads a hover.
#[test]
fn a_code_the_table_does_not_name_is_refused() {
    let geometry = crate::render::polar::PolarGeometry::from_parts(
        vec![crate::render::polar::Wedge {
            azimuth_deg: 0.0,
            half_width_deg: 0.5,
        }],
        1.0,
        0.25,
        Some(0.5),
        2,
    );
    // A two-entry table and a code of 7.
    assert_eq!(
        crate::render::polar::PolarField::from_code_table(
            geometry.clone(),
            vec![0, 7],
            vec![1.0, 2.0],
        ),
        None,
    );
    // The shape has to be exactly radials x gates.
    assert_eq!(
        crate::render::polar::PolarField::from_code_table(
            geometry.clone(),
            vec![0],
            vec![1.0, 2.0],
        ),
        None,
    );
    // And the control: the same parts, in shape, are admitted.
    assert!(
        crate::render::polar::PolarField::from_code_table(geometry, vec![0, 1], vec![1.0, 2.0],)
            .is_some(),
        "the control: a well-formed triple is admitted, or the refusals above prove nothing",
    );
}

// ── The per-sweep admission ─────────────────────────────────────────────────

/// **A computed sweep whose realised numbers fit a byte becomes a plane, and
/// every gate reads back the pattern it was given** — at a surveillance cut's
/// own shape.
///
/// The shape is load-bearing and not decoration. `value_code_plane` walks
/// `radials × gates`, assigns codes through an open-addressed table sized to a
/// cache, and remaps the whole buffer; at the 9×40 and 36×120 shapes the other
/// fixtures in this file use, every one of those is a few hundred operations
/// and a probe error or an off-by-one in the remap has nowhere to show. This
/// is 1,319,040 gates and 254 distinct patterns, which is the top of the form.
///
/// **The values arrive DESCENDING.** The table the plane keeps must be
/// ascending — `Reduce::MaxCode` is a statement about the numbers — so a
/// producer that kept first-seen order would pass a read-back test and paint
/// the wrong cell at every zoom above the closest. A fixture whose first-seen
/// order already happened to be ascending could not tell the two apart.
#[test]
fn a_computed_sweep_inside_the_ceiling_reads_back_its_own_bits() {
    let (radials, gates) = (720usize, 1832usize);
    // 254 distinct patterns, straddling zero, with the unpainted gates woven
    // through so the sentinel is exercised too.
    let value_of = |radial: usize, gate: usize| -> Option<f32> {
        let i = radial * gates + gate;
        if i.is_multiple_of(7) {
            return None;
        }
        // Descending in the order the walk sees them: the first radial hands
        // over the largest numbers first.
        Some((PAINTABLE_CODES - 1 - (i % PAINTABLE_CODES)) as f32 * 0.125 - 12.0)
    };
    let built = value_code_plane(radials, gates, RadarProduct::NormalizedRotation, value_of)
        .expect("254 distinct numbers are exactly what a byte can name");
    assert_eq!(built.plane.shape(), (radials, gates));
    assert_eq!(built.reach_gates, gates);

    let table = built.plane.value_table();
    // The table is ascending over the codes it names, which is what the
    // reduce reads.
    let named = &table[usize::from(crate::render::codes::FIRST_TABLE_CODE)
        ..usize::from(crate::render::codes::FIRST_TABLE_CODE) + PAINTABLE_CODES];
    assert!(
        named.windows(2).all(|w| w[0] < w[1]),
        "the table kept the order the gates arrived in rather than the numbers' own",
    );

    let (level0, _, _) = built.plane.level(0).expect("level 0");
    let mut painted = 0usize;
    let mut unpainted = 0usize;
    for radial in 0..radials {
        for gate in 0..gates {
            let got = table[usize::from(level0[radial * gates + gate])];
            match value_of(radial, gate) {
                Some(want) => {
                    assert_eq!(
                        got.to_bits(),
                        want.to_bits(),
                        "gate ({radial}, {gate}) reads back {got} for {want}",
                    );
                    painted += 1;
                }
                None => {
                    assert_eq!(
                        got.to_bits(),
                        crate::render::polar::UNPAINTED.to_bits(),
                        "gate ({radial}, {gate}) the raster paints nothing at carries a number",
                    );
                    unpainted += 1;
                }
            }
        }
    }
    assert_eq!(painted + unpainted, radials * gates);
    assert!(
        unpainted > 100_000 && painted > 1_000_000,
        "{painted} painted and {unpainted} unpainted: one side is too thin to be evidence",
    );
}

/// **One number past the ceiling is a refusal and not a rounding**, and the
/// refusal names its own reason.
///
/// Asserted as the variant rather than as an `Err`: `value_code_plane` has
/// four other ways to refuse — a shape that overflows, nothing painted, and
/// `CodePlane::build_values`'s own two — and a test reading only `is_err`
/// stays green with the width check deleted, because the table would then be
/// 255 entries and the constructor behind it would refuse the same payload for
/// `TableWidth`. The two are different guards and this names which fired.
///
/// The bound is walked in both directions at the boundary, so it is live.
#[test]
fn a_computed_sweep_past_the_ceiling_stays_a_raster_and_says_so() {
    let distinct = |n: usize| {
        move |radial: usize, gate: usize| -> Option<f32> {
            Some(((radial * 97 + gate) % n) as f32 * 0.5)
        }
    };
    let product = RadarProduct::NormalizedRotation;
    assert!(
        value_code_plane(64, 64, product, distinct(PAINTABLE_CODES)).is_ok(),
        "a sweep painting exactly PAINTABLE_CODES numbers was refused",
    );
    assert_eq!(
        value_code_plane(64, 64, product, distinct(PAINTABLE_CODES + 1)).err(),
        Some(PlaneUnavailable::ValuesTooWide {
            entries: PAINTABLE_CODES + 1
        }),
        "one number past the ceiling was admitted, or refused for another reason",
    );
    // A sweep that paints nothing is the other end of the same door.
    assert_eq!(
        value_code_plane(8, 8, product, |_, _| None).err(),
        Some(PlaneUnavailable::NothingPainted),
    );
}

/// **The reach is measured off what was painted, not off the shape.**
///
/// A gate past it is one the sweep carried and nothing was above threshold in,
/// and drawing the disc out to it would put a ring of nothing outside the
/// weather. Same quantity `PolarBuffers::into_field` reads off the raster.
#[test]
fn a_value_planes_reach_is_the_last_painted_gate() {
    let built = value_code_plane(4, 100, RadarProduct::NormalizedRotation, |_, gate| {
        (gate < 37).then_some(gate as f32)
    })
    .expect("a narrow, well-formed sweep");
    assert_eq!(built.reach_gates, 37);
    assert_eq!(built.plane.shape(), (4, 100));
}
