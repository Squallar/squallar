//! What to do after the volume view has already gone wrong once.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

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

/// The label prefix every wgpu resource the volume view creates must carry.
pub const VOLUME_LABEL_PREFIX: &str = "squallar.volume";

/// How wgpu introduces a resource label inside an error description.
const LABEL_INTRO: &str = "label = '";

/// Surface losses observed while a volume was on screen.
static SURFACE_LOSSES_WITH_VOLUME: AtomicU32 = AtomicU32::new(0);

/// Whether an uncaptured device error has ever been attributed to the volume.
static VOLUME_DEVICE_ERROR: AtomicBool = AtomicBool::new(false);

/// Record that the surface was lost while a volume was on screen.
pub fn note_surface_loss_with_volume() -> u32 {
    SURFACE_LOSSES_WITH_VOLUME.fetch_add(1, Ordering::Relaxed) + 1
}

/// Latch an uncaptured device error as the volume's.
pub fn latch_volume_device_error() {
    VOLUME_DEVICE_ERROR.store(true, Ordering::Relaxed);
}

/// What the recorded failures say about the volume view, if anything.
pub fn recorded_failure() -> Option<VolumeSupport> {
    verdict(
        SURFACE_LOSSES_WITH_VOLUME.load(Ordering::Relaxed),
        VOLUME_DEVICE_ERROR.load(Ordering::Relaxed),
    )
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
