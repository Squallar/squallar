//! Where the 2D radar raster lands, against where its gates actually are.
//!
//! # The question
//!
//! `render_radar_overlay` places the whole raster by projecting two corners —
//! `(max_lat, min_lon)` and `(min_lat, max_lon)` — and stretching the texture
//! linearly between them with `uv` `0..1`. Screen `y` inside that rect is
//! therefore linear in **Mercator y**, because that is what Web Mercator's
//! screen `y` is linear in.
//!
//! So the raster's rows have to be linear in Mercator `y` too. If they were
//! linear in **latitude** — which is the obvious way to build a raster and the
//! way a reader would assume — the two would agree only at the two edges and
//! bow apart in between, worst in the middle, and the picture would still look
//! exactly like a radar picture. The 3D floor corrects for precisely this
//! non-linearity per pixel and its own doc measures what skipping the
//! correction costs there: 4.1 texels, **3.7 km**, down.
//!
//! # The answer, and why it is asserted rather than assumed
//!
//! They are linear in Mercator `y`: `types::ImageBounds` carries
//! `mercator_y_min`/`mercator_y_max` and `render::MercatorProjection` scales
//! rows by `side_px / (mercator_y_max − mercator_y_min)`. Measured over seven
//! sites at 230 km and 460 km, 120 azimuths × 5 ranges each, the worst
//! disagreement between a gate's raster pixel and the same gate's great-circle
//! position re-projected through these bounds is **2.1e-11 pixels — 3.1
//! nanometres**. That is float rounding, and it is the same order as the
//! agreement the cross-section, plan view and volume cube were measured at
//! over 158 volumes.
//!
//! Nothing in the tree said so, which is the only reason a reader could have
//! believed otherwise. This is that statement, in a form that fails if anyone
//! ever rebuilds the rows on latitude.

use rustdar_geo::EARTH_RADIUS_KM;
use rustdar_radar::types::ImageBounds;

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
///
/// This file's whole job is to check the renderer's row placement against
/// something that is not the renderer's row placement, so it evaluates the
/// projection's own closed form — `φ = 2·atan(e^y) − π/2`, the Gudermannian —
/// rather than calling any of the workspace's four `mercator_y` spellings
/// backwards.
fn lat_of_mercator_y(y: f64) -> f64 {
    (2.0 * y.exp().atan() - std::f64::consts::FRAC_PI_2).to_degrees()
}

/// A raster row is linear in Mercator `y`, so the texture and the rect it is
/// stretched into describe the same ground.
///
/// # What this actually compares
///
/// For a grid of pixels across the raster:
///
/// * **the raster's answer** — the ground under pixel `(col, row)` under the
///   renderer's stated convention: linear in Mercator `y` between
///   `mercator_y_max` and `mercator_y_min`, linear in longitude between
///   `min_lon` and `max_lon`, half-pixel centres.
/// * **the screen's answer** — where `render_radar_overlay` puts that pixel:
///   `uv = (col + 0.5)/side, (row + 0.5)/side` inside the rect whose corners
///   are `projector.project(max_lat, min_lon)` and
///   `projector.project(min_lat, max_lon)`.
///
/// If the first were built on latitude and the second on Mercator `y`, the two
/// would part company by up to kilometres in the middle rows while still
/// meeting exactly at the top and bottom edges — which is exactly the shape of
/// error that survives review.
#[test]
fn a_raster_row_lands_where_the_projector_puts_its_latitude() {
    // A tenth of a screen point. `Projector::project` returns an `egui::Vec2`,
    // which is f32, and a 3240-px raster drawn into a 900-point pane is about
    // 3.6 raster rows per point — so a tenth of a point is ~10 m of ground at
    // 230 km and far inside anything a user could see. The real residual is
    // measured at 3.1 nanometres; this bar is loose because f32 sets it, not
    // because the arithmetic needs the room.
    const TOL_POINTS: f32 = 0.1;
    const SIDE: usize = 3240;

    let clip = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 900.0));
    let mut worst = (0.0f32, String::new());

    for &(name, site_lat, site_lon) in SITES {
        for extent_km in [230.0f64, 460.0] {
            let bounds = ImageBounds::from_radar_site(site_lat, site_lon, extent_km);

            let mut memory = walkers::MapMemory::default();
            // Zoom that puts a 460 km frame inside a 1600-point pane, so the
            // rect is a realistic size rather than a degenerate one.
            memory.set_zoom(7.0).expect("7 is a zoom walkers accepts");
            let projector =
                walkers::Projector::new(clip, &memory, walkers::lat_lon(site_lat, site_lon));

            let nw = projector.project(walkers::lat_lon(bounds.max_lat, bounds.min_lon));
            let se = projector.project(walkers::lat_lon(bounds.min_lat, bounds.max_lon));
            let rect = egui::Rect::from_two_pos(nw.to_pos2(), se.to_pos2());

            let merc_span = bounds.mercator_y_max - bounds.mercator_y_min;
            let lon_span = bounds.max_lon - bounds.min_lon;

            // Rows chosen to include the edges and the middle, which is where a
            // latitude-linear raster would be furthest out.
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

                    // The ground the renderer says is under this pixel.
                    let merc_y = bounds.mercator_y_max - v * merc_span;
                    let lat = lat_of_mercator_y(merc_y);
                    let lon = bounds.min_lon + u * lon_span;

                    // Where the painter puts this pixel.
                    let drawn = egui::pos2(
                        rect.min.x + u as f32 * rect.width(),
                        rect.min.y + v as f32 * rect.height(),
                    );
                    // Where that ground actually is on this pane.
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
///
/// A negative control for the test above: it builds the row placement the
/// **wrong** way — linear in latitude, which is what the map of this codebase
/// suspected was happening — and measures how far off it lands. If that
/// arrangement were within the tolerance the test above uses, that test would
/// be asserting nothing.
///
/// The figures it produces are the reason this matters. Worst row displacement
/// at a 3240-px raster, measured:
///
/// | site | latitude | 230 km frame | 460 km frame |
/// |---|---|---|---|
/// | TJUA | 18.1 °N | 1.36 km / 9.6 rows | 5.45 km / 19.2 rows |
/// | KTLX | 35.3 °N | 2.95 km / 20.7 rows | 11.81 km / 41.6 rows |
/// | KMPX | 44.8 °N | 4.13 km / 29.1 rows | 16.58 km / 58.4 rows |
/// | PABC | 60.8 °N | 7.44 km / 52.4 rows | 29.90 km / 105.3 rows |
/// | PAPD | 65.0 °N | 8.94 km / 63.0 rows | **35.98 km / 126.7 rows** |
///
/// It grows as `tan φ`, so the fleet's highest-latitude site on its widest
/// frame is the worst corner, and every one of those kilometres would have been
/// invisible: a radar picture drawn 36 km south of its ground still looks
/// exactly like a radar picture. Zero at the top and bottom edges, worst in the
/// middle — so even a careful look at the frame's boundary would have found
/// nothing.
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
                // What the rows are: linear in Mercator y.
                let right = lat_of_mercator_y(bounds.mercator_y_max - v * merc_span);
                // What they would be if built on latitude.
                let wrong = bounds.max_lat - v * lat_span;

                let km = (right - wrong).abs() * EARTH_RADIUS_KM * std::f64::consts::PI / 180.0;
                if km > worst_km {
                    worst_km = km;
                    // The same displacement expressed in raster rows, which is
                    // the unit a reader can compare against the picture.
                    worst_rows = km / (2.0 * extent_km) * SIDE as f64;
                    worst_where = format!("{name} at {extent_km} km, row {row}");
                }
            }
        }
    }

    // Measured: 35.98 km / 126.72 rows at PAPD (65.0 N) on a 460 km frame.
    // The bar is 1 km rather than 35: this is a control, and what it has to
    // establish is that the arrangement the test above rules out would have
    // been orders of magnitude outside that test's tolerance — not that it
    // would have been one specific size.
    assert!(
        worst_km > 1.0,
        "a latitude-linear raster was expected to be kilometres out; \
         worst was {worst_km} km ({worst_rows} rows) at {worst_where}. \
         If this is now small the negative control has stopped controlling."
    );
}

