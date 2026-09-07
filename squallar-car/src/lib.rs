//! An Apple asset catalog (`Assets.car`) holding the app icon, written directly.
//!
//! App Store Connect reads the icon sizes it demands (its errors 90022, 90023
//! and 90713) out of the COMPILED catalog, and the compiler, `actool`, exists
//! only inside Xcode. This writes the same file on Linux.
//!
//! The format is undocumented. Every constant here was read off catalogs that
//! `actool` (Xcode 26.6, build 17F113) compiled from a one-image
//! `AppIcon.appiconset` for iphone+ipad at deployment target 15.0, and the
//! output of this module was put through Apple's own reader,
//! `assetutil --info`, which decodes it the way it decodes actool's file.
//! Comments quote offsets into that reference file -- "golden a", 244056
//! bytes -- and `block[n]` is the index into its block table.
//!
//! Two layers:
//!
//!   * The container is a BOMStore (the installer "bill of materials"
//!     format), big-endian: a 512-byte header, a block table, a short list of
//!     named blocks ("vars"), and one-page B-trees mapping keys to blocks.
//!   * Inside it, little-endian CoreUI records: a header, the key format, one
//!     facet (the icon set) and its renditions -- the image once per idiom,
//!     plus a "multisize" record per idiom listing the sizes the set holds.
//!
//! Where a value's meaning is not known, the comment says so and the value is
//! copied; nothing here is a guess dressed as a fact.

/// The identifier a catalog stores for a facet name.
///
/// CoreUI derives it from the name; a catalog whose facet entry and rendition
/// keys carry a different identifier for "AppIcon" than the one the reader
/// computes is a catalog with no app icon in it. The function was fitted, not
/// found in a document: fifty-six set names were compiled by actool, each
/// identifier read back by Apple's `assetutil`, and this reproduces every one
/// (`name_identifier_matches_every_compiled_catalog` below carries a slice
/// of that table). The sweep exposed a linear structure -- consecutive last
/// characters add a constant 35937 mod 2^16 -- and the "Icon"/"AAAA" residual
/// of exactly 5·33² identified a Horner-33 fold over the 16-bit words of a
/// 64-bit accumulator. The seed 100823 is a fitted constant with no known
/// derivation; the 32-bit variant fits nothing over all 2^32 seeds.
///
/// UNVERIFIED beyond the sweep: names longer than 16 characters, and non-ASCII
/// names.
pub const fn name_identifier(name: &str) -> u16 {
    let bytes = name.as_bytes();
    let mut v: u64 = 100_823;
    let mut i = 0;
    while i < bytes.len() {
        v = v.wrapping_mul(33).wrapping_add(bytes[i] as u64);
        i += 1;
    }
    let mut h: u16 = 0;
    let mut w = 0;
    while w < 4 {
        h = h
            .wrapping_mul(33)
            .wrapping_add(((v >> (16 * w)) & 0xffff) as u16);
        w += 1;
    }
    h
}

/// The app icon's facet name and its identifier. The identifier is computed,
/// and `name_identifier_matches_every_compiled_catalog` pins that it is the
/// 6849 the reference catalog carries (block[11] bytes 16..18, `c1 1a`).
pub const APP_ICON: (&str, u16) = ("AppIcon", name_identifier("AppIcon"));

/// Write the catalog for one square, opaque app icon.
///
/// `bgra` is `edge * edge * 4` bytes, one pixel as B, G, R, A, rows top-down --
/// the layout Apple's `ARGB` encoding means in memory, read off a compiled
/// catalog whose pixels encoded their own coordinates.
///
/// The catalog carries the phone and pad idioms, which is what App Store
/// Connect asks an iOS app for and what the reference catalog was compiled
/// with. Only a 1024-pixel edge has been checked against Apple's reader.
pub fn assets_car(name: &str, identifier: u16, edge: u32, bgra: &[u8], xcode: &Xcode) -> Vec<u8> {
    assert_eq!(
        bgra.len(),
        (edge * edge * 4) as usize,
        "pixel buffer is not {edge}x{edge} BGRA"
    );

    // Blocks are allocated in the order actool allocated them, so the layout
    // matches golden a block for block. Allocation and use are separate steps
    // because the trees are allocated before the entries they point at.
    let mut bom = Bom::new();
    let header = bom.add(car_header(4, &xcode.asset_storage_version())); // block[1]
    let renditions = bom.reserve_tree(); // block[2], block[3]
    let facets = bom.reserve_tree(); // block[4], block[5]
    let appearances = bom.reserve_tree(); // block[6], block[7]

    let appearance_key = bom.add(APPEARANCE_ANY.as_bytes().to_vec()); // block[8]
    let appearance_value = bom.add(vec![0, 0]); // block[9]: appearance 0, u16
    let facet_key = bom.add(name.as_bytes().to_vec()); // block[10]
    let facet_value = bom.add(facet_value(identifier)); // block[11]
    let key_format = bom.add(key_format()); // block[12]

    let image = image_rendition(edge, bgra);
    let multisize = multisize_rendition(name, edge);
    // The pairing (idiom, kind) below is the order actool allocated them in;
    // it is not sorted and there is no rule known for it, only the readout.
    let mut entries = Vec::new();
    for (idiom, kind) in [
        (Idiom::Pad, Rendition::Multisize),
        (Idiom::Phone, Rendition::Image),
        (Idiom::Phone, Rendition::Multisize),
        (Idiom::Pad, Rendition::Image),
    ] {
        let key = rendition_key(idiom, identifier, kind);
        let key_block = bom.add(key.clone()); // block[13], [15], [17], [19]
        let value = bom.add(match kind {
            Rendition::Image => image.clone(),
            Rendition::Multisize => multisize.clone(),
        }); // block[14], [16], [18], [20]
        entries.push(TreeEntry {
            key: TreeKey::Block {
                index: key_block,
                bytes: key,
            },
            value,
        });
    }

    let metadata = bom.add(extended_metadata()); // block[21]
    let bitmaps = bom.reserve_tree(); // block[22], block[23]
    let bitmap_value = bom.add(bitmap_key_value(&[Idiom::Phone, Idiom::Pad])); // block[24]

    bom.fill_tree(renditions, TREE_PAGE, entries);
    bom.fill_tree(
        facets,
        TREE_PAGE,
        vec![TreeEntry {
            key: TreeKey::Block {
                index: facet_key,
                bytes: name.as_bytes().to_vec(),
            },
            value: facet_value,
        }],
    );
    bom.fill_tree(
        appearances,
        TREE_PAGE,
        vec![TreeEntry {
            key: TreeKey::Block {
                index: appearance_key,
                bytes: APPEARANCE_ANY.as_bytes().to_vec(),
            },
            value: appearance_value,
        }],
    );
    bom.fill_tree(
        bitmaps,
        BITMAP_PAGE,
        vec![TreeEntry {
            key: TreeKey::Inline(u32::from(identifier)),
            value: bitmap_value,
        }],
    );

    // The var list in golden a's order (bytes 241856..241973).
    bom.var("CARHEADER", header);
    bom.var("RENDITIONS", renditions.entry);
    bom.var("FACETKEYS", facets.entry);
    bom.var("APPEARANCEKEYS", appearances.entry);
    bom.var("KEYFORMAT", key_format);
    bom.var("EXTENDED_METADATA", metadata);
    bom.var("BITMAPKEYS", bitmaps.entry);
    bom.finish()
}

