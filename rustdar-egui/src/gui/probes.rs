//! What the last frame drew, for the input harness: the [`Gui`]'s
//! test-only frame probes consolidated into [`FrameProbes`] (WO-E1), and
//! the control-tree probe that rides through production as a no-op.

#[cfg(test)]
use super::*;

/// Every per-frame probe the [`Gui`] records for the input harness: what
/// the last frame actually drew, field by field. Collapsed at WO-E1 from
/// thirty `#[cfg(test)]` fields on `Gui` into this one struct behind the
/// single gated `probes` field — the paths changed (`gui.last_x` became
/// `gui.probes.last_x`), the fields and their meanings did not.
#[cfg(test)]
pub(in crate::ui) struct FrameProbes {
    /// The map panel rect the last frame laid its pane grid out in. Only read
    /// by tests, which need the same rects `render_panes` used.
    pub last_map_panel_rect: egui::Rect,
    /// egui `Id`s the last frame's layers panel actually resolved, in render
    /// order. Only read by tests, which compare them either side of a resize:
    /// an `Id` that moved with the layout silently discards the widget memory
    /// egui keyed on it.
    pub widget_id_probes: Vec<(&'static str, egui::Id)>,
    /// Every menu leaf the last frame actually drew — whichever of the two
    /// presentations was on screen — with the bool each checkbox was really
    /// handed and the rect it landed in. Only read by tests, which need the
    /// state the *renderer* saw rather than the model a test rebuilt.
    pub last_menu_leaves: Vec<ui_menu::DrawnMenuLeaf>,
    /// The pointer state `render_panes` resolved for each pane on the last frame,
    /// in pane order. Only read by tests — and the *only* honest way for one to
    /// observe the modality gate, since resolving it a second time alongside
    /// `Gui::ui` would assert on a replica.
    pub last_pane_pointers: Vec<crate::ui_input::PanePointerProbe>,
    /// Which render arm ran for each pane on the last frame, in the order the
    /// pane loop reached them. Only read by tests — see [`PaneContentProbe`] for
    /// why this is written inside the arms rather than derived from
    /// `panes[i].kind()`.
    pub last_pane_content: Vec<PaneContentProbe>,
    /// What the 3D arm decided for each volume pane on the last frame. Only read
    /// by tests, and it is the only thing that can tell "drew a volume" from
    /// "drew nothing" — see [`map::VolumeArmProbe`].
    pub last_volume_arms: Vec<map::VolumeArmProbe>,
    /// The pane-count buttons the picker actually drew last frame. Only read by
    /// tests, which check the picker narrows on a phone while the config clamp
    /// does not, and that clicking one takes effect.
    pub last_pane_options: Vec<PaneOptionProbe>,
    /// The excluded rects `render_panes` was actually handed. Only read by tests,
    /// which check the chrome's rects reach the map's click filter rather than
    /// stopping at the call site.
    pub last_map_excluded_rects: Vec<egui::Rect>,
    /// The pane borders the last frame painted: pane index, the stroke's
    /// painted bounds, and whether it was the active highlight. Only read by
    /// tests — the M8 pin that every border lies inside its pane, at every
    /// grid position (the outside-stroke bug clipped the outer edges away).
    pub last_pane_borders: Vec<(usize, egui::Rect, bool)>,
    /// The section tracks the last frame painted over map panes: map pane,
    /// section pane, and the painted A and B endpoints. Only read by tests —
    /// the M8 pin that the release frame of a handle drag paints the dropped
    /// geometry, never the stale pre-drag line.
    pub last_section_tracks: Vec<(usize, usize, egui::Pos2, egui::Pos2)>,
    /// The committed region boxes the last frame painted over map panes: map
    /// pane, 3D pane, and the painted rect. Only read by tests — the pin that a
    /// box is drawn on the map it was picked on and on no other, which is the
    /// only on-screen answer to "where is that volume from".
    pub last_region_boxes: Vec<(usize, usize, egui::Rect)>,
    /// The Volume Alpha corner buttons the last frame drew, per pane. Only
    /// read by tests — the M8 pin that the fade hides pane-borne chrome too.
    pub last_alpha_buttons: Vec<(usize, egui::Rect)>,
    /// Each map pane's dispatched kinds in paint order, with the layer each
    /// painted into. Only read by tests — the draw-order pin; see
    /// `PaneRenderCtx::paint_order` for why the layer is the honest half.
    pub last_paint_order: Vec<(usize, Vec<(rustdar_source::id::LayerId, egui::LayerId)>)>,
    /// What the last frame's status bar actually drew. Only read by tests.
    pub last_status_bar: StatusBarProbe,
    /// What the last frame's timeline transport actually drew. Only read by
    /// tests.
    pub last_timeline: TimelineProbe,
    /// What the last frame's top bar actually drew. Only read by tests.
    pub last_top_bar: TopBarProbe,
    /// What the last frame's layer stack actually drew. Only read by tests.
    pub last_stack: StackProbe,
    /// What the last frame's inspector actually drew. Only read by tests —
    /// see [`InspectorProbe`] for why `mode` is written inside the body arms.
    pub last_inspector: InspectorProbe,
    /// What the last frame's Add-layer catalog actually drew. Only read by
    /// tests.
    pub last_catalog: CatalogProbe,
    /// What the last frame's pill rows actually drew, in pane order. Only
    /// read by tests.
    pub last_pills: Vec<pills::PillRowProbe>,
    /// The pill popover the last frame drew, if one was open. Only read by
    /// tests.
    pub last_pill_popover: Option<pills::PillPopoverProbe>,
    /// How many times handler `ControlItem`s were rendered this frame.
    ///
    /// The double-render guard: each render is a load→mutate→save round trip
    /// over the active pane's `overlay_configs`, so two passes in one frame
    /// fight over the handlers' state — the entanglement the plan's §3.8
    /// makes `render_overlay_controls_one` the only host to prevent. The
    /// harness asserts ≤ 1 after every frame.
    pub control_render_passes: u32,
    /// Every handler dropdown the last frame drew, with the text its collapsed
    /// box showed. Only read by tests — see [`DrawnDropdown`].
    pub last_dropdowns: Vec<DrawnDropdown>,
    /// Every control item the last frame's layers panel drew, whatever its
    /// shape — the generalisation of the field above. Only read by tests; see
    /// [`DrawnControlItem`].
    pub last_control_items: Vec<DrawnControlItem>,
    /// Every settings row the last frame's settings window drew. Only read by
    /// tests — see [`settings::DrawnSettingsRow`].
    pub last_settings_rows: Vec<settings::DrawnSettingsRow>,
    /// The action-button indices the last frame's detail popup reported as
    /// triggered, and the ones it actually handled. Only read by tests, which
    /// hold the second to at most one entry per frame — see the note on the
    /// handling in `ui_popups.rs`.
    pub last_popup_triggered: Vec<usize>,
    /// See [`last_popup_triggered`](Self::last_popup_triggered).
    pub last_popup_handled: Vec<usize>,
    /// What the last frame's bottom bar drew. Only read by tests.
    pub last_bottom_bar: BottomBarProbe,
    /// What the last frame's sheet drew. Only read by tests.
    pub last_sheet: SheetProbe,
    /// What the last frame's phone error toast drew. Only read by tests.
    pub last_error_toast: Option<ErrorToastProbe>,
}

#[cfg(test)]
impl Default for FrameProbes {
    fn default() -> Self {
        Self {
            last_map_panel_rect: egui::Rect::ZERO,
            widget_id_probes: Vec::new(),
            last_menu_leaves: Vec::new(),
            last_pane_pointers: Vec::new(),
            last_pane_content: Vec::new(),
            last_volume_arms: Vec::new(),
            last_pane_options: Vec::new(),
            last_map_excluded_rects: Vec::new(),
            last_pane_borders: Vec::new(),
            last_section_tracks: Vec::new(),
            last_region_boxes: Vec::new(),
            last_alpha_buttons: Vec::new(),
            last_paint_order: Vec::new(),
            last_status_bar: StatusBarProbe::default(),
            last_timeline: TimelineProbe::default(),
            last_top_bar: TopBarProbe::default(),
            last_stack: StackProbe::default(),
            last_inspector: InspectorProbe::default(),
            last_catalog: CatalogProbe::default(),
            last_pills: Vec::new(),
            last_pill_popover: None,
            control_render_passes: 0,
            last_dropdowns: Vec::new(),
            last_control_items: Vec::new(),
            last_settings_rows: Vec::new(),
            last_popup_triggered: Vec::new(),
            last_popup_handled: Vec::new(),
            last_bottom_bar: BottomBarProbe::default(),
            last_sheet: SheetProbe::default(),
            last_error_toast: None,
        }
    }
}

/// Test-only readbacks for the two frame-input fields no production getter
/// exists for. WO-E2's sentinel contract test asserts every `FrameInputs`
/// field surfaces after `Gui::apply_frame_inputs`; every other field has a
/// production getter to read it back through, these two do not — the UI
/// consumes them internally — so the readback lives here with the probes.
#[cfg(test)]
impl Gui {
    pub(crate) fn loop_frame_budget_for_test(&self) -> usize {
        self.loop_frame_budget
    }

