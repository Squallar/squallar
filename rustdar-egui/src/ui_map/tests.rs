use super::*;

/// The committed ground track follows the great circle the cut follows, not
/// the rhumb line a straight screen segment would draw.
///
/// Straight in Web Mercator is *constant bearing*, not shortest path, and the
/// section is cut along a great circle. On a 229 km line at 41 °N — a
/// full-range line at the latitude of the northern-tier sites — the two part
/// by 894 m in the middle, three times the range-ring offset that has a doc
/// block of its own, and the user is placed to notice it because the track is
/// drawn over the echo the section was aimed at.
///
/// The projector is stubbed with a plain Web Mercator, which is what
/// `walkers` projects with — the pane's zoom and centre are an affine
/// transform on top and cannot turn a curve into a line.
#[test]
fn a_committed_track_bows_the_way_the_cut_does() {
    // 229 km due east at 41 °N, the orientation where a rhumb line and a
    // great circle are furthest apart.
    let lat = 41.0_f64;
    let dlon = 229.0 / (rustdar_radar::types::KM_PER_DEGREE_LAT * lat.to_radians().cos());
    let line = crate::pane::SectionLine::new(
        crate::pane::GeoPoint { lat, lon: -97.0 },
        crate::pane::GeoPoint {
            lat,
            lon: -97.0 + dlon,
        },
    )
    .expect("a valid line");

    // Web Mercator on the crate's own sphere, scaled so that one point is one
    // metre of *ground* at this latitude — Mercator's local scale factor is
    // `1/cos(lat)`, so the `cos` is what makes the assertion below readable
    // in metres rather than in projected units.
    let scale = rustdar_radar::types::EARTH_RADIUS_KM * 1000.0 * lat.to_radians().cos();
    let project = |p: crate::pane::GeoPoint| {
        let y = (std::f64::consts::FRAC_PI_4 + p.lat.to_radians() / 2.0)
            .tan()
            .ln();
        egui::pos2((p.lon.to_radians() * scale) as f32, (-y * scale) as f32)
    };

    let track = great_circle_track(line, project);
    assert_eq!(track.len(), SECTION_TRACK_SAMPLES + 1);
    assert_eq!(
        (track[0], track[SECTION_TRACK_SAMPLES]),
        (project(line.a()), project(line.b())),
        "the track no longer starts and ends where the user put it"
    );

    // How far the drawn track departs from the straight segment it replaced,
    // measured perpendicular to that segment.
    let (a, b) = (track[0], track[SECTION_TRACK_SAMPLES]);
    let seg = b - a;
    let len = seg.length();
    let bow = track
        .iter()
        .map(|p| ((*p - a).x * seg.y - (*p - a).y * seg.x).abs() / len)
        .fold(0.0_f32, f32::max);
    assert!(
        (700.0..1100.0).contains(&bow),
        "the track bows {bow} m off the straight segment; a rhumb line bows \
             0 and the great circle bows ~894"
    );

    // And the *residual* — what is left of that bow between two drawn
    // vertices — is inside the error budget the module already accepts.
    //
    // This is the assertion that says what the subdivision is for, and it is
    // deliberately a **bar rather than a count**. 258 m is where it was set:
    // the range ring used to sit that far outside the ground the track walked,
    // because the ring was placed on a 6378 km sphere and the track on 6371,
    // so a track that beat 258 m was as registered as everything else on the
    // map. Those are one sphere now and that offset is zero, which makes this
    // a *legacy* ceiling the track beats by two orders of magnitude rather
    // than a live budget. It is kept at its old value on purpose: tightening
    // it to the measurement would pin `SECTION_TRACK_SAMPLES`, and the exact
    // count is a quality knob above the bar, not a claim — lowering it to 8
    // would still pass, correctly.
    let sagitta = (0..SECTION_TRACK_SAMPLES)
        .map(|i| {
            let (p, q) = (track[i], track[i + 1]);
            let half = (i as f64 + 0.5) / SECTION_TRACK_SAMPLES as f64;
            let (lat, lon) = rustdar_radar::beam::great_circle_point(
                (line.a().lat, line.a().lon),
                (line.b().lat, line.b().lon),
                half,
            );
            let on_curve = project(crate::pane::GeoPoint { lat, lon });
            (on_curve - (p + (q - p) * 0.5)).length()
        })
        .fold(0.0_f32, f32::max);
    assert!(
        sagitta < 258.0,
        "the drawn track leaves {sagitta} m of the curve between vertices, \
             which is worse than the 258 m the range ring used to sit off this \
             track before the two spheres were unified"
    );
}

