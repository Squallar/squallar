//! **The one definition validates what a handler answers.**
//!
//! Every user path into a layer's opacity is guarded twice already — the
//! setter refuses non-finite and clamps, and the config reader and writer
//! guard the file — so the only way an out-of-range number reaches the paint
//! walk is `SourceHandler::default_opacity`, an open trait method whose
//! "0..=1" is prose. Unvalidated it does not fail, it *disagrees*: egui's
//! `set_opacity` ignores a non-finite factor outright, so the layer paints at
//! full strength while the slider reads `NaN%` and the floor-strip key hashes
//! a NaN payload nothing obliges to be the same number twice.
//!
//! Driven against a stub handler that answers the fixture's number, because a
//! shipped handler that misbehaved would be a bug in that handler rather than
//! the input this is about.

use super::*;
use crate::pane::PaneState;
use squallar_overlays::render::overlay_state::{OverlayHandler, OverlayRegistry};
use squallar_source::handler::{FetchPayload, PaneRef, RenderMode};

/// The stub's id. Not a `known::` one: this layer is the out-of-tree handler
/// the validation exists for.
const STUB: &str = "stub.opacity";

fn stub_id() -> LayerId {
    LayerId::from_static(STUB)
}

/// A ground layer that answers `default_opacity` with whatever it was built
/// with, honest doc comment or not.
struct StubLayer {
    default_opacity: f32,
}

impl OverlayHandler for StubLayer {
    fn id(&self) -> LayerId {
        stub_id()
    }
    fn time_axis(&self) -> TimeAxis {
        TimeAxis::Live
    }
    /// Ground, so the floor-strip key's walk reaches it — that walk skips
    /// every glass layer, and a glass stub would make the key assertions
    /// below vacuous.
    fn surface(&self) -> Surface {
        Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        0
    }
    fn display_name(&self) -> &str {
        "Stub"
    }
    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
    }
    fn data_generation(&self) -> u64 {
        0
    }
    fn has_data(&self, _pane: &PaneRef<'_>) -> bool {
        true
    }
    fn is_fetching(&self) -> bool {
        false
    }
    fn set_fetching(&mut self, _fetching: bool, _pane: &PaneRef<'_>) {}
    fn fetch_time(&self) -> Option<web_time::Instant> {
        None
    }
    fn apply_fetch_result(&mut self, _result: FetchPayload, _pane: &PaneRef<'_>) {}
    fn retain_selections(&self, _selections: &mut Vec<Arc<dyn OverlayItem>>, _pane: &PaneRef<'_>) {}

    fn default_opacity(&self, _pane: &PaneRef<'_>) -> f32 {
        self.default_opacity
    }
}

fn registry(default_opacity: f32) -> OverlayRegistry {
    OverlayRegistry::with_handlers(vec![Box::new(StubLayer { default_opacity })])
}

/// A pane holding the stub **at its default** — no explicit value, so the
/// resolver takes the handler's answer.
fn pane_at_the_default() -> PaneState {
    let mut pane = PaneState::new();
    pane.set_overlay_enabled(stub_id(), true);
    assert_eq!(
        pane.layer_opacity(&stub_id()),
        None,
        "fixture: the pane holds an explicit value, so the default path is \
         not the one under test",
    );
    pane
}

/// What the one definition hands the walk, the key and the slider for a
/// handler whose default is `given`.
fn resolved(given: f32) -> f32 {
    let overlays = registry(given);
    let pane = pane_at_the_default();
    resolved_layer_opacity(&overlays, 0, &pane, &stub_id())
}

/// The floor-strip content key for a pane whose one ground layer defaults to
/// `given`. Everything else about the inputs is fixed, so the only thing that
/// can move this number is the opacity term.
fn strip_key(given: f32) -> u64 {
    let overlays = registry(given);
    let pane = pane_at_the_default();
    let preferences = UserPreferences::default();
    let memory = walkers::MapMemory::default();
    ground_content_key(
        &GroundKeyInputs {
            overlays: &overlays,
            preferences: &preferences,
            pane: &pane,
            pane_idx: 0,
            strip: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(512.0, 512.0)),
            centre: walkers::lat_lon(35.33, -97.28),
            memory: &memory,
            basemap_generation: None,
            terrain_generation: None,
            tile_zoom_bias: 0,
            is_dark: false,
            user_location: None,
            user_heading: None,
            user_fix_present: false,
        },
        GroundIsMesh::PLAN_VIEW,
    )
}

/// **A handler's answer is validated, not trusted.** Non-finite becomes the
/// documented default of 1.0 — the value that paints what the layer looked
/// like before it had a slider — and everything else is clamped into range,
/// with `-0.0` normalized to `0.0` so one picture has one hash.
///
/// Compared by **bits**, deliberately: `-0.0 == 0.0` is true, and the sign
/// bit is exactly what the strip key would carry through if the resolver left
/// it there.
#[test]
fn a_handlers_out_of_range_default_opacity_is_validated_at_the_resolver() {
    for (given, want) in [
        (f32::NAN, 1.0_f32),
        (f32::INFINITY, 1.0),
        (f32::NEG_INFINITY, 1.0),
        (2.0, 1.0),
        (-1.0, 0.0),
        (-0.0, 0.0),
    ] {
        let got = resolved(given);
        assert_eq!(
            got.to_bits(),
            want.to_bits(),
            "a handler defaulting to {given} resolved to {got}: the painter \
             would silently ignore or clamp it while the slider and the strip \
             key carried it as given",
        );
    }
    // The control. A number the trait's own contract allows is handed back
    // untouched, or the assertions above would hold for a resolver that
    // simply returned a constant.
    assert_eq!(
        resolved(0.37).to_bits(),
        0.37_f32.to_bits(),
        "an in-range default was not handed back as itself",
    );
    assert_eq!(resolved(0.0).to_bits(), 0.0_f32.to_bits());
    assert_eq!(resolved(1.0).to_bits(), 1.0_f32.to_bits());
}

/// **The floor strip's key is a number, and the same number twice.** The key
/// hashes the resolved opacity by bits, so an unvalidated NaN would put a
/// payload in it that nothing obliges to be identical across evaluations —
/// and a key that moves on a frame where nothing changed repaints the 3D
/// pane's whole floor, every frame, forever.
#[test]
fn a_non_finite_default_cannot_move_the_floor_strip_key() {
    // Non-vacuity: the stub reaches the key walk at all, so the equalities
    // below are the validation's doing and not a walk that skipped it.
    assert_ne!(
        strip_key(0.5),
        strip_key(0.25),
        "non-vacuity: two different in-range defaults hash the same, so this \
         layer's opacity is not in the key and nothing below can be read",
    );

    assert_eq!(
        strip_key(f32::NAN),
        strip_key(f32::NAN),
        "the key moved between two evaluations of one unchanged pane",
    );
    assert_eq!(
        strip_key(f32::NAN),
        strip_key(1.0),
        "a NaN default keys differently from the 1.0 it paints at: the strip \
         is cached under a number the painter never used",
    );
    assert_eq!(
        strip_key(-0.0),
        strip_key(0.0),
        "negative zero keys differently from zero, and the two paint the same \
         picture",
    );
}
