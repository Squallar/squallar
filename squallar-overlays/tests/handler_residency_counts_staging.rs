//! **A handler that parks a grid-sized buffer must price it**, checked over
//! the source rather than remembered by the next author.
//!
//! `resident_source_bytes` is a free-form sum: a handler adds up the things it
//! holds, and nothing about the type says what those things are. That has now
//! cost the same term twice. The staging pool landed for MRMS with its byte
//! counted; GMGSI copied the pool, the two caches and the recycle doors and did
//! not copy the one line that adds `retained_bytes()`, so the `overlay grids`
//! census under-reported GMGSI by a whole 15,000,000 B mosaic for as long as
//! its slot was parked — which, on a layer that decodes one granule a frame, is
//! most of the time.
//!
//! **Why a gate and not a shape.** The compiler cannot know what a handler
//! holds, so no trait method, default, or wrapper makes the omission
//! impossible: every candidate — a `staging_pool()` accessor with a `None`
//! default, a sum split into two overridable halves, a cache that prices its
//! own pool — relocates the thing to be forgotten rather than removing it, and
//! the last one double-counts, because a handler's two stores share one slot.
//! Folding the pools into the registry instead would make the figure
//! process-global and so untestable: every suite here injects a pool of its own
//! precisely because the shipped slot is shared. What is left is a check, and
//! the population is two, so the check is small.
//!
//! **Scope.** The residency contract lives on handlers, so the obligation is
//! checked there; the second walk below closes the escape by requiring every
//! retained pool in this crate to be held by a handler in the first place.
//! `squallar-radar`'s `RadarSource` holds no pool — its volumes live in a cache
//! above this crate — and neither does any other registered layer.
//!
//! **Needle hygiene**: this file is not inside either walked tree, so its own
//! mention of the pattern is not in the haystack. Both walks fail loudly on an
//! empty haystack rather than counting zero and going green.

use std::path::{Path, PathBuf};

const CRATE_SRC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
const HANDLERS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/render/handlers");

/// Every `.rs` under `dir`, excluding test modules — a `#[cfg(test)]` file's
/// fixtures hold pools on purpose and answer to nothing.
fn sources(dir: &str) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs")
                && !path.to_string_lossy().contains("tests")
            {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(Path::new(dir), &mut out);
    out.sort();
    out
}

/// Whether this file **retains** a staging pool: a struct **field** holding
/// one for longer than a call.
///
/// Two things wear the same spelling and are not this. A `staging.rs` module
/// hands its own global out (`pub fn global() -> &'static StagingPool`), and a
/// constructor takes one to store (`fn new(.., pool: &'static ..) -> Self`);
/// neither is a resident block, and the second is already covered by the field
/// it writes into. A signature is what both have and a field declaration has
/// not, so `fn`/`->` is the discriminator. [`only_definitions_are_excluded`]
/// keeps it honest by requiring everything it drops to be a pool *definition*.
fn retains_a_pool(src: &str) -> bool {
    src.lines().any(is_pool_field)
}

fn is_pool_field(line: &str) -> bool {
    mentions_a_static_pool(line) && !line.contains("fn ") && !line.contains("->")
}

fn mentions_a_static_pool(line: &str) -> bool {
    line.contains("&'static") && line.contains("StagingPool")
}

/// The body of `fn resident_source_bytes`, by brace matching from its
/// signature. `None` when the file has no such function at all — which for a
/// pool-holding handler is the same defect in its worst form: a parked mosaic
/// priced at the trait's `0`.
fn residency_body(src: &str) -> Option<&str> {
    let at = src.find("fn resident_source_bytes")?;
    let open = at + src[at..].find('{')?;
    let mut depth = 0usize;
    for (i, c) in src[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&src[open..=open + i]);
                }
            }
            _ => {}
        }
    }
    None
}

fn name(path: &Path) -> String {
    path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
        .unwrap_or(path)
        .display()
        .to_string()
}

