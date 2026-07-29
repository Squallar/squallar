use crate::sites::RadarSite;
use crate::sites::get_radar_site;
use chrono::NaiveDateTime;
use nexrad_model::data::Radial;
use nexrad_model::data::Scan;
use rustdar_units::UserPreferences;
use std::collections::HashMap;
use std::f64::consts::PI;

/// Side length, in pixels, of the square radar image every render produces.
/// An RGBA texture is `IMAGE_SIZE² × 4` bytes; a static pane render keeps an
/// `f32` value grid alongside it, doubling that.
///
/// wasm32 halves the side: WebGL2 only guarantees
/// `max_texture_dimension_2d == 2048`, so a 2048² frame sits exactly on the
/// limit with nothing spare for the overlay textures beside it.
#[cfg(target_arch = "wasm32")]
pub const IMAGE_SIZE: usize = 1024;
#[cfg(not(target_arch = "wasm32"))]
pub const IMAGE_SIZE: usize = 2048;

pub const MAX_RANGE_KM: f64 = 230.0; // NEXRAD max range ~230km
pub const PIXELS_PER_KM: f64 = IMAGE_SIZE as f64 / (2.0 * MAX_RANGE_KM);
/// Mean radius of Earth in kilometers.
pub const EARTH_RADIUS_KM: f64 = 6371.0;
/// m/s to mph conversion factor.
pub const MS_TO_MPH: f32 = 2.23694;

#[inline]
pub(crate) fn lat_rad_to_mercator_y(lat_rad: f64) -> f64 {
    (PI / 4.0 + lat_rad / 2.0).tan().ln()
}

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
    /// Extent is `MAX_RANGE_KM` in every direction from the site.
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
}

#[derive(Debug, Clone)]
pub struct ScanInfo {
    pub site: RadarSite,
    /// From the first radial's collection timestamp, not the request.
    pub timestamp: NaiveDateTime,
    /// Volume Coverage Pattern number (e.g. 212, 215, 35)
    pub vcp_number: u16,
    pub available_products: Vec<RadarProduct>,
    /// Elevation angles per product, sorted ascending.
    pub product_elevations: HashMap<RadarProduct, Vec<f32>>,
    pub status: String,
}