    pub(crate) fn floor_tile_zoom_bias_for_test(&self) -> u8 {
        self.floor_tile_zoom_bias
    }
}

/// What one pass over a control tree drew. A no-op outside tests, like
/// [`ui_menu::MenuFrame`].
#[derive(Default)]
pub(crate) struct ControlProbe {
    #[cfg(test)]
    pub drawn: Vec<DrawnDropdown>,
    /// Every item drawn, whatever its shape. See [`DrawnControlItem`].
    #[cfg(test)]
    pub items: Vec<DrawnControlItem>,
}

impl ControlProbe {
    #[inline]
    pub(super) fn record_dropdown(
        &mut self,
        _id: &'static str,
        _label: &str,
        _selected_text: &str,
        _rect: egui::Rect,
    ) {
        #[cfg(test)]
        self.drawn.push(DrawnDropdown {
            id: _id,
            label: _label.to_owned(),
            selected_text: _selected_text.to_owned(),
            rect: _rect,
        });
    }

    /// Record one drawn item. Test-only, so the call sites are gated too —
    /// unlike [`Self::record_dropdown`] this takes a test-only type.
    #[cfg(test)]
    #[inline]
    pub(super) fn record_item(
        &mut self,
        handler: &rustdar_source::id::LayerId,
        kind: DrawnControlKind,
        label: &str,
        rect: egui::Rect,
    ) {
        self.items.push(DrawnControlItem {
            handler: Some(handler.clone()),
            label: label.to_owned(),
            kind,
            rect,
        });
    }
}
