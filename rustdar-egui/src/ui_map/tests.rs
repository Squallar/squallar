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
    let dlon = 229.0 / (111.32 * lat.to_radians().cos());
    let line = crate::pane::SectionLine::new(
        crate::pane::GeoPoint { lat, lon: -97.0 },
        crate::pane::GeoPoint {
            lat,
            lon: -97.0 + dlon,
        },
    )
    .expect("a valid line");

    // Web Mercator on a 6371 km sphere, scaled so that one point is one
    // metre of *ground* at this latitude — Mercator's local scale factor is
    // `1/cos(lat)`, so the `cos` is what makes the assertion below readable
    // in metres rather than in projected units.
    let scale = 6_371_000.0 * lat.to_radians().cos();
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
    // deliberately a **bar rather than a count**: 258 m is the range-ring
    // offset `draw_section_tracks` documents and lives with, so a track that
    // beats it is as registered as everything else on the map. The exact
    // `SECTION_TRACK_SAMPLES` is a quality knob above that bar — it is not
    // pinned here, and lowering it to 8 would still pass, correctly.
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
             which is worse than the 258 m range-ring offset the module already \
             documents as the error it lives with"
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
