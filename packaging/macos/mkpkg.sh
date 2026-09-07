#!/usr/bin/env bash
# Build a macOS product archive (.pkg) from a .app, on Linux, in the shape
# `productbuild` writes.
#
#   mkpkg.sh <app-path> <bundle-id> <short-version> <bundle-version> <min-os> <out.pkg>
#
# App Store Connect takes a macOS build as a .pkg and nothing else -- there is
# no .app or .zip spelling of a store upload the way there is an .ipa for iOS.
# Apple's tool for making one is `productbuild`, which is inside Xcode and is
# macOS-only, so this file is the reason the store channel can run in the same
# Linux container as everything else instead of needing a Mac in the loop.
#
# A product archive is a XAR archive with a fixed interior, and every part of
# it can be produced by a portable tool:
#
#     <archive>/
#       Distribution              XML: product identity, OS floor, installer UI
#       <bundle-id>.pkg/
#         PackageInfo             XML: identifier, version, install location
#         Payload                 the .app as a gzipped cpio (odc) archive
#         Bom                     bill of materials -- path, mode, uid/gid, size
#
#   * Payload  -- `cpio` and `gzip`, both already in the image.
#   * Bom      -- `mkbom` from bomutils. macOS ships mkbom as a closed-source
#                 developer tool; bomutils is an open reimplementation of the
#                 format and builds from source with make + g++ + libxml2.
#   * the XAR  -- `xar`. Not packaged by Debian (measured: `apt-cache policy
#                 xar` is empty on bookworm), so CI builds mackyle/xar from
#                 source. Its configure probes libcrypto for
#                 `OpenSSL_add_all_ciphers`, which OpenSSL 3 turned into a
#                 macro, so the probe fails on a modern distro and the build
#                 stops with "Cannot build without libcrypto (OpenSSL)".
#                 Passing the cache variable
#                 `ac_cv_lib_crypto_OpenSSL_add_all_ciphers=yes` skips the
#                 probe; the source then compiles against the header macro and
#                 links fine. Measured on debian:bookworm-slim: xar 1.6.1.
#
# The two XML files are written field-for-field from a golden: `productbuild
# --component Stub.app /Applications --product req.plist` run on macOS 26.5
# (25E253) on 2026-09-07, with `os = [11.0]` in the product definition. App
# Store Connect reads them and refuses what it does not recognise, and the
# first archive this script wrote -- a hand-shaped Distribution with a
# generator-version of its own -- was refused four ways at once, each one a
# field the golden has and it did not:
#
#   90270 "Unsupported toolchain ... must be created either through Xcode, or
#         using productbuild"    -> PackageInfo generator-version. That
#         attribute is the only toolchain identity in the whole archive;
#         PackageMaker wrote its own name there and productbuild writes
#         "InstallCmds-<ver> (<macOS build>)". It is not a check on which
#         machine ran; it is a check on this string.
#   90230 "Invalid product archive metadata. Error in keyPath
#         [product-metadata.product-identifier]" and "[...product-version]"
#                                -> the Distribution's <product id= version=/>.
#   90264 "The lowest minimum system version in the product definition
#         property list, none, must equal the LSMinimumSystemVersion value"
#                                -> <volume-check><allowed-os-versions>
#                                   <os-version min=/>. An EQUALITY check,
#                                   so <min-os> is handed in by the Makefile
#                                   from the same number it wrote into
#                                   LSMinimumSystemVersion.
#
# This script does NOT sign. `rcodesign sign` signs a XAR pkg installer
# directly -- it prints "signing XAR pkg installer" and rewrites the table of
# contents -- and the Makefile does that afterwards with the Mac Installer
# Distribution certificate, so the signature is applied to the finished
# archive rather than to something this script would have to keep valid.
#
# The .app handed in must ALREADY be signed and must already carry
# Contents/embedded.provisionprofile. A pkg is a container: signing it does
# not sign the app inside it, and Apple validates both.

set -euo pipefail

if [ "$#" -ne 6 ]; then
    echo "usage: $0 <app-path> <bundle-id> <short-version> <bundle-version> <min-os> <out.pkg>" >&2
    exit 2
fi

APP="$1"
BUNDLE_ID="$2"
SHORT_VERSION="$3"
BUNDLE_VERSION="$4"
MIN_OS="$5"
OUT="$6"

