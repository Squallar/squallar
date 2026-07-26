use rustdar_units::{
    DistanceUnit, HailSizeUnit, HeightUnit, PrecipRateUnit, SpeedUnit, TemperatureUnit,
    TimezonePreference, UserPreferences, UnitLabel,
};
use rustdar_gps::HeadingSource;
use crate::actions::GuiAction;

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
    ///
    /// `actions` is only ever pushed to from the `gps-serial` section below, so
    /// without that feature it is an untouched `&mut Vec` and both
    /// `unused_variables` and `clippy::ptr_arg` fire. Taking `&mut [GuiAction]`
    /// as `ptr_arg` suggests is not an option — the feature-enabled build needs
    /// `push` — so the lints are allowed, but only in the configuration that
    /// provokes them. `cfg_attr` rather than a bare `allow` so that if the
    /// serial build later stops using `actions` the warning comes back.
    ///
    /// This is also why the lint went unnoticed: `rustdar-platform` enables
    /// `gps-serial` only for `cfg(not(target_os = "android"))`, so host CI never
    /// compiles the configuration that warns. It shows up only under
    /// `cargo ndk … clippy`.
    #[cfg_attr(not(feature = "gps-serial"), allow(unused_variables, clippy::ptr_arg))]
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

                // --- Storm motion (storm-relative velocity) ---
                //
                // Off by default: the RPG's own SCIT average is in the N0S
                // Product Description Block and is what the RPG used for the
                // 0.5° tilt, so overriding it makes the tilts disagree.
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
