//! The other way a 3D pane stops needing its volume: the layout stops showing
//! it.
//!
//! `GuiAction::ReleaseVolume` covers the *kind* change — a pane converted out
//! of `Volume` — and nothing covered the pane-count reduction, because that
//! hides a pane without converting it: the `PaneState` stays in the vector so a
//! re-split remembers it, and no action is emitted. The store went on holding
//! that pane's resolved grid (36 MiB of GPU texture and ~8 MiB of host bytes on
//! the desktop shape) for the rest of the session, and — worse than the bytes —
//! `VolumeStore::enforce_budget` went on evicting *oldest first* to fit them,
//! which is a live 3D loop's own frames.
//!
//! Every assertion below is in **bytes off a real `VoxelGrid`**. A store of
//! `Refused` stubs would satisfy an entry-count assertion while giving nothing
//! back, which is exactly the shape of test this closes rather than adds to.

use crate::app::tests::{headless, two_pane_app};
use crate::platform_double::TestBridge;
use crate::volume::bridge::tests::ready_grid;
use crate::volume::bridge::{Hold, VolumeEntry};
use rustdar_egui::config_store::{ConfigStore, MemoryConfigStore, UI_CONFIG_KEY};
use rustdar_egui::pane::{PaneKind, VolumeStamp, VolumeTarget};
use rustdar_radar::types::RadarProduct;

const SITE: &str = "KTLX";

fn target(minute: u32) -> VolumeTarget {
    VolumeTarget {
        region: None,
        product: RadarProduct::Reflectivity,
        volume: VolumeStamp {
            site: SITE.to_owned(),
            collected: chrono::NaiveDate::from_ymd_opt(2026, 8, 11)
                .unwrap()
                .and_hms_opt(18, minute, 0)
                .unwrap(),
        },
    }
}

/// GPU texture bytes one [`ready_grid`] costs the store — the figure every
/// assertion here is denominated in.
fn one_grid() -> usize {
    let VolumeEntry::Ready(grid) = ready_grid() else {
        unreachable!("ready_grid is Ready")
    };
    let shape = grid.shape();
    crate::volume::raymarch::grid_bytes_with_mips([
        u32::try_from(shape.nx).unwrap(),
        u32::try_from(shape.ny).unwrap(),
        u32::try_from(shape.nz).unwrap(),
    ])
    .expect("a fixture grid cannot overflow")
        + crate::constants::VOLUME_LUT_BYTES
}

/// Make `pane_idx` a 3D pane, and record that it has already been served
/// `rendered_for` — which is what stops `PrepareVolume` firing again, and so
/// what a release has to clear.
fn aim_at_volume(app: &mut crate::app::App, pane_idx: usize, t: &VolumeTarget) {
    let pane = app.gui.pane_mut(pane_idx).expect("the pane exists");
    pane.site = SITE.to_owned();
    pane.set_kind(PaneKind::Volume);
    pane.volume_mut()
        .expect("a 3D pane has volume state")
        .rendered_for = Some(t.clone());
}

/// Open and resolve a build for `pane_idx` the way production does.
fn make_resident(app: &crate::app::App, pane_idx: usize, t: &VolumeTarget, hold: Hold) {
    app.volume_store.begin_build_held(pane_idx, t, hold);
    assert!(
        app.volume_store.complete(t, ready_grid()),
        "precondition: the entry this just opened takes the result",
    );
}

/// Take the layout down to one pane, leaving the second one *remembered* —
/// which is the whole of what a pane-count reduction does.
///
/// Through the config loader because that is the only route to a layout change
/// this crate has (`Gui::set_pane_count_for_test` is `#[cfg(test)]` inside
/// `rustdar-egui`), and it is a production path rather than a workaround: it is
/// what a returning user's saved layout takes. The config names **one** pane,
/// so the restore loop — bounded by `.take(count)` — never touches pane 1, and
/// every assertion below about pane 1's state is about state this transition
/// left alone.
fn hide_the_second_pane(app: &mut crate::app::App) {
    let store = MemoryConfigStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            &format!(r#"{{"pane_count":1,"site":"{SITE}","panes":[{{"site":"{SITE}"}}]}}"#),
        )
        .expect("the memory store always accepts a write");
    assert!(app.gui.load_ui_config(&store), "the one-pane config parsed");
    assert_eq!(
        app.gui.panes().len(),
        1,
        "precondition: the layout shows one pane",
    );
    assert_eq!(
        app.gui.remembered_pane_count(),
        2,
        "precondition: the hidden pane is still remembered — if it were dropped \
         from the vector this would be a different transition and the leak it \
         models would not exist",
    );
}

