//! **The radar network's coverage is pixels, and this is what says so.**
//!
//! Everything else about this layer was pinned — that it describes a job, that
//! the job round-trips the wire, that the handler answers `has_data` — and none
//! of it looks at the picture. The failure that got through was a map drawing
//! site *labels* (per-frame egui text, straight off the table) with no ink at
//! all under them, which every one of those pins reads as green.
//!
//! **What this raster carries is ground, and that is the point.** The station
//! marker used to be baked in here at a radius in *points*, and the map places
//! this picture by its geographic corners — so a zoom gesture stretched the
//! marker to four times its size two levels in and snapped it back half a
//! second after the zoom stopped. The marker moved to
//! `squallar_egui::site_marker`, which paints it per frame; the 230 km coverage
//! ring stayed, because 230 km is 230 km and *should* scale with the map.
//!
//! The three fills are the layer's whole vocabulary, and they are not
//! decoration: blue/red/purple is how the map says "a radar", "the radar this
//! pane is on" and "the radar it is loading". A test that only counted
//! non-transparent pixels would pass on a raster that painted all three the
//! same, so each is asked for by colour.
//!
//! This layer is also the Tier-2 rig's `--expect-overlay-rasters` vehicle — the
//! one texture overlay that is off by default and reproducible with no upstream
//! — so what is asserted here is what the rig measures the pipeline with.
//!
//! Positions are the real ones from `api.weather.gov/radar/stations`, the same
//! catalogue the app fetches.

use super::*;

/// `KTLX` Twin Lakes, `KINX` Tulsa, `KVNX` Vance AFB — three WSR-88Ds inside
/// one southern-Plains viewport.
const KTLX: (f64, f64) = (35.3331, -97.2775);
const KINX: (f64, f64) = (36.1750, -95.5650);
const KVNX: (f64, f64) = (36.7410, -98.1280);

/// A viewport with all three inside it, comfortably off every edge.
fn viewport() -> GeoBounds {
    GeoBounds {
        max_lat: 38.0,
        min_lat: 34.0,
        min_lon: -100.0,
        max_lon: -94.0,
    }
}

const W: u32 = 400;
const H: u32 = 300;

fn site(name: &str, (lat, lon): (f64, f64)) -> RadarSiteInfo {
    RadarSiteInfo {
        name: name.to_string(),
        lat,
        lon,
        is_current: false,
        is_loading: false,
    }
}

fn input(sites: Vec<RadarSiteInfo>) -> SitesInput {
    SitesInput {
        sites,
        // The zoom a user actually looks at radars from.
        zoom: 7.0,
        is_dark: true,
        device_scale: 1.0,
    }
}

/// The raster's bytes are premultiplied; at full alpha that is the colour
/// itself, so the three fills below are their own literals.
fn count_pixels(out: &RasterizeOutput, rgba: [u8; 4]) -> usize {
    out.rgba.chunks_exact(4).filter(|px| *px == rgba).count()
}

const BLUE: [u8; 4] = [100, 150, 255, 255];
const RED: [u8; 4] = [255, 100, 100, 255];
const PURPLE: [u8; 4] = [160, 32, 240, 255];

/// **The floor one drawn ring has to clear.** The viewport is 6 degrees of
/// longitude across 400 px, so 230 km is tens of pixels of radius and the ring
/// is hundreds of pixels of circumference; anti-aliasing spreads a hairline
/// across neighbours and only fully-saturated pixels carry the literal, so a
/// good part of it is not counted. Fifty is far below what a drawn ring
/// produces and far above what a stray anti-aliased pixel could fake — a ring
/// that regressed to a dot, or to a single pixel, would fail it.
const RING_FLOOR: usize = 50;

/// Where the ink is, relative to the antenna: `(painted within a quarter of the
/// coverage radius, painted in a 2 px annulus at the coverage radius)`.
fn distance_bands(out: &RasterizeOutput, at: (f64, f64)) -> (usize, usize) {
    let mb = MercatorBounds::from_geo(&viewport());
    let (cx, cy) = mb.project(at.0, at.1, W as f32, H as f32);
    let (_, north) = mb.project(
        at.0 + super::COVERAGE_RADIUS_DEG_LAT,
        at.1,
        W as f32,
        H as f32,
    );
    let ring_r = (cy - north).abs();

    let (mut inner, mut on_ring) = (0usize, 0usize);
    for (i, px) in out.rgba.chunks_exact(4).enumerate() {
        if px[3] == 0 {
            continue;
        }
        let (x, y) = ((i as u32 % W) as f32, (i as u32 / W) as f32);
        let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
        if d < ring_r * 0.25 {
            inner += 1;
        } else if (d - ring_r).abs() <= 2.0 {
            on_ring += 1;
        }
    }
    (inner, on_ring)
}

