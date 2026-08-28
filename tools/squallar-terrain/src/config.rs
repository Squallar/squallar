//! The pins, the schedule and the layout.

use std::path::PathBuf;

use crate::Res;
use crate::tiles::Pin;

// ---------------------------------------------------------------------------
// The DEM pin.
//
// `copernicus-dem-30m` is NOT a versioned bucket: every object answers
// `x-amz-version-id: null`, so there is no S3 version id to pin to and no
// per-release prefix to select. The AWS Open Data registry entry
// (awslabs/open-data-registry, datasets/copernicus-dem.yaml) states the bucket
// "comes from Copernicus DEM 2021 release", and its UpdateFrequency reads
// "None, except GLO-30 Public can be updated if the public tile list changes."
//
// So the elevation values are fixed and the only thing that can move underneath
// a rerun is the SET OF TILES, as countries release previously withheld ones.
// That makes `tileList.txt` the pin, and it is pinned by content.
// ---------------------------------------------------------------------------
pub const DEM_BUCKET_DEFAULT: &str = "copernicus-dem-30m";
pub const DEM_BUCKET_REGION: &str = "eu-central-1";

/// Observed 2026-08-27. A mismatch is the signal that the public tile set
/// changed and the archives are stale.
pub const TILELIST: Pin = Pin {
    md5: "637fe75ddf7615ba853dd83caf05cd82",
    count: 26_450,
    bytes: 1_110_900,
    release: "COP-DEM_GLO-30 Public, 2021 release",
};

/// The MODIFIED-work form of the notice, which is the one that applies: tiling
/// and Terrain-RGB encoding make these archives a derivative. The
/// unmodified-redistribution wording is a different string and would be the
/// wrong claim.
pub const ATTRIBUTION: &str = "produced using Copernicus WorldDEM-30 © DLR e.V. 2010-2014 and © Airbus Defence and Space GmbH 2014-2018 provided under COPERNICUS by the European Union and ESA; all rights reserved";

/// One contour band: `minzoom`, `maxzoom`, interval in metres.
#[derive(Clone, Copy, Debug)]
pub struct Band {
    pub lo: u8,
    pub hi: u8,
    pub interval: u32,
}

/// HARMONIC CONSTRAINT: each interval must divide the next coarser one exactly.
/// [`verify_schedule`] enforces it on every build, not only when this is edited.
pub const BANDS: [Band; 3] = [
    Band {
        lo: 10,
        hi: 10,
        interval: 1000,
    },
    Band {
        lo: 11,
        hi: 12,
        interval: 200,
    },
    Band {
        lo: 13,
        hi: 14,
        interval: 100,
    },
];

pub const CONTOUR_LAYER: &str = "contour";
pub const CONTOUR_ATTR: &str = "elev";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encoding {
    /// 1-band grey, `gdaldem hillshade`. The default.
    Hillshade,
    /// 3-band Mapbox Terrain-RGB v1, packed by [`crate::trgb`].
    TerrainRgb,
}

impl Encoding {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hillshade => "hillshade",
            Self::TerrainRgb => "terrain-rgb",
        }
    }
}

pub struct Config {
    pub bucket: String,
    pub work: PathBuf,
    pub out: PathBuf,
    pub tmp: PathBuf,
    pub jobs: usize,
    pub encoding: Encoding,
    pub raster_minzoom: u8,
    pub raster_maxzoom: u8,
    pub tile_format: String,
    /// Zooms at or below this are built from ONE global mercator DEM; zooms
    /// above it are built per super-cell. At z8 the global grid is
    /// 65536x65536, which is 8.6 GB as Float32 and the largest single raster
    /// this build ever materialises.
    pub raster_global_maxzoom: u8,
    /// Chunk edge in whole degrees for the contour pass. Peak disk scales with
    /// the square of this.
    pub chunk_deg: i32,
    /// Tiles per side of a raster super-cell. 64 -> 16384x16384 px -> 1.07 GB
    /// as Float32, and identically so everywhere on the globe.
    pub supercell: u32,
    /// Substring filters, for re-running one region after a failure.
    pub only_chunk: Option<String>,
    pub only_supercell: Option<String>,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn env_parse<T: std::str::FromStr>(name: &str, default: T) -> Res<T> {
    match env(name) {
        None => Ok(default),
        Some(v) => v
            .parse()
            .map_err(|_| format!("{name}={v} is not a valid value").into()),
    }
}

impl Config {
    pub fn from_env() -> Res<Self> {
        let work = PathBuf::from(env("WORK").unwrap_or_else(|| "/mnt/terrain-work".into()));
        let out = env("OUT").map_or_else(|| work.join("out"), PathBuf::from);
        let tmp = env("TMP").map_or_else(|| work.join("tmp"), PathBuf::from);
        let encoding = match env("RASTER_ENCODING").as_deref() {
            None | Some("hillshade") => Encoding::Hillshade,
            Some("terrain-rgb") => Encoding::TerrainRgb,
            Some(other) => {
                return Err(
                    format!("unknown RASTER_ENCODING={other} (terrain-rgb|hillshade)").into(),
                );
            }
        };
        let cfg = Self {
            bucket: env("DEM_BUCKET").unwrap_or_else(|| DEM_BUCKET_DEFAULT.into()),
            work,
            out,
            tmp,
            jobs: env_parse("JOBS", std::thread::available_parallelism()?.get())?,
            encoding,
            raster_minzoom: env_parse("RASTER_MINZOOM", 0u8)?,
            raster_maxzoom: env_parse("RASTER_MAXZOOM", 12u8)?,
            tile_format: env("RASTER_TILE_FORMAT").unwrap_or_else(|| "PNG".into()),
            raster_global_maxzoom: env_parse("RASTER_GLOBAL_MAXZOOM", 8u8)?,
            chunk_deg: env_parse("CHUNK_DEG", 5i32)?,
            supercell: env_parse("SUPERCELL", 64u32)?,
            only_chunk: env("ONLY_CHUNK"),
            only_supercell: env("ONLY_SUPERCELL"),
        };
        cfg.verify()?;
        Ok(cfg)
    }

