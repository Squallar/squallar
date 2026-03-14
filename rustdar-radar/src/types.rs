use chrono::NaiveDateTime;
use nexrad_model::data::Radial;
use nexrad_model::data::Scan;
use std::collections::HashMap;
use std::f64::consts::PI;
use crate::sites::RadarSite;
use crate::sites::get_radar_site;

pub const IMAGE_SIZE: usize = 1800; // 1800x1800 pixels for radar image
pub const MAX_RANGE_KM: f64 = 230.0; // NEXRAD max range ~230km
pub const PIXELS_PER_KM: f64 = IMAGE_SIZE as f64 / (2.0 * MAX_RANGE_KM);
/// m/s to mph conversion factor.
pub const MS_TO_MPH: f32 = 2.23694;

/// Convert latitude (in radians) to Web Mercator Y coordinate.
/// Returns a unitless value; the scale is consistent for relative comparisons.
#[inline]
pub(crate) fn lat_rad_to_mercator_y(lat_rad: f64) -> f64 {
    (PI / 4.0 + lat_rad / 2.0).tan().ln()
}

/// Geographic bounds of the rendered radar image.
/// The image pixels are linearly spaced in Web Mercator Y and longitude,
/// matching the projection used by slippy-map tile providers (CartoDB, OSM).
#[derive(Debug, Clone, Copy)]
pub struct ImageBounds {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lon: f64,
    pub max_lon: f64,
    /// Mercator Y value corresponding to `min_lat` (south edge).
    pub mercator_y_min: f64,
    /// Mercator Y value corresponding to `max_lat` (north edge).
    pub mercator_y_max: f64,
}

impl ImageBounds {
    /// Compute the geographic bounds of a radar image centered on a site.
    /// Uses `MAX_RANGE_KM` to define the image extent. The vertical axis
    /// is mapped in Web Mercator Y so the image aligns with slippy-map tiles.
    pub fn from_radar_site(radar_lat: f64, radar_lon: f64) -> Self {
        let radar_lat_rad = radar_lat.to_radians();
        let lat_deg_per_km = 1.0 / 111.32;
        let lon_deg_per_km = 1.0 / (111.32 * radar_lat_rad.cos());

        let max_lat_offset = MAX_RANGE_KM * lat_deg_per_km;
        let max_lon_offset = MAX_RANGE_KM * lon_deg_per_km;

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

    /// Convert geographic coordinates to image pixel coordinates.
    /// Uses Web Mercator Y mapping for the vertical axis.
    /// Returns `(px, py)` or `None` if outside bounds.
    pub fn geo_to_pixel(&self, lat: f64, lon: f64) -> Option<(usize, usize)> {
        let lon_frac = (lon - self.min_lon) / (self.max_lon - self.min_lon);
        let merc_y = lat_rad_to_mercator_y(lat.to_radians());
        let merc_frac = (merc_y - self.mercator_y_min) / (self.mercator_y_max - self.mercator_y_min);

        if merc_frac < 0.0 || merc_frac > 1.0 || lon_frac < 0.0 || lon_frac > 1.0 {
            return None;
        }

        let px = (lon_frac * IMAGE_SIZE as f64) as usize;
        let py = ((1.0 - merc_frac) * IMAGE_SIZE as f64) as usize;

        if px < IMAGE_SIZE && py < IMAGE_SIZE {
            Some((px, py))
        } else {
            None
        }
    }
}

/// Information about a loaded radar scan
#[derive(Debug, Clone)]
pub struct ScanInfo {
    /// The radar site code
    pub site: RadarSite,
    /// The actual timestamp of the scan data
    pub timestamp: NaiveDateTime,
    /// Volume Coverage Pattern number (e.g. 212, 215, 35)
    pub vcp_number: u16,
    /// Available products in this scan
    pub available_products: Vec<RadarProduct>,
    /// Map of product to available elevation angles (sorted)
    pub product_elevations: HashMap<RadarProduct, Vec<f32>>,
    /// Status message
    pub status: String,
}

impl ScanInfo {
    /// Build a `ScanInfo` by inspecting every sweep/radial in the scan.
    ///
    /// This discovers which radar products are present and at which elevation
    /// angles. Level III products are added to the available list with empty
    /// elevation vectors (populated later as L3 data arrives).
    pub fn from_scan(data: &Scan, site: &str, requested_timestamp: NaiveDateTime) -> Self {
        let vcp_number = data.coverage_pattern_number().number();

        let product_elevations = discover_product_elevations(data);

        // Get list of available products, sorted by priority
        let mut available_products: Vec<RadarProduct> =
            product_elevations.keys().copied().collect();
        available_products.sort_by_key(|p| p.sort_order());

        // Extract actual timestamp from the first radial's collection timestamp
        let actual_timestamp = data
            .sweeps()
            .first()
            .and_then(|s| s.radials().first())
            .and_then(|r| {
                chrono::DateTime::from_timestamp_millis(r.collection_timestamp())
                    .map(|dt| dt.naive_utc())
            })
            .unwrap_or(requested_timestamp);

        let radar_site = get_radar_site(site).unwrap_or_else(|| {
            log::warn!("Unknown radar site '{}', using fallback location", site);
            RadarSite {
                name: "UNKNOWN",
                lat: 0.0,
                lon: 0.0,
                elev: None,
            }
        });

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
            timestamp: actual_timestamp,
            vcp_number,
            available_products,
            product_elevations,
            status,
        }
    }
}

