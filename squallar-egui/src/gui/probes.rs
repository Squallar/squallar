//! What the last frame drew, for the input harness: the [`Gui`]'s test-only
//! frame probes, and the control-tree probe that is a no-op in production.

#[cfg(test)]
use super::*;

/// Every per-frame probe the [`Gui`] records for the input harness: what
/// the last frame actually drew, field by field.
#[cfg(test)]
pub(in crate::ui) struct FrameProbes {
    pub last_map_panel_rect: egui::Rect,
    /// egui `Id`s the last frame's layers panel actually resolved, in render
    /// order. Tests compare them either side of a resize: an `Id` that moved
    /// with the layout silently discards the widget memory egui keyed on it.
    pub widget_id_probes: Vec<(&'static str, egui::Id)>,
    /// Every menu leaf the last frame actually drew — whichever of the two
    /// presentations was on screen — with the bool each checkbox was really
    /// handed and the rect it landed in.
    pub last_menu_leaves: Vec<ui_menu::DrawnMenuLeaf>,
    /// The pointer state `render_panes` resolved for each pane on the last frame,
    /// in pane order — the only honest way to observe the modality gate.
    pub last_pane_pointers: Vec<crate::ui_input::PanePointerProbe>,
    /// Which render arm ran for each pane on the last frame, in the order the
    /// pane loop reached them. See [`PaneContentProbe`] for why this is written
    /// inside the arms rather than derived from `panes[i].kind()`.
    pub last_pane_content: Vec<PaneContentProbe>,
    /// What the 3D arm decided for each volume pane on the last frame — the
    /// only thing that can tell "drew a volume" from "drew nothing". See
    /// [`map::VolumeArmProbe`].
    pub last_volume_arms: Vec<map::VolumeArmProbe>,
    /// The pane-count buttons the picker actually drew last frame.
    pub last_pane_options: Vec<PaneOptionProbe>,
    /// The split-orientation buttons beside them, likewise. Empty when the
    /// picker drew none.
    pub last_split_options: Vec<SplitOptionProbe>,
    /// The excluded rects `render_panes` was actually handed.
    pub last_map_excluded_rects: Vec<egui::Rect>,
    /// The pane borders the last frame painted: pane index, the stroke's
    /// painted bounds, and everything the border encodes — active, group and
    /// partial membership.
    pub last_pane_borders: Vec<(usize, egui::Rect, crate::ui::map::PaneBorderMarks)>,
    /// The section tracks the last frame painted over map panes: map pane,
    /// section pane, and the painted A and B endpoints.
    pub last_section_tracks: Vec<(usize, usize, egui::Pos2, egui::Pos2)>,
    /// The committed region boxes the last frame painted over map panes: map
    /// pane, 3D pane, and the painted rect.
    pub last_region_boxes: Vec<(usize, usize, egui::Rect)>,
    /// The Volume Alpha corner buttons the last frame drew, per pane.
    pub last_alpha_buttons: Vec<(usize, egui::Rect)>,
    /// Each map pane's dispatched kinds in paint order, with the layer each
    /// painted into — the draw-order pin.
    pub last_paint_order: Vec<(usize, Vec<(squallar_source::id::LayerId, egui::LayerId)>)>,
    pub last_status_bar: StatusBarProbe,
    pub last_timeline: TimelineProbe,
    pub last_top_bar: TopBarProbe,
    pub last_stack: StackProbe,
    pub last_inspector: InspectorProbe,
    pub last_catalog: CatalogProbe,
    pub last_pills: Vec<pills::PillRowProbe>,
    pub last_pill_popover: Option<pills::PillPopoverProbe>,
    pub control_render_passes: u32,
    pub last_dropdowns: Vec<DrawnDropdown>,
    pub last_control_items: Vec<DrawnControlItem>,
    pub last_settings_rows: Vec<settings::DrawnSettingsRow>,
    /// The action-button indices the last frame's detail popup reported as
    /// triggered, and the ones it actually handled.
    pub last_popup_triggered: Vec<usize>,
    pub last_popup_handled: Vec<usize>,
    pub last_bottom_bar: BottomBarProbe,
    pub last_sheet: SheetProbe,
    pub last_error_toast: Option<ErrorToastProbe>,
    /// Every basemap attribution the last frame drew, in draw order.
    ///
    /// A `Vec` rather than an `Option` because the defect this guards against
    /// is *more than one*: a credit moved inside the pane loop would draw once
    /// per pane, and an `Option` keeping only the last would read exactly like
    /// the correct case.
    pub last_attribution: Vec<egui::Rect>,
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
            last_split_options: Vec::new(),
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
            last_attribution: Vec::new(),
        }
    }
}

/// Test-only readbacks for the two frame-input fields no production getter
/// exists for — the UI consumes them internally.
#[cfg(test)]
impl Gui {
    pub(crate) fn loop_frame_budget_for_test(&self) -> usize {
        self.loop_frame_budget
    }

    pub(crate) fn floor_tile_zoom_bias_for_test(&self) -> u8 {
        self.floor_tile_zoom_bias
    }

    pub(crate) fn concurrent_renders_for_test(&self) -> usize {
        self.concurrent_renders
    }
}

/// What one pass over a control tree drew. A no-op outside tests, like
/// [`ui_menu::MenuFrame`].
#[derive(Default)]
pub(crate) struct ControlProbe {
    #[cfg(test)]
    pub drawn: Vec<DrawnDropdown>,
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

    /// Record one drawn item. Test-only, so the call sites are gated too.
    #[cfg(test)]
    #[inline]
    pub(super) fn record_item(
        &mut self,
        handler: &squallar_source::id::LayerId,
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
