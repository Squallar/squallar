//! One menu model, two renderers, one dispatcher.
//!
//! The menu used to exist twice: as a real `MenuBar` in `ui_desktop.rs`, and
//! hand-rolled again as a "Controls" block of buttons inside `ui_mobile.rs`'s
//! layers panel. The two drifted — the mobile copy had Refresh and Auto-poll,
//! the desktop one had the overlay toggles — and every new entry had to be
//! added in both places, in the right one of two files, or it silently existed
//! on only one platform.
//!
//! So the menu is described once as data ([`MenuNode`]), rendered by whichever
//! of the two presentations the current [`WidthClass`](crate::ui_layout::WidthClass)
//! calls for, and the resulting [`MenuEvent`]s are applied in exactly one
//! place. A new entry is one line in [`super::Gui::menu_model`] and one arm in
//! [`super::Gui::apply_menu_event`], and it appears in both presentations by
//! construction.

use crate::actions::GuiAction;
use rustdar_overlays::render::overlay_state::OverlayKind;

/// A command the user can invoke from the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MenuAction {
    Exit,
    RefreshRadar,
    OpenTimeDialog,
    OpenSettings,
}

/// A boolean the menu can flip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MenuToggle {
    /// Show/hide a map overlay on the active pane.
    Overlay(OverlayKind),
    /// Automatic polling for new scans.
    AutoPoll,
}

/// One entry in the menu.
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MenuEvent {
    Invoked(MenuAction),
    Toggled(MenuToggle, bool),
}

/// Render the model as a horizontal menu bar with drop-downs.
///
/// Used when there is room for one — see `WidthClass::has_menu_bar`.
pub(super) fn render_menu_bar(ui: &mut egui::Ui, nodes: &[MenuNode]) -> Vec<MenuEvent> {
    let mut events = Vec::new();
    egui::MenuBar::new().ui(ui, |ui| {
        for node in nodes {
            match node {
                MenuNode::Submenu { label, children } => {
                    ui.menu_button(*label, |ui| {
                        render_menu_items(ui, children, &mut events, true);
                    });
                }
                // A top-level leaf is unusual but has to render *somewhere*, or
                // adding one would silently drop it from this presentation only
                // — which is the failure this module exists to remove.
                _ => render_menu_items(ui, std::slice::from_ref(node), &mut events, true),
            }
        }
    });
    events
}

/// Render the model as a flat vertical list, for the slide-out drawer.
///
/// Used when there is no menu bar, and reached through the hamburger.
pub(super) fn render_menu_drawer(ui: &mut egui::Ui, nodes: &[MenuNode]) -> Vec<MenuEvent> {
    let mut events = Vec::new();
    for node in nodes {
        match node {
            MenuNode::Submenu { label, children } => {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(*label).strong());
                ui.indent(*label, |ui| {
                    render_menu_items(ui, children, &mut events, false);
                });
            }
            _ => render_menu_items(ui, std::slice::from_ref(node), &mut events, false),
        }
    }
    events
}