/// A track with no points paints nothing rather than panicking on `first`.
#[test]
fn an_empty_track_paints_nothing() {
    let ctx = egui::Context::default();
    let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
    let painter = egui::Painter::new(ctx, egui::LayerId::debug(), rect);
    paint_section_track(&painter, &[], rect);
}

/// The string the status bar shows, for hover points at hand-checkable
/// offsets from a real site.
///
/// The readout had no test of its own while it carried its own copy of the
/// haversine and forward azimuth. `beam::tests::
/// the_hover_readouts_polar_coordinates_are_bit_identical_to_the_deleted_copy`
/// pins the two spellings against each other, which is what makes moving to
/// the shared one provably not a change; this pins what a user reads, so the
/// next edit to either has something behavioural to fail.
#[test]
fn the_hover_readout_reports_range_and_azimuth_from_the_site() {
    // KTLX. One degree due north is Rₑ·(π/180) = 111.19 km at azimuth 0; one
    // degree due east is shorter than the parallel it looks like it follows
    // and leaves *north* of east, because a great circle bows poleward.
    let (site_lat, site_lon) = (35.3333, -97.2778);
    let prefs = UserPreferences::default();
    let readout = |hover_lat: f64, hover_lon: f64| {
        compute_hover_info_raw(
            &[],
            &HoverInput {
                site_lat,
                site_lon,
                hover_lat,
                hover_lon,
                // Outside the rect, so no gate value is appended and the
                // assertion is on the geometry alone.
                hover_pos: egui::pos2(-1.0, -1.0),
                rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 100.0)),
            },
            RadarProduct::Reflectivity,
            &prefs,
        )
    };

    assert_eq!(
        readout(site_lat + 1.0, site_lon),
        "Lat: 36.3333\u{b0}, Lon: -97.2778\u{b0} | Range: 111.2km, Az: 0.0\u{b0} ",
    );
    assert_eq!(
        readout(site_lat, site_lon + 1.0),
        "Lat: 35.3333\u{b0}, Lon: -96.2778\u{b0} | Range: 90.7km, Az: 89.7\u{b0} ",
    );
    // A site to itself: zero range, and the azimuth is unconstrained rather
    // than wrong, so only the range half is asserted.
    assert!(
        readout(site_lat, site_lon).contains("Range: 0.0km"),
        "a site is not at zero range from itself: {}",
        readout(site_lat, site_lon),
    );
}

/// The hover reads a value grid at the grid's *own* side, not at a constant.
///
/// A grid arrives from a render whose raster size depends on how far the sweep
/// reached, and this crate is never told which ceiling the frontend offered
/// it. So the readout has to derive the side, and the derivation has to be
/// exercised on a grid that is **not** the default size — a test on a
/// `IMAGE_SIZE`-square grid would pass just as well against the old constant.
///
/// The fixture is a 64 × 64 grid with one non-`NaN` cell, placed so that a
/// side read as anything else lands somewhere else: at row 16, column 48, the
/// pointer three quarters across and a quarter down finds it, and the same
/// pointer on a 2048-wide reading would index past the end and report nothing.
#[test]
fn the_hover_reads_a_value_grid_at_the_side_its_length_implies() {
    const SIDE: usize = 64;
    let mut grid = vec![f32::NAN; SIDE * SIDE];
    grid[16 * SIDE + 48] = 42.5;

    let prefs = UserPreferences::default();
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 100.0));
    let readout = |values: &[f32], x: f32, y: f32| {
        compute_hover_info_raw(
            values,
            &HoverInput {
                site_lat: 35.3333,
                site_lon: -97.2778,
                hover_lat: 35.3333,
                hover_lon: -97.2778,
                hover_pos: egui::pos2(x, y),
                rect,
            },
            RadarProduct::Reflectivity,
            &prefs,
        )
    };

    // Three quarters across, a quarter down — column 48, row 16 of 64.
    assert!(
        readout(&grid, 75.5, 25.5).ends_with("| Reflectivity: 42.5 dBZ"),
        "the gate at (48, 16) of a 64-wide grid was not found: {}",
        readout(&grid, 75.5, 25.5),
    );
    // And a pointer elsewhere in the same grid finds the NaN that is there.
    assert!(
        readout(&grid, 25.5, 75.5).ends_with("\u{b0} "),
        "a cell with no gate in it must append nothing",
    );

    // A loop frame's grid: empty on purpose, because a hover goes quiet under
    // a loop. `poll_loop_render_results` stores `Vec::new()` for every frame,
    // so this is the routine case and it must not divide by a zero side.
    assert!(
        readout(&[], 50.0, 50.0).ends_with("\u{b0} "),
        "an empty value grid must read as no value rather than dividing by \
         a side of zero",
    );

    // A length that is not a square is not a grid this display makes, and is
    // refused rather than indexed into at some rounded-down side.
    assert!(readout(&vec![1.0; SIDE * SIDE - 1], 50.0, 50.0).ends_with("\u{b0} "));
}

