//! Where the 2D radar raster lands, against where its gates actually are.

use squallar_geo::EARTH_RADIUS_KM;
use squallar_radar::types::ImageBounds;

/// Sites spanning the fleet's latitude range, which is the axis the error
/// would live on: a Mercator-vs-latitude bow is zero at the equator and grows
/// with `tan φ`.
const SITES: &[(&str, f64, f64)] = &[
    ("TJUA Puerto Rico", 18.1156, -66.0781),
    ("PHKM Kohala HI", 20.1254, -155.7780),
    ("KEWX Austin TX", 29.7039, -98.0286),
    ("KTLX Oklahoma City", 35.3331, -97.2778),
    ("KMPX Minneapolis", 44.8489, -93.5656),
    ("PACG Sitka AK", 56.8528, -135.5292),
    ("PABC Bethel AK", 60.7919, -161.8763),
    ("PAPD Fairbanks AK", 65.0351, -147.5014),
];

/// The inverse of Web Mercator's `y`, written out here on purpose.
fn lat_of_mercator_y(y: f64) -> f64 {
    (2.0 * y.exp().atan() - std::f64::consts::FRAC_PI_2).to_degrees()
}

/// A raster row is linear in Mercator `y`, so the texture and the rect it is
/// stretched into describe the same ground.
#[test]
fn a_raster_row_lands_where_the_projector_puts_its_latitude() {
    const TOL_POINTS: f32 = 0.1;
    const SIDE: usize = 3240;

    let clip = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 900.0));
    let mut worst = (0.0f32, String::new());

    for &(name, site_lat, site_lon) in SITES {
        for extent_km in [230.0f64, 460.0] {
            let bounds = ImageBounds::from_radar_site(site_lat, site_lon, extent_km);

            let mut memory = walkers::MapMemory::default();
            memory.set_zoom(7.0).expect("7 is a zoom walkers accepts");
            let projector =
                walkers::Projector::new(clip, &memory, walkers::lat_lon(site_lat, site_lon));

            let nw = projector.project(walkers::lat_lon(bounds.max_lat, bounds.min_lon));
            let se = projector.project(walkers::lat_lon(bounds.min_lat, bounds.max_lon));
            let rect = egui::Rect::from_two_pos(nw.to_pos2(), se.to_pos2());

            let merc_span = bounds.mercator_y_max - bounds.mercator_y_min;
            let lon_span = bounds.max_lon - bounds.min_lon;

            for row in [
                0usize,
                1,
                SIDE / 8,
                SIDE / 4,
                SIDE / 2,
                3 * SIDE / 4,
                SIDE - 1,
            ] {
                for col in [0usize, SIDE / 4, SIDE / 2, SIDE - 1] {
                    let v = (row as f64 + 0.5) / SIDE as f64;
                    let u = (col as f64 + 0.5) / SIDE as f64;

                    let merc_y = bounds.mercator_y_max - v * merc_span;
                    let lat = lat_of_mercator_y(merc_y);
                    let lon = bounds.min_lon + u * lon_span;

                    let drawn = egui::pos2(
                        rect.min.x + u as f32 * rect.width(),
                        rect.min.y + v as f32 * rect.height(),
                    );
                    let truth = projector.project(walkers::lat_lon(lat, lon)).to_pos2();

                    let err = (drawn - truth).abs();
                    let e = err.x.max(err.y);
                    if e > worst.0 {
                        worst = (
                            e,
                            format!(
                                "{name} at {extent_km} km, pixel ({col},{row}): \
                                 drawn {drawn:?} vs {truth:?}"
                            ),
                        );
                    }
                }
            }
        }
    }

    assert!(
        worst.0 <= TOL_POINTS,
        "the raster and the rect it is stretched into disagree by {} points: {}",
        worst.0,
        worst.1
    );
}

