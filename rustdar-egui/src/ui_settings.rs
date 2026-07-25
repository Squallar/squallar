use rustdar_units::{
    DistanceUnit, HailSizeUnit, HeightUnit, PrecipRateUnit, SpeedUnit, TemperatureUnit,
    TimezonePreference, UserPreferences, UnitLabel,
};
use rustdar_gps::HeadingSource;
use crate::actions::GuiAction;

const IS_MOBILE: bool = cfg!(target_os = "android");
const SETTINGS_POPUP_MARGIN: f32 = 32.0;
const SETTINGS_POPUP_MIN_WIDTH_MOBILE: f32 = 250.0;
const SETTINGS_POPUP_WIDTH_DESKTOP: f32 = 340.0;
const SETTINGS_SMALL_SPACING: f32 = 4.0;
const SETTINGS_LARGE_SPACING: f32 = 8.0;
#[cfg(not(target_os = "android"))]
const GPS_BAUD_RATES: &[u32] = &[4800, 9600, 38400, 115200];

impl super::Gui {
    /// Render the settings window if `show_settings` is true.
    #[allow(unused_variables)]
    pub(super) fn render_settings(&mut self, ctx: &egui::Context, actions: &mut Vec<GuiAction>) {
        if !self.show_settings {
            return;
        }

        let screen = ctx.input(|i| i.viewport_rect());
        let popup_width = if IS_MOBILE {
            (screen.width() - SETTINGS_POPUP_MARGIN).max(SETTINGS_POPUP_MIN_WIDTH_MOBILE)
        } else {
            SETTINGS_POPUP_WIDTH_DESKTOP
        };

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
            .default_pos(screen.center())
            .show(ctx, |ui| {
                ui.heading("Units");
                ui.add_space(SETTINGS_SMALL_SPACING);

                unit_combo(ui, "Timezone", &mut self.preferences.timezone, TimezonePreference::ALL);
                unit_combo(ui, "Temperature", &mut self.preferences.temperature, TemperatureUnit::ALL);
                unit_combo(ui, "Speed", &mut self.preferences.speed, SpeedUnit::ALL);
                unit_combo(ui, "Distance", &mut self.preferences.distance, DistanceUnit::ALL);
                unit_combo(ui, "Height", &mut self.preferences.height, HeightUnit::ALL);
                unit_combo(ui, "Precip rate", &mut self.preferences.precip_rate, PrecipRateUnit::ALL);
                unit_combo(ui, "Hail size", &mut self.preferences.hail_size, HailSizeUnit::ALL);

                ui.add_space(SETTINGS_LARGE_SPACING);
                ui.separator();
                ui.add_space(SETTINGS_SMALL_SPACING);

                // --- GPS section (desktop only) ---
                #[cfg(not(target_os = "android"))]
                {
                    ui.heading("GPS");
                    ui.add_space(SETTINGS_SMALL_SPACING);

                    // Port selection
                    ui.horizontal(|ui| {
                        ui.label("Port:");
                        let current_label = self.gps_config.port_path.as_deref().unwrap_or("Auto-detect");
                        egui::ComboBox::from_id_salt("gps_port")
                            .selected_text(current_label)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.gps_config.port_path, None, "Auto-detect").changed();
                                for port_info in rustdar_gps::detect_gps_ports() {
                                    let label = format!("{} ({})", port_info.port_name, port_info.description);
                                    let val = Some(port_info.port_name.clone());
                                    ui.selectable_value(&mut self.gps_config.port_path, val, label);
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
                                ui.selectable_value(&mut self.gps_config.baud_rate, 0, "Auto-detect");
                                for &rate in GPS_BAUD_RATES {
                                    ui.selectable_value(&mut self.gps_config.baud_rate, rate, rate.to_string());
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

                if ui.button("Reset to defaults").clicked() {
                    self.preferences = UserPreferences::default();
                    self.gps_config = rustdar_gps::GpsConfig::default();
                }
            });

        if !open {
            self.show_settings = false;
        }
    }
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
