//! The shell pass: the docked top bar, then the floating surfaces around the
//! full-bleed map.
//!
//! # One docked panel; everything else floats
//!
//! The Synthesis design's full-bleed rule is that the map fills everything
//! under the top bar and all other chrome floats over it, inside its bounds.
//! So exactly one `Panel` claims space here — the top bar (`ui_topbar.rs`) —
//! and what is left of the root `Ui` **is** the map's `CentralPanel`, edge to
//! edge. That remainder is captured once, as [`ShellOutput::map_rect`], and
//! every floating surface positions itself from it: the status bar along the
//! bottom inset (`ui_statusbar.rs`), the layer stack at top-left
//! (`ui_stack.rs`), the inspector at top-right (`ui_inspector.rs`), and the
//! timeline transport the frame draws later, after the pane loop
//! (`ui_timeline.rs`).
//!
//! The floating surfaces are `egui::Area`s above `Order::Background`, which is
//! what keeps the map from reacting underneath them: every map click resolver
//! runs through `filter_dialog_blocked`/`is_pos_blocked`, whose layer check
//! drops a position covered by any layer above Background — no excluded-rect
//! plumbing required.
//!
//! Below the Compact breakpoint the corner-floating pass stands down: the
//! same flags present as pages of the phone sheet, drawn late in the frame by
//! `ui_sheet.rs` through the same body renderers and the same take window —
//! the shell keeps only the top bar there, and the status bar keeps nothing
//! (its short scan summary lives in the phone top bar).
//!
//! # One take window for the stack and the inspector
//!
//! Both panels are about the active pane — the stack walks its `draw_order`
//! and flips its layers, the inspector edits its product, kind and per-layer
//! configs — and several of the renderers they host need `&mut PaneState`
//! beside `&mut self`. So the pass holds the active pane out of the vector
//! with `std::mem::take`, **once, around both panels**: everything either
//! panel needs of the pane reads and writes the taken value, and nothing
//! inside the window may read `self.panes[..]`, whose active slot holds a
//! default placeholder until the restore. The menu model is built before any
//! take (by the top bar), the stack's row status lines are built from the
//! taken pane against a registry demonstrably loaded with its configs, and
//! [`Gui::write_pane_overlay`] is how the eye and Show toggles write both
//! halves without touching the vector. The restore is followed by
//! `propagate_layer_sync`, so a reorder, a toggle or an edited config fans
//! out to the synced panes the same frame.
//!
//! # Ids do not depend on the breakpoint
//!
//! Every area, panel and combo-box id prefix uses one constant id regardless
//! of which presentation is on screen. egui keys widget memory — combo state,
//! scroll offsets, panel sizes — on those ids, so keying any of them on the
//! layout would silently reset the user's UI state every time the window
//! crossed a breakpoint. The two pre-rebuild files had exactly that hazard
//! latent in them: `"d_"`/`"m_"` control prefixes and
//! `layers_panel`/`mobile_layers_panel` could never collide only because the
//! two files were never compiled together.
//!
//! The full-bleed flip also *strengthened* the positional-id story. egui's
//! `Ui::new_child` computes `unique_id = stable_id.with(parent's
//! next_auto_id_salt)` (`egui-0.35.0/src/ui.rs:255`), so the root `Ui`'s
//! auto-id counter folds into every panel's registered id — which is why a
//! panel that appears or vanishes with the width would re-key everything shown
//! after it. With the status bar, stack and inspector all floating `Area`s
//! (whose ids are their own roots, not children of the root `Ui`), the top bar
//! is the only thing that advances that counter at all, and it is drawn at
//! every width. `crossing_a_breakpoint_re_keys_nothing` pins the whole claim,
//! and `crossing_a_breakpoint_does_not_move_any_widget_id` pins the
//! stored-state half of it.

use crate::actions::GuiAction;
use rustdar_overlays::render::overlay_state::OverlayKind;

use super::{InspectorSelection, PaneState};

/// The one frame every persistent floating surface draws in: the stock
/// window frame with the drop shadow removed.
///
/// The second user test's finding: the timeline's window shadow cast onto
/// the status bar floating under it, and every stacked pair of chrome
/// surfaces (transport over status bar, sheet over bottom bar, chip over
/// either) repeats the smudge. The persistent chrome — stack, inspector,
/// timeline and its chip, status bar, bottom bar, sheet, error toast — is
/// furniture, not a transient surface asserting elevation, so it draws flat;
/// fill, stroke and rounding stay the stock theme's. Popovers, menus,
/// tooltips and the modal dialogs deliberately keep their shadows: they are
/// transient, and the shadow is how they read as *over* the furniture.
///
/// A source-scan test holds the chrome files to this constructor, so a new
/// surface cannot quietly ship the shadowed frame.
pub(crate) fn chrome_frame(style: &egui::Style) -> egui::Frame {
    egui::Frame::window(style).shadow(egui::Shadow::NONE)
}

