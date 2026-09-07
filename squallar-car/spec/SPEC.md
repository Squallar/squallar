# carspec-SPEC — byte-level layout of `Assets.car` as Xcode 26.6 actool writes it for one 1024×1024 app icon

Scope: `xcrun actool --platform iphoneos --minimum-deployment-target 15.0 --app-icon <set>` on a
catalog with ONE `.appiconset` holding ONE universal 1024×1024 PNG. Every value below was read from
the goldens under `scratchpad/goldens/` (labels `a b c d-iphone-only name-*`, plus `batch/out/*`)
and is reproduced by `carspec-dump.py` (which passes `--check-against assetutil.json`,
`--strict-actool` and `--require-decode` on all 61 of them). "golden a @0x…" means the absolute
offset in `goldens/out/a/Assets.car`. Anything not read off a golden is marked **UNVERIFIED**.

Endianness rule: every integer of the BOM container (header, block table, vars, tree headers, leaf
blocks) is **big-endian**; every integer inside a CAR block (CARHEADER, META, kfmt, keys, CSI, TLV,
CELM, CBCK, MSIS, bitmap value) is **little-endian**. Four-byte tags of CAR blocks are stored as an
LE u32 of the FourCC, i.e. reversed on disk (`CTAR`→bytes `RATC`, `kfmt`→`tmfk`, `CTSI`→`ISTC`,
`CELM`→`MLEC`, `CBCK`→`KCBC`, `MSIS`→`SISM`) — **except** EXTENDED_METADATA whose tag is the literal
bytes `META` (golden a @0x3a850 = `4d 45 54 41`).

## 1. BOM container

### 1.1 Header (32 bytes at 0, zero-padded to 0x200)

| off | size | field | golden a | golden b | golden d |
|---|---|---|---|---|---|
| 0x00 | 8 | magic `BOMStore` | | | |
| 0x08 | u32 | version | 1 | 1 | 1 |
| 0x0c | u32 | numberOfBlocks = non-null block-table entries | 24 | 24 | 20 |
| 0x10 | u32 | indexOffset | 0x3b140 | 0x5b80 | 0x1f780 |
| 0x14 | u32 | indexLength (= 4 + 256·8 + 4 + 16 = 2072) | 2072 | 2072 | 2072 |
| 0x18 | u32 | varsOffset | 0x3b0c0 | 0x5b00 | 0x1f700 |
| 0x1c | u32 | varsLength | 117 | 117 | 117 |
| 0x20..0x200 | 480 | zero | | | |

`indexOffset + indexLength == file length` in every golden (the index is the last thing in the file).

### 1.2 File layout (golden a; b/c/d identical in structure)

Blocks are written in ascending block-index order starting at 0x200; each block starts at the next
16-byte boundary after the previous one; gaps are zero. Nothing is 4096-aligned. After the last
block: vars at the next 16-byte boundary, then the index (block table + free list) at the next
16-byte boundary, then EOF.

| idx | offset (a) | len | content |
|---|---|---|---|
| 1 | 0x200 | 436 | CARHEADER |
| 2 | 0x3c0 | 29 | RENDITIONS tree header |
| 3 | 0x3e0 | 4096+18·N | RENDITIONS leaf (N=4 → 4168; d: N=2 → 4132) |
| 4 | 0x1430 | 29 | FACETKEYS tree header |
| 5 | 0x1450 | 4096+len(name) | FACETKEYS leaf (`AppIcon` → 4103; `Other` → 4101) |
| 6 | 0x2460 | 29 | APPEARANCEKEYS tree header |
| 7 | 0x2480 | 4111 | APPEARANCEKEYS leaf (4096+15) |
| 8 | 0x3490 | 15 | appearance key `UIAppearanceAny` (no NUL) |
| 9 | 0x34a0 | 2 | appearance value u16 LE 0 |
| 10 | 0x34b0 | len(name) | facet key = set name, UTF-8, no NUL |
| 11 | 0x34c0 | 18 | facet value (§4) |
| 12 | 0x34e0 | 48 | KEYFORMAT |
| 13,14 | 0x3510, 0x3530 | 18, 236 | rendition key + value: idiom pad, dim2 0, part 218 (MSI) |
| 15,16 | 0x3620, 0x3640 | 18, 112750 | key + value: idiom phone, dim2 1, part 220 (image) |
| 17,18 | 0x1eeb0, 0x1eed0 | 18, 236 | key + value: idiom phone, dim2 0, part 218 (MSI) |
| 19,20 | 0x1efc0, 0x1efe0 | 18, 112750 | key + value: idiom pad, dim2 1, part 220 (image) |
| 21 | 0x3a850 | 1028 | EXTENDED_METADATA |
| 22 | 0x3ac60 | 29 | BITMAPKEYS tree header (blockSize 1024) |
| 23 | 0x3ac80 | 1024 | BITMAPKEYS leaf |
| 24 | 0x3b080 | 52 | BITMAPKEYS value |
| — | 0x3b0c0 | 117 | vars |
| — | 0x3b140 | 2072 | index: block table + free list |

