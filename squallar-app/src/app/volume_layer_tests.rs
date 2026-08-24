//! **The far end of the 3D ask: the layer it names, resolved.**
//!
//! The pane walks its own stack and sends the layer it landed on. This end
//! resolves that name to a handler and asks the handler for its 3D half —
//! and every one of these tests is about that resolution failing safely,
//! because the walk and the dispatch are separated by an action channel and a
//! pane can be re-stacked while an ask is in it.

use super::tests::headless;
use super::*;
use crate::platform_double::TestBridge;
use squallar_volumetric::bridge::VolumeEntry;

fn target(product: squallar_source::product::FieldId) -> squallar_egui::pane::VolumeTarget {
    squallar_egui::pane::VolumeTarget {
        volume: squallar_egui::pane::VolumeStamp {
            site: "KTLX".to_owned(),
            collected: chrono::NaiveDate::from_ymd_opt(2024, 5, 1)
                .expect("a real date")
                .and_hms_opt(12, 0, 0)
                .expect("a real time"),
        },
        product,
        region: None,
    }
}

/// The refusal `layer` produced for `target`, or `None` if it was not refused.
fn refusal(
    app: &mut App,
    layer: &squallar_source::id::LayerId,
    target: &squallar_egui::pane::VolumeTarget,
) -> Option<String> {
    app.handle_prepare_volume(0, layer, target.clone());
    match app.volume_store.lookup(target)?.entry {
        VolumeEntry::Refused(why) => Some(why),
        _ => None,
    }
}

/// **A layer with no 3D half cannot be asked for a grid**, and the refusal
/// names it rather than leaving a pane on an empty box wondering.
///
/// The layer chosen is a real, registered, *flat* one — not a fabricated id —
/// so this is the resolution refusing a handler that exists, which is the
/// reachable case.
#[test]
fn an_ask_naming_a_layer_with_no_3d_half_is_refused_by_name() {
    let mut app = headless(TestBridge::desktop());
    let flat = squallar_source::id::known::NWS_ALERTS;
    let why = refusal(
        &mut app,
        &flat,
        &target(squallar_radar::fields::known::REFLECTIVITY),
    )
    .expect("a layer with no 3D half must be refused, not silently dropped");
    assert!(
        why.contains("NWS") || why.contains("Alert"),
        "the refusal must name the layer that cannot build it; got {why:?}",
    );
}

/// **A refusal counts as served**, so the level-triggered ask quiesces instead
/// of re-emitting the same impossible request every frame for ever.
#[test]
fn a_refused_ask_stops_the_level_trigger_re_asking() {
    let mut app = headless(TestBridge::desktop());
    app.gui
        .pane_mut(0)
        .expect("pane 0")
        .set_view(squallar_radar::types::RenderView::Volume);
    let t = target(squallar_radar::fields::known::REFLECTIVITY);
    app.handle_prepare_volume(0, &squallar_source::id::known::NWS_ALERTS, t.clone());
    assert_eq!(
        app.gui
            .pane(0)
            .expect("pane 0")
            .volume()
            .and_then(|v| v.rendered_for.clone()),
        Some(t),
        "a refusal must mark the pane rendered for the target it refused; \
         otherwise the draw loop asks again on the very next frame",
    );
}

/// **A layer this build does not serve at all** — an id from a newer build —
/// is refused by that id rather than panicking or being served by whichever
/// handler happens to be first.
#[test]
fn an_ask_naming_a_layer_this_build_does_not_serve_is_refused_by_id() {
    let mut app = headless(TestBridge::desktop());
    let unknown = squallar_source::id::LayerId::new("a.layer.from.a.newer.build");
    let why = refusal(
        &mut app,
        &unknown,
        &target(squallar_radar::fields::known::REFLECTIVITY),
    )
    .expect("an unserved layer must be refused");
    assert!(
        why.contains(unknown.as_str()),
        "the refusal must name the id nothing serves; got {why:?}",
    );
}

/// **A field the named layer has no vertical structure for is refused by
/// name**, even though the layer itself does build volumes — the re-ask on
/// this side is about the pair, not about the layer alone.
#[test]
fn an_ask_for_a_field_with_no_vertical_structure_is_refused_by_name() {
    let mut app = headless(TestBridge::desktop());
    let flat_field = squallar_radar::fields::known::ECHO_TOPS;
    let spec = squallar_radar::fields::spec_for(&flat_field).expect("a registered field");
    assert!(
        !spec.vertical,
        "fixture precondition: the field must be registered as having no \
         vertical extent, or this test refuses nothing",
    );
    let why = refusal(
        &mut app,
        &squallar_source::id::known::RADAR,
        &target(flat_field),
    )
    .expect("a flat field must be refused");
    assert!(
        why.contains(spec.name),
        "the refusal must name the field; got {why:?}",
    );
}

/// **A field the named layer does not publish at all is refused by its id.**
#[test]
fn an_ask_for_a_field_the_layer_does_not_publish_is_refused_by_id() {
    let mut app = headless(TestBridge::desktop());
    let alien = squallar_source::product::FieldId::new("NotAFieldThisBuildRegisters");
    let why = refusal(
        &mut app,
        &squallar_source::id::known::RADAR,
        &target(alien.clone()),
    )
    .expect("an unpublished field must be refused");
    assert!(
        why.contains(alien.as_str()),
        "the refusal must name the field the layer does not publish; got {why:?}",
    );
}

/// **The resolution is about the layer and the field, never about the pane.**
///
/// The pane index an ask carries is the volume store's *holder* id, and it
/// routinely names a holder the `Gui`'s own pane vector has no entry for — a
/// headless app starts with exactly one. A resolution that went looking for a
/// pane would find none and refuse every such ask by silently doing nothing,
/// which is how a 3D pane waits for ever.
#[test]
fn an_ask_for_a_holder_beyond_the_pane_vector_is_still_resolved() {
    let mut app = headless(TestBridge::desktop());
    let beyond = app.gui.panes_and_overlays_mut().0.len();
    let t = target(squallar_radar::fields::known::REFLECTIVITY);
    app.handle_prepare_volume(beyond, &squallar_source::id::known::NWS_ALERTS, t.clone());
    assert!(
        matches!(
            app.volume_store.lookup(&t).map(|l| l.entry),
            Some(VolumeEntry::Refused(_)),
        ),
        "the ask must be resolved and answered for holder {beyond}, which the \
         pane vector (len {beyond}) does not have an entry for",
    );
}
