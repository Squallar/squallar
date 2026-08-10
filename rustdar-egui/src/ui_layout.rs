//! Runtime layout context: the one place the UI asks "how much room is there?".
//!
//! This is what replaces the compile-time desktop/mobile split. There is
//! exactly one UI; what it looks like is decided per frame from the size of the
//! content area rather than from `cfg!(target_os = ...)`.
//!
//! The wasm build is why none of this can be compile-time: one binary serves a
//! phone browser and a desktop browser, and a compile-time split is not
//! expressible there. It is also simply more correct — a 500pt window on a
//! desktop wants the compact chrome, and the old gate gave it the roomy one.

use crate::pane::{MAX_PANES_DESKTOP, MAX_PANES_MOBILE};

/// Breakpoint (points) below which the content area is [`WidthClass::Compact`].
const COMPACT_MAX_WIDTH: f32 = 600.0;
/// Breakpoint (points) at and above which the content area is
/// [`WidthClass::Expanded`].
const EXPANDED_MIN_WIDTH: f32 = 1000.0;

/// Gutter (points) left either side of a full-bleed dialog on a compact screen.
const COMPACT_DIALOG_MARGIN: f32 = 32.0;
/// Floor for a compact dialog, so an absurdly narrow window still gets a
/// usable window rather than a sliver.
const COMPACT_DIALOG_MIN_WIDTH: f32 = 200.0;

/// How wide the content area is, bucketed.
///
/// Keyed on `Context::content_rect()` — **not** `screen_rect()`, which ignores
/// safe-area insets, and **not** `ui.available_width()`, which oscillates as
/// panels claim space (the sidebar's own width would feed back into the
/// decision about whether to show the sidebar).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WidthClass {
    /// Phone-sized. The layers panel is a drawer; dialogs are full-bleed.
    Compact,
    /// Small window or tablet. The layers panel is still a drawer.
    Medium,
    /// Desktop-sized. The layers panel is a sidebar, open by default.
    Expanded,
}

impl WidthClass {
    /// Bucket a content width in points.
    pub(crate) fn from_width(width: f32) -> Self {
        if width < COMPACT_MAX_WIDTH {
            Self::Compact
        } else if width < EXPANDED_MIN_WIDTH {
            Self::Medium
        } else {
            Self::Expanded
        }
    }

    /// How many panes the pane picker offers at this size.
    ///
    /// This is a *presentation* limit — how many panes are worth offering on a
    /// screen this wide. It is deliberately not the limit a saved config is
    /// clamped to: see [`Self::max_panes_absolute`].
    pub(crate) fn max_panes(self) -> usize {
        match self {
            Self::Compact => MAX_PANES_MOBILE,
            Self::Medium | Self::Expanded => MAX_PANES_DESKTOP,
        }
    }

    /// Whether the layers panel takes its sidebar form — open by default and
    /// claiming panel space — as opposed to the drawer a narrower width gets.
    ///
    /// A default, not a mandate: the top bar's Layers toggle opens and closes
    /// the panel at every width, so on Expanded this only says what the panel
    /// is until the user says otherwise (see `Gui::stack_open`).
    pub(crate) fn has_persistent_sidebar(self) -> bool {
        self == Self::Expanded
    }

    /// The largest pane count any device may hold, used when *loading* a config.
    ///
    /// Clamping a loaded config to the current device's limit silently destroys
    /// data: a 5-pane layout saved on a desktop, opened once on a phone, comes
    /// back as 4 panes and is written back as 4 on the next save. The config is
    /// shared state, so it is clamped to what the format allows and the picker
    /// does the per-device narrowing.
    pub(crate) fn max_panes_absolute() -> usize {
        MAX_PANES_DESKTOP
    }
}

/// What is driving the pointer, latched across frames.
///
/// Seeded from `Context::os()` and then overridden by whatever real input
/// arrives, so a Bluetooth mouse on a tablet and a touchscreen on a laptop both
/// end up right. Independent of [`WidthClass`] on purpose: a 1400pt Android
/// tablet is Expanded *and* Touch, a 500pt desktop window is Compact *and*
/// Mouse. Collapsing the two axes into one "is mobile" boolean is what the old
/// `cfg!(target_os = "android")` did, and it gets both of those cases wrong.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PointerModality {
    /// The default is the conservative one, matching `LayoutCtx::default()`:
    /// assuming a mouse costs a touch user one gesture, whereas assuming touch
    /// costs a mouse user their map panning (see `InteractionState`).
    #[default]
    Mouse,
    Touch,
}