/// The shared leaf rendering. `in_menu` closes the drop-down after a command,
/// which is only meaningful inside a real menu.
fn render_menu_items(
    ui: &mut egui::Ui,
    nodes: &[MenuNode],
    events: &mut Vec<MenuEvent>,
    in_menu: bool,
) {
    for node in nodes {
        match node {
            MenuNode::Item { label, action } => {
                if ui.button(*label).clicked() {
                    events.push(MenuEvent::Invoked(*action));
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
                if ui.checkbox(&mut current, *label).changed() {
                    events.push(MenuEvent::Toggled(*toggle, current));
                }
            }
            MenuNode::Separator => {
                ui.separator();
            }
            // Nesting deeper than one level is not something either
            // presentation is built for; flatten rather than drop.
            MenuNode::Submenu { children, .. } => {
                render_menu_items(ui, children, events, in_menu);
            }
        }
    }
}

impl super::Gui {
    /// Build this frame's menu, reading the live state the toggles reflect.
    pub(super) fn menu_model(&self) -> Vec<MenuNode> {
        let pane = self.active_pane();
        vec![
            MenuNode::Submenu {
                label: "File",
                children: vec![
                    MenuNode::Item {
                        label: "Refresh Radar",
                        action: MenuAction::RefreshRadar,
                    },
                    MenuNode::Separator,
                    MenuNode::Item {
                        label: "Exit",
                        action: MenuAction::Exit,
                    },
                ],
            },
            MenuNode::Submenu {
                label: "View",
                children: vec![
                    MenuNode::Toggle {
                        label: "Show radar sites",
                        toggle: MenuToggle::Overlay(OverlayKind::RadarSites),
                        value: pane.is_overlay_enabled(OverlayKind::RadarSites),
                    },
                    MenuNode::Toggle {
                        label: "Show city labels",
                        toggle: MenuToggle::Overlay(OverlayKind::CityLabels),
                        value: pane.is_overlay_enabled(OverlayKind::CityLabels),
                    },
                    MenuNode::Separator,
                    MenuNode::Toggle {
                        label: "Auto-poll",
                        toggle: MenuToggle::AutoPoll,
                        value: self.auto_poll.enabled,
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
                actions.push(GuiAction::FetchRadarScan(self.radar.config.clone()));
            }
            MenuEvent::Invoked(MenuAction::OpenTimeDialog) => {
                self.time_dialog.show = true;
                // Close the drawer so the dialog is not hidden behind it. A
                // no-op when the drawer is not the current presentation.
                self.drawer_open = false;
            }
            MenuEvent::Invoked(MenuAction::OpenSettings) => {
                self.show_settings = true;
                self.drawer_open = false;
            }
            MenuEvent::Toggled(MenuToggle::Overlay(kind), on) => {
                self.active_pane_mut().set_overlay_enabled(kind, on);
                self.propagate_layer_sync();
            }
            MenuEvent::Toggled(MenuToggle::AutoPoll, on) => self.auto_poll.enabled = on,
        }
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
                    out.push(MenuEvent::Toggled(*toggle, !*value))
                }
                MenuNode::Separator => {}
            }
        }
    }

    /// Every command the model offers is handled. An unhandled entry is a
    /// button that visibly does nothing, which is worse than a missing one.
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
            let mut actions = Vec::new();
            gui.apply_menu_event(event, &mut actions);
        }
    }

    /// The toggles report live state rather than a constant, so the checkbox
    /// in the menu reflects what the map is actually doing.
    #[test]
    fn the_toggles_read_back_the_state_they_write() {
        let mut gui = Gui::new();

        let before = overlay_toggle(&gui, OverlayKind::RadarSites);
        let mut actions = Vec::new();
        gui.apply_menu_event(
            MenuEvent::Toggled(MenuToggle::Overlay(OverlayKind::RadarSites), !before),
            &mut actions,
        );
        assert_eq!(
            overlay_toggle(&gui, OverlayKind::RadarSites),
            !before,
            "the model must re-read the pane, not report a snapshot"
        );

        gui.apply_menu_event(MenuEvent::Toggled(MenuToggle::AutoPoll, false), &mut actions);
        assert!(!auto_poll_toggle(&gui));
        gui.apply_menu_event(MenuEvent::Toggled(MenuToggle::AutoPoll, true), &mut actions);
        assert!(auto_poll_toggle(&gui));
    }

    fn find_toggle(gui: &Gui, want: MenuToggle) -> bool {
        fn walk(nodes: &[MenuNode], want: MenuToggle) -> Option<bool> {
            for node in nodes {
                match node {
                    MenuNode::Submenu { children, .. } => {
                        if let Some(v) = walk(children, want) {
                            return Some(v);
                        }
                    }
                    MenuNode::Toggle { toggle, value, .. } if *toggle == want => {
                        return Some(*value);
                    }
                    _ => {}
                }
            }
            None
        }
        walk(&gui.menu_model(), want).expect("toggle missing from the menu model")
    }

    fn overlay_toggle(gui: &Gui, kind: OverlayKind) -> bool {
        find_toggle(gui, MenuToggle::Overlay(kind))
    }

    fn auto_poll_toggle(gui: &Gui) -> bool {
        find_toggle(gui, MenuToggle::AutoPoll)
    }

    /// Opening a dialog closes the drawer. On a compact screen the drawer
    /// covers most of the width, so leaving it open hides the dialog the user
    /// just asked for.
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
}
