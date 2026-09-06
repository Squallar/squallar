//! The horizontal wrap: what a viewport straddling the antimeridian draws, and
//! what it may not draw twice.
//!
//! **Two different gates over two different domains, and neither implies the
//! other.** One is over *tiles*: a viewport reaching past the seam must ask for
//! columns from both ends of the grid, or the far side is simply missing. The
//! other is over *ground*: no geographic position may be painted twice in one
//! frame. A gate stated over tile coordinates passes happily while the same
//! ground is drawn from two turns of the world, which is exactly what a doubled
//! continent is — so the second is stated in degrees of longitude and never in
//! columns.

use super::*;

/// The canvas every case here draws into. Landscape, so the zoom floor is taken
/// from the width and the world is exactly as wide as the viewport at it.
const CANVAS: egui::Vec2 = egui::vec2(1920.0, 1080.0);

fn canvas() -> egui::Rect {
    egui::Rect::from_min_size(egui::Pos2::ZERO, CANVAS)
}

/// How far a placement may miss by before it counts as a miss, in points.
///
/// The same order as the sibling suite's, and for the same reason: a rect is
/// returned in `f32` and the world at the floor is up to two ulps short of the
/// viewport.
const TOL_POINTS: f64 = 0.5;

/// A map centred at `(lat, lon)` — **`lon` unfolded**, so 190 means a map panned
/// a whole ten degrees past the antimeridian and not one at −170.
///
/// That is the frame walkers keeps its centre in; see
/// `vendor/walkers/src/viewport.rs`.
fn map_at(lat: f64, lon: f64, zoom: f64) -> walkers::Projector {
    map_on(canvas(), lat, lon, zoom)
}

/// [`map_at`], on a pane other than [`canvas`].
///
/// The zoom floor is a function of the pane's larger side, so a claim made about
/// the floor on one pane is a claim about one number. `VIEWPORTS` is what makes
/// the floor sweep a sweep.
fn map_on(rect: egui::Rect, lat: f64, lon: f64, zoom: f64) -> walkers::Projector {
    let mut memory = walkers::MapMemory::default();
    memory
        .set_zoom(zoom)
        .expect("the zoom is in walkers' range");
    walkers::Projector::new(rect, &memory, walkers::lat_lon(lat, lon))
}

/// Panes the floor sweep visits, in points.
///
/// The 2878x1651 entry is the window the phase-1 report was written from, and
/// the one whose world lands **short** of its own viewport at the floor —
/// `2f64.powf(x.log2())` is not `x`. A sweep that never meets a short world
/// cannot exercise the tolerance the coverage claim carries, which is why the
/// list is a list.
const VIEWPORTS: &[(f32, f32)] = &[
    (1920.0, 1080.0),
    (2878.0, 1651.0),
    (1651.0, 2878.0),
    (1280.0, 800.0),
    (390.0, 844.0),
    (1024.0, 1024.0),
];

/// The width of the whole world, in points, as this projector draws it.
///
/// Read off the projector rather than recomputed from the zoom, so it cannot
/// disagree with what the columns are actually placed by — and in `f64`, because
/// at the zoom floor the world is *short* of the viewport by an amount no `f32`
/// can hold.
fn world_points(projector: &walkers::Projector) -> f64 {
    projector.world_pixels()
}

/// One drawn column: the column index the walk names, the grid column it is
/// asked for, and the ground it actually paints into the viewport.
#[derive(Debug, Clone, Copy)]
struct Painted {
    /// The column the walk names. May be off either end of the grid.
    column: i64,
    /// The column the source is asked for: [`squallar_geo::wrap_tile_x`] of it.
    grid_column: u32,
    /// Ground painted, in degrees east, **not** folded — `[west, east)` with
    /// `east > west`, so an interval crossing the seam reads e.g. 179.5..180.4.
    west_lon: f64,
    east_lon: f64,
}

impl Painted {
    fn ground_degrees(&self) -> f64 {
        self.east_lon - self.west_lon
    }
}

