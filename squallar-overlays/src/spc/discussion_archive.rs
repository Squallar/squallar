//! **SPC mesoscale discussions that were valid at a past instant.**
//!
//! `spcmdrss.xml` is a standing feed of what is active *now*. It keeps no
//! history, so a pane scrubbed to a storm years gone drew today's discussions
//! over it — which is why the marketing harness force-disabled the layer on any
//! parked pane rather than publish a picture of the wrong weather. The Iowa
//! State Mesonet archives the MCDs and addresses them by validity, which is the
//! shape [`squallar_source::handler::FetchConfig::as_of`] was written for.
//!
//! # Two requests per discussion, on purpose
//!
//! IEM's GeoJSON answer carries the polygon and a `product_id` but no product
//! text, and the text is where the type, the "concerning" line and the colours
//! come from. Deriving those from the GeoJSON instead would be a second
//! derivation of the same facts, and the first thing to drift between the two
//! would be the colour of a tornado-possible discussion. So this fetches the
//! body by product id and hands it to
//! [`super::discussion::discussion_from_text`] — the same function the live
//! feed's items go through.
//!
//! # What it does not carry
//!
//! Nothing is dropped relative to the live layer: an MCD is one text product
//! with one polygon, and both arrive. A body that fails to fetch drops that one
//! discussion and logs it rather than failing the round, because a partial
//! picture of the discussions in force beats none of them — but the count the
//! panel reports is then honestly smaller, not silently rounded up.

use super::discussion::{SpcDiscussion, discussion_from_text};
use squallar_source::fetch_policy::FetchError;
use squallar_source::origins::DataSources;

/// IEM is a single small JSON index plus one short text body per discussion.
const ARCHIVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Where SPC keeps the rendered page for an archived discussion.
///
/// The live feed's `<link>` points at the same place, so a popup opened on an
/// archived MD offers the same destination as a popup opened on a live one.
fn spc_archive_link(sources: &DataSources, year: i64, number: u32) -> String {
    format!("{}/products/md/{year}/md{number:04}.html", sources.spc_base)
}

/// The `(product_id, year, number)` of every discussion in one IEM index.
///
/// Separated from the fetching so the shape of IEM's answer is pinned by a test
/// against a captured response rather than against a live service.
pub fn index_entries(iem: &serde_json::Value) -> Vec<(String, i64, u32)> {
    iem.get("features")
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|feature| {
            let props = feature.get("properties")?;
            let product_id = props.get("product_id")?.as_str()?.to_string();
            let year = props.get("year").and_then(serde_json::Value::as_i64)?;
            let number = props.get("num").and_then(serde_json::Value::as_u64)? as u32;
            Some((product_id, year, number))
        })
        .collect()
}

/// Fetch the mesoscale discussions that were valid at `at`, UTC.
pub async fn fetch_archived_discussions(
    sources: &DataSources,
    at: chrono::NaiveDateTime,
) -> Result<Vec<SpcDiscussion>, FetchError> {
    // NOT the application-wide client: IEM answers `OPTIONS` with `405`, so a
    // `User-Agent` makes this a preflighted request that never happens — in the
    // browser, and only in the browser. See `DataSources::iem_client`.
    let client = sources
        .iem_client(ARCHIVE_TIMEOUT)
        .build()
        .map_err(|e| FetchError::permanent(format!("could not build the IEM client: {e}")))?;

    let url = sources.spc_discussions_archive_url(at);
    log::info!("Fetching archived SPC mesoscale discussions valid at {at} from {url}");

    let response = client.get(&url).send().await.map_err(|e| {
        FetchError::from_transport(&e, format!("archived MD index request failed: {e}"))
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::permanent(format!(
            "archived MD index returned {status} for {url}"
        )));
    }
    let index: serde_json::Value = response
        .json()
        .await
        .map_err(|e| FetchError::permanent(format!("the archived MD index was not JSON: {e}")))?;

    let entries = index_entries(&index);
    let mut discussions = Vec::with_capacity(entries.len());
    for (product_id, year, number) in entries {
        let text_url = sources.nws_text_product_url(&product_id);
        let body = match client.get(&text_url).send().await {
            Ok(r) if r.status().is_success() => r.text().await.ok(),
            Ok(r) => {
                log::warn!("archived MD {product_id} returned {}", r.status());
                None
            }
            Err(e) => {
                log::warn!("archived MD {product_id} could not be fetched: {e}");
                None
            }
        };
        let Some(body) = body else { continue };

        let title = format!("Mesoscale Discussion {number}");
        let link = spc_archive_link(sources, year, number);
        // `at` resolves the day-of-month fields in the product's `VALID` line:
        // an archived body names "201819Z" and nothing else, and the instant
        // being asked about is the only thing that says which May.
        if let Some(md) = discussion_from_text(number, title, link, body, at) {
            discussions.push(md);
        }
    }

    log::info!("{} archived mesoscale discussion(s) valid at {at}", discussions.len());
    Ok(discussions)
}

#[cfg(test)]
#[path = "discussion_archive/tests.rs"]
mod tests;