Golden d (iphone only) has blocks 13,14 = (phone image), 15,16 = (phone MSI), then META=17,
BITMAPKEYS tree=18, leaf=19, value=20. Note the key/value block allocation order in a is
pad-MSI, phone-image, phone-MSI, pad-image — it is NOT the leaf (sorted) order; a rendition's value
block always immediately follows its key block.

### 1.3 Block table and free list (golden a @0x3b140)

`u32 count = 256`, then 256 × (`u32 address`, `u32 length`); entry 0 is null; entries 1..N are the
blocks above; entries N+1..255 are null. Then `u32 freeListCount = 0`, then **16 zero bytes**
(a @0x3b948..0x3b958). Any block length that changes (facet key, leaf) changes exactly its
entry's `length` (a-vs-c: @0x3b173 and @0x3b19b, see carspec-diff.md).

### 1.4 Vars (golden a @0x3b0c0)

`u32 count = 7`, then per var `u32 blockIndex`, `u8 nameLen`, name bytes (no NUL), in this order:
`CARHEADER`=1, `RENDITIONS`=2, `FACETKEYS`=4, `APPEARANCEKEYS`=6, `KEYFORMAT`=12,
`EXTENDED_METADATA`=21, `BITMAPKEYS`=22 (d: 17, 18). Total 4 + Σ(5+len) = 117.

## 2. Trees

### 2.1 Tree header block (29 bytes, BE; golden a @0x3c0 / 0x1430 / 0x2460 / 0x3ac60)

| off | size | field | RENDITIONS | FACETKEYS | APPEARANCEKEYS | BITMAPKEYS |
|---|---|---|---|---|---|---|
| 0 | 4 | `tree` | | | | |
| 4 | u32 | version | 1 | 1 | 1 | 1 |
| 8 | u32 | child = leaf block index | 3 | 5 | 7 | 23 |
| 12 | u32 | blockSize | 4096 | 4096 | 4096 | **1024** |
| 16 | u32 | pathCount = number of entries | 4 (d: 2) | 1 | 1 | 1 |
| 20 | u8 | flag: 0 = keys are blocks, 1 = keys inline in the entry | 0 | 0 | 0 | **1** |
| 21 | u32 | key size (all keys have this length) | 18 | len(name) (7; `Other` 5; 16 for the 16-char name) | 15 | 0 |
| 25 | u32 | 0 | 0 | 0 | 0 | 0 |

