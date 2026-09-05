//! **What each layer's item data owns on the heap** — every
//! [`ItemFootprint`] this crate's [`OverlayState`] types need, in one file.
//!
//! [`OverlayState`]: squallar_source::handler::OverlayState
//!
//! # Why one file rather than one impl per handler
//!
//! Two reasons, and the second is the load-bearing one.
//!
//! A footprint is a fact about a **type**, not about the handler that holds
//! it, and reading them together is what makes the double counts visible: the
//! alert list and the paint snapshot built from it share one
//! `Arc<Vec<OverlayFeature>>`, and the rule that only the list prices it is
//! only checkable if both prices are on the same screen.
//!
//! And an impl may live in any module of this crate, which is what lets a
//! layer whose own file is fenced off be priced at all. Every figure here is
//! the allocator's — `capacity()`, never a model of one.
//!
//! # What is priced at ZERO on purpose
//!
//! The three gridded layers (MRMS, GMGSI, HRRR). Their decoded grids are the
//! `overlay grids` census family already, published through
//! [`SourceHandler::resident_source_bytes`], and pricing them here as well
//! would put the same tens of megabytes into two figures a reader is invited
//! to add. Each says so at its impl.
//!
//! [`SourceHandler::resident_source_bytes`]: squallar_source::handler::SourceHandler::resident_source_bytes

use squallar_source::footprint::{ItemFootprint, arc_body};

use crate::metar::types::{CloudLayer, MetarOb};
use crate::nws::alert::NwsAlert;
use crate::render::handlers::sites::SiteRow;
use crate::spc::discussion::SpcDiscussion;
use crate::spc::firewx::SpcFireOutlook;
use crate::spc::outlook::SpcOutlook;
use crate::spc::reports::StormReport;

// ── Scalars and discriminants ────────────────────────────────────────────

squallar_source::impl_pod_footprint!(
    crate::spc::outlook::OutlookDay,
    crate::spc::outlook::OutlookProduct,
    crate::spc::firewx::FireDay,
    crate::spc::firewx::FireHazard,
    crate::spc::firewx::FireProduct,
    crate::metar::types::FlightCategory,
    crate::metar::types::Visibility,
    crate::metar::types::WindDir,
    crate::spc::reports::StormReportKind,
    crate::spc::discussion::MdType,
    crate::nws::alert::AlertCategory,
    // A flash row: two coordinates, two optional energies, an instant and two
    // discriminants, all inline.
    crate::glm::GlmFlash,
    // And the paint row built from it: the four terms the rasterizer reads,
    // all inline.
    crate::render::rasterize::FlashPaint,
);

// ── The feature layers ───────────────────────────────────────────────────

impl ItemFootprint for SiteRow {
    fn owned_bytes(&self) -> u64 {
        self.name.owned_bytes()
    }
}

impl ItemFootprint for CloudLayer {
    fn owned_bytes(&self) -> u64 {
        self.cover.owned_bytes()
    }
}

/// Six strings and the cloud layers. Everything else is a scalar or an
/// `Option` of one, inside the observation's own `size_of`.
impl ItemFootprint for MetarOb {
    fn owned_bytes(&self) -> u64 {
        [
            self.station_id.owned_bytes(),
            self.name.owned_bytes(),
            self.raw_ob.owned_bytes(),
            self.obs_time.owned_bytes(),
            self.wx_string.owned_bytes(),
            self.clouds.owned_bytes(),
        ]
        .into_iter()
        .fold(0u64, u64::saturating_add)
    }
}

/// The four formatted strings the station model draws, held so the frame
/// thread formats nothing.
impl ItemFootprint for crate::render::station_model::StationText {
    fn owned_bytes(&self) -> u64 {
        [
            self.temp_f.owned_bytes(),
            self.dewp_f.owned_bytes(),
            self.pressure_code.owned_bytes(),
            self.visibility.owned_bytes(),
        ]
        .into_iter()
        .fold(0u64, u64::saturating_add)
    }
}

/// **The feed's own text**, which is most of an alert: `description` alone is
/// a full bulletin. The zone geometry is behind an `Arc` this list created,
/// so this is where its body is priced — the paint snapshot that clones the
/// same `Arc` does not price it again.
impl ItemFootprint for NwsAlert {
    fn owned_bytes(&self) -> u64 {
        [
            self.id.owned_bytes(),
            self.event.owned_bytes(),
            self.headline.owned_bytes(),
            self.description.owned_bytes(),
            self.instruction.owned_bytes(),
            self.area_desc.owned_bytes(),
            self.sender_name.owned_bytes(),
            self.effective.owned_bytes(),
            self.expires.owned_bytes(),
            self.onset.owned_bytes(),
            self.ends.owned_bytes(),
            self.affected_zones.owned_bytes(),
            arc_body(&self.features),
        ]
        .into_iter()
        .fold(0u64, u64::saturating_add)
    }
}

