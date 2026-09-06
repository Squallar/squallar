//! What a layer switched **off** costs the GPU.

use super::*;
use crate::UI_CONFIG_KEY;
use crate::overlay_cache::{
    OverlayTextureCache, OverlayTextureData, OverlayTexturePlan, current_quantized_zoom,
};
use squallar_geo::GeoBounds;
use squallar_kv::{KvStore, MemoryKvStore};

/// The layer switched off in most of these — a texture-mode overlay that is on
/// by default, so the fixture below starts from the state a real pane is in.
const KIND: squallar_source::id::LayerId = squallar_source::id::known::NWS_ALERTS;

/// Pixel dimensions for the fixture textures. Small on purpose: what is under
/// test is which cache entries survive, and the numbers only have to be
/// *consistent* between the texture and the plan it is compared against.
const W: u32 = 8;
const H: u32 = 5;

/// The zoom every `needs_rerender` question below is asked at.
const ZOOM: f64 = 7.0;

/// The content token the fixture texture was rendered for, and the one every
/// `needs_rerender` below is asked with — so a `true` answer is never a token
/// mismatch.
const TOKEN: u64 = 4242;

/// A whole-picture dispatch for this fixture's own viewport — what the app
/// would have recorded when it asked for the raster.
fn a_ticket() -> crate::overlay_cache::RenderTicket {
    crate::overlay_cache::RenderTicket::whole(TOKEN, viewport())
}

/// The viewport the fixture texture was rendered for.
fn viewport() -> GeoBounds {
    GeoBounds {
        min_lat: 34.5,
        max_lat: 35.5,
        min_lon: -97.5,
        max_lon: -96.5,
    }
}

/// Ground the fixture texture covers — deliberately far wider than
/// [`viewport`], so `pan_exceeds_coverage` cannot be what makes a re-render
/// look necessary. Only the toggle may do that.
fn covered() -> GeoBounds {
    GeoBounds {
        min_lat: 30.0,
        max_lat: 40.0,
        min_lon: -102.0,
        max_lon: -92.0,
    }
}

/// The plan a frame would produce for a texture of exactly the fixture's size,
/// so the size test inside `needs_rerender` is satisfied and cannot be the
/// reason an answer comes back `true`.
fn plan() -> OverlayTexturePlan {
    OverlayTexturePlan {
        width: W,
        height: H,
        overdraw: 0.25,
        pixels_per_point: 1.0,
        pane_px: [0, 0],
    }
}

/// A texture parked in `kind`'s cache on `pane`, described so that a
/// `needs_rerender` asked with [`plan`], [`viewport`] and [`ZOOM`] answers
/// `false` — i.e. a cache that is *satisfied*.
fn park_texture(ctx: &egui::Context, pane: &mut PaneState, kind: &squallar_source::id::LayerId) {
    let image = egui::ColorImage::from_rgba_unmultiplied(
        [W as usize, H as usize],
        &vec![255u8; (W * H) as usize * 4],
    );
    pane.overlay_cache_mut(kind).show(OverlayTextureData {
        texture: ctx.load_texture(
            format!("{kind:?}_fixture"),
            image,
            egui::TextureOptions::NEAREST,
        ),
        placed: squallar_geo::PlacedRaster::of(covered()),
        data_generation: TOKEN,
        render_zoom: current_quantized_zoom(ZOOM),
        width: W,
        height: H,
        radar_meta: None,
        hit_map: None,
    });
}

/// Whether `kind`'s cache on `pane` is holding pixels.
fn has_texture(pane: &PaneState, kind: &squallar_source::id::LayerId) -> bool {
    pane.overlay_cache(kind)
        .and_then(OverlayTextureCache::current)
        .is_some()
}

/// Switch [`KIND`], `Radar` and `CityLabels` on for `pane` and park a texture
/// for each.
fn park_three(ctx: &egui::Context, pane: &mut PaneState) {
    for kind in [
        KIND,
        squallar_source::id::known::RADAR,
        squallar_source::id::known::CITY_LABELS,
    ] {
        pane.set_overlay_enabled(kind.clone(), true);
        park_texture(ctx, pane, &kind);
    }
}

/// A single-pane `Gui` whose pane holds all three parked textures.
fn gui_with_parked_textures(ctx: &egui::Context) -> Gui {
    let mut gui = Gui::new();
    park_three(ctx, gui.pane_mut(0).expect("a fresh Gui has one pane"));
    gui
}

