use super::*;
use nexrad_model::data::{PulseWidth, Scan, VolumeCoveragePattern};
// Explicit rather than through `super::*`: `types.rs` itself stopped naming
// the type when `unit_label`'s MEHS arm moved to `Quantity::suffix` at M4.
use rustdar_units::HailSizeUnit;

/// **Both** arms of the [`IMAGE_SIZE`] cascade, unconditionally.
///
/// The arm a host build does not compile is the one nothing else would
/// catch: the audit that prompted this changed the wasm literal from 1024 to
/// 4096 on a pristine tree and the whole workspace passed 1508/0 with the
/// wasm `cargo check` exiting 0. `sampler.rs`'s
/// `the_cos_e_correction_is_worth_a_measured_number_of_pixels`
/// looks like it covers this and does not — its `if cfg!(…)` picks the
/// running target's literal, so the other one is dead text.
///
/// Every property here is one the render path depends on, not a restatement
/// of the literals for its own sake; the literals are pinned separately at
/// the end so that editing a value *and* its consequences in step still has
/// to be deliberate.
#[test]
fn both_image_size_arms_are_pinned_not_just_the_compiled_one() {
    for (target, size) in [("wasm32", WASM_IMAGE_SIZE), ("native", NATIVE_IMAGE_SIZE)] {
        // `render::project` indexes `py * IMAGE_SIZE + px` into a single
        // allocation and `ImageBounds` divides the extent by it, so zero is
        // a division by zero before it is an empty image.
        assert!(size > 0, "{target}");
        // Not a power of two because anything requires it — that claim was
        // made here and in `constants.rs`, checked, and found false; a real
        // surveillance cut is rasterized at 7362 px, which is not even a
        // multiple of four, and the projection, the gate loop and the texture
        // upload all take it unchanged. Both arms are powers of two because
        // both are `2 · BASE_EXTENT_KM` at the scale this display is
        // calibrated at, which is a fact about the number and worth pinning
        // as one.
        assert!(size.is_power_of_two(), "{target}: {size}");
        // A browser may legitimately report exactly the WebGL2 guarantee, so
        // *both* arms have to fit it. This is the assertion the 4096 mutation
        // trips. The web arm is allowed to sit exactly on the line: the
        // guarantee bounds each texture's each axis, and the overlays beside
        // the radar frame are sized from the viewport and clamped against the
        // same limit separately (`plan_overlay_texture`), so there was never a
        // sum for the radar frame to leave room in.
        assert!(
            size <= WEBGL2_MAX_TEXTURE_DIMENSION_2D,
            "the {target} image is {size} px, over the \
                 {WEBGL2_MAX_TEXTURE_DIMENSION_2D} px 2D texture size WebGL2 guarantees"
        );
    }

    // The two arms are the same size *today*, and the equality is asserted
    // rather than collapsed because they are not the same decision: native's
    // is what 230 km costs at 4.45 px/km, and the web's is the largest
    // texture WebGL2 guarantees. `LOOP_TEXTURE_BUDGET_BYTES`' frame figures
    // are computed off the loop sizes, which do still differ.
    assert_eq!(WASM_IMAGE_SIZE, 2048);
    assert_eq!(NATIVE_IMAGE_SIZE, 2048);
    assert_eq!(WEBGL2_MAX_TEXTURE_DIMENSION_2D, 2048);

    // And that this target's cascade selected the matching arm. This half
    // *is* `cfg`-gated, because the selection is the one thing here that no
    // other target can check on its behalf.
    #[cfg(target_arch = "wasm32")]
    assert_eq!(IMAGE_SIZE, WASM_IMAGE_SIZE);
    #[cfg(not(target_arch = "wasm32"))]
    assert_eq!(IMAGE_SIZE, NATIVE_IMAGE_SIZE);
}