/// **The pane-count reduction gives the hidden pane's volume back.**
///
/// Reverting `App::release_hidden_pane_volumes` leaves the store at two grids
/// here, which is the leak this closes.
#[test]
fn hiding_a_3d_pane_gives_its_grid_back_and_leaves_the_visible_one_alone() {
    let one = one_grid();
    assert!(one > 0, "precondition: a resident grid costs something");

    let mut app = two_pane_app(SITE, SITE);
    let shown = target(0);
    let hidden = target(1);
    aim_at_volume(&mut app, 0, &shown);
    aim_at_volume(&mut app, 1, &hidden);
    make_resident(&app, 0, &shown, Hold::Single);
    make_resident(&app, 1, &hidden, Hold::Single);
    assert_eq!(
        app.volume_store.texture_bytes(),
        one * 2,
        "precondition: two panes, two resident grids",
    );

    hide_the_second_pane(&mut app);
    app.release_hidden_pane_volumes();

    assert_eq!(
        app.volume_store.texture_bytes(),
        one,
        "the hidden pane's grid is still resident: {} bytes where one grid is \
         {}",
        app.volume_store.texture_bytes(),
        one,
    );
    assert!(
        app.volume_store.lookup(&hidden).is_none(),
        "the hidden pane's target is still in the store",
    );
    assert!(
        app.volume_store.lookup(&shown).is_some(),
        "the visible pane lost the grid it is drawing, so it would rebuild it \
         and flash its first-build message",
    );

    // The half that decides whether the pane can ever come back. `PrepareVolume`
    // is level-triggered on `rendered_for`, so a pane released while it still
    // named a target would re-split into a permanent "Building…".
    assert_eq!(
        app.gui
            .pane(1)
            .and_then(|p| p.volume())
            .and_then(|v| v.rendered_for.clone()),
        None,
        "the released pane still names the grid it no longer has, so a re-split \
         would never ask for another one",
    );
    // Pane 0's own `rendered_for` is deliberately *not* asserted here: the
    // config load rebuilds the content of every pane it names, so pane 0's
    // key was cleared by the transition rather than by the release. What
    // matters at index 0 is that the grid survived, which is asserted above;
    // that a *live* pane's key is left alone is
    // `the_only_pane_on_screen_never_counts_as_hidden`, which reaches the
    // release without going through the loader at all.

    // Edge-triggered: the next frame finds nothing to do rather than repeating
    // the sweep for the life of the split.
    app.release_hidden_pane_volumes();
    assert_eq!(
        app.volume_store.texture_bytes(),
        one,
        "a second pass moved bytes"
    );
}

/// **A hidden 3D *loop* releases its whole resident set, and a visible one
/// keeps every frame of its own.**
///
/// The two directions the set holder makes dangerous, in one drive:
///
///  * a hidden set holder is the one holder nothing else bounds —
///    `dispatch_loop_renders` walks only the visible panes, so its `retain_set`
///    is never restated and its whole set (thirteen grids, 468 MiB on desktop)
///    outlives the layout;
///  * and the release must detach *that* pane rather than drop the entries, or
///    the visible loop beside it loses the frames it is animating.
#[test]
fn hiding_a_looping_3d_pane_releases_its_set_and_not_the_visible_loops() {
    let one = one_grid();
    let mut app = two_pane_app(SITE, SITE);
    let playing: Vec<VolumeTarget> = (0..3).map(target).collect();
    let stranded: Vec<VolumeTarget> = (3..6).map(target).collect();
    aim_at_volume(&mut app, 0, &playing[0]);
    aim_at_volume(&mut app, 1, &stranded[0]);
    for t in &playing {
        make_resident(&app, 0, t, Hold::Set);
    }
    for t in &stranded {
        make_resident(&app, 1, t, Hold::Set);
    }
    assert_eq!(
        app.volume_store.texture_bytes(),
        one * 6,
        "precondition: two loops of three resident grids each",
    );

    hide_the_second_pane(&mut app);
    app.release_hidden_pane_volumes();

    assert_eq!(
        app.volume_store.texture_bytes(),
        one * 3,
        "the hidden loop's resident set outlived the layout that asked for it",
    );
    for t in &playing {
        assert!(
            app.volume_store.lookup(t).is_some(),
            "the visible loop lost the frame at {:?}: the release dropped the \
             entry instead of detaching one pane from it",
            t.volume.collected,
        );
    }
    assert!(
        app.volume_store.holds_set(0),
        "the visible loop stopped being a set holder, so the next build to land \
         sheds every frame it is animating",
    );
    assert!(
        !app.volume_store.holds_set(1),
        "the released pane kept its set-holder mark, so coming back as a live 3D \
         pane it would be exempt from every shed there is",
    );
}

/// A single-pane app releases nothing, whatever the store holds for pane 0.
///
/// The guard on the predicate itself: `hidden_holders` is an index test against
/// the layout's count, and one that was off by one — or that read the
/// *remembered* count instead of the visible one — would take the live pane's
/// volume away on every frame, rebuilding an 8 MiB grid per frame with a warm
/// machine as the only symptom.
#[test]
fn the_only_pane_on_screen_never_counts_as_hidden() {
    let mut app = headless(TestBridge::desktop());
    let t = target(0);
    aim_at_volume(&mut app, 0, &t);
    make_resident(&app, 0, &t, Hold::Single);
    let before = app.volume_store.texture_bytes();
    assert!(before > 0, "precondition: pane 0 holds a real grid");

    app.release_hidden_pane_volumes();

    assert_eq!(
        app.volume_store.texture_bytes(),
        before,
        "the live pane's own grid was released",
    );
    assert_eq!(
        app.gui
            .pane(0)
            .and_then(|p| p.volume())
            .and_then(|v| v.rendered_for.clone()),
        Some(t),
        "the live pane's render key was cleared, so it will rebuild its grid \
         every frame for ever",
    );
}
