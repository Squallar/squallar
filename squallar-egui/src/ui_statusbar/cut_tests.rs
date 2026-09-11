//! **That the nine cuts of `ui:statusbar` are nine cuts and not one.**
//!
//! [`crate::shell_api::StatusbarStamps`] is read as a decomposition — a share
//! of `frame ui (statusbar)` per name — and the two ways that reading goes
//! wrong are both silent. A stamp that is never taken leaves its cut reading a
//! structural zero indistinguishable from work that did not happen; and a pair
//! of stamps taken out of order makes `frame_ledger::micros` saturate to zero,
//! so the cut disappears and its neighbour swallows the span without anything
//! failing.
//!
//! `frame_ledger`'s `the_statusbar_phases_telescope_to_statusbar` and
//! `every_statusbar_stamp_moves_a_cut` cover the arithmetic over a fixture;
//! these cover the stamps a **real frame** produces, through
//! `crate::input_harness::InputHarness`, which runs the shipped
//! `Gui::ui_phased`.

use crate::input_harness::InputHarness;

/// The eight field names [`crate::shell_api::StatusbarStamps`] declares, read
/// out of the struct rather than restated — a ninth stamp added with no site
/// must fail [`every_declared_statusbar_stamp_is_filled_by_the_bar`] rather
/// than join it.
fn declared_stamps() -> Vec<String> {
    let source = include_str!("../shell_api.rs");
    let body = source
        .split_once("pub struct StatusbarStamps {")
        .expect("StatusbarStamps is no longer declared in shell_api.rs")
        .1
        .split_once("\n}")
        .expect("StatusbarStamps' declaration has no recognisable end")
        .0;
    body.lines()
        .filter_map(|line| {
            let field = line
                .trim()
                .strip_prefix("pub ")?
                .strip_suffix(": web_time::Instant,")?;
            Some(field.to_string())
        })
        .collect()
}

/// **Every declared stamp is filled by the bar, from its own clock read.**
///
/// The negative half — "a field with no site fails" — is what the test is for,
/// so the list is read off the struct and not restated here: a restated list
/// passes for free the day a ninth field lands.
#[test]
fn every_declared_statusbar_stamp_is_filled_by_the_bar() {
    let bar = include_str!("../ui_statusbar.rs");
    let stamps = declared_stamps();
    assert_eq!(
        stamps.len(),
        8,
        "StatusbarStamps no longer declares eight stamps ({stamps:?}); the \
         ledger's `statusbar_phase_micros` and `frame_statusbar_lines` are \
         [_; 9] and must move with it",
    );
    for stamp in &stamps {
        assert!(
            bar.contains(&format!("{stamp}: gated,"))
                || bar.contains(&format!("{stamp}: stamped["))
                || bar.contains(&format!("{stamp},")),
            "`StatusbarStamps::{stamp}` is declared but `render_status_bar` \
             never fills it, so its cut reports a structural zero that reads \
             as work that did not happen",
        );
    }
    // **Every interior slot is written, from its own clock read.** Counting
    // the sites is what separates "the struct is filled" from "the struct is
    // filled with eight DIFFERENT readings": a field assigned from a
    // neighbour's stamp would pass the loop above and read exactly zero for
    // ever. `gated` is a plain binding taken after the prologue; the other
    // seven ride the `Option` array the closure writes, and slots 0 and 1 are
    // written twice because the collapsed path crosses them somewhere else.
    for slot in 0..7 {
        let site = format!("cuts[{slot}] = Some(web_time::Instant::now());");
        assert!(
            bar.contains(&site),
            "interior stamp {slot} is never written in `render_status_bar`, \
             so the cut it opens can only ever read zero — a seam of this \
             split has vanished into its neighbour",
        );
    }
    assert_eq!(
        bar.matches("Some(web_time::Instant::now())").count(),
        9,
        "the interior stamp sites have moved: seven slots plus the two the \
         collapsed path writes for slots 0 and 1. A site added or lost \
         changes which span each cut names",
    );
    assert!(
        bar.contains("let gated = web_time::Instant::now();"),
        "`gated` is no longer taken after the bar's prologue, so `gate` and \
         `open` are no longer the two cuts they are documented to be",
    );
}

/// The nine cuts a real frame's stamps produce, in `StatusbarHists`' order,
/// as whole microseconds — `frame_ledger::statusbar_phase_micros`' arithmetic,
/// restated here because that function is in the App crate.
///
/// **Saturating, exactly as `micros` is.** That is the property under test: a
/// reversed pair prints zero rather than panicking, so the ordering assertion
/// below is what stands between a silently lost cut and a reader.
fn cuts_of(h: &InputHarness) -> [u128; 9] {
    let p = h.last_phases();
    let b = &p.statusbar_cuts;
    let span = |a: web_time::Instant, z: web_time::Instant| {
        z.checked_duration_since(a).map_or(0, |d| d.as_micros())
    };
    [
        span(p.topbar, b.gated),
        span(b.gated, b.opened),
        span(b.opened, b.buttoned),
        span(b.buttoned, b.chipped),
        span(b.chipped, b.scanned),
        span(b.scanned, b.aged),
        span(b.aged, b.hovered),
        span(b.hovered, b.errored),
        span(b.errored, p.statusbar),
    ]
}

