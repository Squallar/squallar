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

    /// An empty set has no answer at any instant — the state a layer that has
    /// listed nothing is in, which a sweep lands on first.
    #[test]
    fn a_layer_holding_no_frames_answers_none() {
        assert_eq!(newest_at_or_before(&[], t(20)), None);
    }
}
