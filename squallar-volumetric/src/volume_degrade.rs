//! What to do after the volume view has already gone wrong once.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::OnceLock;

/// Whether the 3D volume view can be used, and if not, why not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VolumeSupport {
    /// Every capability the volume view needs is present.
    Supported,
    /// The view is unavailable. The string is user-facing: a full sentence,
    /// naming the limit or the event that ruled it out.
    Unavailable(String),
}

impl VolumeSupport {
    /// Whether a volume may be rendered.
    pub fn is_supported(&self) -> bool {
        matches!(self, Self::Supported)
    }

    /// The user-facing reason the view is unavailable, if it is.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Supported => None,
            Self::Unavailable(why) => Some(why),
        }
    }
}

/// How many surface losses with a volume on screen retire the view for good.
pub const MAX_SURFACE_LOSSES_WITH_VOLUME: u32 = 2;

/// How long after the capacity probe closes its window a loss is still the
/// probe's: 2000 ms. A browser dispatches the loss its exhaustion caused on
/// the event-loop turn after the exhausting step, and the app observes it on
/// its next frame — which under `ControlFlow::Wait` on an idle page may be as
/// far off as the telemetry tick, two seconds.
pub const PROBE_LOSS_GRACE_MS: u64 = 2000;

/// The label prefix every wgpu resource the volume view creates must carry.
pub const VOLUME_LABEL_PREFIX: &str = "squallar.volume";

/// How wgpu introduces a resource label inside an error description.
const LABEL_INTRO: &str = "label = '";

/// The failures recorded against the volume view, and the one window in
/// which a failure is somebody else's doing.
///
/// The GPU capacity probe on a WebGL2 page (`squallar_web::gpu_probe::webgl2`)
/// allocates until the browser refuses, and a browser's refusal can be to
/// lose **every** WebGL context in the tab — the app's own included. That
/// loss is the probe's, not the volume view's, and must not retire the view.
/// So the probe opens a window here before its first allocation and closes
/// it, with [`PROBE_LOSS_GRACE_MS`] to spare, when it is done; a loss or a
/// device error that lands inside the window is counted in its own field and
/// latched nowhere. The window is bounded on both ends — an opener names how
/// long it may stay open at most, and the guard's drop closes it — so a probe
/// that never returns cannot leave the latch disarmed.
///
/// **What the window can see today: nothing.** Both entry points it guards
/// are fed by wgpu — a `SurfaceStatus::Lost` from `get_current_texture`, and
/// a volume-labelled error on the uncaptured-error sink — and on wgpu-hal
/// 29.0.4's web surface a lost WebGL2 context produces neither:
/// `acquire_texture` hands back the pre-configured swapchain texture
/// unconditionally, nothing in the gles backend consults `isContextLost()`,
/// and nothing in this tree or in winit listens for `webglcontextlost`. A
/// Firefox that loses the app's context today leaves a canvas nobody
/// restores, and this latch is never told. The window is the contract for
/// the day a restore path exists and a loss does reach here; its current
/// effect on the view is nil, and the probe's own ladder cap — not this — is
/// what keeps the probe from losing the app's context.
///
/// A struct rather than loose statics so the policy is testable against a
/// fresh instance with a clock a test drives; the one process-global instance
/// is [`LATCH`], and the free functions below wrap it with the crate clock.
struct Latch {
    /// Surface losses observed while a volume was on screen.
    surface_losses_with_volume: AtomicU32,
    /// Whether an uncaptured device error has ever been attributed to the volume.
    volume_device_error: AtomicBool,
    /// Millis on the clock the caller supplies until which a loss is the
    /// capacity probe's own; `0` when no window is open.
    probe_window_closes_at_ms: AtomicU64,
    /// Surface losses that landed inside the probe's window: recorded, never counted.
    losses_in_probe_window: AtomicU32,
    /// Device errors that landed inside the probe's window: recorded, never latched.
    device_errors_in_probe_window: AtomicU32,
}

impl Latch {
    const fn new() -> Self {
        Self {
            surface_losses_with_volume: AtomicU32::new(0),
            volume_device_error: AtomicBool::new(false),
            probe_window_closes_at_ms: AtomicU64::new(0),
            losses_in_probe_window: AtomicU32::new(0),
            device_errors_in_probe_window: AtomicU32::new(0),
        }
    }

