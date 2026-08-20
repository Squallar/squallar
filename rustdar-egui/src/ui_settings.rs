use crate::actions::GuiAction;
use rustdar_location::HeadingSource;
use rustdar_units::{
    DistanceUnit, HailSizeUnit, HeightUnit, PrecipRateUnit, SpeedUnit, TemperatureUnit,
    TimezonePreference, UnitLabel, UserPreferences,
};

const SETTINGS_SMALL_SPACING: f32 = 4.0;
const SETTINGS_LARGE_SPACING: f32 = 8.0;
#[cfg(feature = "gps-serial")]
const GPS_BAUD_RATES: &[u32] = &[4800, 9600, 38400, 115200];

/// The storm motion override switch's label.
pub(crate) const STORM_MOTION_OVERRIDE_LABEL: &str = "Override the storm motion vector";

/// What actually leaves the machine when the user says yes, in the pane that
/// asks.
#[cfg(target_os = "linux")]
const LOCATION_EGRESS_NOTE: &str = "Approximate, from your system's location \
    service. Finding a position sends your IP address, and - if the Wi-Fi \
    backend is enabled - the identifiers of nearby wireless networks, to \
    api.beacondb.net.";
#[cfg(not(target_os = "linux"))]
const LOCATION_EGRESS_NOTE: &str = "Approximate, from your device's location \
    service. Finding a position may send your IP address and details of nearby \
    wireless networks to that service's provider.";

/// Where a user actually goes to undo a refusal, in the pane that reports one.
#[cfg(target_os = "linux")]
pub(crate) const LOCATION_DENIED_NOTE: &str = "Your desktop's location switch \
    is off, so the portal refused. GNOME has this under Settings \u{203a} \
    Privacy; most other desktops have no page for it, and this works \
    everywhere:\n\
    \n\
    gsettings set org.gnome.system.location enabled true";
/// See the Linux arm above.
#[cfg(not(target_os = "linux"))]
pub(crate) const LOCATION_DENIED_NOTE: &str = "Location for this app is turned \
    off. It can be turned back on in your system settings.";

/// Every row the settings window draws, in draw order, each under a stable id.
pub(crate) const SETTINGS_ROWS: &[&str] = &[
    "units.timezone",
    "units.temperature",
    "units.speed",
    "units.distance",
    "units.height",
    "units.precip_rate",
    "units.hail_size",
    "interface.pin_controls",
    "location",
    "gps.port",
    "gps.baud",
    "gps.connect",
    "heading",
    "storm.fallback",
    "storm.override",
    "storm.speed",
    "storm.direction",
    "advanced.notifier",
    "data.auto_poll",
    "data.live_chunks",
    "data.push",
    "data.refresh",
    "about.version",
    "about.platform",
    "reset",
    "about.exit",
];

/// One settings row the window actually drew: which [`SETTINGS_ROWS`] id it
/// was, and where it landed so a test can find it on screen.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DrawnSettingsRow {
    pub id: &'static str,
    pub rect: egui::Rect,
}

/// The chrome between two settings groups: breathing room, a rule, and the
/// smaller lead-in the next group's content sits under.
fn section_break(ui: &mut egui::Ui) {
    ui.add_space(SETTINGS_LARGE_SPACING);
    ui.separator();
    ui.add_space(SETTINGS_SMALL_SPACING);
}

/// Whether a `DragValue` is mid-edit — being dragged, or holding the keyboard
/// while a number is typed into it.
fn mid_edit(response: &egui::Response) -> bool {
    response.dragged() || response.has_focus()
}

impl super::Gui {
    /// The settings content — the inspector's App › Settings body.
    pub(super) fn render_settings_body(
        &mut self,
        ui: &mut egui::Ui,
        pane: &crate::pane::PaneState,
        actions: &mut Vec<GuiAction>,
    ) {
        self.storm_motion_editing = false;
        for &row in SETTINGS_ROWS {
            #[cfg(test)]
            let row_top = ui.cursor().top();
            let drawn = self.render_settings_row(ui, row, pane, actions);
            #[cfg(test)]
            if drawn {
                self.probes.last_settings_rows.push(DrawnSettingsRow {
                    id: row,
                    rect: egui::Rect::from_x_y_ranges(
                        ui.max_rect().x_range(),
                        row_top..=ui.cursor().top(),
                    ),
                });
            }
            #[cfg(not(test))]
            let _ = drawn;
        }
    }

