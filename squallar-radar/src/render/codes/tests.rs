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
    /// How many distinct values one gate can hold **on the narrowest wire
    /// form this product appears in**.
    ///
    /// Separate from [`CodeSource::reachable_values`] because they answer
    /// different questions and differential reflectivity is where that
    /// matters: unconditionally it is not R8-exact, because the wide form
    /// exists; on a volume that carried it at eight bits it is exact, and that
    /// is the question `CodePlane::build` asks when a caller states the word
    /// size. Comparing the unconditional verdict against the encoder's
    /// conditional one would fail on a product that is behaving correctly.
    fn reachable_values_narrow(self) -> Option<usize> {
        match self {
            CodeSource::WireByteOrWord => Some(PAINTABLE_CODES),
            other => other.reachable_values(),
        }
    }

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

        // The production classification is the thing the encoder enforces, so
        // it -- not this table -- is what has to agree with the measurement.
        // Without this the module would carry an unmeasured second opinion.
        let narrow_lossless = source
            .reachable_values_narrow()
            .is_some_and(|v| v <= PAINTABLE_CODES)
            || palette_ceiling.is_some_and(|c| c <= PAINTABLE_CODES);
        assert_eq!(
            crate::render::codes::r8_fidelity(product).admits_eight_bit_wire(),
            narrow_lossless,
            "{product:?}: `r8_fidelity` says {:?} while the measurement says lossless=             {bounded_lossless}. `CodePlane::build` gates on the former, so a disagreement              means the encoder admits or refuses the wrong product.",
            crate::render::codes::r8_fidelity(product),
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

/// A deterministic code field with every feature the reduce has to handle:
/// both sentinels, codes either side of a mid-scale zero, and the top of the
/// range.
fn woven_codes(radials: usize, gates: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..radials * gates)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            (state >> 33) as u8
        })
        .collect()
}

/// The declared reduce over a level-0 footprint, walked directly.
///
/// The independent answer `max_mip_is_max` compares the chain against: it
/// never looks at a mip level, so an error shared by the builder and this
/// walker would have to be written twice in two shapes.
fn reduce_footprint(plane: &CodePlane, reduce: Reduce, level: usize, r: usize, g: usize) -> u8 {
    let (radials, gates) = plane.shape();
    let span = 1usize << level;
    // The footprint is `[i·2^L, (i+1)·2^L)` — **except at the last cell of an
    // axis, which takes the odd remainder the halving rounded off.** Written
    // from the partition rather than from the builder: a `min` against the
    // extent, which is what stood here, silently agreed with a chain that
    // dropped those gates.
    let end = |i: usize, extent: usize| {
        if i + 1 == (extent >> level).max(1) {
            extent
        } else {
            (i + 1) * span
        }
    };
    let zero = plane.key().offset.round();
    let mut best: Option<u8> = None;
    let mut folded = false;
    for radial in r * span..end(r, radials) {
        for gate in g * span..end(g, gates) {
            let code = plane.code_at(0, radial, gate).expect("inside level 0");
            match code {
                0 => {}
                1 => folded = true,
                _ => {
                    let better = match best {
                        None => true,
                        Some(cur) => match reduce {
                            Reduce::MaxCode => code > cur,
                            Reduce::MaxMagnitude => {
                                let k = |c: u8| (((f32::from(c) - zero).abs() * 1_000.0) as i64, c);
                                k(code) > k(cur)
                            }
                            Reduce::None => false,
                        },
                    };
                    if better {
                        best = Some(code);
                    }
                }
            }
        }
    }
    best.unwrap_or(if folded { 1 } else { 0 })
}

