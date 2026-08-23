//! **One time vocabulary.** How a layer relates to the clock, and how the
//! frames it can show are named.
//!
//! Every layer answers [`SourceHandler::time_axis`] with one of three
//! [`TimeAxis`] arms, and **the arm alone derives the behaviour** — there is no
//! second enum saying what to do with it. The rules live here because they are
//! the contract: a reader deciding how to present a layer reads them, and a
//! layer author picking an arm reads the same paragraph.
//!
//! [`SourceHandler::time_axis`]: crate::handler::SourceHandler::time_axis

use chrono::NaiveDateTime;

use crate::handler::{FetchConfig, FetchPayload, FetchTask, PaneRef};

/// **What a layer's data is a function of, in time.**
///
/// The derivation rules — behaviour is read off the arm, never declared
/// separately:
///
/// - [`Live`](TimeAxis::Live) — the layer draws whatever it last fetched and
///   **ignores `as_of` entirely**. Scrubbing the clock changes nothing it
///   draws, and its raster is not re-made when the depicted instant moves.
///   Station plots, the site table, city labels, the user's own position.
///
/// - [`EventLifetime`](TimeAxis::EventLifetime) — the layer holds items each
///   carrying a validity window, and the picture is *which of them are valid
///   at the depicted instant*. **As-of-dependent**: a change in
///   [`RasterizeContext::as_of`] can change the raster, so a scrubbed pane
///   re-rasterizes, and its texture is cached on the as-of **quantized** by
///   [`SourceHandler::as_of_quantum`] rather than on the raw instant. Alerts
///   (whole-minute lifetimes) and lightning (a sub-minute fade ramp).
///
/// - [`FrameSeries`](TimeAxis::FrameSeries) — the layer's data comes in
///   discrete stamped frames, and the picture is *one named frame*. Presented
///   frame-stamped: the frame shown at depicted instant `T` is the latest one
///   whose [`FrameStamp::valid`] is `<= T`, and nothing is drawn when no frame
///   qualifies. The **time-primary** layer of a pane — the one whose stamp
///   labels the pane and whose frames a scrub steps through — is the topmost
///   enabled `FrameSeries` layer in the pane's draw order.
///
/// `typical_step` is the nominal spacing between consecutive frames, for
/// sizing a scrub step before any listing has arrived; it is a hint, never a
/// guarantee that the frames are evenly spaced. `extends_future` says whether
/// stamps **after** the wall clock are expected (a forecast model), so a
/// timeline may offer a range the clock has not reached.
///
/// [`RasterizeContext::as_of`]: crate::handler::RasterizeContext::as_of
/// [`SourceHandler::as_of_quantum`]: crate::handler::SourceHandler::as_of_quantum
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimeAxis {
    /// Latest-only; `as_of` is not read.
    Live,
    /// Items with validity windows; the picture is a function of `as_of`.
    EventLifetime,
    /// Discrete stamped frames; the picture is one named frame.
    FrameSeries {
        /// Nominal spacing between consecutive frames — a hint for sizing a
        /// step, not a promise of even spacing.
        typical_step: std::time::Duration,
        /// Whether stamps later than the wall clock are expected.
        extends_future: bool,
    },
}

/// **One frame's identity in time.**
///
/// `valid` is the instant the frame **depicts**, UTC, and is the only field a
/// presentation compares against the depicted instant: the frame shown at `T`
/// is the latest whose `valid <= T`.
///
/// `run` is the model cycle (or issuance) the frame was produced by, `None`
/// for observed data that has no such notion. It is **descriptive only** —
/// never part of the `valid <= T` decision — but it separates two frames that
/// depict the same instant from different runs, which is why it is in the
/// `Hash`/`Eq` identity.
///
/// Carries **no site and no layer**: a stamp is scoped by the pane and layer it
/// was asked of. Two sites' stamps must never be pooled into one list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameStamp {
    /// The instant this frame depicts, UTC.
    pub valid: NaiveDateTime,
    /// The run/cycle that produced it; `None` for observed data.
    pub run: Option<NaiveDateTime>,
}

/// **What frames a layer can show over a window**, as a handler answers it.
///
/// `range` is the window that was asked about, echoed back so a caller holding
/// several listings can tell which question each answers. `frames` are the
/// stamps inside it, **sorted ascending by [`FrameStamp::valid`]**, and may be
/// empty.
///
/// `complete` is the honesty flag: `true` only when the handler knows the list
/// is every frame that exists in `range`. A handler answering from whatever it
/// happens to hold resident — with no archive listing to compare against —
/// says `false`, and a presentation must then read the list as "at least
/// these", never as "exactly these".
#[derive(Clone, Debug, PartialEq)]
pub struct FrameListing {
    /// The window asked about, echoed.
    pub range: (NaiveDateTime, NaiveDateTime),
    /// The stamps inside it, ascending by `valid`.
    pub frames: Vec<FrameStamp>,
    /// Whether this is known to be *every* frame in `range`.
    pub complete: bool,
}

impl FrameListing {
    /// The answer a handler with nothing to say gives: no frames, and
    /// `complete: false` because "I hold none" is not "none exist".
    pub fn empty(range: (NaiveDateTime, NaiveDateTime)) -> Self {
        Self {
            range,
            frames: Vec::new(),
            complete: false,
        }
    }
}

