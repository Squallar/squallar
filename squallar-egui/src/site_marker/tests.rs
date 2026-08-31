//! **The property the user reported violated: a site marker keeps its size on
//! glass while the map zooms under it.**
//!
//! The quantity under test is one number — the radius, in points, that the
//! marker occupies on the display — measured at two zooms a single gesture
//! could plausibly cross. Nothing here asserts a mechanism; it asserts the
//! size, and whichever way the map arrives at the size has to hold it.
//!
//! Before the marker moved out of the layer's raster this same measurement,
//! taken off the raster and the stretch its geographic corners were under,
//! read 6.4 points at zoom 6 and 25.7 at zoom 8.

use super::*;

/// The two zooms one gesture crosses. Two levels is an ordinary wheel run and
/// well inside a single pinch.
const READ_AT_ZOOMS: [f64; 2] = [6.0, 8.0];

fn canvas() -> egui::Rect {
    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1920.0, 1080.0))
}

/// **The measurement, taken off the path the app actually draws**: the marker
/// is painted through a real [`egui::Painter`] and the radius read back off the
/// shape the painter emitted, so nothing here models what the draw call does.
fn on_glass_radius_at(zoom: f64) -> f32 {
    let ctx = egui::Context::default();
    ctx.begin_pass(egui::RawInput {
        screen_rect: Some(canvas()),
        ..Default::default()
    });
    let painter = egui::Painter::new(ctx.clone(), egui::LayerId::background(), canvas());
    draw_site_marker(&painter, canvas().center(), zoom, MarkerRole::Ordinary);
    let output = ctx.end_pass();

    let mut filled = output
        .shapes
        .iter()
        .filter_map(|clipped| match clipped.shape {
            egui::Shape::Circle(circle) if circle.fill == MarkerRole::Ordinary.fill() => {
                Some(circle.radius)
            }
            _ => None,
        });
    let radius = filled
        .next()
        .expect("the marker must put a filled disc on glass");
    assert!(
        filled.next().is_none(),
        "one station drew more than one filled disc",
    );
    radius
}

/// The user's report, as an assertion.
///
/// The marker is allowed to grow with the map, but only on its own deliberate
/// ramp — [`MAX_GROWTH_PER_ZOOM`] points per zoom level. Anything steeper is
/// the map dragging the marker rather than the marker following the map, and
/// two zoom levels is where the two stories separate by a factor rather than
/// by a rounding: the ramp adds two points, a stretched texture multiplies by
/// four.
#[test]
fn a_site_marker_keeps_its_size_while_the_map_zooms_under_it() {
    let [lo, hi] = READ_AT_ZOOMS;
    let (r_lo, r_hi) = (on_glass_radius_at(lo), on_glass_radius_at(hi));

    let allowed = MAX_GROWTH_PER_ZOOM * (hi - lo) as f32;
    assert!(
        (r_hi - r_lo).abs() <= allowed,
        "a site marker measured {r_lo:.1} points across at zoom {lo} and \
         {r_hi:.1} at zoom {hi}, a change of {:.1} where at most {allowed:.1} \
         is the marker's own size ramp; the rest is the map scaling it, which \
         is what makes it balloon during a zoom and snap back afterwards",
        (r_hi - r_lo).abs(),
    );
}

/// **The layer still rasterizes, and that is load-bearing twice over.**
///
/// The marker left the raster; the layer did not. What stays behind is ground —
/// each station's coverage ring — and it is the Tier-2 rig's
/// `--expect-overlay-rasters` vehicle: the one texture overlay that is off by
/// default and reproducible with no upstream, so a run at any hour draws the
/// same picture. Turning this layer into a per-frame one would leave that check
/// reading `dispatched == 0`, which is the reading a genuinely broken overlay
/// pipeline gives. `ui_config::rig_seed_tests` guards the seed; this guards the
/// property the seed relies on.
#[test]
fn the_layer_the_rig_seeds_still_rasterizes() {
    use squallar_overlays::render::overlay_state::{OverlayRegistry, RenderMode};

    let registry = OverlayRegistry::with_handlers(crate::sources::all());
    let handler = registry
        .handlers()
        .find(|h| h.id() == squallar_source::id::known::RADAR_SITES)
        .expect("the radar site layer is registered");

    assert_eq!(
        handler.render_mode(),
        RenderMode::Texture,
        "the rig's overlay-raster vehicle stopped rasterizing",
    );
    assert!(
        !handler.default_enabled(),
        "a layer that is on by default is not a vehicle the rig can seed: \
         switching it on would stop meaning anything",
    );
}

