use super::*;
use nexrad_model::data::{PulseWidth, Scan, VolumeCoveragePattern};

/// **Both** arms of the [`IMAGE_SIZE`] cascade, unconditionally.
///
/// The arm a host build does not compile is the one nothing else would
/// catch: the audit that prompted this changed the wasm literal from 1024 to
/// 4096 on a pristine tree and the whole workspace passed 1508/0 with the
/// wasm `cargo check` exiting 0. `sampler.rs`'s
/// `the_cos_e_correction_diverges_from_the_plan_view_by_a_measured_amount`
/// looks like it covers this and does not — its `if cfg!(…)` picks the
/// running target's literal, so the other one is dead text.
///
/// Every property here is one the render path depends on, not a restatement
/// of the literals for its own sake; the literals are pinned separately at
/// the end so that editing a value *and* its consequences in step still has
/// to be deliberate.
#[test]
fn both_image_size_arms_are_pinned_not_just_the_compiled_one() {
    // The web arm has to leave room *beside* the radar frame for the overlay
    // textures, which is the stated reason it halves; native allocates its
    // frame on a real GPU and is only checked against the same floor for
    // symmetry.
    for (target, size, wants_overlay_room) in [
        ("wasm32", WASM_IMAGE_SIZE, true),
        ("native", NATIVE_IMAGE_SIZE, false),
    ] {
        // `render::project` indexes `py * IMAGE_SIZE + px` into a single
        // allocation and `ImageBounds` divides the extent by it, so zero is
        // a division by zero before it is an empty image.
        assert!(size > 0, "{target}");
        // The projection assumes a power of two; `constants.rs` asserts the
        // same thing at compile time for whichever arm it compiled.
        assert!(size.is_power_of_two(), "{target}: {size}");
        // A browser may legitimately report exactly the WebGL2 guarantee, so
        // *both* arms have to fit it — the web one with room to spare,
        // because the overlay textures sit alongside the radar frame in the
        // same budget. This is the assertion the 4096 mutation trips.
        let needed = if wants_overlay_room { size * 2 } else { size };
        assert!(
            needed <= WEBGL2_MAX_TEXTURE_DIMENSION_2D,
            "the {target} image is {size} px and needs {needed} px of the \
                 {WEBGL2_MAX_TEXTURE_DIMENSION_2D} px 2D texture size WebGL2 guarantees"
        );
    }

    // The web arm halves the side, which quarters the RGBA texture. That
    // ratio is what `LOOP_TEXTURE_BUDGET_BYTES`' 4 MiB-vs-16 MiB frame
    // figures are computed from, so it is a relation and not a coincidence.
    assert_eq!(NATIVE_IMAGE_SIZE, WASM_IMAGE_SIZE * 2);

    assert_eq!(WASM_IMAGE_SIZE, 1024);
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

/// The derived geometry moves with whichever arm was selected.
///
/// `PIXELS_PER_KM` is the whole rasterizer's scale factor — `render::project`
/// and `sampler`'s section geometry both go through it — so a changed
/// `IMAGE_SIZE` that left this stale would misplace every pixel rather than
/// resize the image. Written as the ratio rather than as a number so it
/// cannot be satisfied by a literal that happens to match today.
#[test]
fn pixels_per_km_follows_the_selected_image_size() {
    assert_eq!(PIXELS_PER_KM, IMAGE_SIZE as f64 / (2.0 * MAX_RANGE_KM));
    // Both arms land on a usable scale: the coarser of the two still puts
    // more than two pixels on a kilometre, which is what makes a 250 m gate
    // land in its own pixel rather than being dropped.
    for size in [WASM_IMAGE_SIZE, NATIVE_IMAGE_SIZE] {
        let scale = size as f64 / (2.0 * MAX_RANGE_KM);
        assert!(scale > 2.0, "{size} px gives {scale:.3} px/km");
    }
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
/// tag on a worker's out-of-band reply (`rustdar_frontend::offload`'s
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
