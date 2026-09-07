# carspec-diff: byte-range differentials between goldens

Generated from the bytes by `carspec-diff` logic (ranges merged when separated by < 8 equal bytes); each range is attributed to the BOM block it falls in, with the offset inside that block.

Goldens: a = set AppIcon, real icon, iphone+ipad (244056 B); b = same but flat red (25496 B); c = set 'Other' (244056 B); d = iphone only (130968 B).


## a vs b

a vs b — differs ONLY in the pixel payload: the bytes that change are the file-size-dependent BOM header fields, renditionLength, the CBCK compressedLen and the LZFSE streams. Everything else (all structure, names, keys, TLVs, chunk row counts 341/341/341/1) is byte-identical.

| a offset range | bytes | a | b | region in a |
|---|---|---|---|---|
| 0x000011..0x00001c | 11 | `03b140000008180003b0c0` | `005b800000081800005b00` | BOM header |
| 0x0036f4..0x0036f7 | 3 | `4eb701` | `630c00` | block 16 @0x3640 (rendition value (Idiom=1, Dimension2=1, Part=220)) +180 |
| 0x003780..0x003782 | 2 | `ea5b` | `cd03` | block 16 @0x3640 (rendition value (Idiom=1, Dimension2=1, Part=220)) +320 |
| 0x00378c..0x006398 | 11276 | `5819704301011d6052500000f1e94620`… | `080020000053023000000028f0440360`… | block 16 @0x3640 (rendition value (Idiom=1, Dimension2=1, Part=220)) +332 |

(lengths differ: a 244056 B, b 25496 B; ranges beyond the shorter file are not listed)

## a vs c

a vs c — differs ONLY in the set name (AppIcon -> Other): every occurrence of the NameIdentifier 0x1ac1 -> 0x210a (rendition keys, facet value, inline key copy in the RENDITIONS leaf, BITMAPKEYS inline key), the facet key string (block and inline copy), the MSIS rendition name (the image rendition name AppIcon.png is the PNG file name and does NOT change), the FACETKEYS tree keySize (7 -> 5), and the two block-table lengths of the facet key block and the FACETKEYS leaf block.

| a offset range | bytes | a | c | region in a |
|---|---|---|---|---|
| 0x00041c..0x00041e | 2 | `c11a` | `0a21` | block 3 @0x3e0 (RENDITIONS leaf (paths) block) +60 |
| 0x00042e..0x000430 | 2 | `c11a` | `0a21` | block 3 @0x3e0 (RENDITIONS leaf (paths) block) +78 |
| 0x000440..0x000442 | 2 | `c11a` | `0a21` | block 3 @0x3e0 (RENDITIONS leaf (paths) block) +96 |
| 0x000452..0x000454 | 2 | `c11a` | `0a21` | block 3 @0x3e0 (RENDITIONS leaf (paths) block) +114 |
| 0x001448..0x001449 | 1 | `07` | `05` | block 4 @0x1430 (FACETKEYS) +24 |
| 0x001468..0x00146f | 7 | `41707049636f6e` | `4f746865720000` | block 5 @0x1450 (FACETKEYS leaf (paths) block) +24 |
| 0x0034b0..0x0034b7 | 7 | `41707049636f6e` | `4f746865720000` | block 10 @0x34b0 (facet key) +0 |
| 0x0034d0..0x0034d2 | 2 | `c11a` | `0a21` | block 11 @0x34c0 (facet value) +16 |
| 0x00351c..0x00351e | 2 | `c11a` | `0a21` | block 13 @0x3510 (rendition key (Idiom=2, Dimension2=0, Part=218)) +12 |
| 0x003558..0x00355f | 7 | `41707049636f6e` | `4f746865720000` | block 14 @0x3530 (rendition value (Idiom=2, Dimension2=0, Part=218)) +40 |
| 0x00362c..0x00362e | 2 | `c11a` | `0a21` | block 15 @0x3620 (rendition key (Idiom=1, Dimension2=1, Part=220)) +12 |
| 0x01eebc..0x01eebe | 2 | `c11a` | `0a21` | block 17 @0x1eeb0 (rendition key (Idiom=1, Dimension2=0, Part=218)) +12 |
| 0x01eef8..0x01eeff | 7 | `41707049636f6e` | `4f746865720000` | block 18 @0x1eed0 (rendition value (Idiom=1, Dimension2=0, Part=218)) +40 |
| 0x01efcc..0x01efce | 2 | `c11a` | `0a21` | block 19 @0x1efc0 (rendition key (Idiom=2, Dimension2=1, Part=220)) +12 |
| 0x03ac92..0x03ac94 | 2 | `1ac1` | `210a` | block 23 @0x3ac80 (BITMAPKEYS leaf (paths) block) +18 |
| 0x03b173..0x03b174 | 1 | `07` | `05` | block table entry 5 length |
| 0x03b19b..0x03b19c | 1 | `07` | `05` | block table entry 10 length |