(bomutils' 21-byte `BOMTree` ends at the flag byte; actool writes the two extra u32s.) The
meaning of the flag byte is inferred from BITMAPKEYS being the only tree whose entry "key index" is
not a block index (it is the NameIdentifier itself, 6849 = 0x1ac1 @0x3ac90 in a; 0x210a in c) —
**semantics UNVERIFIED**, values verified.

### 2.2 Leaf (paths) block (BE)

```
u16 isLeaf = 1
u16 count
u32 forward = 0
u32 backward = 0
count × { u32 valueBlockIndex ; u32 keyBlockIndex (or inline u32 key for BITMAPKEYS) }
u32 0                                  (4 zero bytes)
keys concatenated in entry order       (absent for BITMAPKEYS)
zero to the block length
block length = blockSize + Σ keyLen    (4168 = 4096 + 4·18; 4103 = 4096 + 7; 4111 = 4096 + 15; 1024 = 1024 + 0)
```
Golden a RENDITIONS leaf @0x3e0: entries (value,key) = (18,17) (16,15) (14,13) (20,19) @0x3ec..0x40c,
inline key copy @0x410..0x458. The copy is a second copy of the same 18-byte keys the key blocks
hold; whether CoreUI reads it is UNVERIFIED — actool writes it, and `--strict-actool` checks it.

Leaf entries are in ascending key order: for RENDITIONS (phone,dim2 0) < (phone,dim2 1) <
(pad,dim2 0) < (pad,dim2 1), i.e. sorted by the key bytes. In these goldens byte-wise (memcmp)
order and u16-tuple order coincide (all differing values < 256); **which one actool uses is
UNVERIFIED** — a catalog with two icon sets whose NameIdentifiers differ in byte order vs numeric
order (e.g. `AppIcon` 0x1ac1 and `Other` 0x210a: LE bytes `c1 1a` vs `0a 21`) would settle it.
Single leaf, no branch nodes, in every golden (forward = backward = 0).

## 3. CARHEADER (block 1, 436 bytes, LE; golden a @0x200)

| off | size | field | value in every golden |
|---|---|---|---|
| 0 | 4 | tag `CTAR` (bytes `RATC`) | |
| 4 | u32 | coreuiVersion | 975 |
| 8 | u32 | storageVersion | 17 |
| 12 | u32 | storageTimestamp | **0** (assetutil's "Timestamp" is not in the file; a and b differ by 1 s in assetutil yet are byte-equal here) |
| 16 | u32 | renditionCount | 4 (d: 2) |
| 20 | char[128] | mainVersionString | `@(#)PROGRAM:CoreUI  PROJECT:CoreUI-975` (two spaces), NUL-padded |
| 148 | char[256] | versionString | `Xcode 26.6 (17F113) via AssetCatalogSimulatorAgent`, NUL-padded |
| 404 | 16 | uuid | all zero |
| 420 | u32 | associatedChecksum | 0 |
| 424 | u32 | schemaVersion | 2 |
| 428 | u32 | colorSpaceID | 1 |
| 432 | u32 | keySemantics | 2 |

## 4. EXTENDED_METADATA (block 21, 1028 bytes; golden a @0x3a850)

| off | size | field | value |
|---|---|---|---|
| 0 | 4 | literal bytes `META` (not swapped) | |
| 4 | char[256] | thinningArguments | empty |
| 260 | char[256] | deploymentPlatformVersion | `15.0` |
| 516 | char[256] | deploymentPlatform | `ios` |
| 772 | char[256] | authoringTool | `@(#)PROGRAM:CoreThemeDefinition  PROJECT:CoreThemeDefinition-653.4  [IIO-2784.5.4]` |

## 5. KEYFORMAT (block 12, 48 bytes; golden a @0x34e0)

`kfmt` (bytes `tmfk`), u32 version 0, u32 count 9, then u32 tokens in this order — this is the
rendition-key field order:

| # | token | name (assetutil) |
|---|---|---|
| 0 | 7 | kCRThemeAppearanceName |
| 1 | 13 | kCRThemeLocalizationName |
| 2 | 12 | kCRThemeScaleName |
| 3 | 15 | kCRThemeIdiomName |
| 4 | 16 | kCRThemeSubtypeName |
| 5 | 9 | kCRThemeDimension2Name |
| 6 | 17 | kCRThemeIdentifierName |
| 7 | 1 | kCRThemeElementName |
| 8 | 2 | kCRThemePartName |

## 6. APPEARANCEKEYS / FACETKEYS / BITMAPKEYS

* APPEARANCEKEYS: one entry, key `UIAppearanceAny` (block 8, 15 bytes) → value block 9 = u16 LE 0.
* FACETKEYS: one entry, key = set name (block 10) → value block 11 (18 bytes, LE, a @0x34c0):
  `u16 hotspotX 0, u16 hotspotY 0, u16 nattrs 3, (u16 attr, u16 value)×3 = (1 Element, 85), (2 Part, 220), (17 Identifier, NameIdentifier)`.
  Bytes: `0000 0000 0300 0100 5500 0200 dc00 1100 c11a`.
* BITMAPKEYS: one entry, inline key = NameIdentifier → value block 24 (52 bytes, LE, a @0x3b080):
  `u32 1, u32 0, u32 40 (= byte length of the rest), u32 9 (= token count), i32 × 9`. Values, in
  KEYFORMAT order: a/b/c `[-1, 1, 2, 6, 1, 3, -1, -1, -1]`; d `[-1, 1, 2, 2, 1, 3, -1, -1, -1]`.
  Reading consistent with both data points (**semantics UNVERIFIED**): a bitmask of the attribute
  values present among this name's renditions (idiom {1,2} → 0b110, d: {1} → 0b010; scale {1} →
  0b10; dimension2 {0,1} → 0b11; localization/subtype {0} → 0b1) and -1 where not tracked
  (appearance, identifier, element, part).

## 7. Rendition keys (18 bytes = 9 × u16 LE, KEYFORMAT order)

| block (a) | Appearance | Localization | Scale | Idiom | Subtype | Dimension2 | Identifier | Element | Part | value |
|---|---|---|---|---|---|---|---|---|---|---|
| 13 @0x3510 | 0 | 0 | 1 | 2 (pad) | 0 | 0 | 6849 | 85 | 218 | MSI |
| 15 @0x3620 | 0 | 0 | 1 | 1 (phone) | 0 | 1 | 6849 | 85 | 220 | image |
| 17 @0x1eeb0 | 0 | 0 | 1 | 1 | 0 | 0 | 6849 | 85 | 218 | MSI |
| 19 @0x1efc0 | 0 | 0 | 1 | 2 | 0 | 1 | 6849 | 85 | 220 | image |

Bytes of key 15: `0000 0000 0100 0100 0000 0100 c11a 5500 dc00`. Dimension2 of the image (1) is
assetutil's "Icon Index" and equals the `index` in the MSIS entry. Idiom 1 = phone, 2 = pad
(assetutil names). d has only the two idiom-1 keys.

## 8. Rendition value = CSI header (184) + TLVs + payload

### 8.1 CSI header (LE)

| off | size | field | image (a block 16 @0x3640) | MSI (a block 14 @0x3530) |
|---|---|---|---|---|
| 0 | 4 | `CTSI` (bytes `ISTC`) | | |
| 4 | u32 | version | 1 | 1 |
| 8 | u32 | flags | **0** (assetutil still reports `Opaque: true`; source of that bit UNVERIFIED) | 0 |
| 12 | u32 | width | 1024 | 0 |
| 16 | u32 | height | 1024 | 0 |
| 20 | u32 | scaleFactor (×100) | 100 | 0 |
| 24 | u32 | pixelFormat | `ARGB` (bytes `BGRA`) | 0 |
| 28 | u32 | colorSpace (low 4 bits = id) | 1 (srgb) | 0 |
| 32 | u32 | modtime | 0 | 0 |
| 36 | u16 | layout | 0x0c (OnePartScale) | 0x3f2 (MultisizeImage) |
| 38 | u16 | 0 | 0 | 0 |
| 40 | char[128] | name | `AppIcon.png` (the PNG file name; unchanged in c) | set name (`AppIcon`; `Other` in c) |
| 168 | u32 | tlvLength | 104 | 28 |
| 172 | u32 | unknown | 1 | 1 |
| 176 | u32 | 0 | 0 | 0 |
| 180 | u32 | renditionLength = payload bytes | 112462 (b: 3171; coord: 1664621) | 24 |

Block length = 184 + tlvLength + renditionLength (= assetutil `SizeOnDisk`; assetutil's
"SHA1Digest" is the **SHA-256 of the whole value block**, verified on a/b/c).

### 8.2 TLVs (`u32 type, u32 length, bytes`)

Image (a @0x36f8..0x3760):

| type | len | bytes | meaning |
|---|---|---|---|
| 0x3e9 Slices | 20 | `01000000 00000000 00000000 00040000 00040000` | 1 slice: x 0, y 0, w 1024, h 1024 |
| 0x3eb Metrics | 28 | `01000000 00000000×4 00040000 00040000` | 1 metric: insets 0,0,0,0; size 1024×1024 |
| 0x3ec BlendModeAndOpacity | 8 | `00000000 0000803f` | blend 0, opacity 1.0f |
| 0x3ee EXIFOrientation | 4 | `01000000` | 1 |
| 0x3ef (name UNVERIFIED) | 4 | `00100000` | 4096 = width·4 = bytes per row in every golden |

MSI (a @0x35e8..0x3604): 0x3ec len 8 `00000000 00000000` (opacity **0.0f**), 0x3ee len 4 `01000000`.

### 8.3 Image payload: CELM v3 with CBCK chunks (a @0x3760)

```
u32 'CELM' (bytes MLEC)   u32 version = 3   u32 compression = 4 (lzfse)   u32 chunkCount = 4
chunk × 4: u32 'CBCK' (bytes KCBC)  u32 0  u32 0  u32 rows  u32 compressedLen  [compressedLen bytes: one complete LZFSE stream]
renditionLength = 16 + 4·20 + Σ compressedLen
```
rows = 341, 341, 341, 1 in every golden (341·4096 = 1,396,736 raw bytes per chunk; the last chunk
is the remaining 1 row). Why 341 (= ⌊1024/3⌋ or a ~1.4 MB byte budget) is **UNVERIFIED**; only
1024-row images exist in the goldens. Golden a chunk headers @0x3770 (23530), @0x936e (71929),
@0x1ac7b (16750), @0x1edfd (157). The two u32s after `CBCK` are 0 in all 24 chunks examined.

LZFSE framing: each chunk's stream is a plain LZFSE stream — `bvx2` block(s) then `bvx$` — nothing
wraps it. actool emits only `bvx2` (compressed-table) blocks; one block for compressible data, up
to 14 blocks of ~104 KB raw for the incompressible coord image (the reference encoder's own block
splitting). Apple's reader also accepts a `bvxn` (LZVN) block inside a chunk (the encoder lane's
output; decoded byte-exactly by liblzfse and by carspec-dump). Uncompressed/`bvx-` acceptance:
not tested here.

