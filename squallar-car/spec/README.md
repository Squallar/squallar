# What this directory is

The byte-level record of how the `.car` format was recovered, kept beside the
encoder so a future change can be checked against the same evidence rather
than against memory.

- `SPEC.md` — every structure, with the reference catalog and byte offset each
  value was read from, and the items still marked UNVERIFIED.
- `goldens-diff.md` — what changed between reference catalogs that differed in
  exactly one input (pixels only; set name only; iPad idiom only), attributed
  to block and field. This is what separates structure from payload.
- `dump.py` — a standalone, stdlib-only decoder that is NOT this crate: it
  parses a catalog independently, decompresses the renditions with its own
  LZFSE/LZVN (validated byte-exactly against Apple's liblzfse), and with
  `--check-against <assetutil.json>` asserts it sees what Apple's own reader
  saw. It is the second opinion on this crate's output.

The reference catalogs themselves are not committed: they are compiled by
Apple's `actool` and read by Apple's `assetutil`, both inside Xcode. To
regenerate them, stage `Assets.xcassets/<Set>.appiconset/{AppIcon.png (1024),
Contents.json}` and run, on a Mac:

    xcrun actool Assets.xcassets --compile out --platform iphoneos \
      --minimum-deployment-target 15.0 --app-icon <Set> \
      --output-partial-info-plist out/partial.plist \
      --target-device iphone --target-device ipad
    xcrun assetutil --info out/Assets.car > out/assetutil.json

Then:

    python3 squallar-car/spec/dump.py out/Assets.car --check-against out/assetutil.json --strict-actool --require-decode

The same command over this crate's output, `--ignore-fields SHA1Digest,SizeOnDisk`
(the compressed bytes legitimately differ), is the check that was passing when
the encoder landed. `assetutil --info` never decodes pixels, so a stream can be
corrupt and still read as fine there; `--require-decode` is what catches that.
