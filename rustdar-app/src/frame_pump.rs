//! The frame pump: the one table that orders how a frame drains, applies, advances and
//! dispatches its background results (WO-E3).

use super::App;

/// When in the frame a `FRAME_PUMP` row runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum PumpPhase {
    /// Runs at `poll_data_channels`' position in `handle_redraw`, BEFORE the renderer-state
    /// early-return — must not need a context.
    Ingest,
    /// Results-apply.
    Apply,
    /// Playback advance.
    Advance,
    /// Dispatch.
    Dispatch,
}

/// One row of the pump: a named step, the phase it runs in, and the receivers it owns.
pub(super) struct DrainEntry {
    /// The `App` method this row runs, by name — what the order pin in
    /// `frame_pump/tests.rs` reads.
    // Read by the pump tests only; production walks `phase` and `run`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) name: &'static str,
    pub(super) phase: PumpPhase,
    /// ChannelHub receiver FIELD NAMES this row drains — the exhaustiveness inventory.
    // Read by the pump tests only; production walks `phase` and `run`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) drains: &'static [&'static str],
    /// ctx is Some only for phases run from `setup_egui_frame`; Ingest rows always get
    /// None.
    pub(super) run: fn(&mut App, Option<&egui::Context>),
}

/// Every drain and dispatch step of a frame, in execution order.
pub(super) const FRAME_PUMP: &[DrainEntry] = &[
    DrainEntry {
        name: "poll_scan_results",
        phase: PumpPhase::Ingest,
        drains: &["scan_receiver"],
        run: pump_poll_scan_results,
    },
    DrainEntry {
        name: "poll_chunk_results",
        phase: PumpPhase::Ingest,
        drains: &["chunk_receiver"],
        run: pump_poll_chunk_results,
    },
    DrainEntry {
        name: "drive_chunk_feeds",
        phase: PumpPhase::Ingest,
        drains: &[],
        run: pump_drive_chunk_feeds,
    },
    // Finished voxel builds land before the stamps are published, so a build and its
    // announcement cannot straddle a frame.
    DrainEntry {
        name: "poll_voxel_results",
        phase: PumpPhase::Ingest,
        drains: &["voxel_receiver"],
        run: pump_poll_voxel_results,
    },
    // Two halves of one moment, in one row on purpose (WO-M14c): the stamps
    // are published, and the 3D panes that were waiting for exactly this
    // volume are dispatched from the same set, before the frame is drawn.
    // Splitting them into two rows would need the arrival set to survive
    // between rows as App state that nothing keeps in step; the row's `name`
    // therefore under-describes it, and this comment is the correction.
    DrainEntry {
        name: "publish_base_volumes",
        phase: PumpPhase::Ingest,
        drains: &[],
        run: pump_publish_base_volumes,
    },
    DrainEntry {
        name: "poll_overlay_fetch_results",
        phase: PumpPhase::Ingest,
        drains: &["overlay_fetch_receiver"],
        run: pump_poll_overlay_fetch_results,
    },
    DrainEntry {
        name: "poll_render_results",
        phase: PumpPhase::Apply,
        drains: &["render_receiver"],
        run: super::render::pump_poll_render_results,
    },
    DrainEntry {
        name: "poll_section_results",
        phase: PumpPhase::Apply,
        drains: &["section_receiver"],
        run: super::render::pump_poll_section_results,
    },
    DrainEntry {
        name: "poll_level3_results",
        phase: PumpPhase::Apply,
        drains: &[
            "sounding_receiver",
            "melting_layer_receiver",
            "storm_motion_receiver",
            "level3_receiver",
        ],
        run: super::render::pump_poll_level3_results,
    },
    DrainEntry {
        name: "poll_site_catalogue",
        phase: PumpPhase::Apply,
        drains: &["site_catalogue_receiver"],
        run: super::render::pump_poll_site_catalogue,
    },
    DrainEntry {
        name: "poll_overlay_render_results",
        phase: PumpPhase::Apply,
        drains: &["overlay_render_receiver"],
        run: super::render::pump_poll_overlay_render_results,
    },
    // Not a channel drain since WO-M12b: a radar frame listing arrives on the
    // one source path in `Ingest`, and this is where the panes waiting on it
    // build their loops. It holds the position the listing channel's own drain
    // held, so a listing still turns into a plan and a dispatch in `Apply`.
    DrainEntry {
        name: "accept_loop_scan_listings",
        phase: PumpPhase::Apply,
        drains: &[],
        run: super::render::pump_accept_loop_scan_listings,
    },
    DrainEntry {
        name: "poll_loop_scan_download_results",
        phase: PumpPhase::Apply,
        drains: &["loop_scan_download_receiver"],
        run: super::render::pump_poll_loop_scan_download_results,
    },
    DrainEntry {
        name: "poll_loop_l3_list_results",
        phase: PumpPhase::Apply,
        drains: &["loop_l3_list_receiver"],
        run: super::render::pump_poll_loop_l3_list_results,
    },
    DrainEntry {
        name: "poll_loop_l3_fetch_results",
        phase: PumpPhase::Apply,
        drains: &["loop_l3_fetch_receiver"],
        run: super::render::pump_poll_loop_l3_fetch_results,
    },
    DrainEntry {
        name: "poll_loop_render_results",
        phase: PumpPhase::Apply,
        drains: &["loop_render_receiver"],
        run: super::render::pump_poll_loop_render_results,
    },
    DrainEntry {
        name: "poll_loop_section_results",
        phase: PumpPhase::Apply,
        drains: &["loop_section_receiver"],
        run: super::render::pump_poll_loop_section_results,
    },
    DrainEntry {
        name: "poll_extract_results",
        phase: PumpPhase::Apply,
        drains: &[],
        run: super::render::pump_poll_extract_results,
    },
    // Results-apply before advance: a frame's last result is IN the frame that advances
    // onto it.
    DrainEntry {
        name: "advance_loop_playback",
        phase: PumpPhase::Advance,
        drains: &[],
        run: super::render::pump_advance_loop_playback,
    },
    // Advance before dispatch: the dispatchers measure a budget that is not being spent on
    // stale panes.
    DrainEntry {
        name: "dispatch_pane_renders",
        phase: PumpPhase::Dispatch,
        drains: &[],
        run: super::render::pump_dispatch_pane_renders,
    },
    DrainEntry {
        name: "dispatch_section_renders",
        phase: PumpPhase::Dispatch,
        drains: &[],
        run: super::render::pump_dispatch_section_renders,
    },
    DrainEntry {
        name: "dispatch_loop_renders",
        phase: PumpPhase::Dispatch,
        drains: &[],
        run: super::render::pump_dispatch_loop_renders,
    },
];