/// A `Gui` with **three** panes in the vector and a layout claiming only
/// **two**, so that `visible_pane_count()` is 2 and pane 2 is past it.
fn skewed_gui(ctx: &egui::Context) -> Gui {
    let mut gui = gui_with_parked_textures(ctx);
    gui.set_pane_count_for_test(3);
    gui.claim_pane_count_for_test(2);
    assert_eq!(
        gui.visible_pane_count(),
        2,
        "premise: the fixture must really be skewed, or pane 2 is an ordinary \
         visible pane and proves nothing this file is about",
    );
    for idx in [1, 2] {
        park_three(ctx, gui.pane_mut(idx).expect("the pane count was grown"));
    }
    gui
}

/// Switch [`KIND`] on or off for pane 0 through the real toggle path — the one
/// every eye, Show switch, catalog tile and preset routes through.
fn toggle(gui: &mut Gui, on: bool) {
    let mut pane = std::mem::take(&mut gui.panes[0]);
    Gui::write_pane_overlay(
        &mut gui.overlays,
        &mut gui.admission,
        0,
        &mut pane,
        &KIND,
        on,
    );
    gui.panes[0] = pane;
}

/// **The leak test.** A layer switched off lets its texture go, and takes
/// neither the radar raster nor a layer that is still on with it.
#[test]
fn switching_a_layer_off_releases_its_texture_and_not_the_others() {
    let ctx = egui::Context::default();
    let mut gui = gui_with_parked_textures(&ctx);

    for kind in [
        KIND,
        squallar_source::id::known::RADAR,
        squallar_source::id::known::CITY_LABELS,
    ] {
        assert!(
            has_texture(gui.pane(0).expect("pane 0"), &kind),
            "premise: the fixture must really have parked a {kind:?} texture, \
             or the assertion about it after the toggle is satisfied by an \
             empty start",
        );
    }

    toggle(&mut gui, false);

    let pane = gui.pane(0).expect("pane 0");
    assert!(
        !has_texture(pane, &KIND),
        "a layer switched off is still holding its full-size texture — this is \
         the per-kind, per-pane residency that survived the session",
    );
    assert!(
        has_texture(pane, &squallar_source::id::known::RADAR),
        "the radar raster was released by a layer-stack toggle. Nothing would \
         put it back: `ui_map_pane`'s viewport loop skips `Radar`, and \
         `dispatch_pane_renders` re-renders on a product/tilt/scan key, not on \
         an empty cache — so on a parked time this pane would show empty map",
    );
    assert!(
        has_texture(pane, &squallar_source::id::known::CITY_LABELS),
        "switching the alerts layer off released a texture belonging to a layer \
         that is still on: the release has become `clear on every write`",
    );
}

/// The in-flight mark is **not** cleared by the release, and that is a decision
/// rather than an omission.
#[test]
fn the_release_leaves_a_render_already_in_flight_marked() {
    let ctx = egui::Context::default();
    let mut gui = gui_with_parked_textures(&ctx);
    gui.pane_mut(0)
        .expect("pane 0")
        .overlay_cache_mut(&KIND)
        .renders
        .record(a_ticket());

    toggle(&mut gui, false);
    toggle(&mut gui, true);

    assert!(
        gui.pane(0)
            .expect("pane 0")
            .overlay_cache(&KIND)
            .expect("the cache entry survives; only its pixels go")
            .renders
            .holds(crate::overlay_cache::RenderSlot::WHOLE),
        "the release cleared the in-flight mark, so this off/on pair has opened \
         the dispatch gate on a render that is still coming: the pane will \
         rasterize and upload the same content twice",
    );
}

/// **Re-enabling regenerates**, asked of the gate that actually decides it
/// rather than of the emptiness of the cache.
#[test]
fn re_enabling_asks_for_a_fresh_render() {
    let ctx = egui::Context::default();
    let mut gui = gui_with_parked_textures(&ctx);

    {
        let cache = gui.pane_mut(0).expect("pane 0").overlay_cache_mut(&KIND);
        cache.needs_rerender(
            TOKEN,
            ZOOM,
            crate::overlay_cache::ZoomDrive::AT_REST,
            &viewport(),
            &plan(),
        );
        assert!(
            !cache.needs_rerender(
                TOKEN,
                ZOOM,
                crate::overlay_cache::ZoomDrive::AT_REST,
                &viewport(),
                &plan()
            ),
            "premise: the parked texture must satisfy the gate for these exact \
             arguments, or the assertion below would pass without any toggle",
        );
    }

    toggle(&mut gui, false);
    toggle(&mut gui, true);

    let cache = gui.pane_mut(0).expect("pane 0").overlay_cache_mut(&KIND);
    assert!(
        cache.needs_rerender(
            TOKEN,
            ZOOM,
            crate::overlay_cache::ZoomDrive::AT_REST,
            &viewport(),
            &plan()
        ),
        "a re-enabled layer is not asking for its picture back: the cache is \
         empty and the gate still says no, so the layer would stay blank",
    );
}

