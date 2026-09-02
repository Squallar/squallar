//! Why the application is being asked to give memory back, and how it says
//! what it gave.
//!
//! Pressure is answered within the session. Economy — what is resident beyond
//! what the scene on screen needs — is evicted first; then the session's
//! capacity presumption comes down and the scene is re-fitted to it, which
//! sheds a rung of the budget ladder only when need alone no longer fits; and
//! nothing about either is written to the store. A reopen fits the same scene
//! to the same budgets whatever this session learned: capacity is measured,
//! probed or presumed at startup, never remembered.
//!
//! Four causes reach [`Pressure`]: a lost surface, a wgpu allocation failure,
//! a platform memory warning, and the wasm heap watermark — the one cause the
//! application raises on itself, from a reading rather than from a failure,
//! so that a browser session sheds before it traps. [`LinearMemoryWatch`] is
//! that reading's session state.

use crate::platform::GpuProbeReport;
use squallar_device_profile::linear_memory::{LinearMemoryVerdict, linear_memory_verdict};

/// What raised the pressure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pressure {
    /// The surface was lost: the device refused, or went away.
    SurfaceLost,
    /// wgpu reported an allocation failure on some resource, whichever it was.
    OutOfMemory,
    /// The platform warned that memory is low — Android's `onLowMemory`, iOS's
    /// `didReceiveMemoryWarning`.
    MemoryWarning,
    /// The fuller of the two wasm linear memories reached the action line of
    /// its ceiling; both figures in bytes.
    LinearMemory { used: u64, max: u64 },
}

impl Pressure {
    /// The cause as the pressure line spells it: lower-case ASCII words.
    pub fn label(self) -> &'static str {
        match self {
            Self::SurfaceLost => "surface lost",
            Self::OutOfMemory => "out of memory",
            Self::MemoryWarning => "memory warning",
            Self::LinearMemory { .. } => "linear memory",
        }
    }

    /// The label, followed by the figures a cause carries, in MiB.
    fn describe(self) -> String {
        match self {
            Self::SurfaceLost | Self::OutOfMemory | Self::MemoryWarning => self.label().to_string(),
            Self::LinearMemory { used, max } => {
                format!("{} {} of {} MiB", self.label(), mib(used), mib(max))
            }
        }
    }
}

/// **Whether an out-of-memory event is the WebGPU probe's own doing.** While
/// the probe's report is `Pending` it is holding its doubling textures — up
/// to 8 GiB for about two seconds — and an allocation the browser refuses the
/// application's device in that window is the probe's, not a wall of this
/// session's: the textures are destroyed the moment it reports. Lowering the
/// presumption on such an event would hold the probed figure down for the
/// whole session, to nine tenths of the *presumption* the app still stood on.
/// So the economy is evicted as for any event, and the presumption is held.
/// Only out-of-memory is attributed: a lost surface or a memory warning in the
/// same window is the application's own.
pub fn is_the_gpu_probes_own(cause: Pressure, probe: GpuProbeReport) -> bool {
    cause == Pressure::OutOfMemory && probe == GpuProbeReport::Pending
}

/// What one pressure event took out of the caches, counted for the line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reclaimed {
    /// Shared render outputs dropped from the dispatcher's render cache.
    pub render_entries: usize,
    /// What those entries occupied.
    pub render_bytes: usize,
    /// Plan-view extraction payloads dropped.
    pub extracts: usize,
}

/// The one line a pressure event logs: integers only, ASCII only.
pub fn pressure_line(cause: Pressure, reclaimed: Reclaimed, rung: u32) -> String {
    format!(
        "budget pressure: {} -> evicted render cache {} entries {} MiB, extracts {}, \
         ladder rung {}",
        cause.describe(),
        reclaimed.render_entries,
        reclaimed.render_bytes / (1024 * 1024),
        reclaimed.extracts,
        rung,
    )
}

/// The one line the heap watermark logs when a reading first crosses the
/// warning line: integers only, ASCII only, the percentage by integer
/// division.
pub fn linear_memory_line(used: u64, max: u64) -> String {
    let percent = u128::from(used) * 100 / u128::from(max.max(1));
    format!(
        "linear memory: {} of {} MiB ({percent}%)",
        mib(used),
        mib(max)
    )
}

fn mib(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

/// The wasm heap watermark's state for this session: the high-water mark it
/// last acted at, and whether the warning has been said for the crossing in
/// progress. Held on the application at its default from construction, and
/// never written anywhere — a reopen has a fresh heap and starts over.
///
/// The verdict itself is `squallar_device_profile`'s; this adds the two
/// pieces of memory the verdict is stateless about. The warning is said once
/// per crossing: it is armed again only by a reading that falls back under
/// the warning line, which a wasm heap never does, so in practice it is said
/// once. An action stands in for the crossing's warning — the `budget
/// pressure:` line carries the same figures — so no warning follows one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinearMemoryWatch {
    last_acted_at: Option<u64>,
    warned: bool,
}

impl LinearMemoryWatch {
    /// Judge one reading and remember what was done about it.
    pub fn observe(&mut self, used: u64, max: u64) -> LinearMemoryVerdict {
        match linear_memory_verdict(used, max, self.last_acted_at) {
            LinearMemoryVerdict::Act => {
                self.last_acted_at = Some(used);
                self.warned = true;
                LinearMemoryVerdict::Act
            }
            LinearMemoryVerdict::Warn if self.warned => LinearMemoryVerdict::Quiet,
            LinearMemoryVerdict::Warn => {
                self.warned = true;
                LinearMemoryVerdict::Warn
            }
            LinearMemoryVerdict::Quiet => {
                self.warned = false;
                LinearMemoryVerdict::Quiet
            }
        }
    }

