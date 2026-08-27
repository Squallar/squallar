use super::*;
use squallar_radar::fields as radar_fields;
use squallar_radar::render::polar::{PolarField, PolarGeometry, Wedge};

/// The committed ground track follows the great circle the cut follows, not
/// the rhumb line a straight screen segment would draw.
#[test]
fn a_committed_track_bows_the_way_the_cut_does() {
    // 229 km due east at 41 °N, the orientation where a rhumb line and a
    // great circle are furthest apart.
    let lat = 41.0_f64;
    let dlon = 229.0 / (squallar_geo::KM_PER_DEGREE_LAT * lat.to_radians().cos());
    let line = crate::pane::SectionLine::new(
        squallar_geo::GeoPoint { lat, lon: -97.0 },
        squallar_geo::GeoPoint {
            lat,
            lon: -97.0 + dlon,
        },
    )
    .expect("a valid line");

    // Web Mercator on the crate's own sphere, scaled so that one point is one
    // metre of *ground* at this latitude — Mercator's local scale factor is
    // `1/cos(lat)`, so the `cos` makes the assertion below read in metres.
    let scale = squallar_geo::EARTH_RADIUS_KM * 1000.0 * lat.to_radians().cos();
    let project = |p: squallar_geo::GeoPoint| {
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
    let sagitta = (0..SECTION_TRACK_SAMPLES)
        .map(|i| {
            let (p, q) = (track[i], track[i + 1]);
            let half = (i as f64 + 0.5) / SECTION_TRACK_SAMPLES as f64;
            let (lat, lon) = squallar_geo::great_circle_point(
                (line.a().lat, line.a().lon),
                (line.b().lat, line.b().lon),
                half,
            );
            let on_curve = project(squallar_geo::GeoPoint { lat, lon });
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
#[test]
fn the_hover_readout_reports_range_and_azimuth_from_the_site() {
    // KTLX. One degree due north is Rₑ·(π/180) = 111.19 km at azimuth 0; one
    // degree due east is shorter than the parallel it looks like it follows
    // and leaves *north* of east, because a great circle bows poleward.
    let (site_lat, site_lon) = (35.3333, -97.2778);
    let prefs = UserPreferences::default();
    let readout = |hover_lat: f64, hover_lon: f64| {
        compute_hover_info_raw(
            &HoverSource::empty(),
            &HoverInput {
                site_lat,
                site_lon,
                hover_lat,
                hover_lon,
            },
            &radar_fields::known::REFLECTIVITY,
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
    assert!(
        readout(site_lat, site_lon).contains("Range: 0.0km"),
        "a site is not at zero range from itself: {}",
        readout(site_lat, site_lon),
    );
}

/// The hover reads the gate the point falls in, on the render's own geometry.
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
            &radar_fields::known::REFLECTIVITY,
            &prefs,
        )
    };

    let north = readout(site_lat + 1.0, site_lon);
    assert!(
        north.ends_with("| Reflectivity: 111.0 dBZ"),
        "one degree north is 111.19 km and so gate 111: {north}",
    );
    // Half a degree north is 55.6 km — gate 55, not gate 111 scaled.
    let half = readout(site_lat + 0.5, site_lon);
    assert!(
        half.ends_with("| Reflectivity: 55.0 dBZ"),
        "half a degree north is 55.60 km and so gate 55: {half}",
    );
    let past = readout(site_lat + 2.0, site_lon);
    assert!(
        past.ends_with("\u{b0} "),
        "past the end of every radial there is no gate to read: {past}",
    );
}

/// **A loop frame with no numbers says so, rather than reading as no data.**
#[test]
fn a_loop_frame_with_no_values_says_so_rather_than_reading_as_no_data() {
    let (site_lat, site_lon) = (35.3333, -97.2778);
    let prefs = UserPreferences::default();

    let geometry = PolarGeometry::from_parts(vec![WHOLE_COMPASS], 0.5, 1.0, None, 200);
    let mut field = PolarField::from_parts(geometry, vec![7.5; 200]);
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
            &radar_fields::known::NORMALIZED_ROTATION,
            &prefs,
        )
    };

    let inside = readout(site_lat + 1.0);
    assert!(
        inside.ends_with(NOT_RESIDENT),
        "a frame holding no values must say so: {inside}",
    );
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
fn pinned_cases() -> Vec<(FieldId, UserPreferences, Option<f32>)> {
    use squallar_units::{DistanceUnit, HailSizeUnit, PrecipRateUnit, SpeedUnit};
    let imperial = UserPreferences {
        distance: DistanceUnit::Miles,
        speed: SpeedUnit::Mph,
        hail_size: HailSizeUnit::Inches,
        precip_rate: PrecipRateUnit::InchesPerHour,
        ..UserPreferences::default()
    };
    let si = UserPreferences::default;
    vec![
        (radar_fields::known::REFLECTIVITY, si(), Some(42.5)),
        (radar_fields::known::REFLECTIVITY, si(), Some(-8.25)),
        (
            radar_fields::known::REFLECTIVITY,
            imperial.clone(),
            Some(42.5),
        ),
        (radar_fields::known::VELOCITY, si(), Some(-17.5)),
        (radar_fields::known::VELOCITY, imperial.clone(), Some(-17.5)),
        (radar_fields::known::SPECTRUM_WIDTH, si(), Some(3.25)),
        (
            radar_fields::known::DIFFERENTIAL_REFLECTIVITY,
            si(),
            Some(1.75),
        ),
        (
            radar_fields::known::CORRELATION_COEFFICIENT,
            si(),
            Some(0.987),
        ),
        (radar_fields::known::DIFFERENTIAL_PHASE, si(), Some(122.0)),
        (
            radar_fields::known::SPECIFIC_DIFFERENTIAL_PHASE,
            si(),
            Some(0.85),
        ),
        (radar_fields::known::NORMALIZED_ROTATION, si(), Some(2.75)),
        (radar_fields::known::ECHO_TOPS, si(), Some(42.0)),
        (
            radar_fields::known::VERTICALLY_INTEGRATED_LIQUID,
            si(),
            Some(18.0),
        ),
        (
            radar_fields::known::MAX_EXPECTED_HAIL_SIZE,
            imperial.clone(),
            Some(1.25),
        ),
        (
            radar_fields::known::HYDROMETEOR_CLASSIFICATION,
            si(),
            Some(6.0),
        ),
        (
            radar_fields::known::PRECIPITATION_RATE,
            imperial,
            Some(0.35),
        ),
        (radar_fields::known::REFLECTIVITY, si(), None),
    ]
}

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
        let geometry = PolarGeometry::from_parts(vec![WHOLE_COMPASS], 0.5, 1.0, None, GATES);
        let mut values = vec![f32::NAN; GATES];
        if let Some(v) = value {
            let (azimuth, ground_km) = squallar_geo::site_bearing_range_km(
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

        let got = compute_hover_info_raw(&source, &input, product, prefs);
        assert_eq!(got, expected, "{label}");
    }
}

/// A pan drag whose release lands on a frame the pane's map is not drawn —
/// the tab was switched, the pane turned 3D, a section pane took over.
///
/// walkers only ever offers `drag_stopped()` to the widget it happened on, on
/// the frame it happened, so a hidden pane loses the release edge outright.
/// Before `Gesture::Vanished` existed the pane went on being shifted by the
/// stored delta every frame and went on demanding a repaint: measured on this
/// exact sequence, **7,200 tile-pixels** of drift over the 120 frames below
/// (632.8 deg of longitude) with a `repaint_delay` of 0 ns.
///
/// The 3D pane is what hides the map here, and it hides it completely: the
/// floor strip builds its own `Map` on an **owned copy** of the memory
/// (`FloorStripCtx::map_memory`), so nothing on a `Volume` frame can see, let
/// alone clear, the pane's own `center_mode`.
#[test]
fn a_drag_whose_release_is_never_seen_stops_the_pane() {
    use crate::input_harness::InputHarness;
    const DT: f64 = 1.0 / 60.0;

    let memory_of = |h: &InputHarness| h.gui().pane(0).expect("pane 0").map_memory.clone();
    let centre = |h: &InputHarness| memory_of(h).detached().expect("a dragged pane is detached");

    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.warm_up();
    let start = h.pane_rects()[0].center();

    h.mouse_press(start);
    h.frame();
    h.mouse_move(start + egui::vec2(60.0, 0.0));
    h.frame();
    assert!(
        memory_of(&h).dragging(),
        "precondition: 60 px of primary drag did not start a pan"
    );
    let dragged_to = centre(&h);

    // The pane stops drawing its map, and the button comes up while it is not
    // there to see it.
    h.make_pane_volume(0);
    h.mouse_release(start + egui::vec2(60.0, 0.0));
    h.frames_for(10, DT);
    assert!(
        memory_of(&h).dragging(),
        "precondition: the hidden pane saw the release after all, so this \
         test is not about a lost release edge"
    );
    assert_eq!(
        dragged_to,
        centre(&h),
        "a pane that is not drawn moved anyway"
    );

    // And back. Two seconds of frames with the pointer up and still.
    h.make_pane_map(0);
    h.frames_for(120, DT);

    let memory = memory_of(&h);
    let now = centre(&h);
    // One tile-pixel of longitude. A pixel of latitude spans *fewer* degrees
    // than this -- by Mercator's local scale factor, 1/cos(35.3 deg) = 1.22 --
    // so the same bound on `dlat` is the looser of the two, at 1.22 px.
    let deg_per_tile_px = 360.0 / (256.0 * 2f64.powf(memory.zoom()));
    let (dlon, dlat) = (now.x() - dragged_to.x(), now.y() - dragged_to.y());
    assert!(
        dlon.abs() < deg_per_tile_px && dlat.abs() < deg_per_tile_px,
        "the restored pane drifted ({}, {}) tile-pixels with no input",
        dlon / deg_per_tile_px,
        dlat / deg_per_tile_px
    );
    assert!(
        !memory.dragging(),
        "the pane still believes a drag is in progress"
    );
    assert_eq!(
        h.repaint_delay(),
        std::time::Duration::MAX,
        "something is still demanding repaints two seconds after the last input"
    );
}