/// Inspect all sweeps in a scan to discover which products exist and at which
/// elevation angles. Rounds angles to 0.1° to collapse SAILS/MRLE duplicates.
/// Level III products are included with empty elevation lists.
fn discover_product_elevations(scan: &Scan) -> HashMap<RadarProduct, Vec<f32>> {
    let mut product_elevations: HashMap<RadarProduct, Vec<f32>> = HashMap::new();

    for (i, sweep) in scan.sweeps().iter().enumerate() {
        if let Some(first_radial) = sweep.radials().first() {
            let raw_angle = first_radial.elevation_angle_degrees();
            // Round to 1 decimal place so SAILS/MRLE repeat scans and
            // split-cuts at the same nominal angle collapse to one entry.
            let elev_angle = (raw_angle * 10.0).round() / 10.0;

            let mut products_found: Vec<&str> = Vec::new();
            for product in RadarProduct::all() {
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
                i, raw_angle, elev_angle, sweep.radials().len(),
                products_found.join(", ")
            );
        } else {
            log::warn!("  Sweep {:2}: no radials!", i);
        }
    }

    // Sort and deduplicate elevation angles for each product
    for angles in product_elevations.values_mut() {
        angles.sort_by(|a, b| a.partial_cmp(b).unwrap());
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

    // Include Level III products with empty elevation lists
    for l3_product in RadarProduct::all().iter().filter(|p| p.is_level3()) {
        product_elevations
            .entry(*l3_product)
            .or_insert_with(Vec::new);
    }

    product_elevations
}

/// Radar product types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    VerticallyIntegratedLiquid,
    HydrometeorClassification,
    PrecipitationRate,
    NormalizedRotation,
}

