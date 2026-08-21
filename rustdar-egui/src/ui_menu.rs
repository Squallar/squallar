//! One menu model, its renderers, one dispatcher.

use crate::actions::GuiAction;
use rustdar_source::id::{LayerId, known};

pub(crate) const VOLUME_PANE_LABEL: &str = "3D volume view";

pub(crate) const DRAW_CROSS_SECTION_LABEL: &str = "Draw cross-section";

pub(crate) const PICK_REGION_LABEL: &str = "Pick 3D region (drag a square on a map)";

/// A command the user can invoke from the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MenuAction {
    Exit,
    RefreshRadar,
    OpenTimeDialog,
    OpenSettings,
}

/// A boolean the menu can flip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MenuToggle {
    Overlay(LayerId),
    AutoPoll,
    /// Feed live panes from the real-time chunk bucket rather than polling the
    /// archive for completed volumes.
    LiveChunks,
    /// Subscribe to the push-notification service so a chunk is fetched the
    /// moment it exists rather than on the next poll.
    ChunkNotifications,
    /// Draw the active pane's ground in 3D, or go back to the plan view.
    VolumePane,
    /// Arm the cross-section draw: the next drag on a map pane becomes a
    /// vertical slice instead of a pan.
    DrawCrossSection,
    /// Arm the 3D region pick: the next drag on a map pane draws the square of
    /// ground a 3D view resamples, instead of panning.
    PickRegion,
}

pub(super) enum MenuNode {
    /// A named group. The menu bar renders it as a drop-down; the drawer
    /// renders it as a heading with its children beneath.
    Submenu {
        label: &'static str,
        children: Vec<MenuNode>,
    },
    Item {
        label: &'static str,
        action: MenuAction,
    },
    Toggle {
        label: &'static str,
        toggle: MenuToggle,
        value: bool,
    },
    Separator,
}

/// Something the user did to the menu this frame, to be handed to
/// [`super::Gui::apply_menu_event`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MenuEvent {
    Invoked(MenuAction),
    Toggled(MenuToggle, bool),
}

/// One leaf a presentation actually put on screen: the bool `ui.checkbox` was
/// really handed, and where the widget landed so a test can click it for real.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DrawnMenuLeaf {
    pub label: &'static str,
    /// `Some(state)` for a toggle, `None` for a command or a submenu header.
    pub value: Option<bool>,
    pub rect: egui::Rect,
    pub id: egui::Id,
}

/// What one presentation produced this frame.
#[derive(Default)]
pub(super) struct MenuFrame {
    pub events: Vec<MenuEvent>,
    #[cfg(test)]
    pub drawn: Vec<DrawnMenuLeaf>,
}

impl MenuFrame {
    /// Record a leaf that was drawn. A no-op outside tests.
    #[inline]
    fn record(&mut self, _label: &'static str, _value: Option<bool>, _response: &egui::Response) {
        #[cfg(test)]
        self.drawn.push(DrawnMenuLeaf {
            label: _label,
            value: _value,
            rect: _response.rect,
            id: _response.id,
        });
    }
}

/// Render the model as one flat dropdown list, for the top bar's ☰ popup.
pub(super) fn render_menu_popup(ui: &mut egui::Ui, nodes: &[MenuNode]) -> MenuFrame {
    let mut out = MenuFrame::default();
    for (i, node) in nodes.iter().enumerate() {
        match node {
            MenuNode::Submenu { children, .. } => {
                if i > 0 {
                    ui.separator();
                }
                render_menu_items(ui, children, &mut out, true);
            }
            // A top-level leaf is unusual but has to render *somewhere*, or
            // adding one would silently drop it from this presentation only.
            _ => render_menu_items(ui, std::slice::from_ref(node), &mut out, true),
        }
    }
    out
}