# What productbuild stamps as its own identity. Measured, not invented: this
# is the value in the golden above, from the InstallCmds framework of the
# macOS build that ran it. Override with MKPKG_GENERATOR to claim another.
GENERATOR="${MKPKG_GENERATOR:-InstallCmds-864.1 (25E253)}"

[ -d "$APP" ] || { echo "ERROR: no such .app: $APP" >&2; exit 1; }
case "$MIN_OS" in
    [0-9]*.[0-9]*) ;;
    *) echo "ERROR: <min-os> must look like 11.0, got '$MIN_OS'" >&2; exit 1 ;;
esac

MISSING=
for t in xar mkbom cpio gzip od; do
    command -v "$t" >/dev/null 2>&1 || MISSING="$MISSING $t"
done
if [ -n "$MISSING" ]; then
    echo "ERROR: missing tool(s) for building a .pkg:$MISSING" >&2
    echo "       xar   -> build mackyle/xar from source with" >&2
    echo "                ./configure ac_cv_lib_crypto_OpenSSL_add_all_ciphers=yes" >&2
    echo "       mkbom -> build hogliux/bomutils from source (make + g++ + libxml2-dev)" >&2
    echo "       Neither is packaged by Debian. See .github/workflows/build.yaml." >&2
    exit 1
fi

APP_NAME="$(basename "$APP")"
EXE="$APP/Contents/MacOS/$(basename "$APP" .app)"
[ -f "$EXE" ] || { echo "ERROR: no executable at $EXE" >&2; exit 1; }

# `hostArchitectures` is read off the executable's magic rather than assumed:
# a fat binary carries both slices, a thin one carries the slice its header
# names. productbuild lists them x86_64 first.
MAGIC="$(od -An -tx1 -N8 "$EXE" | tr -d ' \n')"
case "$MAGIC" in
    cafebabe*)          HOST_ARCHES="x86_64,arm64" ;;
    cffaedfe0c000001*)  HOST_ARCHES="arm64" ;;
    cffaedfe07000001*)  HOST_ARCHES="x86_64" ;;
    *) echo "ERROR: $EXE does not start with a Mach-O or fat magic (got $MAGIC)" >&2; exit 1 ;;
esac

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

ROOT="$WORK/root"
mkdir -p "$ROOT"
cp -a "$APP" "$ROOT/$APP_NAME"

FLAT="$WORK/flat"
mkdir -p "$FLAT/$BUNDLE_ID.pkg"

# Payload: odc cpio of the install root, gzipped. `-n` keeps gzip from
# stamping the time, so two builds of one tree are one file.
( cd "$ROOT" && find . -print | cpio -o --format odc --quiet ) \
    | gzip -9 -n -c > "$FLAT/$BUNDLE_ID.pkg/Payload"

# `-u 0 -g 80`: root:admin, which is what /Applications content is owned by
# and what productbuild records. Without them mkbom writes the building
# user's ids, and the installer would apply them on the target machine.
mkbom -u 0 -g 80 "$ROOT" "$FLAT/$BUNDLE_ID.pkg/Bom"

# productbuild counts every entry under the root and not the root itself
# (golden: a six-entry stub reads numberOfFiles="6"), and its installKBytes
# is the file bytes over 1024 rounded down (the same stub, ~700 bytes, reads
# installKBytes="0"; `du -sk` would say 8 or 12 for it, in blocks).
NUM_FILES="$(find "$ROOT" -mindepth 1 | wc -l)"
INSTALL_KB="$(find "$ROOT" -type f -printf '%s\n' | awk '{ s += $1 } END { printf "%d", s / 1024 }')"

printf '%s' "$(cat <<XML
<?xml version="1.0" encoding="utf-8"?>
<pkg-info overwrite-permissions="true" relocatable="false" identifier="$BUNDLE_ID" postinstall-action="none" version="$SHORT_VERSION" format-version="2" generator-version="$GENERATOR" install-location="/Applications" auth="root" preserve-xattr="true">
    <payload numberOfFiles="$NUM_FILES" installKBytes="$INSTALL_KB"/>
    <bundle path="./$APP_NAME" id="$BUNDLE_ID" CFBundleShortVersionString="$SHORT_VERSION" CFBundleVersion="$BUNDLE_VERSION"/>
    <bundle-version>
        <bundle id="$BUNDLE_ID"/>
    </bundle-version>
    <upgrade-bundle>
        <bundle id="$BUNDLE_ID"/>
    </upgrade-bundle>
    <update-bundle/>
    <atomic-update-bundle/>
    <strict-identifier>
        <bundle id="$BUNDLE_ID"/>
    </strict-identifier>
    <relocate>
        <bundle id="$BUNDLE_ID"/>
    </relocate>
