//! The shell pass: the docked top bar, then the floating surfaces around the
//! full-bleed map.

use crate::actions::GuiAction;
use squallar_source::id::{LayerId, known};

use super::{InspectorSelection, PaneState};

/// The one frame every persistent floating surface draws in: the stock
/// window frame with the drop shadow removed.
pub(crate) fn chrome_frame(style: &egui::Style) -> egui::Frame {
    egui::Frame::window(style).shadow(egui::Shadow::NONE)
}

/// The scroll sources every panel `ScrollArea` accepts: the stock set plus
/// **mouse** drag-to-scroll.
pub(crate) fn panel_scroll_source() -> egui::scroll_area::ScrollSource {
    egui::scroll_area::ScrollSource {
        drag: egui::scroll_area::DragScroll::Always,
        ..Default::default()
    }
}

/// Where a hosted surface goes this frame: the placement the caller decides
/// so the body renderers never key anything on the width.
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
        // A Layer selection describes a map layer, and a pane that draws none
        // has none — the stack shows it no rows to have selected one from. Snap
        // to the pane's own properties, which is what the inspector can still
        // truthfully say about it. A 3D pane is *not* snapped: it draws the
        // layers and has the rows (`PaneState::draws_map_layers`), so a layer
        // selected on it describes something on screen. Before the take, off
        // the live pane — and at every width: the sheet pass below the
        // breakpoint relies on this having run.
        if matches!(self.inspector_sel, InspectorSelection::Layer(_))
            && !self.panes[self.active_pane].draws_map_layers()
        {
            self.inspector_sel = InspectorSelection::PaneProps;
        }

        // Below the breakpoint the same flags present as sheet pages — the
        // sheet pass late in the frame hosts the same bodies through the same
        // take window (`ui_sheet.rs`); nothing floats at the map's corners.
        if self.layout.width == crate::ui_layout::WidthClass::Compact {
            return;
        }

        // The fade rule is total (the plan): the fade closes both panels for
        // real, so this gate is moot in the steady state — it exists for the
        // fade-out transition, whose closing remnants dim with the rest of
        // the chrome, and as the stated rule should anything ever render
        // here while faded.
        let Some(fade) = self.chrome_fade() else {
            return;
        };

        // The slide animations (the plan): each panel's open flag drives a
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

        // Every question below is asked OF this pane: the status lines and
        // the layer body's round trip pass its own `PaneRef`, so there is no
        // "the registry is holding the wrong pane" to guard against. All this
        // has to do is make sure the slots it will be asked about have their
        // state.
        pane.hydrate_layer_states(&self.overlays, self.active_pane);

        let statuses: Vec<(LayerId, Option<String>)> = if stack_slide > 0.0 {
            self.stack_row_statuses(self.active_pane, &pane)
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
        // `propagate_pane_sync` itself.
        self.propagate_pane_sync();
    }

    /// The stack rows' status lines, one per layer in the pane's own order —
    /// empty for a pane that draws no map layers, which has no rows to carry
    /// them.
    pub(super) fn stack_row_statuses(
        &self,
        pane_idx: usize,
        pane: &PaneState,
    ) -> Vec<(LayerId, Option<String>)> {
        if !pane.draws_map_layers() {
            return Vec::new();
        }
        let view = pane.view(pane_idx);
        pane.draw_order()
            .map(|kind| {
                let line = if *kind == known::RADAR {
                    radar_row_status(pane)
                } else {
                    self.overlays.status_line(kind, &view.layer(kind))
                };
                (kind.clone(), line)
            })
            .collect()
    }
}

/// The Radar row's status line: what picture this pane's radar layer is —
/// product code and tilt, e.g. `REF - 0.5°`. `pub(super)` because the
/// inspector's Radar layer body states the same line (`ui_inspector.rs`).
pub(super) fn radar_row_status(pane: &PaneState) -> Option<String> {
    if !pane.is_overlay_enabled(&known::RADAR) {
        return None;
    }
    let (product, tilt) = pane
        .get_rendering_params()
        .unwrap_or((pane.selected_product(), pane.selected_elevation()));
    let code = crate::field_facts::code(&product).to_uppercase();
    if pane.render_view().reads_whole_volume() {
        return Some(code);
    }
    Some(format!("{code} - {tilt:.1}\u{b0}"))
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