/// **The [`TimeAxis::FrameSeries`] rule, written once**: the newest stamp
/// whose [`FrameStamp::valid`] is at or before `t`, and `None` when none is.
///
/// `frames` must be **ascending by `valid`** — the order every
/// [`FrameListing`] and every [`FrameSource::frames_resident`] answer is
/// already in. Where several stamps share one `valid` (two runs depicting the
/// same instant) the **last** of them is answered, so a layer that files its
/// runs in arrival order answers with the newest run rather than the one it
/// happened to hear about first.
///
/// The comparison is `<=`, not `<`: a clock standing exactly on a frame's
/// stamp is showing that frame, not its predecessor.
pub fn newest_at_or_before(frames: &[FrameStamp], t: NaiveDateTime) -> Option<FrameStamp> {
    let qualifying = frames.partition_point(|stamp| stamp.valid <= t);
    frames[..qualifying].last().copied()
}

/// **One range of source time a layer must hold**, inclusive at both ends.
///
/// **Closed, not half-open, and the reason is the question being asked.** A
/// residency window is generated *by* a stop and reaches back from it, so the
/// stop sits on the window's upper edge; under `[start, end)` the one instant
/// a window would fail to cover is the very stop that produced it, which is
/// the assertion the whole type exists to make. Flipping the openness to
/// `(start, end]` moves the failure rather than removing it: a
/// [`TimeAxis::FrameSeries`] layer standing exactly on one of its own frames
/// answers a zero-width window, and an open lower edge makes that window
/// cover nothing at all.
///
/// [`Self::duration`] is `end - start` either way, so a set of closed ranges
/// measures the same total a half-open one would — what changes is only
/// whether the endpoints belong to it, and here they do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResidencyRange {
    /// The oldest instant inside the range.
    pub start: NaiveDateTime,
    /// The newest instant inside the range, **inside it**.
    pub end: NaiveDateTime,
}

impl ResidencyRange {
    /// Whether `t` is inside this range, both edges included.
    pub fn covers(&self, t: NaiveDateTime) -> bool {
        self.start <= t && t <= self.end
    }

    /// How wide this range is. Zero for a range naming a single instant,
    /// which is what a layer whose picture is a function of the instant alone
    /// answers.
    pub fn duration(&self) -> chrono::Duration {
        self.end - self.start
    }
}

/// **What a layer must hold to draw a given set of instants** — the answer to
/// [`SourceHandler::residency_for`], as a coalesced set of [`ResidencyRange`]s
/// sorted ascending and guaranteed not to overlap or touch.
///
/// **The question is a requirement, not a report.** "What would I have to be
/// holding to draw these stops" is answerable by a layer that holds nothing
/// yet, and by a layer whose storage lives above it; it is a different
/// question from [`FrameSource::frames_resident`], which reports what is in
/// hand right now.
///
/// **Why it is a set and not a span.** The bug this type was written for: a
/// twelve-hour satellite loop of thirteen hourly frames asked its lightning
/// layer for the loop's *extent*, twelve hours of archive listed and
/// downloaded object by object, when the thirteen instants it can actually
/// stop on need thirteen five-minute slices — 65 minutes in all. Reading the
/// extent off a span is what made two authorities disagree about one loop and
/// lit it on a single frame. A caller wanting the extent anyway asks
/// [`Self::extent`] and gets it from the same answer, so there is still only
/// one authority.
///
/// **Coalescing is what makes the answer cheap.** Windows are merged when
/// they overlap *or merely touch*, so a loop whose stops are closer together
/// than one layer's window collapses to the handful of ranges it really is
/// rather than one range per stop. [`Self::total`] is therefore the honest
/// cost of the ask: it never double-counts an instant two stops both need.
///
/// [`SourceHandler::residency_for`]: crate::handler::SourceHandler::residency_for
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Residency {
    ranges: Vec<ResidencyRange>,
}

impl Residency {
    /// **Nothing at all**, and for a [`TimeAxis::Live`] layer this is the
    /// *correct* answer rather than a silent degradation: such a layer draws
    /// whatever it last fetched and ignores the depicted instant entirely, so
    /// there is no slice of source time a set of stops obliges it to hold.
    ///
    /// It is also the default body of [`SourceHandler::residency_for`], which
    /// is why a conformance walk asserts every **non**-`Live` layer answers
    /// non-empty: an answer that is right for six layers must not be
    /// inheritable by the nine it is wrong for.
    ///
    /// [`SourceHandler::residency_for`]: crate::handler::SourceHandler::residency_for
    pub fn none() -> Self {
        Self { ranges: Vec::new() }
    }

    /// **Coalesce a layer's per-stop windows into the set it really needs.**
    ///
    /// Each `(start, end)` is one stop's ask, inclusive at both ends and in
    /// any order; the result is sorted and merged, with touching ranges
    /// joined as well as overlapping ones — `[0, 5]` beside `[5, 10]` is one
    /// unbroken `[0, 10]`, not two ranges sharing an instant, so
    /// [`Self::total`] counts that instant once.
    ///
    /// A window whose `end` precedes its `start` names no instant and is
    /// **dropped**; it is a caller's arithmetic error, and keeping it would
    /// let a negative duration into [`Self::total`].
    pub fn over(windows: impl IntoIterator<Item = (NaiveDateTime, NaiveDateTime)>) -> Self {
        let mut sorted: Vec<ResidencyRange> = windows
            .into_iter()
            .filter(|(start, end)| start <= end)
            .map(|(start, end)| ResidencyRange { start, end })
            .collect();
        sorted.sort();

        let mut ranges: Vec<ResidencyRange> = Vec::with_capacity(sorted.len());
        for range in sorted {
            match ranges.last_mut() {
                // `<=` and not `<`: two ranges that merely meet at an instant
                // are one range, and merging them is what keeps `total` from
                // counting that instant twice.
                Some(open) if range.start <= open.end => open.end = open.end.max(range.end),
                _ => ranges.push(range),
            }
        }
        Self { ranges }
    }