fn pump_poll_scan_results(app: &mut App, _ctx: Option<&egui::Context>) {
    app.poll_scan_results();
}

fn pump_poll_chunk_results(app: &mut App, _ctx: Option<&egui::Context>) {
    app.poll_chunk_results();
}

fn pump_drive_chunk_feeds(app: &mut App, _ctx: Option<&egui::Context>) {
    app.drive_chunk_feeds();
}

fn pump_poll_voxel_results(app: &mut App, _ctx: Option<&egui::Context>) {
    app.poll_voxel_results();
}

fn pump_publish_base_volumes(app: &mut App, _ctx: Option<&egui::Context>) {
    let arrived = app.publish_base_volumes();
    app.dispatch_arrived_volumes(&arrived);
}

fn pump_poll_overlay_fetch_results(app: &mut App, _ctx: Option<&egui::Context>) {
    app.poll_overlay_fetch_results();
}

impl App {
    /// Run every [`FRAME_PUMP`] row of `phase`, in table order.
    pub(super) fn run_frame_pump(&mut self, phase: PumpPhase, ctx: Option<&egui::Context>) {
        for entry in FRAME_PUMP.iter().filter(|entry| entry.phase == phase) {
            (entry.run)(self, ctx);
        }
    }
}

#[path = "frame_pump/tests.rs"]
#[cfg(test)]
mod tests;