// ---------------------------------------------------------------- BOMStore --

/// The header is 512 bytes with 32 in use (golden a: bytes 32..512 are zero).
const BOM_HEADER_LEN: usize = 512;
/// Every block, the var list and the block table start on a 16-byte boundary
/// (golden a: block[1] at 512, block[2] at 960 after a 436-byte block, and so
/// on; a block that ends on a boundary is followed with no gap, block[12] at
/// 13536+48 = block[13] at 13584).
const BOM_ALIGN: usize = 16;
/// The block table has 256 slots regardless of how many are used (golden a
/// and golden d, with 24 and 20 blocks, both declare 256 at their index
/// offset).
const BOM_TABLE_SLOTS: usize = 256;
/// Page size of the key-to-block trees (`00 00 10 00` in every tree entry
/// except BITMAPKEYS).
const TREE_PAGE: u32 = 4096;
/// Page size of the BITMAPKEYS tree (`00 00 04 00`, golden a block[22]).
const BITMAP_PAGE: u32 = 1024;

/// A BOMStore under construction: numbered blocks and the names some of them
/// go by. Block 0 is the null block every store starts with.
struct Bom {
    blocks: Vec<Option<Vec<u8>>>,
    vars: Vec<(&'static str, u32)>,
}

/// A tree's two blocks, allocated before the entries exist.
#[derive(Clone, Copy)]
struct TreeSlot {
    entry: u32,
    page: u32,
}

/// One tree entry. A key is usually a block of its own that the page also
/// carries a copy of; the BITMAPKEYS tree instead stores a 32-bit key in the
/// slot where the block index would be (golden a block[23] bytes 16..20 are
/// `00 00 1a c1`, the identifier, and no block 6849 exists).
struct TreeEntry {
    key: TreeKey,
    value: u32,
}

enum TreeKey {
    Block { index: u32, bytes: Vec<u8> },
    Inline(u32),
}

impl TreeKey {
    fn bytes(&self) -> Vec<u8> {
        match self {
            TreeKey::Block { bytes, .. } => bytes.clone(),
            TreeKey::Inline(v) => v.to_be_bytes().to_vec(),
        }
    }
}

impl Bom {
    fn new() -> Self {
        Self {
            blocks: vec![None],
            vars: Vec::new(),
        }
    }

    fn reserve(&mut self) -> u32 {
        self.blocks.push(None);
        (self.blocks.len() - 1) as u32
    }

    fn set(&mut self, index: u32, bytes: Vec<u8>) {
        let slot = &mut self.blocks[index as usize];
        assert!(slot.is_none(), "block {index} filled twice");
        *slot = Some(bytes);
    }

    fn add(&mut self, bytes: Vec<u8>) -> u32 {
        let index = self.reserve();
        self.set(index, bytes);
        index
    }

    fn var(&mut self, name: &'static str, block: u32) {
        self.vars.push((name, block));
    }

    fn reserve_tree(&mut self) -> TreeSlot {
        let entry = self.reserve();
        let page = self.reserve();
        TreeSlot { entry, page }
    }

    /// Fill a one-page tree.
    ///
    /// Entry block (29 bytes, golden a block[2]): `tree`, version 1, the page's
    /// block index, the page size, the entry count, then a byte that is 1 when
    /// keys are inline, the key length (0 when inline) and a zero word. Read
    /// off the three name goldens, whose FACETKEYS entries carry 1, 7 and 16
    /// for names of those lengths, and golden a's BITMAPKEYS entry (`01`, 0).
    ///
    /// Page (golden a block[3]): leaf flag 1, count, forward 0, backward 0,
    /// then per entry the value's block index and the key's block index (or
    /// the inline key), a zero word, then every key's bytes again, zero-padded
    /// to the page size PLUS the key bytes: 4096 + 4×18 = 4168 for RENDITIONS,
    /// 4096 + 7 = 4103 for FACETKEYS, 4096 + 1 = 4097 in golden name-A, and
    /// exactly 1024 for BITMAPKEYS, whose keys are inline.
    ///
    /// The copy in the page is the one Apple's reader uses: without the zero
    /// word before it, `assetutil` reported the appearance as
    /// "pearanceAny\0\0\0\0", the name read four bytes late.
    ///
    /// Entries are stored in ascending key order, which is the order golden a
    /// holds its four renditions in.
    fn fill_tree(&mut self, slot: TreeSlot, page_size: u32, mut entries: Vec<TreeEntry>) {
        entries.sort_by_key(|e| e.key.bytes());
        let inline = entries.iter().all(|e| matches!(e.key, TreeKey::Inline(_)));
        let key_len = entries
            .iter()
            .map(|e| match &e.key {
                TreeKey::Block { bytes, .. } => bytes.len(),
                TreeKey::Inline(_) => 0,
            })
            .max()
            .unwrap_or(0);

        let mut entry = Vec::with_capacity(29);
        entry.extend_from_slice(b"tree");
        entry.extend_from_slice(&1u32.to_be_bytes());
        entry.extend_from_slice(&slot.page.to_be_bytes());
        entry.extend_from_slice(&page_size.to_be_bytes());
        entry.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        entry.push(u8::from(inline));
        entry.extend_from_slice(&(key_len as u32).to_be_bytes());
        entry.extend_from_slice(&0u32.to_be_bytes());
        self.set(slot.entry, entry);

        let mut page = Vec::new();
        page.extend_from_slice(&1u16.to_be_bytes());
        page.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        page.extend_from_slice(&0u32.to_be_bytes());
        page.extend_from_slice(&0u32.to_be_bytes());
        let mut key_bytes = Vec::new();
        for e in &entries {
            page.extend_from_slice(&e.value.to_be_bytes());
            match &e.key {
                TreeKey::Block { index, bytes } => {
                    page.extend_from_slice(&index.to_be_bytes());
                    key_bytes.extend_from_slice(bytes);
                }
                TreeKey::Inline(v) => page.extend_from_slice(&v.to_be_bytes()),
            }
        }
        page.extend_from_slice(&0u32.to_le_bytes());
        page.extend_from_slice(&key_bytes);
        page.resize(page_size as usize + key_bytes.len(), 0);
        self.set(slot.page, page);
    }