</pkg-info>
XML
)" > "$FLAT/$BUNDLE_ID.pkg/PackageInfo"

# `customize="never"` and a hidden single choice: this installs one app to
# /Applications and has nothing to ask about. Both files go out without a
# trailing newline, as productbuild writes them; `$(...)` strips it.
printf '%s' "$(cat <<XML
<?xml version="1.0" encoding="utf-8"?>
<installer-gui-script minSpecVersion="2">
    <pkg-ref id="$BUNDLE_ID">
        <bundle-version>
            <bundle CFBundleShortVersionString="$SHORT_VERSION" CFBundleVersion="$BUNDLE_VERSION" id="$BUNDLE_ID" path="$APP_NAME"/>
        </bundle-version>
    </pkg-ref>
    <product id="$BUNDLE_ID" version="$SHORT_VERSION"/>
    <title>Squallar</title>
    <options customize="never" require-scripts="false" hostArchitectures="$HOST_ARCHES"/>
    <volume-check>
        <allowed-os-versions>
            <os-version min="$MIN_OS"/>
        </allowed-os-versions>
    </volume-check>
    <choices-outline>
        <line choice="default">
            <line choice="$BUNDLE_ID"/>
        </line>
    </choices-outline>
    <choice id="default" title="Squallar" versStr="$SHORT_VERSION"/>
    <choice id="$BUNDLE_ID" title="Squallar" visible="false" customLocation="/Applications">
        <pkg-ref id="$BUNDLE_ID"/>
    </choice>
    <pkg-ref id="$BUNDLE_ID" version="$SHORT_VERSION" onConclusion="none" installKBytes="$INSTALL_KB" updateKBytes="0">#$BUNDLE_ID.pkg</pkg-ref>
</installer-gui-script>
XML
)" > "$FLAT/Distribution"

# `--compression none` on the outer archive. productbuild's own archive
# gzip-encodes Distribution and PackageInfo and stores Payload as-is (it is
# already gzip); xar compresses every member or none, and a second gzip over
# the Payload is the one that would cost. Distribution is named first so it
# lands first in the table of contents, which is where the installer looks.
mkdir -p "$(dirname "$OUT")"
OUT_ABS="$(cd "$(dirname "$OUT")" && pwd)/$(basename "$OUT")"
( cd "$FLAT" && xar --compression none -cf "$OUT_ABS" Distribution "$BUNDLE_ID.pkg" )

# Read the archive back. This proves the XAR holds the two XML files as
# written, and that the OS floor inside it is the one handed in -- the
# equality App Store Connect checks against LSMinimumSystemVersion (90264).
CHECK="$WORK/check"
mkdir -p "$CHECK"
( cd "$CHECK" && xar -xf "$OUT_ABS" )
cmp -s "$FLAT/Distribution" "$CHECK/Distribution" \
    || { echo "ERROR: the Distribution read back from $OUT_ABS is not the one written." >&2; exit 1; }
cmp -s "$FLAT/$BUNDLE_ID.pkg/PackageInfo" "$CHECK/$BUNDLE_ID.pkg/PackageInfo" \
    || { echo "ERROR: the PackageInfo read back from $OUT_ABS is not the one written." >&2; exit 1; }
grep -q "<os-version min=\"$MIN_OS\"/>" "$CHECK/Distribution" \
    || { echo "ERROR: $OUT_ABS carries no <os-version min=\"$MIN_OS\"/>." >&2; exit 1; }
grep -q "<product id=\"$BUNDLE_ID\" version=\"$SHORT_VERSION\"/>" "$CHECK/Distribution" \
    || { echo "ERROR: $OUT_ABS carries no <product id=\"$BUNDLE_ID\" version=\"$SHORT_VERSION\"/>." >&2; exit 1; }

echo "    .pkg: $OUT_ABS"
echo "    product: $BUNDLE_ID $SHORT_VERSION ($BUNDLE_VERSION), macOS >= $MIN_OS, $HOST_ARCHES, generator $GENERATOR"
echo "    payload: $NUM_FILES entries, ${INSTALL_KB} KiB installed"
