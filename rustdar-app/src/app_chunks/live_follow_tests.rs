//! **The half of `UNLINK_NOTE` that is already true, held down.**
//!
//! "Parked in the archive it holds its moment; **still live, it still follows
//! new scans.**" The archive gate that makes the first clause true is one
//! `Gui::apply` arm away from breaking the second, because both used to be the
//! same event. These pin that the live chunk feed still reaches **every**
//! pane on the site — including one that has opted out of shared time.

use super::super::App;
use super::super::tests::two_pane_app;
use super::volume_close_tests::closing_round;

const SITE: &str = "KTLX";

/// Two live panes on one site, pane 1 out of the shared time group.
fn two_live_unlinked_panes() -> App {
    let mut app = two_pane_app(SITE, SITE);
    for idx in 0..2 {
        super::selection_tests::show_on(
            &mut app,
            idx,
            rustdar_radar::types::RadarProduct::Reflectivity,
            0.5,
            &[0.5, 1.0, 1.5],
        );
    }
    app.gui.pane_mut(1).expect("pane 1").time_link = false;
    app.render.ensure_pane_count(2);
    app
}

fn shown_at(app: &App, idx: usize) -> Option<chrono::NaiveDateTime> {
    app.gui
        .pane(idx)
        .and_then(|p| p.scan_info.as_ref().map(|i| i.timestamp))
}

/// **The overshoot guard.** A volume the chunk feed closed reaches both live
/// panes, and the time link has nothing to say about it: nobody asked for this
/// volume, it is simply what the site is doing now.
#[test]
fn a_closed_live_volume_still_reaches_a_time_unlinked_pane_on_the_site() {
    let mut app = two_live_unlinked_panes();
    let before = shown_at(&app, 1).expect("the fixture puts a volume on pane 1");
    assert!(
        !app.gui.pane(1).expect("pane 1").time_link,
        "precondition: pane 1 must be out of the shared time group, or this \
         test passes whatever the gate does",
    );
    assert!(
        app.gui.pane(1).expect("pane 1").viewing_live,
        "precondition: pane 1 must be watching live — the promise is about a \
         live pane, not about any unlinked one",
    );

    app.apply_chunk_outcome(SITE, &closing_round(5));

    let after = shown_at(&app, 1).expect("pane 1 must still hold scan info");
    assert_ne!(
        after, before,
        "the live unlinked pane stopped following new scans. Gating the chunk \
         feed the way the archive is gated breaks the clause of `UNLINK_NOTE` \
         that was already true.",
    );
    assert_eq!(
        shown_at(&app, 0),
        Some(after),
        "both live panes on the site must be on the same new volume",
    );
}

/// **The other clause, from the other producer.** "Parked in the archive it
/// holds its moment" is a claim about the pane, not about which subsystem
/// happens to deliver: a sibling still watching live keeps the chunk feed
/// running, and its volumes must not drag the parked pane forward either.
#[test]
fn a_pane_parked_in_the_archive_is_not_dragged_forward_by_the_live_feed() {
    let mut app = two_live_unlinked_panes();
    app.gui.pane_mut(0).expect("pane 0").viewing_live = false;
    let parked = shown_at(&app, 0).expect("the fixture puts a volume on pane 0");
    assert!(
        app.gui.pane(1).expect("pane 1").viewing_live,
        "precondition: a live sibling is what keeps the feed running for this \
         site; without one `apply_chunk_outcome` returns early and the case \
         below cannot fail",
    );

    app.apply_chunk_outcome(SITE, &closing_round(5));

    assert_eq!(
        shown_at(&app, 0),
        Some(parked),
        "the parked pane was moved by a volume it did not ask for and is not \
         watching for",
    );
    assert_ne!(
        shown_at(&app, 1),
        Some(parked),
        "and the live sibling must still have advanced, or this passes by the \
         feed doing nothing at all",
    );
}