    /// Lay the store out: header, blocks in index order, vars, block table.
    ///
    /// Header (golden a bytes 0..32): `BOMStore`, version 1, the number of
    /// non-null blocks, then the block table's offset and length and the var
    /// list's offset and length. The table is 256 (address, length) pairs
    /// after a count of 256, then a free list holding a count of 0 and two
    /// null pairs that the declared length includes: 4 + 256×8 + 4 + 16 =
    /// 2072, and offset + length is the file length (241984 + 2072 = 244056).
    fn finish(self) -> Vec<u8> {
        let mut out = vec![0u8; BOM_HEADER_LEN];
        let mut table = vec![(0u32, 0u32)];
        for (i, block) in self.blocks.iter().enumerate().skip(1) {
            let bytes = block
                .as_ref()
                .unwrap_or_else(|| panic!("block {i} was reserved and never filled"));
            align(&mut out);
            table.push((out.len() as u32, bytes.len() as u32));
            out.extend_from_slice(bytes);
        }
        assert!(
            table.len() <= BOM_TABLE_SLOTS,
            "{} blocks do not fit a {BOM_TABLE_SLOTS}-slot table",
            table.len()
        );

        align(&mut out);
        let vars_offset = out.len();
        out.extend_from_slice(&(self.vars.len() as u32).to_be_bytes());
        for (name, block) in &self.vars {
            out.extend_from_slice(&block.to_be_bytes());
            out.push(name.len() as u8);
            out.extend_from_slice(name.as_bytes());
        }
        let vars_length = out.len() - vars_offset;

        align(&mut out);
        let index_offset = out.len();
        out.extend_from_slice(&(BOM_TABLE_SLOTS as u32).to_be_bytes());
        for i in 0..BOM_TABLE_SLOTS {
            let (address, length) = table.get(i).copied().unwrap_or((0, 0));
            out.extend_from_slice(&address.to_be_bytes());
            out.extend_from_slice(&length.to_be_bytes());
        }
        out.extend_from_slice(&0u32.to_be_bytes()); // free list: no entries
        out.extend_from_slice(&[0u8; 16]); // ...and two null pairs after it
        let index_length = out.len() - index_offset;

        out[0..8].copy_from_slice(b"BOMStore");
        out[8..12].copy_from_slice(&1u32.to_be_bytes());
        out[12..16].copy_from_slice(&((table.len() - 1) as u32).to_be_bytes());
        out[16..20].copy_from_slice(&(index_offset as u32).to_be_bytes());
        out[20..24].copy_from_slice(&(index_length as u32).to_be_bytes());
        out[24..28].copy_from_slice(&(vars_offset as u32).to_be_bytes());
        out[28..32].copy_from_slice(&(vars_length as u32).to_be_bytes());
        out
    }
}

fn align(out: &mut Vec<u8>) {
    let pad = (BOM_ALIGN - out.len() % BOM_ALIGN) % BOM_ALIGN;
    out.resize(out.len() + pad, 0);
}

// ------------------------------------------------------------------ CoreUI --

/// The CoreUI build the reference catalog names, and what it reports as
/// `CoreUIVersion` (golden a block[1] bytes 4..8, `cf 03`).
const COREUI_VERSION: u32 = 975;
/// `StorageVersion` (block[1] bytes 8..12).
const STORAGE_VERSION: u32 = 17;
/// `SchemaVersion` (block[1] bytes 424..428).
const SCHEMA_VERSION: u32 = 2;
/// The two words after the schema version; Apple's reader does not name
/// them and the golden holds 1 and 2 (block[1] bytes 428..436).
const COLOR_SPACE_ID: u32 = 1;
const KEY_SEMANTICS: u32 = 2;
/// `MainVersion` (block[1] bytes 20..148, a 128-byte field).
const MAIN_VERSION: &str = "@(#)PROGRAM:CoreUI  PROJECT:CoreUI-975";
/// The Xcode this catalog claims to have been compiled by.
///
/// `AssetStorageVersion` (block[1] bytes 148..404, a 256-byte field) names
/// the tool that compiled the catalog, in the form the reference catalog
/// carries: `Xcode 26.6 (17F113) via AssetCatalogSimulatorAgent`. It is a
/// PARAMETER rather than that literal because the bundle already makes an
/// Xcode claim -- `DTXcode`/`DTXcodeBuild` in Info.plist, written by
/// packaging/ios/Makefile from a table keyed on the SDK actually linked --
/// and two different Xcode claims in one bundle is exactly the shape App
/// Store Connect's toolchain check exists to catch. The Makefile passes the
/// same row here, so the catalog and the plist cannot disagree. There is no
/// evidence either way on whether App Store Connect reads this field; it is
/// made consistent because consistency is free and a mismatch is a refusal
/// nobody can explain.
pub struct Xcode<'a> {
    /// The marketing version, e.g. `26.4`.
    pub version: &'a str,
    /// The build, e.g. `17E192`.
    pub build: &'a str,
}

impl Xcode<'_> {
    /// The `AssetStorageVersion` string, in the reference catalog's form.
    pub fn asset_storage_version(&self) -> String {
        format!(
            "Xcode {} ({}) via AssetCatalogSimulatorAgent",
            self.version, self.build
        )
    }
}
/// `Authoring Tool` (block[21] bytes 772..1028).
const AUTHORING_TOOL: &str =
    "@(#)PROGRAM:CoreThemeDefinition  PROJECT:CoreThemeDefinition-653.4  [IIO-2784.5.4]";
/// `Platform` and `PlatformVersion` (block[21] bytes 516..772 and 260..516).
/// The version is the deployment target actool was given, and the test at
/// the bottom of this file keeps it equal to the bundle's `MinimumOSVersion`.
const PLATFORM: &str = "ios";
pub const PLATFORM_VERSION: &str = "15.0";
/// The one appearance an icon set has (golden a block[8]), mapped to 0.
const APPEARANCE_ANY: &str = "UIAppearanceAny";

/// Rendition key attributes, by the numbers CoreUI gives them. The names are
/// the `kCRTheme…Name` strings Apple's reader prints for the key format.
const ATTR_ELEMENT: u16 = 1;
const ATTR_PART: u16 = 2;
const ATTR_APPEARANCE: u16 = 7;
const ATTR_DIMENSION2: u16 = 9;
const ATTR_SCALE: u16 = 12;
const ATTR_LOCALIZATION: u16 = 13;
const ATTR_IDIOM: u16 = 15;
const ATTR_SUBTYPE: u16 = 16;
const ATTR_IDENTIFIER: u16 = 17;
/// The key format: which attributes a rendition key holds and in what order
/// (golden a block[12] bytes 12..48, and the `Key Format` list Apple's
/// reader prints for it).
const KEY_FORMAT: [u16; 9] = [
    ATTR_APPEARANCE,
    ATTR_LOCALIZATION,
    ATTR_SCALE,
    ATTR_IDIOM,
    ATTR_SUBTYPE,
    ATTR_DIMENSION2,
    ATTR_IDENTIFIER,
    ATTR_ELEMENT,
    ATTR_PART,
];