/// The scroll sources every panel `ScrollArea` accepts: the stock set plus
/// **mouse** drag-to-scroll.
///
/// egui 0.35's default is `DragScroll::OnTouch` — content drags scroll only
/// where a touch screen is detected — so on a desktop, click-dragging a
/// panel body did nothing (the second user test). `Always` extends the same
/// gesture to the mouse; widgets that sense their own drags (sliders, the
/// stack's reorder grip, the sheet handle) still win theirs, because the
/// scroll area's catch-all interact registers before its content and egui
/// resolves the overlap to the later registration.
pub(crate) fn panel_scroll_source() -> egui::scroll_area::ScrollSource {
    egui::scroll_area::ScrollSource {
        drag: egui::scroll_area::DragScroll::Always,
        ..Default::default()
    }
}

/// Where a hosted surface goes this frame: the placement the caller decides
/// so the body renderers never key anything on the width.
///
/// Two callers build these — the shell's floating pass here, and the phone
/// sheet (`ui_sheet.rs`), which is exactly why the type exists: the stack and
/// inspector keep one `Area` id and one internal structure whoever is
/// positioning them, and the only thing that changes hands is this geometry.
pub(super) struct SurfaceSlot {
    /// Where the area's pivot corner goes.
    pub pos: egui::Pos2,
    pub pivot: egui::Align2,
    /// The surface's content width.
    pub width: f32,
    /// Space for the whole surface, header included — each renderer charges
    /// its own header allowance against it before capping its scroll body.
    pub avail_height: f32,
    /// Hosted inside the phone sheet: `Order::Foreground` (above the scrim)
    /// and frameless (the sheet's own frame is the background). The frame
    /// choice is id-neutral — `Frame::show` creates one child `Ui` either
    /// way — which is what keeps the breakpoint id contract intact across
    /// the host switch.
    pub sheet: bool,
    /// The opacity the host wants the surface painted at — `1.0` at rest,
    /// less during a fade or a host's own transition. Anything below `1.0`
    /// also disables the contents (`fade::dim`): a transitioning surface
    /// must never catch a click.
    pub opacity: f32,
    /// Whether the surface may interact at all this frame — `false` for a
    /// closing remnant the host is still animating out: its state says
    /// closed, so its widgets must already be dead whatever the opacity
    /// still shows.
    pub interactive: bool,
}

/// What the shell produced this frame.
pub(super) struct ShellOutput {
    pub actions: Vec<GuiAction>,
    /// Screen rects of floating chrome drawn *over* the map, which map click
    /// handling must not treat as map clicks.
    ///
    /// This is an **output** of the shell rather than something the map
    /// reconstructs — only the code that draws a floating thing knows where it
    /// is. Empty in practice since the hamburger went: everything left either
    /// claims panel space or is an egui layer above `Background`, which the
    /// layer half of `is_pos_blocked` catches with no plumbing. The mechanism
    /// stays because painted-in-pane chrome has no layer to be caught by, and
    /// the next thing painted over a pane will need it again.
    pub excluded_rects: Vec<egui::Rect>,
    /// What the top bar left of the content rect — the rect the map's
    /// `CentralPanel` will fill, captured here so the floating surfaces (and
    /// the timeline, drawn after the pane loop) can position against it
    /// without re-deriving the top bar's height.
    pub map_rect: egui::Rect,
}

impl super::Gui {
    /// Draw all the chrome around the map: the one docked panel first, then
    /// the floating surfaces positioned in what it left.
    pub(super) fn render_shell(&mut self, ui: &mut egui::Ui) -> ShellOutput {
        let mut actions = Vec::new();

        self.render_top_bar(ui, &mut actions);

        // Everything the top bar did not claim is the map's — the full-bleed
        // rect every floating surface positions itself in.
        let map_rect = ui.available_rect_before_wrap();

        self.render_status_bar(ui.ctx(), map_rect, &mut actions);

        self.render_stack_and_inspector(ui.ctx(), map_rect, &mut actions);

        ShellOutput {
            actions,
            excluded_rects: Vec::new(),
            map_rect,
        }
    }

