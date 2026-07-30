//! What to do after the volume view has already gone wrong once.
//!
//! Three of the failure modes in a 3D volume view cannot be prevented, only
//! survived, and all three are invisible to the caller at the point they happen:
//!
//! * `create_render_pipeline` and `create_texture` do not return `Result`. A
//!   shader that will not build, or a texture the driver refuses, arrives as an
//!   *uncaptured device error* — asynchronously, from wgpu's own error sink.
//! * `pop_error_scope()` is a `Future`, and a browser cannot block on a future,
//!   so error scopes cannot turn any of it back into an inline `Result`.
//! * A WebGL2 context loss is reported as a lost surface, and rustdar's response
//!   is to drop the whole [`crate::app_state::AppState`] and rebuild it.
//!
//! That last one is why **every counter here is a module-level `static` rather
//! than a field on `AppState`**. `app_render::present_frame`'s
//! `SurfaceStatus::Lost` arm sets `self.state = None`, so anything stored inside
//! `AppState` is destroyed by precisely the event it is supposed to be counting.
//! A two-strike counter kept there would read 0 on every attempt, the volume
//! would be rebuilt, the context would be lost again, and the app would crash in
//! a loop that never terminates — on the web, where a crash takes the tab with
//! it. A `static` outlives the rebuild, which is the whole mechanism.
//!
//! Deliberately `AtomicU32`/`AtomicBool` rather than the `OnceLock` the design
//! sketch suggested: the initial state is a compile-time constant, so there is
//! nothing to initialise lazily, and an atomic needs no lock to be read from the
//! frame path. Both are process-global and **never reset** — see
//! [`note_surface_loss_with_volume`].

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Whether the 3D volume view can be used, and if not, why not.
///
/// A reason string rather than a `bool` because every path that produces one
/// knows something specific and useful, and the UI has to be able to say it.
/// "3D volume view unavailable" with no cause is the outcome this type exists to
/// avoid.
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
///
/// Two, not one: a single context loss has causes that have nothing to do with
/// the volume — a monitor unplugged, a laptop lid, a browser tab backgrounded
/// long enough for the compositor to reclaim the context — and retiring the
/// feature on the first would take 3D away from users whose GPUs are fine.
///
/// Two consecutive losses *while a volume was on screen* is a different claim,
/// and it is the concrete answer to "the volume view must not hard-crash a
/// browser": **the second crash is the last one.** There is no third.
pub const MAX_SURFACE_LOSSES_WITH_VOLUME: u32 = 2;

/// The label prefix every wgpu resource the volume view creates must carry.
///
/// This is the only handle there is for telling a volume error from an unrelated
/// one. `wgpu::Error`'s payload is a `String` description built from the whole
/// source chain, and `wgpu_core::error::ContextError`'s `Display` renders as
/// `In {fn_ident}, label = '{label}'` (wgpu-core-29.0.4 `src/error.rs:11-23`), so
/// the label is present in the description and nowhere else machine-readable.
///
/// Dotted so submodules can extend it — `rustdar.volume.raymarch`,
/// `rustdar.volume.grid`. See [`error_belongs_to_volume`] for why matching it is
/// *not* a substring search on the whole message.
pub const VOLUME_LABEL_PREFIX: &str = "rustdar.volume";

/// How wgpu introduces a resource label inside an error description.
///
/// From `wgpu_core::error::ContextError`'s `#[error(...)]` attribute, which
/// renders `In {fn_ident}, label = '{label}'` (wgpu-core-29.0.4
/// `src/error.rs:12-17`).
const LABEL_INTRO: &str = "label = '";

/// Surface losses observed while a volume was on screen.
///
/// Module-level, and that is the load-bearing property: see the module docs.
static SURFACE_LOSSES_WITH_VOLUME: AtomicU32 = AtomicU32::new(0);

/// Whether an uncaptured device error has ever been attributed to the volume.
///
/// Also module-level, and for a second reason on top of surviving the rebuild:
/// `Device::on_uncaptured_error` takes an `Arc<dyn Fn(Error) + Send + Sync +
/// 'static>`, so the handler cannot borrow `AppState` even if we wanted it to.
static VOLUME_DEVICE_ERROR: AtomicBool = AtomicBool::new(false);

/// Record that the surface was lost while a volume was on screen.
///
/// Returns the running total. Called from `present_frame`'s
/// `SurfaceStatus::Lost` arm, and **only** when a volume was actually being
/// rendered — a loss with no volume on screen says nothing about the volume and
/// must not count against it.
///
/// There is no way to reset this, by design. A user who has crashed the GPU
/// twice gets a working radar viewer without 3D, not a fourth attempt.
pub fn note_surface_loss_with_volume() -> u32 {
    SURFACE_LOSSES_WITH_VOLUME.fetch_add(1, Ordering::Relaxed) + 1
}

/// Latch an uncaptured device error as the volume's.
///
/// One-way, for the same reason as the loss counter: the shader that failed to
/// build will fail to build again on the next device.
pub fn latch_volume_device_error() {
    VOLUME_DEVICE_ERROR.store(true, Ordering::Relaxed);
}

/// What the recorded failures say about the volume view, if anything.
///
/// `None` means nothing has gone wrong yet and the capability probe's answer
/// stands. `Some(Unavailable)` overrides it — a device that has already failed is
/// not made capable by passing a limits check.
pub fn recorded_failure() -> Option<VolumeSupport> {
    verdict(
        SURFACE_LOSSES_WITH_VOLUME.load(Ordering::Relaxed),
        VOLUME_DEVICE_ERROR.load(Ordering::Relaxed),
    )
}