/// The element every app-icon rendition carries (golden a block[11] bytes
/// 8..10, `55 00`) and the two parts under it: the image itself, which
/// Apple's reader calls "Icon Image", and the record listing the set's sizes,
/// "MultiSized Image".
const ELEMENT_ICON: u16 = 0x55;
const PART_ICON_IMAGE: u16 = 0xdc;
const PART_MULTISIZE: u16 = 0xda;

#[derive(Clone, Copy)]
enum Idiom {
    Phone = 1,
    Pad = 2,
}

#[derive(Clone, Copy)]
enum Rendition {
    Image,
    Multisize,
}

/// A CoreUI four-character tag as it sits in the file: the code is a
/// little-endian word, so `CTAR` is stored as the bytes `RATC`.
fn fourcc(tag: &[u8; 4]) -> [u8; 4] {
    u32::from_be_bytes(*tag).to_le_bytes()
}

/// A fixed-width string field, zero-padded.
fn fixed(s: &str, width: usize) -> Vec<u8> {
    assert!(s.len() < width, "{s:?} does not fit a {width}-byte field");
    let mut out = s.as_bytes().to_vec();
    out.resize(width, 0);
    out
}

/// CARHEADER (golden a block[1], 436 bytes).
fn car_header(rendition_count: u32, asset_storage_version: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(436);
    out.extend_from_slice(&fourcc(b"CTAR"));
    out.extend_from_slice(&COREUI_VERSION.to_le_bytes());
    out.extend_from_slice(&STORAGE_VERSION.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // storage timestamp: golden holds 0
    out.extend_from_slice(&rendition_count.to_le_bytes());
    out.extend_from_slice(&fixed(MAIN_VERSION, 128));
    out.extend_from_slice(&fixed(asset_storage_version, 256));
    out.extend_from_slice(&[0u8; 16]); // uuid: golden holds zeros
    out.extend_from_slice(&0u32.to_le_bytes()); // associated checksum: 0
    out.extend_from_slice(&SCHEMA_VERSION.to_le_bytes());
    out.extend_from_slice(&COLOR_SPACE_ID.to_le_bytes());
    out.extend_from_slice(&KEY_SEMANTICS.to_le_bytes());
    out
}

/// EXTENDED_METADATA (golden a block[21], 1028 bytes): `META` and four
/// 256-byte strings -- thinning arguments (empty), platform version,
/// platform, authoring tool. The tag is stored as the literal bytes `META`,
/// unlike every other CoreUI tag.
fn extended_metadata() -> Vec<u8> {
    let mut out = Vec::with_capacity(1028);
    out.extend_from_slice(b"META");
    out.extend_from_slice(&fixed("", 256));
    out.extend_from_slice(&fixed(PLATFORM_VERSION, 256));
    out.extend_from_slice(&fixed(PLATFORM, 256));
    out.extend_from_slice(&fixed(AUTHORING_TOOL, 256));
    out
}

/// KEYFORMAT (golden a block[12], 48 bytes): `kfmt`, version 0, the count,
/// then each attribute as a 32-bit word.
fn key_format() -> Vec<u8> {
    let mut out = Vec::with_capacity(48);
    out.extend_from_slice(&fourcc(b"kfmt"));
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(KEY_FORMAT.len() as u32).to_le_bytes());
    for attr in KEY_FORMAT {
        out.extend_from_slice(&u32::from(attr).to_le_bytes());
    }
    out
}

/// The value under the set name in FACETKEYS (golden a block[11], 18 bytes):
/// a cursor hot spot of (0, 0), then three (attribute, value) pairs -- the
/// element, the image part and the name identifier.
fn facet_value(identifier: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(18);
    for v in [
        0,
        0,
        3,
        ATTR_ELEMENT,
        ELEMENT_ICON,
        ATTR_PART,
        PART_ICON_IMAGE,
        ATTR_IDENTIFIER,
        identifier,
    ] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// A rendition key: one 16-bit value per attribute of [`KEY_FORMAT`], in its
/// order (golden a block[13], [15], [17], [19]). Scale is 1; the image part
/// carries dimension 1 and the multisize part dimension 0.
fn rendition_key(idiom: Idiom, identifier: u16, kind: Rendition) -> Vec<u8> {
    let (dimension2, part) = match kind {
        Rendition::Image => (1, PART_ICON_IMAGE),
        Rendition::Multisize => (0, PART_MULTISIZE),
    };
    let mut out = Vec::with_capacity(18);
    for v in [
        0,
        0,
        1,
        idiom as u16,
        0,
        dimension2,
        identifier,
        ELEMENT_ICON,
        part,
    ] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// The value under the name identifier in BITMAPKEYS (golden a block[24], 52
/// bytes): a version, a zero, the byte length of what follows, the attribute
/// count, then one word per key-format attribute -- a bit set of the values
/// the renditions use for it, or all ones for the four attributes that are
/// not summarised that way. Golden a holds idiom bits 1 and 2 (`06`); golden
/// d, phone only, holds bit 1 (`02`), which is what makes this a bit set.
fn bitmap_key_value(idioms: &[Idiom]) -> Vec<u8> {
    let idiom_bits = idioms.iter().fold(0u32, |acc, i| acc | 1 << (*i as u32));
    let mut out = Vec::with_capacity(52);
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&((KEY_FORMAT.len() as u32 + 1) * 4).to_le_bytes());
    out.extend_from_slice(&(KEY_FORMAT.len() as u32).to_le_bytes());
    for attr in KEY_FORMAT {
        let bits = match attr {
            ATTR_APPEARANCE | ATTR_IDENTIFIER | ATTR_ELEMENT | ATTR_PART => u32::MAX,
            ATTR_LOCALIZATION | ATTR_SUBTYPE => 1 << 0,
            ATTR_SCALE => 1 << 1,
            ATTR_IDIOM => idiom_bits,
            ATTR_DIMENSION2 => (1 << 0) | (1 << 1),
            _ => unreachable!("every key-format attribute is listed"),
        };
        out.extend_from_slice(&bits.to_le_bytes());
    }
    out
}

/// One rendition: a CSI header, its type-length-value list, and a body.
///
/// Header (184 bytes, golden a block[16] bytes 0..184): `CTSI`, version 1,
/// flags 0, width, height, scale (100 = @1x), pixel format, colour space, a
/// modification time of 0, the layout, a zero, a 128-byte name; then the TLV
/// byte length, the word 1, a zero and the body's byte length.
struct Csi<'a> {
    width: u32,
    height: u32,
    scale: u32,
    pixel_format: [u8; 4],
    color_space: u32,
    layout: u16,
    name: &'a str,
    tlvs: Vec<u8>,
    body: Vec<u8>,
}

