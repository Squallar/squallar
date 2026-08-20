use crate::site_position::{CATALOGUE_DISAGREEMENT_LIMIT_KM, SitePosition, SitePositionSource};
use crate::sites::RadarSite;
use crate::sites::get_radar_site;
use chrono::NaiveDateTime;
use nexrad_model::data::Radial;
use nexrad_model::data::Scan;
use rustdar_geo::{KM_PER_DEGREE_LAT, lat_rad_to_mercator_y};
use rustdar_units::{Quantity, UserPreferences};
use std::collections::HashMap;

/// The wasm32 side length, named **outside** the [`IMAGE_SIZE`] cascade so that
/// it is reachable from a host build's tests.
pub const WASM_IMAGE_SIZE: usize = 2048;

/// The native side length. See [`WASM_IMAGE_SIZE`].
pub const NATIVE_IMAGE_SIZE: usize = 2048;

/// The largest 2D texture WebGL2 — and so a browser — is *guaranteed* to accept
/// per axis.
pub const WEBGL2_MAX_TEXTURE_DIMENSION_2D: usize = 2048;

/// Side length, in pixels, of the square radar image a render produces at
/// [`BASE_EXTENT_KM`] and at every extent inside it.
/// An RGBA texture is `IMAGE_SIZE² × 4` bytes; a static pane render keeps an
/// `f32` value grid alongside it, doubling that.
#[cfg(target_arch = "wasm32")]
pub const IMAGE_SIZE: usize = WASM_IMAGE_SIZE;
#[cfg(not(target_arch = "wasm32"))]
pub const IMAGE_SIZE: usize = NATIVE_IMAGE_SIZE;

/// The half-width [`IMAGE_SIZE`] is calibrated at, km: the extent at which the
/// base texture is 4.4522 px/km.
pub const BASE_EXTENT_KM: f64 = 230.0;

/// The half-width to project a plan view at when the scan does not say how far
/// its data reached, km.
///
/// **A fallback and only a fallback.** [`plan_view_extent_km`] reaches it on a
/// `NaN` reach and on a non-positive one, which between them are the two ways a
/// render can be asked for a picture of nothing: a product no radial of the
/// sweep carries (`crate::render`'s `compute_max_range` answers 0.0), and a
/// derived grid that came back with no range bins. Both paint an empty raster,
/// so what this number decides is the size of an empty frame and never where an
/// echo goes.
pub const FALLBACK_EXTENT_KM: f64 = 230.0;

/// The furthest half-width a plan view will project at, km.
///
/// Not a range any radar reaches: the longest real reach in this display is a
/// WSR-88D surveillance cut at 2.125 + 1832 × 0.25 = 460.125 km, and a TDWR's
/// long-range reflectivity is 1390 × 0.3 km = 417 km. It is a ceiling on
/// *arithmetic*, because the extent is now derived from a gate count that
/// arrives over the wire: a mis-framed radial claiming sixty thousand gates
/// would otherwise zoom the whole display out to a continent. 470 km clears
/// the widest honest sweep by 9.9 km and turns every impossible one into a
/// render that is merely too coarse.
pub const MAX_EXTENT_KM: f64 = 470.0;

/// The half-width to project a plan view at, km: **how far this data reaches**,
/// and nothing else.
///
/// The reach comes from the sweep itself ([`crate::render`]'s
/// `compute_max_range`, the per-sweep counterpart of
/// [`crate::voxel::volume_reach_km`]), so this is the one place the raster's
/// geometry is decided and it is now a measurement rather than a decision.
/// [`MAX_EXTENT_KM`] is the only bound left, and it bounds *arithmetic*, not
/// data: a mis-framed radial claiming sixty thousand gates is refused.
pub fn plan_view_extent_km(data_reach_km: f64) -> f64 {
    // `is_nan` spelled out rather than folded into the comparison: every
    // ordering against a `NaN` is false, so `<= 0.0` alone would let one
    // through to arithmetic that propagates it.
    if data_reach_km.is_nan() || data_reach_km <= 0.0 {
        return FALLBACK_EXTENT_KM;
    }
    data_reach_km.min(MAX_EXTENT_KM)
}

/// How many pixels across to paint a plan view of `extent_km`, given the
/// largest side this caller can accept.
pub fn raster_side_px(extent_km: f64, side_ceiling_px: usize, sample_km: f64) -> usize {
    if extent_km > BASE_EXTENT_KM {
        side_ceiling_px.min(data_limited_side_px(extent_km, sample_km))
    } else {
        IMAGE_SIZE.min(side_ceiling_px)
    }
}