/// **The motivating case.** The sync fan-out releases a linked pane's texture,
/// including the pane past `visible_pane_count` that no frame will ever paint.
#[test]
fn the_layer_sync_fan_out_releases_a_hidden_linked_panes_texture() {
    let ctx = egui::Context::default();
    let mut gui = skewed_gui(&ctx);
    for idx in [1, 2] {
        assert!(
            gui.pane(idx).expect("fixture pane").layer_link,
            "premise: the fan-out writes linked targets only, and this test is \
             about what it writes",
        );
    }

    toggle(&mut gui, false);

    for idx in [1, 2] {
        assert!(
            has_texture(gui.pane(idx).expect("fixture pane"), &KIND),
            "premise: pane {idx} must still be holding its own texture here — \
             the toggle above is about pane 0, and if this were already empty \
             the assertion after the fan-out would prove nothing",
        );
    }

    gui.propagate_pane_sync();

    for idx in [1, 2] {
        let target = gui.pane(idx).expect("fixture pane");
        assert!(
            !target.is_overlay_enabled(&KIND),
            "premise: the fan-out must really have copied the off-switch onto \
             pane {idx}",
        );
        assert!(
            !has_texture(target, &KIND),
            "pane {idx} adopted the off-switch and kept the texture",
        );
        assert!(
            has_texture(target, &squallar_source::id::known::RADAR),
            "the fan-out released pane {idx}'s radar raster",
        );
    }
    assert!(
        !has_texture(gui.pane(2).expect("pane 2"), &KIND),
        "pane 2 is past `visible_pane_count`, so no frame will ever run \
         `render_pane_map_content` for it — if this release does not empty its \
         cache, nothing in the application will",
    );
}

/// The **third** wholesale writer, and the one that does not look like a toggle
/// at all: a stored config landing mid-session.
#[test]
fn a_config_restored_mid_session_releases_what_it_switches_off() {
    let ctx = egui::Context::default();
    let mut gui = gui_with_parked_textures(&ctx);
    assert!(
        has_texture(gui.pane(0).expect("pane 0"), &KIND),
        "premise: the pane must be holding a texture before the restore, or the \
         assertion below is about a cache that was never populated",
    );

    let store = MemoryKvStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":1,"site":"KTLX",
                "panes":[{"site":"KTLX","enabled_overlays":{"NwsAlerts":false}}]}"#,
        )
        .expect("the memory store always accepts a write");
    assert!(
        gui.load_ui_config(&store),
        "the fixture config did not parse"
    );

    let pane = gui.pane(0).expect("pane 0");
    assert!(
        !pane.is_overlay_enabled(&KIND),
        "premise: the restore must really have switched the layer off",
    );
    assert!(
        !has_texture(pane, &KIND),
        "a config restored mid-session switched a layer off and left its \
         texture resident. This path can also convert the pane to a \
         cross-section, which `ui_map_pane`'s per-frame clear never runs for \
         again",
    );
    assert!(
        has_texture(pane, &squallar_source::id::known::RADAR),
        "the config restore released the radar raster",
    );
}

// ── An absence is not a curation ──────────────────────────────────────────

/// Take [`KIND`]'s slot off `pane` **leaving no tombstone** — the shape a pane
/// is in when a door refused to mint the slot (`PaneState::add_layer`,
/// `Gui::write_pane_overlay`, `Gui::initialize_pane_enabled` all return
/// without writing), and the shape a pane opened by `set_pane_count` is in
/// before it is seeded. `PaneState::remove_layer` is the other thing, and it
/// does leave one.
fn strip_without_tombstone(pane: &mut PaneState, kind: &squallar_source::id::LayerId) {
    let kept: Vec<crate::pane::LayerSlot> = pane
        .layers
        .take_slots()
        .into_iter()
        .filter(|slot| slot.id != *kind)
        .collect();
    pane.layers.set_slots(kept);
    assert!(
        !pane.layers.holds(kind) && !pane.layers.is_removed(kind),
        "the fixture must leave neither a slot nor a tombstone, or it is \
         testing the curation arm instead",
    );
}

