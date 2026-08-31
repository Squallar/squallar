//! **The floor-strip cache: orbit frames stop paying for the floor.**
//!
//! Scene B's measured cost was never the raymarch (~1 ms GPU): it was the CPU
//! floor leg — a complete second map render per 3D pane per frame, plus the
//! mirror pass's second `update_buffers`, clip rewrite and mid-frame submit.
//! The strip cache skips all of it on frames whose content key is unchanged
//! and whose last paint was complete. These fixtures hold both halves: the
//! skip (an orbit repaints nothing) and the staleness contract (each key
//! input, changed once, repaints exactly once — and a pending input keeps
//! the strip repainting until it resolves).
//!
//! Counts are read off the `Gui`'s per-instance probe
//! (`strip_paints_for_test`) rather than `crate::floor_ledger`'s statics —
//! the ledger is process-global and the test binary runs suites in parallel,
//! exactly the trap `overlay_cache::ledger_tests`' module doc names. The
//! ledger's own real-path evidence is the Tier-2 rig and the scene B leg,
//! which are a fresh process each.

use crate::input_harness::InputHarness;
use crate::volume_view::StubVolumePainter;
use squallar_source::id::known;
use std::sync::Arc;

const FRAME_DT: f64 = 1.0 / 60.0;

/// A harness with one map pane and one 3D pane whose floor is **fully
/// resolved**: painter installed, scan loaded, and the tile layers off on the
/// 3D pane — the harness's stock tile sources are inert (header never
/// arrives), which reads as pending forever and would hold the completeness
/// latch open. The tile fixtures below install real single-tile sources
/// instead.
fn resolved_floor_harness() -> InputHarness {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.make_pane_volume(1);
    h.load_scan("KTLX");
    h.gui_mut()
        .apply(crate::shell_api::GuiEvent::VolumePainter(Some(Arc::new(
            StubVolumePainter::painting(),
        ))));
    // Panes are layer-linked by default and the link fan-out copies pane 0's
    // stack straight back over any per-pane toggle; the fixtures need the 3D
    // pane's stack to be its own.
    h.set_layer_links(false);
    h.set_overlay_on_pane(1, &known::BASEMAP_TILES, false);
    h.set_overlay_on_pane(1, &known::TERRAIN, false);
    h.frames_for(4, FRAME_DT);
    h
}

fn paints(h: &InputHarness) -> u64 {
    h.gui().strip_paints_for_test()
}

/// Run quiet frames and demand the strip stays clean — the settle every
/// fixture stands on. **This is the assertion that is RED on the baseline**:
/// before the cache, every frame painted every strip, so no fixture below
/// could even reach its one-change arm.
fn assert_settles_clean(h: &mut InputHarness, label: &str) {
    let before = paints(h);
    h.frames_for(5, FRAME_DT);
    assert_eq!(
        paints(h),
        before,
        "{label}: a quiet, resolved floor kept repainting; the strip cache \
         is not skipping and the whole 3D-floor lever is dead",
    );
    assert!(
        !h.gui_mut().mirror_source_rects().repainted(),
        "{label}: a clean pass still told the shell to re-render the mirror",
    );
}

/// One change repaints exactly once, then the strip is clean again.
fn assert_one_repaint(h: &mut InputHarness, label: &str, change: impl FnOnce(&mut InputHarness)) {
    assert_settles_clean(h, label);
    let before = paints(h);
    change(h);
    h.frames_for(4, FRAME_DT);
    assert_eq!(
        paints(h) - before,
        1,
        "{label}: one change must repaint the strip exactly once — zero means \
         the input is not in the content key and the floor freezes stale; \
         more means the change left the key oscillating or the completeness \
         latch stuck open",
    );
    assert_settles_clean(h, label);
}