/// Texels per sample the raster is allowed to spend, at most.
///
/// Two, which is Nyquist: below it adjacent gates share a texel and detail the
/// radar measured is lost, above it the picture is sampling its own
/// interpolation rather than any new measurement.
pub const TEXELS_PER_SAMPLE: f64 = 2.0;

/// The largest side worth painting `extent_km` of a field sampled every
/// `sample_km` onto — the point past which more texels buy nothing.
///
/// **The data's own term** is `TEXELS_PER_SAMPLE` per sample across the
/// diameter: `2 · extent_km / sample_km · TEXELS_PER_SAMPLE`.
///
/// **The display's own term** is the scale the base texture is calibrated at,
/// `IMAGE_SIZE / (2 · BASE_EXTENT_KM)` = 4.4522 px/km.
///
/// So the answer is the larger of the two, and the effect is exactly one
/// direction: **a raster is never coarser than the scale this display has
/// always drawn at, and rises above it only as far as the samples justify.**
///
/// A non-positive or non-finite `sample_km` says nothing about sampling, so it
/// answers the display's term alone rather than dividing by it.
pub fn data_limited_side_px(extent_km: f64, sample_km: f64) -> usize {
    let reference_scale_px_per_km = IMAGE_SIZE as f64 / (2.0 * BASE_EXTENT_KM);
    let diameter_km = 2.0 * extent_km.max(0.0);
    let at_reference = diameter_km * reference_scale_px_per_km;
    // `is_finite` before the comparison, not folded into it: every ordering
    // against a `NaN` is false, so `> 0.0` alone would admit one and the
    // division would carry it into the side.
    let at_nyquist = if sample_km.is_finite() && sample_km > 0.0 {
        diameter_km / sample_km * TEXELS_PER_SAMPLE
    } else {
        0.0
    };
    // `ceil` and not `round`: a side one texel short of the data's own limit is
    // still short of it.
    (at_reference.max(at_nyquist).ceil() as usize).max(1)
}

/// m/s to mph conversion factor.
pub const MS_TO_MPH: f32 = 2.23694;

/// Geographic bounds of the rendered radar image. Pixels are linearly spaced
/// in Web Mercator Y and longitude, matching slippy-map tile providers.
#[derive(Debug, Clone, Copy)]
pub struct ImageBounds {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lon: f64,
    pub max_lon: f64,
    pub mercator_y_min: f64,
    pub mercator_y_max: f64,
}

impl ImageBounds {
    /// Extent is `extent_km` in every direction from the site — the number
    /// [`plan_view_extent_km`] chose for this render, not a constant.
    ///
    /// On [`KM_PER_DEGREE_LAT`], which is [`rustdar_geo::EARTH_RADIUS_KM`]
    /// — the same
    /// sphere [`crate::render::render_gate`] paints the gates inside these
    /// bounds on.
    pub fn from_radar_site(radar_lat: f64, radar_lon: f64, extent_km: f64) -> Self {
        let radar_lat_rad = radar_lat.to_radians();
        let lat_deg_per_km = 1.0 / KM_PER_DEGREE_LAT;
        let lon_deg_per_km = 1.0 / (KM_PER_DEGREE_LAT * radar_lat_rad.cos());

        let max_lat_offset = extent_km * lat_deg_per_km;
        let max_lon_offset = extent_km * lon_deg_per_km;

        let min_lat = radar_lat - max_lat_offset;
        let max_lat = radar_lat + max_lat_offset;

        ImageBounds {
            min_lat,
            max_lat,
            min_lon: radar_lon - max_lon_offset,
            max_lon: radar_lon + max_lon_offset,
            mercator_y_min: lat_rad_to_mercator_y(min_lat.to_radians()),
            mercator_y_max: lat_rad_to_mercator_y(max_lat.to_radians()),
        }
    }
}

/// The geographic half of an [`ImageBounds`]: the four lat/lon edges, copied.
impl From<ImageBounds> for rustdar_geo::GeoBounds {
    fn from(bounds: ImageBounds) -> Self {
        Self {
            min_lat: bounds.min_lat,
            max_lat: bounds.max_lat,
            min_lon: bounds.min_lon,
            max_lon: bounds.max_lon,
        }
    }
}

/// The same four edges, as the thing a delivery hands the display: where the
/// raster goes, worked out once.
impl From<ImageBounds> for rustdar_geo::PlacedRaster {
    fn from(bounds: ImageBounds) -> Self {
        rustdar_geo::PlacedRaster::of(bounds.into())
    }
}

