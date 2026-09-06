#!/usr/bin/env bash
# Build a macOS flat installer package (.pkg) from a .app, on Linux.
#
#   mkpkg.sh <app-path> <bundle-id> <short-version> <bundle-version> <out.pkg>
#
# App Store Connect takes a macOS build as a .pkg and nothing else -- there is
# no .app or .zip spelling of a store upload the way there is an .ipa for iOS.
# Apple's tool for making one is `productbuild`, which is inside Xcode and is
# macOS-only, so this file is the reason the store channel can run in the same
# Linux container as everything else instead of needing a Mac in the loop.
#
# A flat package is a XAR archive with a fixed interior, and every part of it
# can be produced by a portable tool:
#
#     <archive>/
#       Distribution              XML: what the installer UI does
#       squallar.pkg/
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

if [ "$#" -ne 5 ]; then
    echo "usage: $0 <app-path> <bundle-id> <short-version> <bundle-version> <out.pkg>" >&2
    exit 2
fi

APP="$1"
BUNDLE_ID="$2"
SHORT_VERSION="$3"
BUNDLE_VERSION="$4"
OUT="$5"

[ -d "$APP" ] || { echo "ERROR: no such .app: $APP" >&2; exit 1; }

# Fail on the tool, by name, before doing any work. A half-built package is
# indistinguishable from a good one to everything downstream except Apple.
MISSING=
for t in xar mkbom cpio gzip; do
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
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# ---- payload root: the install location's contents, not the .app's ----
# The Payload is rooted AT the install location, so `./Squallar.app/...` under
# a root that means /Applications. Copying the bundle in rather than pointing
# cpio at it keeps the archive's paths relative and keeps the host's directory
# layout out of the package.
ROOT="$WORK/root"
mkdir -p "$ROOT"
cp -a "$APP" "$ROOT/$APP_NAME"

FLAT="$WORK/flat"
mkdir -p "$FLAT/$BUNDLE_ID.pkg"

# `--format odc` is the portable POSIX cpio format Apple's installer reads;
# the default `bin` format is byte-order dependent and is not it. `-n` keeps
# the gzip header free of a name and timestamp, so the same input produces the
# same bytes on every run.
( cd "$ROOT" && find . -print | cpio -o --format odc --quiet ) \
    | gzip -9 -n -c > "$FLAT/$BUNDLE_ID.pkg/Payload"

# `-u 0 -g 80`: root:admin, which is what /Applications content is owned by
# and what productbuild records. Without them mkbom writes the building
# user's ids, and the installer would apply them on the target machine.
mkbom -u 0 -g 80 "$ROOT" "$FLAT/$BUNDLE_ID.pkg/Bom"

NUM_FILES="$(find "$ROOT" | wc -l)"
INSTALL_KB="$(du -sk "$ROOT" | cut -f1)"

cat > "$FLAT/$BUNDLE_ID.pkg/PackageInfo" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<pkg-info overwrite-permissions="true" relocatable="false" identifier="$BUNDLE_ID" postinstall-action="none" version="$SHORT_VERSION" format-version="2" generator-version="squallar-mkpkg" auth="root" install-location="/Applications">
    <payload numberOfFiles="$NUM_FILES" installKBytes="$INSTALL_KB"/>
    <bundle-version>
        <bundle id="$BUNDLE_ID" CFBundleShortVersionString="$SHORT_VERSION" CFBundleVersion="$BUNDLE_VERSION" path="./$APP_NAME"/>
    </bundle-version>
</pkg-info>
EOF

# `customize="never"` and a hidden single choice: this installs one app to
# /Applications and has nothing to ask about. `hostArchitectures` names both
# slices because the bundle this wraps is universal.
cat > "$FLAT/Distribution" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<installer-gui-script minSpecVersion="1">
    <title>Squallar</title>
    <options customize="never" require-scripts="false" hostArchitectures="arm64,x86_64"/>
    <choices-outline>
        <line choice="default">
            <line choice="$BUNDLE_ID"/>
        </line>
    </choices-outline>
    <choice id="default"/>
    <choice id="$BUNDLE_ID" visible="false">
        <pkg-ref id="$BUNDLE_ID"/>
    </choice>
    <pkg-ref id="$BUNDLE_ID" version="$SHORT_VERSION" installKBytes="$INSTALL_KB">#$BUNDLE_ID.pkg</pkg-ref>
</installer-gui-script>
EOF

# `--compression none` on the outer archive: the Payload inside is already
# gzipped, and Apple's own product archives leave the outer XAR uncompressed.
# Distribution is named first so it lands first in the table of contents,
# which is where the installer looks for it.
mkdir -p "$(dirname "$OUT")"
OUT_ABS="$(cd "$(dirname "$OUT")" && pwd)/$(basename "$OUT")"
( cd "$FLAT" && xar --compression none -cf "$OUT_ABS" Distribution "$BUNDLE_ID.pkg" )

echo "    .pkg: $OUT_ABS"
echo "    payload: $NUM_FILES entries, ${INSTALL_KB} KiB installed"