/// Every column one frame of `draw_tile_layer`'s walk would place, with the
/// ground each one actually gets onto the glass.
///
/// This reproduces the walk rather than calling it: the walk needs a live
/// `HttpsTiles` and a painter, and what is under test here is the geometry it
/// walks — `tile_span`, `wrap_tile_x` and `Projector::tile_rect_at`, in the
/// order and with the arguments `ui_map_overlays::draw_tile_layer` uses them
/// in. `the_wrap_walk_here_is_the_walk_draw_tile_layer_makes` pins that they
/// have not drifted apart.
fn painted_columns(projector: &walkers::Projector, tile_zoom: u8) -> Vec<Painted> {
    painted_columns_on(canvas(), projector, tile_zoom)
}

/// [`painted_columns`], on a pane other than [`canvas`].
fn painted_columns_on(
    clip: egui::Rect,
    projector: &walkers::Projector,
    tile_zoom: u8,
) -> Vec<Painted> {
    let span = tile_span(projector, clip, tile_zoom);
    let side_ground = 360.0 / f64::from(2u32.saturating_pow(u32::from(tile_zoom)));

    let mut out = Vec::new();
    for column in span.west..=span.east {
        let rect = projector.tile_rect_at(column, span.north, tile_zoom);
        let left = f64::from(rect.left()).max(f64::from(clip.left()));
        let right = f64::from(rect.right()).min(f64::from(clip.right()));
        if right <= left {
            // Placed wholly off the glass: it paints no ground at all.
            continue;
        }

        // Screen back to ground, by the column's own affine: the column's west
        // edge is at `rect.left()` and covers `side_ground` degrees across
        // `rect.width()` points.
        let per_point = side_ground / f64::from(rect.width());
        let west_edge = squallar_geo::tile_to_lon_unbounded(column, tile_zoom);
        out.push(Painted {
            column,
            grid_column: squallar_geo::wrap_tile_x(column, tile_zoom),
            west_lon: west_edge + (left - f64::from(rect.left())) * per_point,
            east_lon: west_edge + (right - f64::from(rect.left())) * per_point,
        });
    }
    out
}

/// Whether two half-open longitude intervals name any ground in common, **on the
/// circle** — the only domain the question has an answer in.
///
/// `[179.5, 180.4)` and `[-180.4, -179.5)` are the same ground written two
/// turns apart, and a comparison of the raw numbers says they are disjoint.
/// Both are carried onto `[0, 360)` and every turn of the second is tried
/// against the first, which is what makes "the same ground" mean the same ground
/// and not the same number.
fn ground_overlap_degrees(a: &Painted, b: &Painted) -> f64 {
    let base = a.west_lon.rem_euclid(360.0);
    let a0: f64 = 0.0;
    let a1 = a.ground_degrees();
    let mut worst: f64 = 0.0;
    // b's own turn, and its neighbours: an interval up to 360 wide can reach a
    // fixed interval from either side, so one offset is not enough.
    let b_base = (b.west_lon - base).rem_euclid(360.0);
    for turn in [-360.0, 0.0, 360.0] {
        let b0 = b_base + turn;
        let b1 = b0 + b.ground_degrees();
        worst = worst.max(a1.min(b1) - a0.max(b0));
    }
    worst
}

/// The zoom floor for this canvas — `walkers`' own, not a copy of the formula.
fn floor_zoom() -> f64 {
    walkers::viewport::min_zoom(canvas())
}

/// Centre longitudes that put the seam inside the viewport, plus two controls
/// that do not.
///
/// The unfolded ones (185, −186, 540.5) are a map panned past the antimeridian,
/// which is what walkers' centre does and what phase 2 exists for.
const SEAM_CENTRES: &[f64] = &[
    179.0, 179.9, 180.0, 180.5, 185.0, -179.0, -179.9, -180.0, -186.0, 540.5, -540.5,
];

/// Centre longitudes nowhere near the seam. Every property here must hold for
/// these too, or it is a property of the seam and not of the map.
const INTERIOR_CENTRES: &[f64] = &[0.0, -97.2778, 17.03664, 151.2093];

/// The zooms the sweeps visit: the floor, both its neighbours to the ulp, a
/// spread above it — and two **below** it.
///
/// The two below are not decoration. `Map::show` refuses them, but a bare
/// `Projector` builds them and this is where the world is smaller than the glass
/// — the only place a walk that did not hold itself to one turn would repeat the
/// world and paint a continent twice. Without them the duplication gate cannot
/// fail for the defect it exists for.
fn sweep_zooms() -> Vec<f64> {
    let floor = floor_zoom();
    vec![
        (floor - 2.5).max(0.0),
        (floor - 1.0).max(0.0),
        floor.next_down(),
        floor,
        floor.next_up(),
        floor + 0.001,
        floor + 0.5,
        floor + 1.0,
        floor + 3.25,
        floor + 8.0,
        floor + 14.0,
    ]
}