    /// The coalesced ranges, ascending and disjoint.
    pub fn ranges(&self) -> &[ResidencyRange] {
        &self.ranges
    }

    /// Whether this asks for nothing — the [`Self::none`] answer.
    ///
    /// **Not the same as asking for no *time*.** A layer whose picture is a
    /// function of the depicted instant alone answers one zero-width range
    /// per stop, which is a real ask with a [`Self::total`] of zero.
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// Whether `t` is inside any range.
    ///
    /// **The law the GLM bug becomes**: every instant a pane's clock can stop
    /// on is covered by what its layer asked to hold, or that stop draws
    /// blank.
    pub fn covers(&self, t: NaiveDateTime) -> bool {
        // The ranges are sorted and disjoint, so the only candidate is the
        // last one that opens at or before `t`.
        let opened = self.ranges.partition_point(|range| range.start <= t);
        matches!(self.ranges[..opened].last(), Some(range) if range.covers(t))
    }

    /// **How much source time this really asks for** — the sum of the
    /// coalesced ranges, which counts no instant twice.
    ///
    /// This is the figure that separates asking for a loop's frames from
    /// asking for its extent: thirteen hourly stops of a five-minute layer
    /// total 65 minutes, against an [`Self::extent`] of twelve hours.
    pub fn total(&self) -> chrono::Duration {
        self.ranges
            .iter()
            .map(ResidencyRange::duration)
            .fold(chrono::Duration::zero(), |sum, width| sum + width)
    }

    /// The outermost instants any range touches — the window a listing has to
    /// be asked over, since an archive addressed by time cannot be asked for
    /// a set. `None` when nothing is asked for.
    ///
    /// **Always at least as wide as [`Self::total`]**, and the gap between
    /// them is the archive a caller reading the extent alone would list and
    /// download for instants nothing depicts.
    pub fn extent(&self) -> Option<(NaiveDateTime, NaiveDateTime)> {
        Some((self.ranges.first()?.start, self.ranges.last()?.end))
    }
}

/// **The residency of a [`TimeAxis::FrameSeries`] layer, written once**: for
/// each stop, the frame that would be drawn there and the reach from that
/// frame's stamp up to the stop itself.
///
/// [`FrameSource::latest_at`] is what decides which frame, so this is not a
/// second derivation of `FrameSeries`'s rule — there is one `<=` in the
/// workspace and this routes through it, the same way every `latest_at`
/// routes through [`newest_at_or_before`].
///
/// **Why the range reaches forward to the stop rather than naming the stamp
/// alone.** A pane's clock stops wherever the user parks it, not only on the
/// layer's own grid; a stop at 07:59 is drawn from the 07:00 granule, and a
/// residency of `[07:00, 07:00]` would leave 07:59 uncovered by the very
/// answer that named the frame it draws from. Where a stop lands exactly on a
/// stamp the range is a single instant, and thirteen hourly stops of an
/// hourly layer total zero seconds — the layer needs thirteen frames and none
/// of the time between them, which is the whole point.
///
/// A stop with no qualifying frame contributes **nothing**: the layer is not
/// asking to hold anything for an instant it would draw blank at, and saying
/// so is different from asking for the gap.
pub fn frame_residency(
    frames: &dyn FrameSource,
    pane: &PaneRef<'_>,
    stops: &[NaiveDateTime],
) -> Residency {
    Residency::over(
        stops
            .iter()
            .filter_map(|&stop| Some((frames.latest_at(pane, stop)?.valid, stop))),
    )
}