/// **The property the marker's move turned on: this picture is ground.**
///
/// A ring at the coverage radius and *nothing at the antenna*. The second half
/// is what fails on a raster that went back to baking the marker: a filled disc
/// at the station is exactly the thing a zoom gesture used to stretch.
#[test]
fn a_stations_ink_is_its_coverage_ring_and_not_a_dot_at_the_antenna() {
    let out = rasterize_radar_sites(&input(vec![site("KTLX", KTLX)]), &viewport(), W, H);

    let (inner, on_ring) = distance_bands(&out, KTLX);
    assert!(
        on_ring >= RING_FLOOR,
        "the station painted {on_ring} pixels at its coverage radius; a drawn \
         ring is hundreds, so this layer is not putting coverage on the map",
    );
    assert_eq!(
        inner, 0,
        "the station painted {inner} pixels at the antenna. Ink there is sized \
         in points, not in kilometres, so the map stretches it with every zoom \
         gesture -- that is the marker, and it belongs in `site_marker`",
    );
}

/// **The ring is ground, so it is the same ring whatever the map zoom says.**
///
/// Same viewport, same texture, five zoom levels apart: the geometry may not
/// move, because 230 km did not.
#[test]
fn the_ring_is_the_same_size_whatever_zoom_the_picture_was_asked_for() {
    let at = |zoom: f64| {
        let mut inp = input(vec![site("KTLX", KTLX)]);
        inp.zoom = zoom;
        let out = rasterize_radar_sites(&inp, &viewport(), W, H);
        distance_bands(&out, KTLX)
    };
    let (inner_lo, ring_lo) = at(4.0);
    let (inner_hi, ring_hi) = at(9.0);

    assert_eq!(
        (inner_lo, inner_hi),
        (0, 0),
        "ink at the antenna is a point-sized thing baked into a picture the \
         map scales",
    );
    assert!(
        ring_lo >= RING_FLOOR && ring_hi >= RING_FLOOR,
        "both zooms must draw a ring to compare: {ring_lo} and {ring_hi}",
    );
}

#[test]
fn every_ordinary_site_paints_a_blue_ring() {
    let out = rasterize_radar_sites(
        &input(vec![
            site("KTLX", KTLX),
            site("KINX", KINX),
            site("KVNX", KVNX),
        ]),
        &viewport(),
        W,
        H,
    );

    let blue = count_pixels(&out, BLUE);
    assert!(
        blue >= RING_FLOOR * 3,
        "three radars in view painted {blue} blue pixels; a drawn ring is \
         hundreds each, so the map is drawing site labels with nothing under them",
    );
    assert_eq!(
        count_pixels(&out, RED),
        0,
        "no pane is on a radar here, so nothing may wear the current-site red",
    );
    assert_eq!(count_pixels(&out, PURPLE), 0, "nothing is loading");
}

#[test]
fn the_panes_own_radar_is_red_and_the_one_it_is_loading_is_purple() {
    let mut sites = vec![site("KTLX", KTLX), site("KINX", KINX), site("KVNX", KVNX)];
    sites[0].is_current = true;
    sites[1].is_loading = true;

    let out = rasterize_radar_sites(&input(sites), &viewport(), W, H);

    let (red, purple, blue) = (
        count_pixels(&out, RED),
        count_pixels(&out, PURPLE),
        count_pixels(&out, BLUE),
    );
    assert!(
        red >= RING_FLOOR,
        "the pane's own radar painted {red} red pixels",
    );
    assert!(
        purple >= RING_FLOOR,
        "the loading radar painted {purple} purple pixels",
    );
    assert!(
        blue >= RING_FLOOR,
        "the third radar, which is neither, painted {blue} blue pixels",
    );
    assert!(
        red < blue + purple + red,
        "the three fills must be three fills",
    );
}

/// The uncooperative control: hand the same viewport no rows and the picture
/// must be empty. Without this, a rasterizer that painted the whole texture
/// blue would pass the tests above.
#[test]
fn an_empty_table_paints_nothing_at_all() {
    let out = rasterize_radar_sites(&input(Vec::new()), &viewport(), W, H);
    assert!(
        out.rgba.iter().all(|&b| b == 0),
        "an empty site list still put ink on the texture",
    );
}

/// And the other control: a viewport nowhere near these radars draws nothing,
/// so the counts above are a function of where the radars are and not of the
/// fixture merely being non-empty.
#[test]
fn radars_outside_the_viewport_paint_nothing() {
    let out = rasterize_radar_sites(
        &input(vec![
            site("KTLX", KTLX),
            site("KINX", KINX),
            site("KVNX", KVNX),
        ]),
        &GeoBounds {
            max_lat: 48.0,
            min_lat: 44.0,
            min_lon: -76.0,
            max_lon: -70.0,
        },
        W,
        H,
    );
    assert_eq!(
        count_pixels(&out, BLUE),
        0,
        "a New England viewport drew Oklahoma's radars",
    );
}

/// **A radar off the texture still covers ground on it**, which the marker's
/// cull could not express: it culled on the antenna, and 230 km is a long way.
#[test]
fn a_radar_just_off_the_texture_still_paints_the_ground_it_covers() {
    // KTLX's latitude, moved 1.2 degrees west of the viewport's western edge:
    // outside the picture, well inside its own coverage of it.
    let just_west = site("KTLX", (35.3331, -101.2));
    let out = rasterize_radar_sites(&input(vec![just_west]), &viewport(), W, H);
    assert!(
        count_pixels(&out, BLUE) > 0,
        "a radar 1.2 degrees west of the viewport covers ground inside it and \
         must paint the arc that lands",
    );
}
