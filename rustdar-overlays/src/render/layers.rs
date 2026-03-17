use crate::nws::alert::AlertCategory;
use crate::spc::outlook::{OutlookDay, OutlookProduct};

/// Identifies each toggleable overlay layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum LayerKind {
    Radar,
    SpcCategorical,
    SpcTornado,
    SpcWind,
    SpcHail,
    SpcProbabilistic,
    SpcMesoscaleDiscussions,
    NwsWarnings,
    NwsWatches,
    NwsAdvisories,
    StormReports,
    CityLabels,
    RadarSites,
}

impl LayerKind {
    /// All layer kinds in canonical order.
    pub const fn all() -> &'static [LayerKind] {
        &[
            LayerKind::Radar,
            LayerKind::SpcCategorical,
            LayerKind::SpcTornado,
            LayerKind::SpcWind,
            LayerKind::SpcHail,
            LayerKind::SpcProbabilistic,
            LayerKind::SpcMesoscaleDiscussions,
            LayerKind::NwsWarnings,
            LayerKind::NwsWatches,
            LayerKind::NwsAdvisories,
            LayerKind::StormReports,
            LayerKind::CityLabels,
            LayerKind::RadarSites,
        ]
    }

    /// Human-readable display name.
    pub fn display_name(self) -> &'static str {
        match self {
            LayerKind::Radar => "Radar",
            LayerKind::SpcCategorical => "Categorical",
            LayerKind::SpcTornado => "Tornado",
            LayerKind::SpcWind => "Wind",
            LayerKind::SpcHail => "Hail",
            LayerKind::SpcProbabilistic => "Probabilistic",
            LayerKind::SpcMesoscaleDiscussions => "Mesoscale Disc.",
            LayerKind::NwsWarnings => "Warnings",
            LayerKind::NwsWatches => "Watches",
            LayerKind::NwsAdvisories => "Advisories",
            LayerKind::StormReports => "Storm Reports",
            LayerKind::CityLabels => "City Labels",
            LayerKind::RadarSites => "Radar Sites",
        }
    }

    /// Whether this layer is an SPC outlook product.
    pub fn is_spc(self) -> bool {
        matches!(
            self,
            LayerKind::SpcCategorical
                | LayerKind::SpcTornado
                | LayerKind::SpcWind
                | LayerKind::SpcHail
                | LayerKind::SpcProbabilistic
        )
    }

    /// Whether this layer is an NWS alerts layer.
    pub fn is_nws(self) -> bool {
        matches!(
            self,
            LayerKind::NwsWarnings | LayerKind::NwsWatches | LayerKind::NwsAdvisories
        )
    }

    /// Convert to the corresponding `AlertCategory`, if this is an NWS layer.
    pub fn to_alert_category(self) -> Option<AlertCategory> {
        match self {
            LayerKind::NwsWarnings => Some(AlertCategory::Warning),
            LayerKind::NwsWatches => Some(AlertCategory::Watch),
            LayerKind::NwsAdvisories => Some(AlertCategory::Advisory),
            _ => None,
        }
    }

    /// Convert to the corresponding `OutlookProduct`, if this is an SPC layer.
    pub fn to_outlook_product(self) -> Option<OutlookProduct> {
        match self {
            LayerKind::SpcCategorical => Some(OutlookProduct::Categorical),
            LayerKind::SpcTornado => Some(OutlookProduct::Tornado),
            LayerKind::SpcWind => Some(OutlookProduct::Wind),
            LayerKind::SpcHail => Some(OutlookProduct::Hail),
            LayerKind::SpcProbabilistic => Some(OutlookProduct::Probabilistic),
            _ => None,
        }
    }
}

/// Per-layer state (enabled toggle).
#[derive(Debug, Clone)]
pub struct LayerState {
    pub enabled: bool,
}

impl LayerState {
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }
}

/// Manages the set of overlay layers and their toggle states.
pub struct LayerManager {
    layers: std::collections::BTreeMap<LayerKind, LayerState>,
    /// Which SPC outlook day is selected.
    pub spc_day: OutlookDay,
}

