//! The pins, the schedule and the layout.

use std::path::PathBuf;

use crate::Res;
use crate::grid::LonLatBox;
use crate::md5;
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

/// The tile format each encoding gets when `RASTER_TILE_FORMAT` is unset.
///
/// Hillshade is an ordinary grey image and WebP is 3.6-4.9x smaller on it --
/// 59-101 GB globally against 289-366 GB. Terrain-RGB carries packed elevation
/// in the colour channels and must stay lossless.
pub fn default_tile_format(encoding: Encoding) -> &'static str {
    match encoding {
        Encoding::Hillshade => "WEBP",
        Encoding::TerrainRgb => "PNG",
    }
}

/// Why a lossy format is refused on terrain-RGB, as an error.
pub fn lossy_terrain_rgb_error(fmt: &str) -> Box<dyn std::error::Error + Send + Sync> {
    format!(
        "terrain-rgb must be stored losslessly; RASTER_TILE_FORMAT={fmt} would \
         quantise the packed elevation bytes. One count of error in the R \
         channel is 6553.6 m."
    )
    .into()
}

/// Why a bounding box is refused, as an error.
///
/// `RASTER_BBOX` exists because the only regional lever before it was
/// `ONLY_SUPERCELL`, a SUBSTRING match on a super-cell name. A region is a
/// two-dimensional block range — CONUS at z11 with `SUPERCELL=64` is columns
/// 5-10 by rows 10-13 — and no substring of `sc_z11_000320_000640` expresses
/// that. Silently building the globe is what the old lever did when asked for a
/// region, so a box this function rejects must not be silently ignored either.
pub fn bbox_error(raw: &str, why: &str) -> Box<dyn std::error::Error + Send + Sync> {
    format!(
        "RASTER_BBOX={raw}: {why}. Spell it west,south,east,north in degrees,          e.g. RASTER_BBOX=-125,24,-66,50 for CONUS."
    )
    .into()
}

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
    /// The region to build, `west,south,east,north` in degrees. `None` is the
    /// globe. A super-cell survives if its tile extent INTERSECTS this box, so
    /// the built region is the box rounded outward to whole super-cells.
    pub raster_bbox: Option<LonLatBox>,
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
        // Parsed above the literal, like `encoding`, so the failure carries
        // bbox_error's sentence rather than env_parse's flattened one.
        let raster_bbox = match env("RASTER_BBOX") {
            None => None,
            Some(raw) => Some(raw.parse::<LonLatBox>().map_err(|e| bbox_error(&raw, &e))?),
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
            // Default by ENCODING, not one global default. Hillshade is an
            // ordinary grey image and WebP is 3.6-4.9x smaller on it -- 59-101
            // GB globally against 289-366 GB, which is real egress and real
            // storage every month forever. Terrain-RGB must stay PNG; see
            // verify(), which refuses a lossy override rather than trusting
            // this default to be the only guard.
            tile_format: env("RASTER_TILE_FORMAT")
                .unwrap_or_else(|| default_tile_format(encoding).to_string()),
            raster_global_maxzoom: env_parse("RASTER_GLOBAL_MAXZOOM", 8u8)?,
            chunk_deg: env_parse("CHUNK_DEG", 5i32)?,
            supercell: env_parse("SUPERCELL", 64u32)?,
            raster_bbox,
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
            return Err(lossy_terrain_rgb_error(&self.tile_format));
        }
        if let Some(b) = self.raster_bbox {
            verify_bbox(b)?;
        }
        Ok(())
    }

    /// Everything about a run that changes WHICH TILES its super-cells produce.
    ///
    /// Deliberately NOT the zoom range. `raster-acc/z{z}.mbtiles` and the
    /// `sc_z{z}_…` `.done` markers already carry the zoom, so two runs that
    /// differ only in zoom range compose correctly and SHOULD share
    /// intermediates — that sharing is the resume feature. Everything listed
    /// here does not compose: a `.done` marker dropped by a hillshade run means
    /// nothing to a terrain-RGB run, and before this existed it silently meant
    /// "already built".
    pub fn raster_scope(&self) -> String {
        format!(
            "enc={} fmt={} gmax={} side={} bbox={} only={}",
            self.encoding.as_str(),
            self.tile_format,
            self.raster_global_maxzoom,
            self.supercell,
            self.raster_bbox
                .map_or_else(|| "global".to_string(), |b| b.to_string()),
            self.only_supercell.as_deref().unwrap_or("-"),
        )
    }

    /// [`Self::raster_scope`] as a short, filesystem-safe directory suffix.
    ///
    /// The encoding rides in front unhashed because these directories hold
    /// hundreds of gigabytes and an operator reading `df` deserves to know which
    /// build they belong to.
    pub fn raster_scope_tag(&self) -> String {
        let scope = self.raster_scope();
        format!(
            "{}-{}",
            self.encoding.as_str(),
            &md5::hex(scope.as_bytes())[..8]
        )
    }

    /// The full identity of the OUTPUT ARCHIVE, which does include the zoom
    /// range: two runs that differ only in zoom range share intermediates but
    /// produce different archives, and aim at the same filename.
    pub fn raster_archive_scope(&self) -> String {
        format!(
            "{} z{}-z{}",
            self.raster_scope(),
            self.raster_minzoom,
            self.raster_maxzoom
        )
    }

    /// Where [`Self::raster_archive_scope`] is recorded beside the archive.
    pub fn raster_scope_stamp(&self) -> PathBuf {
        let mut p = self.raster_pmtiles().into_os_string();
        p.push(".scope");
        PathBuf::from(p)
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

/// Reject a bounding box that is inverted, off the globe, or degenerate.
///
/// Inversion is the one that matters: `-66,50,-125,24` is CONUS with the pairs
/// swapped, and it names a box that no super-cell intersects. Left to run it
/// builds NOTHING and exits 0 on an empty archive.
pub fn verify_bbox(b: LonLatBox) -> Res<()> {
    let raw = b.to_string();
    if !(-180.0..=180.0).contains(&b.w) || !(-180.0..=180.0).contains(&b.e) {
        return Err(bbox_error(&raw, "longitudes must lie in -180..=180"));
    }
    if !(-90.0..=90.0).contains(&b.s) || !(-90.0..=90.0).contains(&b.n) {
        return Err(bbox_error(&raw, "latitudes must lie in -90..=90"));
    }
    if b.w >= b.e {
        return Err(bbox_error(
            &raw,
            "west is not west of east — the box is INVERTED or empty, and an \
             inverted box intersects no super-cell at all",
        ));
    }
    if b.s >= b.n {
        return Err(bbox_error(
            &raw,
            "south is not south of north — the box is INVERTED or empty, and an \
             inverted box intersects no super-cell at all",
        ));
    }
    Ok(())
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

    /// The tile-format default is worth about $20 of egress and $3.50/month of
    /// storage, so it is pinned rather than left to a reading of the code.
    /// Asserted on BOTH arms: a single-arm test would pass against the old
    /// unconditional "PNG" and prove nothing.
    #[test]
    fn hillshade_defaults_to_webp_and_terrain_rgb_does_not() {
        assert_eq!(default_tile_format(Encoding::Hillshade), "WEBP");
        assert_eq!(default_tile_format(Encoding::TerrainRgb), "PNG");
    }

    /// The default is not the only thing keeping terrain-RGB lossless; an
    /// explicit override has to be refused too. One count of error in R is
    /// 6553.6 m.
    #[test]
    fn a_lossy_override_on_terrain_rgb_is_refused() {
        let msg = format!("{}", lossy_terrain_rgb_error("WEBP"));
        assert!(
            msg.contains("losslessly") && msg.contains("6553.6"),
            "the refusal must say what breaks and by how much, got: {msg}"
        );
    }

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

    /// A `Config` that touches no environment. `from_env` reads process-global
    /// state, and two of these tests would race on it.
    fn base() -> Config {
        Config {
            bucket: DEM_BUCKET_DEFAULT.into(),
            work: PathBuf::from("/w"),
            out: PathBuf::from("/w/out"),
            tmp: PathBuf::from("/w/tmp"),
            jobs: 8,
            encoding: Encoding::Hillshade,
            raster_minzoom: 0,
            raster_maxzoom: 12,
            tile_format: "WEBP".into(),
            raster_global_maxzoom: 8,
            chunk_deg: 5,
            supercell: 64,
            raster_bbox: None,
            only_chunk: None,
            only_supercell: None,
        }
    }

    const CONUS: LonLatBox = LonLatBox {
        w: -125.0,
        s: 24.0,
        e: -66.0,
        n: 50.0,
    };

    /// The published object name, pinned. `squallar-egui/src/tiles.rs`,
    /// `squallar-web/sw.js`, `squallar-web/tests/sw_routing.test.mjs` and
    /// `squallar-egui/src/basemap_archive/block_cache/tests.rs` all carry this
    /// string literally. The zoom range and the region deliberately do NOT
    /// appear in it; `raster::guard_output_scope` is what keeps two scopes from
    /// sharing it silently.
    #[test]
    fn the_archive_filename_is_keyed_on_the_encoding_alone() {
        assert_eq!(
            base().raster_pmtiles(),
            PathBuf::from("/w/out/squallar-terrain-hillshade.pmtiles")
        );
        let rgb = Config {
            encoding: Encoding::TerrainRgb,
            tile_format: "PNG".into(),
            raster_minzoom: 11,
            raster_maxzoom: 12,
            raster_bbox: Some(CONUS),
            ..base()
        };
        assert_eq!(
            rgb.raster_pmtiles(),
            PathBuf::from("/w/out/squallar-terrain-terrain-rgb.pmtiles")
        );
        assert_eq!(
            rgb.raster_scope_stamp(),
            PathBuf::from("/w/out/squallar-terrain-terrain-rgb.pmtiles.scope")
        );
    }

    /// RESUME MUST STILL WORK WITHIN A SCOPE. `raster-acc/z{z}.mbtiles` and the
    /// `sc_z{z}_...` markers already carry the zoom, so a run that stops at z11
    /// and a run that continues from z11 have to land in the same directories
    /// or the whole resume feature is gone.
    #[test]
    fn the_intermediate_scope_ignores_the_zoom_range() {
        let a = Config {
            raster_minzoom: 0,
            raster_maxzoom: 11,
            ..base()
        };
        let b = Config {
            raster_minzoom: 11,
            raster_maxzoom: 12,
            ..base()
        };
        assert_eq!(a.raster_scope_tag(), b.raster_scope_tag());
        assert_eq!(a.raster_scope(), b.raster_scope());
    }

    /// ...but the ARCHIVE those two runs write is not the same archive, so the
    /// stamp beside it must tell them apart. Without this the guard would wave
    /// a z11-z12 build straight over a z0-z11 one.
    #[test]
    fn the_archive_scope_does_not_ignore_the_zoom_range() {
        let a = Config {
            raster_minzoom: 0,
            raster_maxzoom: 11,
            ..base()
        };
        let b = Config {
            raster_minzoom: 11,
            raster_maxzoom: 12,
            ..base()
        };
        assert_ne!(a.raster_archive_scope(), b.raster_archive_scope());
        assert!(a.raster_archive_scope().ends_with("z0-z11"));
    }

    /// Every knob that makes a `.done` marker from another run a LIE.
    ///
    /// The encoding row is the one that was live: a default hillshade
    /// `build raster` followed by `RASTER_ENCODING=terrain-rgb` under the same
    /// WORK found every marker already present, built nothing, and shipped the
    /// hillshade tiles labelled terrain-RGB.
    #[test]
    fn the_intermediate_scope_separates_every_knob_that_changes_the_tiles() {
        let b = base();
        for (what, other) in [
            (
                "encoding",
                Config {
                    encoding: Encoding::TerrainRgb,
                    tile_format: "PNG".into(),
                    ..base()
                },
            ),
            (
                "tile format",
                Config {
                    tile_format: "PNG".into(),
                    ..base()
                },
            ),
            (
                "global maxzoom",
                Config {
                    raster_global_maxzoom: 9,
                    ..base()
                },
            ),
            (
                "super-cell side",
                Config {
                    supercell: 32,
                    ..base()
                },
            ),
            (
                "bbox",
                Config {
                    raster_bbox: Some(CONUS),
                    ..base()
                },
            ),
            (
                "only_supercell",
                Config {
                    only_supercell: Some("sc_z11_000320".into()),
                    ..base()
                },
            ),
        ] {
            assert_ne!(
                b.raster_scope_tag(),
                other.raster_scope_tag(),
                "{what} must not share intermediates"
            );
            assert_ne!(b.raster_scope(), other.raster_scope(), "{what}");
        }
    }

    /// The tag goes in a directory name, so it has to be spellable as one.
    #[test]
    fn the_scope_tag_is_filesystem_safe() {
        let tag = Config {
            encoding: Encoding::TerrainRgb,
            tile_format: "PNG".into(),
            raster_bbox: Some(CONUS),
            ..base()
        }
        .raster_scope_tag();
        assert!(
            tag.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
            "{tag}"
        );
        assert!(tag.starts_with("terrain-rgb-"), "{tag}");
    }

    /// Four comma-separated degrees, and nothing else. The shapes here are the
    /// ones a human types: three fields, GDAL's `-te` spacing, a bare word.
    #[test]
    fn a_malformed_bbox_is_refused() {
        for bad in [
            "",
            "-125,24,-66",
            "-125,24,-66,50,0",
            "-125 24 -66 50",
            "-125,24,-66,north",
            "conus",
            "-125,24,-66,NaN",
        ] {
            assert!(
                bad.parse::<LonLatBox>().is_err(),
                "{bad:?} must be rejected"
            );
        }
        let b: LonLatBox = " -125 , 24 , -66 , 50 ".parse().unwrap();
        assert_eq!(b, CONUS);
        assert_eq!(b.to_string(), "-125,24,-66,50");
    }

    /// The guard has to reject, or it is not a guard. An inverted box is the
    /// dangerous one: it parses, it reads as four plausible numbers, and it
    /// intersects no super-cell anywhere -- a build that produces nothing.
    #[test]
    fn an_inverted_or_off_globe_bbox_is_refused() {
        verify_bbox(CONUS).unwrap();
        for (bad, want) in [
            (
                LonLatBox {
                    w: -66.0,
                    s: 24.0,
                    e: -125.0,
                    n: 50.0,
                },
                "INVERTED",
            ),
            (
                LonLatBox {
                    w: -125.0,
                    s: 50.0,
                    e: -66.0,
                    n: 24.0,
                },
                "INVERTED",
            ),
            (
                LonLatBox {
                    w: -125.0,
                    s: 24.0,
                    e: -125.0,
                    n: 50.0,
                },
                "INVERTED",
            ),
            (
                LonLatBox {
                    w: -125.0,
                    s: 24.0,
                    e: -66.0,
                    n: 95.0,
                },
                "-90..=90",
            ),
            (
                LonLatBox {
                    w: -195.0,
                    s: 24.0,
                    e: -66.0,
                    n: 50.0,
                },
                "-180..=180",
            ),
        ] {
            let err = verify_bbox(bad).unwrap_err().to_string();
            assert!(err.contains(want), "{bad:?}: {err}");
            assert!(err.contains("west,south,east,north"), "{bad:?}: {err}");
        }
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
