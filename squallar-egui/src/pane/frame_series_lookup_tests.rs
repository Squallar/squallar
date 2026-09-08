//! **What "which of my layers can loop" pays to find each layer's handler.**
//!
//! [`PaneState::topmost_frame_series_layer`] and
//! [`PaneState::frame_series_layers`] both walk the pane's *enabled* slots and
//! ask [`super::comes_in_stamped_frames`] of each — so the cost of one call is
//! the pane's enabled count times the cost of one registry question.
//!
//! That question used to be spelled as a walk of the registry's handler
//! vector, calling the virtual `OverlayHandler::id` on every candidate and
//! comparing a freshly built `LayerId` against it until one matched — full
//! length on a miss. Measured through the real chrome on the harness's
//! one-pane scene it was **106 handler probes** for six enabled layers
//! (`input_harness::tests::a_frame_with_the_step_picker_closed_asks_no_frame_series_question`).
//! The registry's own resolver answers off the id list it was built with and
//! makes no such call.
//!
//! **Denominator.** One call to each of the two walks, on one pane with every
//! registered layer enabled. Neither walk is per-frame: their production
//! callers are `PaneState::refresh_transport` (a layer toggle, a loop toggle),
//! `begin_loop_for_pane` (arming a loop) and the step picker's dropdown body
//! (a frame the dropdown is open on). This is a per-call figure and not a
//! frame figure.

use super::{PaneState, comes_in_stamped_frames};
use squallar_overlays::render::overlay_state::{
    FetchPayload, OverlayHandler, OverlayItem, OverlayRegistry, PaneRef, RenderMode, Surface,
};
use squallar_source::id::LayerId;
use squallar_source::time::TimeAxis;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The number of handlers the probe registry holds — the size
/// [`crate::sources::all`] registers, so the scan this gate prices is the
/// length the real walk would have paid.
const PROBE_HANDLERS: usize = 18;

/// The ids the probe registry registers, spelled so no two collide and none is
/// a `known` layer any arm has a special case for.
fn probe_ids() -> Vec<LayerId> {
    (0..PROBE_HANDLERS)
        .map(|i| LayerId::new(format!("FrameSeriesProbe{i:02}")))
        .collect()
}

/// A layer that **counts every time it is asked its own identity**.
///
/// `on_frames` is what the predicate under test reads, and the fixture builds
/// both kinds: a registry of all-`FrameSeries` probes would let a walk that
/// answered `true` unconditionally pass the counting gate below.
struct IdProbe {
    id: LayerId,
    on_frames: bool,
    /// Shared with every other probe in the registry, so one read is the whole
    /// walk's figure.
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
        if self.on_frames {
            TimeAxis::FrameSeries {
                typical_step: std::time::Duration::from_secs(300),
                extends_future: false,
            }
        } else {
            TimeAxis::Live
        }
    }
}

/// A registry of `PROBE_HANDLERS` probes — every third one on the frame-series
/// axis — with a pane holding all of them enabled, and the shared counter.
///
/// The counter is zeroed **after** the fixture is built: `with_handlers` and
/// `hydrate_layer_states` both ask handlers their ids, and those are a config
/// load's cost, not a walk's.
fn all_layers_on() -> (OverlayRegistry, PaneState, Vec<LayerId>, Arc<AtomicU64>) {
    let (overlays, mut panes, ids, asked) = all_layers_on_panes(1);
    (overlays, panes.remove(0), ids, asked)
}

/// The same fixture with `panes` panes, each holding every registered layer —
/// the shape a six-pane scene has, and the denominator a one-pane rig hides.
fn all_layers_on_panes(
    panes: usize,
) -> (
    OverlayRegistry,
    Vec<PaneState>,
    Vec<LayerId>,
    Arc<AtomicU64>,
) {
    let asked = Arc::new(AtomicU64::new(0));
    let ids = probe_ids();
    let handlers: Vec<Box<dyn OverlayHandler>> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            Box::new(IdProbe {
                id: id.clone(),
                on_frames: i % 3 == 0,
                asked: Arc::clone(&asked),
            }) as Box<dyn OverlayHandler>
        })
        .collect();
    let overlays = OverlayRegistry::with_handlers(handlers);

    let built: Vec<PaneState> = (0..panes)
        .map(|idx| {
            let mut pane = PaneState::new();
            for id in &ids {
                pane.set_overlay_enabled(id.clone(), true);
            }
            pane.hydrate_layer_states(&overlays, idx);
            pane
        })
        .collect();

    asked.store(0, Ordering::Relaxed);
    (overlays, built, ids, asked)
}

