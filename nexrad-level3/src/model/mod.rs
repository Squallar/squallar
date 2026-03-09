//! Data model types for NEXRAD Level III products.

mod header;
mod product;
mod radial;
mod raster;
mod symbology;

pub use header::*;
pub use product::*;
pub use radial::*;
pub use raster::*;
pub use symbology::*;

/// A fully decoded Level III product message.
#[derive(Debug, Clone)]
pub struct Level3Message {
    /// The 18-byte message header.
    pub header: MessageHeader,
    /// The 102-byte Product Description Block.
    pub pdb: ProductDescriptionBlock,
    /// Decoded symbology data (display layers).
    pub symbology: Option<SymbologyBlock>,
}