    /// Draw one row of [`SETTINGS_ROWS`]. Returns whether anything was drawn,
    /// which is `false` only for a row this build compiles out (the GPS rows
    /// without the `gps-serial` feature) or this platform withholds (the Exit
    /// row where [`Gui::supports_exit`](super::Gui::supports_exit) says no —
    /// the same gate that drops the menu's Exit entry).
    fn render_settings_row(
        &mut self,
        ui: &mut egui::Ui,
        id: &str,
        pane: &crate::pane::PaneState,
        actions: &mut Vec<GuiAction>,
    ) -> bool {
        match id {
            "units.timezone" => {
                ui.heading("Units");
                ui.add_space(SETTINGS_SMALL_SPACING);
                unit_combo(
                    ui,
                    "Timezone",
                    &mut self.preferences.timezone,
                    TimezonePreference::ALL,
                );
                true
            }
            "units.temperature" => {
                unit_combo(
                    ui,
                    "Temperature",
                    &mut self.preferences.temperature,
                    TemperatureUnit::ALL,
                );
                true
            }
            "units.speed" => {
                unit_combo(ui, "Speed", &mut self.preferences.speed, SpeedUnit::ALL);
                true
            }
            "units.distance" => {
                unit_combo(
                    ui,
                    "Distance",
                    &mut self.preferences.distance,
                    DistanceUnit::ALL,
                );
                true
            }
            "units.height" => {
                unit_combo(ui, "Height", &mut self.preferences.height, HeightUnit::ALL);
                true
            }
            "units.precip_rate" => {
                unit_combo(
                    ui,
                    "Precip rate",
                    &mut self.preferences.precip_rate,
                    PrecipRateUnit::ALL,
                );
                true
            }
            "units.hail_size" => {
                unit_combo(
                    ui,
                    "Hail size",
                    &mut self.preferences.hail_size,
                    HailSizeUnit::ALL,
                );
                true
            }
            "interface.pin_controls" => {
                section_break(ui);
                ui.heading("Interface");
                ui.add_space(SETTINGS_SMALL_SPACING);
                ui.checkbox(&mut self.pin_pane_controls, "Pin pane controls");
                ui.label(
                    egui::RichText::new(
                        "Unpinned, each pane's pill row idles translucent and \
                         wakes when the pointer is over the pane - or, \
                         on touch, on a first tap.",
                    )
                    .small()
                    .weak(),
                );
                true
            }
            "location" => {
                section_break(ui);
                ui.heading("Location");
                ui.add_space(SETTINGS_SMALL_SPACING);
                self.render_location_controls(ui, actions);
                section_break(ui);
                true
            }
            #[cfg(feature = "gps-serial")]
            "gps.port" => {
                ui.heading("GPS");
                ui.add_space(SETTINGS_SMALL_SPACING);
                ui.horizontal(|ui| {
                    ui.label("Port:");
                    let ports = gps_port_options(rustdar_nmea_serial::detect_gps_ports());
                    let selected = gps_port_label(&ports, self.serial_config.port_path.as_deref());
                    egui::ComboBox::from_id_salt("gps_port")
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            for (value, label) in &ports {
                                ui.selectable_value(
                                    &mut self.serial_config.port_path,
                                    value.clone(),
                                    label.as_str(),
                                );
                            }
                        });
                });
                true
            }
            #[cfg(feature = "gps-serial")]
            "gps.baud" => {
                ui.horizontal(|ui| {
                    ui.label("Baud:");
                    let baud_label = if self.serial_config.auto_baud() {
                        "Auto-detect".to_string()
                    } else {
                        self.serial_config.baud_rate.to_string()
                    };
                    egui::ComboBox::from_id_salt("gps_baud")
                        .selected_text(baud_label)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.serial_config.baud_rate,
                                0,
                                "Auto-detect",
                            );
                            for &rate in GPS_BAUD_RATES {
                                ui.selectable_value(
                                    &mut self.serial_config.baud_rate,
                                    rate,
                                    rate.to_string(),
                                );
                            }
                        });
                });
                true
            }
            #[cfg(feature = "gps-serial")]
            "gps.connect" => {
                ui.add_space(SETTINGS_SMALL_SPACING);

                if ui.button("Connect GPS").clicked() {
                    actions.push(GuiAction::StartGps {
                        config: self.serial_config.clone(),
                    });
                }
                if ui.button("Disconnect GPS").clicked() {
                    actions.push(GuiAction::StopGps);
                }

                ui.add_space(SETTINGS_SMALL_SPACING);

                if let Some(ref fix) = self.user_fix {
                    ui.label(format!("Fix: {}", fix.fix_quality.label()));
                    if let Some(sats) = fix.satellites {
                        ui.label(format!("Sats: {}", sats));
                    }
                } else {
                    ui.label("No GPS fix");
                }

                section_break(ui);
                true
            }
            #[cfg(not(feature = "gps-serial"))]
            "gps.port" | "gps.baud" | "gps.connect" => false,
            "heading" => {
                ui.horizontal(|ui| {
                    ui.label("Heading:");
                    egui::ComboBox::from_id_salt("heading_source")
                        .selected_text(self.heading_source.label())
                        .show_ui(ui, |ui| {
                            for &src in HeadingSource::ALL {
                                ui.selectable_value(&mut self.heading_source, src, src.label());
                            }
                        });
                });
                true
            }
            "storm.fallback" => {
                section_break(ui);
                ui.heading("Storm motion");
                ui.add_space(SETTINGS_SMALL_SPACING);
                let fallback = &mut self.srv_fallback;
                ui.horizontal(|ui| {
                    ui.label("When none is published:");
                    egui::ComboBox::from_id_salt("srv_fallback")
                        .selected_text(fallback.source().label())
                        .show_ui(ui, |ui| {
                            for choice in [
                                rustdar_radar::srv::SrvFallback::MeanWind,
                                rustdar_radar::srv::SrvFallback::BunkersRightMover,
                            ] {
                                ui.selectable_value(fallback, choice, choice.source().label());
                            }
                        });
                })
                .response
                .on_hover_text(
                    "Most volumes carry the National Weather Service's own storm motion \
                     and it is used whenever it does. This is what stands in when one \
                     does not: the 0-6 km mean wind, which measures closest to it, or \
                     the Bunkers right-mover, a supercell motion prediction that can \
                     point somewhere quite different in weak flow.",
                );
                true
            }
            "storm.override" => {
                ui.checkbox(
                    &mut self.storm_motion_override.enabled,
                    STORM_MOTION_OVERRIDE_LABEL,
                )
                .on_hover_text(
                    "On, storm-relative velocity uses the vector below and nothing else \
                     - in the plan view, the 3D volume and the cross-section alike, and \
                     ahead of the National Weather Service's own.",
                );
                true
            }
            "storm.speed" => {
                let motion = &mut self.storm_motion_override;
                let widget = ui
                    .add_enabled_ui(motion.enabled, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Speed:");
                            ui.add(
                                egui::DragValue::new(&mut motion.speed_kt)
                                    .speed(0.5)
                                    .range(0.0..=rustdar_radar::srm::MAX_OVERRIDE_SPEED_KT)
                                    .suffix(" kt"),
                            )
                        })
                        .inner
                    })
                    .inner;
                self.storm_motion_editing |= mid_edit(&widget);
                true
            }
            "storm.direction" => {
                let motion = &mut self.storm_motion_override;
                let widget = ui
                    .add_enabled_ui(motion.enabled, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("From:");
                            ui.add(
                                egui::DragValue::new(&mut motion.direction_deg)
                                    .speed(1.0)
                                    .range(0.0..=360.0)
                                    .suffix("\u{00b0}"),
                            )
                        })
                        .inner
                    })
                    .inner;
                self.storm_motion_editing |= mid_edit(&widget);
                true
            }
            "advanced.notifier" => {
                section_break(ui);
                ui.heading("Advanced");
                ui.add_space(SETTINGS_SMALL_SPACING);
                ui.label("Notifier endpoint:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.notifier_endpoint)
                        .font(egui::TextStyle::Monospace)
                        .hint_text(crate::DEFAULT_NOTIFIER_ENDPOINT),
                );
                ui.label(
                    egui::RichText::new(
                        "WebSocket chunk-notify URL. Empty uses the built-in default.",
                    )
                    .small()
                    .weak(),
                );
                true
            }
            "data.auto_poll" => {
                section_break(ui);
                ui.heading("Data & live");
                ui.add_space(SETTINGS_SMALL_SPACING);
                ui.checkbox(&mut self.auto_poll.enabled, "Auto-poll");
                true
            }
            "data.live_chunks" => {
                ui.checkbox(&mut self.live_chunks, "Live: real-time chunks");
                true
            }
            "data.push" => {
                ui.checkbox(&mut self.chunk_notifications, "Live: push notifications");
                true
            }
            "data.refresh" => {
                ui.add_space(SETTINGS_SMALL_SPACING);
                let refresh =
                    ui.add_enabled(!self.radar.fetching, egui::Button::new("Refresh radar"));
                if refresh.clicked() {
                    let mut config = self.radar.config.clone();
                    config.site = pane.site().to_string();
                    actions.push(GuiAction::FetchRadarScan(config));
                }
                true
            }
            "about.version" => {
                section_break(ui);
                ui.heading("About");
                ui.add_space(SETTINGS_SMALL_SPACING);
                ui.label(concat!("rustdar ", env!("CARGO_PKG_VERSION")));
                true
            }
            "about.platform" => {
                ui.label(
                    egui::RichText::new(
                        "Runs on Linux, macOS, Windows, the web, Android, iOS and BSD.",
                    )
                    .small()
                    .weak(),
                );
                true
            }
            "reset" => {
                ui.add_space(SETTINGS_SMALL_SPACING);
                if ui.button("Reset to defaults").clicked() {
                    self.preferences = UserPreferences::default();
                    self.serial_config = rustdar_nmea_serial::SerialConfig::default();
                    self.heading_source = rustdar_location::HeadingSource::default();
                    self.storm_motion_override = crate::StormMotionOverride::default();
                    self.srv_fallback = rustdar_radar::srv::SrvFallback::default();
                    actions.push(GuiAction::RequestLocation);
                }
                true
            }
            "about.exit" => {
                if !self.supports_exit {
                    return false;
                }
                ui.add_space(SETTINGS_SMALL_SPACING);
                if ui.button("Exit").clicked() {
                    actions.push(GuiAction::Exit);
                }
                true
            }
            other => unreachable!(
                "SETTINGS_ROWS lists {other:?} but render_settings_row has no arm for it"
            ),
        }
    }

    /// The body of the Location section: one line of state, at most one button,
    /// and — on the platforms where nothing else would say so — whether a fix
    /// has actually arrived.
    fn render_location_controls(&self, ui: &mut egui::Ui, actions: &mut Vec<GuiAction>) {
        use rustdar_location::LocationPermission;

        match self.location_permission {
            LocationPermission::Unavailable => {
                ui.label("Not available on this platform.");
            }
            LocationPermission::Unknown => {
                ui.label("Checking...");
            }
            LocationPermission::Denied => {
                ui.label("Denied.");
                ui.label(LOCATION_DENIED_NOTE);
                if self.location_settings_available && ui.button("Open location settings").clicked()
                {
                    actions.push(GuiAction::OpenLocationSettings);
                }
            }
            LocationPermission::Granted if self.location_active => {
                ui.label("On.");
                if ui.button("Turn off").clicked() {
                    actions.push(GuiAction::StopLocation);
                }
            }
            LocationPermission::Prompt | LocationPermission::Granted => {
                if ui.button("Use my location").clicked() {
                    actions.push(GuiAction::RequestLocation);
                }
            }
        }

        if matches!(
            self.location_permission,
            LocationPermission::Prompt | LocationPermission::Granted
        ) {
            ui.label(LOCATION_EGRESS_NOTE);
        }

        if let Some(line) = self.location_fix_summary() {
            ui.label(line);
        }
    }

    /// Whether a position has actually arrived, in one line, or `None` when
    /// there is nothing to say.
    fn location_fix_summary(&self) -> Option<String> {
        if !self.location_active && self.user_fix.is_none() {
            return None;
        }
        let Some(at) = self.user_fix_at else {
            return Some("Waiting for a fix...".to_owned());
        };
        let minutes = at.elapsed().as_secs() / 60;
        Some(match minutes {
            0 => "Last fix: just now.".to_owned(),
            1 => "Last fix: 1 minute ago.".to_owned(),
            n => format!("Last fix: {n} minutes ago."),
        })
    }
}