    /// The mark the watermark last acted at, or `None` if it never has.
    pub fn last_acted_at(self) -> Option<u64> {
        self.last_acted_at
    }

    /// Whether the warning has been said for the crossing in progress.
    pub fn has_warned(self) -> bool {
        self.warned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use LinearMemoryVerdict::{Act, Quiet, Warn};

    /// Out-of-memory while the probe is pending is the probe's; every other
    /// pairing of cause and probe state is the application's own.
    #[test]
    fn only_an_oom_while_the_probe_is_pending_is_the_probes_own() {
        let found = GpuProbeReport::Found(crate::platform::ProbedCapacity {
            bytes: 4032 << 20,
            failed_at: Some(8128 << 20),
            steps: 7,
            elapsed_ms: 812,
            capped: false,
        });
        assert!(is_the_gpu_probes_own(
            Pressure::OutOfMemory,
            GpuProbeReport::Pending
        ));
        for probe in [
            GpuProbeReport::Absent,
            GpuProbeReport::Skipped,
            GpuProbeReport::Empty,
            found,
        ] {
            assert!(
                !is_the_gpu_probes_own(Pressure::OutOfMemory, probe),
                "{probe:?}: an OOM outside the probe window is the session's own"
            );
        }
        for cause in [
            Pressure::SurfaceLost,
            Pressure::MemoryWarning,
            Pressure::LinearMemory {
                used: 900 * MIB,
                max: GIB,
            },
        ] {
            assert!(
                !is_the_gpu_probes_own(cause, GpuProbeReport::Pending),
                "{cause:?} in the probe window is still the application's own"
            );
        }
    }

    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;

    /// The line names its cause, its counts and its rung, in ASCII, with
    /// integers only — the shape a log scraper can key on.
    #[test]
    fn the_pressure_line_is_ascii_integers_and_names_its_cause() {
        let line = pressure_line(
            Pressure::OutOfMemory,
            Reclaimed {
                render_entries: 3,
                render_bytes: 48 * 1024 * 1024,
                extracts: 2,
            },
            1,
        );
        assert_eq!(
            line,
            "budget pressure: out of memory -> evicted render cache 3 entries 48 MiB, \
             extracts 2, ladder rung 1"
        );
        for cause in [
            Pressure::SurfaceLost,
            Pressure::OutOfMemory,
            Pressure::MemoryWarning,
            Pressure::LinearMemory {
                used: 891 * MIB,
                max: GIB,
            },
        ] {
            let line = pressure_line(cause, Reclaimed::default(), 0);
            assert!(line.starts_with("budget pressure: "), "{line}");
            assert!(line.contains(cause.label()), "{line}");
            assert!(line.is_ascii(), "{line}");
            assert!(
                !line.contains('.'),
                "a fraction crept into the line: {line}"
            );
        }
    }

    /// The heap cause carries its two figures, in MiB, on the pressure line,
    /// and the warning line carries them with the percentage.
    #[test]
    fn the_heap_cause_carries_used_and_ceiling_in_mib() {
        let line = pressure_line(
            Pressure::LinearMemory {
                used: 891 * MIB,
                max: GIB,
            },
            Reclaimed::default(),
            1,
        );
        assert_eq!(
            line,
            "budget pressure: linear memory 891 of 1024 MiB -> evicted render cache \
             0 entries 0 MiB, extracts 0, ladder rung 1"
        );
        assert_eq!(
            linear_memory_line(800 * MIB, GIB),
            "linear memory: 800 of 1024 MiB (78%)"
        );
        assert!(linear_memory_line(800 * MIB, 0).is_ascii());
    }

    /// The warning is said for the first reading past the line and for no
    /// later reading of the same crossing, however far it climbs short of an
    /// action; a reading back under the line arms it again.
    #[test]
    fn the_warn_line_is_said_once_per_crossing() {
        let mut watch = LinearMemoryWatch::default();
        assert_eq!(watch.observe(700 * MIB, GIB), Quiet);
        assert!(!watch.has_warned());
        assert_eq!(watch.observe(768 * MIB, GIB), Warn);
        assert!(watch.has_warned());
        assert_eq!(
            watch.observe(768 * MIB, GIB),
            Quiet,
            "said twice at one reading"
        );
        assert_eq!(
            watch.observe(850 * MIB, GIB),
            Quiet,
            "said twice on one crossing"
        );
        assert_eq!(watch.observe(700 * MIB, GIB), Quiet);
        assert!(
            !watch.has_warned(),
            "a reading back under the line did not re-arm it"
        );
        assert_eq!(watch.observe(768 * MIB, GIB), Warn);
        assert_eq!(
            watch.last_acted_at(),
            None,
            "a warning was counted as an action"
        );
    }

    /// An action records its mark, stands in for the crossing's warning, and
    /// is not repeated until the mark has grown by the refire step.
    #[test]
    fn an_action_records_its_mark_and_silences_the_warning() {
        let mut watch = LinearMemoryWatch::default();
        assert_eq!(watch.observe(891 * MIB, GIB), Act);
        assert_eq!(watch.last_acted_at(), Some(891 * MIB));
        assert!(watch.has_warned());
        assert_eq!(
            watch.observe(891 * MIB, GIB),
            Quiet,
            "acted twice at one reading"
        );
        assert_eq!(watch.observe(891 * MIB + 31 * MIB, GIB), Quiet);
        assert_eq!(watch.observe(891 * MIB + 32 * MIB, GIB), Act);
        assert_eq!(watch.last_acted_at(), Some(891 * MIB + 32 * MIB));
    }
}