### 8.4 Pixel layout (read off `goldens/batch/out/coord`, pixel(x,y) = (R=x>>2, G=y>>2, B=(x^y)&255, A=255))

Decoded raw bytes begin `00 00 00 ff | 01 00 00 ff | 02 00 00 ff | 03 00 00 ff | 04 00 01 ff …`
and row 1 begins `01 00 00 ff | 00 00 00 ff | 03 00 00 ff | 02 00 00 ff`:

* byte order per pixel: **B, G, R, A** (the `ARGB` tag is the LE u32 view of that);
* rows **top-down**, stride = 4096 = width·4, **no padding**, total 4,194,304 bytes = 1024 rows;
* verified byte-for-byte against the input PNGs of coord, a and b (b decodes to `1e 1e c8 ff` =
  (200,30,30,255)).
* Premultiplication: **UNVERIFIED** — every golden is fully opaque. A coord image with a
  non-255 alpha gradient (e.g. A = x&255) would settle it and also show what assetutil's `Opaque`
  bit tracks.

### 8.5 MultiSized Image payload (MSIS, a @0x3604, 24 bytes)

`u32 'MSIS' (bytes SISM), u32 version 1, u32 count 1, entry { u32 width 1024, u32 height 1024, u32 index 1 }`.
These are assetutil entries 3 and 4 ("MultiSized Image", one per idiom, part 218, dimension2 0):
the per-idiom list of icon sizes available for this name; `index` 1 refers to the image rendition
whose Dimension2 (Icon Index) is 1. The rendition name is the set name, TLVs are only 0x3ec/0x3ee,
width/height/scale/pixelFormat/colorSpace are 0, and assetutil's `Scale: 1` comes from the key.