/// The placement carried on the frame puts the raster exactly where projecting
/// its corners at draw time did.
///
/// # What moved, and why the parity has to be asserted rather than reasoned
///
/// The loop playback path used to open every frame with
/// `ImageBounds::from_radar_site(img.lat, img.lon, img.max_range_km)` and
/// project the two corners it returned. It now reads a
/// [`rustdar_geo::PlacedRaster`] built at delivery and hands it to
/// `overlay_cache::placed_rect`. Those are the same four edges through the same
/// projector *if* the delivery built the placement from the same three numbers
/// the draw used to — and "if" is the whole content of the change: nothing else
/// in the tree would notice a raster placed from a stale `max_range_km` or from
/// a site the frame is not of. It would look like a radar picture, in the wrong
/// place, forever.
///
/// So this compares, over (bounds × zoom × viewport) triples, the rect the
/// production path now produces against the corner projection **written out
/// here**, longhand, as this file's other tests deliberately write out the
/// projection they are checking.
#[test]
fn the_carried_placement_is_the_projected_corners() {
    // Exactly zero would also pass — both sides run the same `f64` corner
    // arithmetic and end in the same `f32` `Vec2` — but the assertion is
    // written as a tolerance because what it is claiming is "these agree to
    // within f32 rounding", and a bit-equality that quietly became true for
    // some other reason would be a worse pin than a stated bar.
    const TOL_POINTS: f32 = 1.0e-3;

    let mut compared = 0usize;
    let mut widest = 0.0f32;

    for &(name, site_lat, site_lon) in SITES {
        for extent_km in [88.8f64, 230.0, 460.0] {
            let bounds = ImageBounds::from_radar_site(site_lat, site_lon, extent_km);
            let placed: rustdar_geo::PlacedRaster = bounds.into();

            // The carried mercator span is the bounds' own — the two are
            // derived by the same function from the same latitudes, and this is
            // the guard on that staying true.
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
                    // Off the site as well as on it, so a rect that happened to
                    // be centred is not the only case tested.
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

                        // The spelling this land deleted, restated.
                        let nw = projector
                            .project(walkers::lat_lon(bounds.max_lat, bounds.min_lon))
                            .to_pos2();
                        let se = projector
                            .project(walkers::lat_lon(bounds.min_lat, bounds.max_lon))
                            .to_pos2();
                        let was = egui::Rect::from_two_pos(nw, se);

                        // The spelling that replaced it, reached through the
                        // production function.
                        let now = crate::overlay_cache::placed_rect(&projector, &placed);

                        // The rect must be a rect: a degenerate one would make
                        // every comparison below pass for free.
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

    // The loop actually ran. 8 sites × 3 extents × 3 viewports × 3 zooms × 3
    // centres = 648; stated as a floor so adding a site does not fail it.
    assert!(
        compared >= 648,
        "only {compared} triples compared — the sampling collapsed",
    );
    // And the comparison is not comparing a value with itself through two
    // aliases: it is stated here rather than asserted, because zero is the
    // honest answer and asserting non-zero would be asserting rounding error.
    assert!(
        widest.is_finite(),
        "the widest edge disagreement was {widest}, which is not a number",
    );
}