/// The side derivation itself, at the two ends and at the sizes that are not
/// grids.
#[test]
fn a_value_grids_side_is_its_exact_integer_square_root_or_nothing() {
    for side in [1usize, 64, 1024, 2048, 4096] {
        assert_eq!(value_grid_side(side * side), Some(side), "{side}");
    }
    for len in [0, 2, 3, 5, 2048 * 2048 - 1, 2048 * 2048 + 1] {
        assert_eq!(value_grid_side(len), None, "{len}");
    }
}

/// **Every digit the status bar shows, pinned.**
///
/// The readout is about to stop reading a `side²` raster grid and start reading
/// the gate the render painted. That is a change of *source*, and the whole
/// claim being made for it is that a user watching a still pane sees no
/// difference — so the strings below were captured from the grid
/// implementation before it was touched, and the same table is asserted against
/// whatever the readout reads next. If a digit moves, this fails, and the digit
/// that moved is the finding.
///
/// It covers the product formatters rather than the geometry — one position,
/// seventeen rows — because the geometry half is already pinned by
/// [`the_hover_readout_reports_range_and_azimuth_from_the_site`] and by
/// `rustdar_radar::beam::tests::
/// the_hover_readouts_polar_coordinates_are_bit_identical_to_the_deleted_copy`.
/// What is not pinned anywhere else is the interaction of the value with the
/// user's units: velocity in a metric preference still reads in mph, spectrum
/// width follows the speed unit, hail follows its own, and the distance unit
/// changes the range half of the same string. Those are the rows that would
/// move silently.
///
/// **What this deliberately does not pin is which gate is read.** No fixture
/// here goes through a rasterizer, so the value is placed under the pointer by
/// hand and the assertion is on the formatting alone. That the new source names
/// the same gate as the old grid is
/// `rustdar_radar::render::tests::the_polar_field_answers_what_the_value_grid_holds`,
/// which is where a rasterizer is available to be asked.
const PINNED_READOUTS: &[(&str, &str)] = &[
    (
        "reflectivity",
        "Lat: 35.9000\u{b0}, Lon: -96.8000\u{b0} | Range: 76.4km, Az: 34.3\u{b0} | Reflectivity: 42.5 dBZ",
    ),
    (
        "reflectivity negative",
        "Lat: 35.9000\u{b0}, Lon: -96.8000\u{b0} | Range: 76.4km, Az: 34.3\u{b0} | Reflectivity: -8.2 dBZ",
    ),
    (
        "reflectivity miles",
        "Lat: 35.9000\u{b0}, Lon: -96.8000\u{b0} | Range: 47.5mi, Az: 34.3\u{b0} | Reflectivity: 42.5 dBZ",
    ),
    (
        "velocity m/s",
        "Lat: 35.9000\u{b0}, Lon: -96.8000\u{b0} | Range: 76.4km, Az: 34.3\u{b0} | Velocity: -39.1 mph",
    ),
    (
        "velocity mph",
        "Lat: 35.9000\u{b0}, Lon: -96.8000\u{b0} | Range: 47.5mi, Az: 34.3\u{b0} | Velocity: -39.1 mph",
    ),
    (
        "spectrum width",
        "Lat: 35.9000\u{b0}, Lon: -96.8000\u{b0} | Range: 76.4km, Az: 34.3\u{b0} | Spectrum Width: 7.3 mph",
    ),
    (
        "zdr",
        "Lat: 35.9000\u{b0}, Lon: -96.8000\u{b0} | Range: 76.4km, Az: 34.3\u{b0} | Diff. Reflectivity: 1.75 dB",
    ),
    (
        "cc",
        "Lat: 35.9000\u{b0}, Lon: -96.8000\u{b0} | Range: 76.4km, Az: 34.3\u{b0} | Corr. Coefficient: 0.9870",
    ),
    (
        "phi",
        "Lat: 35.9000\u{b0}, Lon: -96.8000\u{b0} | Range: 76.4km, Az: 34.3\u{b0} | Diff. Phase: 122.0\u{b0}",
    ),
    (
        "kdp",
        "Lat: 35.9000\u{b0}, Lon: -96.8000\u{b0} | Range: 76.4km, Az: 34.3\u{b0} | KDP: 0.85 \u{b0}/km",
    ),
    (
        "nrot",
        "Lat: 35.9000\u{b0}, Lon: -96.8000\u{b0} | Range: 76.4km, Az: 34.3\u{b0} | NROT: 2.75",
    ),
    (
        "echo tops",
        "Lat: 35.9000\u{b0}, Lon: -96.8000\u{b0} | Range: 76.4km, Az: 34.3\u{b0} | Echo Tops: 42.0 kft",
    ),
    (
        "vil",
        "Lat: 35.9000\u{b0}, Lon: -96.8000\u{b0} | Range: 76.4km, Az: 34.3\u{b0} | VIL: 18.0 kg/m\u{b2}",
    ),
    (
        "mehs",
        "Lat: 35.9000\u{b0}, Lon: -96.8000\u{b0} | Range: 47.5mi, Az: 34.3\u{b0} | MEHS: 1.25 in",
    ),
    (
        "hca",
        "Lat: 35.9000\u{b0}, Lon: -96.8000\u{b0} | Range: 76.4km, Az: 34.3\u{b0} | HHC: No Data",
    ),
    (
        "precip rate",
        "Lat: 35.9000\u{b0}, Lon: -96.8000\u{b0} | Range: 47.5mi, Az: 34.3\u{b0} | Precip Rate: 0.35 in/hr",
    ),
    (
        "no gate",
        "Lat: 35.9000\u{b0}, Lon: -96.8000\u{b0} | Range: 76.4km, Az: 34.3\u{b0} ",
    ),
];

