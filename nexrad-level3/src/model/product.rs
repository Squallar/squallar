//! Level III product type enumeration.

/// Known Level III product types with their ICD message codes and S3 prefix codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Level3Product {
    /// Storm-Relative Velocity, tilt 1 (0.5°). ICD code 99.
    StormRelativeVelocity,
    /// Specific Differential Phase (KDP), tilt 1 (0.5°). ICD code 159.
    SpecificDifferentialPhase,
    /// Enhanced Echo Tops. ICD code 135. (Phase 2)
    EnhancedEchoTops,
    /// Digital Vertically Integrated Liquid. ICD code 134. (Phase 2)
    DigitalVil,
}

impl Level3Product {
    /// The ICD message code for this product.
    pub fn message_code(&self) -> i16 {
        match self {
            Level3Product::StormRelativeVelocity => 99,
            Level3Product::SpecificDifferentialPhase => 159,
            Level3Product::EnhancedEchoTops => 135,
            Level3Product::DigitalVil => 134,
        }
    }

    /// The 3-letter code used as the S3 key prefix component.
    pub fn s3_code(&self) -> &'static str {
        match self {
            Level3Product::StormRelativeVelocity => "N0S",
            Level3Product::SpecificDifferentialPhase => "N0K",
            Level3Product::EnhancedEchoTops => "EET",
            Level3Product::DigitalVil => "DVL",
        }
    }

    /// Human-readable product name.
    pub fn name(&self) -> &'static str {
        match self {
            Level3Product::StormRelativeVelocity => "Storm-Relative Velocity",
            Level3Product::SpecificDifferentialPhase => "Specific Differential Phase",
            Level3Product::EnhancedEchoTops => "Enhanced Echo Tops",
            Level3Product::DigitalVil => "Digital VIL",
        }
    }

    /// Look up a product by its ICD message code.
    pub fn from_message_code(code: i16) -> Option<Self> {
        match code {
            99 => Some(Level3Product::StormRelativeVelocity),
            159 => Some(Level3Product::SpecificDifferentialPhase),
            135 => Some(Level3Product::EnhancedEchoTops),
            134 => Some(Level3Product::DigitalVil),
            _ => None,
        }
    }

    /// The nominal elevation angle for the lowest-tilt variant of this product.
    pub fn nominal_elevation(&self) -> f32 {
        match self {
            Level3Product::StormRelativeVelocity => 0.5,
            Level3Product::SpecificDifferentialPhase => 0.5,
            Level3Product::EnhancedEchoTops => 0.0, // composite product
            Level3Product::DigitalVil => 0.0,        // composite product
        }
    }
}