/// **The skip gate.** N orbit-only frames over a resolved floor paint the
/// strip zero further times, and no frame asks the shell for a mirror
/// render. On the baseline this is N paints — the measured 50-77 ms/frame.
#[test]
fn an_orbit_over_a_resolved_floor_never_repaints_the_strip() {
    let mut h = resolved_floor_harness();
    assert!(
        paints(&h) >= 1,
        "non-vacuity: the strip never painted at all, so nothing below is \
         about skipping",
    );
    assert_settles_clean(&mut h, "orbit");

    let camera_before = h
        .gui_mut()
        .pane_mut(1)
        .expect("pane 1")
        .volume()
        .expect("a 3D pane")
        .camera;

    let before = paints(&h);
    let start = h.pane_rects()[1].center();
    h.mouse_move(start);
    h.frame_after(FRAME_DT);
    h.mouse_press(start);
    h.frame_after(FRAME_DT);
    for i in 1..=30 {
        h.mouse_move(start + egui::vec2(3.0 * i as f32, 1.5 * i as f32));
        h.frame_after(FRAME_DT);
        assert!(
            !h.gui_mut().mirror_source_rects().repainted(),
            "orbit frame {i} told the shell to re-render the mirror",
        );
    }
    h.mouse_release(start + egui::vec2(90.0, 45.0));
    h.frame_after(FRAME_DT);

    let camera_after = h
        .gui_mut()
        .pane_mut(1)
        .expect("pane 1")
        .volume()
        .expect("a 3D pane")
        .camera;
    assert_ne!(
        camera_before, camera_after,
        "non-vacuity: the drag never reached the orbit camera, so these were \
         not orbit frames",
    );

    assert_eq!(
        paints(&h) - before,
        0,
        "orbit frames repainted the floor strip; the second map render and \
         the mirror leg are back on every frame",
    );
}

/// A skipped frame still hands the shell the strip rects and the cached
/// affine — the mirror keeps sampling, the floor keeps reprojecting.
#[test]
fn a_skipped_frame_keeps_the_strip_rects_and_the_affine() {
    let mut h = resolved_floor_harness();
    assert_settles_clean(&mut h, "held frame");
    let sources = h.gui_mut().mirror_source_rects();
    assert_eq!(
        sources.rects().len(),
        1,
        "a held frame dropped its strip from the mirror guest list; the \
         shell would release the texture under a floor that is still shown",
    );
}

/// Overlay data bump, the token half: the per-layer cache token is in the
/// key. The as-of term is a token input with no render pipeline attached in
/// this harness (NwsAlerts holds no data offline), so the change is
/// self-contained and the count is exact.
#[test]
fn an_overlay_token_move_repaints_exactly_once() {
    use crate::pane::TimeMode;

    let ts = |hours: i64| {
        chrono::NaiveDate::from_ymd_opt(2026, 8, 22)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            + chrono::Duration::hours(hours)
    };
    let mut h = resolved_floor_harness();
    h.gui_mut()
        .pane_mut(1)
        .expect("pane 1")
        .set_time_mode(TimeMode::AsOf(ts(0)));
    h.frames_for(4, FRAME_DT);
    assert_one_repaint(&mut h, "token move", |h| {
        h.gui_mut()
            .pane_mut(1)
            .expect("pane 1")
            .set_time_mode(TimeMode::AsOf(ts(2)));
    });
}

/// Overlay data bump, the picture half: a raster landing in a layer's cache
/// is a new texture identity, and the strip must repaint to show it — the
/// token moved a frame earlier, but the token alone cannot see the arrival.
#[test]
fn an_overlay_raster_arrival_repaints_exactly_once() {
    let mut h = resolved_floor_harness();
    let ctx = h.egui_ctx();
    assert_one_repaint(&mut h, "raster arrival", |h| {
        let texture = ctx.load_texture(
            "alerts",
            egui::ColorImage::filled([1, 1], egui::Color32::RED),
            egui::TextureOptions::default(),
        );
        h.gui_mut()
            .pane_mut(1)
            .expect("pane 1")
            .overlay_cache_mut(&known::NWS_ALERTS)
            .show(crate::overlay_cache::OverlayTextureData {
                texture,
                placed: squallar_geo::PlacedRaster::of(squallar_geo::GeoBounds {
                    min_lat: 30.0,
                    max_lat: 40.0,
                    min_lon: -100.0,
                    max_lon: -90.0,
                }),
                data_generation: 0,
                render_zoom: 0,
                width: 1,
                height: 1,
                radar_meta: None,
                hit_map: None,
            });
    });
}

