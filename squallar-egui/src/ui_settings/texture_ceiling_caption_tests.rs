//! **The texture-size caption, read back off the glass.**
//!
//! Three things about this line were proven and a fourth was not.
//! [`texture_ceiling_caption`]'s own arithmetic is pinned by the sibling
//! `texture_ceiling_tests` module; the App's composition of the pair is
//! pinned on its side; the row's presence in [`SETTINGS_ROWS`] is pinned by
//! `the_row_is_listed_and_the_reset_arm_names_it`. **Nothing joined them.**
//! No test had ever shown the sentence reaching a frame, and the browser rig
//! does not open the settings screen, so the one claim a user could check —
//! *the words are on the screen* — was the one claim nothing made.
//!
//! These tests drive the shipped UI: the menu's `Settings...` entry, a real
//! scroll to the real row, and the sentence read back out of the **paint
//! output** — `Shape::Text`'s own galley, via
//! [`InputHarness::painted_text_strings_in`] — rather than out of the
//! function that composed it. What is asserted is that the string the
//! composer returns is the string the renderer was handed, for a readout
//! injected from outside exactly as the App injects it.
//!
//! # Why this and not a headed screenshot
//!
//! A screenshot is not an assertion. A window holding its last frame over a
//! wedged app is indistinguishable from a live one, and OCR of a picture is
//! inference about pixels. This is an equality on the text the renderer
//! received. It also reaches a case no photograph of this hardware could:
//! see [`a_device_that_falls_short_says_so_on_the_glass`], whose readout no
//! adapter in this building can produce.
//!
//! # The figure is asserted exactly; only the prose may be approximate
//!
//! Carried here from a scoring bug in a parked OCR leg, because it
//! generalises well past OCR. That leg matched every token through one
//! fuzzy window at ratio 0.86, and `"at most 8192 px in force"` scored 0.90
//! against a glass reading `"at most 4096 px in force"` — so the check
//! **passed on the wrong number, which is the one thing it existed to
//! catch**. The figures are the entire subject of this caption: two
//! sentences differing in one digit make opposite claims about the machine.
//!
//! So every test below asserts the whole sentence by **equality**, and then
//! separately asserts that the *adjacent* figures — the plausible wrong
//! answers, not arbitrary ones — are absent from the row. A near-miss must
//! fail loudly rather than pass quietly: a false red on the instrument in
//! preference to a false green on the result.

use super::*;
use crate::input_harness::InputHarness;
use crate::shell_api::{BudgetReadout, RasterSideReadout};
use squallar_device_profile::budget::{self, BudgetLimits, DeviceProfile, TextureCeiling};

/// Tall on purpose: the memory rows sit low in a 28-row list, and a short
/// viewport turns "the row never drew" into an indistinguishable failure.
/// The offline-areas suite reaches its own row this way.
const TALL_SCREEN: egui::Vec2 = egui::vec2(1024.0, 1600.0);

/// The row under test, by the id [`SETTINGS_ROWS`] lists it under.
const ROW: &str = "memory.texture_ceiling";

/// **What a desktop adapter here reports for `max_texture_dimension_2d`.**
///
/// 16384 is what this class of part answers; the lavapipe software rasteriser
/// answers the same. The exact value matters less than its size: every figure
/// below is derived from it through the real budget code rather than typed
/// in, so a bracket that moved would move these expectations with it.
const REPORTED_2D: u32 = 16384;

/// **The side in force here, computed the way the App computes it** —
/// `TextureCeiling::hold_all` over the resolved desktop budgets, then
/// `Budgets::raster_side_for_adapter`. Not a literal: this is the same
/// arithmetic `AppState::new` and `App::adopt_budgets` run, so the
/// expectations below are pinned to the device profile's own constants
/// rather than to numbers retyped beside them.
///
/// Deterministic across promotions: the desktop bracket pins both ends this
/// reads -- its long-range image side and its raster side ceiling are both
/// built by `Bracket::pinned` rather than stepped -- so no rung of the ladder
/// moves the answer.
fn effective_here(requested: TextureCeiling) -> usize {
    let profile = DeviceProfile {
        limits: BudgetLimits::DESKTOP,
        ..DeviceProfile::for_target()
    };
    let budgets = requested.hold_all(budget::resolve(&profile));
    budgets.raster_side_for_adapter(REPORTED_2D)
}