/// **The frame supply half of a [`TimeAxis::FrameSeries`] layer** — every
/// question a caller may ask about *what this layer can draw when*, and every
/// door a frame's bytes travel through.
///
/// Reached through [`SourceHandler::frames`] / [`SourceHandler::frames_mut`],
/// the accessor pattern [`SourceHandler::volume`] already uses: a layer that
/// does not come in stamped frames answers `None` there and writes none of
/// this, rather than inheriting nine trivial bodies it never meant.
///
/// **Nothing here has a default body, and that is the whole point.** Four
/// paired obligations exist between these methods, every one of them silent
/// under a surface where each half could be inherited alone:
///
/// - [`create_frame_list_task`](Self::create_frame_list_task) without
///   [`apply_frame_listing`](Self::apply_frame_listing) — the listing is
///   fetched and paid for, and the handler learns nothing from it.
/// - [`fetch_frame`](Self::fetch_frame) without
///   [`apply_frame`](Self::apply_frame) — the same, one frame's bytes at a
///   time.
/// - [`TimeAxis::FrameSeries`] declared with **no supply at all**: a layer
///   that says its picture is one named frame and can name none.
/// - [`TimeAxis::FrameSeries::extends_future`] without
///   [`frame_horizon`](Self::frame_horizon) — a rail told to reach past the
///   wall clock and given no distance to reach.
///
/// A layer that genuinely has nothing to do for one of these still writes the
/// body, and the body says why. That is the difference this trait buys: an
/// empty answer that was **decided** reads differently from one that was
/// inherited, and only one of the two can be reviewed.
///
/// Every method is pane-scoped for the reason [`FrameStamp`] carries no site:
/// two panes on two sites hold two frame sets, and an unscoped answer would
/// pool them into one bogus list.
///
/// [`SourceHandler::frames`]: crate::handler::SourceHandler::frames
/// [`SourceHandler::frames_mut`]: crate::handler::SourceHandler::frames_mut
/// [`SourceHandler::volume`]: crate::handler::SourceHandler::volume
pub trait FrameSource {
    /// **What this layer would draw at `t`** — the newest stamp it knows of
    /// whose [`FrameStamp::valid`] is at or before `t`, and `None` when none
    /// is.
    ///
    /// [`TimeAxis::FrameSeries`]'s stated rule, asked of the layer that owns
    /// the frames instead of reconstructed above it. [`newest_at_or_before`]
    /// is that rule as a function, and every implementation routes through it
    /// so there is one expression of `<=` in the workspace to get wrong.
    ///
    /// **No window.** The question is about an instant, not a range, which is
    /// what makes it immune to the defect a windowed listing has: clipping the
    /// answer at the window's own start drops the frame every stop in the
    /// window's leading partial step is drawn from.
    ///
    /// [`FrameStamp::run`] is **not** part of the `valid <= t` decision — see
    /// its own doc — but it is **carried on the answer**, because without it a
    /// forecast frame cannot be told from another run's frame depicting the
    /// same instant.
    ///
    /// **A peer, not the authority.** A presentation holding its own frame
    /// list keeps deciding from that list; this must agree with it, and
    /// nothing above is required to prefer it. That is deliberate while one
    /// layer's frame storage still sits above its handler.
    fn latest_at(&self, pane: &PaneRef<'_>, t: NaiveDateTime) -> Option<FrameStamp>;

    /// **What frames this layer could show over `range`, as it already knows
    /// it.** A synchronous query over handler-owned state — it **never**
    /// performs I/O; [`Self::create_frame_list_task`] is the fetch that fills
    /// that state in.
    fn list_frames(
        &self,
        ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        range: (NaiveDateTime, NaiveDateTime),
    ) -> FrameListing;

    /// **The async supply for [`Self::list_frames`]** — the listing fetch that
    /// teaches this handler what frames exist. Lands as
    /// [`SourceEvent::Frames`](crate::handler::SourceEvent::Frames); `None`
    /// from a layer with no listing to fetch.
    ///
    /// Build the task through
    /// [`FrameListingResult::task`](crate::handler::FrameListingResult::task):
    /// the scope this listing will be filed under is **captured here, at
    /// dispatch**, and travels with the round trip rather than being read back
    /// off a pane that may have moved by the time it lands.
    fn create_frame_list_task(
        &self,
        ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        range: (NaiveDateTime, NaiveDateTime),
    ) -> Option<FetchTask>;

    /// **The async supply for one frame's data.** Lands as
    /// [`SourceEvent::FrameReady`](crate::handler::SourceEvent::FrameReady);
    /// `None` when this handler cannot fetch that stamp (or already holds it).
    fn fetch_frame(
        &self,
        ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        stamp: &FrameStamp,
    ) -> Option<FetchTask>;

    /// The stamps this handler is **holding data for** in this pane, ready to
    /// draw without a fetch, **ascending by `valid`**. A subset of what
    /// [`Self::list_frames`] names.
    ///
    /// The frame cache is the **handler's own**: nothing above keeps a
    /// parallel map of frames, so nothing above can disagree with this answer
    /// — for every layer whose storage really is below this trait. A layer
    /// whose storage is above it answers empty and **says so in its own
    /// body**, which is a reviewable claim rather than an inherited silence.
    fn frames_resident(&self, pane: &PaneRef<'_>) -> Vec<FrameStamp>;

    /// Drop every resident frame **not** in `keep`. The one eviction door, so
    /// the budget that decides what to keep can live above while the storage
    /// stays below.
    fn retain_frames(&mut self, pane: &PaneRef<'_>, keep: &[FrameStamp]);

    /// **Take delivery of a frame listing this handler asked for**, with the
    /// `scope` its own [`Self::create_frame_list_task`] captured at dispatch.
    ///
    /// The scope is this handler's own type and nothing above reads it. It is
    /// how the listing is *filed*: `listing` names no site, so a handler two
    /// panes on two sites both ask of would otherwise pool two sites' stamps
    /// into one list.
    fn apply_frame_listing(
        &mut self,
        listing: FrameListing,
        scope: FetchPayload,
        pane: &PaneRef<'_>,
    );

    /// Take delivery of one frame's data.
    fn apply_frame(&mut self, stamp: FrameStamp, data: FetchPayload, pane: &PaneRef<'_>);

    /// **How far past the wall clock this layer's frames reach, in this pane.**
    ///
    /// Zero for every layer whose stamps are all history — which is every
    /// layer whose [`time_axis`] does not declare `extends_future`, and also a
    /// forecast layer whose current selection happens to name a past set. A
    /// caller reads the axis to decide whether the rail reaches forward at
    /// all, and reads this to decide how far.
    ///
    /// **Pane-scoped because the horizon belongs to the run, not to the
    /// layer**: the same HRRR layer reaches 48 hours off a 00/06/12/18Z cycle
    /// and 18 off every other hour, so nothing above can hold this as a
    /// constant beside the id.
    ///
    /// An upper bound on the range a loop should ask for, not a promise of a
    /// frame at the end of it — [`Self::create_frame_list_task`] clips its own
    /// stamps to the range it is handed.
    ///
    /// [`time_axis`]: crate::handler::SourceHandler::time_axis
    fn frame_horizon(&self, pane: &PaneRef<'_>) -> chrono::Duration;
}

/// **Acceptance: `a_frame_source_cannot_be_declared_half_implemented`.**
///
/// A [`FrameSource`] impl missing one method — `apply_frame`, the half of the
/// `fetch_frame`/`apply_frame` pair that costs a round trip to omit — does not
/// compile. The compile-fail check hangs off a named item rather than off the
/// trait itself so the acceptance carries the name the work order gave it in
/// doc-test output, instead of only a file and a line.
///
/// ```compile_fail
/// use chrono::NaiveDateTime;
/// use rustdar_source::handler::{FetchConfig, FetchPayload, FetchTask, PaneRef};
/// use rustdar_source::time::{FrameListing, FrameSource, FrameStamp};
///
/// struct HalfDeclared;
///
/// impl FrameSource for HalfDeclared {
///     fn latest_at(&self, _pane: &PaneRef<'_>, _t: NaiveDateTime) -> Option<FrameStamp> {
///         None
///     }
///     fn list_frames(
///         &self,
///         _ctx: &FetchConfig,
///         _pane: &PaneRef<'_>,
///         range: (NaiveDateTime, NaiveDateTime),
///     ) -> FrameListing {
///         FrameListing::empty(range)
///     }
///     fn create_frame_list_task(
///         &self,
///         _ctx: &FetchConfig,
///         _pane: &PaneRef<'_>,
///         _range: (NaiveDateTime, NaiveDateTime),
///     ) -> Option<FetchTask> {
///         None
///     }
///     fn fetch_frame(
///         &self,
///         _ctx: &FetchConfig,
///         _pane: &PaneRef<'_>,
///         _stamp: &FrameStamp,
///     ) -> Option<FetchTask> {
///         None
///     }
///     fn frames_resident(&self, _pane: &PaneRef<'_>) -> Vec<FrameStamp> {
///         Vec::new()
///     }
///     fn retain_frames(&mut self, _pane: &PaneRef<'_>, _keep: &[FrameStamp]) {}
///     fn apply_frame_listing(
///         &mut self,
///         _listing: FrameListing,
///         _scope: FetchPayload,
///         _pane: &PaneRef<'_>,
///     ) {
///     }
///     // `apply_frame` is deliberately absent: its bytes would be fetched,
///     // paid for and dropped on the floor.
///     fn frame_horizon(&self, _pane: &PaneRef<'_>) -> chrono::Duration {
///         chrono::Duration::zero()
///     }
/// }
/// ```
///
/// **Floor** — the identical fixture **with** `apply_frame` compiles and is a
/// usable `dyn FrameSource`, so the check above is about the missing method
/// and not about the fixture:
///
/// ```
/// use chrono::NaiveDateTime;
/// use rustdar_source::handler::{FetchConfig, FetchPayload, FetchTask, PaneRef};
/// use rustdar_source::time::{FrameListing, FrameSource, FrameStamp};
///
/// struct WholeDeclared;
///
/// impl FrameSource for WholeDeclared {
///     fn latest_at(&self, _pane: &PaneRef<'_>, _t: NaiveDateTime) -> Option<FrameStamp> {
///         None
///     }
///     fn list_frames(
///         &self,
///         _ctx: &FetchConfig,
///         _pane: &PaneRef<'_>,
///         range: (NaiveDateTime, NaiveDateTime),
///     ) -> FrameListing {
///         FrameListing::empty(range)
///     }
///     fn create_frame_list_task(
///         &self,
///         _ctx: &FetchConfig,
///         _pane: &PaneRef<'_>,
///         _range: (NaiveDateTime, NaiveDateTime),
///     ) -> Option<FetchTask> {
///         None
///     }
///     fn fetch_frame(
///         &self,
///         _ctx: &FetchConfig,
///         _pane: &PaneRef<'_>,
///         _stamp: &FrameStamp,
///     ) -> Option<FetchTask> {
///         None
///     }
///     fn frames_resident(&self, _pane: &PaneRef<'_>) -> Vec<FrameStamp> {
///         Vec::new()
///     }
///     fn retain_frames(&mut self, _pane: &PaneRef<'_>, _keep: &[FrameStamp]) {}
///     fn apply_frame_listing(
///         &mut self,
///         _listing: FrameListing,
///         _scope: FetchPayload,
///         _pane: &PaneRef<'_>,
///     ) {
///     }
///     fn apply_frame(&mut self, _stamp: FrameStamp, _data: FetchPayload, _pane: &PaneRef<'_>) {}
///     fn frame_horizon(&self, _pane: &PaneRef<'_>) -> chrono::Duration {
///         chrono::Duration::zero()
///     }
/// }
///
/// fn takes_one(_: &dyn FrameSource) {}
/// takes_one(&WholeDeclared);
/// ```
#[doc(hidden)]
pub fn a_frame_source_cannot_be_declared_half_implemented() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(minute: u32) -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 8, 22)
            .expect("a real date")
            .and_hms_opt(12, minute, 0)
            .expect("a real time")
    }

    fn observed(minute: u32) -> FrameStamp {
        FrameStamp {
            valid: t(minute),
            run: None,
        }
    }

    /// **The rule at its three edges**, over a known listing: before every
    /// stamp is `None`, after every stamp is the newest, and *on* a stamp is
    /// that stamp.
    #[test]
    fn newest_at_or_before_answers_the_frame_series_rule() {
        let frames = [observed(10), observed(20), observed(30)];

        assert_eq!(
            newest_at_or_before(&frames, t(5)),
            None,
            "an instant before every stamp has no frame to draw",
        );
        assert_eq!(
            newest_at_or_before(&frames, t(35)),
            Some(observed(30)),
            "an instant after every stamp draws the newest",
        );
        // The floor: exactly ON a stamp answers THAT stamp, never its
        // predecessor. An off-by-one in the partition point is the likely
        // defect and only this edge sees it.
        for minute in [10, 20, 30] {
            assert_eq!(
                newest_at_or_before(&frames, t(minute)),
                Some(observed(minute)),
                "the clock standing exactly on :{minute:02} draws that frame, not the one before",
            );
        }
        assert_eq!(
            newest_at_or_before(&frames, t(19)),
            Some(observed(10)),
            "an instant between two stamps carries the earlier one forward",
        );
    }

    /// `run` never decides, and is never dropped either: two runs depicting
    /// one instant are one `valid` with two identities, and the answer must
    /// still say which.
    #[test]
    fn the_answer_carries_the_run_that_produced_it() {
        let earlier_run = FrameStamp {
            valid: t(20),
            run: Some(t(0)),
        };
        let later_run = FrameStamp {
            valid: t(20),
            run: Some(t(12)),
        };
        let frames = [observed(10), earlier_run, later_run];

        assert_eq!(
            newest_at_or_before(&frames, t(25)),
            Some(later_run),
            "the newest stamp at :20 is the last one filed for it, and it carries its run",
        );
        assert_ne!(
            newest_at_or_before(&frames, t(25)),
            Some(observed(20)),
            "an answer that dropped `run` cannot be told from another run's frame",
        );
    }

    fn hour(k: i64) -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 8, 22)
            .expect("a real date")
            .and_hms_opt(0, 0, 0)
            .expect("a real time")
            + chrono::Duration::hours(k)
    }