    fn verify(&self) -> Res<()> {
        verify_schedule(&BANDS)?;
        if self.chunk_deg < 1 {
            return Err("CHUNK_DEG must be at least 1".into());
        }
        if self.supercell < 1 {
            return Err("SUPERCELL must be at least 1".into());
        }
        if self.raster_minzoom > self.raster_maxzoom {
            return Err("RASTER_MINZOOM is above RASTER_MAXZOOM".into());
        }
        // terrain-rgb MUST be stored losslessly. A single-count error in R is
        // 6553.6 m, so lossy WebP or JPEG on a terrain-rgb archive is not
        // "slightly soft", it is destroyed. A hillshade is an ordinary grey
        // image with no such constraint, which is most of why it is smaller.
        if self.encoding == Encoding::TerrainRgb && self.tile_format != "PNG" {
            return Err(format!(
                "terrain-rgb must be stored losslessly; RASTER_TILE_FORMAT={} would \
                 quantise the packed elevation bytes. One count of error in the R \
                 channel is 6553.6 m.",
                self.tile_format
            )
            .into());
        }
        Ok(())
    }

    pub fn contours_pmtiles(&self) -> PathBuf {
        self.out.join("squallar-contours.pmtiles")
    }

    pub fn raster_pmtiles(&self) -> PathBuf {
        self.out.join(format!(
            "squallar-terrain-{}.pmtiles",
            self.encoding.as_str()
        ))
    }

    pub fn tilelist_path(&self) -> PathBuf {
        self.work.join("tileList.txt")
    }

    /// Absolute HTTPS URL of one tile's DEM GeoTIFF.
    pub fn tile_url(&self, name: &str) -> String {
        format!("https://{}.s3.amazonaws.com/{name}/{name}.tif", self.bucket)
    }

    /// The same object as a GDAL virtual path.
    pub fn tile_vsis3(&self, name: &str) -> String {
        format!("/vsis3/{}/{name}/{name}.tif", self.bucket)
    }
}

/// Reject a schedule whose intervals are not harmonic.
///
/// If the constraint is violated, contour lines VANISH as you zoom in — a 500 m
/// line has no counterpart in a 200 m band, so crossing that zoom makes an
/// existing line disappear, which reads as a rendering bug rather than a data
/// choice.
pub fn verify_schedule(bands: &[Band]) -> Res<()> {
    let mut prev: Option<u32> = None;
    for b in bands {
        if b.lo > b.hi {
            return Err(format!("band z{}-z{}: minzoom above maxzoom", b.lo, b.hi).into());
        }
        if b.interval == 0 {
            return Err(format!("band z{}-z{}: zero interval", b.lo, b.hi).into());
        }
        if let Some(p) = prev {
            if b.interval >= p {
                return Err(format!(
                    "band z{}-z{}: interval must decrease as zoom rises",
                    b.lo, b.hi
                )
                .into());
            }
            if p % b.interval != 0 {
                return Err(format!(
                    "NON-HARMONIC SCHEDULE: {} m does not divide {p} m exactly. Contours \
                     would VANISH when crossing into the {} m band, because lines at \
                     multiples of {p} m that are not multiples of {} m have no counterpart \
                     there. Pick an interval that divides {p} m.",
                    b.interval, b.interval, b.interval
                )
                .into());
            }
        }
        prev = Some(b.interval);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_schedule_is_harmonic() {
        verify_schedule(&BANDS).unwrap();
    }

    /// The guard has to reject, or it is not a guard. 1000/500/200 is the
    /// plausible-looking schedule that breaks the constraint: 500 divides 1000
    /// but 200 does not divide 500.
    #[test]
    fn a_non_harmonic_schedule_is_rejected() {
        let bad = [
            Band {
                lo: 10,
                hi: 10,
                interval: 1000,
            },
            Band {
                lo: 11,
                hi: 12,
                interval: 500,
            },
            Band {
                lo: 13,
                hi: 14,
                interval: 200,
            },
        ];
        let err = verify_schedule(&bad).unwrap_err().to_string();
        assert!(err.contains("NON-HARMONIC"), "{err}");
    }

    #[test]
    fn an_interval_that_grows_with_zoom_is_rejected() {
        let bad = [
            Band {
                lo: 10,
                hi: 10,
                interval: 100,
            },
            Band {
                lo: 11,
                hi: 12,
                interval: 200,
            },
        ];
        assert!(verify_schedule(&bad).is_err());
    }
}