/// Render the model as a flat vertical list — the phone sheet's Menu page
/// (`ui_sheet.rs`): headings over indented leaves, because a sheet page is a
/// document the eye scans, not a dropdown the pointer sweeps.
pub(super) fn render_menu_drawer(ui: &mut egui::Ui, nodes: &[MenuNode]) -> MenuFrame {
    let mut out = MenuFrame::default();
    for node in nodes {
        match node {
            MenuNode::Submenu { label, children } => {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(*label).strong());
                ui.indent(*label, |ui| {
                    render_menu_items(ui, children, &mut out, false);
                });
            }
            _ => render_menu_items(ui, std::slice::from_ref(node), &mut out, false),
        }
    }
    out
}

/// The shared leaf rendering. `in_menu` closes the drop-down after a command,
/// which is only meaningful inside a real menu.
fn render_menu_items(ui: &mut egui::Ui, nodes: &[MenuNode], out: &mut MenuFrame, in_menu: bool) {
    for node in nodes {
        match node {
            MenuNode::Item { label, action } => {
                let response = ui.button(*label);
                out.record(label, None, &response);
                if response.clicked() {
                    out.events.push(MenuEvent::Invoked(*action));
                    if in_menu {
                        ui.close_kind(egui::UiKind::Menu);
                    }
                }
            }
            MenuNode::Toggle {
                label,
                toggle,
                value,
            } => {
                let mut current = *value;
                let response = ui.checkbox(&mut current, *label);
                // `*value`, not `current`: what the checkbox was *handed*.
                out.record(label, Some(*value), &response);
                if response.changed() {
                    out.events.push(MenuEvent::Toggled(toggle.clone(), current));
                }
            }
            MenuNode::Separator => {
                ui.separator();
            }
            // Nesting deeper than one level is not something either
            // presentation is built for; flatten rather than drop.
            MenuNode::Submenu { children, .. } => {
                render_menu_items(ui, children, out, in_menu);
            }
        }
    }
}

impl super::Gui {
    /// Build this frame's menu, reading the live state the toggles reflect.
    pub(super) fn menu_model(&self) -> Vec<MenuNode> {
        let pane = self.active_pane();
        let mut file = vec![MenuNode::Item {
            label: "Refresh Radar",
            action: MenuAction::RefreshRadar,
        }];
        // Omitted where the platform has no quit (iOS): `request_exit` returns
        // early there, so the entry would be a button that does nothing.
        if self.supports_exit {
            file.push(MenuNode::Separator);
            file.push(MenuNode::Item {
                label: "Exit",
                action: MenuAction::Exit,
            });
        }
        vec![
            MenuNode::Submenu {
                label: "File",
                children: file,
            },
            MenuNode::Submenu {
                label: "View",
                children: vec![
                    // First, because it decides what the entries under it even
                    // apply to. `pane` is `self.active_pane()`, so every host must
                    // build this model *outside* the frame's `mem::take` windows:
                    // inside one the slot holds a default `PaneState`.
                    MenuNode::Toggle {
                        label: VOLUME_PANE_LABEL,
                        toggle: MenuToggle::VolumePane,
                        value: pane.render_view() == rustdar_radar::types::RenderView::Volume,
                    },
                    // Read off the *global* flag rather than off `pane`: it arms
                    // a gesture, and which pane the gesture ends up aiming is
                    // decided by where the line is drawn.
                    MenuNode::Toggle {
                        label: DRAW_CROSS_SECTION_LABEL,
                        toggle: MenuToggle::DrawCrossSection,
                        value: self.section_draw_armed(),
                    },
                    // The two armed drags are adjacent on purpose: they are
                    // mutually exclusive, so ticking either un-ticks the other.
                    // Global, for the reason above.
                    MenuNode::Toggle {
                        label: PICK_REGION_LABEL,
                        toggle: MenuToggle::PickRegion,
                        value: self.region_pick_armed(),
                    },
                    MenuNode::Separator,
                    MenuNode::Toggle {
                        label: "Show radar sites",
                        toggle: MenuToggle::Overlay(known::RADAR_SITES),
                        value: pane.is_overlay_enabled(&known::RADAR_SITES),
                    },
                    MenuNode::Toggle {
                        label: "Show city labels",
                        toggle: MenuToggle::Overlay(known::CITY_LABELS),
                        value: pane.is_overlay_enabled(&known::CITY_LABELS),
                    },
                    MenuNode::Separator,
                    MenuNode::Toggle {
                        label: rustdar_radar::source::AUTO_POLL_LABEL,
                        toggle: MenuToggle::AutoPoll,
                        value: crate::radar_layer::auto_poll_enabled(&self.overlays),
                    },
                    MenuNode::Toggle {
                        label: rustdar_radar::source::LIVE_CHUNKS_LABEL,
                        toggle: MenuToggle::LiveChunks,
                        value: crate::radar_layer::live_chunks_enabled(self),
                    },
                    MenuNode::Toggle {
                        label: rustdar_radar::source::CHUNK_NOTIFICATIONS_LABEL,
                        toggle: MenuToggle::ChunkNotifications,
                        value: crate::radar_layer::chunk_notifications_enabled(self),
                    },
                    MenuNode::Separator,
                    MenuNode::Item {
                        label: "Time...",
                        action: MenuAction::OpenTimeDialog,
                    },
                    MenuNode::Item {
                        label: "Settings...",
                        action: MenuAction::OpenSettings,
                    },
                ],
            },
        ]
    }