    /// The stack and the inspector, around the one take window — see the
    /// module note for the discipline this pass keeps.
    fn render_stack_and_inspector(
        &mut self,
        ctx: &egui::Context,
        map_rect: egui::Rect,
        actions: &mut Vec<GuiAction>,
    ) {
        // A Layer selection describes a map layer, and a pane with no map has
        // none — the stack shows it no rows to have selected one from. Snap
        // to the pane's own properties, which is what the inspector can still
        // truthfully say about it. Before the take, off the live pane — and
        // at every width: the sheet pass below the breakpoint relies on this
        // having run.
        if matches!(self.inspector_sel, InspectorSelection::Layer(_))
            && !self.panes[self.active_pane].is_map()
        {
            self.inspector_sel = InspectorSelection::PaneProps;
        }

        // Below the breakpoint the same flags present as sheet pages — the
        // sheet pass late in the frame hosts the same bodies through the same
        // take window (`ui_sheet.rs`); nothing floats at the map's corners.
        if self.layout.width == crate::ui_layout::WidthClass::Compact {
            return;
        }

        // The fade rule is total (§1.8): the fade closes both panels for
        // real, so this gate is moot in the steady state — it exists for the
        // fade-out transition, whose closing remnants dim with the rest of
        // the chrome, and as the stated rule should anything ever render
        // here while faded.
        let Some(fade) = self.chrome_fade() else {
            return;
        };

        // The slide animations (§3.3): each panel's open flag drives a
        // factor, and a closing panel renders as a non-interactive remnant
        // sliding off its own edge until the factor reaches zero. Under
        // `cfg(test)` the time is zero and the factors snap — see
        // `ui_fade::anim_time`.
        let stack_open = self.layers_panel_visible();
        let insp_open = self.insp_open;
        let stack_slide = ctx.animate_bool_with_time(
            egui::Id::new("stack_slide"),
            stack_open,
            super::fade::anim_time(),
        );
        let insp_slide = ctx.animate_bool_with_time(
            egui::Id::new("inspector_slide"),
            insp_open,
            super::fade::anim_time(),
        );
        if stack_slide <= 0.0 && insp_slide <= 0.0 {
            return;
        }

        let mut pane = std::mem::take(&mut self.panes[self.active_pane]);

        // The registry must hold *this* pane's configs before anything asks a
        // handler about itself — the status lines below, and the layer body's
        // round trip, both read handler state as "the active pane's". The
        // frame-end reload in `Gui::ui` usually guarantees it already, but an
        // active-pane switch earlier this same frame (the top bar's segments)
        // would leave the previous pane's configs loaded.
        if !pane.overlay_configs.is_empty() {
            self.overlays.load_pane_configs(&pane.overlay_configs);
        }

        let statuses: Vec<(OverlayKind, Option<String>)> = if stack_slide > 0.0 {
            self.stack_row_statuses(&pane)
        } else {
            Vec::new()
        };

        if stack_slide > 0.0 {
            // Sliding out to the left: the whole panel's travel is its width
            // plus both insets, so at factor zero nothing of it remains on
            // the map.
            let travel = (1.0 - stack_slide)
                * (super::ui_stack::STACK_WIDTH + 2.0 * super::ui_stack::STACK_INSET);
            let slot = SurfaceSlot {
                pos: map_rect.left_top()
                    + egui::vec2(
                        super::ui_stack::STACK_INSET - travel,
                        super::ui_stack::STACK_INSET,
                    ),
                pivot: egui::Align2::LEFT_TOP,
                width: super::ui_stack::STACK_WIDTH,
                avail_height: map_rect.height()
                    - super::ui_stack::STACK_INSET
                    - super::ui_stack::STACK_BOTTOM_CLEARANCE,
                sheet: false,
                opacity: fade,
                interactive: stack_open,
            };
            self.render_stack(ctx, slot, &mut pane, &statuses, actions);
        }
        if insp_slide > 0.0 {
            let travel = (1.0 - insp_slide)
                * (super::ui_inspector::INSPECTOR_WIDTH
                    + 2.0 * super::ui_inspector::INSPECTOR_INSET);
            let slot = SurfaceSlot {
                pos: map_rect.right_top()
                    + egui::vec2(
                        -super::ui_inspector::INSPECTOR_INSET + travel,
                        super::ui_inspector::INSPECTOR_INSET,
                    ),
                pivot: egui::Align2::RIGHT_TOP,
                width: super::ui_inspector::INSPECTOR_WIDTH,
                avail_height: map_rect.height()
                    - super::ui_inspector::INSPECTOR_INSET
                    - super::ui_inspector::INSPECTOR_BOTTOM_CLEARANCE,
                sheet: false,
                opacity: fade,
                interactive: insp_open,
            };
            self.render_inspector(ctx, slot, &mut pane, actions);
        }

        self.panes[self.active_pane] = pane;
        // After the restore, so the source it copies from is the real pane
        // rather than the `mem::take` placeholder. It deliberately does
        // **not** copy `content`: a pane's kind is how this pane presents the
        // shared subject, not part of the subject, and propagating it would
        // convert every sibling the moment one pane became a 3D view — from a
        // setting called "Sync Layers". The reasoning is written out on
        // `propagate_layer_sync` itself.
        self.propagate_layer_sync();
    }

