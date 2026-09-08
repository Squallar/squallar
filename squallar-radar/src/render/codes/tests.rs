use super::*;
use crate::palette::get_color_for_value;

/// `(scale, offset)` pairs a real moment block has carried, plus the shapes
/// that break naive arithmetic. The reproduction property is universal in the
/// key, so the set is chosen to be hostile rather than representative.
const KEYS: &[(f32, f32)] = &[
    // Level II 8-bit reflectivity and velocity, and the 8-bit ZDR encoding
    // `voxel.rs` names.
    (2.0, 66.0),
    (2.0, 129.0),
    (16.0, 128.0),
    // The derived products' own fixed-point codecs, from `derive::codec`.
    (25.3, 128.5),
    // Unit scale, and a scale that puts every code below the first stop.
    (1.0, 0.0),
    (1000.0, 0.0),
    // Negative scale: the decode runs backwards down the palette.
    (-2.0, 66.0),
    // The format's "raw words are the values" arm.
    (0.0, 0.0),
    // Non-finite: every decode is non-finite, so nothing paints.
    (f32::NAN, 0.0),
    (f32::INFINITY, 0.0),
];

/// **Every entry of a baked table equals the palette at that code's value.**
///
/// The whole claim the polar representation rests on for a wire-coded moment:
/// resolving colour on the GPU through 256 entries is a re-indexing of
/// [`get_color_for_value`], not a resampling of it. Byte equality over all 256
/// entries and every key in [`KEYS`] — not a tolerance, because there is no
/// approximation here to tolerate.
#[test]
fn the_lut_reproduces_get_color_for_value() {
    let mut checked = 0usize;
    for &product in RadarProduct::all() {
        for &(scale, offset) in KEYS {
            let key = LutKey {
                product,
                scale,
                offset,
            };
            let lut = Lut::build(key);
            for code in 0..=u8::MAX {
                let expected = if scale == 0.0 {
                    get_color_for_value(product, f32::from(code))
                } else {
                    match code {
                        BELOW_THRESHOLD_CODE => (0, 0, 0, 0),
                        RANGE_FOLDED_CODE => palette::RANGE_FOLDED,
                        _ => get_color_for_value(product, (f32::from(code) - offset) / scale),
                    }
                };
                assert_eq!(
                    lut.entry(code),
                    expected,
                    "code {code} of {product:?} at scale {scale} offset {offset}: the baked \
                     table and the palette disagree, so a polar frame would not paint what \
                     the raster paints",
                );
                checked += 1;
            }
        }
    }
    // The walk really ran: seventeen products, every key, every code.
    assert_eq!(
        checked,
        RadarProduct::all().len() * KEYS.len() * LUT_ENTRIES,
        "the reproduction walk did not cover the product x key x code space it claims",
    );
}

/// **The two sentinel codes carry status, not measurement** — for every
/// product, and only where the decode is affine.
#[test]
fn the_sentinel_codes_are_status_and_not_values() {
    for &product in RadarProduct::all() {
        let lut = Lut::build(LutKey::identity(product));
        assert_eq!(
            lut.entry(BELOW_THRESHOLD_CODE),
            (0, 0, 0, 0),
            "{product:?}: a below-threshold gate must be unpainted, so the basemap shows \
             through it; a black entry would paint a hole instead",
        );
        assert_eq!(
            lut.entry(RANGE_FOLDED_CODE),
            palette::RANGE_FOLDED,
            "{product:?}: a range-folded gate must carry the fold colour",
        );
    }
    // And the sentinels are genuinely two distinct answers, so a table that
    // returned one colour for both could not pass the pair above.
    assert_ne!(palette::RANGE_FOLDED, (0, 0, 0, 0));
}