    /// The GLM default, and the figure the 65 minutes below is thirteen of.
    const LIGHTNING_WINDOW_SECS: i64 = 300;

    /// **Acceptance: `a_coarse_loop_asks_for_its_frames_not_its_extent`.**
    ///
    /// Thirteen hourly stops, each needing the five minutes of archive behind
    /// it — the shape a twelve-hour satellite loop hands the lightning layer.
    /// The answer is **65 minutes**, thirteen five-minute windows; the extent
    /// those windows are scattered across is twelve hours and five minutes,
    /// and asking for the extent is what would list and download ~8 600
    /// objects per satellite for a loop that draws thirteen pictures.
    ///
    /// The assertion is on the **total covered duration**, not on the range
    /// count: a coalescer that merged everything into one span would still
    /// report one range, and one range is not a wrong count, it is a wrong
    /// answer.
    #[test]
    fn a_coarse_loop_asks_for_its_frames_not_its_extent() {
        let window = chrono::Duration::seconds(LIGHTNING_WINDOW_SECS);
        let stops: Vec<NaiveDateTime> = (0..13).map(hour).collect();
        let residency = Residency::over(stops.iter().map(|&stop| (stop - window, stop)));

        assert_eq!(
            residency.total(),
            chrono::Duration::seconds(13 * LIGHTNING_WINDOW_SECS),
            "thirteen hourly stops of a five-minute layer are 65 minutes of \
             archive, not the twelve hours they are spread over",
        );
        assert_eq!(
            residency.total(),
            chrono::Duration::minutes(65),
            "the figure, spelled the way the work order spells it",
        );
        assert_eq!(
            residency.ranges().len(),
            13,
            "the stops are an hour apart and the windows five minutes wide, \
             so none of them touch and none may be merged",
        );

        let (from, to) = residency
            .extent()
            .expect("a non-empty answer has an extent");
        assert_eq!(
            (from, to),
            (hour(0) - window, hour(12)),
            "the extent is still available from the same answer — one \
             authority, two readings",
        );
        assert_eq!(
            to - from,
            chrono::Duration::hours(12) + window,
            "and it is 12 h 05 min wide: the quantity a caller reading the \
             span alone would have asked the archive for",
        );
        assert!(
            residency.total() * 11 < to - from,
            "the ask is more than an order of magnitude under its own extent, \
             which is the whole difference between the two questions",
        );

        // The law WO-T2.3 becomes: every stop the pane can make is inside
        // what the layer asked to hold. The upper edge of each window IS its
        // stop, which is why the ranges are closed there.
        for stop in &stops {
            assert!(
                residency.covers(*stop),
                "the stop at {stop} is the instant its own window was built \
                 from and must be inside it",
            );
        }
        assert!(
            !residency.covers(hour(0) + chrono::Duration::minutes(30)),
            "the archive halfway between two stops is depicted by nothing, \
             and a residency that covered it would be the extent again",
        );
    }

