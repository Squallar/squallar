//! **The 3D pane's walk over its own stack** — which layer a Volume-mode pane
//! asks for a grid, and what it says when none of them can give one.
//!
//! Driven against a registry of **two** volume-capable stub layers. That is
//! not decoration: this build ships exactly one 3D source, so "topmost wins"
//! and "a lower layer is reached when the top one does not qualify" have no
//! discriminating input at all with only radar in the registry — the walk
//! would pass every test by returning the only capable layer there is.
//!
//! The stubs publish **radar's own registered rows** rather than inventing
//! a `ProductSpec` table, so `vertical` is the real fact and the only things
//! these fixtures vary are the three the walk is about: which layer is on,
//! which layer can build, and which field each says it is showing.

use super::*;
use rustdar_overlays::render::overlay_state::{OverlayHandler, OverlayRegistry};
use rustdar_source::handler::{FetchPayload, OverlayItem, RenderMode, Surface};
use rustdar_source::product::ProductSpec;

/// The slot config member a stub layer reports its current field from — its
/// **own** state, exactly as `RadarSource` reads the `"product"` member the
/// pane publishes for it.
const STUB_FIELD_KEY: &str = "field";

/// A layer that may or may not have a 3D half, and whose current field is
/// whatever its own slot config says.
struct StubLayer {
    id: LayerId,
    name: &'static str,
    capable: bool,
}

impl StubLayer {
    fn new(id: &'static str, name: &'static str, capable: bool) -> Self {
        Self {
            id: LayerId::from_static(id),
            name,
            capable,
        }
    }
}

impl rustdar_source::volume::VolumeCapable for StubLayer {}

impl OverlayHandler for StubLayer {
    fn id(&self) -> LayerId {
        self.id.clone()
    }
    fn surface(&self) -> Surface {
        Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        0
    }
    fn display_name(&self) -> &str {
        self.name
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

    fn products(&self) -> &'static [ProductSpec] {
        radar_fields::products()
    }

    fn current_field(&self, pane: &PaneRef<'_>) -> Option<FieldId> {
        serde_json::from_value(pane.config.get(STUB_FIELD_KEY)?.clone()).ok()
    }

    fn volume(&self) -> Option<&dyn rustdar_source::volume::VolumeCapable> {
        self.capable
            .then_some(self as &dyn rustdar_source::volume::VolumeCapable)
    }
}

const LOWER: &str = "stub.lower";
const UPPER: &str = "stub.upper";

fn registry(lower_capable: bool, upper_capable: bool) -> OverlayRegistry {
    OverlayRegistry::with_handlers(vec![
        Box::new(StubLayer::new(LOWER, "Lower", lower_capable)),
        Box::new(StubLayer::new(UPPER, "Upper", upper_capable)),
    ])
}