/// **A zero-scale moment has no sentinels**, because in that encoding the raw
/// words are the values and 0 and 1 are ordinary numbers.
///
/// The one branch where the table must *not* apply the status rule. It is the
/// same literal `scale == 0.0` comparison `crate::render`'s `moment_value_at`
/// makes, and reproducing it is what keeps a zero-scale moment's legitimate 0
/// and 1 gates from being repainted as status.
#[test]
fn a_zero_scale_moment_paints_its_raw_words() {
    // Reflectivity's palette paints 0 dBZ and refuses everything below it, so
    // it separates the two readings of code 0 cleanly.
    let product = RadarProduct::Reflectivity;
    let zero_scale = Lut::build(LutKey {
        product,
        scale: 0.0,
        offset: 0.0,
    });
    assert_eq!(
        zero_scale.entry(0),
        get_color_for_value(product, 0.0),
        "under a zero scale, code 0 is the value 0 and must paint as it",
    );
    assert_eq!(
        zero_scale.entry(1),
        get_color_for_value(product, 1.0),
        "under a zero scale, code 1 is the value 1 and must not become the fold colour",
    );
    // The control: the same two codes under a normal scale ARE status, so the
    // branch above is doing something rather than agreeing by coincidence.
    let affine = Lut::build(LutKey::identity(product));
    assert_eq!(affine.entry(0), (0, 0, 0, 0));
    assert_eq!(affine.entry(1), palette::RANGE_FOLDED);
    assert_ne!(zero_scale.entry(1), affine.entry(1));
}

/// **The table's bytes are the texture's bytes**: code order, four channels,
/// straight alpha.
#[test]
fn the_table_bytes_are_straight_alpha_in_code_order() {
    let lut = Lut::build(LutKey::identity(RadarProduct::Reflectivity));
    let bytes = lut.to_rgba_bytes();
    assert_eq!(bytes.len(), LUT_ENTRIES * 4);
    for code in 0..=u8::MAX {
        let at = usize::from(code) * 4;
        let (r, g, b, a) = lut.entry(code);
        assert_eq!(
            (bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]),
            (r, g, b, a),
            "code {code} is not at its own offset in the uploaded bytes",
        );
    }
    // Straight, not premultiplied: a painted entry keeps its own channels at
    // full alpha, so nothing has been scaled on the way out.
    let painted = lut.entry(200);
    // `RANGE_FOLDED` is a painted answer, so its alpha is the alpha every
    // painted answer carries; `OPAQUE` itself is private to the palette.
    assert_eq!(painted.3, palette::RANGE_FOLDED.3);
    assert_eq!(
        (painted.0, painted.1, painted.2),
        {
            let (r, g, b, _) = Lut::colour_of(LutKey::identity(RadarProduct::Reflectivity), 200);
            (r, g, b)
        },
        "the byte form scaled a channel, so it is premultiplied and the fragment would \
         apply the factor twice",
    );
}

/// Where a product's gate values come from, which is what decides how many
/// distinct values one gate can ever hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodeSource {
    /// An 8-bit moment: the wire carries the code itself, so a gate can hold
    /// [`PAINTABLE_CODES`] distinct values and no more. An R8 plane stores it
    /// **as measured**.
    WireByte,
    /// A 16-bit moment. The wire carries 65,534 distinct codes; an R8 plane
    /// can address 254 of them.
    WireWord,
    /// The wire carries this moment at eight bits in most volumes and at
    /// sixteen in some. It is [`CodeSource::WireByte`] on the first and
    /// [`CodeSource::WireWord`] on the second, so it is exact only
    /// conditionally and the migration must branch on the volume in hand.
    WireByteOrWord,
    /// No wire code: the field is computed as `f32` per gate and painted
    /// straight through the palette, so the values it can take are continuous
    /// and an R8 plane is a **quantiser**.
    Computed,
}

impl CodeSource {
    /// How many distinct values one gate can hold, where that is bounded by
    /// the encoding rather than by the palette. `None` is "continuous".
    fn reachable_values(self) -> Option<usize> {
        match self {
            CodeSource::WireByte => Some(PAINTABLE_CODES),
            // 65,536 codes less the two sentinels.
            CodeSource::WireWord => Some((1 << 16) - 2),
            // The wide form is the one that decides.
            CodeSource::WireByteOrWord => Some((1 << 16) - 2),
            CodeSource::Computed => None,
        }
    }
}