/// Remembers the current [`PointerModality`] between frames.
///
/// # Why not `has_touch_screen()`
///
/// egui's `InputState::has_touch_screen()` is `any_touches() || <a latched
/// flag>`, and that flag is set the first time a touch is ever seen and never
/// cleared. One stray palm on a hybrid laptop's touchscreen therefore puts the
/// whole session into touch mode permanently — including the 400ms tap deferral
/// on every subsequent mouse click. This latch moves in *both* directions: the
/// next real mouse event takes it back.
///
/// # Why frames, not events
///
/// A touch does not arrive as a lone `Touch` event. `egui-winit` maps one
/// `TouchPhase::Started` to `Touch{Start}` **plus** `PointerMoved` **plus**
/// `PointerButton{down}`, and eframe's web canvas emits `PointerButton{down}`
/// *before* `Touch{Start}`. So a per-event rule that treats `PointerButton` as
/// mouse evidence would flip to Mouse on the same frame every touch flips it to
/// Touch, and the answer would depend on event order. Instead the whole frame
/// is classified: a frame containing any `Touch` event is touch evidence and
/// yields no mouse evidence at all.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ModalityLatch {
    current: Option<PointerModality>,
}

impl ModalityLatch {
    /// Read this frame's events and return the modality now in force.
    pub(crate) fn update(&mut self, ctx: &egui::Context) -> PointerModality {
        if let Some(seen) = ctx.input(|i| frame_evidence(&i.events)) {
            self.current = Some(seen);
        }
        // Seed from the OS only until real input has spoken. `Unknown` is wasm,
        // where the user agent could be either — start it on Mouse and let the
        // first touch correct it, which costs a phone user one tap's deferral
        // rather than costing every desktop user 400ms forever.
        self.current.unwrap_or(match ctx.os() {
            egui::os::OperatingSystem::Android | egui::os::OperatingSystem::IOS => {
                PointerModality::Touch
            }
            _ => PointerModality::Mouse,
        })
    }
}

/// Classify one frame's events. `None` means the frame said nothing about the
/// modality (no pointer input at all), so the latch keeps its previous answer.
fn frame_evidence(events: &[egui::Event]) -> Option<PointerModality> {
    let mut saw_mouse = false;
    for event in events {
        match event {
            // Any touch in the frame settles it outright — see the type docs
            // for why this beats the pointer events it arrives alongside.
            egui::Event::Touch { .. } => return Some(PointerModality::Touch),
            egui::Event::PointerMoved(_)
            | egui::Event::PointerButton { .. }
            | egui::Event::MouseWheel { .. } => saw_mouse = true,
            _ => {}
        }
    }
    saw_mouse.then_some(PointerModality::Mouse)
}

/// One frame's resolved layout facts. Computed once per frame at the top of
/// `Gui::ui` and passed down; nothing below recomputes it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LayoutCtx {
    /// The rect the UI may draw content in — the viewport minus safe-area
    /// insets. Everything (root `Ui`, dialogs, breakpoints) keys off this.
    pub content_rect: egui::Rect,
    pub width: WidthClass,
    pub modality: PointerModality,
}

impl Default for LayoutCtx {
    /// The value a `Gui` holds before its first frame has resolved one.
    ///
    /// Nothing should ever read this — `Gui::ui` overwrites it before drawing
    /// anything — so it is deliberately the *conservative* choice rather than a
    /// plausible one: a zero-sized content rect classifies as `Compact`, which
    /// is the layout that assumes the least room. A leak shows up as chrome
    /// that is too cramped, which is visible, rather than chrome that overflows
    /// a small screen, which is not.
    fn default() -> Self {
        Self {
            content_rect: egui::Rect::ZERO,
            width: WidthClass::Compact,
            modality: PointerModality::Mouse,
        }
    }
}

impl LayoutCtx {
    /// Resolve this frame's layout.
    ///
    /// `extra_insets` are `(top, bottom, left, right)` insets supplied by the
    /// host application, applied *on top of* the ones egui already knows about.
    /// `egui-winit` fills `RawInput::safe_area_insets` itself on iOS, so
    /// `content_rect()` is already correct there; the Android host pushes its
    /// `WindowInsets` through a side channel on `Gui` instead, and this is
    /// where the two are reconciled into one number. Routing the Android insets
    /// to `egui_input_mut().safe_area_insets` would make `extra_insets` dead —
    /// but that wiring lives in the host crate, and this type is the single
    /// source of truth either way, so nothing below has to know which route
    /// they took.
    pub(crate) fn resolve(
        ctx: &egui::Context,
        latch: &mut ModalityLatch,
        extra_insets: (f32, f32, f32, f32),
    ) -> Self {
        let (top, bottom, left, right) = extra_insets;
        let content_rect = shrink_to_content(ctx.content_rect(), top, bottom, left, right);
        Self {
            content_rect,
            width: WidthClass::from_width(content_rect.width()),
            modality: latch.update(ctx),
        }
    }