/// Loop tick: the active radar image's identity is in the key. Without it
/// the floor freezes on one loop frame while the volume above it animates —
/// the stale-floor bug class this WO must not ship.
#[test]
fn a_loop_tick_repaints_the_floor_exactly_once() {
    use crate::pane::{LoopFrame, LoopFrameImage, LoopPhase, TimeMode};

    let mut h = resolved_floor_harness();
    let ctx = h.egui_ctx();

    let ts = |minutes: i64| {
        chrono::NaiveDate::from_ymd_opt(2026, 8, 22)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            + chrono::Duration::minutes(minutes)
    };
    let radar_frame = |name: &str| {
        LoopFrameImage::PlanView(crate::pane::RadarImageData {
            texture: ctx.load_texture(
                name.to_owned(),
                egui::ColorImage::filled([1, 1], egui::Color32::BLUE),
                egui::TextureOptions::default(),
            ),
            lat: 35.33,
            lon: -97.28,
            placed: squallar_geo::PlacedRaster::of(squallar_geo::GeoBounds {
                min_lat: 30.0,
                max_lat: 40.0,
                min_lon: -100.0,
                max_lon: -90.0,
            }),
            hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
            max_range_km: 100.0,
            nyquist_ms: None,
            melting_layer_source: None,
            storm_motion: None,
        })
    };

    {
        let pane = h.gui_mut().pane_mut(1).expect("pane 1");
        let state = pane.time_state_mut(&known::RADAR);
        state.phase = LoopPhase::Paused;
        state.frames = (0..2)
            .map(|i| LoopFrame {
                timestamp: ts(i * 15),
                image: Some(radar_frame(&format!("loop{i}"))),
                render_in_flight: false,
                render_failed: false,
            })
            .collect();
        pane.set_time_mode(TimeMode::AsOf(ts(0)));
    }
    h.frames_for(4, FRAME_DT);

    assert_one_repaint(&mut h, "loop tick", |h| {
        h.gui_mut()
            .pane_mut(1)
            .expect("pane 1")
            .set_time_mode(TimeMode::AsOf(ts(15)));
    });
}

/// User-location move: the marker is drawn straight off `ctx.user_location`
/// on the Ground surface — no cache token exists for it, so the key carries
/// the quantized position itself.
#[test]
fn a_user_location_move_repaints_exactly_once_and_jitter_not_at_all() {
    let mut h = resolved_floor_harness();
    h.set_overlay_on_pane(1, &known::USER_LOCATION, true);
    h.set_gps_fix(squallar_location::Fix::from_lat_lon(35.25, -97.5));
    h.frames_for(4, FRAME_DT);

    assert_one_repaint(&mut h, "location move", |h| {
        h.set_gps_fix(squallar_location::Fix::from_lat_lon(35.251, -97.5));
    });

    // GPS jitter under the ~0.55 m quantum must NOT repaint: a fix per
    // second at rest would otherwise hold the strip awake forever.
    let before = paints(&h);
    h.set_gps_fix(squallar_location::Fix::from_lat_lon(35.251_000_05, -97.5));
    h.frames_for(4, FRAME_DT);
    assert_eq!(
        paints(&h),
        before,
        "sub-quantum GPS jitter repainted the strip; a parked phone would \
         never skip",
    );
}

/// Rung flip: the shell's deferred mirror-plan stamp forces the repaint the
/// realloc needs, and the tile zoom bias is a key input in its own right.
#[test]
fn a_rung_flip_and_a_bias_change_each_repaint_exactly_once() {
    let mut h = resolved_floor_harness();
    assert_one_repaint(&mut h, "mirror-plan stamp", |h| {
        h.set_mirror_plan_stamp(1);
    });
    assert_one_repaint(&mut h, "tile zoom bias", |h| {
        h.set_floor_tile_zoom_bias(1);
    });
}