/// Every product, where its gate values come from, and **whether an R8 code
/// plane can carry it without dropping a distinction the raster paints
/// today**. The last column is a claim the test recomputes from two
/// independent measurements; it is never read back from the thing it
/// describes.
///
/// Complete by assertion: a new [`RadarProduct`] with no row here fails the
/// walk rather than defaulting into a verdict.
const R8_FIDELITY: &[(RadarProduct, CodeSource, bool)] = &[
    (RadarProduct::Reflectivity, CodeSource::WireByte, true),
    (RadarProduct::Velocity, CodeSource::WireByte, true),
    (RadarProduct::SpectrumWidth, CodeSource::WireByte, true),
    (
        RadarProduct::CorrelationCoefficient,
        CodeSource::WireByte,
        true,
    ),
    (
        RadarProduct::HydrometeorClassification,
        CodeSource::WireByte,
        true,
    ),
    (
        RadarProduct::DifferentialReflectivity,
        CodeSource::WireByteOrWord,
        false,
    ),
    (RadarProduct::DifferentialPhase, CodeSource::WireWord, false),
    (
        RadarProduct::StormRelativeVelocity,
        CodeSource::Computed,
        false,
    ),
    (
        RadarProduct::SpecificDifferentialPhase,
        CodeSource::Computed,
        false,
    ),
    (
        RadarProduct::NormalizedRotation,
        CodeSource::Computed,
        false,
    ),
    (RadarProduct::EchoTops, CodeSource::Computed, false),
    (
        RadarProduct::EchoTopsInterpolated,
        CodeSource::Computed,
        false,
    ),
    (
        RadarProduct::VerticallyIntegratedLiquid,
        CodeSource::Computed,
        false,
    ),
    (RadarProduct::VilDensity, CodeSource::Computed, false),
    (RadarProduct::PrecipitationRate, CodeSource::Computed, false),
    (
        RadarProduct::ProbabilityOfSevereHail,
        CodeSource::Computed,
        true,
    ),
    (
        RadarProduct::MaxExpectedHailSize,
        CodeSource::Computed,
        true,
    ),
];

