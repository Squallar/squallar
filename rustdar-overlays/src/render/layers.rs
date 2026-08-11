use crate::nws::alert::AlertCategory;
use crate::spc::outlook::OutlookProduct;

/// Finer-grained than `OverlayKind`: one variant per user-facing toggle.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
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
