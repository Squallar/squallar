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

    // The marker is a textured quad pair; its disc's radius on glass is the
    // quad's half side less the ring and the sprite's edge ramp, which are
    // the two things the quad is wider than the disc by.
    let mut quads = output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Mesh(mesh)
                if mesh
                    .vertices
                    .iter()
                    .any(|v| v.color == MarkerRole::Ordinary.fill()) =>
            {
                Some(mesh.calc_bounds())
            }
            _ => None,
        });
    let bounds = quads
        .next()
        .expect("the marker must put a disc in its role's colour on glass");
    assert!(
        quads.next().is_none(),
        "one station drew more than one marker",
    );
    let half = bounds.width() / 2.0;
    half - marker_shape(zoom).stroke - SPRITE_EDGE_PX / ctx.pixels_per_point()
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

/// **The split, as two assertions: this layer draws nothing but glass, and the
/// one that took its ground is the rig's vehicle.**
///
/// Everything `RadarSites` puts on the map is now a length in points — the
/// marker, the station name, and the selected station's coverage ring — so it is
/// `PerFrameDirect` and answers no job at all. What was genuinely ground, the
/// network's 230 km coverage, is `RadarCoverage`, and *that* is the Tier-2 rig's
/// `--expect-overlay-rasters` vehicle: the one texture overlay that is off by
/// default and reproducible with no upstream, so a run at any hour draws the
/// same picture. Turning it into a per-frame layer too would leave that check
/// reading `dispatched == 0`, which is the reading a genuinely broken overlay
/// pipeline gives. `ui_config::rig_seed_tests` guards the seed; this guards the
/// properties the seed relies on.
#[test]
fn the_sites_layer_is_screen_space_and_the_coverage_layer_is_the_rigs_vehicle() {
    use squallar_overlays::render::overlay_state::{OverlayRegistry, RenderMode};

    let registry = OverlayRegistry::with_handlers(crate::sources::all());
    let mode = |id| {
        registry
            .handlers()
            .find(|h| h.id() == id)
            .unwrap_or_else(|| panic!("{id:?} is registered"))
            .render_mode()
    };

    assert_eq!(
        mode(squallar_source::id::known::RADAR_SITES),
        RenderMode::PerFrameDirect,
        "the site layer went back to rasterizing; a marker baked into a picture \
         placed by its geographic corners is stretched by every zoom gesture, \
         and a ring that arrives a round trip after the tap is not selection \
         feedback",
    );
    assert_eq!(
        mode(squallar_source::id::known::RADAR_COVERAGE),
        RenderMode::Texture,
        "the rig's overlay-raster vehicle stopped rasterizing",
    );

    let coverage = registry
        .handlers()
        .find(|h| h.id() == squallar_source::id::known::RADAR_COVERAGE)
        .expect("the coverage layer is registered");
    assert!(
        !coverage.default_enabled(),
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

/// **Blue, red, purple — and this is now the only place it is said.**
///
/// The three fills used to be pinned off the overlay raster, which drew one ring
/// per station in the colour that said "a radar", "the radar this pane is on"
/// and "the radar it is loading". That raster no longer knows which station a
/// pane is on (`CoverageSite` carries a position and nothing else), so the
/// vocabulary lives here with the marker that paints it. A test that only
/// counted discs would pass on a map that painted all three the same.
#[test]
fn the_three_marker_roles_are_three_colours() {
    let fills = [
        MarkerRole::Ordinary.fill(),
        MarkerRole::Current.fill(),
        MarkerRole::Loading.fill(),
    ];
    let distinct: std::collections::HashSet<_> = fills.iter().collect();
    assert_eq!(
        distinct.len(),
        3,
        "the map has no other way to say which station it is on; got {fills:?}",
    );
}

/// **The selected station's ring is 230 km, measured off the projector.**
///
/// The radius is checked against the same projection the ring is drawn through
/// rather than against a number written here: an independently computed
/// "expected" radius would be a second definition of the coverage distance, and
/// `geodesy_one_definition` refuses those for a reason — a 230/111.32 written
/// from memory is about 250 m out on this ring.
#[test]
fn the_ring_is_the_projected_coverage_radius() {
    let mut memory = walkers::MapMemory::default();
    memory.set_zoom(7.0).expect("zoom 7 is in walkers' range");
    let projector = walkers::Projector::new(canvas(), &memory, walkers::lat_lon(35.3331, -97.2775));

    let placement = ring_placement(&projector, 35.3331, -97.2775, canvas().center().x, {
        world_width_in_points(&projector)
    })
    .expect("a station at the centre of a zoom-7 canvas draws its ring");

    let here = projector
        .project(walkers::lat_lon(35.3331, -97.2775))
        .to_pos2();
    let north = projector
        .project(walkers::lat_lon(
            35.3331 + squallar_overlays::render::rasterize::COVERAGE_RADIUS_DEG_LAT,
            -97.2775,
        ))
        .to_pos2();

    assert!(
        (placement.radius - (here.y - north.y).abs()).abs() < 0.01,
        "the ring came back {} points where the projector puts one coverage \
         radius at {}",
        placement.radius,
        (here.y - north.y).abs(),
    );
    // The uncooperative half: a radius that is merely non-zero would pass the
    // check above on a ring the projector never saw.
    assert!(
        placement.radius > MIN_RING_RADIUS_POINTS,
        "a zoom-7 coverage radius must be well clear of the legibility floor; \
         got {}",
        placement.radius,
    );
}

/// Pulled far enough back and the ring is smaller than the dot over it, so it is
/// not drawn at all rather than drawn as a smudge round the marker.
///
/// **The threshold is crossed inside this test rather than asserted at one
/// zoom.** A single-zoom assertion is a fixture that goes stale the moment the
/// floor or the tile size moves; walking out until the projector puts 230 km
/// under the floor, and checking the answer flips exactly there, is the property
/// itself.
#[test]
fn a_ring_smaller_than_its_marker_is_not_drawn() {
    let place_at = |zoom: f64| {
        let mut memory = walkers::MapMemory::default();
        memory
            .set_zoom(zoom)
            .unwrap_or_else(|_| panic!("zoom {zoom} is in walkers' range"));
        let projector = walkers::Projector::new(canvas(), &memory, walkers::lat_lon(35.0, -97.0));
        ring_placement(
            &projector,
            35.0,
            -97.0,
            canvas().center().x,
            world_width_in_points(&projector),
        )
    };

    let drawn = place_at(3.0).expect("230 km is well clear of the floor at zoom 3");
    assert!(drawn.radius > MIN_RING_RADIUS_POINTS);

    assert_eq!(
        place_at(0.0),
        None,
        "at zoom 0 the whole world is one tile and 230 km is under \
         {MIN_RING_RADIUS_POINTS} points, so the ring must be withheld rather \
         than drawn as a smudge round the marker",
    );
}

/// **The ring folds across the seam exactly as the marker does.**
///
/// The same defect, one layer along: a station written -165.295 seen from 170E
/// projects a turn the wrong way, and a ring drawn at that x is drawn off the
/// world. `PGUA` is the control — in the centre's own turn, so it lands with or
/// without the fold and cannot be what makes this pass.
#[test]
fn a_ring_across_the_seam_is_drawn_where_its_station_is() {
    let mut memory = walkers::MapMemory::default();
    memory.set_zoom(5.0).expect("zoom 5 is in walkers' range");
    let projector = walkers::Projector::new(canvas(), &memory, walkers::lat_lon(60.0, 170.0));
    let centre_x = canvas().center().x;
    let world = world_width_in_points(&projector);

    let at = |lat: f64, lon: f64| {
        ring_placement(&projector, lat, lon, centre_x, world)
            .unwrap_or_else(|| panic!("{lat},{lon} must place a ring at zoom 5"))
            .center
    };

    let guam = at(13.4558, 144.8111);
    let nome = at(64.5114, -165.2950);

    assert!(
        guam.x.abs() < world,
        "the control landed at {guam:?}, which is already a turn out",
    );
    assert!(
        canvas().expand(400.0).contains(nome),
        "PAEC's ring centre landed at {nome:?}; unfolded it is ~950 points \
         negative on a canvas of {:?}, and the ring draws off the world",
        canvas(),
    );
}

/// **The order labels compete in is a total order with no ties.**
///
/// This is the whole of the stability argument: nothing in the key is measured
/// off the viewport, so panning cannot reorder the contest, and the index
/// tie-break means two stations of one rank cannot swap depending on which was
/// projected first.
#[test]
fn the_label_order_is_selected_first_then_the_tables_own_order() {
    use LabelRank::*;
    // Deliberately shuffled relative to rank, so a function that returned the
    // input order would fail.
    let ranks = [Secondary, Primary, Selected, Primary, Current, Secondary];
    assert_eq!(
        label_order(&ranks),
        vec![2, 4, 1, 3, 0, 5],
        "selected, then current, then the WSR-88Ds in table order, then the \
         terminal radars in table order",
    );

    // The property, not the fixture: the same ranks always give the same order.
    assert_eq!(label_order(&ranks), label_order(&ranks));
    assert!(
        label_order(&[]).is_empty(),
        "an empty viewport lays out no labels",
    );
}

/// Lay a set of names out and report which ones drew.
fn placed_names(anchors: &[(egui::Pos2, &str)]) -> Vec<String> {
    let ctx = egui::Context::default();
    ctx.begin_pass(egui::RawInput {
        screen_rect: Some(canvas()),
        ..Default::default()
    });
    let painter = egui::Painter::new(ctx.clone(), egui::LayerId::background(), canvas());
    let mut occupied = walkers::OccupiedAreas::new();
    let mut drew = Vec::new();
    for (at, name) in anchors {
        if try_draw_site_label(
            &painter,
            &mut occupied,
            *at,
            name,
            egui::FontId::monospace(10.0),
            egui::Color32::WHITE,
            true,
        ) {
            drew.push((*name).to_owned());
        }
    }
    let _ = ctx.end_pass();
    drew
}

/// **The user's report, as an assertion: `KMPX` and `KMSP` stop drawing on top
/// of each other.**
///
/// Minneapolis' WSR-88D and the terminal radar beside it are 11 km apart, which
/// at a continental zoom is a few points — the two plates overlapped and the
/// glyphs interleaved into the nonsense string `KMTMSP`. Exactly one of the two
/// may draw.
///
/// **An equality and not a ceiling.** "At most one" is also satisfied by drawing
/// neither, which is the failure this repo has shipped before: a declutter that
/// suppresses everything reads green on any one-sided bound.
#[test]
fn two_labels_a_few_points_apart_place_exactly_one() {
    let drew = placed_names(&[
        (egui::pos2(800.0, 500.0), "KMPX"),
        (egui::pos2(803.0, 502.0), "KMSP"),
    ]);
    assert_eq!(
        drew,
        vec!["KMPX".to_string()],
        "two plates three points apart overlap; the first to ask keeps the \
         spot and the second must be dropped whole rather than drawn through",
    );
}

/// The Detroit cluster, which the user named: `KDTX`, `TDTW` and `KDXX` inside a
/// few points of each other read as one smear.
#[test]
fn a_dense_cluster_still_places_a_name() {
    let drew = placed_names(&[
        (egui::pos2(600.0, 400.0), "KDTX"),
        (egui::pos2(604.0, 401.0), "TDTW"),
        (egui::pos2(598.0, 403.0), "KDXX"),
    ]);
    assert_eq!(
        drew,
        vec!["KDTX".to_string()],
        "the first name to ask finds nothing claimed and therefore always \
         draws; a cluster that came back with no names at all is the \
         silent-partial-success this gate exists to catch",
    );
}

/// Well-separated names all draw. Without this, a declutter that dropped
/// everything but the first would pass both tests above.
#[test]
fn labels_that_do_not_collide_all_draw() {
    let drew = placed_names(&[
        (egui::pos2(200.0, 200.0), "KTLX"),
        (egui::pos2(700.0, 300.0), "KINX"),
        (egui::pos2(1200.0, 800.0), "KVNX"),
    ]);
    assert_eq!(
        drew,
        vec!["KTLX".to_string(), "KINX".to_string(), "KVNX".to_string()],
        "three names hundreds of points apart claim disjoint screen and must \
         all draw",
    );
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