#[derive(Debug, Clone)]
pub struct ScanInfo {
    /// Where this volume's radar is, and how high.
    ///
    /// **Not simply the table row.** The row is the starting point; a volume
    /// that states its own position overrides it, and a position learned from
    /// an earlier volume overrides it in a session where no fresh volume has
    /// arrived. [`ScanInfo::site_source`] says which of the three this is.
    pub site: RadarSite,
    /// Which of the three things above [`ScanInfo::site`] came from.
    pub site_source: SitePositionSource,
    /// The canonical integer position behind [`ScanInfo::site`], when there is
    /// one.
    ///
    /// `None` for [`SitePositionSource::Table`] and
    /// [`SitePositionSource::Unknown`] — the table's rows are `f64` literals
    /// and there is nothing measured to remember. `Some` for the other two.
    pub site_position: Option<SitePosition>,
    /// From the **first** radial of the **first** sweep, not the request.
    pub timestamp: NaiveDateTime,
    /// Volume Coverage Pattern number (e.g. 212, 215, 35)
    pub vcp_number: u16,
    pub available_products: Vec<RadarProduct>,
    /// Elevation angles per product, sorted ascending.
    ///
    /// **Accumulated by the UI, not a property of one volume.**
    pub product_elevations: HashMap<RadarProduct, Vec<f32>>,
    pub status: String,
}

/// Where a volume's radar sits once what it states about itself — or nothing —
/// is applied to the table row that names it.
///
/// One definition, because [`ScanInfo::from_scan`] and
/// [`ScanInfo::place_against_the_table`] have to agree: the second exists to
/// redo the first's decision against a table that has since learned the
/// radar, and two copies of this would let them drift.
fn placed_on(position: Option<&SitePosition>, row: &'static RadarSite) -> RadarSite {
    match position {
        Some(position) => position.applied_to(Some(row)),
        None => row.clone(),
    }
}

/// Whether the fetched catalogue agrees that `site` is where `stated` puts it,
/// or has nothing to say about `site` at all.
///
/// `true` when the catalogue cannot speak, which is the honest answer rather
/// than a lenient one.
fn confirmed_by_catalogue(site: &str, stated: &SitePosition) -> bool {
    let Some((lat, lon)) = crate::sites::catalogue_position(site) else {
        return true;
    };
    let apart_km = crate::sites::distance_km(stated.lat(), stated.lon(), lat, lon);
    if apart_km > CATALOGUE_DISAGREEMENT_LIMIT_KM {
        // Error, not warning. Every reachable cause is something somebody
        // needs to see: a corrupt Volume Data Block, a producer writing a
        // scale nothing here recognises, or a radar that genuinely relocated.
        log::error!(
            "volume for {site} states ({:.5}, {:.5}), {apart_km:.1} km from where the \
             catalogue places it ({lat:.5}, {lon:.5}); keeping the catalogue's position",
            stated.lat(),
            stated.lon(),
        );
        return false;
    }
    true
}

impl ScanInfo {
    /// Re-place this volume's radar against the site table as it stands now,
    /// answering whether that moved anything.
    ///
    /// A `ScanInfo` names its radar out of the table it was built against, so
    /// one built before that radar was in the table carries
    /// [`sites::UNKNOWN_SITE_NAME`](crate::sites::UNKNOWN_SITE_NAME) — the
    /// first volume of an install whose site catalogue is not cached yet, and
    /// which nothing has decoded a volume for. Every consumer that looks the
    /// volume up by that name then misses, which on the frontend means the
    /// volume is fetched and decoded and never rasterised.
    ///
    /// Call it wherever the table learns a radar mid-session: the position a
    /// volume states about itself, and the first fetched catalogue. A radar
    /// the table still cannot place is left exactly as it was — a row at
    /// (0, 0) would be worse than no picture.
    pub fn place_against_the_table(&mut self, site: &str) -> bool {
        if self.site.name != crate::sites::UNKNOWN_SITE_NAME {
            return false;
        }
        let Some(row) = crate::sites::get_radar_site(site) else {
            return false;
        };
        self.site = placed_on(self.site_position.as_ref(), row);
        true
    }