/// Each `cfg` arm of the cascade selects the constant named for it.
///
/// The assertion at the end of the test above is `cfg`-gated and so covers
/// only the arm this build compiles. Pointing the wasm32 arm at
/// `NATIVE_IMAGE_SIZE` leaves the whole workspace passing and the wasm
/// `cargo check` exiting 0 — measured, not assumed; it was one of two
/// mutations that survived the probe run that landed these tests. Nothing
/// on a host evaluates that line, so the line is read rather than run.
#[test]
fn each_cfg_arm_selects_the_image_size_named_for_it() {
    let source = include_str!("../types.rs");
    // The shipped half only: the strings below appear verbatim in this
    // test's own source, so scanning the whole file would find them here.
    let (code, _) = source
        .split_once("#[cfg(test)]")
        .expect("types.rs no longer has a test module");

    for (cfg, arm) in [
        (r#"#[cfg(target_arch = "wasm32")]"#, "WASM_IMAGE_SIZE"),
        (
            r#"#[cfg(not(target_arch = "wasm32"))]"#,
            "NATIVE_IMAGE_SIZE",
        ),
    ] {
        // The definition under this `cfg`, counted before it is read: two
        // would mean whichever came first is what got checked, and a decoy
        // in a doc comment or a string literal would be a second.
        let definition = format!("{cfg}\npub const IMAGE_SIZE: usize =");
        let occurrences = code.matches(&definition).count();
        assert_eq!(
            occurrences, 1,
            "expected exactly one IMAGE_SIZE definition under `{cfg}`, \
                 found {occurrences}"
        );
        let at = code.find(&definition).expect("just counted one");
        let (selected, _) = code[at + definition.len()..]
            .split_once(';')
            .expect("a const definition with no semicolon");
        assert_eq!(
            selected.trim(),
            arm,
            "the `{cfg}` arm selects `{}`, not `{arm}`, so that build does \
                 not get the image size named for it — and no host build can \
                 evaluate the line.",
            selected.trim()
        );
    }
}

/// A 250 m gate has a pixel of its own at [`BASE_EXTENT_KM`], on both arms.
///
/// `MercatorProjection::px_per_km` is the whole rasterizer's scale factor, and
/// it is `IMAGE_SIZE / (2 · extent)` per render rather than a constant — so the
/// property worth pinning is not the arithmetic (which the projection does in
/// one line) but that the *coarser* arm still resolves a gate at the extent the
/// base texture is calibrated for. Below two pixels per kilometre a 250 m gate
/// stops landing in a pixel of its own and the display starts dropping gates
/// rather than drawing them small.
///
/// This is now a **worst case over every short render**, not one point on the
/// curve. [`raster_side_px`] hands the base texture to every extent at or
/// inside this one, so px/km rises as the extent falls: a TDWR Doppler sweep
/// reaching 88.8 km gets 11.53 px/km out of the same 2048 pixels. The bound
/// binds where it is loosest, which is exactly here.
#[test]
fn a_quarter_kilometre_gate_still_gets_its_own_pixel_at_the_base_extent() {
    for size in [WASM_IMAGE_SIZE, NATIVE_IMAGE_SIZE] {
        let scale = size as f64 / (2.0 * BASE_EXTENT_KM);
        assert!(scale > 2.0, "{size} px gives {scale:.3} px/km");
    }
}

/// What each reach is drawn at, across the whole range of reaches this
/// display can be handed.
///
/// **Every row that states a reach is drawn at that reach.** The rows carry
/// the reaches real radars really produce, decoded from real volumes rather
/// than rounded, because the claim is about data and not about arithmetic: a
/// WSR-88D's Doppler cut is 2.125 + 1192 × 0.25 and a TDWR's is 0 + 592 × 0.15,
/// and the display has no business drawing either of them anywhere else.
///
/// The two rows that do *not* state a reach — 0.0 and a negative — are the
/// fallback's whole membership. See
/// `an_unstated_reach_is_the_only_way_to_reach_the_fallback_extent`.
#[test]
fn the_extent_is_the_reach_the_data_states_bounded_only_by_the_cap() {
    for (reach, extent, why) in [
        (0.0, FALLBACK_EXTENT_KM, "a product no radial carries"),
        (-1.0, FALLBACK_EXTENT_KM, "a negative reach"),
        (10.0, 10.0, "a 40-gate Level III packet"),
        (
            88.797,
            88.797,
            "every TDWR Doppler moment: 592 × 0.15 km, measured at TOKC, \
             TDAL, TPIT and TATL",
        ),
        (
            208.125,
            208.125,
            "KCBW's 5.05° cut, 824 × 0.25 km — the first tilt that used to be \
             raised to the floor",
        ),
        (229.9, 229.9, "just inside the old floor"),
        (230.0, 230.0, "exactly where the old floor stood"),
        (230.1, 230.1, "just past it"),
        (249.125, 249.125, "KCBW's 3.96° cut, 988 × 0.25 km"),
        (
            300.125,
            300.125,
            "every WSR-88D Doppler cut: 2.125 + 1192 × 0.25 km",
        ),
        (
            416.985,
            416.985,
            "TDWR long-range reflectivity, 1390 × 0.3 km on a 0.48° cut",
        ),
        (
            460.125,
            460.125,
            "WSR-88D surveillance: 2.125 + 1832 × 0.25 km",
        ),
        (470.0, MAX_EXTENT_KM, "exactly the cap"),
        (12_000.0, MAX_EXTENT_KM, "a mis-framed gate count"),
        (f64::INFINITY, MAX_EXTENT_KM, "an infinite reach"),
    ] {
        assert_eq!(
            plan_view_extent_km(reach),
            extent,
            "{reach} km ({why}) must be drawn at {extent} km",
        );
    }
    // A `NaN` reach is the one input the bound would pass straight through, and
    // an unplaceable raster is a worse answer than a too-wide one.
    assert_eq!(plan_view_extent_km(f64::NAN), FALLBACK_EXTENT_KM);
}

/// The fallback is unreachable from any sweep that says how far it reached.
///
/// [`FALLBACK_EXTENT_KM`] and [`BASE_EXTENT_KM`] hold the same number, so the
/// table above cannot tell "the fallback fired" from "the data reached 230 km".
/// This can: it sweeps the whole positive range finely and asserts the extent
/// **is the reach itself** everywhere under the cap, which leaves no input
/// except a `NaN` or a non-positive one that can produce the fallback at all.
///
/// The step is 0.05 km rather than something round so the walk cannot skip past
/// [`BASE_EXTENT_KM`] and cannot land on it exactly, and both are checked
/// separately underneath.
#[test]
fn an_unstated_reach_is_the_only_way_to_reach_the_fallback_extent() {
    let mut reach = 0.05_f64;
    while reach < MAX_EXTENT_KM {
        assert_eq!(
            plan_view_extent_km(reach),
            reach,
            "a sweep reaching {reach} km must be drawn at {reach} km",
        );
        reach += 0.05;
    }

    // The two values that used to be indistinguishable from a fallback.
    assert_eq!(plan_view_extent_km(BASE_EXTENT_KM), BASE_EXTENT_KM);
    assert_eq!(plan_view_extent_km(FALLBACK_EXTENT_KM), FALLBACK_EXTENT_KM);

    // And the fallback's entire membership, stated as a closed set.
    for unstated in [0.0, -0.0, -1.0, -MAX_EXTENT_KM, f64::NEG_INFINITY, f64::NAN] {
        assert_eq!(
            plan_view_extent_km(unstated),
            FALLBACK_EXTENT_KM,
            "{unstated} is not a reach and must answer the fallback",
        );
    }
}

/// How many pixels each extent gets, at each ceiling a caller in this
/// workspace passes.
///
/// The first block is the guarantee: at or under [`BASE_EXTENT_KM`] the answer
/// is [`IMAGE_SIZE`] whatever ceiling is offered, so a derived 1° × 1 km grid,
/// a fetched Level III product, a TDWR Doppler moment and every tilt from about
/// 5° up are drawn on the base texture. What that texture now *buys* them has
/// changed — the extent is their own reach rather than a 230 km frame, so the
/// same pixels land on less ground and resolve it more finely — but the side
/// itself has not moved, which is why nothing in that block is affected by the
/// device having an answer at all. A Doppler cut on a WSR-88D is **not** in
/// that block —
/// it reaches 300.125 km — and its rows sit in the second and third instead.
/// The last block is the mechanism the device gate and the loop policy both
/// use: a ceiling is a ceiling, and one at or below the base size fixes the
/// side rather than being ignored.
#[test]
fn the_side_follows_the_extent_up_to_the_ceiling_the_caller_owns() {
    const LONG_RANGE: usize = 4096;
    // The gate spacing every row is sampled at unless it says otherwise. A
    // quarter-kilometre gate asks for more pixels than `LONG_RANGE` offers at
    // every extent in this table, so the ceiling is what binds and these rows
    // say what they always said. The rows where the *data* binds instead are
    // `a_raster_is_never_asked_for_more_texels_than_its_samples_can_fill`.
    const SUPER_RES_GATE_KM: f64 = 0.25;
    for (extent, ceiling, side, why) in [
        // At and below the reference extent: the base size, whatever is on
        // offer.
        (0.0, LONG_RANGE, IMAGE_SIZE, "a product no radial carries"),
        (88.797, LONG_RANGE, IMAGE_SIZE, "a TDWR Doppler moment"),
        (208.125, LONG_RANGE, IMAGE_SIZE, "KCBW's 5.05° cut"),
        (
            BASE_EXTENT_KM,
            LONG_RANGE,
            IMAGE_SIZE,
            "exactly the reference extent, with 4096 offered",
        ),
        // Past it: the ceiling, because there is now ground to spend it on —
        // once there is enough data to spend it *with*. A tenth of a kilometre
        // past the reference extent there is not: 460.2 km of ground sampled
        // every 0.25 km carries 3682 px at two texels a gate, and the raster
        // stops there rather than taking the whole 4096. That used to be a
        // cliff — 2048 px at 230.0 km and 4096 at 230.1 — and it is now a
        // continuous rise, which is the same fact seen from the other side.
        (
            230.1,
            LONG_RANGE,
            3682,
            "one tenth of a kilometre past the reference extent",
        ),
        (300.125, LONG_RANGE, LONG_RANGE, "a WSR-88D Doppler cut"),
        (417.0, LONG_RANGE, LONG_RANGE, "a TDWR long-range cut"),
        (
            460.125,
            LONG_RANGE,
            LONG_RANGE,
            "a WSR-88D surveillance cut",
        ),
        (MAX_EXTENT_KM, LONG_RANGE, LONG_RANGE, "the cap"),
        // A ceiling at the base size fixes the side outright: this is what a
        // browser offers, what a device whose textures stop at 2048 asks for,
        // and what every caller with no GPU to consult gets by default. The
        // extent is untouched by it, so these two rows are the same 2048
        // pixels over 300 and 460 km of ground — see `raster_side_px`.
        (300.125, IMAGE_SIZE, IMAGE_SIZE, "a Doppler cut on the web"),
        (
            460.125,
            IMAGE_SIZE,
            IMAGE_SIZE,
            "the base size as the ceiling",
        ),
        (
            BASE_EXTENT_KM,
            IMAGE_SIZE,
            IMAGE_SIZE,
            "the reference extent, ceiling == base",
        ),
        // And a ceiling *below* it is honoured rather than clamped away —
        // the web's loop frames, which are deliberately leaner than its
        // static renders.
        (
            BASE_EXTENT_KM,
            1024,
            1024,
            "a loop frame at the reference extent",
        ),
        (460.125, 1024, 1024, "a loop frame of a surveillance cut"),
    ] {
        assert_eq!(
            raster_side_px(extent, ceiling, SUPER_RES_GATE_KM),
            side,
            "{extent} km under a {ceiling} px ceiling ({why})",
        );
    }
}

/// A raster is never asked for more texels than its own samples can fill, and
/// never for fewer than this display's calibrated scale.
///
/// The ceiling the caller owns says what the *device* can hold; this says what
/// the *data* is worth, and the smaller of the two is what gets painted. Both
/// halves are asserted because each alone is a different bug: without the
/// Nyquist term a device that reports 32768 would be handed 16384 px of a
/// surveillance cut and spend three quarters of them interpolating between
/// gates, and without the calibrated-scale floor a 1 km volumetric grid would
/// be *narrowed* to 1840 px and lose the azimuthal detail it has today.
///
/// Every extent and spacing here is measured off a real volume through this
/// crate — KDMX, KCRP, KFTG, KATX, KPDT and KTLX for the WSR-88D rows, TORD
/// 2020-08-10 for the TDWR one.
#[test]
fn a_raster_is_never_asked_for_more_texels_than_its_samples_can_fill() {
    // A ceiling far above anything either term asks for, so that these rows
    // report the data's own answer rather than a device's.
    const UNBOUNDED: usize = 1 << 20;
    for (extent, sample, want, why) in [
        // The Nyquist term binds: fine gates over a lot of ground.
        (460.11, 0.25, 7362, "a WSR-88D surveillance cut, 1832 gates"),
        (300.11, 0.25, 4802, "a WSR-88D Doppler cut, 1192 gates"),
        (417.00, 0.30, 5560, "a TDWR long-range reflectivity cut"),
        // The calibrated-scale term binds: a coarsely sampled field, which at
        // two texels a bin would ask for only 1840 px.
        (460.00, 1.00, 4096, "a 1 km volumetric grid over 460 km"),
        // Exactly twice the reference extent is exactly twice the base side,
        // which is what "the scale does not move" means arithmetically.
        (
            BASE_EXTENT_KM,
            1.00,
            IMAGE_SIZE,
            "the reference extent itself",
        ),
        // Nothing to divide by: the sampling term says nothing and the scale
        // term answers alone, rather than a division carrying an infinity or a
        // `NaN` into the side.
        (460.00, 0.0, 4096, "a field that states no spacing"),
        (460.00, -1.0, 4096, "a negative spacing"),
        (460.00, f64::NAN, 4096, "a spacing that is not a number"),
        (460.00, f64::INFINITY, 4096, "an infinite spacing"),
    ] {
        assert_eq!(
            data_limited_side_px(extent, sample),
            want,
            "±{extent} km sampled every {sample} km ({why})",
        );
        // And what the two together answer is never more than the data's own
        // figure — which is the property `raster_side_px` is built on. Inside
        // the reference extent the base branch answers instead, so the rows
        // that sit there compare against the base size.
        let expected = if extent > BASE_EXTENT_KM {
            want
        } else {
            IMAGE_SIZE
        };
        assert_eq!(
            raster_side_px(extent, UNBOUNDED, sample),
            expected,
            "±{extent} km under an unbounded ceiling ({why})",
        );
    }

    // A degenerate extent paints nothing, and must still answer a side a
    // buffer can be allocated at rather than zero.
    assert!(data_limited_side_px(0.0, 0.25) >= 1);
    assert!(data_limited_side_px(-5.0, 0.25) >= 1);
}

/// A sweep that stops short keeps the whole base texture and spends it on the
/// ground it actually covers.
///
/// This is [`BASE_EXTENT_KM`]'s surviving role and the other half of the range
/// ring fix. The extent following the data would be worth much less if the
/// *side* followed it too — the picture would be the same coarse thing on a
/// smaller canvas. It does not: [`raster_side_px`] hands [`IMAGE_SIZE`] to
/// every extent at or inside the reference one, so the pixels a short sweep
/// used to spend on empty corners are spent on the sweep instead.
///
/// The figures are the measured TDWR Doppler case. A 88.797 km reach inside a
/// 230 km frame put `π · 88.797² / 460²` = 11.7% of the raster over data and
/// left the other 88.3% permanently empty; at its own extent the disc is `π/4`
/// of the raster. The ratio of those is the pixel budget the change recovers,
/// and it is asserted rather than described.
#[test]
fn a_short_sweep_gets_the_base_texture_over_its_own_ground() {
    const TDWR_DOPPLER_KM: f64 = 88.797;

    let extent = plan_view_extent_km(TDWR_DOPPLER_KM);
    assert_eq!(extent, TDWR_DOPPLER_KM);

    // The side did not move, so the whole texture is still paid for.
    // A TDWR's Doppler gates are 0.15 km, which over this reach asks for 2368
    // px — but the sweep is inside the reference extent, so the base branch
    // answers and neither the ceiling nor the sampling moves it.
    let side = raster_side_px(extent, IMAGE_SIZE, 0.15);
    assert_eq!(side, IMAGE_SIZE);

    // And the scale it buys, against what the same sweep used to get.
    let now = side as f64 / (2.0 * extent);
    let before = side as f64 / (2.0 * BASE_EXTENT_KM);
    assert!(
        (now / before - BASE_EXTENT_KM / TDWR_DOPPLER_KM).abs() < 1e-9,
        "{now:.4} px/km against {before:.4}",
    );
    assert!(
        now > 11.5 && now < 11.6,
        "a TDWR Doppler sweep should resolve at ~11.53 px/km, got {now:.4}",
    );

    // A 0.15 km TDWR Doppler gate was sub-pixel at the old frame and is not
    // now, which is the quality half of the same fact.
    const TDWR_DOPPLER_GATE_KM: f64 = 0.15;
    assert!(
        TDWR_DOPPLER_GATE_KM * before < 1.0,
        "the premise is that the gate used to be sub-pixel",
    );
    assert!(
        TDWR_DOPPLER_GATE_KM * now > 1.5,
        "the gate must now cover more than a pixel of its own",
    );

    // The share of the raster that can carry data, both ways round.
    let covered = |ext: f64| std::f64::consts::PI * TDWR_DOPPLER_KM.powi(2) / (4.0 * ext * ext);
    assert!(
        covered(BASE_EXTENT_KM) < 0.12,
        "under 12% of the old frame could ever be painted, not {:.3}",
        covered(BASE_EXTENT_KM),
    );
    assert!(
        (covered(extent) - std::f64::consts::FRAC_PI_4).abs() < 1e-12,
        "a disc at its own extent fills π/4 of its raster",
    );
}

/// The bounds are the extent, in degrees, and nothing else — so a raster twice
/// as wide covers twice the ground and a 230 km one covers exactly what it
/// always did.
///
/// Both halves matter, and they are asserted to different strictnesses on
/// purpose. The scaling is checked to a relative 1e-12: the offsets are
/// recovered by subtracting a 35° anchor from a 37° bound, and that
/// cancellation alone costs ~1e-14 of the 2 °-wide difference it leaves, so a
/// tighter bar would be measuring the subtraction rather than the bounds. It
/// is still four orders inside anything a wrong extent could do — the failures
/// this guards against are ratios like 1.0 and 2.04, not 2.000000000001. The
/// floor is checked **bit for bit**, against the arithmetic spelt the way the
/// pre-extent code spelt it, because there the claim is not "close": it is
/// that nothing already on screen moved.
#[test]
fn bounds_scale_with_the_extent_and_reproduce_the_floor_exactly() {
    const LAT: f64 = 35.3333;
    const LON: f64 = -97.2778;

    let at = |extent| ImageBounds::from_radar_site(LAT, LON, extent);
    let floor = at(BASE_EXTENT_KM);
    let doubled = at(2.0 * BASE_EXTENT_KM);

    let scales = |wide: f64, narrow: f64, anchor: f64, what: &str| {
        let (wide, narrow) = (wide - anchor, narrow - anchor);
        assert!(
            (wide / narrow - 2.0).abs() < 1e-12,
            "twice the extent gave {:.12}× the {what} offset",
            wide / narrow,
        );
    };
    scales(doubled.max_lat, floor.max_lat, LAT, "latitude");
    scales(doubled.max_lon, floor.max_lon, LON, "longitude");

    // The pre-extent geometry, spelt as it was spelt: 230 km at
    // `1/KM_PER_DEGREE_LAT` degrees per km, longitude widened by the site's
    // own cosine.
    //
    // The divisor is the shared constant and not a literal, for two reasons
    // that happen to agree. It is what the pre-extent code did — this constant
    // was called `MAX_RANGE_KM` then, and the degree was already
    // `KM_PER_DEGREE_LAT`, so this is the arithmetic being reproduced rather
    // than a re-derivation of it. **`MAX_RANGE_KM` no longer exists under any
    // name**: it became `BASE_EXTENT_KM` when the extent started following the
    // sweep, and `BASE_EXTENT_KM` stopped being a floor under the extent when
    // the range ring was found still drawing at 230 km around 88.8 km of TDWR
    // Doppler data. Do not go looking for it.
    // And `rustdar-radar/tests/geodesy_one_definition.rs` scans every `.rs`
    // file in the workspace for a second spelling of the planet: a literal
    // `111.32` here would be exactly the fourth definition that guard exists
    // to refuse, and it would be one in an *assertion about the bounds*, which
    // is the worst place to keep one — the test would go on passing while the
    // production constant moved out from under it.
    let lat_offset = BASE_EXTENT_KM * (1.0 / KM_PER_DEGREE_LAT);
    let lon_offset = BASE_EXTENT_KM * (1.0 / (KM_PER_DEGREE_LAT * LAT.to_radians().cos()));
    assert_eq!(floor.min_lat.to_bits(), (LAT - lat_offset).to_bits());
    assert_eq!(floor.max_lat.to_bits(), (LAT + lat_offset).to_bits());
    assert_eq!(floor.min_lon.to_bits(), (LON - lon_offset).to_bits());
    assert_eq!(floor.max_lon.to_bits(), (LON + lon_offset).to_bits());
    assert_eq!(
        floor.mercator_y_max.to_bits(),
        lat_rad_to_mercator_y((LAT + lat_offset).to_radians()).to_bits(),
    );
    assert_eq!(
        floor.mercator_y_min.to_bits(),
        lat_rad_to_mercator_y((LAT - lat_offset).to_radians()).to_bits(),
    );
}

/// A volume with no sweeps — enough to build a `ScanInfo`, and the strongest
/// form of the case below: no product can be *discovered* from it.
fn empty_scan() -> Scan {
    Scan::new(
        VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            PulseWidth::Short,
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
    )
}

/// Every Level III product is in the selector from the moment a volume loads,
/// with an empty tilt list — not from the moment its fetch lands.
///
/// This is the availability half of "a user cannot tell which datasource a
/// product comes from". A product that materialises in the picker a second or
/// two after the scan, and again after every archive poll (which rebuilds
/// `ScanInfo` from the volume alone), is a product visibly unlike its
/// neighbours. Listed from the start, the entry is stable and the angle fills
/// in behind it; `PaneState::get_rendering_params` reads the empty list as
/// "the selection stands" so a render is dispatched immediately.
#[test]
fn every_level3_product_is_listed_from_the_moment_a_volume_loads() {
    let info = ScanInfo::from_scan(
        &empty_scan(),
        "KTLX",
        chrono::NaiveDate::from_ymd_opt(2026, 7, 26)
            .unwrap()
            .and_hms_opt(1, 48, 0)
            .unwrap(),
        None,
    );

    for product in RadarProduct::all().iter().filter(|p| p.is_level3()) {
        assert!(
            info.available_products.contains(product),
            "{} is not selectable until its fetch lands",
            product.name(),
        );
        assert_eq!(
            info.product_elevations.get(product).map(Vec::as_slice),
            Some(&[][..]),
            "{} must be listed with an empty tilt list, not absent",
            product.name(),
        );
    }

    // The volume itself carries nothing, so nothing else is listed: the
    // products above are there because they were added, not because an empty
    // scan happens to yield every product.
    assert_eq!(
        info.available_products.len(),
        RadarProduct::all().iter().filter(|p| p.is_level3()).count(),
        "a sweepless volume listed a Level II product: {:?}",
        info.available_products,
    );
}

/// A cut carrying exactly the moments named and nothing else.
///
/// [`settling_sweep`] below is reflectivity-only by construction; what the two
/// tests after this vary is the moment *set*, because which fields a volume's
/// radials carry is the whole input to what it can be asked to draw.
///
/// Three radials, not 360: `discover_product_elevations` reads the first radial
/// for its moments and the sweep's median for its label, and three is the
/// smallest count that makes "median" mean anything.
fn sweep_carrying(number: u8, elevation: f32, moments: &[MomentSlot]) -> nexrad_model::data::Sweep {
    use nexrad_model::data::{MomentData, Radial, RadialStatus, Sweep};
    // The gate geometry is irrelevant here — nothing rasterizes these — but a
    // moment has to be present or absent, and only a real block is present.
    let carried = |slot: MomentSlot| {
        moments
            .contains(&slot)
            .then(|| MomentData::from_fixed_point(600, 0, 250, 8, 2.0, 66.0, vec![200u8; 600]))
    };
    let radials = (0..3u16)
        .map(|i| {
            Radial::new(
                0,
                i,
                f32::from(i) * 120.0,
                1.0,
                RadialStatus::IntermediateRadialData,
                number,
                elevation,
                carried(MomentSlot::Reflectivity),
                carried(MomentSlot::Velocity),
                carried(MomentSlot::SpectrumWidth),
                carried(MomentSlot::DifferentialReflectivity),
                carried(MomentSlot::DifferentialPhase),
                carried(MomentSlot::CorrelationCoefficient),
                None,
            )
        })
        .collect();
    Sweep::new(number, radials)
}

/// A volume of `sweeps` under a VCP number, dated from its radials.
fn scan_of(vcp: u16, sweeps: Vec<nexrad_model::data::Sweep>) -> Scan {
    Scan::new(
        VolumeCoveragePattern::new(
            vcp,
            0,
            0.5,
            PulseWidth::Short,
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
        sweeps,
    )
}

fn a_time() -> NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 11)
        .unwrap()
        .and_hms_opt(0, 1, 39)
        .unwrap()
}