/// **Every cell of every mip level is the declared reduce over its own level-0
/// footprint** — for both operators, including the sentinel rule.
///
/// The property the zoomed-out picture rests on. The chain is built level from
/// level, which is only equal to reducing the whole footprint at once because
/// each operator is a maximum over a total order with the same fallback; this
/// asserts that rather than arguing it. Shapes are deliberately odd so the
/// halving leaves a remainder, where a footprint is LARGER than `2^level` on
/// one or both axes — the last cell of an odd extent takes it, which is what
/// keeps the footprints a partition of level 0.
#[test]
fn max_mip_is_max() {
    let shapes = [(720usize, 1832usize), (360, 230), (37, 5), (1, 1), (2, 3)];
    let operators = [
        (RadarProduct::Reflectivity, Reduce::MaxCode),
        (RadarProduct::Velocity, Reduce::MaxMagnitude),
    ];
    let mut cells_checked = 0usize;
    for (radials, gates) in shapes {
        for (product, expected_reduce) in operators {
            let key = LutKey {
                product,
                scale: 2.0,
                offset: 129.0,
            };
            let plane = CodePlane::build(
                radials,
                gates,
                woven_codes(radials, gates, (radials * gates) as u64),
                key,
                8,
            )
            .expect("a well-formed payload inside both caps");
            assert_eq!(plane.reduce(), expected_reduce);
            assert_eq!(plane.levels(), full_mip_levels(radials, gates));

            for level in 1..plane.levels() {
                let (_, r, g) = plane.level(level).expect("inside the chain");
                for radial in 0..r {
                    for gate in 0..g {
                        assert_eq!(
                            plane.code_at(level, radial, gate),
                            Some(reduce_footprint(
                                &plane,
                                expected_reduce,
                                level,
                                radial,
                                gate
                            )),
                            "{product:?} {radials}x{gates} level {level} cell ({radial}, {gate}): \
                             the chain and a direct walk of the level-0 footprint disagree, so a \
                             zoomed-out fragment would not read the strongest echo under it",
                        );
                        cells_checked += 1;
                    }
                }
            }
        }
    }
    assert!(
        cells_checked > 500_000,
        "only {cells_checked} mip cells were compared, which is not the shape set this claims",
    );
}

/// **The sentinel rule survives the reduce**: status codes never outrank a
/// measurement, and they only survive where nothing under the cell measured.
#[test]
fn the_reduce_keeps_status_under_measurement() {
    let key = LutKey {
        product: RadarProduct::Velocity,
        scale: 2.0,
        offset: 129.0,
    };
    // A 2x2 whose cells are: below-threshold, range-folded, a slow gate and a
    // fast one. Code 0 sits 129 from the mid-scale zero and would win a naive
    // magnitude comparison against either real velocity.
    let plane = CodePlane::build(2, 2, vec![0, 1, 130, 250], key, 8).expect("well formed");
    assert_eq!(
        plane.code_at(1, 0, 0),
        Some(250),
        "the fastest real gate must win; a sentinel outranking it means codes 0 and 1 were \
         not excluded from the magnitude comparison",
    );
    // Nothing measured: the fold survives over the below-threshold cell.
    let folded = CodePlane::build(2, 2, vec![0, 1, 0, 0], key, 8).expect("well formed");
    assert_eq!(folded.code_at(1, 0, 0), Some(1));
    // Nothing at all: below threshold, and the mip is unpainted rather than black.
    let empty = CodePlane::build(2, 2, vec![0, 0, 0, 0], key, 8).expect("well formed");
    assert_eq!(empty.code_at(1, 0, 0), Some(0));
}

/// **A categorical product gets exactly one level**, with no branch and no cfg.
#[test]
fn a_categorical_plane_is_one_level() {
    let key = LutKey::identity(RadarProduct::HydrometeorClassification);
    let plane = CodePlane::build(360, 920, woven_codes(360, 920, 7), key, 8).expect("well formed");
    assert_eq!(plane.reduce(), Reduce::None);
    assert_eq!(
        plane.levels(),
        1,
        "hydrometeor class codes are ordinally meaningless, so a maximum over them would name \
         a class nobody measured",
    );
    assert_eq!(plane.resident_bytes(), 360 * 920);
    assert!(plane.level(1).is_none());
}