/// **The far side draws.** A viewport straddling the antimeridian asks for tiles
/// from *both* ends of the grid, counted.
///
/// This is the failure the clamp in `squallar_geo::lon_to_tile_x` used to
/// guarantee: every column west of the seam collapsed onto column 0, so the
/// western half of the glass drew the same tile over and over and the ground
/// that belongs there — the far east — was never asked for at all.
#[test]
fn a_viewport_straddling_the_seam_asks_for_tiles_on_both_sides() {
    let mut straddling = 0usize;
    let mut missing = Vec::new();

    for &lon in SEAM_CENTRES {
        for zoom in sweep_zooms() {
            let projector = map_at(0.0, lon, zoom);
            let tile_zoom = zoom.round() as u8;
            let world = 2u32.saturating_pow(u32::from(tile_zoom));
            let painted = painted_columns(&projector, tile_zoom);

            // Does this case actually straddle? The seam is where a column off
            // the grid appears beside one on it.
            let off_grid = painted
                .iter()
                .filter(|p| p.column < 0 || p.column >= i64::from(world))
                .count();
            let on_grid = painted.len() - off_grid;
            if off_grid == 0 || on_grid == 0 {
                continue;
            }
            straddling += 1;

            let west_end = painted.iter().filter(|p| p.grid_column == 0).count();
            let east_end = painted
                .iter()
                .filter(|p| p.grid_column == world - 1)
                .count();
            if west_end == 0 || east_end == 0 {
                missing.push(format!(
                    "  centre {lon} zoom {zoom} (tile zoom {tile_zoom}, {world} columns \
                     round): {} columns drawn, {off_grid} of them off the grid, but \
                     {west_end} at grid column 0 and {east_end} at grid column {}",
                    painted.len(),
                    world - 1
                ));
            }
        }
    }

    assert!(
        straddling >= 40,
        "the sweep must not go vacuous: only {straddling} of its cases straddle the seam"
    );
    assert!(
        missing.is_empty(),
        "{} of {straddling} straddling viewports do not ask for both sides of the \
         antimeridian:\n{}",
        missing.len(),
        missing.join("\n")
    );
    println!("both sides asked for: {straddling} straddling viewports");
}

/// **No ground position is drawn twice in one frame.**
///
/// Stated over geography and never over tile coordinates, because those are two
/// different claims and only this one is the user's. A frame can name every
/// column exactly once — a perfect tile-coordinate identity — while two of them
/// are a whole turn apart and paint the same continent at two places on the
/// glass. That is what a doubled continent *is*.
///
/// **The assertion is against the extent of the thing it describes**, not
/// against a number: the ground two columns share may not exceed nothing, and
/// the ground the whole frame paints may not exceed the ground the viewport
/// shows. Both are contradictions rather than thresholds — neither can fire on
/// legitimate geometry, and neither has a constant to loosen.
#[test]
fn no_ground_position_is_drawn_twice_in_one_frame() {
    let mut cases = 0usize;
    let mut doubled = Vec::new();
    let mut worst_overlap = 0.0f64;

    for &lon in SEAM_CENTRES.iter().chain(INTERIOR_CENTRES) {
        for lat in [0.0, 55.0, -60.0] {
            for zoom in sweep_zooms() {
                let projector = map_at(lat, lon, zoom);
                let tile_zoom = zoom.round() as u8;
                let painted = painted_columns(&projector, tile_zoom);
                cases += 1;

                // Pairwise, over ground. `TOL` in degrees, taken from the
                // tolerance in points: a shared *edge* is not an overlap, and
                // the edge is only exact to `f32`.
                let world = world_points(&projector);
                let tol = TOL_POINTS * 360.0 / world;
                for (i, a) in painted.iter().enumerate() {
                    for b in &painted[i + 1..] {
                        let over = ground_overlap_degrees(a, b);
                        worst_overlap = worst_overlap.max(over);
                        if over > tol {
                            doubled.push(format!(
                                "  centre {lon} lat {lat} zoom {zoom}: columns {} and {} \
                                 (grid columns {} and {}) both paint {over} deg of the same \
                                 ground — {:.4}..{:.4} and {:.4}..{:.4}",
                                a.column,
                                b.column,
                                a.grid_column,
                                b.grid_column,
                                a.west_lon,
                                a.east_lon,
                                b.west_lon,
                                b.east_lon,
                            ));
                        }
                    }
                }

                // And the whole frame against its own viewport: the ground the
                // columns paint cannot exceed the ground the glass shows.
                let painted_deg: f64 = painted.iter().map(Painted::ground_degrees).sum();
                let viewport_deg = f64::from(CANVAS.x) * 360.0 / world;
                assert!(
                    painted_deg <= viewport_deg + 1.0,
                    "centre {lon} lat {lat} zoom {zoom}: the columns paint {painted_deg} deg \
                     of ground into a viewport that shows {viewport_deg} deg"
                );
            }
        }
    }

    assert!(
        cases >= 400,
        "the sweep must not go vacuous: only {cases} cases ran"
    );
    assert!(
        doubled.is_empty(),
        "{} pairs of columns, across {cases} frames, paint the same ground twice:\n{}",
        doubled.len(),
        doubled.join("\n")
    );
    println!("no doubled ground over {cases} frames; worst shared edge {worst_overlap} deg");
}

