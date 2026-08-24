//! **Which timeline the two radar DRAW arms address** (WO-T3.8).
//!
//! `ui_map_pane`'s radar arm and `ui_section_pane`'s `looping` both ask
//! `time_state(&known::RADAR).is_active()` to choose between *radar's playing
//! frame* and *radar's live picture*. WO-T3.7 wrote down why each is
//! radar-addressed and found that retargeting either at `transport_state()`
//! passed every suite in the tree. These are the gates.
//!
//! **The reachable state both fork on** is a pane whose transport sits on a
//! **satellite** loop while radar itself is NOT looping:
//! `PaneState::refresh_transport` returns early while the transport's own loop
//! is active, so arming a GMGSI loop and then enabling radar leaves the
//! controls on the satellite. Radar then draws its live scan, exactly as it
//! does on any pane that is not looping.
//!
//! A transport-addressed read in that state answers "a loop is running" about
//! a timeline that is not radar's, takes the loop branch, and finds nothing:
//! `active_image` / `active_section_image` are radar-addressed and radar holds
//! no frames. **The picture disappears** — the map paints no radar at all, and
//! the section pane paints its "cutting…" placeholder over a cut it already
//! has.
//!
//! Everything below is read off the glass the frame really painted, through
//! the same `Gui::frame` a user drives.

use super::InputHarness;
use crate::overlay_cache::OverlayTextureData;
use crate::pane::{LayerTimeState, LoopFrame, LoopPhase};
use squallar_geo::GeoPoint;
use squallar_source::id::{LayerId, known};

/// The satellite layer these pins park the transport on. A real registered
/// layer, so the slot it gets is a real slot rather than the pane's orphan
/// state.
const SATELLITE: LayerId = known::GMGSI;

fn ts(minutes: i64) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 22)
        .unwrap()
        .and_hms_opt(6, 0, 0)
        .unwrap()
        + chrono::Duration::minutes(minutes)
}

/// Arm a **running** satellite loop on pane 0 and hand it the transport,
/// leaving radar's own timeline idle.
///
/// The preconditions are the ones the WO-T3.7 pins carry: two genuinely
/// different objects, the transport's one really running and radar's really
/// not — without all three, a "the picture is still there" assertion could
/// pass because the fixture collapsed the two reads into one.
fn a_satellite_loop_takes_the_transport(h: &mut InputHarness) {
    h.gui_mut().enable_overlay_for_test(&SATELLITE);
    {
        let pane = &mut h.gui_mut().panes_mut()[0];
        let mut ls = LayerTimeState::new();
        ls.phase = LoopPhase::Playing;
        ls.span_secs = 43_200;
        ls.frames = vec![LoopFrame {
            timestamp: ts(0),
            image: None,
            render_in_flight: false,
            render_failed: false,
        }];
        *pane.time_state_mut(&SATELLITE) = ls;
        pane.set_transport_layer(SATELLITE);
    }
    h.warm_up();

    let pane = &h.gui().panes()[0];
    assert!(
        pane.slot(&known::RADAR).is_some(),
        "precondition: radar has a REAL slot, so the radar-addressed read \
         under test is not the pane's orphan state",
    );
    assert!(
        pane.slot(&SATELLITE).is_some(),
        "precondition: the satellite has a REAL slot of its own",
    );
    assert!(
        !std::ptr::eq(pane.transport_state(), pane.time_state(&known::RADAR)),
        "precondition: the transport really addresses another timeline, or the \
         two reads are one object and the case is vacuous",
    );
    assert!(
        pane.transport_state().is_active(),
        "precondition: the satellite loop survived the frames above and is \
         genuinely running, so a transport-addressed `is_active()` reads TRUE",
    );
    assert!(
        !pane.time_state(&known::RADAR).is_active(),
        "precondition: radar is NOT looping — the whole fork is which of the \
         two timelines answers that question",
    );
}

// ---------------------------------------------------------------------------
// 1. The map's radar arm.

/// Put a **live** radar raster in pane 0's radar cache, placed over the ground
/// the pane is actually showing so the quad lands inside the pane rect, and
/// answer its texture id.
///
/// The placement is read back off the live projector rather than invented: a
/// raster placed off-screen is dropped by `draw_overlay_texture` and the
/// assertions below would then read an empty list in *both* arms.
fn live_radar_raster(h: &mut InputHarness) -> egui::TextureId {
    let rect = h.pane_rects()[0];
    let nw = h.ground_at(0, rect.min);
    let se = h.ground_at(0, rect.max);
    let texture = h.ctx.load_texture(
        "live-radar",
        egui::ColorImage::filled([1, 1], egui::Color32::RED),
        egui::TextureOptions::default(),
    );
    let id = texture.id();
    h.gui_mut().panes_mut()[0]
        .overlay_cache_mut(&known::RADAR)
        .show(OverlayTextureData {
            texture,
            placed: squallar_geo::PlacedRaster::of(squallar_geo::GeoBounds {
                min_lat: nw.y().min(se.y()),
                max_lat: nw.y().max(se.y()),
                min_lon: nw.x().min(se.x()),
                max_lon: nw.x().max(se.x()),
            }),
            data_generation: 0,
            render_zoom: 0,
            width: 1,
            height: 1,
            radar_meta: None,
            hit_map: None,
        });
    h.warm_up();
    id
}

/// Draw a map pane holding a live radar raster, with the transport optionally
/// parked on a running satellite loop, and answer whether **that exact
/// texture** reached the glass inside the pane.
///
/// Identity, never "something was painted": the whole defect is a pane with no
/// radar on it, and every other layer goes on painting either way.
fn radars_live_raster_reaches_the_glass(park_on_the_satellite: bool) -> bool {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.close_layers();
    h.warm_up();
    let radar = live_radar_raster(&mut h);
    if park_on_the_satellite {
        a_satellite_loop_takes_the_transport(&mut h);
    }
    h.warm_up();

    h.painted_images_in(h.pane_rects()[0])
        .iter()
        .any(|image| image.texture == radar)
}