/// **A hostile payload is refused and counted** — never truncated, never a
/// panic, and never an allocation sized by the claim.
#[test]
fn a_hostile_polar_payload_is_refused() {
    let _ledger = super::hold_refusal_ledger();
    let key = LutKey::identity(RadarProduct::Reflectivity);
    let before = CodePlane::refusals();

    // The 60,000-gate radial `types.rs` already documents for the raster.
    assert_eq!(
        CodePlane::build(720, 60_000, Vec::new(), key, 8),
        Err(PlaneRefusal::Shape {
            radials: 720,
            gates: 60_000
        }),
    );
    // Four thousand radials, past twice what the RDA can declare.
    assert_eq!(
        CodePlane::build(4_000, 1_832, Vec::new(), key, 8),
        Err(PlaneRefusal::Shape {
            radials: 4_000,
            gates: 1_832
        }),
    );
    // Degenerate shapes.
    assert!(matches!(
        CodePlane::build(0, 1_832, Vec::new(), key, 8),
        Err(PlaneRefusal::Shape { .. })
    ));
    assert!(matches!(
        CodePlane::build(720, 0, Vec::new(), key, 8),
        Err(PlaneRefusal::Shape { .. })
    ));
    // A code buffer that does not match the shape it claims.
    // The arm that matters more: a buffer LONGER than the shape would build
    // happily without the check, quietly dropping the tail. A short buffer at
    // least fails loudly on the first read past its end; a long one paints a
    // sweep whose last gates were never looked at.
    assert_eq!(
        CodePlane::build(4, 4, vec![0; 20], key, 8),
        Err(PlaneRefusal::CodeCount { got: 20, want: 16 }),
        "a payload longer than its declared shape must be refused, not silently truncated",
    );
    assert_eq!(
        CodePlane::build(4, 4, vec![0; 15], key, 8),
        Err(PlaneRefusal::CodeCount { got: 15, want: 16 }),
    );

    assert_eq!(
        CodePlane::refusals() - before,
        6,
        "every refusal must reach the always-on ledger, or a hostile sweep in a running app \
         leaves no evidence it arrived",
    );

    // The caps are live rather than decorative: the widest real cut is inside
    // them and one step past the cap is out.
    assert!(CodePlane::build(720, 1_832, vec![0; 720 * 1_832], key, 8).is_ok());
    assert!(matches!(
        CodePlane::build(720, MAX_POLAR_GATES + 1, Vec::new(), key, 8),
        Err(PlaneRefusal::Shape { .. })
    ));
}

/// **A product an R8 plane cannot carry never gets one.**
///
/// The fidelity finding, enforced at construction rather than left to a
/// reader. `docs/radar-polar-design.md` §7 schedules NROT and SRV onto R8
/// planes and its §2.4 reduce table adds KDP, VIL, EchoTops and VIL density;
/// all six are refused here, so following the document cannot produce the
/// regression.
#[test]
fn a_product_an_r8_plane_cannot_carry_is_refused() {
    // This test refuses payloads too, so it is one of the neighbours whose
    // bumps a delta-reading test would otherwise attribute to itself.
    let _ledger = super::hold_refusal_ledger();
    let mut refused = Vec::new();
    let mut admitted = Vec::new();
    for &product in RadarProduct::all() {
        let key = LutKey::identity(product);
        let built = CodePlane::build(4, 4, vec![0; 16], key, 8);
        match r8_fidelity(product) {
            R8Fidelity::WireByteExact | R8Fidelity::ComputedPaletteFits => {
                assert!(
                    built.is_ok(),
                    "{product:?} is exact at eight bits and was refused"
                );
                admitted.push(product);
            }
            R8Fidelity::ExactOnEightBitWireOnly => {
                assert!(built.is_ok(), "{product:?} is exact on the 8-bit wire form");
                // ...and refused on the wide one, which is the whole point of
                // the verdict being conditional.
                assert_eq!(
                    CodePlane::build(4, 4, vec![0; 16], key, 16),
                    Err(PlaneRefusal::WideWireWord {
                        product,
                        word_bits: 16
                    }),
                );
                admitted.push(product);
            }
            fidelity => {
                assert_eq!(
                    built,
                    Err(PlaneRefusal::NotRepresentable { product, fidelity }),
                    "{product:?} cannot survive eight bits and must be refused, not quantised",
                );
                refused.push(product);
            }
        }
    }
    // The six the design would have put on a plane are all in the refused set.
    for product in [
        RadarProduct::NormalizedRotation,
        RadarProduct::StormRelativeVelocity,
        RadarProduct::SpecificDifferentialPhase,
        RadarProduct::VerticallyIntegratedLiquid,
        RadarProduct::EchoTops,
        RadarProduct::VilDensity,
    ] {
        assert!(
            refused.contains(&product),
            "{product:?} is one of the six `docs/radar-polar-design.md` schedules onto an R8 \
             plane, and it is measurably lossy there",
        );
    }
    assert_eq!(refused.len(), 9, "{refused:?}");
    assert_eq!(admitted.len(), 8, "{admitted:?}");
    assert_eq!(refused.len() + admitted.len(), RadarProduct::all().len());
}