    /// Apply one menu event. The only place menu semantics live.
    pub(super) fn apply_menu_event(&mut self, event: MenuEvent, actions: &mut Vec<GuiAction>) {
        match event {
            MenuEvent::Invoked(MenuAction::Exit) => actions.push(GuiAction::Exit),
            MenuEvent::Invoked(MenuAction::RefreshRadar) => {
                // The active pane's site, not the persisted global one —
                // see `active_pane_fetch_config`.
                actions.push(GuiAction::FetchRadarScan(self.active_pane_fetch_config()));
            }
            MenuEvent::Invoked(MenuAction::OpenTimeDialog) => {
                self.time_dialog.show = true;
                // Close the layers drawer so the dialog is not hidden behind it.
                self.drawer_open = false;
            }
            MenuEvent::Invoked(MenuAction::OpenSettings) => {
                // The inspector's App › Settings body. The drawer yields: on a
                // narrow width it covers most of the screen.
                self.open_settings();
                self.drawer_open = false;
            }
            MenuEvent::Toggled(MenuToggle::Overlay(kind), on) => {
                self.set_active_pane_overlay(&kind, on);
                self.propagate_layer_sync();
            }
            MenuEvent::Toggled(MenuToggle::AutoPoll, on) => self.set_auto_poll_enabled(on),
            MenuEvent::Toggled(MenuToggle::LiveChunks, on) => self.apply_layer_control(
                &crate::radar_layer::POLL_LAYER,
                &crate::radar_layer::live_chunks_update(on),
            ),
            MenuEvent::Toggled(MenuToggle::ChunkNotifications, on) => self.apply_layer_control(
                &crate::radar_layer::POLL_LAYER,
                &crate::radar_layer::chunk_notifications_update(on),
            ),
            MenuEvent::Toggled(MenuToggle::VolumePane, on) => {
                // Recorded rather than written, through the one route the UI has.
                self.request_pane_view(
                    self.active_pane,
                    if on {
                        rustdar_radar::types::RenderView::Volume
                    } else {
                        rustdar_radar::types::RenderView::PlanView
                    },
                );
            }
            MenuEvent::Toggled(MenuToggle::DrawCrossSection, on) => {
                // A direct write, and it may be one: the flag is on `Gui` rather
                // than on a pane, so no `mem::take` window can swallow it. The
                // setter exists because *disarming* drops a half-drawn anchor.
                self.set_section_draw_armed(on);
                // Closing the layers drawer is the point, not a courtesy: on a
                // narrow width it covers the map the line has to be drawn on. The
                // ☰ dropdown closes itself on arm for the same reason.
                if on {
                    self.drawer_open = false;
                }
            }
            MenuEvent::Toggled(MenuToggle::PickRegion, on) => {
                // The arm above, with the box in place of the line — same direct
                // write, same setter, same drawer close.
                self.set_region_pick_armed(on);
                if on {
                    self.drawer_open = false;
                }
            }
        }
    }
}

