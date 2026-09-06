//! **The acceptance for [`crate::app::offline`]: an ordinary `App`, pumped,
//! originates nothing.**
//!
//! Counted, not traced. `strace -f -e trace=connect` is what *found* this — 42
//! non-loopback `connect()` calls out of one unit test, and 2,638 out of the
//! whole `--lib` suite, on 2026-09-06 — but a tracer needs a person to run it,
//! so it can only ever be the after-the-fact reading of a defect that already
//! shipped. The quantity here is the app's own count of network-originating
//! steps, read in-process on every `cargo test`, and the sockets are downstream
//! of it: no step, no socket.
//!
//! It also ratchets. A network path added later that nobody thought to decline
//! still goes through `spawn_detached` and so still lands in the tally, and this
//! reddens instead of a bucket quietly getting billed again.

use crate::app::offline;

const SCREEN: egui::Vec2 = egui::vec2(1600.0, 900.0);
const MAX_TEXTURE_SIDE: usize = 16384;

/// One frame through the production doors, in the app's own order: drain what
/// arrived (which is where the frame pump's `Ingest` phase runs
/// `drive_chunk_feeds`, and so the notifier and the chunk rounds), lay the
/// frame out, then dispatch what the draw loop asked for.
///
/// Answers how many of the frame's actions were network-reaching ones, read
/// with the dispatcher's own predicate so the floor below cannot drift from
/// what the switch actually declines.
fn one_frame(app: &mut crate::app::App, ctx: &egui::Context, time: f64) -> usize {
    app.poll_data_channels();
    ctx.begin_pass(egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
        time: Some(time),
        max_texture_side: Some(MAX_TEXTURE_SIDE),
        ..Default::default()
    });
    let actions = app.gui.ui(ctx);
    let _ = ctx.end_pass();
    let asked = actions
        .iter()
        .filter(|action| crate::app::fetch::reaches_the_network(action))
        .count();
    app.process_gui_actions(actions);
    asked
}

/// **The acceptance.** An `App` built the way every suite here builds one, run
/// for eight frames, takes no step that leaves this process.
///
/// The window is eight frames rather than one because the paths do not all
/// open on the same frame: `App::new` asks for the site catalogue before a
/// frame exists, `Gui::check_auto_polls` pushes the first `FetchRadarScan` on
/// frame one, `drive_chunk_feeds` reaches the notifier on the first `Ingest`,
/// and an overlay whose `auto_fetch_delay` is zero re-asks on every frame after
/// that.
///
/// **The non-vacuity floor is the second assertion, and it fails on its own.**
/// A zero is what a scene that asked for nothing at all reads too — an `App`
/// with no pane, or a `Gui::ui` that stopped emitting fetch actions, would pass
/// the first assertion while proving nothing. So the frames must be shown to
/// have *emitted* the actions the switch is there to decline.
#[test]
fn an_app_built_the_ordinary_way_originates_nothing_over_the_network() {
    // Before the constructor, not after: `App::new` makes a request of its own
    // (`spawn_site_catalogue_refresh`), and a reset placed after it would be
    // the one call this test could never see.
    offline::reset();
    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    let ctx = egui::Context::default();

    let mut asked = 0;
    for frame in 0..8 {
        asked += one_frame(&mut app, &ctx, frame as f64 / 60.0);
    }

    let tally = offline::taken();
    assert_eq!(
        tally.total(),
        0,
        "an `App` under test reached the network: {} detached task(s) and {} \
         notifier sync(s). Every one of them is a socket to a third-party \
         bucket or to this project's own notifier service, opened by a test \
         that asked for neither — and an arrival on one of them changes what \
         a later frame paints, whenever the network happens to deliver it. \
         Run the suite under `strace -f -e trace=connect` to see which host",
        tally.detached_tasks,
        tally.notifier_sockets,
    );

    assert!(
        asked > 0,
        "the fixture stopped asking: these frames emitted no network-reaching \
         action at all, so the zero above is what a scene that wants nothing \
         reads and not what the switch removed"
    );
}

/// The instrument's own floor: a tally that cannot count would report zero
/// whatever the app did, and the acceptance above would be a check that cannot
/// fail.
#[test]
fn the_tally_counts_what_it_is_given() {
    offline::reset();
    assert_eq!(offline::taken().total(), 0, "a reset tally is not zero");

    offline::record(offline::Origin::DetachedTask);
    offline::record(offline::Origin::DetachedTask);
    offline::record(offline::Origin::NotifierSocket);

    let tally = offline::taken();
    assert_eq!(
        (tally.detached_tasks, tally.notifier_sockets, tally.total()),
        (2, 1, 3),
        "the tally does not count, so the acceptance's zero means nothing"
    );
    offline::reset();
}

/// **The switch is on for every `App` this build makes, and it is `cfg!(test)`
/// that puts it there** — not a call the constructors remember to make.
///
/// `App::new` is reached directly here rather than through
/// `crate::app::tests::headless`, so a future edit that moves the posture back
/// into a test helper fails this rather than passing quietly on the helper
/// every other suite happens to use.
#[test]
fn an_app_is_offline_by_construction_rather_than_by_being_asked() {
    let mut platform = crate::platform_double::TestBridge::desktop();
    crate::test_sites::install();
    let location = squallar_location::LocationFacade::new(Box::new(platform.location_provider()));
    let app = crate::app::App::new(Box::new(platform), location);
    assert!(
        app.offline_for_tests,
        "an `App` built without going through a test helper came up online"
    );
    assert!(
        !app.may_reach_the_network(offline::Origin::DetachedTask),
        "the switch is set and the gate still says yes"
    );
}