/// **What a real surveillance sweep's plane actually costs, in process.**
///
/// The campaign has been quoting `squallar_device_profile`'s arithmetic over
/// constants. This is the object: a plane built at the shape a WSR-88D
/// surveillance cut really declares (720 radials x 1832 gates, the shape
/// `types/tests.rs` names), with its byte total read off the vectors it
/// allocated rather than recomputed from the shape it was asked for.
///
/// Content does not enter the figure — only shape does — so this is exact for
/// **any** reflectivity sweep of that cut, not an estimate from one volume.
#[test]
fn a_surveillance_sweeps_plane_costs_what_the_chain_sums_to() {
    const RADIALS: usize = 720;
    const GATES: usize = 1_832;
    let key = LutKey {
        product: RadarProduct::Reflectivity,
        scale: 2.0,
        offset: 66.0,
    };
    let plane = CodePlane::build(RADIALS, GATES, woven_codes(RADIALS, GATES, 11), key, 8)
        .expect("the real surveillance shape is inside both caps");

    // The chain summed the long way, level by level, from the plane's own
    // levels rather than from a closed form.
    let summed: usize = (0..plane.levels())
        .map(|l| {
            let (bytes, r, g) = plane.level(l).expect("inside the chain");
            assert_eq!(bytes.len(), r * g);
            bytes.len()
        })
        .sum();
    assert_eq!(plane.resident_bytes(), summed);
    assert_eq!(plane.levels(), 11);
    assert_eq!(
        plane.resident_bytes(),
        1_758_630,
        "the measured plane and the price's arithmetic must agree, or one of them is wrong \
         about the representation",
    );
    // Level 0 alone, which is what a still pane retains for a readout.
    assert_eq!(plane.level(0).expect("level 0").0.len(), 1_319_040);
    // And the table that colours it, per sweep key rather than per frame.
    assert_eq!(Lut::build(key).to_rgba_bytes().len(), 1_024);
}

// ── The wire form ────────────────────────────────────────────────────────────

/// The fixture the wire tests pin: a small plane with both sentinels and real
/// measurements, at a shape whose chain is more than one level.
fn a_wire_fixture() -> CodePlane {
    CodePlane::build(
        3,
        4,
        vec![0, 1, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49],
        LutKey {
            product: RadarProduct::Reflectivity,
            scale: 2.0,
            offset: 66.0,
        },
        8,
    )
    .expect("the fixture is inside every cap and its product is exact on R8")
}

/// **The bytes a plane puts on the wire are these bytes.**
///
/// Digest suite in the shape `render_input/tests.rs` and `render/polar/tests.rs`
/// use — length and layout digest over a literal fixture — because the two
/// things that can silently break a wire form are a reordered field and a
/// retyped one, and neither moves a round-trip: the encoder and the decoder
/// are one codec, so a symmetric change is invisible to a parity test. The
/// same reasoning `wire_identity::WIRE_HEIGHT_REPLY_ROWS` records.
///
/// The head and the tail are pinned **separately** because that is how they
/// travel: the head is scalars in the reply's `OUT` block, the tail is its own
/// transferable buffer starting at offset zero.
#[test]
fn the_plane_wire_layout_is_the_one_this_protocol_ships() {
    let plane = a_wire_fixture();

    let mut head = Vec::new();
    plane.write_wire_head(&mut head);
    assert_eq!(
        (head.len(), crate::wire::layout_digest(&head)),
        (19, 0xe6e4_4710_6e47_1997),
        "the head block `write_wire_head` writes moved. A field was added, \
         removed, reordered or retyped -- and because `from_wire` is the same \
         codec read backwards, nothing else in this crate can see that. The \
         block is `CodePlane::build`'s own arguments but the codes: radials \
         u32, gates u32, product u16, scale f32, offset f32, word_bits u8.",
    );
    assert_eq!(
        head.len(),
        CodePlane::WIRE_HEAD_BYTES,
        "the constant and the writer disagree about the block's width",
    );

    let tail = plane.to_bytes();
    assert_eq!(
        (tail.len(), crate::wire::layout_digest(&tail)),
        (12, 0x6c2b_85b6_2288_e8a5),
        "the code tail moved",
    );
}

