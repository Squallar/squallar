mod alert;
mod colorscale;
mod discussion;
mod labels;
mod location;
mod metar;
mod outlook;
mod radar;
mod reports;
mod sites;

use super::overlay_state::OverlayHandler;

/// Create the default set of overlay handlers.
pub(crate) fn create_handlers() -> Vec<Box<dyn OverlayHandler>> {
    vec![
        Box::new(radar::RadarHandler::new()),
        Box::new(outlook::SpcOutlookHandler::new()),
        Box::new(discussion::SpcDiscussionHandler::new()),
        Box::new(alert::NwsAlertHandler::new()),
        Box::new(reports::StormReportsHandler::new()),
        Box::new(metar::MetarHandler::new()),
        Box::new(labels::CityLabelsHandler::new()),
        Box::new(sites::RadarSitesHandler::new()),
        Box::new(location::UserLocationHandler::new()),
        Box::new(colorscale::ColorScaleHandler::new()),
    ]
}
