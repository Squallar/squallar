//! **The fake source: a layer whose only job is to prove the seam.**
//!
//! Compiled only under this crate's `fake-source` feature, which nothing that
//! ships ever turns on. It exists so a test build can register a **thirteenth**
//! layer that no consumer has an arm for, and watch the UI draw it: its
//! controls reachable through the parity walk, its field in the catalogue, its
//! legend from the registry, its frames on the timeline.
//!
//! **Why a cargo feature and not `cfg(test)`.** `cfg(test)` is crate-local: a
//! fake behind it would be invisible to `rustdar-egui`'s and `rustdar-app`'s
//! test builds, which are exactly the crates whose product-blindness it exists
//! to demonstrate. Never "simplify" this to `cfg(test)`.
//!
//! **Nothing here reaches the network or a clock of its own.** Frames come off
//! a fixed epoch-aligned grid and their data is synthesised in memory, so the
//! layer answers the same thing on every machine and in every run.

use std::collections::BTreeSet;
use std::sync::{Arc, LazyLock};

use chrono::NaiveDateTime;
use rustdar_geo::GeoBounds;
use rustdar_source::id::{LayerId, known};
use rustdar_source::job::{DescribedJob, JobCodec};
use rustdar_source::product::{FieldId, LegendScale, ProductSpec};
use rustdar_source::time::{FrameListing, FrameStamp, TimeAxis};
use rustdar_units::Quantity;

use crate::render::controls::{ControlEffect, ControlItem, ControlUpdate, ControlValue};
use crate::render::overlay_state::{
    FetchConfig, FetchPayload, FetchTask, OverlayHandler, OverlayItem, PaneMut, PaneRef,
    RasterizeContext, RenderMode, Surface,
};
use crate::render::rasterize::{AlphaMode, RasterizeOutput};

/// The gap between two fake frames, in seconds.
///
/// **Deliberately alien**: seven minutes is neither radar's ~five-minute volume
/// cadence nor the model's hour, so a timeline that has quietly hardcoded
/// either one draws this layer wrong in a way a test can see.
pub const FRAME_STEP_SECS: i64 = 420;

/// The group label this source files its field under.
pub const GROUP: &str = "Fake";

/// The one field this source publishes.
pub const FIELD_ID: &str = "FakeField";

/// The dropdown this layer offers beside its toggle.
pub const TINT_CONTROL: &str = "tint";
pub const TINT_LABEL: &str = "Fake tint";
pub const ENABLED_CONTROL: &str = "enabled";

/// Which of the two synthetic colour ramps a pane draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeTint {
    Warm,
    Cool,
}

impl FakeTint {
    /// The persisted spelling — the bytes a saved config holds.
    pub fn as_str(self) -> &'static str {
        match self {
            FakeTint::Warm => "warm",
            FakeTint::Cool => "cool",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FakeTint::Warm => "Warm",
            FakeTint::Cool => "Cool",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "warm" => Some(FakeTint::Warm),
            "cool" => Some(FakeTint::Cool),
            _ => None,
        }
    }

    /// The end colour the ramp runs to. The start is always black.
    fn end_rgb(self) -> [u8; 3] {
        match self {
            FakeTint::Warm => [255, 96, 0],
            FakeTint::Cool => [0, 96, 255],
        }
    }
}

/// The fake field's colour bar, built once because [`ProductSpec::scale`] is a
/// borrow.
static SCALE: LazyLock<LegendScale> = LazyLock::new(|| LegendScale {
    thresholds: vec![
        (0.0, [0, 0, 0]),
        (25.0, [64, 64, 64]),
        (50.0, [128, 128, 128]),
        (75.0, [192, 192, 192]),
        (100.0, [255, 255, 255]),
    ],
    is_gradient: true,
    min_value: 0.0,
    max_value: 100.0,
});

/// The one registration, with **all eleven facts stated** — the no-`Default`
/// doctrine applies to a fake exactly as it applies to a real source, because
/// a fake that could shrug off a field would not prove the contract it exists
/// to prove.
static FIELDS: LazyLock<[ProductSpec; 1]> = LazyLock::new(|| {
    [ProductSpec {
        id: FieldId::from_static(FIELD_ID),
        name: "Fake Field",
        code: "fake",
        sort_order: 0,
        group: GROUP,
        quantity: Quantity::Unitless { label: "fu" },
        scale: &SCALE,
        value_domain: (SCALE.min_value, SCALE.max_value),
        domain_label_ends: ("\u{2265}", "fu"),
        // No vertical extent: the fake never reaches an isosurface slider, and
        // saying otherwise would offer a 3D editor over a field with no volume.
        vertical: false,
        tilted: false,
    }]
});

