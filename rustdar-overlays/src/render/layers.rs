use crate::nws::alert::AlertCategory;
use crate::spc::outlook::{OutlookDay, OutlookProduct};

/// Finer-grained than `OverlayKind`: one variant per user-facing toggle.
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
    Lightning,
    Metar,
    CityLabels,
    RadarSites,
}

impl LayerKind {
    /// Order here is the order the layer list is presented in.
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
            LayerKind::Lightning,
            LayerKind::Metar,
            LayerKind::CityLabels,
            LayerKind::RadarSites,
        ]
    }

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
            LayerKind::StormReports => "SPC Storm Reports",
            LayerKind::Lightning => "GLM Lightning",
            LayerKind::Metar => "METAR",
            LayerKind::CityLabels => "City Labels",
            LayerKind::RadarSites => "Radar Sites",
        }
    }

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

    pub fn is_nws(self) -> bool {
        matches!(
            self,
            LayerKind::NwsWarnings | LayerKind::NwsWatches | LayerKind::NwsAdvisories
        )
    }

    /// `None` for non-NWS layers.
    pub fn to_alert_category(self) -> Option<AlertCategory> {
        match self {
            LayerKind::NwsWarnings => Some(AlertCategory::Warning),
            LayerKind::NwsWatches => Some(AlertCategory::Watch),
            LayerKind::NwsAdvisories => Some(AlertCategory::Advisory),
            _ => None,
        }
    }

    /// `None` for non-SPC layers.
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

#[derive(Debug, Clone)]
pub struct LayerState {
    pub enabled: bool,
}

impl LayerState {
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }
}

pub struct LayerManager {
    /// `BTreeMap`, so iteration follows `LayerKind`'s `Ord` and is stable.
    layers: std::collections::BTreeMap<LayerKind, LayerState>,
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
        layers.insert(LayerKind::Lightning, LayerState::new(false));
        layers.insert(LayerKind::Metar, LayerState::new(false));
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

    /// Inserts a disabled entry if the layer is unknown, so egui always has a
    /// `&mut bool` to bind a checkbox to.
    pub fn enabled_mut(&mut self, kind: LayerKind) -> &mut bool {
        &mut self.layers.entry(kind).or_insert(LayerState::new(false)).enabled
    }

    pub fn any_spc_enabled(&self) -> bool {
        self.layers
            .iter()
            .any(|(kind, state)| kind.is_spc() && state.enabled)
    }

    pub fn enabled_spc_products(&self) -> Vec<OutlookProduct> {
        self.layers
            .iter()
            .filter(|(kind, state)| kind.is_spc() && state.enabled)
            .filter_map(|(kind, _)| kind.to_outlook_product())
            .collect()
    }

    pub fn any_nws_enabled(&self) -> bool {
        self.layers
            .iter()
            .any(|(kind, state)| kind.is_nws() && state.enabled)
    }

    pub fn enabled_nws_categories(&self) -> Vec<AlertCategory> {
        self.layers
            .iter()
            .filter(|(kind, state)| kind.is_nws() && state.enabled)
            .filter_map(|(kind, _)| kind.to_alert_category())
            .collect()
    }

    /// Days 1-2 publish separate tornado/wind/hail; day 3 only a combined
    /// probabilistic; days 4-8 one probabilistic product.
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