/// A TDWR offers the eight products it can draw, not the fourteen it used to.
///
/// The shape is `TPIT`'s real VCP 90, byte-verified: a reflectivity-only
/// long-range surveillance cut and a `REF`/`VEL`/`SW` Doppler cut at the same
/// nominal tilt. No dual-pol moment appears anywhere in a TDWR volume, and no
/// RPG stands behind the site, so six of the fourteen entries the picker used
/// to show could never paint a pixel: the five Level III products (no `N0K`,
/// `EET`, `DVL` or `DPR` object is generated for `PIT` — see the evidence at
/// the gate in `types.rs`) and the hybrid classification, which lists off the
/// reflectivity slot and so used to ride in on a single-pol volume's
/// reflectivity.
///
/// The eight that survive are the ones with a path to pixels: three native
/// moments, two velocity derivations, and three reflectivity-volume
/// integrations that need environmental heights but no RPG.
#[test]
fn a_tdwr_volume_offers_only_what_it_can_render() {
    // `TPIT`'s row, which is what tells `from_scan` this is a terminal radar.
    // Nothing is compiled in, so a test that did not place it would be handed
    // `UNKNOWN_SITE_NAME` and offered the whole WSR-88D product list.
    crate::sites::fixture::install();
    let scan = scan_of(
        90,
        vec![
            sweep_carrying(1, 0.26, &[MomentSlot::Reflectivity]),
            sweep_carrying(
                2,
                0.26,
                &[
                    MomentSlot::Reflectivity,
                    MomentSlot::Velocity,
                    MomentSlot::SpectrumWidth,
                ],
            ),
        ],
    );

    let info = ScanInfo::from_scan(&scan, "TPIT", a_time(), None);

    let offered: Vec<&str> = info.available_products.iter().map(|p| p.code()).collect();
    assert_eq!(
        offered,
        ["ref", "vel", "sw", "nrot", "srv", "eti", "posh", "mehs"],
        "the picker must offer only what a TDWR volume can be asked to draw",
    );
}