impl ScanInfo {
    /// Level III products are listed with empty elevation vectors, filled in
    /// later as L3 data arrives.
    pub fn from_scan(data: &Scan, site: &str, requested_timestamp: NaiveDateTime) -> Self {
        let vcp_number = data.coverage_pattern_number().number();

        let product_elevations = discover_product_elevations(data);

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

        let radar_site = get_radar_site(site).cloned().unwrap_or_else(|| {
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

/// Rounds elevation angles to 0.1° so SAILS/MRLE repeat scans and split cuts
/// at the same nominal angle collapse to one entry.
fn discover_product_elevations(scan: &Scan) -> HashMap<RadarProduct, Vec<f32>> {
    let mut product_elevations: HashMap<RadarProduct, Vec<f32>> = HashMap::new();

    for (i, sweep) in scan.sweeps().iter().enumerate() {
        if let Some(first_radial) = sweep.radials().first() {
            let raw_angle = first_radial.elevation_angle_degrees();
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

    for l3_product in RadarProduct::all().iter().filter(|p| p.is_level3()) {
        product_elevations.entry(*l3_product).or_default();
    }

    product_elevations
}

/// A Level II moment field on a [`Radial`], named rather than read.
///
/// Several products share one: NROT is derived from velocity, and interpolated
/// echo tops from reflectivity. Naming the field — instead of only being able
/// to fetch it — is what lets a moment be put *back* onto a radial, which
/// [`crate::render_input`] does when it rebuilds a scan from a payload.
///
/// Deliberately a smaller set than [`RadarProduct`]: the Level III products
/// have no Level II field at all, which is what
/// [`RadarProduct::moment_slot`]'s `None` means.
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
            RadarProduct::EchoTopsInterpolated => "eti",
            RadarProduct::VerticallyIntegratedLiquid => "vil",
            RadarProduct::VilDensity => "vild",
            RadarProduct::ProbabilityOfSevereHail => "posh",
            RadarProduct::MaxExpectedHailSize => "mehs",
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
            RadarProduct::EchoTopsInterpolated => "Echo Tops (Interp)",
            RadarProduct::VerticallyIntegratedLiquid => "Vertically Integrated Liquid",
            RadarProduct::VilDensity => "VIL Density",
            RadarProduct::ProbabilityOfSevereHail => "Prob. of Severe Hail",
            RadarProduct::MaxExpectedHailSize => "Max Expected Hail Size",
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
            RadarProduct::EchoTopsInterpolated => 10,
            RadarProduct::VerticallyIntegratedLiquid => 11,
            RadarProduct::VilDensity => 12,
            RadarProduct::ProbabilityOfSevereHail => 13,
            RadarProduct::MaxExpectedHailSize => 14,
            RadarProduct::HydrometeorClassification => 15,
            RadarProduct::PrecipitationRate => 16,
        }
    }

    pub fn is_level3(&self) -> bool {
        matches!(
            self,
            RadarProduct::SpecificDifferentialPhase
                | RadarProduct::EchoTops
                | RadarProduct::VerticallyIntegratedLiquid
                | RadarProduct::PrecipitationRate
        )
    }

    /// The AWIPS product IDs to fetch for this product, one per tilt. These key
    /// the `unidata-nexrad-level3` bucket (`TLX_N0S_2026_07_25_...`). `None`
    /// for Level II products.
    ///
    /// Storm-relative velocity is deliberately absent: it once fetched five
    /// objects here — `N0S` for the vector in its PDB and `N0G`/`N1G`/
    /// `N2U`/`N3U` as dealiased tilts — and is now derived entirely from the
    /// Level II volume already in hand, dealiased locally with a Bunkers
    /// right-mover default vector. See [`crate::srv`].
    pub fn level3_products(&self) -> Option<&'static [&'static str]> {
        match self {
            RadarProduct::SpecificDifferentialPhase => Some(&["N0K"]),
            RadarProduct::EchoTops => Some(&["EET"]),
            RadarProduct::VerticallyIntegratedLiquid => Some(&["DVL"]),
            RadarProduct::PrecipitationRate => Some(&["DPR"]),
            _ => None,
        }
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
        if !self.is_level3() {
            return None;
        }
        Some(match self {
            RadarProduct::PrecipitationRate => crate::level3::VolumePick::Latest,
            _ => crate::level3::VolumePick::NEAREST,
        })
    }

    /// A stable identifier for this product on a wire.
    ///
    /// Deliberately not the enum's declaration order and not the serde
    /// representation: reordering or renaming the variants must not silently
    /// change what an already-encoded message means. Both message formats that
    /// cross the browser's worker boundary — [`crate::render_input`]'s payload
    /// and `rustdar_frontend::offload`'s job framing — read this one table.
    ///
    /// The match is exhaustive, so a new variant fails to compile until it is
    /// given a code.
    pub fn wire_code(&self) -> u16 {
        match self {
            RadarProduct::Reflectivity => 1,
            RadarProduct::Velocity => 2,
            RadarProduct::SpectrumWidth => 3,
            RadarProduct::DifferentialPhase => 4,
            RadarProduct::CorrelationCoefficient => 5,
            RadarProduct::DifferentialReflectivity => 6,
            RadarProduct::StormRelativeVelocity => 7,
            RadarProduct::SpecificDifferentialPhase => 8,
            RadarProduct::EchoTops => 9,
            RadarProduct::EchoTopsInterpolated => 10,
            RadarProduct::VerticallyIntegratedLiquid => 11,
            RadarProduct::HydrometeorClassification => 12,
            RadarProduct::PrecipitationRate => 13,
            RadarProduct::NormalizedRotation => 14,
            RadarProduct::VilDensity => 15,
            RadarProduct::ProbabilityOfSevereHail => 16,
            RadarProduct::MaxExpectedHailSize => 17,
        }
    }

    /// The inverse of [`wire_code`](Self::wire_code). `None` for a code this
    /// build does not know, which is a message from another build rather than a
    /// bug to panic on.
    pub fn from_wire_code(code: u16) -> Option<Self> {
        let product = match code {
            1 => RadarProduct::Reflectivity,
            2 => RadarProduct::Velocity,
            3 => RadarProduct::SpectrumWidth,
            4 => RadarProduct::DifferentialPhase,
            5 => RadarProduct::CorrelationCoefficient,
            6 => RadarProduct::DifferentialReflectivity,
            7 => RadarProduct::StormRelativeVelocity,
            8 => RadarProduct::SpecificDifferentialPhase,
            9 => RadarProduct::EchoTops,
            10 => RadarProduct::EchoTopsInterpolated,
            11 => RadarProduct::VerticallyIntegratedLiquid,
            12 => RadarProduct::HydrometeorClassification,
            13 => RadarProduct::PrecipitationRate,
            14 => RadarProduct::NormalizedRotation,
            15 => RadarProduct::VilDensity,
            16 => RadarProduct::ProbabilityOfSevereHail,
            17 => RadarProduct::MaxExpectedHailSize,
            _ => return None,
        };
        debug_assert_eq!(product.wire_code(), code);
        Some(product)
    }

    /// Which of a radial's moment fields this product reads.
    ///
    /// The single product → moment table. [`get_moment`](Self::get_moment)
    /// reads a radial *through* it rather than repeating it, so a consumer that
    /// needs to name the field — [`crate::render_input`], which has to place a
    /// moment back on a reconstructed radial — cannot come to disagree with the
    /// consumer that reads it.
    pub fn moment_slot(&self) -> Option<MomentSlot> {
        match self {
            RadarProduct::Reflectivity => Some(MomentSlot::Reflectivity),
            RadarProduct::Velocity => Some(MomentSlot::Velocity),
            RadarProduct::SpectrumWidth => Some(MomentSlot::SpectrumWidth),
            RadarProduct::DifferentialReflectivity => Some(MomentSlot::DifferentialReflectivity),
            RadarProduct::CorrelationCoefficient => Some(MomentSlot::CorrelationCoefficient),
            RadarProduct::DifferentialPhase => Some(MomentSlot::DifferentialPhase),
            // NROT is derived from velocity
            RadarProduct::NormalizedRotation => Some(MomentSlot::Velocity),
            // Storm-relative velocity is derived from velocity too — every
            // velocity tilt lists, an upgrade over the four fixed Level III
            // tilts the product used to fetch. See `crate::srv`.
            RadarProduct::StormRelativeVelocity => Some(MomentSlot::Velocity),
            // Interpolated echo tops integrate the whole reflectivity volume;
            // tying availability to the reflectivity moment lists it alongside
            // the reflectivity tilts (the rendered field is tilt-independent).
            RadarProduct::EchoTopsInterpolated => Some(MomentSlot::Reflectivity),
            // VIL density integrates the whole reflectivity volume twice over
            // (local VIL divided by local echo tops), so it lists the same way.
            RadarProduct::VilDensity => Some(MomentSlot::Reflectivity),
            // The hail pair integrates the whole reflectivity volume too
            // (`crate::hail`); the environmental heights it also needs ride
            // the render parameters, not a moment.
            RadarProduct::ProbabilityOfSevereHail | RadarProduct::MaxExpectedHailSize => {
                Some(MomentSlot::Reflectivity)
            }
            // The hybrid hydrometeor classification composites every dual-pol
            // tilt of the volume (crate::hhc); listing on reflectivity puts
            // the tilt-independent volume product alongside the reflectivity
            // tilts, the same convention as ETI and VIL density. The render
            // payload carries the rest of the moments (crate::render_input's
            // extras).
            RadarProduct::HydrometeorClassification => Some(MomentSlot::Reflectivity),
            // Level III products. No Level II moment stands behind them.
            RadarProduct::SpecificDifferentialPhase
            | RadarProduct::EchoTops
            | RadarProduct::VerticallyIntegratedLiquid
            | RadarProduct::PrecipitationRate => None,
        }
    }

    /// The moment data for this product on a radial.
    pub fn get_moment<'a>(&self, radial: &'a Radial) -> Option<&'a nexrad_model::data::MomentData> {
        self.moment_slot()?.read(radial)
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
            RadarProduct::MaxExpectedHailSize => format!("MEHS: {:.2} in", value),
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
        match self {
            RadarProduct::Reflectivity => "dBZ",
            RadarProduct::Velocity | RadarProduct::StormRelativeVelocity => prefs.speed.suffix(),
            RadarProduct::SpectrumWidth => prefs.speed.suffix(),
            RadarProduct::DifferentialReflectivity => "dB",
            RadarProduct::CorrelationCoefficient => "CC",
            RadarProduct::DifferentialPhase => "\u{00b0}",
            RadarProduct::SpecificDifferentialPhase => "\u{00b0}/km",
            RadarProduct::EchoTops | RadarProduct::EchoTopsInterpolated => {
                prefs.height.kilo_suffix()
            }
            RadarProduct::VerticallyIntegratedLiquid => "kg/m\u{00b2}",
            RadarProduct::VilDensity => "g/m\u{00b3}",
            RadarProduct::ProbabilityOfSevereHail => "%",
            RadarProduct::MaxExpectedHailSize => "in",
            RadarProduct::HydrometeorClassification => "HHC",
            RadarProduct::PrecipitationRate => prefs.precip_rate.suffix(),
            RadarProduct::NormalizedRotation => "NROT",
        }
    }
}
