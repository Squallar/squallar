mod alert;
mod discussion;
mod outlook;

use super::overlay_state::OverlayHandler;

/// Create the default set of overlay handlers.
pub(crate) fn create_handlers() -> Vec<Box<dyn OverlayHandler>> {
    vec![
        Box::new(outlook::SpcOutlookHandler::new()),
        Box::new(discussion::SpcDiscussionHandler::new()),
        Box::new(alert::NwsAlertHandler::new()),
    ]
}
