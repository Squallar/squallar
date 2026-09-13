//! **What the as-of half of the cache token pays to find its own handler.**
//!
//! [`super::overlay_cache_token`] is asked once per layer, per pane, per frame
//! by the walk's cache-token pass and again by
//! [`super::ground_content_key`] on a pane wearing a 3D floor strip. On a pane
//! following live its as-of half returns on an enum test and costs nothing;
//! on a **scrubbed** pane it has to reach the layer's handler to ask what time
//! axis the layer lives on.
//!
//! It used to reach it by walking the registry's handler vector and calling
//! the virtual `OverlayHandler::id` on each candidate — one indirect call and
//! one `LayerId` built and dropped per handler sitting *ahead of* the one it
//! wanted. The registry's own resolver answers the same question off the id
//! list it was built with, making no such call.
//!
//! **Why no existing gate saw this.**
//! `lookup_tax_tests::registry_lookups_ask_no_handler_its_own_id` asserts
//! exactly this property — zero `id` calls — and it was green over the
//! defect, for two independent reasons. Its fixture's pane follows live, so
//! the branch never ran; and the scan bypassed
//! `squallar_overlays::render::overlay_state::lookup_ledger` entirely,
//! because only the registry's own resolver notes it, so the scan would have
//! been invisible to that ceiling even on a scrubbed pane. This counts the
//! calls at the handler instead, where nothing can be bypassed.
//!
//! **Denominator.** One [`super::overlay_cache_token`] per registered layer,
//! on one scrubbed pane — which is what the walk's cache-token pass makes per
//! pane per frame. Not a frame figure and not a six-pane figure.

use super::*;
use crate::pane::{PaneState, TimeMode};
use squallar_overlays::render::overlay_state::{
    FetchPayload, OverlayHandler, OverlayItem, OverlayRegistry, PaneRef, RenderMode, Surface,
};
use squallar_source::time::TimeAxis;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The number of handlers the probe registry holds — the size
/// [`crate::sources::all`] registers, so the scan this gate prices is the
/// length the real walk would have paid.
const PROBE_HANDLERS: usize = 18;

/// The ids the probe registry registers, spelled so no two collide and none
/// is a `known` layer the walk has a special arm for.
fn probe_ids() -> Vec<LayerId> {
    (0..PROBE_HANDLERS)
        .map(|i| LayerId::new(format!("AsOfLookupProbe{i:02}")))
        .collect()
}

fn ts(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 6, 1)
        .expect("a real date")
        .and_hms_opt(12, minute, 0)
        .expect("a real time")
}

/// A texture layer on the event-lifetime axis that **counts every time it is
/// asked its own identity**.
///
/// Everything else answers the trait's own default, so what the fixture below
/// exercises is the as-of term's resolution and nothing else.
struct IdProbe {
    id: LayerId,
    /// Shared with every other probe in the registry, so one read is the
    /// whole pass's figure.
    asked: Arc<AtomicU64>,
}