/// The fake source's one field.
pub fn products() -> &'static [ProductSpec] {
    &*FIELDS
}

/// Every fake frame stamp inside `range`, on the epoch-aligned
/// [`FRAME_STEP_SECS`] grid, ascending.
///
/// A pure function of the window: two calls with one window agree, on any
/// machine, without a clock or a network.
pub fn frames_in(range: (NaiveDateTime, NaiveDateTime)) -> Vec<FrameStamp> {
    let (from, to) = range;
    if to < from {
        return Vec::new();
    }
    let start = from.and_utc().timestamp();
    let end = to.and_utc().timestamp();
    // Round the window's start UP to the grid, so the list is every grid point
    // inside the window rather than one before it.
    let first = start.div_euclid(FRAME_STEP_SECS) * FRAME_STEP_SECS;
    let first = if first < start {
        first + FRAME_STEP_SECS
    } else {
        first
    };
    let mut out = Vec::new();
    let mut t = first;
    while t <= end {
        if let Some(valid) = chrono::DateTime::from_timestamp(t, 0) {
            out.push(FrameStamp {
                valid: valid.naive_utc(),
                run: None,
            });
        }
        t += FRAME_STEP_SECS;
    }
    out
}

/// One fake frame's synthetic data: the stamp it depicts and a level derived
/// from it, so two frames differ and the difference is a function of the stamp.
#[derive(Debug, Clone, PartialEq)]
pub struct FakeFrameData {
    pub valid: NaiveDateTime,
    pub level: f32,
}

impl FakeFrameData {
    /// Deterministic from the stamp alone — no clock, no RNG.
    pub fn for_stamp(valid: NaiveDateTime) -> Self {
        let step = valid.and_utc().timestamp().div_euclid(FRAME_STEP_SECS);
        Self {
            valid,
            level: (step.rem_euclid(101)) as f32,
        }
    }
}

/// The described input the fake's raster is built from.
#[derive(Debug, Clone, PartialEq)]
pub struct FakeInput {
    pub tint: FakeTint,
    pub level: f32,
    pub device_scale: f32,
}

rustdar_source::impl_job_input!(FakeInput);

/// The fake's raster: a horizontal ramp from black to the tint's end colour,
/// scaled by `level`.
///
/// **Premultiplied, and translucent.** Alpha ramps across the raster so the
/// fixture floor next door (`every_fixture_draws_pixels_the_two_conventions_
/// disagree_about`) has something to discriminate; every colour channel is
/// written already multiplied by that alpha, so no channel can exceed it and
/// the declaration matches the bytes. Because the bytes are premultiplied the
/// run funnel does not touch them, which is what lets the through-the-worker
/// output equal the direct call's **field for field** -- the equality the
/// parity test beside this asserts.
///
/// A pure function of `(input, w, h)` — `bounds` is accepted for symmetry with
/// the other rasterizers and deliberately unread, so the direct call and the
/// through-the-worker call cannot differ by a geometry the wire rounds.
pub fn rasterize_fake(input: &FakeInput, bounds: &GeoBounds, w: u32, h: u32) -> RasterizeOutput {
    let _ = bounds;
    let end = input.tint.end_rgb();
    let scale = (input.level / 100.0).clamp(0.0, 1.0);
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let t = if w > 1 {
                x as f32 / (w - 1) as f32
            } else {
                0.0
            };
            // 40..=240: never zero (so every pixel is "drawn") and never 255
            // (so every pixel is one the two alpha conventions disagree about).
            let alpha = 40.0 + 200.0 * t;
            let px = (y * w as usize + x) * 4;
            for (c, channel) in end.iter().enumerate() {
                rgba[px + c] = (f32::from(*channel) / 255.0 * scale * alpha) as u8;
            }
            rgba[px + 3] = alpha as u8;
        }
    }
    RasterizeOutput {
        rgba,
        hit_cells: None,
        alpha: AlphaMode::Premultiplied,
    }
}

/// What this pane has chosen of the fake layer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FakePaneState {
    pub enabled: bool,
    pub tint: FakeTint,
}

impl FakePaneState {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            tint: FakeTint::Warm,
        }
    }
}

