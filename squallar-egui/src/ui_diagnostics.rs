//! The frame diagnostics overlay: service, segment and cadence percentiles
//! over a trailing window, on screen — the readout for a device whose console
//! is out of reach.
//!
//! **Every figure is a trailing-window reading, never cumulative-from-boot.**
//! The frame instrument's histograms only ever grow, so the panel keeps the
//! snapshot it took one window ago and shows [`Hist::diff`] of that against
//! the current one. A cumulative display would keep boot and every quiet
//! minute since inside every percentile it quotes.
//!
//! While visible, the panel asks for a repaint once per [`WINDOW_PERIOD`] —
//! never per frame — so an otherwise idle scene stays an idle scene with a
//! slow pulse, not a continuous repaint loop.
//!
//! The three denominators are the frame ledger's, restated on the rows that
//! carry them: **service** is the redraw minus the swapchain acquire,
//! **interact vs idle** splits presented frames by whether their input
//! carried a pointer/touch/wheel event, and **cadence** is the interval
//! between consecutive presented frames — never added to service.

use squallar_device_profile::hist::Hist;
use std::time::Duration;
use web_time::Instant;

/// How long the trailing window is, and how often the panel repaints while
/// visible. The same 2 s cadence the frame-telemetry log lines print at, so
/// the panel and a captured log describe the same span of frames.
pub(in crate::ui) const WINDOW_PERIOD: Duration = Duration::from_secs(2);

/// What the gpu-passes row says while no probe has supplied a figure — the
/// same absence-not-extrapolation contract the telemetry lines hold.
const GPU_PASSES_ABSENT: &str = "gpu passes: unavailable";

/// One cumulative reading of every histogram the panel windows. The fields
/// are copies: a snapshot must hold still while its recorder keeps counting,
/// or the diff against it would be a diff against itself.
#[derive(Clone, Copy)]
struct Snapshot {
    service_interact: Hist,
    service_idle: Hist,
    /// `[pre, pump, ui, prepare, finish, post]`, interact frames only.
    segments: [Hist; 6],
    acquire: Hist,
    cadence: Hist,
}

/// Every count zero — what the figure rows render from until the first
/// window closes.
const EMPTY_SNAPSHOT: Snapshot = Snapshot {
    service_interact: Hist::new(),
    service_idle: Hist::new(),
    segments: [Hist::new(); 6],
    acquire: Hist::new(),
    cadence: Hist::new(),
};

impl Snapshot {
    fn copied(d: &crate::shell_api::FrameDiagnostics<'_>) -> Self {
        Self {
            service_interact: *d.service_interact,
            service_idle: *d.service_idle,
            segments: [
                *d.segments[0],
                *d.segments[1],
                *d.segments[2],
                *d.segments[3],
                *d.segments[4],
                *d.segments[5],
            ],
            acquire: *d.acquire,
            cadence: *d.cadence,
        }
    }

    /// The counts gained since `earlier` — the windowed view, field by field.
    fn diff(&self, earlier: &Snapshot) -> Snapshot {
        Snapshot {
            service_interact: self.service_interact.diff(&earlier.service_interact),
            service_idle: self.service_idle.diff(&earlier.service_idle),
            segments: [
                self.segments[0].diff(&earlier.segments[0]),
                self.segments[1].diff(&earlier.segments[1]),
                self.segments[2].diff(&earlier.segments[2]),
                self.segments[3].diff(&earlier.segments[3]),
                self.segments[4].diff(&earlier.segments[4]),
                self.segments[5].diff(&earlier.segments[5]),
            ],
            acquire: self.acquire.diff(&earlier.acquire),
            cadence: self.cadence.diff(&earlier.cadence),
        }
    }
}

/// The overlay's windowing state. Empty whenever the overlay is hidden —
/// figures never span a period the panel was closed for.
#[derive(Default)]
pub(in crate::ui) struct DiagnosticsState {
    /// The cumulative snapshot the next window is measured from.
    baseline: Option<Snapshot>,
    /// When [`Self::baseline`] was taken.
    baseline_at: Option<Instant>,
    /// The trailing window on display: the diff of the two most recent
    /// baselines. `None` until the first window closes.
    window: Option<Snapshot>,
    /// The gpu-passes row, verbatim, once a probe supplies one.
    gpu_passes: Option<String>,
}