impl ItemFootprint for StormReport {
    fn owned_bytes(&self) -> u64 {
        [
            self.time.owned_bytes(),
            self.location.owned_bytes(),
            self.county.owned_bytes(),
            self.state.owned_bytes(),
            self.comments.owned_bytes(),
        ]
        .into_iter()
        .fold(0u64, u64::saturating_add)
    }
}

/// The discussion's own text plus **two** copies of its geometry: the raw
/// rings and the [`OverlayFeature`](squallar_source::feature::OverlayFeature)
/// built from them. Both are really held; the figure says so.
impl ItemFootprint for SpcDiscussion {
    fn owned_bytes(&self) -> u64 {
        [
            self.title.owned_bytes(),
            self.text.owned_bytes(),
            self.link.owned_bytes(),
            self.concerning.owned_bytes(),
            self.polygon.owned_bytes(),
            self.feature.owned_bytes(),
        ]
        .into_iter()
        .fold(0u64, u64::saturating_add)
    }
}

impl ItemFootprint for SpcOutlook {
    fn owned_bytes(&self) -> u64 {
        self.features.owned_bytes()
    }
}

impl ItemFootprint for SpcFireOutlook {
    fn owned_bytes(&self) -> u64 {
        self.features.owned_bytes()
    }
}

// ── The item wrappers ────────────────────────────────────────────────────

/// **One poll's flashes, in one heap block.**
///
/// A flash is scalars — two coordinates, two optional energies, an instant
/// and two discriminants — so the slab's whole cost is the one buffer its
/// rows live in. At the ~125 000 flashes a busy 20 s poll delivers that is
/// the largest single item figure in the census.
///
/// **Not the same bytes as the layer's S3 granule cache**, which
/// `GlmHandler::resident_source_bytes` prices into `overlay grids` instead.
/// The cache holds a `Vec<GlmFlash>` per granule across polls; this slab is
/// the fresh `Vec` `flashes_in_window` **cloned** out of them for one poll and
/// installed. Two allocations, two lifetimes, and each figure names one of
/// them — which is the double count this file exists to make visible.
impl ItemFootprint for crate::render::handlers::glm::GlmSlab {
    fn owned_bytes(&self) -> u64 {
        self.flashes.owned_bytes()
    }
}

/// The item ONE click resolves to, built on demand out of the slab. Priced
/// for completeness; nothing holds a list of these any more.
impl ItemFootprint for crate::render::handlers::glm::GlmFlashItem {
    fn owned_bytes(&self) -> u64 {
        0
    }
}

impl ItemFootprint for crate::render::handlers::alert::AlertItem {
    fn owned_bytes(&self) -> u64 {
        self.alert.owned_bytes()
    }
}

impl ItemFootprint for crate::render::handlers::reports::StormReportItem {
    fn owned_bytes(&self) -> u64 {
        self.report.owned_bytes()
    }
}

impl ItemFootprint for crate::render::handlers::discussion::DiscussionItem {
    fn owned_bytes(&self) -> u64 {
        self.md.owned_bytes()
    }
}

// ── The gridded layers, at zero and on purpose ───────────────────────────

/// **Zero here because it is counted in `overlay grids`.** An MRMS mosaic is
/// tens of megabytes of `f32`, priced by
/// [`SourceHandler::resident_source_bytes`] and published as its own census
/// family; adding it to the item family as well would put the same bytes in
/// two figures on one line.
///
/// [`SourceHandler::resident_source_bytes`]: squallar_source::handler::SourceHandler::resident_source_bytes
impl ItemFootprint for crate::mrms::MrmsGrid {
    fn owned_bytes(&self) -> u64 {
        0
    }
}

/// Zero for the same reason as [`crate::mrms::MrmsGrid`]: the satellite
/// granule is `overlay grids`.
impl ItemFootprint for crate::render::gridded::ResidentGrid {
    fn owned_bytes(&self) -> u64 {
        0
    }
}

/// Zero for the same reason as [`crate::mrms::MrmsGrid`]: the model grid is
/// `overlay grids`.
impl ItemFootprint for crate::hrrr::HrrrGridData {
    fn owned_bytes(&self) -> u64 {
        0
    }
}

// ── Built paint inputs, for the memos that park them ─────────────────────
//
// These are the `price` a `BuiltMemo` is constructed with. Each takes what
// the memo actually holds and answers what freeing it would give back.