    /// **Floor for the acceptance above** — the same call with **one** stop
    /// yields **one** window of exactly the layer's own width.
    ///
    /// Without it, thirteen ranges totalling 65 minutes could as easily be a
    /// fixture that happens to be thirteen unmergeable pieces as a coalescer
    /// doing its job. Here the count tracks the stops and the width tracks
    /// the layer.
    #[test]
    fn one_stop_asks_for_one_window() {
        let window = chrono::Duration::seconds(LIGHTNING_WINDOW_SECS);
        let residency = Residency::over([(hour(6) - window, hour(6))]);

        assert_eq!(residency.ranges().len(), 1);
        assert_eq!(
            residency.total(),
            window,
            "one stop costs one window, so thirteen of them costing thirteen \
             is the stops and not the arithmetic",
        );
        assert_eq!(
            residency.extent(),
            Some((hour(6) - window, hour(6))),
            "with one range the extent and the ask are the same quantity",
        );
    }

    /// **Coalescing is load-bearing, and this is where it fires.** Stops
    /// closer together than the layer's own window overlap, and the answer is
    /// the unbroken stretch they cover rather than one range per stop.
    ///
    /// The floor above proves the count follows the stops; this proves it
    /// does not *only* follow the stops. Both are needed: a coalescer that
    /// never merges passes the first, and one that always merges passes
    /// neither.
    #[test]
    fn overlapping_and_touching_windows_become_one_range() {
        let window = chrono::Duration::seconds(LIGHTNING_WINDOW_SECS);
        // Stops every four minutes: each window reaches a minute behind the
        // previous stop's, so all four overlap.
        let stops: Vec<NaiveDateTime> = (0..4)
            .map(|k| hour(0) + chrono::Duration::minutes(4 * k))
            .collect();
        let overlapping = Residency::over(stops.iter().map(|&stop| (stop - window, stop)));

        assert_eq!(
            overlapping.ranges().len(),
            1,
            "four overlapping windows are one stretch of archive: {:?}",
            overlapping.ranges(),
        );
        assert_eq!(
            overlapping.total(),
            window + chrono::Duration::minutes(12),
            "and it costs the stretch once — 5 min behind the first stop plus \
             the 12 min the stops span — not four times five",
        );

        // Merely *touching* merges too, or the instant they share would be
        // counted twice by `total` and the set would carry a seam that is not
        // in the archive.
        let touching = Residency::over([(hour(0), hour(1)), (hour(1), hour(2))]);
        assert_eq!(
            touching.ranges(),
            [ResidencyRange {
                start: hour(0),
                end: hour(2),
            }],
            "two ranges meeting at an instant are one range",
        );
        assert_eq!(touching.total(), chrono::Duration::hours(2));
    }