/// The bow a latitude-linear raster would have had, stated as a number.
#[test]
fn a_latitude_linear_raster_would_have_been_wrong_by_kilometres() {
    const SIDE: usize = 3240;
    let mut worst_km = 0.0f64;
    let mut worst_rows = 0.0f64;
    let mut worst_where = String::new();

    for &(name, site_lat, site_lon) in SITES {
        for extent_km in [230.0f64, 460.0] {
            let bounds = ImageBounds::from_radar_site(site_lat, site_lon, extent_km);
            let merc_span = bounds.mercator_y_max - bounds.mercator_y_min;
            let lat_span = bounds.max_lat - bounds.min_lat;

            for row in 0..SIDE {
                let v = (row as f64 + 0.5) / SIDE as f64;
                let right = lat_of_mercator_y(bounds.mercator_y_max - v * merc_span);
                let wrong = bounds.max_lat - v * lat_span;

                let km = (right - wrong).abs() * EARTH_RADIUS_KM * std::f64::consts::PI / 180.0;
                if km > worst_km {
                    worst_km = km;
                    worst_rows = km / (2.0 * extent_km) * SIDE as f64;
                    worst_where = format!("{name} at {extent_km} km, row {row}");
                }
            }
        }
    }

    assert!(
        worst_km > 1.0,
        "a latitude-linear raster was expected to be kilometres out; \
         worst was {worst_km} km ({worst_rows} rows) at {worst_where}. \
         If this is now small the negative control has stopped controlling."
    );
}

/// The placement carried on the frame puts the raster exactly where projecting
/// its corners at draw time did.
#[test]
fn the_carried_placement_is_the_projected_corners() {
    const TOL_POINTS: f32 = 1.0e-3;

    let mut compared = 0usize;
    let mut widest = 0.0f32;

    for &(name, site_lat, site_lon) in SITES {
        for extent_km in [88.8f64, 230.0, 460.0] {
            let bounds = ImageBounds::from_radar_site(site_lat, site_lon, extent_km);
            let placed: squallar_geo::PlacedRaster = bounds.into();

            assert_eq!(
                placed.mercator_y,
                (bounds.mercator_y_min, bounds.mercator_y_max),
                "{name}: the placement's mercator span must be ImageBounds' own",
            );

            for &viewport in &[
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 900.0)),
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(432.0, 936.0)),
                egui::Rect::from_min_size(egui::pos2(120.0, 64.0), egui::vec2(800.0, 600.0)),
            ] {
                for &zoom in &[3.0f64, 7.0, 11.0] {
                    let mut memory = walkers::MapMemory::default();
                    memory.set_zoom(zoom).expect("a zoom walkers accepts");
                    for centre in [
                        (site_lat, site_lon),
                        (site_lat + 1.5, site_lon - 2.0),
                        (site_lat - 3.0, site_lon + 4.0),
                    ] {
                        let projector = walkers::Projector::new(
                            viewport,
                            &memory,
                            walkers::lat_lon(centre.0, centre.1),
                        );

                        let nw = projector
                            .project(walkers::lat_lon(bounds.max_lat, bounds.min_lon))
                            .to_pos2();
                        let se = projector
                            .project(walkers::lat_lon(bounds.min_lat, bounds.max_lon))
                            .to_pos2();
                        let was = egui::Rect::from_two_pos(nw, se);

                        let now = crate::overlay_cache::placed_rect(&projector, &placed);

                        assert!(
                            was.width() > 1.0 && was.height() > 1.0,
                            "{name} at {extent_km} km, zoom {zoom}: degenerate \
                             reference rect {was:?} — this triple compares nothing",
                        );

                        for (label, a, b) in [
                            ("left", was.left(), now.left()),
                            ("right", was.right(), now.right()),
                            ("top", was.top(), now.top()),
                            ("bottom", was.bottom(), now.bottom()),
                        ] {
                            let off = (a - b).abs();
                            widest = widest.max(off);
                            assert!(
                                off <= TOL_POINTS,
                                "{name} at {extent_km} km, zoom {zoom}, viewport \
                                 {viewport:?}, centre {centre:?}: {label} edge \
                                 moved {off} points ({a} → {b})",
                            );
                        }
                        compared += 1;
                    }
                }
            }
        }
    }

    assert!(
        compared >= 648,
        "only {compared} triples compared — the sampling collapsed",
    );
    assert!(
        widest.is_finite(),
        "the widest edge disagreement was {widest}, which is not a number",
    );
}