    /// Level III products are listed with empty elevation vectors, filled in
    /// later as L3 data arrives.
    /// 1. **The volume in hand.** Every Message 31 volume states its own
    ///    latitude, longitude and heights in its Volume Data Block.
    ///
    ///    **Within a kilometre of the fetched catalogue, and not otherwise.**
    ///    A radar reporting itself outranks a record about it by metres, which
    ///    is the scale radars actually move at.
    ///
    /// 2. **A position learned from an earlier volume**, supplied by the
    ///    caller out of its own store.
    ///
    /// 3. **[`crate::sites::radars()`]**, whatever this process has resolved.
    ///    Still the answer for a pre-2010 `AR2V0001` volume, which is Message 1
    ///    throughout and carries no Volume Data Block to read.
    ///
    /// A site none of the three can place gets
    /// [`SitePositionSource::Unknown`] and a placeholder row.
    pub fn from_scan(
        data: &Scan,
        site: &str,
        requested_timestamp: NaiveDateTime,
        learned: Option<SitePosition>,
    ) -> Self {
        let vcp_number = data.coverage_pattern_number().number();

        let row = get_radar_site(site);
        let stated = data
            .site()
            .and_then(SitePosition::from_volume)
            .filter(|stated| confirmed_by_catalogue(site, stated));
        let (site_position, site_source) = match (stated, learned, row.is_some()) {
            (Some(volume), _, _) => (Some(volume), SitePositionSource::Volume),
            (None, Some(learned), _) => (Some(learned), SitePositionSource::Learned),
            (None, None, true) => (None, SitePositionSource::Table),
            (None, None, false) => (None, SitePositionSource::Unknown),
        };

        // A radar this process knows of and cannot place has no `row` and is
        // still not anonymous: the catalogue listed its identifier, and
        // `sites` leaked it.
        let known_name = crate::sites::static_name(site);
        let radar_site = match (site_position, row) {
            (Some(position), Some(row)) => placed_on(Some(&position), row),
            (Some(position), None) => position
                .applied_to_named(known_name.unwrap_or(crate::sites::UNKNOWN_SITE_NAME), None),
            (None, Some(row)) => placed_on(None, row),
            (None, None) => {
                log::error!(
                    "no position for radar site '{site}': it is in no table row, \
                     its volume states none, and nothing was learned for it",
                );
                RadarSite {
                    name: known_name.unwrap_or(crate::sites::UNKNOWN_SITE_NAME),
                    lat: 0.0,
                    lon: 0.0,
                    heights: None,
                }
            }
        };

        let product_elevations = discover_product_elevations(data, &radar_site);

        let mut available_products: Vec<RadarProduct> =
            product_elevations.keys().copied().collect();
        available_products.sort_by_key(|p| p.sort_order());

        let actual_timestamp = data
            .sweeps()
            .first()
            .and_then(|s| s.radials().first())
            .and_then(|r| {
                chrono::DateTime::from_timestamp_millis(r.collection_timestamp())
                    .map(|dt| dt.naive_utc())
            })
            .unwrap_or(requested_timestamp);

        let status = format!(
            "Loaded {} products: {}",
            available_products.len(),
            available_products
                .iter()
                .map(|p| p.name())
                .collect::<Vec<_>>()
                .join(", ")
        );

        ScanInfo {
            site: radar_site,
            site_source,
            site_position,
            timestamp: actual_timestamp,
            vcp_number,
            available_products,
            product_elevations,
            status,
        }
    }
}