/// A readout carrying nothing but the pair under test, as the App composes it.
fn readout(raster: RasterSideReadout) -> BudgetReadout {
    BudgetReadout {
        generation: 1,
        raster,
        ..BudgetReadout::default()
    }
}

/// The pair, spelled once.
fn priced(requested: TextureCeiling, effective: usize) -> RasterSideReadout {
    RasterSideReadout {
        requested,
        effective_side_px: Some(effective),
    }
}

/// Whitespace collapsed, and nothing else touched.
///
/// A wrapped label is still one galley and one painted run, so this changes
/// nothing today; it exists so that a column narrow enough to re-flow the
/// sentence cannot turn a correct caption into a failing equality. **No
/// digit and no word is normalised** — that would be the fuzzy window this
/// module's header is about.
fn norm(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Where a scroll gesture has to land to move the settings body: over the
/// inspector, never the map.
fn scroll_pos(h: &InputHarness) -> egui::Pos2 {
    h.inspector_rect()
        .expect("the inspector must be on screen to be scrolled")
        .center()
}

/// The band the texture row occupies, narrowed to the inspector's column.
///
/// The narrowing is load-bearing — a settings row's probe carries the whole
/// content width, so its bare rect also spans the map beside the panel and
/// would sweep up anything drawn there as if the row had said it.
fn row_rect(h: &InputHarness) -> egui::Rect {
    let row = h.settings_row(ROW).expect("the texture row drew").rect;
    let column = h
        .inspector_rect()
        .expect("the inspector is on screen while settings are open");
    egui::Rect::from_x_y_ranges(column.x_range(), row.y_range()).expand(4.0)
}

/// Open Settings and scroll the Texture size row onto the glass, the way the
/// offline-areas suite reaches its own row.
fn open_texture_row(h: &mut InputHarness) {
    h.open_settings();
    assert!(
        SETTINGS_ROWS.contains(&ROW),
        "the row table no longer lists {ROW}",
    );
    let pos = scroll_pos(h);
    let found = h.scroll_until(pos, egui::vec2(0.0, -160.0), 120, |h| {
        h.settings_row(ROW)
            .is_some_and(|drawn| h.screen_rect().contains(drawn.rect.center()))
    });
    assert!(found, "the Texture size row never reached the glass");
}

/// **Drive the shipped screen** under `requested` with `raster` in force, and
/// return every text the texture row painted.
fn painted_row(requested: TextureCeiling, raster: RasterSideReadout) -> Vec<String> {
    let mut h = InputHarness::with_screen(TALL_SCREEN);
    // The user's own setting, as a restored config leaves it.
    h.gui_mut().texture_ceiling = requested;
    // The App's readout, injected exactly as `apply_frame_inputs` delivers it.
    h.set_budget_readout(readout(raster));
    open_texture_row(&mut h);
    h.painted_text_strings_in(row_rect(&h))
}

/// The row painted `needle` somewhere.
fn says(texts: &[String], needle: &str) -> bool {
    texts.iter().any(|text| text.contains(needle))
}

/// **The whole sentence reached the renderer**, by equality against what the
/// composer returns for the same inputs — never by substring, so a caption
/// truncated or run together with its neighbour fails.
fn assert_paints_the_composed_sentence(
    texts: &[String],
    raster: RasterSideReadout,
    requested: TextureCeiling,
) {
    let want = texture_ceiling_caption(Some(raster), requested);
    assert!(
        texts.iter().any(|text| norm(text) == norm(&want)),
        "the row never painted the composed caption.\n  wanted: {want:?}\n  \
         painted: {texts:#?}",
    );
}

/// **The mandatory arm: a machine that reaches the setting says so, in the
/// user's own figure, on the glass.**
///
/// The device reasoning, and why this is the shape this hardware produces:
/// `hold_all(4096)` lowers both ends of the desktop bracket to 4096, so
/// `raster_side_for_adapter(16384)` clamps `16384 / 2` into `[4096, 4096]`
/// and resolves to 4096. Asking for the largest offered rung on an adapter
/// this size is therefore *reached*, never clamped — asserted here rather
/// than assumed, because it is the premise the sentence rests on.
#[test]
fn the_size_in_force_reaches_the_glass_beside_the_size_asked_for() {
    let asked = TextureCeiling::clamped(4096);
    let effective = effective_here(asked);
    assert_eq!(
        effective, 4096,
        "an adapter reporting {REPORTED_2D} px was expected to reach the \
         largest offered rung; the bracket moved under this test",
    );

    let raster = priced(asked, effective);
    let texts = painted_row(asked, raster);
    assert_paints_the_composed_sentence(&texts, raster, asked);

    assert!(says(&texts, "4096 px asked for"), "painted: {texts:#?}");
    assert!(
        says(&texts, "at most 4096 px in force"),
        "painted: {texts:#?}",
    );
    assert!(
        says(&texts, "your setting"),
        "a device that reaches the setting must name the setting as what \
         held it; painted: {texts:#?}",
    );
    assert!(
        !says(&texts, "does not reach"),
        "a device that REACHES the setting was reported as clamping it; \
         painted: {texts:#?}",
    );
    // The adjacent figures, not arbitrary ones. See the module header: a
    // near-miss on the number is the failure this suite exists to catch.
    for wrong in ["at most 8192 px in force", "at most 2048 px in force"] {
        assert!(
            !says(&texts, wrong),
            "the row carried {wrong:?} beside the figure actually in force; \
             painted: {texts:#?}",
        );
    }
    for other in ["No limit asked for", "Applying"] {
        assert!(
            !says(&texts, other),
            "a settled row carried another shape's words ({other:?}); \
             painted: {texts:#?}",
        );
    }
}

/// **The neutral posture still names the size in force** — the shape a fresh
/// install sees, and the one a user who never touched the control lives
/// under.
///
/// Its whole argument is that a bare "No limit" cannot say what the machine
/// will actually do, so the absence assertions matter as much as the
/// presence ones: a row that also said "does not reach" would be telling a
/// user with no ceiling at all that their machine fell short of one.
#[test]
fn the_neutral_posture_names_the_size_in_force_on_the_glass() {
    let asked = TextureCeiling::NONE;
    let effective = effective_here(asked);
    assert_eq!(
        effective, 8192,
        "with no ceiling asked for, the desktop bracket's own ceiling was \
         expected to be what an adapter reporting {REPORTED_2D} px reaches",
    );

    let raster = priced(asked, effective);
    let texts = painted_row(asked, raster);
    assert_paints_the_composed_sentence(&texts, raster, asked);

    assert!(says(&texts, "No limit asked for"), "painted: {texts:#?}");
    assert!(
        says(&texts, "at most 8192 px in force"),
        "painted: {texts:#?}",
    );
    assert!(
        says(&texts, "all this device reaches"),
        "painted: {texts:#?}",
    );
    // Presence alone would pass on a frame still carrying the previous
    // shape's words, which is the whole reason this arm is written out.
    for other in ["does not reach", "your setting", "Applying"] {
        assert!(
            !says(&texts, other),
            "a session under no ceiling at all carried {other:?}; \
             painted: {texts:#?}",
        );
    }
    for wrong in ["at most 4096 px in force", "at most 2048 px in force"] {
        assert!(!says(&texts, wrong), "painted: {texts:#?}");
    }
}

/// **A device that falls short says so, beside the setting** — the case the
/// control exists for.
///
/// **No adapter in this building can produce this readout**, and that is
/// precisely why it is worth having here. The shape needs
/// `effective < requested`, i.e. a part reporting under 4096 px; every
/// adapter here — the software rasteriser included — reports 16384, so a
/// headed screenshot leg on this hardware provably could not reach it. The
/// readout is a plain value, so this drives the real screen with the pair a
/// small device would produce and reads the sentence back off the paint.
#[test]
fn a_device_that_falls_short_says_so_on_the_glass() {
    let asked = TextureCeiling::clamped(4096);
    assert!(
        effective_here(asked) > 2048,
        "this test's premise is that the hardware here CANNOT clamp 4096, so \
         the clamped pair has to be constructed rather than measured",
    );

    let raster = priced(asked, 2048);
    let texts = painted_row(asked, raster);
    assert_paints_the_composed_sentence(&texts, raster, asked);

    assert!(says(&texts, "4096 px asked for"), "painted: {texts:#?}");
    assert!(
        says(&texts, "at most 2048 px in force"),
        "the device's own figure is not on the line, so the user is shown \
         only what they asked for; painted: {texts:#?}",
    );
    assert!(
        says(&texts, "does not reach"),
        "a device below the setting is not named as what held it; \
         painted: {texts:#?}",
    );
    assert!(
        !says(&texts, "your setting"),
        "a clamped device was reported as honouring the setting; \
         painted: {texts:#?}",
    );
    assert!(
        !says(&texts, "at most 4096 px in force"),
        "the row printed the figure ASKED FOR as the figure in force; \
         painted: {texts:#?}",
    );
}

/// **The clamped and the reached device do not read alike on the glass.**
///
/// The sibling unit test makes this claim about the composer's return value.
/// It is remade here about the paint, because a caption that composed two
/// different sentences and painted one of them — a stale galley, a row drawn
/// from a cached string — would satisfy the unit test and fail the user.
#[test]
fn the_clamped_and_the_reached_device_paint_different_sentences() {
    let asked = TextureCeiling::clamped(4096);
    let clamped = painted_row(asked, priced(asked, 2048));
    let reached = painted_row(asked, priced(asked, effective_here(asked)));

    let line_of = |texts: &[String]| {
        texts
            .iter()
            .find(|text| text.contains("px asked for"))
            .cloned()
            .expect("the row painted no caption at all")
    };
    assert_ne!(
        norm(&line_of(&clamped)),
        norm(&line_of(&reached)),
        "the clamped and the unclamped device read identically on the glass, \
         so this line cannot tell the user which they have",
    );
}

/// **A rung the App has not priced yet is not reported as in force.**
///
/// The readout is composed on the telemetry tick, so for up to one period the
/// App is genuinely still rendering at the old size. This is the transient a
/// screenshot leg could only photograph and hope to catch; driven here, it is
/// deterministic.
#[test]
fn a_rung_the_app_has_not_priced_yet_says_so_on_the_glass() {
    // The user has just picked 1024; the readout still carries the posture
    // the App last priced, which is no ceiling at all.
    let asked = TextureCeiling::clamped(1024);
    let stale = priced(TextureCeiling::NONE, effective_here(TextureCeiling::NONE));

    let texts = painted_row(asked, stale);
    assert_paints_the_composed_sentence(&texts, stale, asked);

    assert!(says(&texts, "Applying 1024 px"), "painted: {texts:#?}");
    assert!(
        !says(&texts, "in force"),
        "a ceiling the App has not seen was reported as in force; \
         painted: {texts:#?}",
    );
    assert!(
        !says(&texts, "8192"),
        "the figure from before the change was painted under the new choice; \
         painted: {texts:#?}",
    );
}
