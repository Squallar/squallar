//! **The site markers are pixels, and this is what says so.**
//!
//! Everything else about this layer was pinned — that it describes a job, that
//! the job round-trips the wire, that the handler answers `has_data` — and
//! none of it looks at the picture. The failure that got through was a map
//! drawing site *labels* (per-frame egui text, straight off the table) with no
//! *markers* under them (this raster, off a copy of the table), which every
//! one of those pins reads as green.
//!
//! The three fills are the layer's whole vocabulary, and they are not
//! decoration: blue/red/purple is how the map says "a radar", "the radar this
//! pane is on" and "the radar it is loading". A test that only counted
//! non-transparent pixels would pass on a raster that painted all three the
//! same, so each is asked for by colour.
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
        // Above the label-plate threshold and at the marker radius cap, which
        // is the zoom a user actually looks at radars from.
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

/// **The floor a filled disc has to clear.** The marker radius at zoom 7 is
/// `(5 + 7).clamp(4, 12) = 12` px, so the fill inside the white ring is on the
/// order of 300 px; anti-aliasing and the stroke eat the rim. Fifty is far
/// below what a drawn disc produces and far above what a stray anti-aliased
/// pixel could fake — a marker that regressed to a dot, a ring with no fill,
/// or a single pixel would all fail it.
const DISC_FLOOR: usize = 50;

#[test]
fn every_ordinary_site_paints_a_blue_disc() {
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
        blue >= DISC_FLOOR * 3,
        "three radars in view painted {blue} blue pixels; a filled marker is \
         ~300 each, so the map is drawing site labels with nothing under them",
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
        red >= DISC_FLOOR,
        "the pane's own radar painted {red} red pixels",
    );
    assert!(
        purple >= DISC_FLOOR,
        "the loading radar painted {purple} purple pixels",
    );
    assert!(
        blue >= DISC_FLOOR,
        "the third radar, which is neither, painted {blue} blue pixels",
    );
    assert!(
        red < blue + purple + red,
        "the three fills must be three fills",
    );
}

/// The uncooperative control: hand the same viewport no rows and the picture
/// must be empty. Without this, a rasterizer that painted the whole texture
/// blue would pass the test above.
#[test]
fn an_empty_table_paints_nothing_at_all() {
    let out = rasterize_radar_sites(&input(Vec::new()), &viewport(), W, H);
    assert!(
        out.rgba.iter().all(|&b| b == 0),
        "an empty site list still put ink on the texture",
    );
}

/// And the other control: a viewport nowhere near these radars draws no
/// markers, so the count above is a function of where the radars are and not
/// of the fixture merely being non-empty.
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
