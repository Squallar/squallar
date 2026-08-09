use crate::actions::GuiAction;
use rustdar_gps::HeadingSource;
use rustdar_units::{
    DistanceUnit, HailSizeUnit, HeightUnit, PrecipRateUnit, SpeedUnit, TemperatureUnit,
    TimezonePreference, UnitLabel, UserPreferences,
};

/// Width of the settings window when there is room for a fixed-width one.
/// Narrower screens get a full-bleed window instead — see
/// [`crate::ui_layout::LayoutCtx::dialog_width`].
const SETTINGS_POPUP_ROOMY_WIDTH: f32 = 340.0;
const SETTINGS_SMALL_SPACING: f32 = 4.0;
const SETTINGS_LARGE_SPACING: f32 = 8.0;
#[cfg(feature = "gps-serial")]
const GPS_BAUD_RATES: &[u32] = &[4800, 9600, 38400, 115200];

impl super::Gui {
    /// Render the settings window if `show_settings` is true.
    pub(super) fn render_settings(&mut self, ctx: &egui::Context, actions: &mut Vec<GuiAction>) {
        if !self.show_settings {
            return;
        }

        let popup_width = self.layout.dialog_width(SETTINGS_POPUP_ROOMY_WIDTH);

        let mut open = true;
        egui::Window::new("Settings")
            .id(egui::Id::new("settings_window"))
            .open(&mut open)
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(true)
            // Outer width since egui 0.35 (#7725) — content is 14px narrower at
            // the stock theme. See the note in `ui_popups.rs`; same reasoning,
            // deliberately not compensated.
            .default_width(popup_width)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(self.layout.dialog_center())
            .show(ctx, |ui| {
                ui.heading("Units");
                ui.add_space(SETTINGS_SMALL_SPACING);

                unit_combo(
                    ui,
                    "Timezone",
                    &mut self.preferences.timezone,
                    TimezonePreference::ALL,
                );
                unit_combo(
                    ui,
                    "Temperature",
                    &mut self.preferences.temperature,
                    TemperatureUnit::ALL,
                );
                unit_combo(ui, "Speed", &mut self.preferences.speed, SpeedUnit::ALL);
                unit_combo(
                    ui,
                    "Distance",
                    &mut self.preferences.distance,
                    DistanceUnit::ALL,
                );
                unit_combo(ui, "Height", &mut self.preferences.height, HeightUnit::ALL);
                unit_combo(
                    ui,
                    "Precip rate",
                    &mut self.preferences.precip_rate,
                    PrecipRateUnit::ALL,
                );
                unit_combo(
                    ui,
                    "Hail size",
                    &mut self.preferences.hail_size,
                    HailSizeUnit::ALL,
                );

                ui.add_space(SETTINGS_LARGE_SPACING);
                ui.separator();
                ui.add_space(SETTINGS_SMALL_SPACING);

                // --- Location (all platforms) ---
                //
                // Ungated, and above the GPS block rather than inside it,
                // because it is a different question with a different answer on
                // every platform:
                //
                //   Location — may this app know where you are, from the OS.
                //              A privilege, granted and withdrawn in system
                //              settings, and the only one rustdar asks for.
                //   GPS      — open this serial port and read NMEA from it.
                //              A device the user plugged in. No permission
                //              anywhere, and absent from four of five targets.
                //
                // Written to read as two questions, not two spellings of one:
                // "Use my location" against "Connect GPS" below.
                ui.heading("Location");
                ui.add_space(SETTINGS_SMALL_SPACING);
                self.render_location_controls(ui, actions);

                ui.add_space(SETTINGS_LARGE_SPACING);
                ui.separator();
                ui.add_space(SETTINGS_SMALL_SPACING);

                // --- GPS section (serial-capable targets only) ---
                //
                // Gated on the feature rather than on `not(android)` so that the
                // gate matches the one on `detect_gps_ports` below. An OS cfg
                // can never satisfy a feature cfg, and mismatching the two is
                // what stopped this crate building standalone.
                #[cfg(feature = "gps-serial")]
                {
                    ui.heading("GPS");
                    ui.add_space(SETTINGS_SMALL_SPACING);

                    // Port selection
                    ui.horizontal(|ui| {
                        ui.label("Port:");
                        // One list, read by both halves. The collapsed box used
                        // to show the bare device path while the list it opened
                        // showed "path (description)" — the same divergence the
                        // handler dropdowns had. Enumerated once per frame
                        // because `detect_gps_ports` touches the serial
                        // subsystem, so formatting the two halves separately
                        // would mean probing it twice.
                        let ports = gps_port_options(rustdar_gps::detect_gps_ports());
                        let selected = gps_port_label(&ports, self.gps_config.port_path.as_deref());
                        egui::ComboBox::from_id_salt("gps_port")
                            .selected_text(selected)
                            .show_ui(ui, |ui| {
                                for (value, label) in &ports {
                                    ui.selectable_value(
                                        &mut self.gps_config.port_path,
                                        value.clone(),
                                        label.as_str(),
                                    );
                                }
                            });
                    });

                    // Baud rate
                    ui.horizontal(|ui| {
                        ui.label("Baud:");
                        let baud_label = if self.gps_config.auto_baud() {
                            "Auto-detect".to_string()
                        } else {
                            self.gps_config.baud_rate.to_string()
                        };
                        egui::ComboBox::from_id_salt("gps_baud")
                            .selected_text(baud_label)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.gps_config.baud_rate,
                                    0,
                                    "Auto-detect",
                                );
                                for &rate in GPS_BAUD_RATES {
                                    ui.selectable_value(
                                        &mut self.gps_config.baud_rate,
                                        rate,
                                        rate.to_string(),
                                    );
                                }
                            });
                    });

                    ui.add_space(SETTINGS_SMALL_SPACING);

                    // Start/stop button
                    // Note: gps_active state is only meaningful on desktop
                    if ui.button("Connect GPS").clicked() {
                        actions.push(GuiAction::StartGps {
                            config: self.gps_config.clone(),
                        });
                    }
                    if ui.button("Disconnect GPS").clicked() {
                        actions.push(GuiAction::StopGps);
                    }

                    ui.add_space(SETTINGS_SMALL_SPACING);

                    // Fix status
                    if let Some(ref fix) = self.user_fix {
                        ui.label(format!("Fix: {}", fix.fix_quality.label()));
                        if let Some(sats) = fix.satellites {
                            ui.label(format!("Sats: {}", sats));
                        }
                    } else {
                        ui.label("No GPS fix");
                    }

                    ui.add_space(SETTINGS_LARGE_SPACING);
                    ui.separator();
                    ui.add_space(SETTINGS_SMALL_SPACING);
                }