/// The uncooperative control. Without it, a measurement that returned the same
/// constant whatever it was handed — a marker that had stopped being drawn,
/// say — would pass the test above.
#[test]
fn the_measurement_moves_when_the_marker_is_meant_to_move() {
    assert!(
        marker_shape(7.0).radius > marker_shape(0.0).radius,
        "the deliberate ramp must be visible to this fixture, or the test \
         above is comparing two readings of nothing",
    );
    assert!(
        on_glass_radius_at(READ_AT_ZOOMS[0]) > 0.0,
        "the fixture must put a measurable marker on glass",
    );
    assert_ne!(
        MarkerRole::Current.fill(),
        MarkerRole::Ordinary.fill(),
        "the three fills must be three fills, or the disc the measurement \
         picks out is not the one it thinks it is",
    );
}

/// **A station across the antimeridian is placed where it is, not a turn away.**
///
/// `walkers::Projector::project` is linear in longitude and folds nothing: from
/// a map centred at 170E, `PAEC` (written -165.295, and 25 degrees *east* of
/// that centre on the ground) projects to x = -947 on a 1920-point canvas —
/// 335 degrees the wrong way round, off the canvas, and culled by
/// `visible_radar_sites` before it can be drawn.
///
/// The raster the marker used to come from folded this itself
/// (`MercatorBounds::nearest_lon`) while the *name* beside it, which has always
/// come off this projection, did not — so the seam already showed nameless
/// dots. [`fold_into_turn`] is where both now agree.
///
/// Positions from `api.weather.gov/radar/stations`.
#[test]
fn a_station_across_the_seam_is_placed_where_it_is() {
    let mut memory = walkers::MapMemory::default();
    memory.set_zoom(3.0).expect("zoom 3 is in walkers' range");
    let projector = walkers::Projector::new(canvas(), &memory, walkers::lat_lon(45.0, 170.0));
    let world = world_width_in_points(&projector);
    let centre_x = canvas().center().x;

    let place = |lat: f64, lon: f64| {
        let p = projector.project(walkers::lat_lon(lat, lon)).to_pos2();
        egui::pos2(fold_into_turn(p.x, centre_x, world), p.y)
    };

    // `PGUA` is the control: written in the same turn as the centre, it lands
    // with or without the fold, so it cannot be what makes this test pass.
    let guam = place(13.4558, 144.8111);
    let nome = place(64.5114, -165.2950);
    let bethel = place(60.7919, -161.8764);

    assert!(
        canvas().contains(guam),
        "the control must land on the canvas, or this measures nothing: {guam:?}",
    );
    for (name, at, raw_lon) in [("PAEC", nome, -165.2950), ("PABC", bethel, -161.8764)] {
        assert!(
            canvas().contains(at),
            "{name} (written {raw_lon}, and east of a map centred at 170E) \
             landed at {at:?}, off a canvas of {:?}; a station a turn away in \
             the coordinates is a station in view on the ground",
            canvas(),
        );
    }
}

/// The fold's own edges, which the station fixture above cannot reach.
#[test]
fn folding_leaves_a_degenerate_turn_alone() {
    // In frame already: untouched, whatever the width.
    assert_eq!(fold_into_turn(500.0, 480.0, 2048.0), 500.0);
    // A projector with no usable scale must not move anything.
    for bad in [0.0, 1.0, f32::NAN, f32::INFINITY] {
        assert_eq!(
            fold_into_turn(-947.5, 960.0, bad),
            -947.5,
            "a world width of {bad} is not a turn to fold into",
        );
    }
    // And the fold is a whole number of turns, never a nudge.
    let folded = fold_into_turn(-947.5, 960.0, 2048.0);
    assert!(
        ((folded - -947.5) / 2048.0).fract().abs() < 1e-4,
        "folded by {} points, which is not a whole turn of 2048",
        folded - -947.5,
    );
}