/// **And nothing is missing either** — at the zoom floor and one ulp on each
/// side of it, which is where the arithmetic is tightest.
///
/// At the floor the world is the viewport, so a seam-straddling frame needs
/// every column the world has *plus one*: the column the seam cuts arrives off
/// both ends of the walk, as two disjoint slices. One column fewer and there is
/// a strip of void; one more and a continent is doubled, which the sibling gate
/// above holds.
///
/// **The tolerance is relative and the sweep is asserted to need it.**
/// `2f64.powf(x.log2())` is not `x`, so at the floor the world lands a hair
/// *short* of the viewport and the last column falls femtopoints short of the
/// glass. A test carrying a tolerance nothing exercises measures nothing, so the
/// count of cases where the world really is short is asserted non-zero.
#[test]
fn the_glass_is_covered_at_the_floor_and_one_ulp_either_side() {
    let mut cases = 0usize;
    let mut short_world = 0usize;
    let mut gaps = Vec::new();

    for pane in VIEWPORTS {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(pane.0, pane.1));
        let floor = walkers::viewport::min_zoom(rect);
        for &lon in SEAM_CENTRES.iter().chain(INTERIOR_CENTRES) {
            for zoom in [floor.next_down(), floor, floor.next_up()] {
                let projector = map_on(rect, 0.0, lon, zoom);
                let tile_zoom = zoom.round() as u8;
                let world = projector.world_pixels();
                cases += 1;
                if world < f64::from(rect.width()) {
                    short_world += 1;
                }

                let painted = painted_columns_on(rect, &projector, tile_zoom);
                let covered: f64 = painted.iter().map(Painted::ground_degrees).sum();
                let wanted = f64::from(rect.width()) * 360.0 / world;
                if covered + TOL_POINTS * 360.0 / world < wanted {
                    gaps.push(format!(
                        "  pane {pane:?} centre {lon} zoom {zoom} (floor {floor}): {} columns \
                         cover {covered} deg of the {wanted} deg the glass shows",
                        painted.len()
                    ));
                }
            }
        }
    }

    assert!(
        short_world > 0,
        "no case in the sweep has a world shorter than its viewport, so the tolerance \
         this test carries measures nothing — the rounding it exists for did not happen"
    );
    assert!(
        gaps.is_empty(),
        "{} of {cases} viewports at the floor leave void:\n{}",
        gaps.len(),
        gaps.join("\n")
    );
    println!(
        "{cases} viewports covered at their own floors, {short_world} of them with a world \
         short of the glass"
    );
}