    /// The answer is sorted and disjoint however the windows arrive, and a
    /// window wholly inside another does not shorten the one it is inside.
    #[test]
    fn the_answer_is_sorted_and_disjoint_whatever_order_it_was_built_in() {
        let residency = Residency::over([
            (hour(5), hour(6)),
            (hour(0), hour(4)),
            // Wholly inside the range above: `end.max` is what keeps the
            // outer range's own end, and a plain assignment would truncate it.
            (
                hour(5) + chrono::Duration::minutes(10),
                hour(5) + chrono::Duration::minutes(20),
            ),
        ]);

        assert_eq!(
            residency.ranges(),
            [
                ResidencyRange {
                    start: hour(0),
                    end: hour(4),
                },
                ResidencyRange {
                    start: hour(5),
                    end: hour(6),
                },
            ],
        );
        assert_eq!(residency.total(), chrono::Duration::hours(5));
        assert!(residency.covers(hour(5) + chrono::Duration::minutes(15)));
        assert!(
            !residency.covers(hour(4) + chrono::Duration::minutes(30)),
            "the gap between two ranges is not covered, or `covers` is not a \
             predicate at all",
        );
    }

    /// The empty answer: it covers nothing, has no extent, and asks for no
    /// time. Read by every `Live` layer, where it is correct.
    #[test]
    fn the_empty_residency_covers_nothing() {
        let none = Residency::none();
        assert!(none.is_empty());
        assert_eq!(none.extent(), None);
        assert_eq!(none.total(), chrono::Duration::zero());
        assert!(!none.covers(hour(3)));
        assert_eq!(none, Residency::over([]));
    }

    /// A zero-width range names one instant and covers exactly it — the
    /// answer a layer whose picture is a function of the instant alone gives,
    /// and the answer a framed layer gives for a stop standing on one of its
    /// own frames. **Asking for no time is not asking for nothing.**
    #[test]
    fn a_zero_width_range_is_an_ask_with_no_duration() {
        let residency = Residency::over([(hour(3), hour(3))]);

        assert!(!residency.is_empty(), "one instant is still an ask");
        assert_eq!(residency.total(), chrono::Duration::zero());
        assert!(residency.covers(hour(3)));
        assert!(!residency.covers(hour(3) + chrono::Duration::seconds(1)));
    }

    /// A window whose end precedes its start names no instant and is dropped
    /// rather than allowed to subtract from [`Residency::total`].
    #[test]
    fn an_inverted_window_is_dropped() {
        let residency = Residency::over([(hour(6), hour(3)), (hour(0), hour(1))]);

        assert_eq!(
            residency.ranges(),
            [ResidencyRange {
                start: hour(0),
                end: hour(1),
            }],
        );
        assert_eq!(residency.total(), chrono::Duration::hours(1));
    }