## 9. NameIdentifier hash (verified on all 56 distinct names / 58 points)

```
v = 100823                                   # 64-bit accumulator seed
for each byte c of the UTF-8 name: v = (v*33 + c) mod 2^64
h = 0
for i in 0..3: h = (h*33 + ((v >> 16*i) & 0xffff)) mod 2^16    # the four LE 16-bit words, low first
NameIdentifier = h
```
How it was found: per-position weights on the sweep are 33^(n−i+3) mod 2^16 (last char 33³) and
"Icon" vs "AAAA" leaves a residual of exactly 5·33² = five carries out of the low 16 bits, which is
what a Horner-33 fold over 16-bit words of a wider accumulator produces; the 32-bit variant fits
nothing, the 64-bit variant has exactly one seed in [0, 2³⁰) that fits all pair differences
(`carspec-scratch/carspec-hash-sweep-A.py`), and that seed reproduces every name with additive
constant 0. The seed 100823 is a fitted constant — no derivation from a known init (0/5381) was
found. UNVERIFIED beyond the data: names longer than 16 chars, non-ASCII names (assumed UTF-8
bytes). Proof table (observed = bytes of the facet value attribute 17, cross-checked with the
`NameIdentifier` in that golden's assetutil.json):

| name | len | id in bytes (facet attr 17) | assetutil NameIdentifier | computed | ok | golden (facet key offset, identifier offset) |
|---|---|---|---|---|---|---|
| `0` | 1 | 50233 (0xc439) | 50233 | 50233 | yes | `goldens/batch/out/h-0/Assets.car` (0x34b0, 0x34d0) |
| `1` | 1 | 20634 (0x509a) | 20634 | 20634 | yes | `goldens/batch/out/h-1/Assets.car` (0x34b0, 0x34d0) |
| `2` | 1 | 56571 (0xdcfb) | 56571 | 56571 | yes | `goldens/batch/out/h-2/Assets.car` (0x34b0, 0x34d0) |
| `3` | 1 | 26972 (0x695c) | 26972 | 26972 | yes | `goldens/batch/out/h-3/Assets.car` (0x34b0, 0x34d0) |
| `AA` | 2 | 64228 (0xfae4) | 64228 | 64228 | yes | `goldens/batch/out/h-AA/Assets.car` (0x34b0, 0x34d0) |
| `AAA` | 3 | 12049 (0x2f11) | 12049 | 12049 | yes | `goldens/batch/out/h-AAA/Assets.car` (0x34b0, 0x34d0) |
| `AAAA` | 4 | 14571 (0x38eb) | 14571 | 14571 | yes | `goldens/batch/out/h-AAAA/Assets.car` (0x34b0, 0x34d0) |
| `AAAAA` | 5 | 12659 (0x3173) | 12659 | 12659 | yes | `goldens/batch/out/h-AAAAA/Assets.car` (0x34b0, 0x34d0) |
| `AAAAAA` | 6 | 10611 (0x2973) | 10611 | 10611 | yes | `goldens/batch/out/h-AAAAAA/Assets.car` (0x34b0, 0x34d0) |
| `AAAAAAA` | 7 | 20227 (0x4f03) | 20227 | 20227 | yes | `goldens/batch/out/h-AAAAAAA/Assets.car` (0x34b0, 0x34d0) |
| `AAAAAAAA` | 8 | 22111 (0x565f) | 22111 | 22111 | yes | `goldens/batch/out/h-AAAAAAAA/Assets.car` (0x34b0, 0x34d0) |
| `AAB` | 3 | 47986 (0xbb72) | 47986 | 47986 | yes | `goldens/batch/out/h-AAB/Assets.car` (0x34b0, 0x34d0) |
| `AB` | 2 | 34629 (0x8745) | 34629 | 34629 | yes | `goldens/batch/out/h-AB/Assets.car` (0x34b0, 0x34d0) |
| `ABA` | 3 | 18322 (0x4792) | 18322 | 18322 | yes | `goldens/batch/out/h-ABA/Assets.car` (0x34b0, 0x34d0) |
| `AC` | 2 | 5030 (0x13a6) | 5030 | 5030 | yes | `goldens/batch/out/h-AC/Assets.car` (0x34b0, 0x34d0) |
| `AD` | 2 | 40967 (0xa007) | 40967 | 40967 | yes | `goldens/batch/out/h-AD/Assets.car` (0x34b0, 0x34d0) |
| `BA` | 2 | 4965 (0x1365) | 4965 | 4965 | yes | `goldens/batch/out/h-BA/Assets.car` (0x34b0, 0x34d0) |
| `BAA` | 3 | 22450 (0x57b2) | 22450 | 22450 | yes | `goldens/batch/out/h-BAA/Assets.car` (0x34b0, 0x34d0) |
| `CA` | 2 | 11238 (0x2be6) | 11238 | 11238 | yes | `goldens/batch/out/h-CA/Assets.car` (0x34b0, 0x34d0) |
| `F` | 1 | 54415 (0xd48f) | 54415 | 54415 | yes | `goldens/batch/out/h-F/Assets.car` (0x34b0, 0x34d0) |
| `G` | 1 | 24816 (0x60f0) | 24816 | 24816 | yes | `goldens/batch/out/h-G/Assets.car` (0x34b0, 0x34d0) |
| `H` | 1 | 60753 (0xed51) | 60753 | 60753 | yes | `goldens/batch/out/h-H/Assets.car` (0x34b0, 0x34d0) |
| `I` | 1 | 31154 (0x79b2) | 31154 | 31154 | yes | `goldens/batch/out/h-I/Assets.car` (0x34b0, 0x34d0) |
| `J` | 1 | 1555 (0x0613) | 1555 | 1555 | yes | `goldens/batch/out/h-J/Assets.car` (0x34b0, 0x34d0) |
| `K` | 1 | 37492 (0x9274) | 37492 | 37492 | yes | `goldens/batch/out/h-K/Assets.car` (0x34b0, 0x34d0) |
| `L` | 1 | 7893 (0x1ed5) | 7893 | 7893 | yes | `goldens/batch/out/h-L/Assets.car` (0x34b0, 0x34d0) |
| `M` | 1 | 43830 (0xab36) | 43830 | 43830 | yes | `goldens/batch/out/h-M/Assets.car` (0x34b0, 0x34d0) |
| `N` | 1 | 14231 (0x3797) | 14231 | 14231 | yes | `goldens/batch/out/h-N/Assets.car` (0x34b0, 0x34d0) |
| `O` | 1 | 50168 (0xc3f8) | 50168 | 50168 | yes | `goldens/batch/out/h-O/Assets.car` (0x34b0, 0x34d0) |
| `P` | 1 | 20569 (0x5059) | 20569 | 20569 | yes | `goldens/batch/out/h-P/Assets.car` (0x34b0, 0x34d0) |
| `Q` | 1 | 56506 (0xdcba) | 56506 | 56506 | yes | `goldens/batch/out/h-Q/Assets.car` (0x34b0, 0x34d0) |
| `R` | 1 | 26907 (0x691b) | 26907 | 26907 | yes | `goldens/batch/out/h-R/Assets.car` (0x34b0, 0x34d0) |
| `S` | 1 | 62844 (0xf57c) | 62844 | 62844 | yes | `goldens/batch/out/h-S/Assets.car` (0x34b0, 0x34d0) |
| `T` | 1 | 33245 (0x81dd) | 33245 | 33245 | yes | `goldens/batch/out/h-T/Assets.car` (0x34b0, 0x34d0) |
| `U` | 1 | 3646 (0x0e3e) | 3646 | 3646 | yes | `goldens/batch/out/h-U/Assets.car` (0x34b0, 0x34d0) |
| `V` | 1 | 39583 (0x9a9f) | 39583 | 39583 | yes | `goldens/batch/out/h-V/Assets.car` (0x34b0, 0x34d0) |
| `W` | 1 | 9984 (0x2700) | 9984 | 9984 | yes | `goldens/batch/out/h-W/Assets.car` (0x34b0, 0x34d0) |
| `X` | 1 | 45921 (0xb361) | 45921 | 45921 | yes | `goldens/batch/out/h-X/Assets.car` (0x34b0, 0x34d0) |
| `Y` | 1 | 16322 (0x3fc2) | 16322 | 16322 | yes | `goldens/batch/out/h-Y/Assets.car` (0x34b0, 0x34d0) |
| `Z` | 1 | 52259 (0xcc23) | 52259 | 52259 | yes | `goldens/batch/out/h-Z/Assets.car` (0x34b0, 0x34d0) |
| `a` | 1 | 41674 (0xa2ca) | 41674 | 41674 | yes | `goldens/batch/out/h-a/Assets.car` (0x34b0, 0x34d0) |
| `b` | 1 | 12075 (0x2f2b) | 12075 | 12075 | yes | `goldens/batch/out/h-b/Assets.car` (0x34b0, 0x34d0) |
| `c` | 1 | 48012 (0xbb8c) | 48012 | 48012 | yes | `goldens/batch/out/h-c/Assets.car` (0x34b0, 0x34d0) |
| `d` | 1 | 18413 (0x47ed) | 18413 | 18413 | yes | `goldens/batch/out/h-d/Assets.car` (0x34b0, 0x34d0) |
| `e` | 1 | 54350 (0xd44e) | 54350 | 54350 | yes | `goldens/batch/out/h-e/Assets.car` (0x34b0, 0x34d0) |
| `A` | 1 | 5802 (0x16aa) | 5802 | 5802 | yes | `goldens/batch/out/hU-A/Assets.car` (0x34b0, 0x34d0) |
| `B` | 1 | 41739 (0xa30b) | 41739 | 41739 | yes | `goldens/batch/out/hU-B/Assets.car` (0x34b0, 0x34d0) |
| `C` | 1 | 12140 (0x2f6c) | 12140 | 12140 | yes | `goldens/batch/out/hU-C/Assets.car` (0x34b0, 0x34d0) |
| `D` | 1 | 48077 (0xbbcd) | 48077 | 48077 | yes | `goldens/batch/out/hU-D/Assets.car` (0x34b0, 0x34d0) |
| `E` | 1 | 18478 (0x482e) | 18478 | 18478 | yes | `goldens/batch/out/hU-E/Assets.car` (0x34b0, 0x34d0) |
| `AppIcon` | 7 | 6849 (0x1ac1) | 6849 | 6849 | yes | `goldens/out/a/Assets.car` (0x34b0, 0x34d0) |
| `Other` | 5 | 8458 (0x210a) | 8458 | 8458 | yes | `goldens/out/c/Assets.car` (0x34b0, 0x34d0) |
| `AppIcon2` | 8 | 21817 (0x5539) | 21817 | 21817 | yes | `goldens/out/name-AppIcon2/Assets.car` (0x34b0, 0x34d0) |
| `Icon` | 4 | 44501 (0xadd5) | 44501 | 44501 | yes | `goldens/out/name-Icon/Assets.car` (0x34b0, 0x34d0) |
| `Squallar` | 8 | 3761 (0x0eb1) | 3761 | 3761 | yes | `goldens/out/name-Squallar/Assets.car` (0x34b0, 0x34d0) |
| `abcdefghijklmnop` | 16 | 15232 (0x3b80) | 15232 | 15232 | yes | `goldens/out/name-abcdefghijklmnop/Assets.car` (0x34b0, 0x34d0) |

## 10. Byte differentials

See `carspec-diff.md` (generated): a-vs-b differs only in BOM offsets, renditionLength, CBCK
compressedLen and the LZFSE bytes; a-vs-c only in the identifier (8 places), the name string
(3 places), the FACETKEYS keySize and two block-table lengths; a-vs-d is the removal of the two
pad renditions (20 blocks, masks idiom 0x6 → 0x2).

## 11. Beside the .car

actool also emits `<Set>60x60@2x.png` (120×120) and `<Set>76x76@2x~ipad.png` (152×152), RGBA with
sRGB+eXIf chunks, and a partial Info.plist (`CFBundleIcons`/`CFBundleIcons~ipad` →
`CFBundlePrimaryIcon` → `CFBundleIconFiles` [`<Set>60x60`, `<Set>76x76`], `CFBundleIconName` `<Set>`).
Not part of this spec beyond noting they exist.

## 12. Tool

```
python3 carspec-dump.py Assets.car                                    # full dump with offsets
python3 carspec-dump.py Assets.car --check-against assetutil.json --strict-actool --require-decode
python3 carspec-dump.py Assets.car --check-against <other compile>/assetutil.json --ignore-fields SHA1Digest,SizeOnDisk
python3 carspec-dump.py Assets.car --pixels-dir out/                  # BGRA raw + PNG per image rendition
```
The LZFSE/LZVN decoder inside it was validated byte-exactly against Apple's liblzfse
(`carspec-oracle/`, Go + blacktop/lzfse-cgo) on all 36 chunk streams of a, b, d, coord and the
encoder lane's output.