/// **A plane survives its own wire form, chain and all.**
///
/// Over every product an R8 plane admits, at both wire word sizes and at
/// shapes that exercise the odd-dimension halving, because the chain is
/// rebuilt on the far side rather than carried and a rebuild that disagreed
/// with the original would be a different picture at every zoom but the
/// closest.
#[test]
fn a_plane_survives_its_own_wire_form() {
    let mut checked = 0usize;
    for &product in RadarProduct::all() {
        for &(scale, offset) in KEYS {
            for &(radials, gates) in &[(1usize, 1usize), (3, 4), (7, 5), (2, 9)] {
                for word_bits in [8u8, 16] {
                    let key = LutKey {
                        product,
                        scale,
                        offset,
                    };
                    let codes = woven_codes(radials, gates, 3);
                    let Ok(plane) = CodePlane::build(radials, gates, codes, key, word_bits) else {
                        continue;
                    };

                    let mut head = Vec::new();
                    plane.write_wire_head(&mut head);
                    let tail = plane.to_bytes();
                    let mut r = crate::wire::Reader::new(&head);
                    let back = CodePlane::from_wire(&mut r, tail.clone())
                        .expect("a plane this build wrote is a plane this build reads");
                    assert!(r.at_end(), "the head block left bytes unread");
                    // Compared as BYTES rather than as values, because
                    // `KEYS` carries a NaN scale on purpose and NaN is not
                    // equal to itself: a value comparison would report every
                    // non-finite key as a round-trip failure while the bits
                    // crossed intact. The struct comparison below covers
                    // everything that can be compared as a value.
                    let mut back_head = Vec::new();
                    back.write_wire_head(&mut back_head);
                    assert_eq!(
                        (&back_head, &back.to_bytes()),
                        (&head, &tail),
                        "{product:?} at ({scale}, {offset}) {radials}x{gates} \
                         w{word_bits} did not survive its own wire form",
                    );
                    if !scale.is_nan() && !offset.is_nan() {
                        assert_eq!(
                            back, plane,
                            "{product:?} at ({scale}, {offset}) \
                             {radials}x{gates} w{word_bits}: the rebuilt \
                             plane is not the one that was sent",
                        );
                    }
                    // The provenance byte is carried, not inferred: nothing in
                    // the plane could re-derive it, and inferring it would be
                    // a second opinion about which sweeps are representable.
                    assert_eq!(back.word_bits(), word_bits);
                    checked += 1;
                }
            }
        }
    }
    assert!(
        checked >= 100,
        "the sweep degenerated to {checked} planes; a round-trip that round-trips \
         nothing is not evidence",
    );
}

/// **The mip chain is rebuilt on the far side, not sent.**
///
/// The tail is level 0 alone, so what crosses is strictly smaller than the
/// object built from it — and a hostile payload cannot supply a chain that
/// disagrees with the codes beneath it, because there is no chain to supply.
#[test]
fn the_wire_carries_level_zero_and_the_chain_is_rebuilt() {
    let plane = a_wire_fixture();
    let (radials, gates) = plane.shape();
    assert_eq!(plane.to_bytes().len(), radials * gates);
    assert!(
        plane.levels() > 1,
        "premise: this fixture has a chain to leave behind",
    );
    assert!(
        plane.to_bytes().len() < plane.resident_bytes(),
        "the tail is the whole object, so nothing is being rebuilt",
    );
    // And the two spellings of the tail are the same bytes: one borrows, one
    // moves, and the encoder uses the moving one so a reply never holds two
    // copies of a megabyte-plus buffer.
    assert_eq!(plane.to_bytes(), plane.clone().into_codes());
}