    /// The stack rows' status lines, one per layer in the pane's own order —
    /// empty for a pane with no map, which has no rows to carry them.
    ///
    /// Radar's is the exception with a reason: the product and tilt are pane
    /// state — the radar handler holds only the layer toggle — so the line
    /// is read off the taken pane rather than asked of the registry. The
    /// caller must have loaded the pane's configs into the registry first;
    /// both the shell pass above and the sheet pass do.
    pub(super) fn stack_row_statuses(
        &self,
        pane: &PaneState,
    ) -> Vec<(OverlayKind, Option<String>)> {
        if !pane.is_map() {
            return Vec::new();
        }
        pane.draw_order
            .iter()
            .map(|&kind| {
                let line = if kind == OverlayKind::Radar {
                    radar_row_status(pane)
                } else {
                    self.overlays.status_line(kind)
                };
                (kind, line)
            })
            .collect()
    }
}

/// The Radar row's status line: what picture this pane's radar layer is —
/// product code and tilt, e.g. `REF - 0.5°`. `pub(super)` because the
/// inspector's Radar layer body states the same line (`ui_inspector.rs`).
///
/// The tilt is the *snapped* angle where a scan is loaded — the one the pane
/// is actually rendering — falling back to the raw selection before any scan
/// arrives. `None` while the layer is hidden: the dimmed row carries no line,
/// like every other layer's. `code()` is the fetch path's lowercase spelling,
/// uppercased here because the row is a display, not a URL.
pub(super) fn radar_row_status(pane: &PaneState) -> Option<String> {
    if !pane.is_overlay_enabled(OverlayKind::Radar) {
        return None;
    }
    let (product, tilt) = pane
        .get_rendering_params()
        .unwrap_or((pane.selected_product, pane.selected_elevation));
    Some(format!(
        "{} - {tilt:.1}\u{b0}",
        product.code().to_uppercase()
    ))
}

#[cfg(test)]
mod chrome_frame_tests {
    use super::chrome_frame;

    /// The persistent chrome's frame is the stock window frame minus the
    /// shadow — nothing else moves, so the theme contract holds.
    #[test]
    fn the_chrome_frame_is_the_stock_window_frame_without_its_shadow() {
        let style = egui::Style::default();
        let frame = chrome_frame(&style);
        let stock = egui::Frame::window(&style);
        assert_eq!(
            frame.shadow,
            egui::Shadow::NONE,
            "the chrome frame must cast no shadow - the timeline's smudge on \
             the status bar is the finding this pins"
        );
        assert_eq!(frame.fill, stock.fill, "the fill is the stock theme's");
        assert_eq!(
            frame.stroke, stock.stroke,
            "the stroke is the stock theme's"
        );
        assert_eq!(
            frame.corner_radius, stock.corner_radius,
            "the rounding is the stock theme's"
        );
        assert_eq!(
            frame.inner_margin, stock.inner_margin,
            "the margins are the stock theme's - the surfaces' own margin \
             math depends on them"
        );
    }