/// The gate is dual-pol and RPG, not "is it a WSR-88D": a legacy Message 1
/// volume hides the classification and keeps its Level III products.
///
/// Both halves matter and they fail in opposite directions. Gating the hybrid
/// classification on the *site* would leave it offered on the pre-2013 archive
/// — single-pol WSR-88D volumes, where `crate::hhc` refuses for exactly the
/// reason it refuses on a TDWR. Gating the Level III family on the volume's
/// moments would withdraw all five from any single-pol WSR-88D scan, and those
/// objects come from the RPG and do not care what the volume in hand carries.
#[test]
fn a_single_pol_wsr88d_volume_hides_hhc_but_keeps_level3() {
    let scan = scan_of(
        11,
        vec![sweep_carrying(1, 0.5, &[MomentSlot::Reflectivity])],
    );

    let info = ScanInfo::from_scan(&scan, "KTLX", a_time(), None);

    assert!(
        !info
            .available_products
            .contains(&RadarProduct::HydrometeorClassification),
        "a volume with no ΦDP and no ρHV offered a hydrometeor classification",
    );
    for product in RadarProduct::all().iter().filter(|p| p.is_level3()) {
        assert!(
            info.available_products.contains(product),
            "{} left the picker with the classification — the Level III gate \
                 is reading the volume's moments, not the site",
            product.name(),
        );
    }

    let offered: Vec<&str> = info.available_products.iter().map(|p| p.code()).collect();
    assert_eq!(
        offered,
        [
            "ref", "kdp", "eet", "eti", "vil", "vild", "posh", "mehs", "dpr"
        ],
        "a reflectivity-only WSR-88D volume offers its reflectivity \
             derivations and every Level III product, and nothing else",
    );
}