/// Theme flip: the strip's ground is styled by the live theme.
#[test]
fn a_theme_flip_repaints_exactly_once() {
    let mut h = resolved_floor_harness();
    // Flip to whichever theme is not in force — flipping to the current one
    // would assert that a no-op repaints.
    let dark = h.egui_ctx().global_style().visuals.dark_mode;
    assert_one_repaint(&mut h, "theme flip", |h| {
        h.set_os_theme(!dark);
    });
}

/// Viewport change: the strip is a projector, and reframing it is new
/// pixels.
#[test]
fn a_viewport_change_repaints_exactly_once() {
    let mut h = resolved_floor_harness();
    assert_one_repaint(&mut h, "viewport", |h| {
        let zoom = h.gui_mut().pane_mut(1).expect("pane 1").map_memory.zoom();
        h.gui_mut()
            .pane_mut(1)
            .expect("pane 1")
            .map_memory
            .set_zoom(zoom + 0.5)
            .expect("a zoom walkers accepts");
    });
}

/// A dead-network tile source whose whole world is one tile: `0/0/0` cached
/// makes every span complete, which is what lets the arrival fixtures assert
/// exact counts.
struct SingleTileWorld;

impl walkers::sources::TileSource for SingleTileWorld {
    fn tile_url(&self, tile_id: walkers::TileId) -> String {
        format!(
            "http://127.0.0.1:1/{}/{}/{}.png",
            tile_id.zoom, tile_id.x, tile_id.y
        )
    }

    fn attribution(&self) -> walkers::sources::Attribution {
        walkers::sources::Attribution {
            text: "test",
            url: "http://127.0.0.1:1/",
            logo_light: None,
            logo_dark: None,
        }
    }

    fn max_zoom(&self) -> u8 {
        0
    }
}

fn single_tile_source(h: &InputHarness) -> crate::tile_source::HttpsTiles {
    squallar_radar::tls::init();
    crate::tile_source::HttpsTiles::with_client(
        SingleTileWorld,
        h.egui_ctx(),
        reqwest::Client::builder()
            .build()
            .expect("the test client should build"),
    )
}

/// The one tile of [`SingleTileWorld`], filled corner to corner.
fn world_tile() -> walkers::Tile {
    walkers::Tile::Vector(Arc::new(vec![walkers::ShapeOrText::Shape(
        egui::Shape::rect_filled(
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(4096.0, 4096.0)),
            0.0,
            egui::Color32::from_rgb(0x10, 0x20, 0x30),
        ),
    )]))
}

const WORLD: walkers::TileId = walkers::TileId {
    x: 0,
    y: 0,
    zoom: 0,
};

/// Tile arrival, basemap source: a put is new ground, and the put-generation
/// is per source.
#[test]
fn a_basemap_tile_arrival_repaints_exactly_once() {
    let mut h = resolved_floor_harness();
    let mut source = single_tile_source(&h);
    source.put_for_test(WORLD, world_tile());
    h.gui_mut().map_tiles.tiles = Some(source);
    h.set_overlay_on_pane(1, &known::BASEMAP_TILES, true);
    h.frames_for(4, FRAME_DT);

    assert_one_repaint(&mut h, "basemap arrival", |h| {
        h.gui_mut()
            .map_tiles
            .tiles
            .as_mut()
            .expect("the installed base source")
            .put_for_test(WORLD, world_tile());
    });
}

/// Tile arrival, terrain source: the second `HttpsTiles`, with its own
/// generation — an arrival there must repaint even while the basemap is
/// untouched.
#[test]
fn a_terrain_tile_arrival_repaints_exactly_once() {
    let mut h = resolved_floor_harness();
    let mut source = single_tile_source(&h);
    source.put_for_test(WORLD, world_tile());
    h.gui_mut().map_tiles.terrain = Some(source);
    h.set_overlay_on_pane(1, &known::TERRAIN, true);
    h.frames_for(4, FRAME_DT);

    assert_one_repaint(&mut h, "terrain arrival", |h| {
        h.gui_mut()
            .map_tiles
            .terrain
            .as_mut()
            .expect("the installed terrain source")
            .put_for_test(WORLD, world_tile());
    });
}