impl Csi<'_> {
    fn bytes(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(184 + self.tlvs.len() + self.body.len());
        out.extend_from_slice(&fourcc(b"CTSI"));
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // rendition flags
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.extend_from_slice(&self.scale.to_le_bytes());
        out.extend_from_slice(&self.pixel_format);
        out.extend_from_slice(&self.color_space.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // modification time
        out.extend_from_slice(&self.layout.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&fixed(self.name, 128));
        out.extend_from_slice(&(self.tlvs.len() as u32).to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(self.body.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.tlvs);
        out.extend_from_slice(&self.body);
        out
    }
}

/// TLV types seen in the goldens. 0x3ef's meaning is not known: actool writes
/// it on the image rendition with the value 4096, which is also the row
/// stride of a 1024-wide BGRA image, and nothing here distinguishes the two.
const TLV_SLICES: u32 = 0x3e9;
const TLV_METRICS: u32 = 0x3eb;
const TLV_BLEND_MODE_AND_OPACITY: u32 = 0x3ec;
const TLV_EXIF_ORIENTATION: u32 = 0x3ee;
const TLV_UNKNOWN_3EF: u32 = 0x3ef;
const TLV_UNKNOWN_3EF_VALUE: u32 = 0x1000;

fn tlv(out: &mut Vec<u8>, kind: u32, words: &[u32]) {
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&((words.len() * 4) as u32).to_le_bytes());
    for w in words {
        out.extend_from_slice(&w.to_le_bytes());
    }
}

/// The image layout word (golden a block[16] bytes 36..38, `0c 00`).
const LAYOUT_IMAGE: u16 = 0x0c;
/// The multisize layout word (block[14] bytes 36..38, `f2 03`).
const LAYOUT_MULTISIZE: u16 = 0x3f2;

/// The image rendition (golden a block[16]; the pad copy, block[20], is byte
/// for byte the same).
///
/// TLVs (104 bytes, block[16] bytes 184..288): slices (count 1, x 0, y 0,
/// width, height); metrics (count 1, two zero insets, width, height); blend
/// mode 0 with opacity 1.0; EXIF orientation 1; and 0x3ef.
fn image_rendition(edge: u32, bgra: &[u8]) -> Vec<u8> {
    let mut tlvs = Vec::with_capacity(104);
    tlv(&mut tlvs, TLV_SLICES, &[1, 0, 0, edge, edge]);
    tlv(&mut tlvs, TLV_METRICS, &[1, 0, 0, 0, 0, edge, edge]);
    tlv(
        &mut tlvs,
        TLV_BLEND_MODE_AND_OPACITY,
        &[0, 1.0f32.to_bits()],
    );
    tlv(&mut tlvs, TLV_EXIF_ORIENTATION, &[1]);
    tlv(&mut tlvs, TLV_UNKNOWN_3EF, &[TLV_UNKNOWN_3EF_VALUE]);
    Csi {
        width: edge,
        height: edge,
        scale: 100,
        pixel_format: fourcc(b"ARGB"),
        color_space: 1,
        layout: LAYOUT_IMAGE,
        name: "AppIcon.png",
        tlvs,
        body: pixels_lzfse(bgra, (edge * 4) as usize),
    }
    .bytes()
}

/// The multisize rendition (golden a block[14], 236 bytes): a header with no
/// size, format or colour space, two TLVs (blend mode 0 with opacity 0.0,
/// EXIF orientation 1), and a 24-byte body: `MSIS`, version 1, one entry --
/// width, height, index 1.
fn multisize_rendition(name: &str, edge: u32) -> Vec<u8> {
    let mut tlvs = Vec::with_capacity(28);
    tlv(
        &mut tlvs,
        TLV_BLEND_MODE_AND_OPACITY,
        &[0, 0.0f32.to_bits()],
    );
    tlv(&mut tlvs, TLV_EXIF_ORIENTATION, &[1]);
    let mut body = Vec::with_capacity(24);
    body.extend_from_slice(&fourcc(b"MSIS"));
    for w in [1, 1, edge, edge, 1] {
        body.extend_from_slice(&w.to_le_bytes());
    }
    Csi {
        width: 0,
        height: 0,
        scale: 0,
        pixel_format: [0; 4],
        color_space: 0,
        layout: LAYOUT_MULTISIZE,
        name,
        tlvs,
        body,
    }
    .bytes()
}

/// Compression 4 = lzfse, the one actool used; 0 would be uncompressed.
const COMPRESSION_LZFSE: u32 = 4;
/// Rows per chunk. actool split a 1024x1024 BGRA image into chunks of 341
/// rows and one of 1 (golden a block[16]: `CBCK` records with row counts
/// 341, 341, 341, 1). Why 341 is not known; it is what is reproduced.
const ROWS_PER_CHUNK: usize = 341;