impl DiagnosticsState {
    /// Take this frame's facts. Hidden clears everything; shown takes a
    /// baseline if none stands, and closes the window once
    /// [`WINDOW_PERIOD`] has passed since the standing one.
    ///
    /// `now` is a parameter rather than a clock read so a test can close a
    /// window without waiting one out.
    pub(in crate::ui) fn observe(
        &mut self,
        shown: bool,
        inputs: Option<&crate::shell_api::FrameDiagnostics<'_>>,
        now: Instant,
    ) {
        if !shown {
            if self.baseline.is_some() || self.gpu_passes.is_some() {
                *self = Self::default();
            }
            return;
        }
        let Some(d) = inputs else {
            return;
        };
        if self.gpu_passes.as_deref() != d.gpu_passes {
            self.gpu_passes = d.gpu_passes.map(str::to_owned);
        }
        match (&self.baseline, self.baseline_at) {
            (Some(baseline), Some(at)) => {
                if now.duration_since(at) >= WINDOW_PERIOD {
                    let current = Snapshot::copied(d);
                    self.window = Some(current.diff(baseline));
                    self.baseline = Some(current);
                    self.baseline_at = Some(now);
                }
            }
            _ => {
                self.baseline = Some(Snapshot::copied(d));
                self.baseline_at = Some(now);
            }
        }
    }

    /// How long until the standing window closes — what the panel hands to
    /// `request_repaint_after`, so the repaint lands when the figures move
    /// and not once per frame.
    pub(in crate::ui) fn due_in(&self, now: Instant) -> Duration {
        match self.baseline_at {
            Some(at) => WINDOW_PERIOD.saturating_sub(now.duration_since(at)),
            // No baseline standing: the next frame's observe takes one.
            None => Duration::ZERO,
        }
    }

    /// The panel's rows, id and text, in draw order. The roster is fixed —
    /// a row whose data is absent says so in its text rather than vanishing.
    pub(in crate::ui) fn rows(&self) -> Vec<(&'static str, String)> {
        let w = self.window.as_ref().unwrap_or(&EMPTY_SNAPSHOT);
        vec![
            (
                "window",
                match self.window {
                    Some(_) => "trailing 2 s - service excludes acquire".to_owned(),
                    None => "collecting the first 2 s window...".to_owned(),
                },
            ),
            (
                "service.interact",
                format!(
                    "interact n={}  p50 {}  p95 {}  p99 {} ms",
                    w.service_interact.total(),
                    pctl_ms(&w.service_interact, 0.50),
                    pctl_ms(&w.service_interact, 0.95),
                    pctl_ms(&w.service_interact, 0.99),
                ),
            ),
            (
                "service.idle",
                format!(
                    "idle     n={}  p50 {}  p95 {}  p99 {} ms",
                    w.service_idle.total(),
                    pctl_ms(&w.service_idle, 0.50),
                    pctl_ms(&w.service_idle, 0.95),
                    pctl_ms(&w.service_idle, 0.99),
                ),
            ),
            (
                "segments",
                format!(
                    "seg p99 (interact): pre {} pump {} ui {} prep {} fin {} post {} ms",
                    pctl_ms(&w.segments[0], 0.99),
                    pctl_ms(&w.segments[1], 0.99),
                    pctl_ms(&w.segments[2], 0.99),
                    pctl_ms(&w.segments[3], 0.99),
                    pctl_ms(&w.segments[4], 0.99),
                    pctl_ms(&w.segments[5], 0.99),
                ),
            ),
            (
                "acquire",
                format!(
                    "acquire n={}  p50 {}  p99 {} ms - vsync wait, not service",
                    w.acquire.total(),
                    pctl_ms(&w.acquire, 0.50),
                    pctl_ms(&w.acquire, 0.99),
                ),
            ),
            (
                "cadence",
                format!(
                    "cadence n={}  p50 {}  p95 {}  p99 {} ms - presented-frame interval",
                    w.cadence.total(),
                    pctl_ms(&w.cadence, 0.50),
                    pctl_ms(&w.cadence, 0.95),
                    pctl_ms(&w.cadence, 0.99),
                ),
            ),
            (
                "gpu",
                self.gpu_passes
                    .clone()
                    .unwrap_or_else(|| GPU_PASSES_ABSENT.to_owned()),
            ),
        ]
    }
}