    /// `src` with comments, string literals and char literals blanked, so
    /// the scan below only ever matches *code* — a doc comment or an
    /// assertion message mentioning `Frame::window(` must not trip it.
    /// Nested block comments, escapes, raw strings and the `'"'` char
    /// literal are handled on the same terms as the glyph scanner
    /// (`ui_glyphs.rs`), whose directory walk this test mirrors too.
    fn code_only(src: &str) -> String {
        let chars: Vec<char> = src.chars().collect();
        let mut out = String::with_capacity(src.len());
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '/' if chars.get(i + 1) == Some(&'/') => {
                    while i < chars.len() && chars[i] != '\n' {
                        i += 1;
                    }
                }
                '/' if chars.get(i + 1) == Some(&'*') => {
                    let mut depth = 1;
                    i += 2;
                    while i < chars.len() && depth > 0 {
                        if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                            depth += 1;
                            i += 2;
                        } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                            depth -= 1;
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                }
                'r' if matches!(chars.get(i + 1), Some(&'"' | &'#')) => {
                    // A raw string opener, or just an `r` before a `#`.
                    let mut hashes = 0;
                    let mut j = i + 1;
                    while chars.get(j) == Some(&'#') {
                        hashes += 1;
                        j += 1;
                    }
                    if chars.get(j) == Some(&'"') {
                        j += 1;
                        while j < chars.len() {
                            if chars[j] == '"'
                                && (0..hashes).all(|k| chars.get(j + 1 + k) == Some(&'#'))
                            {
                                j += 1 + hashes;
                                break;
                            }
                            j += 1;
                        }
                        i = j;
                    } else {
                        out.push('r');
                        i += 1;
                    }
                }
                '"' => {
                    i += 1;
                    while i < chars.len() {
                        match chars[i] {
                            '\\' => i += 2,
                            '"' => {
                                i += 1;
                                break;
                            }
                            _ => i += 1,
                        }
                    }
                }
                '\'' => {
                    // A char literal is blanked; a lifetime's quote passes.
                    if chars.get(i + 1) == Some(&'\\') {
                        i += 2;
                        while i < chars.len() && chars[i] != '\'' {
                            i += 1;
                        }
                        i += 1;
                    } else if chars.get(i + 2) == Some(&'\'') {
                        i += 3;
                    } else {
                        out.push('\'');
                        i += 1;
                    }
                }
                c => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        out
    }

    /// Every persistent floating surface frames through [`chrome_frame`]:
    /// a direct `Frame::window` in shipping UI code is a shadowed frame
    /// waiting to ship. Self-maintaining (the M9 review retired a fixed
    /// five-file list a new chrome file would have escaped): every `.rs`
    /// under this crate's `src/` is walked, except test-named files —
    /// developer code on the glyph scan's own terms — and `ui_shell.rs`
    /// itself, where [`chrome_frame`] is built *from* the stock frame and
    /// the test above compares against it. The transient surfaces (dialogs,
    /// popovers, menus) keep their shadows deliberately, but they do so
    /// through `egui::Window`, which frames itself — nothing shipping needs
    /// a direct `Frame::window(`, and a legitimate future exception earns
    /// an explicit exemption here, with its reason.
    #[test]
    fn the_persistent_chrome_only_frames_through_chrome_frame() {
        let mut roots = vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")];
        let mut scanned = 0usize;
        while let Some(dir) = roots.pop() {
            for entry in std::fs::read_dir(&dir).expect("source dir must be readable") {
                let path = entry.expect("dir entry").path();
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if path.is_dir() {
                    roots.push(path);
                } else if name.ends_with(".rs") && name != "ui_shell.rs" && !name.contains("test") {
                    let src =
                        std::fs::read_to_string(&path).expect("chrome source must be readable");
                    scanned += 1;
                    assert!(
                        !code_only(&src).contains("Frame::window("),
                        "{name} builds a shadowed window frame directly - frame \
                         the surface through shell::chrome_frame instead, or \
                         exempt the file here saying why"
                    );
                }
            }
        }
        assert!(
            scanned > 30,
            "the scan visited only {scanned} sources - the walk is broken, \
             not the tree"
        );
    }

    /// The stripper itself: a broken one passes the scan vacuously, so the
    /// false-positive vectors it exists for — comments, strings, raw
    /// strings — and the code it must still see are each pinned.
    #[test]
    fn the_chrome_scan_reads_code_and_skips_prose() {
        let src = r##"
// a Frame::window( mention in a comment
/* and /* nested */ Frame::window( in a block */
const A: &str = "Frame::window( in a string";
const B: &str = r#"Frame::window( in a raw string"#;
const C: char = '"';
const D: &str = "after the char literal: Frame::window(";
"##;
        assert!(
            !code_only(src).contains("Frame::window("),
            "prose tripped the scan: {:?}",
            code_only(src)
        );
        assert!(
            code_only("let f = egui::Frame::window(&style);").contains("Frame::window("),
            "real code must still be seen"
        );
    }
}