/// A sweep that opens off its own tilt while the antenna settles: the first
/// thirty radials ramp from `first` to `flown`, the rest sit on `flown`, so
/// the sweep's median is `flown` and its first radial is not.
fn settling_sweep(number: u8, first: f32, flown: f32) -> nexrad_model::data::Sweep {
    use nexrad_model::data::{MomentData, Radial, RadialStatus, Sweep};
    const RADIALS: usize = 360;
    const SETTLING: usize = 30;
    let radials = (0..RADIALS)
        .map(|i| {
            let elevation = if i < SETTLING {
                first + (flown - first) * (i as f32 / SETTLING as f32)
            } else {
                flown
            };
            Radial::new(
                0,
                i as u16,
                i as f32 * (360.0 / RADIALS as f32),
                360.0 / RADIALS as f32,
                RadialStatus::IntermediateRadialData,
                number,
                elevation,
                Some(MomentData::from_fixed_point(
                    600,
                    0,
                    250,
                    8,
                    2.0,
                    66.0,
                    vec![200u8; 600],
                )),
                None,
                None,
                None,
                None,
                None,
                None,
            )
        })
        .collect();
    Sweep::new(number, radials)
}

/// The picker's entries name the tilt each sweep **flew**, not the one it
/// happened to open on.
///
/// These labels are the values `render::find_sweep` is later handed to find
/// the sweep again, so the two have to be the same quantity. Read off the
/// first radial, the two cuts here — 0.44° and 0.84° — would both be offered
/// as "0.7°", collapsing to a single entry that drew one of them and left
/// the other with no label that could reach it. That is the KDDC VCP 215
/// case this whole change is about, and no fixed-elevation fixture can
/// express it: it needs a sweep whose median and first radial disagree.
#[test]
fn the_picker_lists_the_tilt_each_sweep_flew() {
    let scan = Scan::new(
        VolumeCoveragePattern::new(
            215,
            0,
            0.5,
            PulseWidth::Short,
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
        vec![
            settling_sweep(1, 0.676, 0.44),
            settling_sweep(2, 0.739, 0.84),
        ],
    );
    let info = ScanInfo::from_scan(
        &scan,
        "KDDC",
        chrono::NaiveDate::from_ymd_opt(2026, 7, 26)
            .unwrap()
            .and_hms_opt(1, 48, 0)
            .unwrap(),
        None,
    );

    assert_eq!(
        info.product_elevations
            .get(&RadarProduct::Reflectivity)
            .map(Vec::as_slice),
        Some(&[0.4f32, 0.8][..]),
        "the two cuts must be offered as the tilts they flew — off the first \
             radial both round to 0.7 and one of them disappears",
    );

    // And the labels are usable: each reaches its own cut rather than the
    // two of them sharing one sweep.
    for (label, flown) in [(0.4f32, 0.44f64), (0.8, 0.84)] {
        let found = crate::render::find_sweep(&scan, RadarProduct::Reflectivity, label)
            .unwrap_or_else(|| panic!("{label}° is offered and must be reachable"));
        let drawn = crate::volumetric::sweep_elevation_deg(found).expect("the sweep has radials");
        assert!(
            (drawn - flown).abs() < 1e-4,
            "{label}° drew the {drawn}° cut, not the {flown}° one it names",
        );
    }
}

/// MEHS is a hail *size*, so it reads in the unit the hail-size preference
/// asks for — and at the precision that unit carries (`25 mm`, not
/// `25.40 mm`: the field is quantised in quarter inches, so the hundredth
/// of a millimetre is arithmetic, not measurement).
///
/// Pinned at the two operational stops of the ramp, the NWS severe-hail
/// criterion (1.00 in) and SPC significant severe (2.00 in), so the numbers
/// a warning decision is made on are the ones under test. `unit_label` is
/// asserted alongside `format_value` because it is the same unit printed in
/// two places — the hover readout and the colour bar's title — and a pane
/// that disagreed with itself would be worse than one stuck on inches.
#[test]
fn mehs_reads_in_the_users_hail_size_unit() {
    let expected = [
        (HailSizeUnit::Inches, "in", "MEHS: 1.00 in", "MEHS: 2.00 in"),
        (
            HailSizeUnit::Centimeters,
            "cm",
            "MEHS: 2.5 cm",
            "MEHS: 5.1 cm",
        ),
        (
            HailSizeUnit::Millimeters,
            "mm",
            "MEHS: 25 mm",
            "MEHS: 51 mm",
        ),
    ];
    for (unit, label, severe, sig_severe) in expected {
        let prefs = UserPreferences {
            hail_size: unit,
            ..UserPreferences::default()
        };
        assert_eq!(
            RadarProduct::MaxExpectedHailSize.unit_label(&prefs),
            label,
            "{unit:?} colour-bar title",
        );
        assert_eq!(
            RadarProduct::MaxExpectedHailSize.format_value(1.0, &prefs),
            severe,
            "{unit:?} at the 1.00 in severe criterion",
        );
        assert_eq!(
            RadarProduct::MaxExpectedHailSize.format_value(2.0, &prefs),
            sig_severe,
            "{unit:?} at the 2.00 in significant-severe threshold",
        );
    }
}

/// …and the default reading is what it was before the preference reached
/// this arm: `MEHS: {:.2} in`, the literal the arm used to be.
///
/// This is the whole no-silent-change claim. Everyone who has never opened
/// the settings dialog is on `HailSizeUnit::Inches`, so if any row here
/// moved, consuming the preference would have quietly restated every hail
/// size in the app.
#[test]
fn mehs_in_inches_is_what_it_printed_before_the_preference_existed() {
    let prefs = UserPreferences::default();
    assert_eq!(
        prefs.hail_size,
        HailSizeUnit::Inches,
        "the premise: inches is the default nobody has to choose",
    );
    for value in [0.0f32, 0.25, 0.75, 1.0, 1.375, 2.0, 4.0, 7.125] {
        assert_eq!(
            RadarProduct::MaxExpectedHailSize.format_value(value, &prefs),
            format!("MEHS: {value:.2} in"),
        );
    }
    assert_eq!(RadarProduct::MaxExpectedHailSize.unit_label(&prefs), "in");
}

/// Every view is pinned to a literal byte, its code is distinct, and the
/// two directions agree — the same claims
/// `every_product_has_a_stable_distinct_wire_code` makes for products, for
/// the axis a render cache key gained.
///
/// The literals are what makes this more than self-consistency.
/// Distinctness and round-trip both survive a **renumbering** — swapping
/// two views' bytes in both [`RenderView::wire_code`] and
/// [`RenderView::from_wire_code`] keeps them — and this byte is the kind
/// tag on a worker's out-of-band reply (`rustdar_worker::offload`'s
/// `decode_output`), so a renumbering makes the page run the *volume*
/// decoder over a cross-section's bytes.
///
/// That one lands softly, and is pinned anyway. The payload magics catch
/// it: `VoxelGrid::from_bytes` refuses bytes wearing `RDXS`, so the frame
/// is a clean "nothing to draw" rather than a misparse. But the byte is
/// still a wire contract between two builds, the guard that saves it lives
/// in another type entirely, and pinning it costs three lines.
#[test]
fn every_render_view_has_a_stable_distinct_wire_code() {
    let table: [(RenderView, u8); 3] = [
        (RenderView::PlanView, 1),
        (RenderView::CrossSection, 2),
        (RenderView::Volume, 3),
    ];
    let mut seen = std::collections::HashSet::new();
    for (view, code) in table {
        assert_eq!(
            view.wire_code(),
            code,
            "{view:?} moved on the wire: it tags as {} now, not {code}",
            view.wire_code(),
        );
        assert!(seen.insert(code), "{view:?} reuses wire code {code}");
        assert_eq!(
            RenderView::from_wire_code(code),
            Some(view),
            "wire code {code} no longer decodes to {view:?}",
        );
    }
    assert_eq!(
        table.len(),
        RenderView::all().len(),
        "a view left `all()` without leaving the table above",
    );
    assert_eq!(RenderView::from_wire_code(0), None);
    assert_eq!(
        RenderView::from_wire_code(4),
        None,
        "4 decodes, so the table above has stopped being the whole wire",
    );
    assert_eq!(RenderView::from_wire_code(u8::MAX), None);
}

/// The view half of the whole-volume question, with the plan view on the
/// *false* side.
///
/// Both sides are asserted deliberately. A predicate that answered `true`
/// for everything would be safe in the download direction and would also
/// make the whole distinction vacuous — every plan view would drag the
/// whole volume down every live feed — so the `false` arm is the one that
/// says the predicate still discriminates.
#[test]
fn only_the_vertical_views_read_the_whole_volume() {
    assert!(!RenderView::PlanView.reads_whole_volume());
    assert!(RenderView::CrossSection.reads_whole_volume());
    assert!(RenderView::Volume.reads_whole_volume());
    // And the product half genuinely cannot answer for it: reflectivity is
    // a one-sweep product, and a reflectivity section is not.
    assert!(!RadarProduct::Reflectivity.reads_whole_volume());
}

/// A volume that states its own position, as `crate::scan::decoded` builds
/// one out of the first Message 31's Volume Data Block.
///
/// The counterpart of [`empty_scan`], which is the shape a chunk-fed or a
/// pre-2010 volume arrives in: `Scan::new`, with no site on it at all.
fn scan_stating(lat: f32, lon: f32, site_height_m: i16, tower_height_m: u16) -> Scan {
    Scan::with_site(
        nexrad_model::meta::Site::new(*b"KTLX", lat, lon, site_height_m, tower_height_m),
        VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            PulseWidth::Short,
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
    )
}

fn at(timestamp_minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 26)
        .unwrap()
        .and_hms_opt(1, timestamp_minute, 0)
        .unwrap()
}