/// The product, preferences and value behind each row of [`PINNED_READOUTS`],
/// in the same order.
fn pinned_cases() -> Vec<(RadarProduct, UserPreferences, Option<f32>)> {
    use rustdar_units::{DistanceUnit, HailSizeUnit, PrecipRateUnit, SpeedUnit};
    let imperial = UserPreferences {
        distance: DistanceUnit::Miles,
        speed: SpeedUnit::Mph,
        hail_size: HailSizeUnit::Inches,
        precip_rate: PrecipRateUnit::InchesPerHour,
        ..UserPreferences::default()
    };
    let si = UserPreferences::default;
    vec![
        (RadarProduct::Reflectivity, si(), Some(42.5)),
        (RadarProduct::Reflectivity, si(), Some(-8.25)),
        (RadarProduct::Reflectivity, imperial.clone(), Some(42.5)),
        (RadarProduct::Velocity, si(), Some(-17.5)),
        (RadarProduct::Velocity, imperial.clone(), Some(-17.5)),
        (RadarProduct::SpectrumWidth, si(), Some(3.25)),
        (RadarProduct::DifferentialReflectivity, si(), Some(1.75)),
        (RadarProduct::CorrelationCoefficient, si(), Some(0.987)),
        (RadarProduct::DifferentialPhase, si(), Some(122.0)),
        (RadarProduct::SpecificDifferentialPhase, si(), Some(0.85)),
        (RadarProduct::NormalizedRotation, si(), Some(2.75)),
        (RadarProduct::EchoTops, si(), Some(42.0)),
        (RadarProduct::VerticallyIntegratedLiquid, si(), Some(18.0)),
        (
            RadarProduct::MaxExpectedHailSize,
            imperial.clone(),
            Some(1.25),
        ),
        (RadarProduct::HydrometeorClassification, si(), Some(6.0)),
        (RadarProduct::PrecipitationRate, imperial, Some(0.35)),
        (RadarProduct::Reflectivity, si(), None),
    ]
}

/// See [`PINNED_READOUTS`].
#[test]
fn the_hover_readouts_digits_do_not_move() {
    const SIDE: usize = 8;
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(80.0, 80.0));
    // Row 3, column 5 of an 8 x 8 grid.
    let hover_pos = egui::pos2(55.0, 35.0);
    let cases = pinned_cases();
    assert_eq!(
        cases.len(),
        PINNED_READOUTS.len(),
        "one case per pinned row"
    );

    for ((product, prefs, value), &(label, expected)) in cases.iter().zip(PINNED_READOUTS) {
        let mut grid = vec![f32::NAN; SIDE * SIDE];
        if let Some(v) = value {
            grid[3 * SIDE + 5] = *v;
        }
        let got = compute_hover_info_raw(
            &grid,
            &HoverInput {
                site_lat: 35.3333,
                site_lon: -97.2778,
                hover_lat: 35.9,
                hover_lon: -96.8,
                hover_pos,
                rect,
            },
            *product,
            prefs,
        );
        assert_eq!(got, expected, "{label}");
    }
}