use squallar_source::job::DescribedJob;

use crate::render::rasterize::{
    AlertsInput, CoverageInput, DiscussionPaint, DiscussionsInput, MetarInput, OutlooksInput,
    ReportPaint, ReportsInput,
};

squallar_source::impl_pod_footprint!(ReportPaint, crate::render::rasterize::CoverageSite);

impl ItemFootprint for DiscussionPaint {
    fn owned_bytes(&self) -> u64 {
        self.polygon.owned_bytes()
    }
}

/// **The alert rows, WITHOUT the geometry.**
///
/// `AlertPaint::features` is an `Arc` clone of the body
/// [`NwsAlert::owned_bytes`] already priced. Freeing this input frees the
/// pointers and the ids; the rings stay, because the alert list still holds
/// them. Pricing them here would double count inside one census figure.
fn alerts_input_bytes(input: &AlertsInput) -> u64 {
    let rows = (input.alerts.capacity() * size_of::<crate::render::rasterize::AlertPaint>()) as u64;
    let ids = input.alerts.iter().fold(0u64, |sum, paint| {
        sum.saturating_add(paint.id.owned_bytes())
    });
    rows.saturating_add(ids)
        .saturating_add(input.enabled_categories.owned_bytes())
        .saturating_add(input.hidden_ids.owned_bytes())
}

/// **The storm-report rows, whole.** Unlike the alerts, these rows are BUILT
/// — a `ReportPaint` per report, not a share of one — so freeing the input
/// frees all of it.
fn reports_rows_bytes(rows: &std::sync::Arc<Vec<ReportPaint>>) -> u64 {
    arc_body(rows)
}

/// The observations, whole: the memo holds a **copy** cloned out of the
/// items, not a share of them.
fn metar_rows_bytes(rows: &std::sync::Arc<Vec<MetarOb>>) -> u64 {
    arc_body(rows)
}

/// One [`DescribedJob`] memo's price, for the row whose input is a `T`.
///
/// A memo holds exactly one input type — the layer's own — so a downcast
/// that misses is a wiring mistake, and it answers zero rather than
/// panicking on a frame path.
fn described<T: squallar_source::job::JobInput>(job: &DescribedJob, price: fn(&T) -> u64) -> u64 {
    job.downcast_ref::<T>().map_or(0, price)
}

pub(crate) fn alerts_job(job: &DescribedJob) -> u64 {
    described(job, alerts_input_bytes)
}

pub(crate) fn outlooks_job(job: &DescribedJob) -> u64 {
    described(job, |input: &OutlooksInput| input.features.owned_bytes())
}

pub(crate) fn discussions_job(job: &DescribedJob) -> u64 {
    described(job, |input: &DiscussionsInput| {
        input.discussions.owned_bytes()
    })
}

pub(crate) fn coverage_job(job: &DescribedJob) -> u64 {
    described(job, |input: &CoverageInput| input.sites.owned_bytes())
}

pub(crate) fn metar_job(rows: &std::sync::Arc<Vec<MetarOb>>) -> u64 {
    metar_rows_bytes(rows)
}

pub(crate) fn reports_rows(rows: &std::sync::Arc<Vec<ReportPaint>>) -> u64 {
    reports_rows_bytes(rows)
}

/// **The lightning layer's built paint rows** — the memo's whole row set,
/// which at the ~125 000 flashes a busy 20 s poll delivers is the largest
/// single figure in the parked family.
///
/// `arc_body`, not the pointer price: the memo CREATED this body out of the
/// slab, so nothing else in the census holds it. The slab those rows were
/// taken off is the same layer's item data and a separate 48 bytes a flash —
/// two disjoint figures over one granule, which is the whole reason the item
/// and parked families are read apart.
pub(crate) fn glm_flash_rows(
    rows: &std::sync::Arc<Vec<crate::render::rasterize::FlashPaint>>,
) -> u64 {
    arc_body(rows)
}

/// The METAR job's own input, for the memo that holds whole jobs rather than
/// the row set — kept beside the others so every built shape has a price.
#[allow(dead_code)]
pub(crate) fn metar_input_job(job: &DescribedJob) -> u64 {
    described(job, |input: &MetarInput| arc_body(&input.obs))
}

/// The reports job's own input; see [`metar_input_job`].
#[allow(dead_code)]
pub(crate) fn reports_input_job(job: &DescribedJob) -> u64 {
    described(job, |input: &ReportsInput| arc_body(&input.reports))
}

#[cfg(test)]
#[path = "footprint/tests.rs"]
mod tests;