## a vs d-iphone-only

a vs d — iphone only: 2 renditions instead of 4, so 20 blocks instead of 24; everything after the RENDITIONS leaf shifts. Listed for completeness; read carspec-SPEC.md section on block order for the structural reading (the pad MSI/image pairs are simply absent, BITMAPKEYS idiom mask 0x6 -> 0x2).

| a offset range | bytes | a | d-iphone-only | region in a |
|---|---|---|---|---|
| 0x00000f..0x00001c | 13 | `180003b140000008180003b0c0` | `140001f780000008180001f700` | BOM header |
| 0x000210..0x000211 | 1 | `04` | `02` | block 1 @0x200 (CARHEADER) +16 |
| 0x0003d3..0x0003d4 | 1 | `04` | `02` | block 2 @0x3c0 (RENDITIONS) +19 |
| 0x0003e3..0x0003e4 | 1 | `04` | `02` | block 3 @0x3e0 (RENDITIONS leaf (paths) block) +3 |
| 0x0003ef..0x000457 | 104 | `1200000011000000100000000f000000`… | `100000000f0000000e0000000d000000`… | block 3 @0x3e0 (RENDITIONS leaf (paths) block) +15 |
| 0x001410..0x001454 | 68 | `00000000000000000000000000000000`… | `74726565000000010000000500001000`… | block 3 @0x3e0 (RENDITIONS leaf (paths) block) +4144 |
| 0x00145f..0x00146f | 16 | `0b0000000a0000000041707049636f6e` | `00000000000000000000000000000000` | block 5 @0x1450 (FACETKEYS leaf (paths) block) +15 |
| 0x002440..0x002487 | 71 | `00000000000000000000000000000000`… | `74726565000000010000000700001000`… | block 5 @0x1450 (FACETKEYS leaf (paths) block) +4080 |
| 0x00248f..0x0024a7 | 24 | `09000000080000000055494170706561`… | `00000000000000000000000000000000`… | block 7 @0x2480 (APPEARANCEKEYS leaf (paths) block) +15 |
| 0x003470..0x00347f | 15 | `000000000000000000000000000000` | `5549417070656172616e6365416e79` | block 7 @0x2480 (APPEARANCEKEYS leaf (paths) block) +4080 |
| 0x003490..0x0034b7 | 39 | `5549417070656172616e6365416e7900`… | `41707049636f6e000000000000000000`… | block 8 @0x3490 (appearance key) +0 |
| 0x0034c0..0x003543 | 131 | `000000000300010055000200dc001100`… | `746d666b000000000900000007000000`… | block 11 @0x34c0 (facet value) +0 |
| 0x003554..0x00355f | 11 | `f203000041707049636f6e` | `0000000000000000000000` | block 14 @0x3530 (rendition value (Idiom=2, Dimension2=0, Part=218)) +36 |
| 0x0035b8..0x0035ed | 53 | `00000000000000000000000000000000`… | `6800000001000000000000004eb70100`… | block 14 @0x3530 (rendition value (Idiom=2, Dimension2=0, Part=218)) +136 |
| 0x0035f8..0x01ee01 | 112649 | `ee03000004000000010000005349534d`… | `00000000000000000004000000040000`… | block 14 @0x3530 (rendition value (Idiom=2, Dimension2=0, Part=218)) +200 |
| 0x01ee09..0x01ee5f | 86 | `010000009d0000006276783200100000`… | `00000000000000000000000000000000`… | block 16 @0x3640 (rendition value (Idiom=1, Dimension2=1, Part=220)) +112585 |
| 0x01ee68..0x01ee9a | 50 | `00000000000000000000000000000000`… | `ee03000004000000010000005349534d`… | block 16 @0x3640 (rendition value (Idiom=1, Dimension2=1, Part=220)) +112680 |
| 0x01eea5..0x01eec1 | 28 | `a8b0f2ff016276782400000000000001`… | `00000000000000000000000000000000`… | block 16 @0x3640 (rendition value (Idiom=1, Dimension2=1, Part=220)) +112741 |
| 0x01eed0..0x01eed5 | 5 | `4953544301` | `0000000000` | block 18 @0x1eed0 (rendition value (Idiom=1, Dimension2=0, Part=218)) +0 |
| 0x01eef4..0x01eeff | 11 | `f203000041707049636f6e` | `0000000000000000000000` | block 18 @0x1eed0 (rendition value (Idiom=1, Dimension2=0, Part=218)) +36 |
| 0x01ef78..0x01efb9 | 65 | `1c000000010000000000000018000000`… | `00000000000000000000000000000000`… | block 18 @0x1eed0 (rendition value (Idiom=1, Dimension2=0, Part=218)) +168 |
| 0x01efc4..0x01efd1 | 13 | `0100020000000100c11a5500dc` | `00000000000000000000000000` | block 19 @0x1efc0 (rendition key (Idiom=2, Dimension2=1, Part=220)) +4 |
| 0x01efe0..0x01efe5 | 5 | `4953544301` | `0000000000` | block 20 @0x1efe0 (rendition value (Idiom=2, Dimension2=1, Part=220)) +0 |
| 0x01efed..0x01f013 | 38 | `04000000040000640000004247524101`… | `00000000000000000000000000000000`… | block 20 @0x1efe0 (rendition value (Idiom=2, Dimension2=1, Part=220)) +13 |
| 0x01f088..0x01f0a1 | 25 | `6800000001000000000000004eb70100`… | `000000000000000000000000696f7300`… | block 20 @0x1efe0 (rendition value (Idiom=2, Dimension2=1, Part=220)) +168 |
| 0x01f0ad..0x01f0bd | 16 | `04000000040000eb0300001c00000001` | `00000000000000000000000000000000` | block 20 @0x1efe0 (rendition value (Idiom=2, Dimension2=1, Part=220)) +205 |
| 0x01f0d1..0x01f0dd | 12 | `04000000040000ec03000008` | `000000000000000000000000` | block 20 @0x1efe0 (rendition value (Idiom=2, Dimension2=1, Part=220)) +241 |
| 0x01f0e6..0x01f114 | 46 | `803fee0300000400000001000000ef03`… | `00000000000000000000000000000000`… | block 20 @0x1efe0 (rendition value (Idiom=2, Dimension2=1, Part=220)) +262 |
| 0x01f11c..0x01ff98 | 3708 | `55010000ea5b00006276783200501500`… | `00000000000000000000000000000000`… | block 20 @0x1efe0 (rendition value (Idiom=2, Dimension2=1, Part=220)) +316 |

(lengths differ: a 244056 B, d-iphone-only 130968 B; ranges beyond the shorter file are not listed)
