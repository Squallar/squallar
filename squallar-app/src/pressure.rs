//! Why the application is being asked to give memory back, and how it says
//! what it gave.
//!
//! Pressure is answered within the session. Economy — what is resident beyond
//! what the scene on screen needs — is evicted first; then the budget ladder
//! steps down one rung, held in the device profile's memo for the life of the
//! process; and nothing about either is written to the store. A reopen starts
//! at the ladder top whatever this session learned: capacity is measured,
//! probed or presumed at startup, never remembered.

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
    // The wasm linear-memory watermark is a fourth cause, carrying the used and
    // maximum heap bytes; it is not delivered by this module yet.
}

impl Pressure {
    /// The cause as the pressure line spells it: lower-case ASCII words.
    pub fn label(self) -> &'static str {
        match self {
            Self::SurfaceLost => "surface lost",
            Self::OutOfMemory => "out of memory",
            Self::MemoryWarning => "memory warning",
        }
    }
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
        cause.label(),
        reclaimed.render_entries,
        reclaimed.render_bytes / (1024 * 1024),
        reclaimed.extracts,
        rung,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