/// **The completeness latch.** A strip with a pending tile repaints on every
/// frame — that is what keeps `request_once` re-asking — and settles the
/// frame after the tile lands.
#[test]
fn a_pending_tile_keeps_the_strip_repainting_until_it_lands() {
    let mut h = resolved_floor_harness();
    h.gui_mut().map_tiles.tiles = Some(single_tile_source(&h));
    h.set_overlay_on_pane(1, &known::BASEMAP_TILES, true);
    h.frames_for(2, FRAME_DT);

    let before = paints(&h);
    h.frames_for(10, FRAME_DT);
    assert_eq!(
        paints(&h) - before,
        10,
        "a strip with a pending tile stopped repainting; the fetch retry \
         path is dead and the floor never fills in",
    );

    h.gui_mut()
        .map_tiles
        .tiles
        .as_mut()
        .expect("the installed base source")
        .put_for_test(WORLD, world_tile());
    h.frames_for(4, FRAME_DT);
    assert_settles_clean(&mut h, "after the tile landed");
}

/// **All-or-nothing across panes.** With two floors on screen and only the
/// second dirty, the frame that finds it dirty after the first already
/// skipped paints nothing (the mirror pass would blank the skipped strip);
/// the forced frame after it paints both.
#[test]
fn a_dirty_second_pane_defers_one_frame_and_then_both_repaint() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(3);
    h.make_pane_volume(1);
    h.make_pane_volume(2);
    h.load_scan("KTLX");
    h.gui_mut()
        .apply(crate::shell_api::GuiEvent::VolumePainter(Some(Arc::new(
            StubVolumePainter::painting(),
        ))));
    h.set_layer_links(false);
    for idx in [1, 2] {
        h.set_overlay_on_pane(idx, &known::BASEMAP_TILES, false);
        h.set_overlay_on_pane(idx, &known::TERRAIN, false);
    }
    h.frames_for(4, FRAME_DT);
    assert_settles_clean(&mut h, "two floors");

    let before = paints(&h);
    {
        let pane = h.gui_mut().pane_mut(2).expect("pane 2");
        let zoom = pane.map_memory.zoom();
        pane.map_memory
            .set_zoom(zoom + 0.5)
            .expect("a zoom walkers accepts");
    }

    // Frame 1: pane 1 skips first, pane 2 is found dirty too late to flip
    // the frame — nothing paints, the repaint is owed.
    h.frame_after(FRAME_DT);
    assert_eq!(
        paints(&h) - before,
        0,
        "the frame that found a later pane dirty painted anyway; the mirror \
         pass would blank the strip the earlier pane skipped",
    );
    assert!(
        !h.gui_mut().mirror_source_rects().repainted(),
        "the deferral frame told the shell to render the mirror over a pass \
         with a missing strip",
    );

    // Frame 2: the forced repaint carries every strip.
    h.frame_after(FRAME_DT);
    assert_eq!(
        paints(&h) - before,
        2,
        "the forced frame after a deferral must repaint both strips",
    );
    assert!(
        h.gui_mut().mirror_source_rects().repainted(),
        "the forced frame's mirror render is the whole point of deferring",
    );
    assert_settles_clean(&mut h, "after the deferral");
}

/// The graphics-state reset repaints: the mirror texture died with the
/// device, and a clean key must not keep sampling a texture that is gone.
#[test]
fn losing_the_graphics_state_forces_a_repaint() {
    let mut h = resolved_floor_harness();
    // `clear_graphics_state` drops the painter; reinstall it so the pane
    // still draws a floor afterwards.
    assert_one_repaint(&mut h, "graphics reset", |h| {
        h.gui_mut().clear_graphics_state();
        h.gui_mut()
            .apply(crate::shell_api::GuiEvent::VolumePainter(Some(Arc::new(
                StubVolumePainter::painting(),
            ))));
    });
}