/// **Precedence is the design**: the volume in hand, then what an earlier
/// volume taught, then the compiled-in table, then nothing.
///
/// One table rather than four tests, because the property is the *ordering*
/// and an ordering asserted one pair at a time is an ordering nobody has
/// checked. Every row uses the same site and the same three candidate
/// positions, so the only thing that varies between rows is which of them are
/// present — which is exactly the variable the ordering is about.
///
/// The three positions are a degree apart, not a metre, so a row that resolves
/// to the wrong one says so in the failure message rather than in the last
/// decimal place.
#[test]
fn the_precedence_is_volume_then_learned_then_table() {
    // The radars this renders against; there are none until a test asks.
    crate::sites::fixture::install();
    use crate::site_position::{SitePosition, SitePositionSource};

    let table = crate::sites::get_radar_site("KTLX").expect("in the table");
    let volume_lat = 36.5f32;
    let learned = SitePosition {
        lat_udeg: 37_500_000,
        lon_udeg: -97_277_500,
        site_height_m: 370,
        tower_height_m: 20,
    };

    for (case, site, scan, memo, want_lat, want_source) in [
        (
            "a volume outranks everything",
            "KTLX",
            scan_stating(volume_lat, -97.2775, 370, 20),
            Some(learned),
            f64::from(volume_lat),
            SitePositionSource::Volume,
        ),
        (
            "a volume outranks the table with nothing learned",
            "KTLX",
            scan_stating(volume_lat, -97.2775, 370, 20),
            None,
            f64::from(volume_lat),
            SitePositionSource::Volume,
        ),
        (
            "what was learned outranks the table",
            "KTLX",
            empty_scan(),
            Some(learned),
            learned.lat(),
            SitePositionSource::Learned,
        ),
        (
            "the table is what is left",
            "KTLX",
            empty_scan(),
            None,
            table.lat,
            SitePositionSource::Table,
        ),
        (
            "a volume places a site the table has never heard of",
            "KXYZ",
            scan_stating(volume_lat, -97.2775, 370, 20),
            None,
            f64::from(volume_lat),
            SitePositionSource::Volume,
        ),
        (
            "what was learned places one too",
            "KXYZ",
            empty_scan(),
            Some(learned),
            learned.lat(),
            SitePositionSource::Learned,
        ),
        (
            "and with none of the three there is no answer",
            "KXYZ",
            empty_scan(),
            None,
            0.0,
            SitePositionSource::Unknown,
        ),
    ] {
        let info = ScanInfo::from_scan(&scan, site, at(48), memo);
        assert_eq!(info.site_source, want_source, "{case}: source");
        assert!(
            (info.site.lat - want_lat).abs() < 1e-5,
            "{case}: site is at {} and should be at {want_lat}",
            info.site.lat,
        );
        // The integer position rides along for the two sources that have one,
        // and does not for the two that do not — that is what the caller
        // persists, so a `Some` here for a table row would write the table
        // into the cache and make it look measured.
        assert_eq!(
            info.site_position.is_some(),
            matches!(
                want_source,
                SitePositionSource::Volume | SitePositionSource::Learned
            ),
            "{case}: site_position",
        );
    }
}

