//! **An `App` a test builds reaches nothing outside this process.**
//!
//! Every `App` built inside this crate's test binary is offline by
//! construction — the switch is `App::offline_for_tests`, set from `cfg!(test)`
//! in `App::new` and settable nowhere else, so a release build cannot reach it
//! and no test has to remember to ask.
//!
//! **What it removes, measured under `strace -f -e trace=connect`, 2026-09-06.**
//! One ordinary unit test — `idle_raster_tests`, an `n_pane_app` and a handful
//! of frames — made **42 `connect()` calls to non-loopback addresses**: eight S3
//! fronts with their IPv6 twins, three CloudFront edges, and `192.168.254.254`,
//! which is the chunk-notification service at [`DEFAULT_NOTIFIER_ENDPOINT`].
//! The whole `--lib` suite made **2,638**. Both are **0** now, and the 259
//! connections the suite still makes are all to `127.0.0.1`.
//!
//! Three costs came with those calls. The suite fails when the network does. It
//! bills a third-party bucket and this project's own notifier stack on every
//! run, from a repo that has already taken a real bill from unfiltered traffic
//! to that stack. And it races: a live KTLX volume landing mid-fixture takes
//! `Gui::apply(ScanInfoForSite)` through the session's initial-zoom claim and
//! the pane goes from zoom 4 to 7. Traced over ten isolated runs of one
//! instrument, that claim fell **inside the measured window six times** and
//! after the test ended the other four.
//!
//! **Where the switch is read.** Two shapes, and which one a site takes is
//! decided by whether a test could legitimately want the step to *run*:
//!
//! *Declined* — the step does not happen, because its answer could only ever
//! come from the network and no sound test can depend on when that arrives.
//! Each sits after every synchronous effect (generation bumps, spinners,
//! in-flight marks) and before the request:
//!
//! - `App::handle_gui_action` — `FetchRadarScan`, `CheckForNewScans`,
//!   `FetchOverlay`, `RefreshOverlay`. Ahead of the handler, so nothing marks a
//!   fetch in flight that can never land.
//! - `App::spawn_site_catalogue_refresh` — the one request `App::new` makes on
//!   its own, which nothing a test did afterwards could have stopped.
//! - `App::spawn_fetch` and `App::handle_navigate_one_scan` — the archive
//!   volume walk, after `next_scan_generation`.
//! - `App::spawn_level3_fetches` — fired on every scan delivery, and these
//!   suites deliver scans by the hundred. Neither the sounding nor the objects
//!   ride `App::http_client`, so the inert client below cannot reach them.
//! - `App::spawn_loop_l3_listing` / `spawn_loop_l3_pairing`.
//! - `App::drive_chunk_feeds` and `App::fetch_notified_chunk` — **ahead of
//!   `take_for_round` / `take_now`**, not at the executor: the poller travels
//!   into the task and back on the channel, so a dropped task is a feed that
//!   never gets its poller back.
//! - `App::drive_chunk_notifications` — takes the same teardown branch the
//!   "notifications off" setting takes, because `ChunkNotifier::sync_sites`
//!   opens its websockets on its own thread and never touches the executor.
//!
//! *Routed to a sink* — the step runs and fails, because a suite is asserting
//! on the failure. `App::new` installs
//! [`unreachable_http_client`] and `Gui::go_offline_for_tests`, so the tile
//! sources and every overlay and frame fetch dispatch exactly as shipped and
//! are refused at `127.0.0.1:1`.
//!
//! **Not a gate: `App::spawn_detached`.** Dropping the future there was tried
//! and over-fires — `gmgsi_loop_tests`, `satellite_loop_draw_tests` and
//! `mrms_loop_tests` drive the real frame dispatch on purpose and assert that
//! its *failure* comes back on the arrival path, which a task that never ran
//! cannot deliver. It reddened all five. So that door counts instead.
//!
//! **The counter is the gate; the sockets are the symptom.**
//! `app::offline_tests` builds an `App` the ordinary way, pumps frames and
//! asserts the tally is zero — in-process, on every `cargo test`, where an
//! `strace` reading needs a person and a tracer. Because it counts detached
//! *tasks* rather than proven sockets it over-approximates, which is the safe
//! direction for an assertion of zero and is what makes it a ratchet: a network
//! path added later that nobody thought to decline still lands here.
//!
//! [`DEFAULT_NOTIFIER_ENDPOINT`]: squallar_radar::source::DEFAULT_NOTIFIER_ENDPOINT