/// **The walk never names more columns than the world has**, plus the one the
/// grid's phase adds.
///
/// The ceiling `tiles::one_turn_at_most` states, measured rather than assumed,
/// and stated against the world's own width rather than against a number. Below
/// the widget's zoom floor — which `Map::show` refuses but a bare `Projector`
/// will build, and which this sweep therefore reaches deliberately — it is what
/// stops the walk from repeating the world instead of drawing it once.
#[test]
fn the_walk_never_names_more_columns_than_the_world_has() {
    let mut cases = 0usize;
    let mut over = Vec::new();

    for &lon in SEAM_CENTRES.iter().chain(INTERIOR_CENTRES) {
        // Deliberately below the floor as well as above it.
        for zoom in [0.0, 0.5, 1.5, 2.0, floor_zoom(), floor_zoom() + 2.0, 12.0] {
            for bias in [0i32, 1, 2] {
                let projector = map_at(0.0, lon, zoom);
                let Ok(tile_zoom) = u8::try_from(zoom.round() as i32 + bias) else {
                    continue;
                };
                let world = i64::from(2u32.saturating_pow(u32::from(tile_zoom)));
                let span = tile_span(&projector, canvas(), tile_zoom);
                let columns = span.east - span.west + 1;
                cases += 1;
                // `world + 2`, and the second is not slack. A half-open interval
                // of one turn laid on a grid of `world` columns touches
                // `world + 1` of them — the column the west edge lands inside
                // reaches one further than the turn's own width — and at the
                // floor the turn is read back a part in 1e13 long, which can
                // put the east edge a hair inside one more.
                if columns > world + 2 {
                    over.push(format!(
                        "  centre {lon} zoom {zoom} tile zoom {tile_zoom}: the walk names \
                         {columns} columns of a world that is {world} columns round"
                    ));
                }
            }
        }
    }

    assert!(
        cases >= 100,
        "the sweep must not go vacuous: only {cases} cases ran"
    );
    assert!(
        over.is_empty(),
        "{} of {cases} spans name more columns than there is world:\n{}",
        over.len(),
        over.join("\n")
    );
}

/// **A grid column is asked for, whatever column the walk names.**
///
/// The wrap is two different numbers for the same tile — the column the glass is
/// looking at, and the column the source holds it under — and the source is the
/// one that must always be on the grid. `tile_source::tile_id_is_valid` refuses
/// anything else outright, so a `TileId` built from the walk's own column would
/// simply never be fetched.
#[test]
fn every_column_the_walk_names_is_asked_for_on_the_grid() {
    let mut cases = 0usize;
    for &lon in SEAM_CENTRES.iter().chain(INTERIOR_CENTRES) {
        for zoom in sweep_zooms() {
            let projector = map_at(0.0, lon, zoom);
            let tile_zoom = zoom.round() as u8;
            let world = 2u32.saturating_pow(u32::from(tile_zoom));
            for painted in painted_columns(&projector, tile_zoom) {
                cases += 1;
                assert!(
                    painted.grid_column < world,
                    "centre {lon} zoom {zoom}: column {} is asked for as grid column {} of \
                     a grid {world} columns round",
                    painted.column,
                    painted.grid_column
                );
                assert_eq!(
                    i64::from(painted.grid_column),
                    painted.column.rem_euclid(i64::from(world)),
                    "centre {lon} zoom {zoom}: column {} names a different tile than the \
                     one a whole turn away",
                    painted.column,
                );
            }
        }
    }
    assert!(cases >= 1000, "only {cases} columns walked");
}

/// The walk this file reproduces is the walk `draw_tile_layer` makes.
///
/// Three things, and each is a way the reproduction could drift into testing
/// something the app does not do: the span comes from `tile_span` with the pane
/// rect; the id asked for is `wrap_tile_x` of the walked column; the rect is
/// `tile_rect_at` of the walked column, **not** of the wrapped one. The third is
/// the one that matters — placing by the wrapped column is the whole defect,
/// and it puts the tile a world off the glass rather than under the pointer.
#[test]
fn the_wrap_walk_here_is_the_walk_draw_tile_layer_makes() {
    let tile_zoom = 5u8;
    let grid = i64::from(2u32.pow(u32::from(tile_zoom)));
    let mut off_grid = 0usize;

    // One centre past each end of the seam, so neither sign of the excursion is
    // the only one exercised.
    for centre in [185.0, -185.0] {
        let projector = map_at(0.0, centre, 5.0);
        let span = tile_span(&projector, canvas(), tile_zoom);
        assert!(
            span.west < 0 || span.east >= grid,
            "the fixture at centre {centre} is not past the antimeridian: {span:?}"
        );

        for column in span.west..=span.east {
            let wrapped = squallar_geo::wrap_tile_x(column, tile_zoom);
            let placed = projector.tile_rect_at(column, span.north, tile_zoom);
            let by_wrapped = projector.tile_rect(walkers::TileId {
                x: wrapped,
                y: span.north,
                zoom: tile_zoom,
            });

            // How many whole turns the wrap moved this column by. Zero on the
            // grid, where placing by either column must be the identical rect.
            let turns = (column - i64::from(wrapped)) / grid;
            if turns == 0 {
                assert_eq!(placed, by_wrapped, "column {column} at centre {centre}");
                continue;
            }
            off_grid += 1;
            let apart = f64::from(placed.left() - by_wrapped.left());
            let want = turns as f64 * world_points(&projector);
            assert!(
                (apart - want).abs() < 0.01,
                "column {column} at centre {centre} is placed {apart} points from where its \
                 wrapped id {wrapped} would put it; {turns} turn(s) of world is {want}"
            );
        }
    }

    assert!(
        off_grid >= 4,
        "only {off_grid} columns were off the grid, so the placement claim is vacuous"
    );
}