/// **Every handler that retains a staging pool counts its parked buffer.**
///
/// The relation itself — that the figure rises by exactly the parked bytes — is
/// asserted per layer in `handlers::{gmgsi,mrms}::tests`. This is the half
/// those cannot cover: a *third* whole-grid source, added by an author who
/// reads the two existing handlers and copies everything but the one line.
///
/// **Floor:** delete the `retained_bytes()` term from either handler's
/// residency and this reddens naming that file.
#[test]
fn a_handler_holding_a_staging_pool_prices_its_parked_buffer() {
    let files = sources(HANDLERS);
    assert!(
        files.len() >= 15,
        "the walk found {} handler sources, which is not this tree — a walk \
         over the wrong directory finds no pools and passes saying nothing",
        files.len(),
    );

    let mut holders = Vec::new();
    let mut plain = 0usize;
    for path in &files {
        let src = std::fs::read_to_string(path).expect("a handler source reads");
        if retains_a_pool(&src) {
            holders.push((name(path), src));
        } else {
            plain += 1;
        }
    }

    // Positive checks on the same haystack: the needle still matches the two
    // layers that certainly hold a pool, and it does not match everything.
    assert!(
        holders.iter().any(|(n, _)| n.ends_with("gmgsi.rs")),
        "the pool needle no longer matches the GMGSI handler, so this walk is \
         reading a spelling that has moved. Found: {:?}",
        holders.iter().map(|(n, _)| n).collect::<Vec<_>>(),
    );
    assert!(
        holders.iter().any(|(n, _)| n.ends_with("mrms.rs")),
        "the pool needle no longer matches the MRMS handler. Found: {:?}",
        holders.iter().map(|(n, _)| n).collect::<Vec<_>>(),
    );
    assert!(
        plain > 0,
        "every handler read as holding a pool, so this check cannot fail and \
         is not checking anything",
    );

    for (file, src) in &holders {
        let body = residency_body(src).unwrap_or_else(|| {
            panic!(
                "{file} retains a staging pool and has no `resident_source_bytes` \
                 at all, so its parked grid is priced at the trait's 0",
            )
        });
        assert!(
            body.contains("retained_bytes"),
            "{file} retains a staging pool whose parked buffer is one whole \
             grid — 15,000,000 B for GMGSI, 49,000,000 B for MRMS — and its \
             `resident_source_bytes` does not read it. The block is resident \
             whether or not anything is decoding, so the `overlay grids` census \
             the memory governor sheds against would report this layer as \
             holding less than it does, and an under-reporting census sheds \
             nothing. Add the pool's `retained_bytes()` to the sum, and a \
             relation test beside the ones in `handlers::gmgsi::tests` and \
             `handlers::mrms::tests` that parks a buffer and requires the \
             figure to rise by exactly what the slot says it holds. Body was:\
             \n{body}",
        );
    }
}

/// **A retained pool may only be held where something prices it.**
///
/// The check above asks handlers the question; this one is why asking handlers
/// is enough. A pool parked on some other long-lived struct would be resident
/// bytes with no `resident_source_bytes` anywhere above it, and the walk over
/// the handler directory would never see it.
#[test]
fn every_retained_staging_pool_in_this_crate_is_held_by_a_handler() {
    let files = sources(CRATE_SRC);
    assert!(
        files.len() >= 40,
        "the walk found {} crate sources, which is not this crate",
        files.len(),
    );

    let mut holders = Vec::new();
    for path in &files {
        let src = std::fs::read_to_string(path).expect("a crate source reads");
        if retains_a_pool(&src) {
            holders.push(name(path));
        }
    }
    assert!(
        holders.len() >= 2,
        "the pool needle matched {} files across the whole crate, so it has \
         rotted: two layers certainly retain one. Found: {holders:?}",
        holders.len(),
    );
    for file in &holders {
        assert!(
            file.contains("/render/handlers/"),
            "{file} retains a staging pool outside the handler tree, where \
             nothing prices its parked grid into `resident_source_bytes`. \
             Either hold it on the handler that owns the source, or extend \
             `a_handler_holding_a_staging_pool_prices_its_parked_buffer` to \
             reach it — a grid-sized block nobody counts is exactly the census \
             gap this pair of tests exists to close",
        );
    }
}

/// **The `fn`/`->` discriminator only ever drops a pool *definition*.**
///
/// [`retains_a_pool`] narrows "mentions a `&'static` pool" to "declares one as
/// a field", and a needle that narrows can narrow to nothing and read green.
/// So every line it drops is checked to belong to a module that *defines* a
/// pool — a `staging.rs` handing out its own `global()` — rather than to some
/// third shape that holds one and would now be invisible to both walks above.
#[test]
fn only_definitions_are_excluded() {
    let mut dropped = Vec::new();
    for path in sources(CRATE_SRC) {
        let src = std::fs::read_to_string(&path).expect("a crate source reads");
        if src.lines().any(mentions_a_static_pool) && !retains_a_pool(&src) {
            dropped.push((name(&path), src));
        }
    }
    assert!(
        !dropped.is_empty(),
        "nothing was dropped, so this check is vacuous and the discriminator \
         in `retains_a_pool` is no longer doing anything",
    );
    for (file, src) in &dropped {
        assert!(
            src.contains("static GLOBAL") || src.contains("fn global()"),
            "{file} names a `&'static` staging pool, is not read as holding one, \
             and does not define one either — so it is a third shape the walks \
             above cannot see. Widen `retains_a_pool` to reach it",
        );
    }
}