/// A chunk-fed volume keeps falling back to the table.
///
/// `crate::chunks` assembles a live volume through `Scan::new`, which takes no
/// site, so there is nothing on it to prefer — and this is the path the
/// application spends most of its time on. Named separately from the
/// precedence table because it is a *regression* guard rather than a statement
/// about ordering: the risk is that a change to `from_scan` starts demanding a
/// site that this path structurally cannot supply.
#[test]
fn a_chunk_fed_volume_falls_back_to_the_table() {
    // The radars this renders against; there are none until a test asks.
    crate::sites::fixture::install();
    // The real assembler, not a stand-in for it: this is the object the live
    // feed hands a `Scan` out of, so if it ever grew a site the fixture would
    // grow one with it.
    let mut assembler = crate::chunks::VolumeAssembler::new(
        "KTLX",
        crate::chunks::VolumeIndex::new(1).expect("1 is in range"),
    );
    let scan = assembler.snapshot();
    assert!(
        scan.site().is_none(),
        "the chunk path builds its Scan with `Scan::new`, which takes no site",
    );

    let info = ScanInfo::from_scan(&scan, "KTLX", at(48), None);
    let table = crate::sites::get_radar_site("KTLX").expect("in the table");
    assert_eq!(
        info.site_source,
        crate::site_position::SitePositionSource::Table
    );
    assert_eq!(info.site.lat, table.lat);
    assert_eq!(info.site.lon, table.lon);
    assert_eq!(info.site.heights, table.heights);
}