/// A step that leaves this process, counted where it is taken.
///
/// Constructed on the production path as well as the test one — it is the
/// argument to `App::may_reach_the_network`, which is how every gated site
/// spells the question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    /// A detached task on the app's own executor: `App::spawn_detached`.
    DetachedTask,
    /// A chunk-notification websocket, asked for by
    /// `App::drive_chunk_notifications`.
    NotifierSocket,
}

/// What the process has originated since the last [`reset`], by kind.
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Tally {
    /// Tasks started through `App::spawn_detached`.
    pub(crate) detached_tasks: u32,
    /// Notifier syncs that were allowed to reach a real endpoint.
    pub(crate) notifier_sockets: u32,
}

#[cfg(test)]
impl Tally {
    /// Every counted step, whatever kind.
    pub(crate) fn total(self) -> u32 {
        self.detached_tasks + self.notifier_sockets
    }
}

#[cfg(test)]
thread_local! {
    /// **Per thread, not per process.** `cargo test` runs this crate's suites
    /// concurrently in one binary, so a process-global tally would be every
    /// sibling's writes as well as this test's — the same hazard
    /// `squallar_egui::overlay_cache::ledger` carries its `test-support`
    /// feature for. An `App` and everything that dispatches off it live on one
    /// thread, so the thread is the right scope: the count a test reads is the
    /// count its own `App` made.
    static TALLY: std::cell::Cell<Tally> = const { std::cell::Cell::new(Tally {
        detached_tasks: 0,
        notifier_sockets: 0,
    }) };
}

/// Count one step that was **allowed** — a declined one originated nothing and
/// is not counted, which is what lets the gate assert a bare zero.
#[cfg(test)]
pub(crate) fn record(origin: Origin) {
    TALLY.with(|tally| {
        let mut counts = tally.get();
        match origin {
            Origin::DetachedTask => counts.detached_tasks += 1,
            Origin::NotifierSocket => counts.notifier_sockets += 1,
        }
        tally.set(counts);
    });
}

/// The shipped build counts nothing: this is the whole of the instrument
/// outside `cfg(test)`, and it compiles away.
#[cfg(not(test))]
pub(crate) fn record(_origin: Origin) {}

/// Start this thread's tally from zero.
#[cfg(test)]
pub(crate) fn reset() {
    TALLY.with(|tally| tally.set(Tally::default()));
}

/// What this thread has originated since the last [`reset`].
#[cfg(test)]
pub(crate) fn taken() -> Tally {
    TALLY.with(std::cell::Cell::get)
}

/// **An HTTP client that cannot leave this machine**, whatever URL it is
/// handed.
///
/// Installed as `App::http_client` under the switch, and the reason the switch
/// does not have to know every origin: a task that runs against this reaches
/// `127.0.0.1:1` and is refused, so the *dispatch* under test still runs, still
/// fails, and still delivers its failure on the production arrival path — which
/// is exactly what `gmgsi_loop_tests`, `satellite_loop_draw_tests`,
/// `mrms_loop_tests` and `glm_loop_draw_tests` assert.
///
/// **A dead proxy, not a timeout.** Those four suites already asked for a
/// client "that cannot connect" and got one whose only defence was a 1 ms
/// timeout — and a timeout is not a refusal: the request still resolved
/// `*.s3.amazonaws.com` and still opened the socket, it just gave up on it
/// quickly. Measured 2026-09-06 under `strace -f -e trace=connect`, those four
/// suites made **1,988 connections to S3 between them** while their own doc
/// comments said no test in the tree reaches a network. Routing every scheme
/// through a proxy that is not listening moves the connection itself, so there
/// is nothing left to time out.
///
/// The browser arm carries neither knob — a `fetch` is governed by the page,
/// not by the client — so the `cfg` selects the builder value and the one body
/// below builds whichever it was handed.
pub(crate) fn unreachable_http_client() -> reqwest::Client {
    let builder = reqwest::Client::builder();
    #[cfg(not(target_arch = "wasm32"))]
    let builder = builder
        .timeout(std::time::Duration::from_millis(50))
        .connect_timeout(std::time::Duration::from_millis(50))
        .proxy(
            reqwest::Proxy::all("http://127.0.0.1:1")
                .expect("a literal loopback proxy URL always parses"),
        );
    builder
        .build()
        .expect("a client with no connection to make")
}