/// Rounds elevation angles to 0.1° so SAILS/MRLE repeat scans and split cuts
/// at the same nominal angle collapse to one entry.
///
/// The angle is the sweep's **median**
/// ([`crate::volumetric::sweep_elevation_deg`]), not its first radial's.
fn discover_product_elevations(scan: &Scan, site: &RadarSite) -> HashMap<RadarProduct, Vec<f32>> {
    let mut product_elevations: HashMap<RadarProduct, Vec<f32>> = HashMap::new();

    // Asked once of the volume, not once per sweep.
    let volume_is_dual_pol = scan.sweeps().iter().any(|sweep| {
        sweep.radials().first().is_some_and(|radial| {
            radial.differential_phase().is_some() && radial.correlation_coefficient().is_some()
        })
    });

    for (i, sweep) in scan.sweeps().iter().enumerate() {
        if let Some(first_radial) = sweep.radials().first() {
            let raw_angle = crate::volumetric::sweep_elevation_deg(sweep.radials())
                .unwrap_or_else(|| f64::from(first_radial.elevation_angle_degrees()));
            let elev_angle = (raw_angle * 10.0).round() as f32 / 10.0;

            let mut products_found: Vec<&str> = Vec::new();
            for product in RadarProduct::all() {
                // The one product whose moment slot does not stand for the data
                // it reads: reflectivity is where it *lists*, ΦDP and ρHV are
                // what it classifies from.
                if *product == RadarProduct::HydrometeorClassification && !volume_is_dual_pol {
                    continue;
                }
                if product.get_moment(first_radial).is_some() {
                    products_found.push(product.code());
                    product_elevations
                        .entry(*product)
                        .or_default()
                        .push(elev_angle);
                }
            }
            log::info!(
                "  Sweep {:2}: raw={:.2}° rounded={:.1}° radials={} products=[{}]",
                i,
                raw_angle,
                elev_angle,
                sweep.radials().len(),
                products_found.join(", ")
            );
        } else {
            log::warn!("  Sweep {:2}: no radials!", i);
        }
    }

    for angles in product_elevations.values_mut() {
        angles.sort_by(|a, b| a.total_cmp(b));
        angles.dedup();
    }
    for (product, angles) in &product_elevations {
        log::info!(
            "  {} → {} unique elevations: {:?}",
            product.code(),
            angles.len(),
            angles
        );
    }

    // Level III objects are made by an RPG, and only the WSR-88D network has
    // one. A TDWR is served by the Supplemental Product Generator, which
    // publishes its own short list and none of the four objects
    // [`RadarProduct::level3_products`] names.
    if site.is_wsr88d() {
        for l3_product in RadarProduct::all().iter().filter(|p| p.is_level3()) {
            product_elevations.entry(*l3_product).or_default();
        }
    }

    product_elevations
}

/// A Level II moment field on a [`Radial`], named rather than read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MomentSlot {
    Reflectivity,
    Velocity,
    SpectrumWidth,
    DifferentialReflectivity,
    DifferentialPhase,
    CorrelationCoefficient,
}

impl MomentSlot {
    /// This field's value on `radial`.
    pub fn read<'a>(&self, radial: &'a Radial) -> Option<&'a nexrad_model::data::MomentData> {
        match self {
            MomentSlot::Reflectivity => radial.reflectivity(),
            MomentSlot::Velocity => radial.velocity(),
            MomentSlot::SpectrumWidth => radial.spectrum_width(),
            MomentSlot::DifferentialReflectivity => radial.differential_reflectivity(),
            MomentSlot::DifferentialPhase => radial.differential_phase(),
            MomentSlot::CorrelationCoefficient => radial.correlation_coefficient(),
        }
    }

    /// The product that **is** this moment, rather than one computed from it.
    pub fn product(&self) -> RadarProduct {
        match self {
            MomentSlot::Reflectivity => RadarProduct::Reflectivity,
            MomentSlot::Velocity => RadarProduct::Velocity,
            MomentSlot::SpectrumWidth => RadarProduct::SpectrumWidth,
            MomentSlot::DifferentialReflectivity => RadarProduct::DifferentialReflectivity,
            MomentSlot::DifferentialPhase => RadarProduct::DifferentialPhase,
            MomentSlot::CorrelationCoefficient => RadarProduct::CorrelationCoefficient,
        }
    }
}

/// Why a cell of a decoded grid has no number — or, for [`GateReport::Value`],
/// that it has one.
///
/// The decoder answers a gate query four ways and a dense `f64`/`f32` grid can
/// only write one of them, so the other three arrive as the same `NaN` and the
/// consumer cannot tell them apart. They are not the same fact:
///
/// * [`BelowThreshold`](Self::BelowThreshold) is a **measurement**. The radar
///   illuminated that gate and found nothing above the moment's signal
///   threshold. "Empty" is what it observed.
/// * [`RangeFolded`](Self::RangeFolded) is also a measurement, and the
///   *opposite* one: there is signal, and only its range is ambiguous.
/// * [`NotReported`](Self::NotReported) is the sole genuine absence — no gate
///   exists there to have said anything.
///
///
/// [`Ord`] is derived over the declaration order below, so `max` is the rule
/// for collapsing several gates into one cell: a measured number beats
/// ambiguous signal, ambiguous signal beats measured emptiness, and measured
/// emptiness beats no gate at all. Reordering the variants silently changes
/// what every aggregating grid reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u8)]
pub enum GateReport {
    /// No gate covered this cell: the radial carried no such moment, the cell
    /// is past the moment's last gate, or no radial served the azimuth. The
    /// default, because a grid starts out having been told nothing.
    #[default]
    NotReported = 0,
    /// Every gate under this cell was below the moment's signal threshold
    /// (raw code 0). The radar looked and saw nothing — a measurement of
    /// absence, not an absence of measurement.
    BelowThreshold = 1,
    /// A gate under this cell was range folded (raw code 1) and none carried a
    /// value: signal is present, and only its range is ambiguous past the
    /// unambiguous range of the cut's PRF.
    RangeFolded = 2,
    /// At least one gate under this cell carried a number, so the grid's own
    /// value is defined here.
    Value = 3,
}