/// Everything downstream of `ScanInfo::site` moves with the volume, and this
/// is the list of things that do.
///
/// Each of these reads the site position out of a `ScanInfo` and turns it into
/// a number a user sees: the framing of the raster the gates are painted into,
/// the range and bearing under the cursor, the ground track a cross-section
/// runs along and the footprint of the 3D box. A correction that reached
/// `ScanInfo` and not these would be invisible.
///
/// The offset used here is `KTLX`'s own 43 m re-survey step, which is the
/// largest position change any site in the archive has ever made and the
/// reason last-writer-wins is the right rule.
#[test]
fn a_corrected_position_reaches_every_consumer_of_it() {
    // The radars this renders against; there are none until a test asks.
    crate::sites::fixture::install();
    let table = crate::sites::get_radar_site("KTLX").expect("in the table");
    // 43 m north, spelled in degrees at KTLX's latitude.
    let moved_lat = table.lat + 43.0 / (KM_PER_DEGREE_LAT * 1000.0);
    let info = ScanInfo::from_scan(
        &scan_stating(moved_lat as f32, table.lon as f32, 370, 20),
        "KTLX",
        at(48),
        None,
    );
    let moved = (info.site.lat, info.site.lon);
    assert!(
        (moved.0 - table.lat).abs() > 1e-6,
        "the fixture must actually move the site",
    );

    // The raster the gates are painted into, and so where every echo lands
    // against the map under it. Both at the floor extent, because what is
    // under test is the *centre* moving: a difference in extent would move
    // these corners for a reason that has nothing to do with the position.
    let was = ImageBounds::from_radar_site(table.lat, table.lon, BASE_EXTENT_KM);
    let now = ImageBounds::from_radar_site(moved.0, moved.1, BASE_EXTENT_KM);
    assert_ne!(was.min_lat, now.min_lat);
    assert_ne!(was.max_lat, now.max_lat);
    assert_ne!(was.mercator_y_min, now.mercator_y_min);

    // The hover readout's range and azimuth, and the cross-section's ground
    // track, both of which start from the site.
    let target = (table.lat + 0.5, table.lon + 0.5);
    let (was_bearing, was_range) =
        rustdar_geo::site_bearing_range_km(table.lat, table.lon, target.0, target.1);
    let (now_bearing, now_range) =
        rustdar_geo::site_bearing_range_km(moved.0, moved.1, target.0, target.1);
    assert_ne!(was_range, now_range);
    assert_ne!(was_bearing, now_bearing);

    // And the site's own height, which every beam height is measured above —
    // hail, HCA, echo tops, the cross-section's base, the 3D grid's datum.
    //
    // 370 m is 1214 ft and the row already records 1214 ft, so `adjudicate`
    // hands back the figure the row had rather than reconverting to it. The
    // difference is invisible here and load-bearing one layer down: a row
    // rebuilt rather than kept compares unequal to itself, and a launch would
    // then leak a fresh table for a volume that had not moved.
    //
    // This read 1213 while the compiled-in table existed. That table stated
    // KTLX's ground in feet from a source finer than a volume's whole metre,
    // and the metre could not contradict it. With the table deleted there is
    // no finer figure anywhere in the process and a height is the volume's
    // metre converted — one foot at KTLX, against a threshold of a metre.
    assert_eq!(
        info.site.height_ft(crate::sites::Datum::SiteBase),
        Some(1214),
        "a metre the volume cannot contradict must not move the row's feet",
    );
}

/// [`rustdar_geo::mercator_y_from_sin_lat`] is [`rustdar_geo::lat_rad_to_mercator_y`],
/// reached from the sine — one projection with two spellings, not two
/// conventions.
///
/// `ln(tan(π/4 + φ/2)) ≡ artanh(sin φ)` is an identity, so this is a
/// floating-point measurement of an exact equality rather than a tolerance on
/// an approximation. It matters because the two spellings are used on opposite
/// sides of one comparison: `ImageBounds` builds a raster's top and bottom
/// edges with the angle form, and `render::MercatorProjection::pixel_at` places
/// every gate between them with the sine form. A gap between them is a gap
/// between where the frame says the ground is and where the gates are drawn.
#[test]
fn the_mercator_y_from_a_sine_is_the_one_from_an_angle() {
    let mut worst = 0.0f64;
    let mut worst_lat = 0.0f64;
    // Every hundredth of a degree over the whole span a Web Mercator picture
    // is defined on, well past the ±85° the projection is usually cut at.
    for i in -8_900..=8_900 {
        let lat_deg = f64::from(i) / 100.0;
        let lat_rad = lat_deg.to_radians();
        let from_angle = rustdar_geo::lat_rad_to_mercator_y(lat_rad);
        let from_sine = rustdar_geo::mercator_y_from_sin_lat(lat_rad.sin());
        let gap = (from_angle - from_sine).abs();
        if gap > worst {
            worst = gap;
            worst_lat = lat_deg;
        }
    }
    // Mercator y runs to ±5.0 at 89°, so 1e-12 is under a part in 1e12 of the
    // axis — and a raster is at most 4096 pixels tall.
    assert!(
        worst < 1e-12,
        "the two spellings of Mercator y differ by {worst:.3e} at {worst_lat}\u{b0}",
    );

    // A pole is where the projection is genuinely infinite, and both spellings
    // have to say so rather than one of them returning a finite number a row
    // could be computed from.
    assert!(rustdar_geo::mercator_y_from_sin_lat(1.0).is_infinite());
    assert!(rustdar_geo::mercator_y_from_sin_lat(-1.0).is_infinite());
}
