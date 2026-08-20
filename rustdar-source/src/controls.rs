//! Declarative UI control descriptors. Handlers return [`ControlItem`] trees;
//! the egui crate renders them without knowing which overlay produced them, and
//! interactions come back as [`ControlUpdate`].

#[derive(Debug, Clone)]
pub enum ControlItem {
    Toggle {
        id: &'static str,
        label: String,
        enabled: bool,
    },
    Section {
        label: String,
        collapsible: bool,
        expanded: bool,
        items: Vec<ControlItem>,
    },
    Dropdown {
        id: &'static str,
        label: String,
        options: Vec<(String, String)>,
        selected: String,
    },
    Slider {
        id: &'static str,
        label: String,
        min: f64,
        max: f64,
        value: f64,
        logarithmic: bool,
        format: String,
    },
    ButtonRow {
        buttons: Vec<ControlButton>,
    },
    InfoText {
        text: String,
    },
    Heading {
        text: String,
    },
    Separator,
}

#[derive(Debug, Clone)]
pub struct ControlButton {
    pub id: &'static str,
    pub label: String,
    pub enabled: bool,
    pub highlight: bool,
}

#[derive(Debug, Clone)]
pub struct ControlUpdate {
    pub id: &'static str,
    pub value: ControlValue,
}

#[derive(Debug, Clone)]
pub enum ControlValue {
    Bool(bool),
    String(String),
    Float(f64),
    Action,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ControlEffect {
    #[default]
    None,
    Fetch,
}