/// The GPS port dropdown's options, as `(value, label)` — "Auto-detect" plus
/// every port given.
#[cfg(feature = "gps-serial")]
fn gps_port_options(
    ports: impl IntoIterator<Item = rustdar_nmea_serial::GpsPortInfo>,
) -> Vec<(Option<String>, String)> {
    std::iter::once((None, "Auto-detect".to_owned()))
        .chain(ports.into_iter().map(|port| {
            (
                Some(port.port_name.clone()),
                format!("{} ({})", port.port_name, port.description),
            )
        }))
        .collect()
}

/// The label the port list puts against `selected`.
#[cfg(feature = "gps-serial")]
fn gps_port_label(ports: &[(Option<String>, String)], selected: Option<&str>) -> String {
    ports
        .iter()
        .find(|(value, _)| value.as_deref() == selected)
        .map(|(_, label)| label.clone())
        .unwrap_or_else(|| selected.unwrap_or("Auto-detect").to_owned())
}

/// Generic combo box for a unit preference enum.
fn unit_combo<T: Copy + PartialEq + UnitLabel>(
    ui: &mut egui::Ui,
    label: &str,
    current: &mut T,
    options: &[T],
) {
    ui.horizontal(|ui| {
        ui.label(format!("{label}:"));
        egui::ComboBox::from_id_salt(label)
            .selected_text(current.display_label())
            .show_ui(ui, |ui| {
                for &option in options {
                    ui.selectable_value(current, option, option.display_label());
                }
            });
    });
}