impl OverlayHandler for IdProbe {
    fn id(&self) -> LayerId {
        self.asked.fetch_add(1, Ordering::Relaxed);
        self.id.clone()
    }
    fn surface(&self) -> Surface {
        Surface::Glass
    }
    fn draw_order_weight(&self) -> u32 {
        100
    }
    fn display_name(&self) -> &str {
        "IdProbe"
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
    fn time_axis(&self) -> TimeAxis {
        // The one axis whose picture is a function of the depicted instant,
        // and therefore the axis the as-of term has to resolve a handler to
        // recognise.
        TimeAxis::EventLifetime
    }
}

/// What one cache-token pass over a scrubbed pane cost and produced.
struct TokenPass {
    /// `OverlayHandler::id` calls the pass made, counted at the handlers.
    asked: u64,
    /// The token each layer came out with, in registry order.
    tokens: Vec<u64>,
}

/// One [`super::overlay_cache_token`] per registered layer, on a pane parked
/// at `at`.
///
/// The counter is zeroed **after** the fixture is built: `with_handlers`,
/// `set_overlay_enabled` and `hydrate_layer_states` all ask a handler its id,
/// and those are a config load's cost, not a frame's.
fn token_pass(at: chrono::NaiveDateTime) -> TokenPass {
    let asked = Arc::new(AtomicU64::new(0));
    let ids = probe_ids();
    let handlers: Vec<Box<dyn OverlayHandler>> = ids
        .iter()
        .map(|id| {
            Box::new(IdProbe {
                id: id.clone(),
                asked: Arc::clone(&asked),
            }) as Box<dyn OverlayHandler>
        })
        .collect();
    let overlays = OverlayRegistry::with_handlers(handlers);

    let mut pane = PaneState::new();
    for id in &ids {
        pane.set_overlay_enabled(id.clone(), true);
    }
    pane.hydrate_layer_states(&overlays, 0);
    // A park clears the live flag, as every scrub, step and Set Time does.
    pane.set_viewing_live(false);
    pane.set_time_mode(TimeMode::AsOf(at));

    asked.store(0, Ordering::Relaxed);
    let tokens = ids
        .iter()
        .map(|id| overlay_cache_token(&overlays, 0, &pane, id, false))
        .collect();
    TokenPass {
        asked: asked.load(Ordering::Relaxed),
        tokens,
    }
}

/// **Resolving a scrubbed layer's handler must not ask a handler its own id.**
///
/// A resolver that scans makes one indirect call and one `LayerId` per
/// candidate it strides past, so over a whole pass it is quadratic in the
/// registry's size: with `PROBE_HANDLERS` handlers the defect made
/// `PROBE_HANDLERS * (PROBE_HANDLERS + 1) / 2` of them for a question the
/// registry already holds the answer to. Zero is what an indexed resolver
/// makes, on a hit and on a miss alike.
#[test]
fn a_scrubbed_panes_as_of_term_asks_no_handler_its_own_id() {
    let pass = token_pass(ts(0));
    // Printed whether or not the assertion fires: the figure is the finding.
    eprintln!(
        "one scrubbed pane, {PROBE_HANDLERS} layers: {} `OverlayHandler::id` \
         calls over {} tokens",
        pass.asked,
        pass.tokens.len(),
    );
    assert!(
        pass.tokens.len() > 1,
        "premise: the pass covered {} layers, and a one-handler registry makes \
         a scan and an index cost the same — this gate would assert nothing",
        pass.tokens.len(),
    );
    assert_eq!(
        pass.asked, 0,
        "the as-of term asked handlers their own identity {} times over \
         {PROBE_HANDLERS} layers. It is resolving the layer by walking the \
         handler vector, which costs an indirect call and a `LayerId` per \
         handler ahead of the one it wants — per layer, per pane, per frame, \
         on every scrubbed pane.",
        pass.asked,
    );
}

/// **The other direction: the term is still resolved.**
///
/// A resolution deleted outright — `as_of_term` returning `0` before it looks
/// anything up — would make the count above zero and leave a scrubbed pane
/// drawing the live pane's rasters. So this asserts the term still moves the
/// token when the depicted instant crosses the layer's own quantum, with the
/// probe on the trait's default 60-second one.
#[test]
fn the_as_of_term_still_moves_the_token_across_a_quantum() {
    let noon = token_pass(ts(0));
    let same_quantum = token_pass(ts(0));
    let next_quantum = token_pass(ts(2));

    assert_eq!(
        noon.tokens, same_quantum.tokens,
        "two passes at the same instant produced different tokens, so the \
         comparison below cannot tell a moved clock from a noisy one",
    );
    assert_ne!(
        noon.tokens, next_quantum.tokens,
        "a scrubbed pane two minutes further back got the same tokens across \
         all {PROBE_HANDLERS} event-lifetime layers, over a 60-second \
         quantum: the as-of term is no longer reaching the handler at all, \
         and every scrubbed pane is about to draw the live pane's pictures.",
    );
}