    /// A [`FrameSource`] holding a known listing, for [`frame_residency`].
    struct StubFrames(Vec<FrameStamp>);

    impl FrameSource for StubFrames {
        fn latest_at(&self, _pane: &PaneRef<'_>, t: NaiveDateTime) -> Option<FrameStamp> {
            newest_at_or_before(&self.0, t)
        }
        fn list_frames(
            &self,
            _ctx: &FetchConfig,
            _pane: &PaneRef<'_>,
            range: (NaiveDateTime, NaiveDateTime),
        ) -> FrameListing {
            FrameListing::empty(range)
        }
        fn create_frame_list_task(
            &self,
            _ctx: &FetchConfig,
            _pane: &PaneRef<'_>,
            _range: (NaiveDateTime, NaiveDateTime),
        ) -> Option<FetchTask> {
            None
        }
        fn fetch_frame(
            &self,
            _ctx: &FetchConfig,
            _pane: &PaneRef<'_>,
            _stamp: &FrameStamp,
        ) -> Option<FetchTask> {
            None
        }
        fn frames_resident(&self, _pane: &PaneRef<'_>) -> Vec<FrameStamp> {
            self.0.clone()
        }
        fn retain_frames(&mut self, _pane: &PaneRef<'_>, _keep: &[FrameStamp]) {}
        fn apply_frame_listing(
            &mut self,
            _listing: FrameListing,
            _scope: FetchPayload,
            _pane: &PaneRef<'_>,
        ) {
        }
        fn apply_frame(&mut self, _stamp: FrameStamp, _data: FetchPayload, _pane: &PaneRef<'_>) {}
        fn frame_horizon(&self, _pane: &PaneRef<'_>) -> chrono::Duration {
            chrono::Duration::zero()
        }
    }

    fn stub_frames(count: i64) -> StubFrames {
        StubFrames(
            (0..count)
                .map(|k| FrameStamp {
                    valid: hour(k),
                    run: None,
                })
                .collect(),
        )
    }

    /// **A framed layer asks for its frames, and for nothing between them.**
    ///
    /// Thirteen hourly frames and thirteen stops standing exactly on them:
    /// thirteen single-instant ranges, **zero** seconds of source time in
    /// all. The layer needs thirteen granules; the twelve hours they are
    /// spread across is archive it draws nothing from.
    #[test]
    fn a_framed_layer_asks_for_its_stamps_and_not_the_hours_between_them() {
        let frames = stub_frames(13);
        let pane = PaneRef::bare(0);
        let stops: Vec<NaiveDateTime> = (0..13).map(hour).collect();

        let residency = frame_residency(&frames, &pane, &stops);

        assert_eq!(residency.ranges().len(), 13);
        assert_eq!(
            residency.total(),
            chrono::Duration::zero(),
            "a stop standing on its own frame needs that frame and no time \
             around it",
        );
        assert_eq!(
            residency.extent(),
            Some((hour(0), hour(12))),
            "the extent is the twelve hours, and the ask is none of it",
        );
        for stop in &stops {
            assert!(residency.covers(*stop), "the stop at {stop} draws a frame");
        }
    }

    /// A stop **between** two frames is drawn by carrying the earlier one
    /// forward, so the range reaches from that stamp up to the stop — and the
    /// stop is inside its own answer.
    ///
    /// This is why the range is not the stamp alone: a pane's clock parks
    /// wherever the user leaves it, and `[07:00, 07:00]` would not cover
    /// 07:59.
    #[test]
    fn a_stop_between_frames_reaches_back_to_the_frame_it_draws() {
        let frames = stub_frames(3);
        let pane = PaneRef::bare(0);
        let parked = hour(1) + chrono::Duration::minutes(59);

        let residency = frame_residency(&frames, &pane, &[parked]);

        assert_eq!(
            residency.ranges(),
            [ResidencyRange {
                start: hour(1),
                end: parked,
            }],
        );
        assert!(residency.covers(parked), "the parked instant is covered");
        assert!(residency.covers(hour(1)), "so is the frame it draws");
    }

    /// A stop with no qualifying frame contributes **nothing** — the layer is
    /// not asking to hold archive for an instant it would draw blank at.
    ///
    /// The floor for the two tests above: a `frame_residency` that answered
    /// one range per stop regardless would pass both of them.
    #[test]
    fn a_stop_before_every_frame_asks_for_nothing() {
        let frames = StubFrames(vec![FrameStamp {
            valid: hour(5),
            run: None,
        }]);
        let pane = PaneRef::bare(0);

        let residency = frame_residency(&frames, &pane, &[hour(1), hour(2)]);
        assert!(
            residency.is_empty(),
            "nothing depicts hour 1 or hour 2, so nothing is asked for: {:?}",
            residency.ranges(),
        );

        // Premise: the same layer does answer for a stop it can draw, so the
        // empty above is about the instants and not about the fixture.
        assert!(!frame_residency(&frames, &pane, &[hour(6)]).is_empty());
    }

    /// An empty set has no answer at any instant — the state a layer that has
    /// listed nothing is in, which a sweep lands on first.
    #[test]
    fn a_layer_holding_no_frames_answers_none() {
        assert_eq!(newest_at_or_before(&[], t(20)), None);
    }
}