    /// Width for a modal dialog: full-bleed with a gutter when compact, a fixed
    /// comfortable width otherwise.
    ///
    /// Measured against the *content* rect, so a phone dialog stops at the
    /// notch instead of running under it.
    pub(crate) fn dialog_width(&self, roomy_width: f32) -> f32 {
        match self.width {
            WidthClass::Compact => {
                (self.content_rect.width() - COMPACT_DIALOG_MARGIN).max(COMPACT_DIALOG_MIN_WIDTH)
            }
            WidthClass::Medium | WidthClass::Expanded => roomy_width,
        }
    }

    /// Where a modal dialog is centred. `content_rect`, not `viewport_rect`, so
    /// a centred dialog is centred in the *visible* area.
    pub(crate) fn dialog_center(&self) -> egui::Pos2 {
        self.content_rect.center()
    }
}

/// Inset a rect, refusing to produce an inverted or degenerate one.
///
/// A host that reports insets larger than the window (a transient during
/// rotation, or a units mix-up) must not hand the rest of the UI a negative
/// width — every downstream consumer, from the breakpoint to the dialog width,
/// would then get nonsense out of a perfectly ordinary comparison.
fn shrink_to_content(rect: egui::Rect, top: f32, bottom: f32, left: f32, right: f32) -> egui::Rect {
    if !(top.is_finite() && bottom.is_finite() && left.is_finite() && right.is_finite()) {
        return rect;
    }
    let min = egui::pos2(rect.left() + left.max(0.0), rect.top() + top.max(0.0));
    let max = egui::pos2(
        rect.right() - right.max(0.0),
        rect.bottom() - bottom.max(0.0),
    );
    if max.x <= min.x || max.y <= min.y {
        return rect;
    }
    egui::Rect::from_min_max(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with(events: Vec<egui::Event>, os: egui::os::OperatingSystem) -> egui::Context {
        let ctx = egui::Context::default();
        ctx.set_os(os);
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1024.0, 768.0),
            )),
            events,
            ..Default::default()
        });
        ctx
    }

    fn touch_event(phase: egui::TouchPhase) -> egui::Event {
        egui::Event::Touch {
            device_id: egui::TouchDeviceId(0),
            id: egui::TouchId(0),
            phase,
            pos: egui::pos2(10.0, 10.0),
            force: None,
        }
    }

    fn mouse_button_event() -> egui::Event {
        egui::Event::PointerButton {
            pos: egui::pos2(10.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        }
    }

    /// A touch flips the latch even though the same frame also carries the
    /// `PointerButton` that `egui-winit` synthesises for it. This is the case a
    /// per-event rule gets wrong.
    #[test]
    fn a_touch_frame_beats_the_pointer_events_it_arrives_with() {
        let mut latch = ModalityLatch::default();
        let ctx = ctx_with(
            vec![
                touch_event(egui::TouchPhase::Start),
                egui::Event::PointerMoved(egui::pos2(10.0, 10.0)),
                mouse_button_event(),
            ],
            egui::os::OperatingSystem::Nix,
        );
        assert_eq!(latch.update(&ctx), PointerModality::Touch);
    }

    /// eframe's web canvas emits the `PointerButton` *before* the `Touch`, so
    /// the answer must not depend on which came first.
    #[test]
    fn a_touch_frame_is_touch_whatever_order_the_events_came_in() {
        let mut latch = ModalityLatch::default();
        let ctx = ctx_with(
            vec![mouse_button_event(), touch_event(egui::TouchPhase::Start)],
            egui::os::OperatingSystem::Nix,
        );
        assert_eq!(latch.update(&ctx), PointerModality::Touch);
    }

    /// The whole point of not using `has_touch_screen()`: one stray touch on a
    /// hybrid laptop must not cost the user touch-mode forever.
    #[test]
    fn a_stray_touch_does_not_latch_a_hybrid_laptop_into_touch_mode() {
        let mut latch = ModalityLatch::default();

        let touched = ctx_with(
            vec![touch_event(egui::TouchPhase::Start)],
            egui::os::OperatingSystem::Nix,
        );
        assert_eq!(latch.update(&touched), PointerModality::Touch);

        let moused = ctx_with(
            vec![egui::Event::PointerMoved(egui::pos2(40.0, 40.0))],
            egui::os::OperatingSystem::Nix,
        );
        assert_eq!(
            latch.update(&moused),
            PointerModality::Mouse,
            "the next real mouse movement must take the session back"
        );
    }

    /// An input-free frame says nothing, so the previous answer stands. Without
    /// this the modality would flap back to the OS seed every idle frame.
    #[test]
    fn an_input_free_frame_keeps_the_previous_modality() {
        let mut latch = ModalityLatch::default();

        let touched = ctx_with(
            vec![touch_event(egui::TouchPhase::Start)],
            egui::os::OperatingSystem::Nix,
        );
        assert_eq!(latch.update(&touched), PointerModality::Touch);

        let idle = ctx_with(vec![], egui::os::OperatingSystem::Nix);
        assert_eq!(latch.update(&idle), PointerModality::Touch);
    }

    /// Non-pointer events are not mouse evidence. A phone with a Bluetooth
    /// keyboard must not be talked out of touch mode by typing.
    #[test]
    fn keyboard_input_is_not_evidence_of_a_mouse() {
        let mut latch = ModalityLatch::default();

        let touched = ctx_with(
            vec![touch_event(egui::TouchPhase::Start)],
            egui::os::OperatingSystem::Nix,
        );
        assert_eq!(latch.update(&touched), PointerModality::Touch);

        let typed = ctx_with(
            vec![egui::Event::Text("a".into())],
            egui::os::OperatingSystem::Nix,
        );
        assert_eq!(latch.update(&typed), PointerModality::Touch);
    }

    /// Before any input arrives the OS is the only hint there is.
    #[test]
    fn the_os_seeds_the_modality_until_real_input_arrives() {
        let mut latch = ModalityLatch::default();
        let android = ctx_with(vec![], egui::os::OperatingSystem::Android);
        assert_eq!(latch.update(&android), PointerModality::Touch);

        let mut latch = ModalityLatch::default();
        let mac = ctx_with(vec![], egui::os::OperatingSystem::Mac);
        assert_eq!(latch.update(&mac), PointerModality::Mouse);
    }

    /// ...and real input beats the seed, which is the case a plain
    /// `cfg!(target_os)` cannot express: a mouse plugged into a tablet.
    #[test]
    fn real_input_overrides_the_os_seed() {
        let mut latch = ModalityLatch::default();
        let android_with_mouse = ctx_with(
            vec![egui::Event::PointerMoved(egui::pos2(5.0, 5.0))],
            egui::os::OperatingSystem::Android,
        );
        assert_eq!(latch.update(&android_with_mouse), PointerModality::Mouse);
    }

    /// The breakpoints are absolute claims, written as literals rather than
    /// derived from the constants, so moving a constant fails here instead of
    /// silently moving the test with it.
    #[test]
    fn the_width_classes_split_at_600_and_1000() {
        assert_eq!(WidthClass::from_width(599.0), WidthClass::Compact);
        assert_eq!(WidthClass::from_width(600.0), WidthClass::Medium);
        assert_eq!(WidthClass::from_width(999.0), WidthClass::Medium);
        assert_eq!(WidthClass::from_width(1000.0), WidthClass::Expanded);
    }

    /// The chrome decision the width class still owns: which form the layers
    /// panel defaults to. Only the widest class gets the sidebar — the top bar
    /// carries everything else identically at every width.
    #[test]
    fn each_width_class_picks_a_different_chrome() {
        assert!(!WidthClass::Compact.has_persistent_sidebar());
        assert!(!WidthClass::Medium.has_persistent_sidebar());
        assert!(WidthClass::Expanded.has_persistent_sidebar());
    }

    /// The pane picker narrows on a phone, and the two roomier classes agree —
    /// the split is Compact-vs-the-rest, not three separate limits.
    #[test]
    fn the_pane_picker_narrows_only_on_a_compact_screen() {
        assert_eq!(WidthClass::Compact.max_panes(), MAX_PANES_MOBILE);
        assert_eq!(WidthClass::Medium.max_panes(), MAX_PANES_DESKTOP);
        assert_eq!(WidthClass::Expanded.max_panes(), MAX_PANES_DESKTOP);
        assert!(
            WidthClass::Compact.max_panes() < WidthClass::Expanded.max_panes(),
            "precondition: the compact limit must actually be the narrower one, \
             or the three assertions above are satisfied by any single constant"
        );
    }

    /// The config clamp must not be the current device's limit. A 5-pane
    /// desktop layout opened on a phone has to survive the round trip.
    #[test]
    fn the_config_clamp_is_wider_than_a_compact_screen_offers() {
        assert_eq!(WidthClass::max_panes_absolute(), MAX_PANES_DESKTOP);
        assert!(
            WidthClass::max_panes_absolute() > WidthClass::Compact.max_panes(),
            "a config clamped to the compact limit silently truncates layouts"
        );
    }

    /// A compact dialog is full-bleed inside the *content* rect. Measuring it
    /// against the viewport is what put a `screen.width - 32` popup under the
    /// notch.
    #[test]
    fn a_compact_dialog_fills_the_content_rect_not_the_viewport() {
        let layout = LayoutCtx {
            // A 400pt-wide phone whose viewport was 400 but whose content
            // starts 20pt in from each side.
            content_rect: egui::Rect::from_min_max(
                egui::pos2(20.0, 40.0),
                egui::pos2(380.0, 800.0),
            ),
            width: WidthClass::Compact,
            modality: PointerModality::Touch,
        };
        assert_eq!(layout.dialog_width(340.0), 360.0 - 32.0);
        assert_eq!(layout.dialog_center(), egui::pos2(200.0, 420.0));
    }

    /// A roomy screen takes the caller's width verbatim — the compact branch
    /// must not leak into it.
    #[test]
    fn a_roomy_dialog_keeps_the_requested_width() {
        let layout = LayoutCtx {
            content_rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 900.0)),
            width: WidthClass::Expanded,
            modality: PointerModality::Mouse,
        };
        assert_eq!(layout.dialog_width(340.0), 340.0);
    }

    /// A very narrow window still gets a usable dialog rather than a sliver.
    #[test]
    fn a_compact_dialog_has_a_floor() {
        let layout = LayoutCtx {
            content_rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(180.0, 400.0)),
            width: WidthClass::Compact,
            modality: PointerModality::Touch,
        };
        assert_eq!(layout.dialog_width(340.0), 200.0);
    }

    /// Host-supplied insets shrink the content rect, and the breakpoint is
    /// taken from the shrunk width — a 610pt window with 20pt of side insets
    /// is a Compact 570pt content area, not a Medium one.
    #[test]
    fn host_insets_move_the_breakpoint() {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(610.0, 800.0));
        assert_eq!(
            WidthClass::from_width(rect.width()),
            WidthClass::Medium,
            "precondition: the raw viewport is Medium, so the inset is what moves it"
        );

        let content = shrink_to_content(rect, 0.0, 0.0, 20.0, 20.0);
        assert_eq!(content.width(), 570.0);
        assert_eq!(WidthClass::from_width(content.width()), WidthClass::Compact);
    }

    /// Insets that would invert the rect are refused outright: every consumer
    /// downstream compares widths, and a negative one turns an ordinary
    /// comparison into nonsense.
    #[test]
    fn absurd_insets_cannot_invert_the_content_rect() {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 800.0));
        assert_eq!(shrink_to_content(rect, 0.0, 0.0, 300.0, 300.0), rect);
        assert_eq!(shrink_to_content(rect, 500.0, 500.0, 0.0, 0.0), rect);
        assert_eq!(shrink_to_content(rect, f32::NAN, 0.0, 0.0, 0.0), rect);
    }

    /// The insets really are applied on top of whatever egui already resolved,
    /// rather than replacing it — driven through a real `Context` so the
    /// `content_rect()` half of the sum is egui's own.
    #[test]
    fn resolve_stacks_host_insets_on_top_of_eguis_own() {
        let ctx = egui::Context::default();
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 800.0),
            )),
            safe_area_insets: Some(egui::SafeAreaInsets(egui::epaint::MarginF32 {
                top: 50.0,
                ..Default::default()
            })),
            ..Default::default()
        });

        assert_eq!(
            ctx.content_rect().top(),
            50.0,
            "precondition: egui applied the RawInput inset itself"
        );

        let mut latch = ModalityLatch::default();
        let layout = LayoutCtx::resolve(&ctx, &mut latch, (10.0, 0.0, 0.0, 0.0));
        assert_eq!(
            layout.content_rect.top(),
            60.0,
            "the host inset must stack on egui's, not replace it"
        );
    }
}