/// Collect every leaf label under `nodes`, submenus flattened, in model order.
#[cfg(test)]
fn collect_leaf_labels(nodes: &[MenuNode], out: &mut Vec<&'static str>) {
    for node in nodes {
        match node {
            MenuNode::Submenu { children, .. } => collect_leaf_labels(children, out),
            MenuNode::Item { label, .. } | MenuNode::Toggle { label, .. } => out.push(label),
            MenuNode::Separator => {}
        }
    }
}

#[cfg(test)]
impl super::Gui {
    /// Every leaf label the menu model currently offers, submenus flattened —
    /// the inventory the parity walk asserts against the drawn
    /// [`DrawnMenuLeaf`]s.
    pub(crate) fn menu_model_leaf_labels(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        collect_leaf_labels(&self.menu_model(), &mut out);
        out
    }

    /// The model's top-level groups: each submenu header with the leaf labels
    /// under it, in model order — how the menu-bar presentation has to be
    /// walked, one drop-down at a time.
    pub(crate) fn menu_model_groups(&self) -> Vec<(&'static str, Vec<&'static str>)> {
        self.menu_model()
            .iter()
            .filter_map(|node| match node {
                MenuNode::Submenu { label, children } => {
                    let mut leaves = Vec::new();
                    collect_leaf_labels(children, &mut leaves);
                    Some((*label, leaves))
                }
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Gui;

    fn leaves(nodes: &[MenuNode], out: &mut Vec<MenuEvent>) {
        for node in nodes {
            match node {
                MenuNode::Submenu { children, .. } => leaves(children, out),
                MenuNode::Item { action, .. } => out.push(MenuEvent::Invoked(*action)),
                MenuNode::Toggle { toggle, value, .. } => {
                    out.push(MenuEvent::Toggled(toggle.clone(), !*value))
                }
                MenuNode::Separator => {}
            }
        }
    }

    /// Everything a menu entry is allowed to move, as one value. Coarse on
    /// purpose: an entry whose effect is invisible here is one whose effect is
    /// invisible to the user too.
    fn state_fingerprint(gui: &Gui) -> String {
        let mut overlays: Vec<(String, bool)> = gui
            .active_pane()
            .layers
            .iter()
            .map(|slot| (format!("{:?}", slot.id), slot.enabled))
            .collect();
        overlays.sort();
        format!(
            "settings={} insp={} sel={:?} time={} drawer={} auto_poll={} live_chunks={} \
             notify={} view={:?} pending_view={:?} armed={} \
             overlays={overlays:?}",
            gui.settings_visible(),
            gui.insp_open,
            gui.inspector_sel,
            gui.time_dialog.show,
            gui.drawer_open,
            crate::radar_layer::auto_poll_enabled(&gui.overlays),
            crate::radar_layer::live_chunks_enabled(gui),
            crate::radar_layer::chunk_notifications_enabled(gui),
            gui.active_pane().render_view(),
            // Both halves, because a pane view change is deliberately a two-step
            // operation: recording the request is the whole of what the dispatcher's
            // arm does, so a fingerprint holding only the *applied* view would
            // report the arm as a no-op.
            gui.pending_pane_view_for_test(),
            // The armed draw is a mode with no other observable — it converts
            // nothing until a gesture completes — so without it the toggle's arm
            // would read as a no-op.
            gui.section_draw_armed(),
        )
    }

    /// Every command the model offers actually *does* something.
    #[test]
    fn every_menu_entry_has_a_dispatcher_arm() {
        let mut gui = Gui::new();
        let mut events = Vec::new();
        leaves(&gui.menu_model(), &mut events);
        assert!(
            events.len() >= 6,
            "precondition: the model should have real content, found {}",
            events.len()
        );

        for event in events {
            let before = state_fingerprint(&gui);
            let mut actions = Vec::new();
            gui.apply_menu_event(event.clone(), &mut actions);
            let after = state_fingerprint(&gui);

            assert!(
                !actions.is_empty() || after != before,
                "{event:?} dispatched to a no-op: it emitted no GuiAction and \
                 changed nothing observable, so the menu entry is a button that \
                 does nothing when clicked"
            );
        }
    }

    /// The toggles report live state rather than a constant, so the checkbox
    /// in the menu reflects what the map is actually doing.
    #[test]
    fn the_toggles_read_back_the_state_they_write() {
        let mut gui = Gui::new();

        let before = overlay_toggle(&gui, known::RADAR_SITES);
        let mut actions = Vec::new();
        gui.apply_menu_event(
            MenuEvent::Toggled(MenuToggle::Overlay(known::RADAR_SITES), !before),
            &mut actions,
        );
        assert_eq!(
            overlay_toggle(&gui, known::RADAR_SITES),
            !before,
            "the model must re-read the pane, not report a snapshot"
        );

        gui.apply_menu_event(
            MenuEvent::Toggled(MenuToggle::AutoPoll, false),
            &mut actions,
        );
        assert!(!auto_poll_toggle(&gui));
        gui.apply_menu_event(MenuEvent::Toggled(MenuToggle::AutoPoll, true), &mut actions);
        assert!(auto_poll_toggle(&gui));
    }

    fn find_toggle(gui: &Gui, want: &MenuToggle) -> bool {
        fn walk(nodes: &[MenuNode], want: &MenuToggle) -> Option<bool> {
            for node in nodes {
                match node {
                    MenuNode::Submenu { children, .. } => {
                        if let Some(v) = walk(children, want) {
                            return Some(v);
                        }
                    }
                    MenuNode::Toggle { toggle, value, .. } if toggle == want => {
                        return Some(*value);
                    }
                    _ => {}
                }
            }
            None
        }
        walk(&gui.menu_model(), want).expect("toggle missing from the menu model")
    }

    fn overlay_toggle(gui: &Gui, kind: LayerId) -> bool {
        find_toggle(gui, &MenuToggle::Overlay(kind))
    }

    fn auto_poll_toggle(gui: &Gui) -> bool {
        find_toggle(gui, &MenuToggle::AutoPoll)
    }

    /// The 3D toggle reads the *active pane's* kind, not a global flag.
    #[test]
    fn the_volume_toggle_describes_the_active_pane_and_no_other() {
        let mut gui = Gui::new();
        gui.set_pane_count_for_test(2);
        assert!(
            !find_toggle(&gui, &MenuToggle::VolumePane),
            "precondition: two fresh map panes"
        );

        gui.pane_mut(1)
            .unwrap()
            .set_view(rustdar_radar::types::RenderView::Volume);
        assert!(
            !find_toggle(&gui, &MenuToggle::VolumePane),
            "the toggle read some other pane's kind: pane 0 is the active one and \
             it is still a map"
        );

        gui.active_pane = 1;
        assert!(find_toggle(&gui, &MenuToggle::VolumePane));
    }

    /// Unticking the 3D toggle asks for a map back, rather than doing nothing.
    #[test]
    fn the_volume_toggle_converts_in_both_directions() {
        let mut gui = Gui::new();
        let mut actions = Vec::new();

        gui.apply_menu_event(
            MenuEvent::Toggled(MenuToggle::VolumePane, true),
            &mut actions,
        );
        assert_eq!(
            gui.pending_pane_view_for_test(),
            Some((0, rustdar_radar::types::RenderView::Volume))
        );

        gui.apply_menu_event(
            MenuEvent::Toggled(MenuToggle::VolumePane, false),
            &mut actions,
        );
        assert_eq!(
            gui.pending_pane_view_for_test(),
            Some((0, rustdar_radar::types::RenderView::PlanView)),
            "unticking the box asked for a volume pane again, so a pane converted \
             by accident can never be converted back"
        );

        assert!(
            actions.is_empty(),
            "converting a pane is local to the Gui and needs nothing of the host"
        );
    }

    /// Opening a dialog closes the drawer. On a compact screen the drawer
    /// covers most of the width, so leaving it open hides the dialog.
    #[test]
    fn opening_a_dialog_from_the_drawer_closes_it() {
        for (event, opened) in [
            (
                MenuEvent::Invoked(MenuAction::OpenSettings),
                "settings" as &str,
            ),
            (MenuEvent::Invoked(MenuAction::OpenTimeDialog), "time"),
        ] {
            let mut gui = Gui::new();
            gui.drawer_open = true;
            let mut actions = Vec::new();
            gui.apply_menu_event(event, &mut actions);
            assert!(!gui.drawer_open, "{opened} dialog left the drawer open");
        }
    }

    /// An overlay detail item, so the pager popup can be opened without a map
    /// click. The concrete items are `pub(crate)` to `rustdar-overlays`; the
    /// trait is not.
    #[derive(Debug)]
    struct StubOverlayItem;

    impl rustdar_overlays::render::overlay_state::OverlayItem for StubOverlayItem {
        fn layer_id(&self) -> rustdar_source::id::LayerId {
            rustdar_source::id::known::NWS_ALERTS
        }
        fn popup_content(
            &self,
            _prefs: &rustdar_units::UserPreferences,
        ) -> rustdar_overlays::render::overlay_state::PopupContent {
            rustdar_overlays::render::overlay_state::PopupContent {
                title: "Stub".to_owned(),
                accent_rgb: [255, 0, 0],
                width: 300.0,
                sections: Vec::new(),
                actions: Vec::new(),
            }
        }
        fn matches(
            &self,
            _other: &dyn rustdar_overlays::render::overlay_state::OverlayItem,
        ) -> bool {
            false
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    /// Escape and back close what is open, one layer per press, and say so.
    #[test]
    fn a_back_press_closes_one_open_layer_at_a_time() {
        let mut gui = Gui::new();
        gui.drawer_open = true;
        // A non-default selection, so a close that rewrote it would show.
        gui.select_layer(known::NWS_ALERTS);
        gui.time_dialog.show = true;
        gui.overlays.selected_overlays = vec![std::sync::Arc::new(StubOverlayItem)];
        gui.overlays.selected_overlay_page = 0;

        assert!(gui.dismiss_top_layer(), "the overlay pager was open");
        assert!(
            gui.overlays.selected_overlays.is_empty(),
            "the overlay pager did not close"
        );
        assert!(
            gui.insp_open && gui.time_dialog.show && gui.drawer_open,
            "closing the pager took a layer under it with it: {}",
            state_fingerprint(&gui)
        );

        assert!(gui.dismiss_top_layer(), "the time dialog was open");
        assert!(!gui.time_dialog.show, "the time dialog did not close");
        assert!(
            gui.insp_open && gui.drawer_open,
            "one press closed more than one layer: {}",
            state_fingerprint(&gui)
        );

        assert!(gui.dismiss_top_layer(), "the inspector was open");
        assert!(!gui.insp_open, "the inspector did not close");
        assert_eq!(
            gui.inspector_sel,
            crate::ui::InspectorSelection::Layer(known::NWS_ALERTS),
            "a dismissal must LEAVE the selection alone: the panel has one \
             close now, and reopening returns the body the user was reading"
        );
        assert!(gui.drawer_open, "the drawer went with it");

        assert!(gui.dismiss_top_layer(), "the drawer was open");
        assert!(!gui.drawer_open, "the drawer did not close");

        assert!(
            !gui.dismiss_top_layer(),
            "reported something dismissed with nothing open, so a press would \
             never reach the exit path at all: {}",
            state_fingerprint(&gui)
        );
    }
}