/// The pixel body (golden a block[16] bytes 288..): `CELM`, version 3, the
/// compression type, the chunk count, then per chunk `CBCK`, two zero words,
/// the row count, the compressed byte length and one self-contained lzfse
/// stream (`bvx2` … `bvx$`) of those rows.
fn pixels_lzfse(bgra: &[u8], stride: usize) -> Vec<u8> {
    let chunks: Vec<&[u8]> = bgra.chunks(ROWS_PER_CHUNK * stride).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&fourcc(b"CELM"));
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&COMPRESSION_LZFSE.to_le_bytes());
    out.extend_from_slice(&(chunks.len() as u32).to_le_bytes());
    for chunk in chunks {
        let mut stream = Vec::new();
        lzfse_rust::encode_bytes(chunk, &mut stream).expect("encoding into memory cannot fail");
        out.extend_from_slice(&fourcc(b"CBCK"));
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&((chunk.len() / stride) as u32).to_le_bytes());
        out.extend_from_slice(&(stream.len() as u32).to_le_bytes());
        out.extend_from_slice(&stream);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Xcode that compiled the reference catalog every literal in this
    /// module was read from. Tests build with this identity so the bytes they
    /// pin are the golden's bytes.
    const GOLDEN_XCODE: Xcode<'static> = Xcode {
        version: "26.6",
        build: "17F113",
    };

    /// A test image with every pixel encoding its own position, so a wrong
    /// channel order, row order or stride is a wrong pixel and not a wrong
    /// colour cast.
    fn coordinate_image(edge: u32) -> Vec<u8> {
        let mut bgra = Vec::with_capacity((edge * edge * 4) as usize);
        for y in 0..edge {
            for x in 0..edge {
                bgra.extend_from_slice(&[
                    ((x ^ y) & 255) as u8,
                    (y >> 2) as u8,
                    (x >> 2) as u8,
                    255,
                ]);
            }
        }
        bgra
    }

    fn u16le(b: &[u8], o: usize) -> u16 {
        u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
    }
    fn u32le(b: &[u8], o: usize) -> u32 {
        u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
    }
    fn u16be(b: &[u8], o: usize) -> u16 {
        u16::from_be_bytes(b[o..o + 2].try_into().unwrap())
    }
    fn u32be(b: &[u8], o: usize) -> u32 {
        u32::from_be_bytes(b[o..o + 4].try_into().unwrap())
    }

    /// One tree entry as read back: the key's bytes and the value block.
    type Entry = (Vec<u8>, Vec<u8>);

    /// A reader for the container, written from the format description and
    /// not from the writer, so that the two can disagree. Every check is an
    /// `Err`, not a panic, so `rejects_a_broken_container` can show the gate
    /// has a failing arm.
    struct Store {
        bytes: Vec<u8>,
        blocks: Vec<(u32, u32)>,
        vars: Vec<(String, u32)>,
    }

    fn parse(bytes: &[u8]) -> Result<Store, String> {
        if bytes.len() < BOM_HEADER_LEN {
            return Err(format!("{} bytes is shorter than the header", bytes.len()));
        }
        if &bytes[0..8] != b"BOMStore" {
            return Err("magic is not BOMStore".into());
        }
        if u32be(bytes, 8) != 1 {
            return Err("version is not 1".into());
        }
        let count = u32be(bytes, 12) as usize;
        let index_offset = u32be(bytes, 16) as usize;
        let index_length = u32be(bytes, 20) as usize;
        let vars_offset = u32be(bytes, 24) as usize;
        let vars_length = u32be(bytes, 28) as usize;
        if index_offset + index_length != bytes.len() {
            return Err(format!(
                "index {index_offset}+{index_length} does not end at the file end {}",
                bytes.len()
            ));
        }
        let slots = u32be(bytes, index_offset) as usize;
        if slots != BOM_TABLE_SLOTS {
            return Err(format!("block table has {slots} slots"));
        }
        let mut blocks = Vec::with_capacity(slots);
        for i in 0..slots {
            let at = index_offset + 4 + 8 * i;
            blocks.push((u32be(bytes, at), u32be(bytes, at + 4)));
        }
        if blocks[0] != (0, 0) {
            return Err("block 0 is not null".into());
        }
        let used: Vec<(u32, u32)> = blocks.iter().copied().filter(|b| b.1 != 0).collect();
        if used.len() != count {
            return Err(format!(
                "header counts {count} blocks, table has {}",
                used.len()
            ));
        }
        let mut end = BOM_HEADER_LEN;
        for (address, length) in &used {
            let (address, length) = (*address as usize, *length as usize);
            if address % BOM_ALIGN != 0 {
                return Err(format!("block at {address} is not 16-aligned"));
            }
            if address < end {
                return Err(format!("block at {address} overlaps the one before"));
            }
            if address + length > vars_offset {
                return Err(format!("block at {address} runs into the vars"));
            }
            end = address + length;
        }
        let free_list = index_offset + 4 + 8 * slots;
        if u32be(bytes, free_list) != 0 || bytes[free_list + 4..] != [0u8; 16] {
            return Err("free list is not empty with two null pairs".into());
        }
        let mut vars = Vec::new();
        let mut at = vars_offset + 4;
        for _ in 0..u32be(bytes, vars_offset) {
            let index = u32be(bytes, at);
            let len = bytes[at + 4] as usize;
            let name = String::from_utf8(bytes[at + 5..at + 5 + len].to_vec()).unwrap();
            if index as usize >= slots || blocks[index as usize].1 == 0 {
                return Err(format!("var {name} names block {index}, which is empty"));
            }
            vars.push((name, index));
            at += 5 + len;
        }
        if at - vars_offset != vars_length {
            return Err("vars length disagrees with the var list".into());
        }
        Ok(Store {
            bytes: bytes.to_vec(),
            blocks,
            vars,
        })
    }

    /// The message a broken container was refused with.
    fn rejection(parsed: Result<Store, String>) -> String {
        parsed.err().expect("a broken container was accepted")
    }

    impl Store {
        fn block(&self, index: u32) -> Result<&[u8], String> {
            let (address, length) = self
                .blocks
                .get(index as usize)
                .copied()
                .ok_or_else(|| format!("block {index} is off the table"))?;
            if length == 0 {
                return Err(format!("block {index} is empty"));
            }
            Ok(&self.bytes[address as usize..(address + length) as usize])
        }

        fn var(&self, name: &str) -> Result<&[u8], String> {
            let (_, index) = self
                .vars
                .iter()
                .find(|(n, _)| n == name)
                .ok_or_else(|| format!("no var {name}"))?;
            self.block(*index)
        }

        /// Walk a one-page tree: `(key bytes, value block)` per entry.
        fn tree(&self, name: &str) -> Result<Vec<Entry>, String> {
            let entry = self.var(name)?;
            if &entry[0..4] != b"tree" || u32be(entry, 4) != 1 {
                return Err(format!("{name} entry is not a version-1 tree"));
            }
            let page = self.block(u32be(entry, 8))?;
            let page_size = u32be(entry, 12) as usize;
            let count = u32be(entry, 16) as usize;
            let inline = entry[20] == 1;
            let key_len = u32be(entry, 21) as usize;
            if u16be(page, 0) != 1 || u16be(page, 2) as usize != count {
                return Err(format!("{name} page is not a leaf of {count}"));
            }
            let mut out = Vec::new();
            let mut key_bytes = Vec::new();
            for i in 0..count {
                let at = 12 + 8 * i;
                let value = self.block(u32be(page, at))?.to_vec();
                let key = if inline {
                    page[at + 4..at + 8].to_vec()
                } else {
                    let key = self.block(u32be(page, at + 4))?.to_vec();
                    if key.len() != key_len {
                        return Err(format!(
                            "{name} key {i} is {} bytes, entry says {key_len}",
                            key.len()
                        ));
                    }
                    key_bytes.extend_from_slice(&key);
                    key
                };
                out.push((key, value));
            }
            let keys_at = 12 + 8 * count + 4;
            if page[keys_at - 4..keys_at] != [0; 4] {
                return Err(format!("{name} page has no zero word before its keys"));
            }
            if page[keys_at..keys_at + key_bytes.len()] != key_bytes[..] {
                return Err(format!("{name} page does not repeat its keys"));
            }
            if page.len() != page_size + key_bytes.len() {
                return Err(format!(
                    "{name} page is {} bytes, not {page_size} + keys",
                    page.len()
                ));
            }
            if out.windows(2).any(|w| w[0].0 > w[1].0) {
                return Err(format!("{name} keys are not in ascending order"));
            }
            Ok(out)
        }
    }

    fn catalog() -> Vec<u8> {
        assets_car(
            APP_ICON.0,
            APP_ICON.1,
            1024,
            &coordinate_image(1024),
            &GOLDEN_XCODE,
        )
    }

    #[test]
    fn container_parses_and_every_var_resolves() {
        let store = parse(&catalog()).expect("the container holds its own invariants");
        let names: Vec<&str> = store.vars.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "CARHEADER",
                "RENDITIONS",
                "FACETKEYS",
                "APPEARANCEKEYS",
                "KEYFORMAT",
                "EXTENDED_METADATA",
                "BITMAPKEYS"
            ]
        );
        for name in names {
            store.var(name).unwrap();
        }
    }

    /// The gate's failing arm: each of these is one byte or one cut away from
    /// a good file, and the reader has to say no to every one.
    #[test]
    fn rejects_a_broken_container() {
        let good = catalog();
        parse(&good).unwrap();

        let truncated = &good[..good.len() - 1];
        assert!(rejection(parse(truncated)).contains("does not end at the file end"));

        let mut bad_magic = good.clone();
        bad_magic[0] = b'X';
        assert!(rejection(parse(&bad_magic)).contains("magic"));

        let mut misaligned = good.clone();
        let index_offset = u32be(&good, 16) as usize;
        misaligned[index_offset + 4 + 8..index_offset + 4 + 12]
            .copy_from_slice(&513u32.to_be_bytes());
        assert!(rejection(parse(&misaligned)).contains("not 16-aligned"));

        let mut miscounted = good.clone();
        miscounted[12..16].copy_from_slice(&99u32.to_be_bytes());
        assert!(rejection(parse(&miscounted)).contains("header counts 99"));

        // A tree whose page points a key at an empty block.
        let store = parse(&good).unwrap();
        let entry = store.var("FACETKEYS").unwrap();
        let (page_address, _) = store.blocks[u32be(entry, 8) as usize];
        let mut dangling = good.clone();
        let key_slot = page_address as usize + 16;
        dangling[key_slot..key_slot + 4].copy_from_slice(&200u32.to_be_bytes());
        assert!(
            parse(&dangling)
                .unwrap()
                .tree("FACETKEYS")
                .unwrap_err()
                .contains("block 200")
        );
    }

    #[test]
    fn trees_walk_and_keys_match_their_values() {
        let store = parse(&catalog()).unwrap();

        let appearances = store.tree("APPEARANCEKEYS").unwrap();
        assert_eq!(appearances, vec![(b"UIAppearanceAny".to_vec(), vec![0, 0])]);

        let facets = store.tree("FACETKEYS").unwrap();
        assert_eq!(facets.len(), 1);
        assert_eq!(facets[0].0, b"AppIcon");
        // Golden a block[11] @13504, 18 bytes.
        assert_eq!(
            facets[0].1,
            [
                0, 0, 0, 0, 3, 0, 1, 0, 0x55, 0, 2, 0, 0xdc, 0, 0x11, 0, 0xc1, 0x1a
            ]
        );

        let bitmaps = store.tree("BITMAPKEYS").unwrap();
        assert_eq!(bitmaps.len(), 1);
        assert_eq!(bitmaps[0].0, 6849u32.to_be_bytes());
        // Golden a block[24] @241792, 52 bytes.
        let mut want = vec![1, 0, 0, 0, 0, 0, 0, 0, 40, 0, 0, 0, 9, 0, 0, 0];
        for w in [u32::MAX, 1, 2, 6, 1, 3, u32::MAX, u32::MAX, u32::MAX] {
            want.extend_from_slice(&w.to_le_bytes());
        }
        assert_eq!(bitmaps[0].1, want);

        let renditions = store.tree("RENDITIONS").unwrap();
        // Golden a block[3] lists, in this order, blocks [17], [15], [13], [19]:
        // phone multisize, phone image, pad multisize, pad image.
        let keys: Vec<Vec<u16>> = renditions
            .iter()
            .map(|(k, _)| (0..9).map(|i| u16le(k, 2 * i)).collect())
            .collect();
        assert_eq!(
            keys,
            [
                [0, 0, 1, 1, 0, 0, 6849, 0x55, 0xda],
                [0, 0, 1, 1, 0, 1, 6849, 0x55, 0xdc],
                [0, 0, 1, 2, 0, 0, 6849, 0x55, 0xda],
                [0, 0, 1, 2, 0, 1, 6849, 0x55, 0xdc],
            ]
        );
    }

    /// Lengths and literals of the fixed records, each from golden a.
    #[test]
    fn fixed_records_match_the_reference_catalog() {
        let store = parse(&catalog()).unwrap();

        let header = store.var("CARHEADER").unwrap();
        assert_eq!(header.len(), 436); // block[1] @512
        assert_eq!(
            &header[0..20],
            b"RATC\xcf\x03\0\0\x11\0\0\0\0\0\0\0\x04\0\0\0"
        );
        assert_eq!(&header[20..58], MAIN_VERSION.as_bytes());
        // The reference catalog was compiled by Xcode 26.6 (17F113); a catalog
        // built with that identity carries the same bytes there.
        let want = GOLDEN_XCODE.asset_storage_version();
        assert_eq!(&header[148..148 + want.len()], want.as_bytes());
        assert_eq!(&header[424..436], b"\x02\0\0\0\x01\0\0\0\x02\0\0\0");

        let metadata = store.var("EXTENDED_METADATA").unwrap();
        assert_eq!(metadata.len(), 1028); // block[21] @239696
        assert_eq!(&metadata[0..4], b"META");
        assert_eq!(&metadata[260..264], b"15.0");
        assert_eq!(&metadata[516..519], b"ios");
        assert_eq!(
            &metadata[772..772 + AUTHORING_TOOL.len()],
            AUTHORING_TOOL.as_bytes()
        );

        // block[12] @13536, 48 bytes.
        let key_format = store.var("KEYFORMAT").unwrap();
        assert_eq!(
            key_format,
            b"tmfk\0\0\0\0\x09\0\0\0\x07\0\0\0\x0d\0\0\0\x0c\0\0\0\x0f\0\0\0\x10\0\0\0\x09\0\0\0\x11\0\0\0\x01\0\0\0\x02\0\0\0"
        );

        // block[2] @960: the RENDITIONS tree entry names page block 3, a
        // 4096-byte page, 4 entries, block keys of 18 bytes.
        assert_eq!(
            store.var("RENDITIONS").unwrap(),
            b"tree\0\0\0\x01\0\0\0\x03\0\0\x10\0\0\0\0\x04\0\0\0\0\x12\0\0\0\0"
        );
        // block[22] @240736: BITMAPKEYS names page block 23, 1024-byte page,
        // 1 entry, inline keys.
        assert_eq!(
            store.var("BITMAPKEYS").unwrap(),
            b"tree\0\0\0\x01\0\0\0\x17\0\0\x04\0\0\0\0\x01\x01\0\0\0\0\0\0\0\0"
        );
        assert_eq!(store.block(3).unwrap().len(), 4168); // block[3] @992
        assert_eq!(store.block(5).unwrap().len(), 4103); // block[5] @5200
        assert_eq!(store.block(7).unwrap().len(), 4111); // block[7] @9344
        assert_eq!(store.block(23).unwrap().len(), 1024); // block[23] @240768

        // The whole layout up to the first image is the golden's, address for
        // address, because everything before it is fixed-size.
        let addresses: Vec<u32> = (1..=16).map(|i| store.blocks[i].0).collect();
        assert_eq!(
            addresses,
            [
                512, 960, 992, 5168, 5200, 9312, 9344, 13456, 13472, 13488, 13504, 13536, 13584,
                13616, 13856, 13888
            ]
        );
        assert_eq!(
            u32be(&store.bytes, 20),
            2072,
            "index length, golden bytes 20..24"
        );
        assert_eq!(
            u32be(&store.bytes, 28),
            117,
            "vars length, golden bytes 28..32"
        );
    }

    /// The image rendition, decoded by something that is not the encoder:
    /// header fields as Apple's reader reports them, the chunking actool
    /// used, and the pixels back out of lzfse in the order they went in.
    #[test]
    fn image_rendition_round_trips_through_lzfse() {
        let bgra = coordinate_image(1024);
        let store = parse(&assets_car(
            APP_ICON.0,
            APP_ICON.1,
            1024,
            &bgra,
            &GOLDEN_XCODE,
        ))
        .unwrap();
        let renditions = store.tree("RENDITIONS").unwrap();
        let images: Vec<&Vec<u8>> = renditions
            .iter()
            .filter(|(k, _)| u16le(k, 16) == PART_ICON_IMAGE)
            .map(|(_, v)| v)
            .collect();
        assert_eq!(images.len(), 2, "one image per idiom");
        assert_eq!(images[0], images[1], "the idioms share one encoding");

        let csi = images[0];
        assert_eq!(&csi[0..4], b"ISTC");
        assert_eq!(u32le(csi, 4), 1, "version");
        assert_eq!(u32le(csi, 8), 0, "flags");
        assert_eq!(
            (u32le(csi, 12), u32le(csi, 16), u32le(csi, 20)),
            (1024, 1024, 100)
        );
        assert_eq!(
            &csi[24..28],
            b"BGRA",
            "the ARGB pixel format code, stored little-endian"
        );
        assert_eq!(u16le(csi, 36), LAYOUT_IMAGE);
        assert_eq!(&csi[40..51], b"AppIcon.png");
        let tlv_len = u32le(csi, 168) as usize;
        let body_len = u32le(csi, 180) as usize;
        assert_eq!(tlv_len, 104, "golden a block[16] bytes 168..172");
        assert_eq!(184 + tlv_len + body_len, csi.len());
        // Golden a block[16] bytes 184..288.
        let mut want = Vec::new();
        for w in [
            0x3e9,
            20,
            1,
            0,
            0,
            1024,
            1024,
            0x3eb,
            28,
            1,
            0,
            0,
            0,
            0,
            1024,
            1024,
            0x3ec,
            8,
            0,
            0x3f80_0000,
            0x3ee,
            4,
            1,
            0x3ef,
            4,
            0x1000,
        ] {
            want.extend_from_slice(&(w as u32).to_le_bytes());
        }
        assert_eq!(&csi[184..288], &want[..]);

        let body = &csi[288..];
        assert_eq!(&body[0..4], b"MLEC");
        assert_eq!((u32le(body, 4), u32le(body, 8)), (3, COMPRESSION_LZFSE));
        let chunks = u32le(body, 12);
        assert_eq!(chunks, 4);
        let mut rows = Vec::new();
        let mut pixels = Vec::new();
        let mut at = 16;
        for _ in 0..chunks {
            assert_eq!(&body[at..at + 4], b"KCBC");
            assert_eq!((u32le(body, at + 4), u32le(body, at + 8)), (0, 0));
            rows.push(u32le(body, at + 12));
            let len = u32le(body, at + 16) as usize;
            let stream = &body[at + 20..at + 20 + len];
            // An lzfse stream opens with a compressed block, `bvx2`, or for
            // a small input the LZVN kind, `bvxn`; the goldens hold only the
            // first, and actool's one-row chunk is exactly the size at which
            // this encoder switches to the second.
            assert!(
                matches!(&stream[0..4], b"bvx2" | b"bvxn"),
                "chunk does not open with an lzfse block: {:?}",
                &stream[0..4]
            );
            assert_eq!(&stream[len - 4..], b"bvx$");
            lzfse_rust::decode_bytes(stream, &mut pixels).unwrap();
            at += 20 + len;
        }
        assert_eq!(at, body.len(), "bytes after the last chunk");
        assert_eq!(rows, [341, 341, 341, 1], "golden a's chunk rows");
        assert_eq!(pixels, bgra, "pixels differ after the round trip");
    }

    #[test]
    fn multisize_rendition_lists_the_one_size() {
        let store = parse(&catalog()).unwrap();
        let renditions = store.tree("RENDITIONS").unwrap();
        let multi: Vec<&Vec<u8>> = renditions
            .iter()
            .filter(|(k, _)| u16le(k, 16) == PART_MULTISIZE)
            .map(|(_, v)| v)
            .collect();
        assert_eq!(multi.len(), 2);
        let csi = multi[0];
        assert_eq!(csi.len(), 236, "golden a block[14] @13616");
        assert_eq!(u16le(csi, 36), LAYOUT_MULTISIZE);
        assert_eq!(&csi[40..47], b"AppIcon");
        assert_eq!(u32le(csi, 168), 28);
        assert_eq!(u32le(csi, 180), 24);
        // Golden a block[14] bytes 212..236.
        assert_eq!(
            &csi[212..],
            b"SISM\x01\0\0\0\x01\0\0\0\0\x04\0\0\0\x04\0\0\x01\0\0\0"
        );
    }

    /// The deployment target written into the catalog is the bundle's.
    #[test]
    fn platform_version_is_the_bundle_minimum() {
        let plist = include_str!("../../packaging/ios/Info.plist");
        let key = "<key>MinimumOSVersion</key>";
        let after = &plist[plist
            .find(key)
            .expect("Info.plist names a MinimumOSVersion")
            + key.len()..];
        let start = after.find("<string>").unwrap() + "<string>".len();
        let end = after.find("</string>").unwrap();
        assert_eq!(&after[start..end], PLATFORM_VERSION);
    }

    /// Every pair here was read by Apple's `assetutil` out of a catalog Apple's
    /// `actool` compiled from a set of that name. A slice of the 56-name sweep,
    /// chosen to cover the arithmetic that identified the function: single
    /// characters, consecutive last characters (the constant 35937 step),
    /// consecutive second-to-last characters (the 6273 step), the repeated-A
    /// series (position weights), and the two names this project uses.
    #[test]
    fn name_identifier_matches_every_compiled_catalog() {
        let compiled: &[(&str, u16)] = &[
            ("AppIcon", 6849),
            ("Other", 8458),
            ("A", 5802),
            ("B", 41739),
            ("a", 41674),
            ("0", 50233),
            ("AA", 64228),
            ("AB", 34629),
            ("AC", 5030),
            ("AD", 40967),
            ("BA", 4965),
            ("CA", 11238),
            ("AAA", 12049),
            ("AAAA", 14571),
            ("Icon", 44501),
            ("Squallar", 3761),
            ("AppIcon2", 21817),
            ("abcdefghijklmnop", 15232),
        ];
        for (name, want) in compiled {
            assert_eq!(
                name_identifier(name),
                *want,
                "actool compiled a set named {name:?} with identifier {want}; \
                 this function computes {}",
                name_identifier(name)
            );
        }
        assert_eq!(
            APP_ICON.1, 6849,
            "the app icon's identifier is the reference catalog's"
        );
    }
}
