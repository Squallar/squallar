use rustdar_units::{
    DistanceUnit, HailSizeUnit, HeightUnit, PrecipRateUnit, SpeedUnit, TemperatureUnit,
    TimezonePreference, UserPreferences,
};

const IS_MOBILE: bool = cfg!(target_os = "android");

impl super::Gui {
    /// Render the settings window if `show_settings` is true.
    pub(super) fn render_settings(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }

        let screen = ctx.input(|i| i.viewport_rect());
        let popup_width = if IS_MOBILE {
            (screen.width() - 32.0).max(250.0)
        } else {
            340.0
        };

        let mut open = true;
        egui::Window::new("Settings")
            .id(egui::Id::new("settings_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(!IS_MOBILE)
            .default_width(popup_width)
            .max_width(popup_width)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.heading("Units");
                ui.add_space(4.0);

                unit_combo(ui, "Timezone", &mut self.preferences.timezone, TimezonePreference::ALL);
                unit_combo(ui, "Temperature", &mut self.preferences.temperature, TemperatureUnit::ALL);
                unit_combo(ui, "Speed", &mut self.preferences.speed, SpeedUnit::ALL);
                unit_combo(ui, "Distance", &mut self.preferences.distance, DistanceUnit::ALL);
                unit_combo(ui, "Height", &mut self.preferences.height, HeightUnit::ALL);
                unit_combo(ui, "Precip rate", &mut self.preferences.precip_rate, PrecipRateUnit::ALL);
                unit_combo(ui, "Hail size", &mut self.preferences.hail_size, HailSizeUnit::ALL);

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                if ui.button("Reset to defaults").clicked() {
                    self.preferences = UserPreferences::default();
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

/// Trait for label display on unit enums.
trait UnitLabel {
    fn display_label(self) -> &'static str;
}

impl UnitLabel for SpeedUnit {
    fn display_label(self) -> &'static str { SpeedUnit::label(self) }
}
impl UnitLabel for DistanceUnit {
    fn display_label(self) -> &'static str { DistanceUnit::label(self) }
}
impl UnitLabel for HeightUnit {
    fn display_label(self) -> &'static str { HeightUnit::label(self) }
}
impl UnitLabel for PrecipRateUnit {
    fn display_label(self) -> &'static str { PrecipRateUnit::label(self) }
}
impl UnitLabel for HailSizeUnit {
    fn display_label(self) -> &'static str { HailSizeUnit::label(self) }
}
impl UnitLabel for TimezonePreference {
    fn display_label(self) -> &'static str { TimezonePreference::label(self) }
}