pub(crate) struct FakeSourceHandler {
    /// The registry's own copy, for a caller that supplied no pane.
    defaults: FakePaneState,
    /// The stamps this layer is holding synthetic data for.
    ///
    /// Held on the handler rather than in pane state because `apply_frame`
    /// takes a `&PaneRef` and cannot write one; the fake has no site, so there
    /// is nothing per-pane to keep them apart.
    resident: BTreeSet<NaiveDateTime>,
    /// The level the newest applied frame carried — what `prepare_job` draws.
    level: f32,
    generation: u64,
}

impl FakeSourceHandler {
    pub fn new() -> Self {
        Self {
            defaults: FakePaneState::new(false),
            resident: BTreeSet::new(),
            level: 0.0,
            generation: 0,
        }
    }

    fn view<'a>(&'a self, pane: &PaneRef<'a>) -> &'a FakePaneState {
        pane.state_as::<FakePaneState>().unwrap_or(&self.defaults)
    }
}

impl OverlayHandler for FakeSourceHandler {
    fn id(&self) -> LayerId {
        known::FAKE_SOURCE
    }
    fn surface(&self) -> Surface {
        Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        130
    }
    fn display_name(&self) -> &str {
        "Fake Source"
    }
    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
    }
    fn default_enabled(&self) -> bool {
        false
    }

    fn products(&self) -> &'static [ProductSpec] {
        products()
    }

    fn is_enabled(&self, pane: &PaneRef<'_>) -> bool {
        self.view(pane).enabled
    }

    fn set_enabled(&mut self, enabled: bool, pane: &mut PaneMut<'_>) {
        match pane.state_as::<FakePaneState>() {
            Some(state) => state.enabled = enabled,
            None => self.defaults.enabled = enabled,
        }
    }

    fn data_generation(&self) -> u64 {
        self.generation
    }

    fn has_data(&self, _pane: &PaneRef<'_>) -> bool {
        !self.resident.is_empty()
    }

    fn is_fetching(&self) -> bool {
        false
    }
    fn set_fetching(&mut self, _fetching: bool, _pane: &PaneRef<'_>) {}
    fn fetch_time(&self) -> Option<web_time::Instant> {
        None
    }

    /// This layer never fetches a *round* — its data arrives one frame at a
    /// time through [`OverlayHandler::apply_frame`].
    fn apply_fetch_result(&mut self, _result: FetchPayload, _pane: &PaneRef<'_>) {}

    fn retain_selections(&self, _selections: &mut Vec<Arc<dyn OverlayItem>>, _pane: &PaneRef<'_>) {}

    fn prepare_job(&self, ctx: &RasterizeContext, pane: &PaneRef<'_>) -> Option<DescribedJob> {
        if self.resident.is_empty() {
            return None;
        }
        Some(DescribedJob::new(FakeInput {
            tint: self.view(pane).tint,
            level: self.level,
            device_scale: ctx.device_scale,
        }))
    }

    fn job_codec(&self) -> Option<&'static JobCodec> {
        crate::render::jobs::JOB_CODECS
            .iter()
            .find(|row| row.label == crate::render::jobs::FAKE_LABEL)
    }

    /// One toggle and one dropdown — the two shapes the parity walk has to be
    /// able to reach on a layer nothing above has an arm for.
    fn controls(&self, pane: &PaneRef<'_>) -> Vec<ControlItem> {
        let state = self.view(pane);
        vec![
            ControlItem::Toggle {
                id: ENABLED_CONTROL,
                label: "Fake Source".to_string(),
                enabled: state.enabled,
            },
            ControlItem::Dropdown {
                id: TINT_CONTROL,
                label: TINT_LABEL.to_string(),
                options: [FakeTint::Warm, FakeTint::Cool]
                    .into_iter()
                    .map(|t| (t.as_str().to_string(), t.label().to_string()))
                    .collect(),
                selected: state.tint.as_str().to_string(),
            },
        ]
    }

    fn apply_control(&mut self, update: &ControlUpdate, pane: &mut PaneMut<'_>) -> ControlEffect {
        let apply = |state: &mut FakePaneState| match (update.id, &update.value) {
            (ENABLED_CONTROL, ControlValue::Bool(on)) => state.enabled = *on,
            (TINT_CONTROL, ControlValue::String(value)) => {
                if let Some(tint) = FakeTint::parse(value) {
                    state.tint = tint;
                }
            }
            _ => {}
        };
        match pane.state_as::<FakePaneState>() {
            Some(state) => apply(state),
            None => apply(&mut self.defaults),
        }
        ControlEffect::None
    }

    fn create_pane_state(&self, enabled: bool) -> Option<FetchPayload> {
        Some(Box::new(FakePaneState::new(enabled)))
    }

    fn deserialize_pane_state(
        &self,
        value: serde_json::Value,
        enabled: bool,
    ) -> Option<FetchPayload> {
        let mut state = FakePaneState::new(enabled);
        if let Some(on) = value.get("enabled").and_then(serde_json::Value::as_bool) {
            state.enabled = on;
        }
        if let Some(tint) = value
            .get("tint")
            .and_then(serde_json::Value::as_str)
            .and_then(FakeTint::parse)
        {
            state.tint = tint;
        }
        Some(Box::new(state))
    }

    fn serialize_pane_state(&self, state: &dyn std::any::Any) -> serde_json::Value {
        let Some(state) = state.downcast_ref::<FakePaneState>() else {
            return serde_json::Value::Null;
        };
        serde_json::json!({
            "enabled": state.enabled,
            "tint": state.tint.as_str(),
        })
    }

    // ── Time ──────────────────────────────────────────────────────────────

    fn time_axis(&self) -> TimeAxis {
        TimeAxis::FrameSeries {
            typical_step: std::time::Duration::from_secs(FRAME_STEP_SECS as u64),
            extends_future: false,
        }
    }

    /// Every grid point in the window, and `complete` because this list really
    /// is every frame that exists — the grid *is* the archive.
    fn list_frames(
        &self,
        ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        range: (NaiveDateTime, NaiveDateTime),
    ) -> FrameListing {
        let _ = (ctx, pane);
        FrameListing {
            range,
            frames: frames_in(range),
            complete: true,
        }
    }

    fn frames_resident(&self, _pane: &PaneRef<'_>) -> Vec<FrameStamp> {
        self.resident
            .iter()
            .map(|valid| FrameStamp {
                valid: *valid,
                run: None,
            })
            .collect()
    }

    fn retain_frames(&mut self, _pane: &PaneRef<'_>, keep: &[FrameStamp]) {
        self.resident
            .retain(|valid| keep.iter().any(|s| s.valid == *valid && s.run.is_none()));
    }

    /// Synthetic data, resolved in memory. The task is still a real
    /// [`FetchTask`], so the arrival travels the same road every other frame
    /// takes; only the *source* of the bytes is different.
    fn fetch_frame(
        &self,
        ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        stamp: &FrameStamp,
    ) -> Option<FetchTask> {
        let _ = (ctx, pane);
        if self.resident.contains(&stamp.valid) {
            return None;
        }
        let data = FakeFrameData::for_stamp(stamp.valid);
        Some(FetchTask {
            kind: known::FAKE_SOURCE,
            future: Box::pin(async move { Box::new(data) as FetchPayload }),
        })
    }

    fn apply_frame(&mut self, stamp: FrameStamp, data: FetchPayload, _pane: &PaneRef<'_>) {
        let Ok(frame) = data.downcast::<FakeFrameData>() else {
            log::error!("a frame reached the fake layer under another layer's payload");
            return;
        };
        self.resident.insert(stamp.valid);
        self.level = frame.level;
        self.generation += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdar_source::job::{EncodeCtx, JobGeometry};
    use rustdar_source::wire::Reader;

    const W: u32 = 64;
    const H: u32 = 48;

    fn bounds() -> GeoBounds {
        GeoBounds {
            min_lat: 34.0,
            max_lat: 36.0,
            min_lon: -99.0,
            max_lon: -97.0,
        }
    }

    fn geometry() -> JobGeometry {
        JobGeometry {
            width: W,
            height: H,
            bounds: bounds(),
            side_ceiling_px: 0,
        }
    }

    fn row() -> &'static JobCodec {
        crate::render::jobs::JOB_CODECS
            .iter()
            .find(|row| row.label == crate::render::jobs::FAKE_LABEL)
            .expect("the fake row is registered whenever this module compiles")
    }

    /// **The C1 proof in miniature: the worker path and the direct path paint
    /// the same picture.**
    ///
    /// The bytes go all the way round — `encode`, `decode`, `run`,
    /// `encode_out`, `decode_out` — through the registry's own function
    /// pointers, and the result is compared field for field against calling
    /// [`rasterize_fake`] in process. A row whose encode drops a member, whose
    /// decode reads them back in the wrong order, or whose reply adapter
    /// mislabels the alpha convention differs here.
    #[test]
    fn the_worker_path_and_the_direct_path_paint_the_same_raster() {
        for tint in [FakeTint::Warm, FakeTint::Cool] {
            let input = FakeInput {
                tint,
                level: 73.0,
                device_scale: 2.0,
            };
            let direct = rasterize_fake(&input, &bounds(), W, H);

            let job = DescribedJob::new(input.clone());
            let mut bytes = Vec::new();
            (row().encode)(
                &job,
                &EncodeCtx {
                    geometry: geometry(),
                },
                &mut bytes,
            );
            let mut reader = Reader::new(&bytes);
            let (decoded, geo) =
                (row().decode)(&mut reader, geometry()).expect("a row must decode its own encode");
            assert!(
                reader.at_end(),
                "the fake row's decode must consume exactly what its encode wrote",
            );
            assert_eq!(
                decoded.downcast_ref::<FakeInput>(),
                Some(&input),
                "decode ∘ encode is not the identity for the fake row",
            );

            let out = (row().run)(&decoded, &geo).expect("the fake row always runs");
            let mut head = Vec::new();
            let mut tails: Vec<Vec<u8>> = Vec::new();
            (row().encode_out)(out, &mut head, &mut tails);
            let back = (row().decode_out)(&head, tails)
                .expect("the reply adapter must decode its own encode");
            let back = back
                .take::<RasterizeOutput>()
                .expect("the fake row's output is a raster");

            assert_eq!(
                back.rgba, direct.rgba,
                "{tint:?}: the raster that came back over the wire is not the \
                 one the direct call paints",
            );
            assert_eq!(
                back.alpha, direct.alpha,
                "{tint:?}: the alpha convention moved"
            );
            assert!(
                back.hit_cells.is_none() && direct.hit_cells.is_none(),
                "the fake layer resolves no clicks and must carry no hit cells",
            );
            // Non-triviality floor: an all-zero raster would satisfy the
            // equality above without either path having painted anything.
            assert!(
                direct.rgba.iter().any(|&b| b != 0),
                "{tint:?}: the fixture painted nothing, so the comparison \
                 above cannot fail",
            );
        }
    }

    /// The two tints paint differently, so the byte comparison above is about
    /// the input and not only about the size of the buffer.
    #[test]
    fn the_two_tints_are_distinguishable() {
        let warm = rasterize_fake(
            &FakeInput {
                tint: FakeTint::Warm,
                level: 73.0,
                device_scale: 1.0,
            },
            &bounds(),
            W,
            H,
        );
        let cool = rasterize_fake(
            &FakeInput {
                tint: FakeTint::Cool,
                level: 73.0,
                device_scale: 1.0,
            },
            &bounds(),
            W,
            H,
        );
        assert_ne!(warm.rgba, cool.rgba, "both tints paint the same bytes");
    }

    /// The refusal contract: a tint byte outside this build's values is
    /// refused, not silently read as one of them.
    #[test]
    fn a_tint_byte_this_build_does_not_know_is_refused() {
        let mut bytes = vec![2u8];
        bytes.extend_from_slice(&1.0f32.to_le_bytes());
        bytes.extend_from_slice(&1.0f32.to_le_bytes());
        let mut reader = Reader::new(&bytes);
        assert!(
            (row().decode)(&mut reader, geometry()).is_none(),
            "the fake row read a tint byte it does not have a value for",
        );
    }

    /// **The alien cadence, as a fact rather than a stub.** The stamps are
    /// every epoch-aligned multiple of seven minutes inside the window, and
    /// nothing outside it.
    #[test]
    fn the_frame_grid_is_epoch_aligned_and_seven_minutes_apart() {
        let at = |secs: i64| {
            chrono::DateTime::from_timestamp(secs, 0)
                .expect("a literal in chrono's range")
                .naive_utc()
        };
        // A window opening one second AFTER a grid point, so the rounding is
        // exercised rather than being a no-op on a grid-aligned start.
        let frames = frames_in((at(FRAME_STEP_SECS + 1), at(FRAME_STEP_SECS * 5)));
        let seconds: Vec<i64> = frames
            .iter()
            .map(|f| f.valid.and_utc().timestamp())
            .collect();
        assert_eq!(
            seconds,
            vec![
                FRAME_STEP_SECS * 2,
                FRAME_STEP_SECS * 3,
                FRAME_STEP_SECS * 4,
                FRAME_STEP_SECS * 5,
            ],
            "the fake's grid is not every epoch-aligned {FRAME_STEP_SECS}s inside the window",
        );
        assert!(
            frames.iter().all(|f| f.run.is_none()),
            "the fake publishes observations, not model runs",
        );
        assert_eq!(
            FRAME_STEP_SECS, 420,
            "the step is deliberately neither radar's ~300s volume cadence nor \
             the model's 3600s hour; a timeline that hardcoded either draws \
             this layer wrong, and that is what this layer is for",
        );
    }

    /// One frame's synthetic data is a function of its stamp alone: two runs
    /// agree, and two different stamps do not.
    ///
    /// **The inequality is over `level`, not over the whole value.**
    /// `FakeFrameData` carries `valid` as well, so two different stamps differ
    /// on that field no matter what the level does — an `assert_ne!` on the
    /// struct passes even when the level has stopped depending on the stamp,
    /// which is exactly what a tamper found it doing.
    #[test]
    fn a_frames_data_is_a_function_of_its_stamp() {
        let a = chrono::DateTime::from_timestamp(FRAME_STEP_SECS * 3, 0)
            .expect("a literal in chrono's range")
            .naive_utc();
        let b = chrono::DateTime::from_timestamp(FRAME_STEP_SECS * 4, 0)
            .expect("a literal in chrono's range")
            .naive_utc();
        assert_eq!(
            FakeFrameData::for_stamp(a),
            FakeFrameData::for_stamp(a),
            "the same stamp must synthesise the same frame on every call",
        );
        assert_ne!(
            FakeFrameData::for_stamp(a).level,
            FakeFrameData::for_stamp(b).level,
            "two different stamps synthesise the same level, so nothing this \
             layer draws depends on which frame is showing",
        );
        assert_eq!(
            FakeFrameData::for_stamp(a).valid,
            a,
            "a frame's data must carry the stamp it was synthesised for",
        );
    }

    /// The one field states every fact, and its id is the bare spelling a
    /// config file would hold.
    #[test]
    fn the_one_field_states_its_facts_under_its_persisted_spelling() {
        let fields = products();
        assert_eq!(fields.len(), 1, "the fake publishes exactly one field");
        let spec = &fields[0];
        assert_eq!(spec.id.as_str(), FIELD_ID);
        assert_eq!(
            serde_json::to_string(&spec.id).expect("a FieldId serializes"),
            format!("\"{FIELD_ID}\""),
            "the field id must persist as the bare string, like every other",
        );
        assert_eq!(spec.group, GROUP);
        assert!(!spec.name.is_empty() && !spec.code.is_empty());
        assert!(!spec.scale.thresholds.is_empty());
        assert!(spec.value_domain.0 < spec.value_domain.1);
        assert!(!spec.domain_label_ends.0.is_empty());
        assert!(!spec.vertical, "the fake has no vertical extent");
    }

    /// The two controls the parity walk has to be able to reach, and the
    /// dropdown's selection surviving a round trip through pane state.
    #[test]
    fn the_layer_offers_a_toggle_and_a_dropdown_that_persist() {
        let mut handler = FakeSourceHandler::new();
        let items = handler.controls(&PaneRef::bare(0));
        assert_eq!(items.len(), 2, "one toggle and one dropdown: {items:?}");
        assert!(matches!(
            items[0],
            ControlItem::Toggle {
                id: ENABLED_CONTROL,
                ..
            }
        ));
        assert!(matches!(
            items[1],
            ControlItem::Dropdown {
                id: TINT_CONTROL,
                ..
            }
        ));

        handler.apply_control(
            &ControlUpdate {
                id: TINT_CONTROL,
                value: ControlValue::String(FakeTint::Cool.as_str().to_string()),
            },
            &mut PaneMut::bare(0),
        );
        let saved = handler.serialize_pane_state(&handler.defaults);
        assert_eq!(saved["tint"], FakeTint::Cool.as_str());
        let restored = handler
            .deserialize_pane_state(saved, false)
            .expect("the fake keeps per-pane state");
        assert_eq!(
            restored
                .downcast_ref::<FakePaneState>()
                .expect("its own state type")
                .tint,
            FakeTint::Cool,
        );
    }
}