/// One percentile as milliseconds for a row: `-` on an empty histogram,
/// `over` when the ranked sample sits in the at-or-over-64 ms clamp (whose
/// upper edge does not exist), otherwise the conservative bin upper edge —
/// see [`Hist::percentile_upper_micros`].
fn pctl_ms(h: &Hist, q: f64) -> String {
    match h.percentile_upper_micros(q) {
        None => "-".to_owned(),
        Some(u32::MAX) => "over".to_owned(),
        Some(us) => {
            let ms = f64::from(us) / 1000.0;
            if ms >= 100.0 {
                format!("{ms:.0}")
            } else if ms >= 10.0 {
                format!("{ms:.1}")
            } else {
                format!("{ms:.2}")
            }
        }
    }
}

impl super::Gui {
    /// Draw the overlay, when its switch is on. The window's close button is
    /// the same switch: dismissing the panel here persists exactly as
    /// unticking it in Settings does.
    pub(super) fn render_diagnostics_panel(&mut self, ctx: &egui::Context) {
        if !self.diagnostics_panel {
            return;
        }
        ctx.request_repaint_after(self.diagnostics.due_in(Instant::now()));
        let rows = self.diagnostics.rows();
        let mut open = true;
        egui::Window::new("Frame diagnostics")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .default_pos(ctx.content_rect().right_top() + egui::vec2(-420.0, 48.0))
            .show(ctx, |ui| {
                for (id, text) in &rows {
                    ui.label(egui::RichText::new(text).monospace().small());
                    #[cfg(test)]
                    self.probes.last_diagnostics_rows.push(*id);
                    #[cfg(not(test))]
                    let _ = id;
                }
            });
        if !open {
            self.diagnostics_panel = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell_api::FrameDiagnostics;

    /// Every field of a [`FrameDiagnostics`] pointed at one recorder — the
    /// panel windows each field independently, so one recorder driving all
    /// of them exercises the mechanism without ten parallel scripts.
    fn all_fields(h: &Hist) -> FrameDiagnostics<'_> {
        FrameDiagnostics {
            service_interact: h,
            service_idle: h,
            segments: [h; 6],
            acquire: h,
            cadence: h,
            gpu_passes: None,
        }
    }

    fn row<'a>(rows: &'a [(&'static str, String)], id: &str) -> &'a str {
        &rows
            .iter()
            .find(|(row_id, _)| *row_id == id)
            .unwrap_or_else(|| panic!("the panel roster lost its {id} row"))
            .1
    }

    /// The figures on display are the diff of the two most recent snapshots,
    /// not the recorder's cumulative-from-boot totals.
    #[test]
    fn the_figures_are_a_trailing_window_not_the_boot_cumulative() {
        let mut recorder = Hist::new();
        let mut state = DiagnosticsState::default();
        let t0 = Instant::now();

        // Three 1 ms samples before the baseline; they are "boot".
        for _ in 0..3 {
            recorder.record(1_000);
        }
        state.observe(true, Some(&all_fields(&recorder)), t0);
        assert!(
            row(&state.rows(), "window").contains("collecting"),
            "one snapshot is not a window; the panel must say it is collecting",
        );

        // Two 8 ms samples inside the window.
        let at_baseline = recorder;
        for _ in 0..2 {
            recorder.record(8_000);
        }
        state.observe(
            true,
            Some(&all_fields(&recorder)),
            t0 + WINDOW_PERIOD + Duration::from_millis(50),
        );

        let rows = state.rows();
        let interact = row(&rows, "service.interact");
        assert_eq!(
            recorder.total(),
            5,
            "control: the cumulative recorder holds all five samples",
        );
        assert!(
            interact.contains("n=2"),
            "the window holds the two samples recorded inside it; row: {interact}",
        );
        assert!(
            !interact.contains("n=5"),
            "cumulative-from-boot leaked into the panel; row: {interact}",
        );
        // And the distribution is the window's, not the boot mixture's: the
        // window is all-8 ms, so its p50 is 8 ms-ish where the cumulative
        // recorder's is 1 ms-ish. Formatted through the same helper the row
        // used, so the assertion cannot drift from the display.
        let windowed = recorder.diff(&at_baseline);
        assert!(
            interact.contains(&format!("p50 {}", pctl_ms(&windowed, 0.50))),
            "the row's p50 is not the windowed histogram's; row: {interact}",
        );
        assert_ne!(
            pctl_ms(&windowed, 0.50),
            pctl_ms(&recorder, 0.50),
            "non-triviality: the windowed and cumulative p50 must differ for \
             this fixture to prove anything",
        );
    }

    /// A window never closes early: a second observe inside the period leaves
    /// the standing baseline and the displayed window alone.
    #[test]
    fn a_window_holds_until_its_period_has_passed() {
        let mut recorder = Hist::new();
        let mut state = DiagnosticsState::default();
        let t0 = Instant::now();
        recorder.record(1_000);
        state.observe(true, Some(&all_fields(&recorder)), t0);
        recorder.record(1_000);
        state.observe(
            true,
            Some(&all_fields(&recorder)),
            t0 + WINDOW_PERIOD - Duration::from_millis(200),
        );
        assert!(
            row(&state.rows(), "window").contains("collecting"),
            "an observe inside the period must not close the window",
        );
    }

    /// Hiding the panel discards the baseline and window, so figures never
    /// span a period the panel was closed for.
    #[test]
    fn hiding_the_panel_discards_the_window() {
        let mut recorder = Hist::new();
        let mut state = DiagnosticsState::default();
        let t0 = Instant::now();
        recorder.record(1_000);
        state.observe(true, Some(&all_fields(&recorder)), t0);
        state.observe(
            true,
            Some(&all_fields(&recorder)),
            t0 + WINDOW_PERIOD + Duration::from_millis(50),
        );
        assert!(
            row(&state.rows(), "window").contains("trailing"),
            "precondition: a window is on display",
        );

        state.observe(false, None, t0 + WINDOW_PERIOD * 2);
        assert!(
            row(&state.rows(), "window").contains("collecting"),
            "a hidden panel must forget its window",
        );
    }

    /// The gpu-passes row prints absence text until a probe supplies a line,
    /// and the supplied line verbatim once one does.
    #[test]
    fn the_gpu_row_is_absence_text_until_a_probe_speaks() {
        let recorder = Hist::new();
        let mut state = DiagnosticsState::default();
        state.observe(true, Some(&all_fields(&recorder)), Instant::now());
        assert_eq!(row(&state.rows(), "gpu"), GPU_PASSES_ABSENT);

        // The shape the probe's line really has, so what this test feeds
        // through the seam is what the app composes.
        let line = "gpu passes: raymarch n=6, p50=900 us, p99=1200 us; \
                    ground n=0, p50=none, p99=none; mirror n=6, p50=400 us, \
                    p99=500 us; main n=6, p50=300 us, p99=400 us; 6 frames";
        let mut with_line = all_fields(&recorder);
        with_line.gpu_passes = Some(line);
        state.observe(true, Some(&with_line), Instant::now());
        assert_eq!(row(&state.rows(), "gpu"), line);
    }

    /// Off draws zero panel widgets; on draws the whole fixed roster — the
    /// count is from the draw itself, through the real `Gui::ui` pass.
    #[test]
    fn the_panel_draws_nothing_until_its_switch_is_on() {
        let mut h = crate::input_harness::InputHarness::new();
        h.frame();
        assert_eq!(
            h.gui().probes.last_diagnostics_rows,
            Vec::<&'static str>::new(),
            "with the switch off, the panel must draw zero widgets",
        );

        h.gui_mut().diagnostics_panel = true;
        h.frame();
        assert_eq!(
            h.gui().probes.last_diagnostics_rows,
            vec![
                "window",
                "service.interact",
                "service.idle",
                "segments",
                "acquire",
                "cadence",
                "gpu",
            ],
            "with the switch on, the panel draws its whole roster",
        );
    }

    /// The formatter's three regimes and both sentinels.
    #[test]
    fn percentile_formatting_names_its_sentinels() {
        assert_eq!(pctl_ms(&Hist::new(), 0.5), "-");
        let mut h = Hist::new();
        // 200 us lands in the [176.776, 210.224) us bin; the conservative
        // upper edge rounds up to 211 us.
        h.record(200);
        assert_eq!(pctl_ms(&h, 0.5), "0.21", "bin upper edge, two decimals");
        let mut over = Hist::new();
        over.record(70_000);
        assert_eq!(
            pctl_ms(&over, 0.5),
            "over",
            "the over-64 ms clamp has no upper edge to quote",
        );
    }
}