    /// Whether a failure at `now_ms` is the probe's: the window is open and
    /// has not reached its close.
    fn probe_window_is_open(&self, now_ms: u64) -> bool {
        now_ms < self.probe_window_closes_at_ms.load(Ordering::Relaxed)
    }

    /// Record a surface loss with a volume on screen at `now_ms`. Returns the
    /// losses counted against the view so far — unchanged when this one fell
    /// inside the probe's window.
    fn note_surface_loss(&self, now_ms: u64) -> u32 {
        if self.probe_window_is_open(now_ms) {
            self.losses_in_probe_window.fetch_add(1, Ordering::Relaxed);
            return self.surface_losses_with_volume.load(Ordering::Relaxed);
        }
        self.surface_losses_with_volume
            .fetch_add(1, Ordering::Relaxed)
            + 1
    }

    /// Latch an uncaptured device error as the volume's, unless it fell
    /// inside the probe's window.
    fn latch_device_error(&self, now_ms: u64) {
        if self.probe_window_is_open(now_ms) {
            self.device_errors_in_probe_window
                .fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.volume_device_error.store(true, Ordering::Relaxed);
    }

    /// Open the probe's window at `now_ms`, to close by itself `at_most_ms`
    /// later whatever the probe does. A second opener replaces the first:
    /// one probe runs per page.
    fn open_probe_window(&self, now_ms: u64, at_most_ms: u64) {
        self.probe_window_closes_at_ms
            .store(now_ms.saturating_add(at_most_ms), Ordering::Relaxed);
    }

    /// Bring the window's close forward to `grace_ms` after `now_ms`. Never
    /// later than the bound the opener set, and nothing when no window is open.
    fn close_probe_window_after(&self, now_ms: u64, grace_ms: u64) {
        let closes_at = now_ms.saturating_add(grace_ms);
        // `fetch_min` rather than a store: the opener's bound is the ceiling,
        // and a closed window (0) stays closed.
        self.probe_window_closes_at_ms
            .fetch_min(closes_at, Ordering::Relaxed);
    }

    fn recorded_failure(&self) -> Option<VolumeSupport> {
        verdict(
            self.surface_losses_with_volume.load(Ordering::Relaxed),
            self.volume_device_error.load(Ordering::Relaxed),
        )
    }
}

/// The process-global record.
static LATCH: Latch = Latch::new();

/// The clock the window is measured against, installed by the first opener
/// ([`ProbeWindow::open`]). The opener's rather than this crate's because
/// this crate's charter admits no clock dependency (`std::time::Instant`
/// panics on wasm32, and the window is a web mechanism first), and the one
/// opener there is has a wall clock to hand. Any monotonic millisecond
/// count will do; only differences are read.
static CLOCK: OnceLock<fn() -> u64> = OnceLock::new();

/// Millis on the installed clock, or `0` when no opener has installed one —
/// under which no window has ever been opened, so every window reads closed.
fn now_ms() -> u64 {
    CLOCK.get().map_or(0, |clock| clock())
}

/// Record that the surface was lost while a volume was on screen. Returns
/// the losses counted against the view so far — unchanged when this loss
/// fell inside the capacity probe's window ([`ProbeWindow`]).
pub fn note_surface_loss_with_volume() -> u32 {
    LATCH.note_surface_loss(now_ms())
}

/// Latch an uncaptured device error as the volume's — unless it fell inside
/// the capacity probe's window ([`ProbeWindow`]), where it is recorded and
/// latched nowhere.
pub fn latch_volume_device_error() {
    LATCH.latch_device_error(now_ms())
}

/// What the recorded failures say about the volume view, if anything.
pub fn recorded_failure() -> Option<VolumeSupport> {
    LATCH.recorded_failure()
}

/// Surface losses that landed inside the probe's window and were therefore
/// not counted against the view. A reader of the probe's report uses this to
/// say whether the page's own context went with the probe's.
pub fn losses_in_probe_window() -> u32 {
    LATCH.losses_in_probe_window.load(Ordering::Relaxed)
}

/// Device errors that landed inside the probe's window and were not latched.
pub fn device_errors_in_probe_window() -> u32 {
    LATCH.device_errors_in_probe_window.load(Ordering::Relaxed)
}

/// Whether a loss right now would be the probe's.
pub fn probe_window_is_open() -> bool {
    LATCH.probe_window_is_open(now_ms())
}

/// **The window in which a context loss is the capacity probe's own.** Opened
/// by the probe before its first allocation, and closed — [`PROBE_LOSS_GRACE_MS`]
/// after — when the guard drops, so the probe cannot forget to. Bounded from
/// the other side too: the opener says how long the window may stay open at
/// most, and a probe that never returns disarms the latch for that long and
/// no longer.
#[must_use = "dropping the guard at once closes the window before the first allocation"]
pub struct ProbeWindow {
    latch: &'static Latch,
}

impl ProbeWindow {
    /// Open the window on the process-global record, to close by itself
    /// `at_most_ms` from now on `clock` — a monotonic millisecond count the
    /// opener supplies, installed once for the life of the process; a later
    /// opener's clock is ignored in favour of the first.
    pub fn open(clock: fn() -> u64, at_most_ms: u64) -> Self {
        let _ = CLOCK.set(clock);
        LATCH.open_probe_window(now_ms(), at_most_ms);
        Self { latch: &LATCH }
    }
}

impl Drop for ProbeWindow {
    fn drop(&mut self) {
        self.latch
            .close_probe_window_after(now_ms(), PROBE_LOSS_GRACE_MS);
    }
}

/// The policy, separated from the process-global state that feeds it.
fn verdict(losses: u32, device_error: bool) -> Option<VolumeSupport> {
    if losses >= MAX_SURFACE_LOSSES_WITH_VOLUME {
        return Some(VolumeSupport::Unavailable(format!(
            "The 3D volume view has been disabled: the graphics device was lost \
             {losses} times while a volume was on screen. Restart squallar to try \
             again."
        )));
    }
    if device_error {
        return Some(VolumeSupport::Unavailable(
            "The 3D volume view has been disabled: the graphics driver rejected \
             one of its resources. Restart squallar to try again."
                .to_owned(),
        ));
    }
    None
}

/// Whether an uncaptured device error belongs to the volume view.
pub fn error_belongs_to_volume(rendered: &str) -> bool {
    labels_in(rendered).any(|label| {
        label == VOLUME_LABEL_PREFIX
            || label
                .strip_prefix(VOLUME_LABEL_PREFIX)
                .is_some_and(|rest| rest.starts_with('.'))
    })
}

/// Every resource label wgpu named in an error description.
fn labels_in(rendered: &str) -> impl Iterator<Item = &str> {
    rendered
        .split(LABEL_INTRO)
        .skip(1)
        .filter_map(|rest| rest.split_once('\''))
        .map(|(label, _)| label)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A healthy device gets no override, so the probe's answer stands.
    #[test]
    fn nothing_recorded_leaves_the_probes_answer_alone() {
        assert_eq!(verdict(0, false), None);
    }

    /// One loss is not enough. A monitor unplugged is not a broken GPU.
    #[test]
    fn a_single_surface_loss_does_not_retire_the_view() {
        assert_eq!(verdict(1, false), None);
    }

    /// The second loss is the last one, and it says how many.
    #[test]
    fn the_second_surface_loss_retires_the_view_permanently() {
        for losses in MAX_SURFACE_LOSSES_WITH_VOLUME..=8 {
            let Some(VolumeSupport::Unavailable(why)) = verdict(losses, false) else {
                panic!("{losses} losses did not retire the volume view");
            };
            assert!(
                why.contains("device was lost"),
                "the reason must name the cause, not just refuse: {why:?}"
            );
            assert!(
                why.contains(&losses.to_string()),
                "the reason must say how many losses it took: {why:?}"
            );
        }
    }

    /// A latched device error retires the view on its own, with its own reason.
    #[test]
    fn a_latched_device_error_retires_the_view_with_its_own_reason() {
        let Some(VolumeSupport::Unavailable(why)) = verdict(0, true) else {
            panic!("a latched device error did not retire the volume view");
        };
        assert!(why.contains("driver rejected"), "{why:?}");
        assert!(
            !why.contains("lost"),
            "a driver rejection must not be reported as a lost device: {why:?}"
        );
    }

    /// Every reason is a sentence a user can act on.
    #[test]
    fn every_unavailable_reason_is_user_readable() {
        for state in [
            verdict(MAX_SURFACE_LOSSES_WITH_VOLUME, false),
            verdict(0, true),
        ] {
            let why = state
                .expect("expected an Unavailable")
                .reason()
                .unwrap()
                .to_owned();
            assert!(why.ends_with('.'), "not a sentence: {why:?}");
            assert!(
                why.contains("3D volume view"),
                "the reason does not say what is unavailable: {why:?}"
            );
            assert!(
                !why.contains("wgpu") && !why.contains("Unavailable"),
                "the reason leaks implementation vocabulary at the user: {why:?}"
            );
        }
    }

    /// A validation error carrying a volume label is the volume's.
    #[test]
    fn an_error_labelled_for_the_volume_is_recognised() {
        for label in [
            "squallar.volume",
            "squallar.volume.raymarch",
            "squallar.volume.grid.texture",
        ] {
            let rendered = format!(
                "Validation Error\n\nCaused by:\n  In Device::create_texture, label = '{label}'\n"
            );
            assert!(
                error_belongs_to_volume(&rendered),
                "a device error labelled {label:?} was not attributed to the volume"
            );
        }
    }

    /// Everything else is not, which is the half that matters.
    #[test]
    fn an_error_from_anywhere_else_is_not_the_volumes() {
        for rendered in [
            "Validation Error\n\nCaused by:\n  In Device::create_texture, label = 'egui sampler'\n",
            "Validation Error\n\nCaused by:\n  In Queue::write_buffer\n",
            "Out of Memory",
            // Names squallar, but is not one of the volume view's resources.
            "In Device::create_buffer, label = 'squallar.loop.frame'",
            // The near miss the substring version got wrong.
            "In Device::create_texture, label = 'squallar.volumetric.eet'",
            "In Device::create_texture, label = 'squallar.volumes'",
            // The prefix as free text rather than as a label.
            "Shader compilation failed for the squallar.volume raymarch",
            // A truncated message: the label never closes, so there is no label.
            "In Device::create_texture, label = 'squallar.volume",
        ] {
            assert!(
                !error_belongs_to_volume(rendered),
                "an unrelated device error was attributed to the volume, which \
                 would swallow it: {rendered:?}"
            );
        }
    }

    /// A volume label anywhere in the source chain counts, not only the first.
    #[test]
    fn a_volume_label_later_in_the_chain_still_counts() {
        let rendered = "Validation Error\n\nCaused by:\n  \
             In CommandEncoder::begin_render_pass, label = 'egui main render pass'\n  \
             In a bind group, label = 'squallar.volume.grid'\n";
        assert!(error_belongs_to_volume(rendered));
    }

    /// The process-global counter survives being read and keeps counting.
    /// No other test in this binary may open a window on [`LATCH`]: the
    /// window tests below drive fresh instances, so this one's losses are
    /// never exempted from under it — and with no opener, no clock is
    /// installed and the window reads closed.
    #[test]
    fn the_global_loss_counter_survives_and_retires_the_view() {
        assert_eq!(
            LATCH.surface_losses_with_volume.load(Ordering::Relaxed),
            0,
            "something else in this binary has already touched the loss counter, \
             so this test is racing it"
        );
        assert!(
            CLOCK.get().is_none(),
            "something in this binary opened a probe window"
        );
        assert!(!probe_window_is_open());

        assert_eq!(note_surface_loss_with_volume(), 1);
        assert_eq!(
            recorded_failure(),
            None,
            "one loss retired the view; two is the threshold"
        );

        assert_eq!(note_surface_loss_with_volume(), 2);
        assert!(
            recorded_failure().is_some_and(|v| !v.is_supported()),
            "the second loss did not retire the view through the global counter"
        );

        // Monotone: a third loss cannot walk it back.
        assert_eq!(note_surface_loss_with_volume(), 3);
        assert!(recorded_failure().is_some_and(|v| !v.is_supported()));
    }

    /// Losses inside the probe's window are the probe's: recorded in their
    /// own field, counted against the view not at all, and the view stays
    /// supported however many there are.
    #[test]
    fn a_loss_inside_the_probes_window_does_not_count_against_the_view() {
        let latch = Latch::new();
        latch.open_probe_window(1_000, 5_000);
        for now in [1_000, 2_500, 5_999] {
            assert_eq!(
                latch.note_surface_loss(now),
                0,
                "a loss at {now} inside the window was counted against the view"
            );
        }
        assert_eq!(
            latch.recorded_failure(),
            None,
            "three in-window losses retired the view"
        );
        assert_eq!(latch.losses_in_probe_window.load(Ordering::Relaxed), 3);
        assert_eq!(latch.surface_losses_with_volume.load(Ordering::Relaxed), 0);
    }

    /// The other arm: a loss outside the window — before it opens, or once
    /// its bound has passed — counts exactly as it always did, and two of
    /// them retire the view.
    #[test]
    fn a_loss_outside_the_probes_window_still_counts_and_retires_the_view() {
        let latch = Latch::new();
        assert_eq!(latch.note_surface_loss(500), 1, "no window yet: counted");
        latch.open_probe_window(1_000, 5_000);
        assert_eq!(latch.note_surface_loss(3_000), 1, "inside: not counted");
        assert_eq!(latch.recorded_failure(), None);
        // The bound is exclusive: at exactly the close the window is shut.
        assert_eq!(latch.note_surface_loss(6_000), 2, "at the bound: counted");
        assert!(
            latch.recorded_failure().is_some_and(|v| !v.is_supported()),
            "two out-of-window losses did not retire the view"
        );
        assert_eq!(latch.losses_in_probe_window.load(Ordering::Relaxed), 1);
    }

    /// A device error takes the same window: inside, recorded and not
    /// latched; outside, latched as before.
    #[test]
    fn a_device_error_takes_the_same_window_as_a_loss() {
        let latch = Latch::new();
        latch.open_probe_window(0, 4_000);
        latch.latch_device_error(100);
        assert_eq!(
            latch.recorded_failure(),
            None,
            "an in-window device error latched"
        );
        assert_eq!(
            latch.device_errors_in_probe_window.load(Ordering::Relaxed),
            1
        );

        latch.latch_device_error(4_000);
        assert!(
            latch
                .recorded_failure()
                .is_some_and(|v| v.reason().unwrap().contains("driver rejected")),
            "an out-of-window device error did not latch"
        );
    }

    /// Closing brings the close forward by the grace and never pushes it
    /// back: a probe that finishes early shortens the window, and a grace
    /// longer than the opener's bound changes nothing.
    #[test]
    fn closing_shortens_the_window_by_the_grace_and_never_extends_it() {
        let latch = Latch::new();
        latch.open_probe_window(0, 10_000);
        latch.close_probe_window_after(1_000, 2_000);
        assert!(
            latch.probe_window_is_open(2_999),
            "the grace still covers 2999"
        );
        assert!(
            !latch.probe_window_is_open(3_000),
            "the grace is spent at 3000"
        );
        assert_eq!(
            latch.note_surface_loss(3_000),
            1,
            "after the grace: counted"
        );

        let latch = Latch::new();
        latch.open_probe_window(0, 1_000);
        latch.close_probe_window_after(500, 20_000);
        assert!(
            !latch.probe_window_is_open(1_000),
            "a close with a long grace extended the window past the opener's bound"
        );
    }

    /// A window nobody opened is closed, and closing it keeps it closed.
    #[test]
    fn an_unopened_window_is_closed_and_a_close_does_not_open_it() {
        let latch = Latch::new();
        assert!(!latch.probe_window_is_open(0));
        latch.close_probe_window_after(0, 5_000);
        assert!(!latch.probe_window_is_open(1));
        assert_eq!(latch.note_surface_loss(1), 1);
    }

    /// The bound the opener names is the most the window can stay open: a
    /// probe that never returns disarms the latch for that long and no more.
    #[test]
    fn the_openers_bound_closes_the_window_without_a_close() {
        let latch = Latch::new();
        latch.open_probe_window(7_000, 6_000);
        assert!(latch.probe_window_is_open(12_999));
        assert!(!latch.probe_window_is_open(13_000));
    }
}