                // --- Heading source (all platforms) ---
                ui.horizontal(|ui| {
                    ui.label("Heading:");
                    egui::ComboBox::from_id_salt("heading_source")
                        .selected_text(self.gps_config.heading_source.label())
                        .show_ui(ui, |ui| {
                            for &src in HeadingSource::ALL {
                                ui.selectable_value(
                                    &mut self.gps_config.heading_source,
                                    src,
                                    src.label(),
                                );
                            }
                        });
                });

                ui.add_space(SETTINGS_LARGE_SPACING);
                ui.separator();
                ui.add_space(SETTINGS_SMALL_SPACING);

                // --- Storm motion (storm-relative velocity) ---
                //
                // Off by default: the RPG's own SCIT average is in the N0S
                // Product Description Block and is the vector the RPG itself
                // fitted for this volume. An override replaces it on all four
                // tilts at once — every one of them is derived.
                ui.heading("Storm motion");
                ui.add_space(SETTINGS_SMALL_SPACING);
                let motion = &mut self.storm_motion_override;
                ui.checkbox(&mut motion.enabled, "Override average storm motion");
                ui.add_enabled_ui(motion.enabled, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Speed:");
                        // Upper bound shared with `DERIVED_OFFSET`, which is
                        // sized so nothing this widget admits can saturate the
                        // derived gate encoding.
                        ui.add(
                            egui::DragValue::new(&mut motion.speed_kt)
                                .speed(0.5)
                                .range(0.0..=rustdar_radar::srm::MAX_OVERRIDE_SPEED_KT)
                                .suffix(" kt"),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("From:");
                        ui.add(
                            egui::DragValue::new(&mut motion.direction_deg)
                                .speed(1.0)
                                .range(0.0..=360.0)
                                .suffix("\u{00b0}"),
                        );
                    });
                });

                ui.add_space(SETTINGS_LARGE_SPACING);
                ui.separator();
                ui.add_space(SETTINGS_SMALL_SPACING);

                if ui.button("Reset to defaults").clicked() {
                    self.preferences = UserPreferences::default();
                    self.gps_config = rustdar_gps::GpsConfig::default();
                    self.storm_motion_override = crate::StormMotionOverride::default();
                    // The location memo lives outside `Gui` — it is persisted
                    // under its own key by the frontend's gate, precisely so a
                    // 3 s autosave timer cannot lose it — so resetting it is an
                    // action rather than an assignment. Included because this
                    // button is the obvious thing a user reaches for when they
                    // want a dismissed permission prompt back, and a "reset"
                    // that quietly kept one piece of state would be a lie.
                    actions.push(GuiAction::RequestLocation);
                }
            });

        if !open {
            self.show_settings = false;
        }
    }

    /// The body of the Location section: one line of state, at most one button,
    /// and — on the platforms where nothing else would say so — whether a fix
    /// has actually arrived.
    fn render_location_controls(&self, ui: &mut egui::Ui, actions: &mut Vec<GuiAction>) {
        use rustdar_gps::LocationPermission;

        match self.location_permission {
            // No service to grant. No control, because there is no sequence of
            // clicks that changes this, and "open system settings" would send
            // the user hunting for a switch that does not exist.
            LocationPermission::Unavailable => {
                ui.label("Not available on this platform.");
            }
            // The startup window every platform has. Deliberately not a button:
            // offering one here is how the app ends up asking before the OS has
            // said whether anyone has been asked.
            LocationPermission::Unknown => {
                ui.label("Checking\u{2026}");
            }
            // A decision, and one only the user can reverse. No button — the
            // platform will not show a second dialog — so the only useful thing
            // here is where to go instead.
            LocationPermission::Denied => {
                ui.label("Denied.");
                ui.label(
                    "Location for this app is turned off. It can be turned back \
                     on in your system settings.",
                );
            }
            LocationPermission::Granted if self.location_active => {
                ui.label("On.");
                // "Turn off", not "revoke": this stops the stream and nothing
                // more. No platform lets an app hand a permission back.
                if ui.button("Turn off").clicked() {
                    actions.push(GuiAction::StopLocation);
                }
            }
            // Granted-but-idle and never-asked land on the same button on
            // purpose. From the user's side they are one thing — "start using
            // my location" — and the difference between them is only whether a
            // dialog appears, which the OS decides and this pane cannot promise
            // either way.
            LocationPermission::Prompt | LocationPermission::Granted => {
                if ui.button("Use my location").clicked() {
                    actions.push(GuiAction::RequestLocation);
                }
            }
        }

        if let Some(line) = self.location_fix_summary() {
            ui.label(line);
        }
    }

    /// Whether a position has actually arrived, in one line, or `None` when
    /// there is nothing to say.
    ///
    /// # Why this is not the `Fix:` readout in the GPS block
    ///
    /// That one is inside `#[cfg(feature = "gps-serial")]`, which web, Android,
    /// iOS and every build without a serial port do not compile. On exactly
    /// those platforms — the ones where the OS location service is the *only*
    /// source — the section above would otherwise say "On." beside an empty map
    /// and explain nothing. That is also the likely Linux outcome: GeoClue can
    /// take a while, or answer with nothing at all.
    ///
    /// Coarse on purpose. Seconds would tick in a window nobody is watching for
    /// a value that changes every few minutes.
    fn location_fix_summary(&self) -> Option<String> {
        if !self.location_active && self.user_fix.is_none() {
            return None;
        }
        let Some(at) = self.user_fix_at else {
            return Some("Waiting for a fix\u{2026}".to_owned());
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
///
/// Takes the ports rather than calling `detect_gps_ports` itself so the
/// labelling can be tested; enumeration needs real hardware.
#[cfg(feature = "gps-serial")]
fn gps_port_options(
    ports: impl IntoIterator<Item = rustdar_gps::GpsPortInfo>,
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
///
/// Falls back to the bare device path for a configured port that is no longer
/// plugged in: it is not in the list, but naming it is better than silently
/// reading "Auto-detect" while a specific port is still configured.
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
        gps_port_options([rustdar_gps::GpsPortInfo {
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
    ///
    /// It used to show the raw `port_path`, so a chosen port read
    /// `/dev/ttyUSB0` until you opened the list and found it described there
    /// as `/dev/ttyUSB0 (FT232R USB UART)`. The same defect the handler
    /// dropdowns had, hidden behind a non-default feature that wasm and
    /// Android never build.
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
    /// Naming it beats reading "Auto-detect" while a specific port is set.
    #[test]
    fn an_unplugged_port_is_still_named() {
        assert_eq!(gps_port_label(&ports(), Some("/dev/ttyS9")), "/dev/ttyS9");
        assert_eq!(gps_port_label(&[], None), "Auto-detect");
    }
}