/// **Asking which of a pane's layers come in stamped frames must not ask a
/// handler its own id.**
///
/// Both walks in one gate because both ask the same predicate, once per
/// enabled slot: a resolver that scans costs an indirect call and a `LayerId`
/// per candidate it strides past — full registry length for every layer that
/// is *not* on the frame axis, which on this fixture is two out of every
/// three. Zero is what an indexed resolver makes, on a hit and on a miss
/// alike.
#[test]
fn asking_which_layers_come_in_stamped_frames_asks_no_handler_its_own_id() {
    let (overlays, pane, ids, asked) = all_layers_on();

    let topmost = pane.topmost_frame_series_layer(&overlays).cloned();
    let after_topmost = asked.load(Ordering::Relaxed);
    let every = pane.frame_series_layers(&overlays);
    let total = asked.load(Ordering::Relaxed);

    // Printed whether or not the assertions fire: the figures are the finding.
    eprintln!(
        "one pane, {} enabled layers: {after_topmost} `OverlayHandler::id` calls \
         for the topmost, {total} for both walks",
        ids.len(),
    );
    assert!(
        ids.len() > 1,
        "premise: the pane holds {} layers, and a one-handler registry makes a \
         scan and an index cost the same — this gate would assert nothing",
        ids.len(),
    );
    // The other direction, in the same run: a predicate deleted outright would
    // read zero above and answer for every layer or for none.
    assert_eq!(
        every.len(),
        PROBE_HANDLERS.div_ceil(3),
        "the walk no longer picks out the frame-series layers; it found {:?}",
        every,
    );
    assert_eq!(
        topmost.as_ref(),
        every.last(),
        "the topmost frame-series layer must be the last member of the set the \
         other walk returns — the two predicates have drifted",
    );
    assert_eq!(
        total, 0,
        "the frame-series walks asked handlers their own identity {total} times \
         over {PROBE_HANDLERS} enabled layers. The predicate is resolving each \
         layer by walking the handler vector, which costs an indirect call and \
         a `LayerId` per handler ahead of the one it wants — and the full \
         registry length for every layer that is not on the frame axis.",
    );
}

/// **The predicate itself still reads the declared axis, both ways.**
///
/// The gate above prices the walks; this one holds the answer they are made
/// of, including for an id the registry does not hold — which must be `false`
/// rather than a panic, since a config may name a layer this build lacks.
#[test]
fn the_frame_series_predicate_reads_the_declared_axis() {
    let (overlays, _pane, ids, _asked) = all_layers_on();
    assert!(
        comes_in_stamped_frames(&overlays, &ids[0]),
        "probe 0 declares FrameSeries and was not recognised",
    );
    assert!(
        !comes_in_stamped_frames(&overlays, &ids[1]),
        "probe 1 declares Live and was recognised as coming in frames",
    );
    assert!(
        !comes_in_stamped_frames(&overlays, &LayerId::new("NotRegisteredAtAll")),
        "an id this build does not register has no declared axis, so it cannot \
         come in stamped frames",
    );
}

/// **The six-pane figure, measured rather than multiplied.**
///
/// The one-pane denominator is what has hidden every superlinear per-pane cost
/// this campaign: a cost whose underlying list grows with the group is not
/// six times its one-pane reading at six panes. This one is not such a cost —
/// the walk is over the *registry*, whose length is fixed however many panes
/// are open — and that is asserted here rather than assumed, so a future
/// spelling that starts consulting the pane's siblings fails instead of
/// quietly costing more.
#[test]
fn the_frame_series_walks_cost_the_same_at_six_panes_as_at_one() {
    const PANES: usize = 6;
    let (overlays, panes, _ids, asked) = all_layers_on_panes(PANES);
    for pane in &panes {
        let _ = pane.topmost_frame_series_layer(&overlays);
        let _ = pane.frame_series_layers(&overlays);
    }
    let six = asked.load(Ordering::Relaxed);

    let (overlays, panes, _ids, asked) = all_layers_on_panes(1);
    for pane in &panes {
        let _ = pane.topmost_frame_series_layer(&overlays);
        let _ = pane.frame_series_layers(&overlays);
    }
    let one = asked.load(Ordering::Relaxed);

    eprintln!("{PANES} panes: {six} `OverlayHandler::id` calls; one pane: {one}",);
    assert_eq!(
        six, 0,
        "{PANES} panes' frame-series walks asked handlers their own identity \
         {six} times",
    );
    assert_eq!(
        one, 0,
        "one pane's frame-series walks asked handlers their own identity {one} \
         times",
    );
}
