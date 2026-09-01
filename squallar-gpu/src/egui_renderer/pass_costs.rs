//! What ending an egui pass costs the frame thread, phase by phase.
//!
//! [`super::EguiRenderer::end_pass_and_upload`] is the frame's whole
//! CPU-side prepare: tessellation, the texture-delta uploads, the optional
//! pane-mirror pass and egui's buffer staging (which also dispatches every
//! paint callback's `prepare` — the raymarch included). This ledger stamps
//! each phase and keeps running totals, so the app can say where prepare
//! time goes without this crate owning a logger (`squallar-gpu` declares no
//! `log` dependency; the sentence lives with the app the same way the
//! upload totals' does).

/// Cumulative microseconds the pass phases have cost this renderer.
///
/// **Product telemetry, not a campaign instrument.** Always on, no feature
/// gate, no debug arm: five clock reads and five `u64` adds per pass — four
/// for these totals and one more at function entry, which only
/// [`PassPhaseStamps`] reads.
/// **No figure here ever gates CI** — wall-clock totals describe a machine,
/// they do not pass or fail one.
///
/// # Denominator
///
/// **Every pass this renderer ended** — every `end_pass_and_upload` call,
/// whether or not the frame then acquired a surface and presented. A pass
/// with no mirror request contributes 0 to [`Self::mirror_us`] and is still
/// one pass.
///
/// # Why a zero here is readable
///
/// [`Self::passes`] is the non-vacuity floor, the same way
/// [`super::texture_upload::UploadTotals::deltas`] is for the byte figures:
/// `passes == 0` is a renderer that ended no pass; `passes > 0` with a phase
/// total of 0 is a phase that really cost under a microsecond per pass
/// (mirror on a frame with no 3D pane does) — or a clock that stopped, which
/// no phase-total read can tell apart without reading this one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PassCosts {
    /// Passes ended, by any outcome. What makes the phase totals readable.
    pub passes: u64,
    /// Microseconds in `Context::tessellate`.
    pub tessellate_us: u64,
    /// Microseconds filing and draining this pass's texture deltas — the
    /// frame-thread side of [`super::texture_upload`], memcpys into staging
    /// slots and any blocking `write_texture` included.
    pub upload_apply_us: u64,
    /// Microseconds in the pane-mirror pass, 0 on a pass with no request.
    pub mirror_us: u64,
    /// Microseconds in egui's `update_buffers`, which also runs every paint
    /// callback's `prepare` — the 3D raymarch's CPU-side encode is in here.
    pub buffers_and_callbacks_us: u64,
}

impl PassCosts {
    /// Every phase microsecond this ledger holds, summed. One number for a
    /// reader that wants "prepare cost" without the split.
    pub fn total_us(&self) -> u64 {
        self.tessellate_us + self.upload_apply_us + self.mirror_us + self.buffers_and_callbacks_us
    }
}

/// The instants [`super::EguiRenderer::end_pass_and_upload`] crossed, handed
/// back on the [`super::PreparedFrame`] so a caller can cut **its own**
/// prepare span at the seams this function actually has.
///
/// # Why stamps and not durations, when [`PassCosts`] already exists
///
/// [`PassCosts`] answers "what did the phases cost", cumulatively, over every
/// pass ended. It cannot answer "what else is in prepare", for two reasons
/// that are both fatal on their own:
///
/// * **Its denominator is a different set of frames.** Every pass ended,
///   presented or not, idle or interact. The app's `prepare` segment is
///   presented *interact* frames only, so the two are not subtractable and a
///   residual computed from them is not a residual of anything.
/// * **Durations can be summed but not placed.** `end_pass_and_upload` is
///   only part of the app's `prepare` span — the mirror-rung planning ahead of
///   it and `Context::end_pass` inside it are outside every figure
///   [`PassCosts`] keeps. Four durations cannot say how much of prepare they
///   failed to cover; five instants against the caller's own two bracketing
///   stamps say it exactly.
///
/// Five stamps, and with the caller's `ui_end` and acquire-start they cut
/// prepare into six spans that telescope to it exactly:
///
/// ```text
/// ui_end ─plan─> entry ─end-pass─> tessellate ─tessellate─> upload
///        ─upload─> upload_done ─mirror─> buffers ─buffers─> acquire
/// ```
///
/// The `mirror` span is `upload_done → buffers` rather than the mirror pass's
/// own bracket, so it stays a real cut of prepare whether or not a mirror was
/// requested: on a pass with none it is the cost of finding that out, which is
/// sub-microsecond and reads as the zero it is.
#[derive(Clone, Copy, Debug)]
pub struct PassPhaseStamps {
    /// Function entry, before `Context::end_pass`.
    pub entry: web_time::Instant,
    /// After `end_pass` and the platform-output handoff — tessellation opens.
    pub tessellate: web_time::Instant,
    /// After tessellation — the texture-delta apply opens.
    pub upload: web_time::Instant,
    /// After the texture-delta apply — the optional mirror pass opens.
    pub upload_done: web_time::Instant,
    /// After the mirror pass — `update_buffers` opens.
    pub buffers: web_time::Instant,
}

/// The single-writer ledger behind [`PassCosts`]: the renderer owns one and
/// notes each pass; readers take copies through the two accessors.
#[derive(Default)]
pub(super) struct PassCostLedger {
    totals: PassCosts,
    /// [`PassCosts::passes`] at the last [`Self::totals_if_moved`] answer, so
    /// an unmoved ledger costs one compare and says nothing.
    reported: u64,
}

impl PassCostLedger {
    /// Count one ended pass. Phase figures are microseconds measured by the
    /// caller, which is the only place the phases exist as spans.
    pub(super) fn note(
        &mut self,
        tessellate_us: u64,
        upload_apply_us: u64,
        mirror_us: u64,
        buffers_and_callbacks_us: u64,
    ) {
        self.totals.passes += 1;
        self.totals.tessellate_us += tessellate_us;
        self.totals.upload_apply_us += upload_apply_us;
        self.totals.mirror_us += mirror_us;
        self.totals.buffers_and_callbacks_us += buffers_and_callbacks_us;
    }

    /// The running totals, asked unconditionally.
    pub(super) fn totals(&self) -> PassCosts {
        self.totals
    }

    /// The running totals, only when a pass has ended since the last time
    /// this was asked — the same read-and-mark contract as
    /// [`super::texture_upload::TextureUploads::totals_if_moved`].
    pub(super) fn totals_if_moved(&mut self) -> Option<PassCosts> {
        if self.totals.passes == self.reported {
            return None;
        }
        self.reported = self.totals.passes;
        Some(self.totals)
    }
}

#[cfg(test)]
mod tests;