/// **The regression: a loaded picture left the glass with nothing said.**
///
/// A pane the active one never held a layer on used to take that layer off
/// every linked sibling. The chain: a refusal leaves the active pane with no
/// slot; `propagate_layer_state` fans the stack out; its price
/// (`Gui::adopt_layers_increment`) counts only what a destination **gains**,
/// so a fan-out that takes layers away is `Increment::ZERO` and
/// `AdmissionLedger::ask` short-circuits it to an admission **without even
/// counting an ask**; `PaneState::adopt_layers` then replaces the destination
/// stack wholesale and `release_disabled_overlay_textures` clears the cache.
/// One refusal anywhere, and the whole linked group goes blank with no
/// verdict, no notice and nothing on `budget state:`.
///
/// A pane opened by `set_pane_count` reaches the same state without any
/// refusal at all: `PaneState::with_site` gives it `GroupId::FIRST` and
/// `layer_link: true` with an empty stack, so it joins the group as a source
/// of truth before anything has seeded it.
#[test]
fn the_fan_out_keeps_a_layer_the_source_pane_never_held() {
    let ctx = egui::Context::default();
    let mut gui = skewed_gui(&ctx);
    strip_without_tombstone(gui.pane_mut(0).expect("pane 0"), &KIND);

    for idx in [1, 2] {
        assert!(
            has_texture(gui.pane(idx).expect("fixture pane"), &KIND),
            "premise: pane {idx} must be drawing {KIND:?} before the fan-out, \
             or the assertion after it is satisfied by an empty start",
        );
    }

    let before = gui.admission().counts();
    gui.propagate_pane_sync();
    assert_eq!(
        gui.admission().counts().asked,
        before.asked,
        "premise: the fan-out really is a free act here — it asks nothing, \
         which is why nothing in the ledger could ever have shown this",
    );

    for idx in [1, 2] {
        let target = gui.pane(idx).expect("fixture pane");
        assert!(
            target.is_overlay_enabled(&KIND),
            "pane {idx} lost a layer the source pane never held. An absence is \
             not a curation: the source has no tombstone for it",
        );
        assert!(
            has_texture(target, &KIND),
            "pane {idx} kept the slot and lost the loaded texture, which is \
             the same disappearance one layer down",
        );
    }
}

/// **The control arm, one fact changed: the source pane *removed* the layer.**
///
/// A removal is the arrangement a linked group shares, it leaves a tombstone,
/// and it must still fan out — otherwise the fix above turns "keep what the
/// source never had" into "linking no longer removes", which is the opposite
/// defect and the one the tombstones were added for.
#[test]
fn the_fan_out_still_takes_away_a_layer_the_source_pane_removed() {
    let ctx = egui::Context::default();
    let mut gui = skewed_gui(&ctx);
    assert!(
        gui.pane_mut(0).expect("pane 0").remove_layer(&KIND),
        "premise: the removal must really happen, or this is the absence arm \
         again under another name",
    );
    assert!(
        gui.pane(0).expect("pane 0").layers.is_removed(&KIND),
        "premise: and it must have left the tombstone that tells the two \
         apart",
    );

    for idx in [1, 2] {
        assert!(
            has_texture(gui.pane(idx).expect("fixture pane"), &KIND),
            "premise: pane {idx} is drawing {KIND:?} before the fan-out",
        );
    }

    gui.propagate_pane_sync();

    for idx in [1, 2] {
        let target = gui.pane(idx).expect("fixture pane");
        assert!(
            !target.layers.holds(&KIND),
            "pane {idx} kept a layer the group curated out",
        );
        assert!(
            !has_texture(target, &KIND),
            "pane {idx} adopted the removal and kept the texture",
        );
        assert!(
            has_texture(target, &squallar_source::id::known::RADAR),
            "the fan-out released pane {idx}'s radar raster",
        );
    }
}

/// **The kept slot lands at its draw-order weight**, not on top of the stack
/// it was merged back into. A layer preserved above every other one would draw
/// over the group's arrangement, which is a second, quieter way for the fix
/// above to change what the user sees.
#[test]
fn a_kept_slot_lands_in_draw_order() {
    let ctx = egui::Context::default();
    let mut gui = skewed_gui(&ctx);
    strip_without_tombstone(gui.pane_mut(0).expect("pane 0"), &KIND);

    let weights: HashMap<squallar_source::id::LayerId, u32> = gui
        .overlays
        .handlers()
        .map(|h| (h.id(), h.draw_order_weight()))
        .collect();
    gui.propagate_pane_sync();

    let order: Vec<squallar_source::id::LayerId> = gui
        .pane(1)
        .expect("pane 1")
        .layers
        .iter()
        .map(|slot| slot.id.clone())
        .collect();
    let kept = order
        .iter()
        .position(|id| *id == KIND)
        .expect("the fan-out kept the slot — the test above holds that");
    assert!(
        order
            .last()
            .is_some_and(|last| weights[last] > weights[&KIND]),
        "premise: something in the group's arrangement must sort ABOVE {KIND:?}, \
         or appending and placing produce the same list and this test cannot \
         fail: {order:?}",
    );
    assert!(
        kept + 1 < order.len(),
        "the kept slot was appended to the top of the stack: {order:?}",
    );
    let sequence: Vec<u32> = order.iter().map(|id| weights[id]).collect();
    assert!(
        sequence.windows(2).all(|pair| pair[0] <= pair[1]),
        "the kept slot landed out of draw order, so it would paint over the \
         arrangement the group shares: {sequence:?} for {order:?}",
    );
}