/// **A doctored plane payload is refused, and counted where the producer
/// counts.**
///
/// The decode goes through `CodePlane::build`, so the caps, the code-count
/// check and the eight lossy products' refusal are the producer's own rather
/// than a second opinion the wire grew — and the evidence a hostile sweep
/// arrived lands in the one always-on ledger.
#[test]
fn a_doctored_plane_payload_is_refused_and_counted() {
    let _ledger = super::hold_refusal_ledger();
    let plane = a_wire_fixture();
    let mut head = Vec::new();
    plane.write_wire_head(&mut head);
    let tail = plane.to_bytes();

    // Control: the untouched pair decodes and costs no refusal.
    let before = CodePlane::refusals();
    assert!(CodePlane::from_wire(&mut crate::wire::Reader::new(&head), tail.clone()).is_some());
    assert_eq!(CodePlane::refusals(), before, "a good payload was counted");

    let counted = |what: &str, head: &[u8], tail: Vec<u8>| {
        let before = CodePlane::refusals();
        assert_eq!(
            CodePlane::from_wire(&mut crate::wire::Reader::new(head), tail),
            None,
            "{what} was accepted",
        );
        assert_eq!(
            CodePlane::refusals(),
            before + 1,
            "{what} was refused somewhere other than the producer's own constructor",
        );
    };

    counted("a code buffer shorter than its declared shape", &head, {
        let mut short = tail.clone();
        short.pop();
        short
    });
    counted("a code buffer longer than its declared shape", &head, {
        let mut long = tail.clone();
        long.push(0);
        long
    });
    counted(
        "a radial count past the cap",
        &{
            let mut h = head.clone();
            h[0..4].copy_from_slice(&(MAX_POLAR_RADIALS as u32 + 1).to_le_bytes());
            h
        },
        tail.clone(),
    );
    counted(
        "a gate count past the cap",
        &{
            let mut h = head.clone();
            h[4..8].copy_from_slice(&(MAX_POLAR_GATES as u32 + 1).to_le_bytes());
            h
        },
        tail.clone(),
    );
    counted(
        "a product an R8 plane cannot carry",
        &{
            let mut h = head.clone();
            h[8..10].copy_from_slice(&RadarProduct::NormalizedRotation.wire_code().to_le_bytes());
            h
        },
        tail.clone(),
    );
    counted(
        "a wide wire word for a product that is exact only at eight bits",
        &{
            let mut h = head.clone();
            h[8..10].copy_from_slice(
                &RadarProduct::DifferentialReflectivity
                    .wire_code()
                    .to_le_bytes(),
            );
            h[18] = 16;
            h
        },
        tail.clone(),
    );

    // A product code this build does not know cannot reach the constructor --
    // there is no key to build with -- so it is refused WITHOUT reaching the
    // ledger. Named here rather than left as a hole someone finds later.
    let before = CodePlane::refusals();
    let mut unknown = head.clone();
    unknown[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
    assert_eq!(
        CodePlane::from_wire(&mut crate::wire::Reader::new(&unknown), tail.clone()),
        None,
        "an unknown product wire code was accepted",
    );
    assert_eq!(
        CodePlane::refusals(),
        before,
        "an unreadable head reached the plane constructor",
    );

    // A truncated head is refused at every cut, and none of them is a plane.
    let before = CodePlane::refusals();
    for cut in 0..head.len() {
        assert_eq!(
            CodePlane::from_wire(&mut crate::wire::Reader::new(&head[..cut]), tail.clone()),
            None,
            "the plane head truncated to {cut} bytes was accepted",
        );
    }
    assert_eq!(
        CodePlane::refusals(),
        before,
        "a head that ran out mid-field reached the plane constructor",
    );
}