impl GateReport {
    /// What one `MomentValue` reports, before any cell aggregation.
    pub fn of(value: &nexrad_model::data::MomentValue) -> Self {
        match value {
            nexrad_model::data::MomentValue::Value(_) => Self::Value,
            nexrad_model::data::MomentValue::BelowThreshold => Self::BelowThreshold,
            nexrad_model::data::MomentValue::RangeFolded => Self::RangeFolded,
        }
    }

    /// Whether the radar *looked* at this cell, whatever it found.
    pub fn is_measured(self) -> bool {
        self != Self::NotReported
    }
}

/// What a render *draws*, as opposed to what it draws it of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderView {
    /// The plan-view raster every render produced before cross-sections
    /// existed.
    PlanView,
    /// A vertical slice along a line.
    CrossSection,
    /// A resampled Cartesian grid, for a raymarch.
    Volume,
}

impl RenderView {
    /// Whether a render of this view reads every tilt carrying the moment,
    /// rather than the one sweep `crate::render::find_sweep` picks.
    pub fn reads_whole_volume(self) -> bool {
        match self {
            Self::PlanView => false,
            Self::CrossSection | Self::Volume => true,
        }
    }

    /// Whether a pane producing this view can animate a sequence of past
    /// volumes.
    pub fn can_loop(self) -> bool {
        match self {
            Self::PlanView | Self::CrossSection | Self::Volume => true,
        }
    }

    /// Whether the pane's **selected elevation** chooses which picture a render
    /// of this view showing `product` produces — and therefore whether anything
    /// holding such a render has to key on the tilt.
    ///
    /// `false` means the tilt is not part of that render's identity: two
    /// renders of one `(site, product, view)` at different selections are the
    /// same bytes, so a cache may collapse them into one slot and a loop may
    /// keep its frames across a tilt click.
    pub fn elevation_selects_picture(self, product: RadarProduct) -> bool {
        match self {
            Self::PlanView => !product.tilt_independent_plan_view(),
            Self::CrossSection | Self::Volume => false,
        }
    }

    /// A stable byte for the wire and for a cache key, **not** the declaration
    /// order.
    pub fn wire_code(self) -> u8 {
        match self {
            Self::PlanView => 1,
            Self::CrossSection => 2,
            Self::Volume => 3,
        }
    }