/// A pane whose stack is `[lower, upper]` — bottom to top, the order
/// [`PaneState::layers`] itself keeps — with each slot's enabled flag and
/// current field as given.
fn pane(slots: [(&'static str, bool, Option<&FieldId>); 2]) -> PaneState {
    let mut pane = PaneState::new();
    pane.layers.clear();
    for (id, enabled, field) in slots {
        let mut slot = LayerSlot::new(LayerId::from_static(id), enabled);
        if let Some(field) = field {
            slot.config = serde_json::json!({ STUB_FIELD_KEY: field });
        }
        pane.layers.push(slot);
    }
    pane
}

/// A field with vertical extent, and one without — read off the registry
/// rather than asserted, so a registration change fails here loudly instead of
/// making every case below agree by accident.
fn vertical_and_flat() -> (FieldId, FieldId) {
    let vertical = radar_fields::known::REFLECTIVITY;
    let flat = radar_fields::known::ECHO_TOPS;
    assert!(
        radar_fields::spec_for(&vertical)
            .expect("registered")
            .vertical,
        "fixture precondition: the vertical field must be registered as vertical",
    );
    assert!(
        !radar_fields::spec_for(&flat).expect("registered").vertical,
        "fixture precondition: the flat field must be registered as NOT vertical",
    );
    (vertical, flat)
}

/// **The stack is walked from the top.** Two capable layers, both on a
/// vertical field: the one drawn over the other is the one asked.
#[test]
fn the_topmost_qualifying_layer_is_the_one_asked_for_a_grid() {
    let (vertical, _) = vertical_and_flat();
    let pane = pane([
        (LOWER, true, Some(&vertical)),
        (UPPER, true, Some(&vertical)),
    ]);
    let ask = pane
        .volume_ask(&registry(true, true), 0)
        .expect("two capable layers on vertical fields must produce an ask");
    assert_eq!(
        ask.layer,
        LayerId::from_static(UPPER),
        "the topmost capable layer must win; the slot list runs bottom to top",
    );
    assert_eq!(
        ask.field, vertical,
        "the ask carries that layer's own field"
    );
}

/// **A layer that is switched off is not asked**, however high it sits — the
/// walk reaches the capable layer below it.
#[test]
fn a_switched_off_layer_is_walked_past_to_the_one_below() {
    let (vertical, _) = vertical_and_flat();
    let pane = pane([
        (LOWER, true, Some(&vertical)),
        (UPPER, false, Some(&vertical)),
    ]);
    let ask = pane
        .volume_ask(&registry(true, true), 0)
        .expect("the enabled layer below must be reached");
    assert_eq!(ask.layer, LayerId::from_static(LOWER));
}

/// **A layer with no 3D half is not asked**, however high it sits.
#[test]
fn a_layer_with_no_volume_half_is_walked_past_to_the_one_below() {
    let (vertical, _) = vertical_and_flat();
    let pane = pane([
        (LOWER, true, Some(&vertical)),
        (UPPER, true, Some(&vertical)),
    ]);
    let ask = pane
        .volume_ask(&registry(true, false), 0)
        .expect("the capable layer below must be reached");
    assert_eq!(
        ask.layer,
        LayerId::from_static(LOWER),
        "the topmost layer answered `None` for its 3D half, so it is not a \
         candidate at all",
    );
}

/// **A capable layer on a field with no vertical extent is walked past**, not
/// refused — the walk is looking for the first slot that qualifies, and a
/// lower one that does still gets the grid.
#[test]
fn a_capable_layer_on_a_flat_field_is_walked_past_to_the_one_below() {
    let (vertical, flat) = vertical_and_flat();
    let pane = pane([(LOWER, true, Some(&vertical)), (UPPER, true, Some(&flat))]);
    let ask = pane
        .volume_ask(&registry(true, true), 0)
        .expect("the layer below is on a vertical field and must be asked");
    assert_eq!(ask.layer, LayerId::from_static(LOWER));
    assert_eq!(ask.field, vertical);
}

/// **Nothing qualifies, and the topmost capable layer is on a flat field**:
/// the pane says which layer and which field, because "3D unavailable" is not
/// something a reader can act on.
#[test]
fn a_pane_whose_capable_layers_are_all_on_flat_fields_names_the_layer_and_the_field() {
    let (_, flat) = vertical_and_flat();
    let pane = pane([(LOWER, true, Some(&flat)), (UPPER, true, Some(&flat))]);
    let why = pane
        .volume_ask(&registry(true, true), 0)
        .expect_err("no slot qualifies");
    let name = radar_fields::spec_for(&flat).expect("registered").name;
    assert!(
        why.contains(name),
        "the plate must name the field that cannot be rendered; got {why:?}",
    );
    assert!(
        why.contains("Upper"),
        "the plate must name the TOPMOST capable layer — the one the reader is \
         looking at — not whichever the walk happened to see last; got {why:?}",
    );
    assert!(
        !why.contains("Lower"),
        "naming both layers tells the reader to go looking; got {why:?}",
    );
}

/// **Every capable layer is switched off**: the plate says what to turn on.
#[test]
fn a_pane_whose_capable_layers_are_all_off_says_which_to_turn_on() {
    let (vertical, _) = vertical_and_flat();
    let pane = pane([
        (LOWER, false, Some(&vertical)),
        (UPPER, false, Some(&vertical)),
    ]);
    let why = pane
        .volume_ask(&registry(true, true), 0)
        .expect_err("nothing is on");
    assert!(
        why.contains("Turn on"),
        "the plate must name an action the reader can take; got {why:?}",
    );
    assert!(
        why.contains("Upper") && why.contains("Lower"),
        "both capable layers are switchable, so both are offered; got {why:?}",
    );
}

/// **A layer whose current field is not one of its own registered rows** is
/// refused by name rather than resolved to some other row and drawn as if it
/// were that field.
#[test]
fn a_field_this_build_does_not_register_is_refused_by_name() {
    let alien = FieldId::new("NotAFieldThisBuildRegisters");
    assert!(
        radar_fields::spec_for(&alien).is_none(),
        "fixture precondition: the id must be unregistered",
    );
    let pane = pane([(LOWER, true, None), (UPPER, true, Some(&alien))]);
    let why = pane
        .volume_ask(&registry(true, true), 0)
        .expect_err("an unregistered field cannot be built");
    assert!(
        why.contains(alien.as_str()),
        "the plate must name the field this build is missing; got {why:?}",
    );
    assert!(
        why.contains("Upper"),
        "the plate must name the layer that has no such field; got {why:?}",
    );
}

/// **No layer in the stack has a 3D half at all**: the pane still says so,
/// rather than painting nothing.
#[test]
fn a_stack_with_no_3d_layer_at_all_still_says_so() {
    let (vertical, _) = vertical_and_flat();
    let pane = pane([
        (LOWER, true, Some(&vertical)),
        (UPPER, true, Some(&vertical)),
    ]);
    assert_eq!(
        pane.volume_ask(&registry(false, false), 0),
        Err(NO_VOLUME_LAYER.to_owned()),
        "a pane whose stack has no 3D source is not a blank pane",
    );
}

/// **A layer this build does not serve keeps its slot and is walked past.**
/// The open-id doctrine: a config from a newer build names layers this one has
/// no handler for, and they must not stop the walk.
#[test]
fn a_slot_no_handler_serves_does_not_stop_the_walk() {
    let (vertical, _) = vertical_and_flat();
    let mut pane = pane([
        (LOWER, true, Some(&vertical)),
        (UPPER, true, Some(&vertical)),
    ]);
    let mut unserved = LayerSlot::new(LayerId::new("a.layer.from.a.newer.build"), true);
    unserved.config = serde_json::json!({ STUB_FIELD_KEY: vertical });
    pane.layers.push(unserved);
    let ask = pane
        .volume_ask(&registry(true, true), 0)
        .expect("the unserved slot is skipped, not fatal");
    assert_eq!(ask.layer, LayerId::from_static(UPPER));
}