#[cfg(all(test, feature = "gps-serial"))]
mod tests {
    use super::*;

    /// Built by the shipped `gps_port_options`, so the labels under test are
    /// the ones the dropdown really offers.
    fn ports() -> Vec<(Option<String>, String)> {
        gps_port_options([rustdar_nmea_serial::GpsPortInfo {
            port_name: "/dev/ttyUSB0".to_owned(),
            description: "FT232R USB UART".to_owned(),
        }])
    }

    /// A port is offered under its description, not its bare device path —
    /// `/dev/ttyUSB0` alone does not tell you which of two dongles it is.
    #[test]
    fn the_port_list_describes_each_port() {
        let ports = ports();
        assert_eq!(ports[0], (None, "Auto-detect".to_owned()));
        assert_eq!(
            ports[1],
            (
                Some("/dev/ttyUSB0".to_owned()),
                "/dev/ttyUSB0 (FT232R USB UART)".to_owned()
            ),
        );
    }

    /// The collapsed box shows what the open list shows.
    #[test]
    fn the_gps_port_box_shows_the_label_its_list_shows() {
        let ports = ports();
        for (value, label) in &ports {
            assert_eq!(
                gps_port_label(&ports, value.as_deref()),
                *label,
                "the collapsed box disagrees with the list entry for {value:?}"
            );
        }
    }

    /// A configured port that is no longer plugged in is not in the list.
    #[test]
    fn an_unplugged_port_is_still_named() {
        assert_eq!(gps_port_label(&ports(), Some("/dev/ttyS9")), "/dev/ttyS9");
        assert_eq!(gps_port_label(&[], None), "Auto-detect");
    }
}

#[cfg(test)]
mod label_tests {
    use super::*;

    /// The storm motion switch must not name a source the app no longer has.
    #[test]
    fn the_storm_motion_switch_does_not_name_an_rpg_average() {
        let label = STORM_MOTION_OVERRIDE_LABEL.to_ascii_lowercase();
        assert!(
            !label.contains("average"),
            "the switch reads {STORM_MOTION_OVERRIDE_LABEL:?}, naming the RPG \
             SCIT average that left with the Level III SRM fetches",
        );
        assert!(
            label.contains("storm motion"),
            "the switch reads {STORM_MOTION_OVERRIDE_LABEL:?}, which does not \
             say what it overrides",
        );
    }
}