    /// The view a [`wire_code`](Self::wire_code) names, or `None` for a byte
    /// this build does not have — the two ends of a worker port can be
    /// different builds.
    pub fn from_wire_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::PlanView),
            2 => Some(Self::CrossSection),
            3 => Some(Self::Volume),
            _ => None,
        }
    }

    /// Every view there is, for the sweeps that have to cover all of them.
    pub fn all() -> &'static [RenderView] {
        &[
            RenderView::PlanView,
            RenderView::CrossSection,
            RenderView::Volume,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum RadarProduct {
    Reflectivity,
    Velocity,
    SpectrumWidth,
    DifferentialPhase,
    CorrelationCoefficient,
    DifferentialReflectivity,
    StormRelativeVelocity,
    SpecificDifferentialPhase,
    EchoTops,
    EchoTopsInterpolated,
    VerticallyIntegratedLiquid,
    VilDensity,
    ProbabilityOfSevereHail,
    MaxExpectedHailSize,
    HydrometeorClassification,
    PrecipitationRate,
    NormalizedRotation,
}

impl RadarProduct {
    pub fn code(&self) -> &'static str {
        crate::product_spec::spec(*self).code
    }

    pub fn name(&self) -> &'static str {
        crate::product_spec::spec(*self).name
    }

    pub fn all() -> &'static [RadarProduct] {
        &[
            RadarProduct::Reflectivity,
            RadarProduct::Velocity,
            RadarProduct::SpectrumWidth,
            RadarProduct::DifferentialPhase,
            RadarProduct::CorrelationCoefficient,
            RadarProduct::DifferentialReflectivity,
            RadarProduct::StormRelativeVelocity,
            RadarProduct::SpecificDifferentialPhase,
            RadarProduct::EchoTops,
            RadarProduct::EchoTopsInterpolated,
            RadarProduct::VerticallyIntegratedLiquid,
            RadarProduct::VilDensity,
            RadarProduct::ProbabilityOfSevereHail,
            RadarProduct::MaxExpectedHailSize,
            RadarProduct::HydrometeorClassification,
            RadarProduct::PrecipitationRate,
            RadarProduct::NormalizedRotation,
        ]
    }

    /// Order products are listed in the UI.
    pub fn sort_order(&self) -> u8 {
        crate::product_spec::spec(*self).sort_order
    }

    pub fn is_level3(&self) -> bool {
        crate::product_spec::spec(*self).is_level3
    }

    /// The AWIPS product IDs to fetch for this product. These key the
    /// `unidata-nexrad-level3` bucket (`TLX_N0S_2026_07_25_...`). `None` for
    /// Level II products.
    pub fn level3_products(&self) -> Option<&'static [&'static str]> {
        crate::product_spec::spec(*self).level3_codes
    }

    /// Every product whose [`level3_products`](Self::level3_products) names
    /// `code` — the inverse of that table, derived from it rather than written
    /// out a second time.
    pub fn level3_readers(code: &str) -> Vec<RadarProduct> {
        let mut readers: Vec<RadarProduct> = Self::all()
            .iter()
            .copied()
            .filter(|p| {
                p.level3_products()
                    .is_some_and(|codes| codes.contains(&code))
            })
            .collect();
        readers.sort_by_key(|p| p.sort_order());
        readers
    }

    /// The distinct AWIPS objects `products` need between them, each named once.
    pub fn level3_codes_for(products: &[RadarProduct]) -> Vec<&'static str> {
        let mut codes: Vec<&'static str> = products
            .iter()
            .filter_map(|p| p.level3_products())
            .flatten()
            .copied()
            .collect();
        codes.sort_unstable();
        codes.dedup();
        codes
    }

    /// Which object of a paired volume this product's Level III rendition is —
    /// what [`crate::level3::product_from_candidates`] is given when a
    /// particular volume's object is wanted (a loop frame, a validation twin).
    ///
    /// [`crate::level3::VolumePick::Latest`] for the QPE family, which emits an
    /// end-of-volume composite *plus* a partial intermediate per SAILS/MRLE
    /// scan under the same volume start: the nearest-to-start candidate there is
    /// an intermediate, and a loop paired that way would animate partial
    /// accumulations. Nearest for everything else, which publishes once per
    /// volume.
    ///
    /// Meaningless for a Level II product, and it says so — `None` rather than a
    /// default nobody should read.
    pub fn level3_volume_pick(&self) -> Option<crate::level3::VolumePick> {
        crate::product_spec::spec(*self).level3_volume_pick
    }

    /// A stable identifier for this product on a wire.
    pub fn wire_code(&self) -> u16 {
        crate::product_spec::spec(*self).wire_code
    }

    /// The inverse of [`wire_code`](Self::wire_code). `None` for a code this
    /// build does not know, which is a message from another build rather than a
    /// bug to panic on.
    pub fn from_wire_code(code: u16) -> Option<Self> {
        Self::all()
            .iter()
            .copied()
            .find(|p| crate::product_spec::spec(*p).wire_code == code)
    }

    /// Which of a radial's moment fields this product reads.
    pub fn moment_slot(&self) -> Option<MomentSlot> {
        crate::product_spec::spec(*self).moment_slot
    }

    /// The moment data for this product on a radial.
    pub fn get_moment<'a>(&self, radial: &'a Radial) -> Option<&'a nexrad_model::data::MomentData> {
        self.moment_slot()?.read(radial)
    }

    /// Whether this product **is** a moment the RDA put on the wire, rather
    /// than a field computed from one.
    pub fn is_wire_moment(&self) -> bool {
        self.moment_slot()
            .is_some_and(|slot| slot.product() == *self)
    }

    /// Whether this product reads every tilt carrying its moment, rather than
    /// the one sweep `crate::render::find_sweep` picks.
    pub fn reads_whole_volume(&self) -> bool {
        crate::product_spec::spec(*self).reads_whole_volume
    }

    /// Whether a **plan view** of this product draws the same picture whatever
    /// tilt is selected, so everything that keys a plan-view raster on the
    /// elevation may drop that half of the key.
    ///
    /// Four Level II products qualify, and they are the four
    /// [`crate::render::render_radar_to_image_full`] dispatches *before* it
    /// calls `find_sweep`: interpolated echo tops, the hail pair, and the
    /// hybrid classification. Each reduces the whole volume to one polar grid,
    /// and the `elevation_angle` argument reaches no line of any of them.
    pub fn tilt_independent_plan_view(&self) -> bool {
        !self.is_level3() && crate::derive::volume_slot(*self).is_none()
    }

    /// Whether this product's picture is a function of the environmental
    /// 0 °C / −20 °C heights — the per-site pair a sounding lands
    /// ([`crate::sounding`]), which rides the render parameters rather than a
    /// moment because no radial carries it.
    pub fn reads_env_heights(&self) -> bool {
        crate::product_spec::spec(*self).reads_env_heights
    }

    /// Format a radar product value for display (e.g. in a hover tooltip).
    pub fn format_value(&self, value: f32, prefs: &UserPreferences) -> String {
        match self {
            RadarProduct::Reflectivity => format!("Reflectivity: {:.1} dBZ", value),
            RadarProduct::Velocity | RadarProduct::StormRelativeVelocity => {
                let converted = prefs.speed.convert_from_ms(value);
                format!("{}: {:.1} {}", self.name(), converted, prefs.speed.suffix())
            }
            RadarProduct::SpectrumWidth => {
                let converted = prefs.speed.convert_from_ms(value);
                format!("Spectrum Width: {:.1} {}", converted, prefs.speed.suffix())
            }
            RadarProduct::DifferentialReflectivity => {
                format!("Diff. Reflectivity: {:.2} dB", value)
            }
            RadarProduct::CorrelationCoefficient => format!("Corr. Coefficient: {:.4}", value),
            RadarProduct::DifferentialPhase => format!("Diff. Phase: {:.1}°", value),
            RadarProduct::SpecificDifferentialPhase => format!("KDP: {:.2} °/km", value),
            RadarProduct::EchoTops | RadarProduct::EchoTopsInterpolated => {
                let converted = prefs.height.convert_kft_to_kilo(value);
                format!(
                    "{}: {:.1} {}",
                    self.name(),
                    converted,
                    prefs.height.kilo_suffix()
                )
            }
            RadarProduct::VerticallyIntegratedLiquid => format!("VIL: {:.1} kg/m²", value),
            RadarProduct::VilDensity => format!("VIL Density: {:.2} g/m³", value),
            RadarProduct::ProbabilityOfSevereHail => format!("POSH: {:.0}%", value),
            // The field computes in mm (`crate::hail`); the render seam
            // converts to inches, so the value arrives here in inches — the
            // unit US hail sizes are reported in.
            RadarProduct::MaxExpectedHailSize => {
                let converted = prefs.hail_size.convert_from_inches(value);
                let decimals = prefs.hail_size.decimals();
                format!("MEHS: {converted:.decimals$} {}", self.unit_label(prefs))
            }
            RadarProduct::HydrometeorClassification => {
                let class = match value as u16 {
                    0..=9 => "No Data",
                    10..=19 => "Biological",
                    20..=29 => "Clutter/AP",
                    30..=39 => "Ice Crystals",
                    40..=49 => "Dry Snow",
                    50..=59 => "Wet Snow",
                    60..=69 => "Rain",
                    70..=79 => "Heavy Rain",
                    80..=89 => "Big Drops",
                    90..=99 => "Graupel",
                    100..=109 => "Hail+Rain",
                    110..=119 => "Large Hail",
                    120..=139 => "Giant Hail",
                    140..=149 => "Unknown",
                    150.. => "Range Folded",
                };
                format!("HHC: {class}")
            }
            RadarProduct::PrecipitationRate => {
                let converted = prefs.precip_rate.convert_from_in_per_hr(value);
                format!(
                    "Precip Rate: {:.2} {}",
                    converted,
                    prefs.precip_rate.suffix()
                )
            }
            RadarProduct::NormalizedRotation => format!("NROT: {:.2}", value),
        }
    }

    /// Short unit label for this product (used in the color scale legend).
    pub fn unit_label(&self, prefs: &UserPreferences) -> &'static str {
        match self.quantity() {
            Quantity::Unitless { label } => label,
            q => q.suffix(prefs),
        }
    }

    /// The unit domain this product's values live in, from the registration.
    pub(crate) fn quantity(&self) -> Quantity {
        crate::product_spec::spec(*self).quantity
    }
}

#[cfg(test)]
mod tests;
