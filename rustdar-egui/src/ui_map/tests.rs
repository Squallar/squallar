use super::*;
use rustdar_radar::render::polar::{PolarField, PolarGeometry, Wedge};

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
    let dlon = 229.0 / (rustdar_geo::KM_PER_DEGREE_LAT * lat.to_radians().cos());
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
    let scale = rustdar_geo::EARTH_RADIUS_KM * 1000.0 * lat.to_radians().cos();
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
            let (lat, lon) = rustdar_geo::great_circle_point(
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
            // A source over no picture at all, so nothing is appended and the
            // assertion is on the geometry alone.
            &HoverSource::empty(),
            &HoverInput {
                site_lat,
                site_lon,
                hover_lat,
                hover_lon,
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

/// The hover reads the gate the point falls in, on the render's own geometry.
///
/// This replaced a test that the readout derived a raster grid's side from its
/// length. There is no grid and no side any more: the pointer's position stops
/// being a pixel and becomes an azimuth and a ground range, and the answer
/// comes from the same wedge-and-gate rule
/// [`rustdar_radar::render::polar::PolarGeometry::pick`] states once for the
/// whole workspace.
///
/// The fixture gives every gate a different number, so reading the wrong one is
/// visible in the string rather than aliasing onto the right answer — which is
/// the same property the 64-wide grid fixture was chosen for.
#[test]
fn the_hover_reads_the_gate_the_point_falls_in() {
    let (site_lat, site_lon) = (35.3333, -97.2778);
    let prefs = UserPreferences::default();

    // One radial over the whole compass, 200 gates of 1 km from 0.5 km out.
    // Gate g therefore spans [g, g + 1) km and carries `g` dBZ.
    const GATES: usize = 200;
    let geometry = PolarGeometry::from_parts(vec![WHOLE_COMPASS], 0.5, 1.0, None, GATES);
    let values: Vec<f32> = (0..GATES).map(|g| g as f32).collect();
    let source = HoverSource::resident(PolarField::from_parts(geometry, values));

    let readout = |hover_lat: f64, hover_lon: f64| {
        compute_hover_info_raw(
            &source,
            &HoverInput {
                site_lat,
                site_lon,
                hover_lat,
                hover_lon,
            },
            RadarProduct::Reflectivity,
            &prefs,
        )
    };

    // One degree due north of the site is 111.19 km, which is gate 111.
    let north = readout(site_lat + 1.0, site_lon);
    assert!(
        north.ends_with("| Reflectivity: 111.0 dBZ"),
        "one degree north is 111.19 km and so gate 111: {north}",
    );
    // Half a degree north is 55.6 km — gate 55, not gate 111 scaled by
    // anything, which is what a rule reading the range at the wrong interval
    // would give.
    let half = readout(site_lat + 0.5, site_lon);
    assert!(
        half.ends_with("| Reflectivity: 55.0 dBZ"),
        "half a degree north is 55.60 km and so gate 55: {half}",
    );
    // Two degrees north is 222.4 km, past the last gate this field has.
    let past = readout(site_lat + 2.0, site_lon);
    assert!(
        past.ends_with("\u{b0} "),
        "past the end of every radial there is no gate to read: {past}",
    );
}

/// **A loop frame with no numbers says so, rather than reading as no data.**
///
/// The state a looping pane is in for a product the volume behind it cannot
/// answer for. A blank readout already means "the radar looked here and found
/// nothing"; letting a frame that kept no values wear that meaning shows the
/// reader a hole in the weather that is really a hole in the application.
///
/// The counterpart claim — that a looping pane of a wire moment reads a real
/// number — is `rustdar_radar::hover::hover_tests::
/// a_looping_pane_and_a_still_pane_read_one_point_alike`, which is where a
/// volume is available to read from.
#[test]
fn a_loop_frame_with_no_values_says_so_rather_than_reading_as_no_data() {
    let (site_lat, site_lon) = (35.3333, -97.2778);
    let prefs = UserPreferences::default();

    let geometry = PolarGeometry::from_parts(vec![WHOLE_COMPASS], 0.5, 1.0, None, 200);
    let mut field = PolarField::from_parts(geometry, vec![7.5; 200]);
    // What a loop frame carries: the geometry, and no numbers.
    field.strip_values();
    let source = HoverSource::from_volume(field, None);

    let readout = |hover_lat: f64| {
        compute_hover_info_raw(
            &source,
            &HoverInput {
                site_lat,
                site_lon,
                hover_lat,
                hover_lon: site_lon,
            },
            RadarProduct::NormalizedRotation,
            &prefs,
        )
    };

    // Inside the picture: the gate is there and its number is not.
    let inside = readout(site_lat + 1.0);
    assert!(
        inside.ends_with(NOT_RESIDENT),
        "a frame holding no values must say so: {inside}",
    );
    // Outside it: nothing was painted, which is the ordinary blank.
    let outside = readout(site_lat + 2.0);
    assert!(
        outside.ends_with("\u{b0} "),
        "past the last gate there is nothing to be missing: {outside}",
    );
}

/// A radial painted over the whole compass, so a fixture can put a point in it
/// without arranging an azimuth.
const WHOLE_COMPASS: Wedge = Wedge {
    azimuth_deg: 0.0,
    half_width_deg: 180.0,
};

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
    const GATES: usize = 200;
    let input = HoverInput {
        site_lat: 35.3333,
        site_lon: -97.2778,
        hover_lat: 35.9,
        hover_lon: -96.8,
    };
    let cases = pinned_cases();
    assert_eq!(
        cases.len(),
        PINNED_READOUTS.len(),
        "one case per pinned row"
    );

    for ((product, prefs, value), &(label, expected)) in cases.iter().zip(PINNED_READOUTS) {
        // The value goes in the gate the point actually falls in, found through
        // the same `pick` the readout will use — so a row with a value is a row
        // where the readout has one to find, whatever the geometry.
        let geometry = PolarGeometry::from_parts(vec![WHOLE_COMPASS], 0.5, 1.0, None, GATES);
        let mut values = vec![f32::NAN; GATES];
        if let Some(v) = value {
            let (azimuth, ground_km) = rustdar_geo::site_bearing_range_km(
                input.site_lat,
                input.site_lon,
                input.hover_lat,
                input.hover_lon,
            );
            let at = geometry
                .pick(azimuth, ground_km)
                .expect("the pinned point is inside the fixture's radial");
            values[at.gate] = *v;
        }
        let source = HoverSource::resident(PolarField::from_parts(geometry, values));

        let got = compute_hover_info_raw(&source, &input, *product, prefs);
        assert_eq!(got, expected, "{label}");
    }
}