/// The eight stamps and the parent's two ends, in the order a frame crosses
/// them.
fn ordered(h: &InputHarness) -> Vec<(&'static str, web_time::Instant)> {
    let p = h.last_phases();
    let b = &p.statusbar_cuts;
    vec![
        ("topbar", p.topbar),
        ("gated", b.gated),
        ("opened", b.opened),
        ("buttoned", b.buttoned),
        ("chipped", b.chipped),
        ("scanned", b.scanned),
        ("aged", b.aged),
        ("hovered", b.hovered),
        ("errored", b.errored),
        ("statusbar", p.statusbar),
    ]
}

/// **The stamps a real frame takes are monotone, on every path the bar has.**
///
/// This is the property the ledger's fixture cannot hold: `micros` saturates,
/// so a pair taken out of order in the shipped function reports `0` and the
/// span lands in whichever neighbour brackets it. Nothing else in the tree
/// fails when that happens — the nine still telescope to the parent, because
/// both ends are the parent's.
///
/// Driven over every path `render_status_bar` can take: drawn, collapsed, and
/// not drawn at all.
#[test]
fn the_stamps_a_real_frame_takes_are_monotone_on_every_path() {
    let mut h = InputHarness::new();

    // Wide and expanded: every boundary crossed.
    h.set_screen(egui::vec2(1440.0, 900.0));
    h.frame();
    let wide = ordered(&h);

    // Wide and collapsed: `opened` and `buttoned` are crossed, the six after
    // them come off one clock read, and `close` holds the teardown.
    h.set_status_bar_collapsed_for_test(true);
    h.frame();
    let collapsed = ordered(&h);

    // Compact: the bar is not drawn at all and `gate` holds the whole cut.
    h.set_screen(egui::vec2(420.0, 900.0));
    h.frame();
    let compact = ordered(&h);

    for (path, stamps) in [
        ("a drawn bar", wide),
        ("a collapsed bar", collapsed),
        ("a bar that is not drawn", compact),
    ] {
        for pair in stamps.windows(2) {
            let (before, at) = pair[0];
            let (after, then) = pair[1];
            assert!(
                then >= at,
                "on {path}, `{after}` is stamped BEFORE `{before}`, so the \
                 cut between them saturates to zero in the ledger and its \
                 span is charged to a neighbour with nothing going red",
            );
        }
    }
}

/// **The nine cuts telescope to the parent on a frame a clock produced.**
///
/// The ledger's own telescoping test drives a synthetic timeline; this one
/// drives the shipped `Gui::ui_phased` and holds the same identity over
/// whatever instants the machine's clock actually returned. Stated in
/// microseconds with the same truncation bound the ledger carries: nine
/// truncating spans can lose eight microseconds and never more, and may never
/// exceed the parent.
#[test]
fn a_real_frames_nine_cuts_telescope_to_its_statusbar_cut() {
    let mut h = InputHarness::new();
    h.set_screen(egui::vec2(1440.0, 900.0));
    for step in 0..3 {
        if step == 1 {
            h.set_status_bar_collapsed_for_test(true);
        }
        if step == 2 {
            h.set_screen(egui::vec2(420.0, 900.0));
        }
        h.frame();
        let p = h.last_phases();
        let parent = p
            .statusbar
            .checked_duration_since(p.topbar)
            .map_or(0, |d| d.as_micros());
        let summed: u128 = cuts_of(&h).iter().sum();
        assert!(
            summed <= parent && parent - summed <= 8,
            "step {step}: the nine cuts sum to {summed} us against a parent \
             span of {parent} us, so they are not a decomposition of \
             `ui.statusbar`",
        );
    }
}

/// **A bar that is not drawn files its whole cut under `gate`.**
///
/// The reading a phone-width leg produces, and the one a reader is most
/// likely to misread as an absence. Held as an identity rather than as a
/// magnitude: on a compact width the eight cuts after `gate` are exactly zero
/// because `skipped_after` fills their stamps from one clock read, so `gate`
/// plus `close` is the whole span and `close` is the return out of the call.
#[test]
fn a_bar_that_is_not_drawn_files_its_whole_cut_under_gate() {
    let mut h = InputHarness::new();
    h.set_screen(egui::vec2(420.0, 900.0));
    h.frame();
    let cuts = cuts_of(&h);
    assert_eq!(
        &cuts[1..8],
        &[0; 7],
        "a compact-width frame charged one of `open`..`error`, but the bar \
         returns before any of them can run — so the split is naming a span \
         that did not happen",
    );
    let p = h.last_phases();
    let parent = p
        .statusbar
        .checked_duration_since(p.topbar)
        .map_or(0, |d| d.as_micros());
    assert!(
        cuts[0] + cuts[8] <= parent && parent - (cuts[0] + cuts[8]) <= 8,
        "`gate` ({}) plus `close` ({}) is not the whole of a not-drawn \
         frame's {parent} us cut, so the span went somewhere this family \
         cannot name",
        cuts[0],
        cuts[8],
    );
}