impl Default for LayerManager {
    fn default() -> Self {
        Self::new()
    }
}

impl LayerManager {
    pub fn new() -> Self {
        use std::collections::BTreeMap;
        let mut layers = BTreeMap::new();

        layers.insert(LayerKind::Radar, LayerState::new(true));
        layers.insert(LayerKind::SpcCategorical, LayerState::new(false));
        layers.insert(LayerKind::SpcTornado, LayerState::new(false));
        layers.insert(LayerKind::SpcWind, LayerState::new(false));
        layers.insert(LayerKind::SpcHail, LayerState::new(false));
        layers.insert(LayerKind::SpcProbabilistic, LayerState::new(false));
        layers.insert(LayerKind::SpcMesoscaleDiscussions, LayerState::new(true));
        layers.insert(LayerKind::NwsWatches, LayerState::new(true));
        layers.insert(LayerKind::NwsAdvisories, LayerState::new(true));
        layers.insert(LayerKind::NwsWarnings, LayerState::new(true));
        layers.insert(LayerKind::StormReports, LayerState::new(false));
        layers.insert(LayerKind::CityLabels, LayerState::new(true));
        layers.insert(LayerKind::RadarSites, LayerState::new(false));

        Self {
            layers,
            spc_day: OutlookDay::Day1,
        }
    }

    pub fn is_enabled(&self, kind: LayerKind) -> bool {
        self.layers
            .get(&kind)
            .map(|s| s.enabled)
            .unwrap_or(false)
    }

    pub fn set_enabled(&mut self, kind: LayerKind, enabled: bool) {
        if let Some(state) = self.layers.get_mut(&kind) {
            state.enabled = enabled;
        }
    }

    pub fn toggle(&mut self, kind: LayerKind) {
        if let Some(state) = self.layers.get_mut(&kind) {
            state.enabled = !state.enabled;
        }
    }

    /// Get a mutable reference to the enabled flag for use with egui checkbox.
    pub fn enabled_mut(&mut self, kind: LayerKind) -> &mut bool {
        &mut self.layers.entry(kind).or_insert(LayerState::new(false)).enabled
    }

    /// Returns true if any SPC outlook layer is enabled.
    pub fn any_spc_enabled(&self) -> bool {
        self.layers
            .iter()
            .any(|(kind, state)| kind.is_spc() && state.enabled)
    }

    /// Get all currently enabled SPC product types.
    pub fn enabled_spc_products(&self) -> Vec<OutlookProduct> {
        self.layers
            .iter()
            .filter(|(kind, state)| kind.is_spc() && state.enabled)
            .filter_map(|(kind, _)| kind.to_outlook_product())
            .collect()
    }

    /// Returns true if any NWS alerts layer is enabled.
    pub fn any_nws_enabled(&self) -> bool {
        self.layers
            .iter()
            .any(|(kind, state)| kind.is_nws() && state.enabled)
    }

    /// Get all currently enabled NWS alert categories.
    pub fn enabled_nws_categories(&self) -> Vec<AlertCategory> {
        self.layers
            .iter()
            .filter(|(kind, state)| kind.is_nws() && state.enabled)
            .filter_map(|(kind, _)| kind.to_alert_category())
            .collect()
    }

    /// Return the SPC-relevant `LayerKind` variants for the current day.
    /// Day 1-2 have separate tornado/wind/hail; Day 3 uses combined probabilistic;
    /// Days 4-8 only have a single probabilistic product.
    pub fn spc_layers_for_day(&self) -> Vec<LayerKind> {
        if self.spc_day.is_extended() {
            return vec![LayerKind::SpcProbabilistic];
        }
        match self.spc_day {
            OutlookDay::Day1 | OutlookDay::Day2 => vec![
                LayerKind::SpcCategorical,
                LayerKind::SpcTornado,
                LayerKind::SpcWind,
                LayerKind::SpcHail,
            ],
            OutlookDay::Day3 => vec![
                LayerKind::SpcCategorical,
                LayerKind::SpcProbabilistic,
            ],
            _ => unreachable!(),
        }
    }
}
