//! The archive phase of an OpenMapTiles-schema vector-tile build: read sorted
//! features, group them by tile, simplify and clip per zoom, encode MVT, gzip,
//! and write a PMTiles archive.
//!
//! **None of that exists yet.** This binary is the skeleton that carries the
//! crate's placement -- its own workspace, outside the app's lockfile -- and
//! its CI gate. Every invocation prints usage and exits non-zero.
//!
//! The layout is deliberately one file. The stages the phase will grow (a
//! reader for the sorted feature input, geometry in tile-pixel space, an MVT
//! encoder, gzip, a PMTiles writer) are named here and nowhere else: an empty
//! module per stage would make the file list claim work that has not been
//! done. Each becomes a module when it has something in it.
//!
//! Nothing in the app may depend on this crate. The basemap migration ships on
//! planetiler's output and swaps this pipeline underneath later, so this crate
//! being absent, unbuilt or broken must never be able to hold the app up. Its
//! manifest explains why it is not a workspace member.
//!
//! Usage:
//!   squallar-tiler <sorted-features> <output.pmtiles>

use std::process::ExitCode;

/// Everything the tool has to say for itself, in one place so that the text and
/// the test that pins it cannot drift apart.
///
/// It opens with the package name rather than a hand-typed string, because
/// this project has been renamed once already (`rustdar` to `squallar`, 2026-
/// 08-23) and a usage line naming a binary that no longer exists is worse than
/// no usage line at all.
const USAGE: &str = concat!(
    env!("CARGO_PKG_NAME"),
    " -- the archive phase of a vector-tile build\n",
    "\n",
    "Usage:\n",
    "  ",
    env!("CARGO_PKG_NAME"),
    " <sorted-features> <output.pmtiles>\n",
    "\n",
    "Not implemented. This is the crate skeleton; the archive phase lands\n",
    "behind it. See the module comment in src/main.rs for what it will hold.\n",
);

/// Prints usage and fails, whatever it was asked for.
///
/// Failing is the honest answer while nothing is implemented: a stub that
/// exited 0 would let a caller -- a script, a future CI step -- believe an
/// archive had been written. 2 rather than 1 is the usual "you invoked this
/// wrongly" code, and every invocation of this build is a wrong one.
fn main() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::USAGE;

    /// The usage text names the binary the manifest actually builds.
    ///
    /// Not a tautology despite both names deriving from the manifest: the
    /// package name and the binary name are separate facts, and this crate
    /// relies on Cargo deriving the second from the first because it declares
    /// no `[[bin]]`. Declaring one with a different `name` -- the ordinary way
    /// a binary stops being called after its package -- reds this, and leaves
    /// the usage text still printing the old name.
    ///
    /// Renaming the package alone does not red it, and should not: both env
    /// vars move together, and the usage text moves with them. That is the
    /// property, not a hole in it.
    #[test]
    fn the_usage_text_names_the_binary_that_is_built() {
        let binary = env!("CARGO_BIN_NAME");
        assert_eq!(
            binary,
            env!("CARGO_PKG_NAME"),
            "the binary and the package have diverged; USAGE names the package"
        );
        assert!(
            USAGE.starts_with(binary),
            "usage text does not open with {binary}: {USAGE}"
        );
    }

    /// Usage names both operands the archive phase will take.
    ///
    /// The stub takes no arguments, so this pins the interface the phase is
    /// being built toward rather than anything it does today. It is here to
    /// red when the two operands stop being what this crate is for.
    #[test]
    fn the_usage_text_names_an_input_and_an_output() {
        assert!(USAGE.contains("<sorted-features>"), "{USAGE}");
        assert!(USAGE.contains("<output.pmtiles>"), "{USAGE}");
    }
}
