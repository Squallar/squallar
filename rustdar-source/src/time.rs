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
