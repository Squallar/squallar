//! **What the per-layer fetch context pays to find its own handler.**
//!
//! [`super::fetch_config_for_layer`] narrows three fields for one named
//! layer — the instant it depicts ([`super::as_of_for_layer`]), how wide a
//! span it reaches back over, and which instants its transport can stop on —
//! and each of the three answers the same question first: *what time axis is
//! this one layer on?*
//!
//! Each used to answer it by walking the registry's handler vector and calling
//! the virtual `OverlayHandler::id` on every candidate up to the one it
//! wanted, comparing a freshly built `LayerId` each time. The registry holds
//! an index for exactly that question and its resolver makes no such call, so
//! the walk is pure tax: three of them, per layer, per resolution.
//!
//! **Denominator.** One [`super::fetch_config_for_layer`] per registered
//! layer, on one scrubbed pane. That is what a fetch round pays per layer it
//! dispatches, and — through the `as_of` field alone — what
//! `App::spawn_overlay_render` pays per overlay raster it dispatches. Not a
//! frame figure and not a six-pane figure.

use super::{as_of_for_layer, fetch_config_for_layer};
use squallar_egui::Gui;
use squallar_egui::pane::TimeMode;
use squallar_overlays::render::overlay_state::{
    FetchPayload, OverlayHandler, OverlayItem, OverlayRegistry, PaneRef, RenderMode, Surface,
};
use squallar_source::id::LayerId;
use squallar_source::time::TimeAxis;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The number of handlers the probe registry holds — the size
/// `squallar_egui::sources::all` registers, so the scan this gate prices is
/// the length the real dispatch would have paid.
const PROBE_HANDLERS: usize = 18;

/// The ids the probe registry registers, spelled so no two collide and none is
/// a `known` layer any arm has a special case for.
fn probe_ids() -> Vec<LayerId> {
    (0..PROBE_HANDLERS)
        .map(|i| LayerId::new(format!("FetchScanProbe{i:02}")))
        .collect()
}

fn ts(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 6, 1)
        .expect("a real date")
        .and_hms_opt(12, minute, 0)
        .expect("a real time")
}

/// A layer on the event-lifetime axis that **counts every time it is asked its
/// own identity**.
///
/// `EventLifetime` because it is the arm all three narrowings act on: a probe
/// on `Live` would be answered by an enum test in two of the three and the
/// gate would be pricing a branch that does not run.
struct IdProbe {
    id: LayerId,
    /// Shared with every other probe in the registry, so one read is the whole
    /// pass's figure.
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
        TimeAxis::EventLifetime
    }
}

/// A [`Gui`] whose registry is `PROBE_HANDLERS` counting probes, with pane 0
/// scrubbed to `at`, and the shared counter.
///
/// The counter is zeroed **after** the fixture is built: `with_handlers` asks
/// every handler its id once to fill the registry's index, and that is a
/// construction cost, not a dispatch's.
fn scrubbed_gui(at: chrono::NaiveDateTime) -> (Gui, Vec<LayerId>, Arc<AtomicU64>) {
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
    let mut gui = Gui::new();
    gui.overlays = OverlayRegistry::with_handlers(handlers);
    // A park clears the live flag, as every scrub, step and Set Time does.
    gui.pane_mut(0).expect("pane 0").set_viewing_live(false);
    gui.pane_mut(0)
        .expect("pane 0")
        .set_time_mode(TimeMode::AsOf(at));
    asked.store(0, Ordering::Relaxed);
    (gui, ids, asked)
}

fn base_config(
    clock: chrono::NaiveDateTime,
) -> squallar_overlays::render::overlay_state::FetchConfig {
    squallar_overlays::render::overlay_state::FetchConfig {
        client: {
            squallar_source::tls::init();
            reqwest::Client::new()
        },
        zone_cache_dir: None,
        sources: squallar_radar::sources::DataSources::production(),
        viewport: None,
        as_of: clock,
        depicted_span_secs: None,
        depicted_frames: Vec::new(),
    }
}

/// **Narrowing a layer's fetch context must not ask a handler its own id.**
///
/// A resolver that scans makes one indirect call and one `LayerId` per
/// candidate it strides past, so over a whole registry it is quadratic in the
/// registry's size — and [`super::fetch_config_for_layer`] resolves the same
/// layer three separate times, so it is three of those. Zero is what an
/// indexed resolver makes, on a hit and on a miss alike.
#[test]
fn narrowing_a_layers_fetch_context_asks_no_handler_its_own_id() {
    let clock = ts(30);
    let (gui, ids, asked) = scrubbed_gui(ts(0));
    for id in &ids {
        let _ = fetch_config_for_layer(&gui, 0, id, base_config(clock));
    }
    let count = asked.load(Ordering::Relaxed);
    // Printed whether or not the assertion fires: the figure is the finding.
    eprintln!(
        "one scrubbed pane, {PROBE_HANDLERS} layers: {count} `OverlayHandler::id` \
         calls narrowing {} fetch contexts",
        ids.len(),
    );
    assert!(
        ids.len() > 1,
        "premise: the pass covered {} layers, and a one-handler registry makes \
         a scan and an index cost the same — this gate would assert nothing",
        ids.len(),
    );
    assert_eq!(
        count, 0,
        "narrowing a fetch context asked handlers their own identity {count} \
         times over {PROBE_HANDLERS} layers. One of its three narrowings is \
         resolving the layer by walking the handler vector, which costs an \
         indirect call and a `LayerId` per handler ahead of the one it wants."
    );
}

/// **The as-of half alone, on the path that pays it most often.**
///
/// `App::spawn_overlay_render` calls [`super::as_of_for_layer`] and neither of
/// its two siblings, once per overlay raster it dispatches — so this prices
/// that path on its own rather than inferring it from the figure above.
#[test]
fn the_as_of_narrowing_alone_asks_no_handler_its_own_id() {
    let clock = ts(30);
    let (gui, ids, asked) = scrubbed_gui(ts(0));
    for id in &ids {
        let _ = as_of_for_layer(&gui, 0, id, clock);
    }
    let count = asked.load(Ordering::Relaxed);
    eprintln!(
        "one scrubbed pane, {PROBE_HANDLERS} layers: {count} `OverlayHandler::id` \
         calls over {} as-of narrowings",
        ids.len(),
    );
    assert_eq!(
        count, 0,
        "the as-of narrowing asked handlers their own identity {count} times \
         over {PROBE_HANDLERS} layers — one raster dispatch's worth per layer."
    );
}

/// **The other direction: the narrowing still happens.**
///
/// A predicate deleted outright would make both counts zero and leave a
/// scrubbed pane fetching the live pane's context. So this asserts an
/// event-lifetime layer on a scrubbed pane is still handed the pane's own
/// clock, and that a layer the registry does not hold is still handed the
/// fallback.
#[test]
fn a_scrubbed_panes_event_layer_is_still_told_its_own_clock() {
    let clock = ts(30);
    let scrubbed = ts(0);
    let (gui, ids, _asked) = scrubbed_gui(scrubbed);
    assert_eq!(
        as_of_for_layer(&gui, 0, &ids[PROBE_HANDLERS - 1], clock),
        scrubbed,
        "the last probe in the registry is on the event-lifetime axis and the \
         pane is scrubbed, so it must depict the pane's instant",
    );
    assert_eq!(
        as_of_for_layer(&gui, 0, &LayerId::new("NotRegisteredAtAll"), clock),
        clock,
        "an id this build does not register has no declared axis, so it keeps \
         the fallback clock",
    );
}