/// **An R8 plane is exact for a wire-coded byte moment and lossy for a
/// computed field with a gradient palette — measured, per product.**
///
/// This is the fidelity question the polar representation has to answer before
/// anything renders through it, and it has two independent halves that must
/// not be conflated:
///
/// * **How many values a gate can hold.** For an 8-bit moment that is 254,
///   because the wire says so. An R8 plane stores those codes *as measured*,
///   which is why polar is not a quality trade there — and it is why the
///   width of the palette is irrelevant for such a product. Reflectivity's
///   palette resolves several thousand distinct colours across its extent, and
///   a real reflectivity sweep still shows at most 254 of them, today and
///   under a code plane alike.
/// * **How many colours the palette can show.** That decides the *computed*
///   fields, which have no wire code at all: quantising a continuous `f32`
///   into 254 codes drops every distinction finer than the quantum.
///
/// The verdict is `min(reachable values, palette width) <= PAINTABLE_CODES`,
/// and each side is established soundly rather than by a converged estimate:
///
/// * a **lossy** verdict needs a *lower* bound on the palette's width, and
///   sampling gives exactly that — a probe can miss a colour between two
///   samples, never invent one;
/// * a **lossless** verdict needs an *upper* bound, which comes either from
///   the encoding (254 wire codes) or, for a banded scale, from its own stop
///   count. No gradient-palette product is declared lossless on a sample.
#[test]
fn an_r8_plane_is_exact_for_wire_bytes_and_lossy_for_computed_gradients() {
    const SAMPLES: usize = 200_000;

    for &product in RadarProduct::all() {
        let rows = R8_FIDELITY.iter().filter(|(p, ..)| *p == product).count();
        assert_eq!(
            rows, 1,
            "{product:?} has {rows} rows in R8_FIDELITY; every product needs exactly one, so a \
             new product forces a fidelity decision rather than inheriting one",
        );
    }
    assert_eq!(
        R8_FIDELITY.len(),
        RadarProduct::all().len(),
        "R8_FIDELITY names a product twice or misses one",
    );

    let mut lossless = Vec::new();
    let mut lossy = Vec::new();
    for &(product, source, claimed_lossless) in R8_FIDELITY {
        let scale = palette::get_legend_scale_ref(product);
        let floor = distinct_palette_colours(product, SAMPLES);
        assert!(
            floor > 0,
            "{product:?} painted nothing anywhere inside its own legend's extent \
             ({}..{}), so the walk is reading the wrong domain",
            scale.min_value,
            scale.max_value,
        );

        // An upper bound on the palette's width, where one exists. A banded
        // scale can show no more colours than it has stops; a gradient scale
        // interpolates between them and this instrument bounds it nowhere.
        let palette_ceiling = (!scale.is_gradient).then_some(scale.thresholds.len());
        if let Some(ceiling) = palette_ceiling {
            assert!(
                floor <= ceiling,
                "{product:?}: the sampler found {floor} distinct colours in a banded scale of \
                 {ceiling} stops, which is impossible unless it is not sampling that scale",
            );
        }

        // Lossless needs an upper bound from SOME side.
        let bounded_lossless = source
            .reachable_values()
            .is_some_and(|v| v <= PAINTABLE_CODES)
            || palette_ceiling.is_some_and(|c| c <= PAINTABLE_CODES);
        // Lossy needs a lower bound, on both sides at once: the encoding must
        // be able to produce more values than the plane holds AND the palette
        // must actually distinguish more of them than the plane can index.
        let bounded_lossy = source
            .reachable_values()
            .is_none_or(|v| v > PAINTABLE_CODES)
            && floor > PAINTABLE_CODES;

        assert!(
            bounded_lossless != bounded_lossy,
            "{product:?} is neither soundly lossless nor soundly lossy: source {source:?} \
             ({:?} reachable values), palette floor {floor}, palette ceiling \
             {palette_ceiling:?}. A gradient palette over a wide encoding cannot be declared \
             exact on a sampled floor.",
            source.reachable_values(),
        );
        assert_eq!(
            bounded_lossless,
            claimed_lossless,
            "{product:?}: source {source:?} reaches {:?} distinct values and its palette \
             resolves at least {floor} colours over {}..{}, against {PAINTABLE_CODES} \
             paintable R8 codes. The measured verdict is lossless={bounded_lossless} and the \
             row claims {claimed_lossless}.",
            source.reachable_values(),
            scale.min_value,
            scale.max_value,
        );

        if bounded_lossless {
            lossless.push(product);
        } else {
            lossy.push((product, floor));
        }
    }

    // Both verdicts occur. A walk landing every product on one side would pass
    // just as well against a measurement that had stopped working.
    assert!(
        !lossy.is_empty(),
        "no product exceeded {PAINTABLE_CODES}, so either the measurement is broken or the \
         R8 fidelity constraint has stopped existing",
    );
    assert!(
        !lossless.is_empty(),
        "every product exceeded {PAINTABLE_CODES}, which would make the walk useless as a \
         discriminator",
    );

    // The load-bearing half of the design's claim, stated as its own
    // assertion: polar is not a quality trade for the moments it migrates
    // first, and it is exactly the wire-byte moments that carry that.
    for &(product, source, claimed_lossless) in R8_FIDELITY {
        if source == CodeSource::WireByte {
            assert!(
                claimed_lossless,
                "{product:?} reads 8-bit codes off the wire, so an R8 plane stores it as \
                 measured and cannot be lossy",
            );
        }
    }
}