/// The policy, separated from the process-global state that feeds it.
///
/// Split out so it can be tested exhaustively without touching the statics —
/// which `cargo test` runs in parallel threads and could not share hermetically.
fn verdict(losses: u32, device_error: bool) -> Option<VolumeSupport> {
    if losses >= MAX_SURFACE_LOSSES_WITH_VOLUME {
        return Some(VolumeSupport::Unavailable(format!(
            "The 3D volume view has been disabled: the graphics device was lost \
             {losses} times while a volume was on screen. Restart rustdar to try \
             again."
        )));
    }
    if device_error {
        return Some(VolumeSupport::Unavailable(
            "The 3D volume view has been disabled: the graphics driver rejected \
             one of its resources. Restart rustdar to try again."
                .to_owned(),
        ));
    }
    None
}

/// Whether an uncaptured device error belongs to the volume view.
///
/// Takes the *rendered* error rather than a `wgpu::Error` so the classification
/// can be tested against the exact strings wgpu produces without a device to
/// produce them.
///
/// # Why this is not `rendered.contains(VOLUME_LABEL_PREFIX)`
///
/// Because that is wrong, and it was written that way first. A prefix is a
/// property of the *label*, not of the whole message, and the two differ on
/// strings this repo already contains: `rustdar.volumetric.eet` starts with
/// `rustdar.volume` character for character. A substring search claims every
/// error from the echo-tops path as the volume's, silently swallowing errors
/// that the debug build is supposed to re-raise. So the label is extracted from
/// wgpu's own `label = '…'` framing and matched as a path segment: exactly
/// [`VOLUME_LABEL_PREFIX`], or that followed by a `.`.
///
/// Every label is checked, not only the first: an error's description is the
/// whole formatted source chain and may name more than one resource.
pub fn error_belongs_to_volume(rendered: &str) -> bool {
    labels_in(rendered).any(|label| {
        label == VOLUME_LABEL_PREFIX
            || label
                .strip_prefix(VOLUME_LABEL_PREFIX)
                .is_some_and(|rest| rest.starts_with('.'))
    })
}

/// Every resource label wgpu named in an error description.
///
/// An unterminated label — a truncated message, or a label containing a quote —
/// yields nothing rather than the rest of the string, so a mangled message
/// cannot be read as a volume label by accident.
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
    ///
    /// Separately from the loss counter: a pipeline the driver refused and a
    /// context that keeps dying are different problems and a user told the wrong
    /// one will chase the wrong fix.
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
    ///
    /// The inputs are built the way wgpu builds them — `wgpu_core`'s
    /// `ContextError` renders `In {fn_ident}, label = '{label}'` and wgpu's
    /// `Error::Validation` description is the whole formatted source chain
    /// (wgpu-29.0.4 `src/backend/wgpu_core.rs:280`).
    #[test]
    fn an_error_labelled_for_the_volume_is_recognised() {
        for label in [
            "rustdar.volume",
            "rustdar.volume.raymarch",
            "rustdar.volume.grid.texture",
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
    ///
    /// Installing an uncaptured-error handler replaces wgpu's default, and that
    /// default panics. A classifier that said "mine" too readily would convert
    /// every unrelated validation error in the application into a silently
    /// swallowed one.
    ///
    /// `rustdar.volumetric.eet` is the case that matters and it is not
    /// hypothetical: this function was first written as
    /// `rendered.contains(VOLUME_LABEL_PREFIX)`, and `rustdar.volumetric` starts
    /// with `rustdar.volume` character for character. A prefix is a property of
    /// the label, not of the message.
    #[test]
    fn an_error_from_anywhere_else_is_not_the_volumes() {
        for rendered in [
            "Validation Error\n\nCaused by:\n  In Device::create_texture, label = 'egui sampler'\n",
            "Validation Error\n\nCaused by:\n  In Queue::write_buffer\n",
            "Out of Memory",
            // Names rustdar, but is not one of the volume view's resources.
            "In Device::create_buffer, label = 'rustdar.loop.frame'",
            // The near miss the substring version got wrong.
            "In Device::create_texture, label = 'rustdar.volumetric.eet'",
            "In Device::create_texture, label = 'rustdar.volumes'",
            // The prefix as free text rather than as a label.
            "Shader compilation failed for the rustdar.volume raymarch",
            // A truncated message: the label never closes, so there is no label.
            "In Device::create_texture, label = 'rustdar.volume",
        ] {
            assert!(
                !error_belongs_to_volume(rendered),
                "an unrelated device error was attributed to the volume, which \
                 would swallow it: {rendered:?}"
            );
        }
    }

    /// A volume label anywhere in the source chain counts, not only the first.
    ///
    /// An `Error::Validation` description is the whole formatted chain, so the
    /// resource that failed is not necessarily the one named first.
    #[test]
    fn a_volume_label_later_in_the_chain_still_counts() {
        let rendered = "Validation Error\n\nCaused by:\n  \
             In CommandEncoder::begin_render_pass, label = 'egui main render pass'\n  \
             In a bind group, label = 'rustdar.volume.grid'\n";
        assert!(error_belongs_to_volume(rendered));
    }

    /// The process-global counter survives being read and keeps counting.
    ///
    /// **This is the only test that touches `SURFACE_LOSSES_WITH_VOLUME`.** A
    /// second one would race it, because `cargo test` runs tests in parallel
    /// threads of one process and the counter is deliberately process-global and
    /// never reset. The policy itself is covered by the hermetic `verdict` tests
    /// above; this pins only the wiring — that the global is monotone and that
    /// `recorded_failure` reads it.
    #[test]
    fn the_global_loss_counter_survives_and_retires_the_view() {
        assert_eq!(
            SURFACE_LOSSES_WITH_VOLUME.load(Ordering::Relaxed),
            0,
            "something else in this binary has already touched the loss counter, \
             so this test is racing it"
        );

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
}