/// **The map's radar arm is read off radar's own timeline, never off whichever
/// layer holds the transport.**
///
/// The arm chooses between radar's playing frame and radar's live raster.
/// Retargeting it at the transport makes a running satellite loop answer for
/// radar: the arm takes the loop branch, `active_image()` is still
/// radar-addressed and answers `None` because radar holds no frames, and the
/// arm paints **nothing at all** — no texture, no range ring, no hover source.
/// The user turns on a satellite loop and the radar vanishes off the map.
///
/// **The floor is the first arm** and is an identity check against the one
/// texture the fixture put in the cache, so "nothing was painted in either
/// case" fails here rather than passing below.
#[test]
fn the_maps_radar_arm_is_read_off_radars_timeline_not_the_transports() {
    assert!(
        radars_live_raster_reaches_the_glass(false),
        "floor: with radar driving the transport, the pane's live radar raster \
         is on the glass — if this arm never painted it the assertion below \
         would be satisfied by two blank panes",
    );
    assert!(
        radars_live_raster_reaches_the_glass(true),
        "the radar disappeared off the map. A pane whose transport sits on a \
         satellite loop while radar is not looping took the loop branch of the \
         radar draw arm; `active_image()` is radar-addressed and radar holds no \
         frames, so nothing was painted — no texture, no range ring, no hover \
         readout — over a pane that has a perfectly good scan in its cache",
    );
}

// ---------------------------------------------------------------------------
// 2. The section pane's `looping`.

/// Two points either side of a storm near KTLX, as the ends of a drawn line.
fn section_ends() -> (GeoPoint, GeoPoint) {
    (
        GeoPoint {
            lat: 35.0,
            lon: -97.8,
        },
        GeoPoint {
            lat: 35.6,
            lon: -96.9,
        },
    )
}

/// KTLX's reflectivity ladder on VCP 212, as the sampler resolves it.
fn vcp_212_rungs() -> Vec<f64> {
    vec![
        0.4834, 0.8789, 1.3184, 1.8018, 2.4170, 3.1201, 4.0430, 5.0977, 6.4160, 8.0273, 10.0195,
        12.5000, 15.6006, 19.5117,
    ]
}

/// The axes of a complete VCP 212 reflectivity section 100 km long, whose
/// ladder is [`vcp_212_rungs`].
fn vcp_212_axes() -> squallar_radar::xsect::SectionAxes {
    squallar_radar::xsect::SectionAxes {
        length_km: 100.0,
        base_km_msl: 0.4,
        top_km_msl: 20.4,
        near_ground_range_km: 10.0,
        far_ground_range_km: 110.0,
        coverage_ground_range_km: 110.0,
        cone_of_silence_km: 0.0,
        tilt_count: 14,
        widest_tilt_gap_deg: 4.9,
        top_tilt_deg: 19.5,
        top_declared_cut_deg: 19.5,
    }
}

/// What a section pane holding a finished cut says about itself, with the
/// transport optionally parked on a running satellite loop:
/// `(the cut's own ladder count is captioned, the "cutting…" placeholder is
/// on screen)`.
///
/// The ladder count is the identity: it is read off the **live** cut's own
/// axes, so it can only be captioned by the branch that drew that cut.
fn what_a_section_pane_draws(park_on_the_satellite: bool) -> (bool, bool) {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    let (a, b) = section_ends();
    h.make_pane_cross_section(0, a, b);
    h.place_section(0, vcp_212_axes(), &vcp_212_rungs());
    h.close_layers();
    if park_on_the_satellite {
        a_satellite_loop_takes_the_transport(&mut h);
    }
    h.warm_up();

    let pane = h.pane_rects()[0];
    (
        h.text_painted_in(pane, "14 tilts"),
        h.text_painted_in(pane, "Cutting the cross-section"),
    )
}

/// **A section pane's `looping` is read off radar's own timeline, never off
/// whichever layer holds the transport.**
///
/// `render_cross_section` picks its picture on that one flag: the loop's frame
/// when it is set, the live cut when it is not. `LoopFrameImage` has no
/// non-radar section shape at all, so a transport-addressed read hands a
/// satellite loop the answer for a decision only radar can be about. The pane
/// then asks `active_section_image()` — radar-addressed, radar holds no frames
/// — gets `None`, and **paints "Cutting the cross-section…" over a cut it has
/// already finished**, for as long as the satellite loop runs.
///
/// **The floor is the first arm**, asserted against the cut's own ladder count
/// rather than against the other arm, so "neither said anything" fails here.
#[test]
fn a_section_panes_loop_test_is_read_off_radars_timeline_not_the_transports() {
    let (captioned, cutting) = what_a_section_pane_draws(false);
    assert!(
        captioned && !cutting,
        "floor: with radar driving the transport, a pane holding a finished \
         14-rung cut captions it and shows no placeholder \
         (captioned={captioned}, placeholder={cutting}) — if this arm were \
         already drawing the placeholder the assertion below would be \
         satisfied by two broken panes",
    );

    let (captioned, cutting) = what_a_section_pane_draws(true);
    assert!(
        captioned && !cutting,
        "the section pane threw away the cut it had finished and went back to \
         \"Cutting the cross-section…\" because a satellite loop happened to be \
         playing (captioned={captioned}, placeholder={cutting}). \
         `active_section_image()` is radar-addressed and radar holds no frames, \
         so the loop branch has nothing to draw — the user watches a finished \
         cross-section turn into a progress message",
    );
}