impl RadarProduct {
    pub fn code(&self) -> &'static str {
        match self {
            RadarProduct::Reflectivity => "ref",
            RadarProduct::Velocity => "vel",
            RadarProduct::SpectrumWidth => "sw",
            RadarProduct::DifferentialPhase => "phi",
            RadarProduct::CorrelationCoefficient => "rho",
            RadarProduct::DifferentialReflectivity => "zdr",
            RadarProduct::StormRelativeVelocity => "srv",
            RadarProduct::SpecificDifferentialPhase => "kdp",
            RadarProduct::EchoTops => "eet",
            RadarProduct::VerticallyIntegratedLiquid => "vil",
            RadarProduct::HydrometeorClassification => "hhc",
            RadarProduct::PrecipitationRate => "dpr",
            RadarProduct::NormalizedRotation => "nrot",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            RadarProduct::Reflectivity => "Reflectivity",
            RadarProduct::Velocity => "Velocity",
            RadarProduct::SpectrumWidth => "Spectrum Width",
            RadarProduct::DifferentialPhase => "Differential Phase",
            RadarProduct::CorrelationCoefficient => "Correlation Coefficient",
            RadarProduct::DifferentialReflectivity => "Differential Reflectivity",
            RadarProduct::StormRelativeVelocity => "Storm-Relative Velocity",
            RadarProduct::SpecificDifferentialPhase => "Specific Differential Phase",
            RadarProduct::EchoTops => "Echo Tops",
            RadarProduct::VerticallyIntegratedLiquid => "Vertically Integrated Liquid",
            RadarProduct::HydrometeorClassification => "Hydrometeor Classification",
            RadarProduct::PrecipitationRate => "Precipitation Rate",
            RadarProduct::NormalizedRotation => "Normalized Rotation",
        }
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
            RadarProduct::VerticallyIntegratedLiquid,
            RadarProduct::HydrometeorClassification,
            RadarProduct::PrecipitationRate,
            RadarProduct::NormalizedRotation,
        ]
    }

    /// Canonical sort key for ordering products in the UI.
    pub fn sort_order(&self) -> u8 {
        match self {
            RadarProduct::Reflectivity => 0,
            RadarProduct::Velocity => 1,
            RadarProduct::SpectrumWidth => 2,
            RadarProduct::DifferentialReflectivity => 3,
            RadarProduct::CorrelationCoefficient => 4,
            RadarProduct::DifferentialPhase => 5,
            RadarProduct::NormalizedRotation => 6,
            RadarProduct::StormRelativeVelocity => 7,
            RadarProduct::SpecificDifferentialPhase => 8,
            RadarProduct::EchoTops => 9,
            RadarProduct::VerticallyIntegratedLiquid => 10,
            RadarProduct::HydrometeorClassification => 11,
            RadarProduct::PrecipitationRate => 12,
        }
    }

    /// Whether this product comes from Level III data (as opposed to Level II base moments).
    pub fn is_level3(&self) -> bool {
        matches!(
            self,
            RadarProduct::StormRelativeVelocity
            | RadarProduct::SpecificDifferentialPhase
            | RadarProduct::EchoTops
            | RadarProduct::VerticallyIntegratedLiquid
            | RadarProduct::HydrometeorClassification
            | RadarProduct::PrecipitationRate
        )
    }

    /// The TGFTP directory names for all available tilts of this product.
    /// Used to fetch from `https://tgftp.nws.noaa.gov/SL.us008001/DF.of/DC.radar/DS.{dir}/SI.{site}/sn.last`.
    /// Returns `None` for Level II products.
    pub fn tgftp_dirs(&self) -> Option<&'static [&'static str]> {
        match self {
            RadarProduct::StormRelativeVelocity => Some(&["56rm0", "56rm1", "56rm2", "56rm3"]),
            RadarProduct::SpecificDifferentialPhase => Some(&["163k0"]),
            RadarProduct::EchoTops => Some(&["135et"]),
            RadarProduct::VerticallyIntegratedLiquid => Some(&["134il"]),
            RadarProduct::HydrometeorClassification => Some(&["177hh"]),
            RadarProduct::PrecipitationRate => Some(&["176pr"]),
            _ => None,
        }
    }

    /// Get the moment data for this product from a radial.
    /// Centralizes the product → accessor mapping in one place.
    pub fn get_moment<'a>(&self, radial: &'a Radial) -> Option<&'a nexrad_model::data::MomentData> {
        match self {
            RadarProduct::Reflectivity => radial.reflectivity(),
            RadarProduct::Velocity => radial.velocity(),
            RadarProduct::SpectrumWidth => radial.spectrum_width(),
            RadarProduct::DifferentialReflectivity => radial.differential_reflectivity(),
            RadarProduct::CorrelationCoefficient => radial.correlation_coefficient(),
            RadarProduct::DifferentialPhase => radial.differential_phase(),
            // NROT is derived from velocity data
            RadarProduct::NormalizedRotation => radial.velocity(),
            // Level III products don't come from Level II radials
            RadarProduct::StormRelativeVelocity
            | RadarProduct::SpecificDifferentialPhase
            | RadarProduct::EchoTops
            | RadarProduct::VerticallyIntegratedLiquid
            | RadarProduct::HydrometeorClassification
            | RadarProduct::PrecipitationRate => None,
        }
    }

    /// Format a radar product value for display (e.g. in a hover tooltip).
    pub fn format_value(&self, value: f32) -> String {
        match self {
            RadarProduct::Reflectivity => format!(" | Reflectivity: {:.1} dBZ", value),
            RadarProduct::Velocity | RadarProduct::StormRelativeVelocity => {
                let mph = value * MS_TO_MPH;
                format!(" | {}: {:.1} mph", self.name(), mph)
            }
            RadarProduct::SpectrumWidth => {
                let mph = value * MS_TO_MPH;
                format!(" | Spectrum Width: {:.1} mph", mph)
            }
            RadarProduct::DifferentialReflectivity => format!(" | Diff. Reflectivity: {:.2} dB", value),
            RadarProduct::CorrelationCoefficient => format!(" | Corr. Coefficient: {:.4}", value),
            RadarProduct::DifferentialPhase => format!(" | Diff. Phase: {:.1}°", value),
            RadarProduct::SpecificDifferentialPhase => format!(" | KDP: {:.2} °/km", value),
            RadarProduct::EchoTops => format!(" | Echo Tops: {:.1} kft", value),
            RadarProduct::VerticallyIntegratedLiquid => format!(" | VIL: {:.1} kg/m²", value),
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
                format!(" | HHC: {class}")
            }
            RadarProduct::PrecipitationRate => format!(" | Precip Rate: {:.2} in/hr", value),
            RadarProduct::NormalizedRotation => format!(" | NROT: {:.2}", value),
        }
    }
}
