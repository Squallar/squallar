//! WO-E2 contract test 3 of 3: **the seam ratchet**. The App reaches the
//! `Gui` through `Gui::apply` / `Gui::apply_frame_inputs` and through nothing
//! else that looks like a setter push; the residual method-call coupling only
//! ever shrinks.
//!
//! Counted over **whitespace-collapsed** source, because three of the
//! converted call sites were line-wrapped (`self.gui` on one line, the
//! method on the next) and invisible to a single-line grep — the shape that
//! let them survive the first audit. Collapsing first means a wrapped
//! regression counts exactly like a straight one.
//!
//! Needles are built from split literals (the E0c arch_ratchets discipline)
//! so this file never contains its own patterns and the campaign-close
//! zero-grep holds without excluding it.
//!
//! These per-file ceilings are migration scaffolding: they end at 0 (WO-E8)
//! and the test is deleted at campaign close.

/// The App-pokes-Gui coupling (a field access of `gui` on `self`, dot
/// included), split so this file cannot count itself.
const SELF_GUI: &str = concat!("self.", "gui.");
/// A setter push through that coupling — the shape WO-E2 deleted.
const SELF_GUI_SET: &str = concat!("self.", "gui.", "set_");
/// The seam call through that coupling; its presence is the control that the
/// scrape is reading real, current source.
const SELF_GUI_APPLY: &str = concat!("self.", "gui.", "apply");

const APP: &str = include_str!("../app.rs");
const APP_FETCH: &str = include_str!("../app_fetch.rs");
const APP_RENDER: &str = include_str!("../app_render.rs");
const APP_CHUNKS: &str = include_str!("../app_chunks.rs");

/// The file with every run of whitespace removed, so a call wrapped across
/// lines counts exactly like one that is not.
fn collapsed(source: &str) -> String {
    source.split_whitespace().collect()
}

#[test]
fn no_production_file_pushes_through_a_gui_setter() {
    // Presence control: the seam really is in the scraped source. A moved or
    // renamed file must fail here loudly, never count zero.
    assert!(
        collapsed(APP).contains(SELF_GUI_APPLY),
        "control: app.rs no longer contains `{SELF_GUI_APPLY}` — the scrape \
         is not reading the seam it exists to guard",
    );
    for (name, source) in [
        ("app.rs", APP),
        ("app_fetch.rs", APP_FETCH),
        ("app_render.rs", APP_RENDER),
        ("app_chunks.rs", APP_CHUNKS),
    ] {
        let n = collapsed(source).matches(SELF_GUI_SET).count();
        assert_eq!(
            n, 0,
            "{name} contains {n} `{SELF_GUI_SET}` push(es) (counted \
             whitespace-collapsed, so a line-wrapped call counts too). \
             WO-E2 replaced the setter push with Gui::apply for event-shaped \
             state and Gui::apply_frame_inputs for frame-composed state — \
             route the new push through the seam.",
        );
    }
}

#[test]
fn the_gui_coupling_only_ever_shrinks() {
    // Measured whitespace-collapsed at the WO-E2 Land 1 conversion. Higher
    // than the old single-line baseline for app_render.rs because collapsing
    // counts the wrapped receivers the old grep missed — these are the real
    // totals.
    for (name, source, ceiling) in [
        ("app.rs", APP, 38),
        ("app_fetch.rs", APP_FETCH, 46),
        ("app_render.rs", APP_RENDER, 109),
        ("app_chunks.rs", APP_CHUNKS, 18),
    ] {
        let n = collapsed(source).matches(SELF_GUI).count();
        assert!(
            n <= ceiling,
            "{name}: {n} `{SELF_GUI}` occurrences > ceiling {ceiling}. This \
             count only ratchets DOWN — lower the pin in the land that earns \
             it; never raise. WO-E8 drives it to 0 when the radar root \
             fields dissolve.",
        );
    }
}