/// **A footprint written in the folded ±180 frame is placed where the pane is
/// looking**, once the pane is looking past the antimeridian.
///
/// Everything drawn *on* the map — a radar image's own geographic box, a saved
/// download area, a region outline — is written in ±180, while the map's centre
/// is not. `Projector::project` is linear and folds nothing, so without the fold
/// in `overlay_cache::geo_corner_rect` a Pacific radar disappears the moment the
/// user pans a degree past 180: not drawn wrong, drawn a whole world away and
/// culled.
///
/// The assertion is against the extent of the thing it describes: the box must
/// land **inside the glass it geographically belongs on**, not within some
/// number of points of it.
#[test]
fn a_footprint_written_in_the_folded_frame_lands_on_the_glass() {
    // PGUA Guam and PAEC Kotzebue: the two WSR-88Ds a Pacific pane reaches
    // across the seam for, with their real 230 km coverage as a rough box.
    for (lat, lon) in [(13.456, 144.808), (64.511, -165.295)] {
        for centre in [176.0, 180.0, 185.0, 190.0, -186.0, 540.0] {
            // Only the panes that can actually see it are a claim about it.
            let turn = squallar_geo::fold_lon_near(lon, centre);
            if (turn - centre).abs() > 6.0 {
                continue;
            }

            let projector = map_at(lat, centre, 7.0);
            let rect = crate::overlay_cache::geo_corner_rect(
                &projector,
                (lat + 2.1, lon - 2.1),
                (lat - 2.1, lon + 2.1),
            );
            assert!(
                canvas().intersects(rect),
                "a footprint at ({lat}, {lon}) is placed at {rect:?}, off a pane centred at \
                 {centre} that is looking straight at it"
            );
        }
    }
}

/// **A rect wider than half a turn keeps its width.**
///
/// This is the half of the fold that has to survive the fold. Carrying each
/// corner to its own nearest representation is the obvious spelling and it is
/// wrong for anything wide: the two corners of a picture more than half a turn
/// across fold *opposite* ways, and the rect comes back inside out. An overlay
/// picture at the zoom floor is the viewport plus up to 25 % overdraw a side —
/// 1.5 turns — so this is not a hypothetical width.
///
/// Stated against the extent of the thing it describes: a picture covering `d`
/// degrees must be placed `d / 360` of a world wide. Never against a number.
#[test]
fn a_rect_wider_than_half_a_turn_keeps_its_width() {
    let mut cases = 0usize;
    for centre in [0.0, 90.0, 179.9, 180.0, 185.0, -180.0, 540.5] {
        let projector = map_at(0.0, centre, floor_zoom());
        let world = world_points(&projector);
        // A picture the width of the glass, and the same picture with the
        // overdraw the planner affords at this floor.
        for span_deg in [180.0, 359.0, 360.0, 450.0, 540.0] {
            let rect = crate::overlay_cache::geo_corner_rect(
                &projector,
                (60.0, centre - span_deg / 2.0),
                (-60.0, centre + span_deg / 2.0),
            );
            cases += 1;
            let want = span_deg / 360.0 * world;
            assert!(
                (f64::from(rect.width()) - want).abs() <= TOL_POINTS,
                "a picture covering {span_deg} deg, seen from a pane centred at {centre}, \
                 is placed {} points wide where {span_deg} deg of a {world}-point world is \
                 {want}",
                rect.width()
            );
        }
    }
    assert!(cases >= 30, "only {cases} widths measured");
}
