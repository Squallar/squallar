# macOS app build, run entirely on Linux inside the rust-cross container:
#
#   make -C packaging/macos                    arm64  (aarch64-apple-darwin), release
#   make -C packaging/macos ARCH=x86_64        Intel  (x86_64-apple-darwin),  release
#   make -C packaging/macos DEBUG=1            debug profile
#   make -C packaging/macos BIN=path/to/exe    package a binary that already exists
#   make -C packaging/macos ARCHIVE=1          also produce squallar.app.zip
#
# `ARCHIVE` and not the obvious `ZIP`: a variable set on make's command line is
# exported into the recipe's environment, and Info-ZIP's `zip` reads `ZIP` (and
# `ZIPOPT`) as a list of default options to prepend to its own argv. `ZIP=1`
# therefore turns `zip -qry squallar.app.zip squallar.app` into
# `zip 1 -qry squallar.app.zip squallar.app`, which succeeds, and writes the
# archive to `1.zip`. Found by running it.
#
# No Apple tools and no macOS host are involved: the .app is assembled with
# cp/sed, and signed with `rcodesign`, which is a Rust program.
#
# CI already runs *inside* the image, so it sets IN_CONTAINER=1 and the recipe
# executes directly instead of nesting a second container.
#
# This mirrors packaging/ios/Makefile, with three differences that are not stylistic:
#
#   * The layout is not flat. A macOS bundle is
#     Contents/{Info.plist, MacOS/squallar, Resources/, PkgInfo}; an iOS one puts
#     all of that at the top level.
#   * There is no ObjC shell. winit's AppKit backend brings up NSApplication
#     itself, so `squallar`'s own `main` is the executable and there is
#     nothing to compile or link by hand.
#   * Signing is not optional. arm64 macOS refuses to execute an unsigned
#     Mach-O at all, and — the reason this file exists — `locationd` keys its
#     permission grant to the code signature, so an app whose identity changes
#     every build is an app that re-prompts every build.
#
# ── The signing identity ────────────────────────────────────────────────
#
# macOS remembers "this app may use your location" against the app's
# *designated requirement*. Signed with a certificate, that requirement pins
# the signing leaf, so every build signed with the same certificate is the same
# application and the grant survives. Signed ad-hoc, it pins the code hash
# instead, so every rebuild is a new application and the user is asked again.
#
# Make one certificate, once, and keep using it:
#
#   rcodesign generate-self-signed-certificate \
#       --algorithm rsa \
#       --person-name squallar --validity-days 3650 \
#       --p12-file squallar-signing.p12 --p12-password "$$PW"
#
# `--algorithm rsa` is load-bearing and is checked below. Apple's
# Security.framework verifies only RSA and ECDSA code signatures; it has no
# Ed25519 code-signing path at all. An Ed25519 key still *signs* — rcodesign
# emits a well-formed CodeDirectory, binds the Info.plist and seals resources
# exactly as it does for RSA — but macOS cannot verify the CMS blob, so it
# reports the whole signature as "invalid signature (code or signature have
# been modified)" and `syspolicy_check` calls it a Fatal codesign error. That
# is strictly worse than the ad-hoc fallback: a merely-untrusted app is
# "unidentified developer", which the user can override with right-click →
# Open, whereas an unverifiable one reads as damaged and cannot be overridden
# at all. Measured on macOS 26.4.1/arm64 against the real CI artifact: an
# Ed25519-signed bundle failed `codesign --verify --deep --strict`, and the
# byte-identical bundle re-signed RSA (and ECDSA) passed.
#
# Then either point this Makefile at it —
#
#   make -C packaging/macos P12_FILE=../squallar-signing.p12 P12_PASSWORD="$$PW"
#
# — or, for CI, put `base64 -w0 squallar-signing.p12` in the repository secret
# `MACOS_SIGNING_P12` and the password in `MACOS_SIGNING_P12_PASSWORD`; this
# recipe reads both from the environment. Those secrets do not exist until
# somebody creates them, and no workflow can create them.
#
# `--validity-days 3650`, not the 365 default: an expiry forces a new
# certificate, and a new certificate costs every user one more prompt.
#
# With no certificate at all this falls back to an ad-hoc signature, which is
# a real signature (so the app launches) with an unstable identity (so it
# re-prompts). That is the right fallback for a fork pull request, which never
# has access to the secret — but it is labelled in the log, not silent.
#
# ── Notarization: the part that actually removes the warning ────────────
#
# A signature is not what makes another person's Mac open the app. Three
# things have to hold together, and any two of them still produce a refusal:
#
#   1. a Developer ID Application certificate ISSUED BY APPLE -- a self-signed
#      certificate is not one, however well formed;
#   2. the hardened runtime flag on every Mach-O in the bundle; and
#   3. a notarization ticket from Apple, stapled into the .app.
#
# Measured on macOS 26.4.1/arm64 against bundles this recipe produced:
# `spctl -a -t exec` rejected the ad-hoc bundle, rejected the
# self-signed-certificate bundle, and rejected both again with the hardened
# runtime flag set. Nothing short of 1+2+3 was accepted. Note which direction
# that cuts: an un-notarized Developer ID app is the one macOS reports as
# "Apple could not verify ... free of malware", which is a worse dialog than
# the unsigned one, not a better one.
#
# Order is forced, and cannot be rearranged:
#
#   sign (--for-notarization) -> notary-submit -> staple -> zip
#
# `rcodesign staple` refuses a .zip outright ("do not know how to staple") and
# refuses a bare Mach-O ("cannot staple Mach-O binaries") -- both measured with
# 0.29.0 -- so the ticket goes into the .app directory and the zip is made
# afterwards, around the stapled bundle. A .dmg can also carry a ticket, but
# building one needs Apple's hdiutil, i.e. a Mac, so it is not available to
# this Linux-only pipeline. The upload does not need a zip made here either:
# `notary-submit` given a bundle path zips it itself for the upload and staples
# the directory afterwards.
#
# Stapling is what makes the ticket work OFFLINE. A notarized-but-unstapled app
# still passes Gatekeeper on a machine that can reach Apple, and fails on one
# that cannot; that difference is the whole reason the zip is built last.
#
# ── One name means shippable, every other outcome means not ─────────────
#
# `squallar.app.zip` is written only when a ticket was stapled. Every other
# outcome -- no notary credentials, an ad-hoc signature, Apple unreachable,
# Apple saying no -- writes `squallar-NOT-NOTARIZED.app.zip` instead, and a
# one-word reason lands in `NOTARIZATION` beside it. The reason for making the
# degraded build LOUD rather than making it fail is set out in full at the
# notarize stage further down: a notary outage is not evidence about this
# repository's code and must not stop everything that gates on a green board,
# but a build that is quietly indistinguishable from a good one is how "we
# ship notarized releases" stops being true without anybody deciding it.
#
# ── The three notarization secrets ──────────────────────────────────────
#
#   MACOS_NOTARY_ISSUER_ID   the App Store Connect issuer ID (a UUID)
#   MACOS_NOTARY_KEY_ID      the API key ID (10 alphanumeric characters)
#   MACOS_NOTARY_KEY_P8      the AuthKey_<KEY_ID>.p8 Apple hands out ONCE:
#                            either `base64 -w0` of it, or the PEM text itself
#
# All three, or none. This recipe combines them with
# `rcodesign encode-app-store-connect-api-key` into the single JSON file the
# notary client wants; that file is written into a scratch directory that is
# removed on exit, success or failure.
#
# ── What a person has to do once the Apple Developer account is approved ─
#
#   1. Make a signing request. Apple never sees the private key:
#
#        rcodesign generate-certificate-signing-request \
#            --csr-pem-file squallar.csr \
#            --pem-file squallar-signing-key.pem
#
#      (or `--p12-file`, if the key already lives in one). The key must be RSA
#      or ECDSA -- see the Ed25519 note above, which is a different failure and
#      a worse one.
#
#   2. developer.apple.com -> Certificates, IDs & Profiles -> Certificates ->
#      "+" -> **Developer ID Application**. Not "Mac Development", not
#      "Mac App Distribution": those are for the App Store and for machines on
#      the team, and Gatekeeper does not accept them for software handed to
#      the public. Upload the CSR, download the .cer.
#
#   3. Put the certificate and its key into one .p12 and set the secrets:
#
#        base64 -w0 squallar-signing.p12   -> MACOS_SIGNING_P12
#                                          -> MACOS_SIGNING_P12_PASSWORD
#
#      Replacing the self-signed certificate changes the app's designated
#      requirement once, which costs every existing user one more location
#      prompt. It is a one-time cost and it is the last one, provided the same
#      Developer ID certificate is then kept.
#
#   4. appstoreconnect.apple.com -> Users and Access -> Integrations ->
#      App Store Connect API -> Team Keys -> "+", role **Developer**. Copy the
#      issuer ID and key ID, download the .p8 (Apple offers it once), and set
#      the three MACOS_NOTARY_* secrets above.
#
# Until all of that exists the build still runs: it signs, it says in the log
# that it is not notarizing and why, and it uploads the artifact. A fork pull
# request, which never gets secrets, takes exactly that path. Nothing here
# fails for want of a paid account.
SHELL := bash
.SHELLFLAGS := -euo pipefail -c
.ONESHELL:
ROOT := $(realpath $(CURDIR)/../..)

# Digest-pinned to the same image the CI matrix uses. Engines honour the
# DIGEST and ignore the tag, so a tag that reads 1.97.1 beside a digest built
# from 1.94.1 is a local build on a toolchain CI does not have: this pin was
# sha256:433d3e61 until 2026-09-06, which carried RUST_VERSION=1.94.1 while
# rust-toolchain.toml and .github/workflows/build.yaml both said 1.97.1.
# Change it together with the workflow's, or not at all.
IMAGE ?= ghcr.io/usa-reddragon/rust-cross:1.97.1@sha256:68d608cfe4232a8a9b42ce41ea26412eaadc8f53218fed7e52e43974b388a302
# Named volume for the crate cache, so a rebuild does not re-download the index.
CARGO_VOL ?= squallar-xcross-cargo
# arm64 by default: it is what every Mac sold since 2020 is, and the one arch
# where an unsigned build will not even launch.
ARCH ?= aarch64
# Separate from target/, which holds host artifacts.
TARGET_DIR ?= target-macos
# `rcodesign sign`. Not in the rust-cross image; CI installs it (see
# .github/workflows/build.yaml) and a developer gets it with
# `cargo install apple-codesign` or from
# https://github.com/indygreg/apple-platform-rs/releases.
RCODESIGN ?= rcodesign
# How long to wait on Apple's notary service before giving up. Apple's own
# figure is 'usually under 15 minutes'; the rcodesign default is 600s. A
# time-out here is not a rejection -- the submission stays queued and
# `rcodesign notary-wait <id>` resumes it -- so this is a bound on how long
# one CI row may sit still, not a bound on notarization.
NOTARY_TIMEOUT ?= 900

.PHONY: build clean
build:
	@cd "$(ROOT)"
	# One scratch directory, one trap. Everything secret this recipe writes to
	# disk -- the decoded .p12, its password, the notary API key -- goes here.
	# The previous spelling removed the password file on the success path only,
	# so a signing failure left a plaintext password in /tmp; `set -e` exits
	# before the `rm`. A single EXIT trap cannot be got wrong that way, and a
	# second `trap ... EXIT` later in the recipe would silently replace the
	# first, which is why there is exactly one.
	SCRATCH="$$(mktemp -d)"
	trap 'rm -rf "$$SCRATCH"' EXIT
	chmod 700 "$$SCRATCH"
	case "$(ARCH)" in
	  aarch64|arm64) RUST_TARGET=aarch64-apple-darwin ;;
	  x86_64|amd64)  RUST_TARGET=x86_64-apple-darwin ;;
	  # Not a real triple, and deliberately named like one: it is the output
	  # directory for the one bundle a person should actually download. See
	  # the BIN_ARM64/BIN_X86_64 branch below.
	  universal)     RUST_TARGET=universal-apple-darwin ;;
	  *) echo "ERROR: ARCH must be aarch64, x86_64 or universal, not '$(ARCH)'"; exit 1 ;;
	esac
	if [ -n "$(DEBUG)" ]; then PROFILE_DIR=debug; CARGO_PROFILE=; else PROFILE_DIR=release; CARGO_PROFILE=--release; fi

	# ---- the executable ----
	# BIN= names one that already exists. CI passes it because the build matrix
	# has already compiled exactly this binary for exactly this triple, and
	# compiling the workspace a second time to package it would double the
	# row's wall clock for a file that is already on disk.
	#
	# The corollary is that BIN= picks the profile too. DEBUG= and the default
	# `--release` above govern a build this recipe runs itself and nothing
	# else, so a caller handing over a path under debug/ gets a debug .app no
	# matter what else it passed — which is how every published macOS build
	# came to be an unoptimized one. Said out loud here rather than left to be
	# read off a path.
	# ARCH=universal takes two already-built Mach-O binaries and fuses them,
	# rather than building anything: there is no cargo target that emits a fat
	# binary, and the two halves are compiled by two different CI rows in
	# parallel. `rcodesign macho-universal-create` is the lipo(1) here, and it
	# is the same tool that signs, so the pipeline gains no new dependency.
	#
	# Why one universal bundle at all: notarization is per-submission, so two
	# per-arch bundles cost two uploads, two waits and two tickets, and hand
	# the person downloading it a choice they should not have to make.
	BIN_PATH="$(BIN)"
	if [ "$$RUST_TARGET" = universal-apple-darwin ]; then
	  [ -n "$(BIN_ARM64)" ] && [ -n "$(BIN_X86_64)" ] || {
	    echo "ERROR: ARCH=universal needs BIN_ARM64= and BIN_X86_64= (paths to the two"
	    echo "       already-built squallar binaries). Nothing here can compile them:"
	    echo "       cargo has no fat-binary target."
	    exit 1
	  }
	  for f in "$(BIN_ARM64)" "$(BIN_X86_64)"; do
	    [ -f "$$f" ] || { echo "ERROR: $$f does not exist"; exit 1; }
	  done
	  command -v $(RCODESIGN) >/dev/null 2>&1 || {
	    echo "ERROR: ARCH=universal needs $(RCODESIGN) to fuse the two binaries."; exit 1; }
	  BIN_PATH="$$SCRATCH/squallar-universal"
	  echo "==> fusing arm64 + x86_64 into one Mach-O"
	  $(RCODESIGN) macho-universal-create -o "$$BIN_PATH" "$(BIN_ARM64)" "$(BIN_X86_64)"
	fi
	if [ -n "$$BIN_PATH" ]; then
	  EXE="$$BIN_PATH"
	  [ -f "$$EXE" ] || { echo "ERROR: BIN=$$EXE does not exist"; exit 1; }
	  echo "==> packaging the existing $$EXE"
	  case "$$EXE" in
	    */debug/*)
	      echo "    NOTE: a debug-profile binary. The .app will carry workspace code at"
	      echo "          opt-level = 0 with debug assertions on."
	      ;;
	  esac
	else
	  EXE="$(TARGET_DIR)/$$RUST_TARGET/$$PROFILE_DIR/squallar"
	  # RUSTUP_TOOLCHAIN from the image's own RUST_VERSION: rust-toolchain.toml
	  # says `stable`, and obeying it here fetches a toolchain that has none of
	  # the image's 23 preinstalled targets.
	  #
	  # MACOSX_DEPLOYMENT_TARGET is deliberately *not* exported here. It lives
	  # in /.cargo/config.toml so that every `cargo` invocation gets it,
	  # including bare `cargo check` rows and a developer's local build; an
	  # export here would cover only this one path and would be the copy that
	  # drifts. The guard further down verifies the linked binary really got it.
	  INCMD="set -e
	      export RUSTUP_TOOLCHAIN=\"\$${RUST_VERSION:?the rust-cross image must set RUST_VERSION}\"
	      export CARGO_TARGET_DIR=$(TARGET_DIR)
	      cargo build $$CARGO_PROFILE --target $$RUST_TARGET -p squallar --bin squallar"
	  if [ -n "$(IN_CONTAINER)" ]; then
	    echo "==> cargo build ($$RUST_TARGET, $$PROFILE_DIR)"
	    sh -c "$$INCMD"
	  else
	    ENGINE="$${SQUALLAR_CONTAINER_ENGINE:-}"
	    if [ -z "$$ENGINE" ]; then
	      if command -v podman >/dev/null 2>&1; then ENGINE=podman
	      elif command -v docker >/dev/null 2>&1; then ENGINE=docker
	      else echo "ERROR: need podman or docker (set SQUALLAR_CONTAINER_ENGINE, or IN_CONTAINER=1 if already inside the image)."; exit 1; fi
	    fi
	    echo "==> [$$ENGINE] cargo build ($$RUST_TARGET, $$PROFILE_DIR)"
	    # Rootless podman already maps container root to the invoking user, so
	    # chowning there would push the files onto a subuid instead.
	    if [ "$$ENGINE" = docker ]; then INCMD="$$INCMD
	      chown -R $$(id -u):$$(id -g) $(TARGET_DIR)"; fi
	    "$$ENGINE" run --rm \
	        -v "$(ROOT)":/project -w /project \
	        -v "$(CARGO_VOL)":/usr/local/cargo/registry \
	        "$(IMAGE)" sh -c "$$INCMD"
	  fi
	  [ -f "$$EXE" ] || { echo "ERROR: the build produced no executable at $$EXE"; exit 1; }
	fi

	# ---- bundle metadata ----
	# Single source: packaging/macos/project.yml. Its own file and never packaging/ios/project.yml:
	# this reader takes the first `KEY:` in the file, comments included, and the
	# iOS spec's comment block names two of these keys before its settings do.
	yaml_val() { sed -n "s/.*$$1: *\"\{0,1\}\([^\"]*\)\"\{0,1\}.*/\1/p" packaging/macos/project.yml | head -1; }
	BUNDLE_ID="$$(yaml_val PRODUCT_BUNDLE_IDENTIFIER)"
	MARKETING_VERSION="$$(yaml_val MARKETING_VERSION)"
	CURRENT_PROJECT_VERSION="$$(yaml_val CURRENT_PROJECT_VERSION)"
	MACOS_MIN="$$(yaml_val MACOSX_DEPLOYMENT_TARGET)"
	: "$${BUNDLE_ID:?could not read PRODUCT_BUNDLE_IDENTIFIER from packaging/macos/project.yml}"
	: "$${MACOS_MIN:?could not read MACOSX_DEPLOYMENT_TARGET from packaging/macos/project.yml}"

	# The plist's floor and the compiler's floor are two different files, and
	# the whole point of the floor is that a binary built below it links
	# cleanly and then `doesNotRecognizeSelector:` on a user's Mac. So they are
	# checked against each other, and then both are checked against what the
	# linker actually wrote.
	CARGO_MIN="$$(sed -n 's/^ *MACOSX_DEPLOYMENT_TARGET *= *"\([^"]*\)".*/\1/p' .cargo/config.toml | head -1)"
	: "$${CARGO_MIN:?no MACOSX_DEPLOYMENT_TARGET in .cargo/config.toml; the Apple deployment floor is unset}"
	[ "$$CARGO_MIN" = "$$MACOS_MIN" ] || {
	  echo "ERROR: .cargo/config.toml says MACOSX_DEPLOYMENT_TARGET=$$CARGO_MIN"
	  echo "       packaging/macos/project.yml says MACOSX_DEPLOYMENT_TARGET=$$MACOS_MIN"
	  exit 1
	}

	# ---- assemble the .app (plain file ops, no Apple tools) ----
	echo "==> assemble squallar.app"
	OUT="packaging/macos/build/$$RUST_TARGET"; APP="$$OUT/squallar.app"
	rm -rf "$$OUT"; mkdir -p "$$APP/Contents/MacOS" "$$APP/Contents/Resources"
	# `chmod 0755` and not `chmod +x`: macho-universal-create writes its output
	# 0770, and +x on that carries a group-writable, other-unreadable mode into
	# the zip and out to whoever unpacks it.
	cp "$$EXE" "$$APP/Contents/MacOS/squallar"; chmod 0755 "$$APP/Contents/MacOS/squallar"
	# Four bytes of type and creator. Vestigial, and every real .app has it.
	printf 'APPL????' > "$$APP/Contents/PkgInfo"
	# Resolve the Xcode build-setting variables xcodebuild would have expanded.
	sed -e "s|\$$(EXECUTABLE_NAME)|squallar|g" \
	    -e "s|\$$(PRODUCT_BUNDLE_IDENTIFIER)|$$BUNDLE_ID|g" \
	    -e "s|\$$(MARKETING_VERSION)|$$MARKETING_VERSION|g" \
	    -e "s|\$$(CURRENT_PROJECT_VERSION)|$$CURRENT_PROJECT_VERSION|g" \
	    -e "s|\$$(MACOSX_DEPLOYMENT_TARGET)|$$MACOS_MIN|g" \
	    packaging/macos/Info.plist > "$$APP/Contents/Info.plist"
	# An unexpanded $(...) here means a key silently ships as a literal.
	! grep -q '\$$(' "$$APP/Contents/Info.plist" || { echo "ERROR: unexpanded build setting left in Info.plist"; exit 1; }

	# ---- the guard the deployment floor exists for ----
	# Reads the floor out of the Mach-O rather than out of any of the three
	# files that claim it. Rust does no availability checking at all, so this is
	# the only stage between "wrote 11.0 down somewhere" and "a user's Mac
	# throws doesNotRecognizeSelector:".
	OTOOL="$$(command -v otool 2>/dev/null || true)"
	if [ -z "$$OTOOL" ]; then OTOOL="$$(ls /opt/osxcross/bin/*-apple-darwin*-otool 2>/dev/null | head -1 || true)"; fi
	if [ -n "$$OTOOL" ]; then
	  MINOS="$$("$$OTOOL" -l "$$APP/Contents/MacOS/squallar" \
	    | grep -A6 -E 'LC_BUILD_VERSION|LC_VERSION_MIN_MACOSX' \
	    | grep -oE '^ *(minos|version) [0-9.]+' | head -1 | awk '{print $$2}' || true)"
	  : "$${MINOS:?could not read a deployment target out of the linked binary}"
	  [ "$$MINOS" = "$$MACOS_MIN" ] || {
	    echo "ERROR: the linked binary claims macOS $$MINOS, the bundle claims $$MACOS_MIN."
	    echo "       MACOSX_DEPLOYMENT_TARGET did not reach the compiler. Check that"
	    echo "       .cargo/config.toml is being discovered (cargo looks upward from the"
	    echo "       working directory), and that nothing in the environment overrode it."
	    exit 1
	  }
	  echo "    binary deployment target: macOS $$MINOS (matches the bundle)"
	  "$$OTOOL" -L "$$APP/Contents/MacOS/squallar" | grep -q CoreLocation || {
	    echo "ERROR: CoreLocation.framework is not linked into the binary."; exit 1; }
	  echo "    CoreLocation.framework: linked"
	else
	  echo "    note: no otool on PATH, so the binary's deployment target was not verified."
	fi

	# ---- sign ----
	# Read the long note at the top: this is not hardening, it is what stops
	# locationd from treating every build as a different application -- and,
	# when the notary credentials are there too, it is what Apple will agree
	# to notarize.
	#
	# The notary credentials are inspected HERE, before signing, because they
	# change how the app is signed. `--for-notarization` is not a post-hoc
	# switch: it requires an Apple-issued Developer ID certificate, requires a
	# time-stamp server, and turns the hardened runtime on for every Mach-O it
	# signs. An app signed without it is an app the notary rejects, and finding
	# that out costs a full upload and wait per attempt.
	NOTARY_READY=
	if [ -n "$${MACOS_NOTARY_ISSUER_ID:-}" ] && [ -n "$${MACOS_NOTARY_KEY_ID:-}" ] && [ -n "$${MACOS_NOTARY_KEY_P8:-}" ]; then
	  NOTARY_READY=1
	fi
	IDENTITY=none
	if ! command -v $(RCODESIGN) >/dev/null 2>&1; then
	  echo ""
	  echo "WARNING: $(RCODESIGN) is not on PATH, so squallar.app is UNSIGNED."
	  echo "         An arm64 Mac will refuse to execute it at all, and on any Mac"
	  echo "         macOS will treat each rebuild as a new app and re-ask for"
	  echo "         location permission. Install it with:"
	  echo "           cargo install apple-codesign"
	  echo "         or from https://github.com/indygreg/apple-platform-rs/releases"
	elif [ -n "$${MACOS_SIGNING_P12:-}$(P12_FILE)" ]; then
	  # A real, stable identity. The p12 arrives either as a path or, in CI, as
	  # base64 in a secret — GitHub secrets are text.
	  P12="$(P12_FILE)"
	  if [ -z "$$P12" ]; then
	    P12="$$SCRATCH/signing.p12"
	    printf '%s' "$${MACOS_SIGNING_P12}" | base64 -d > "$$P12"
	  fi
	  PWFILE="$$SCRATCH/p12-password"
	  printf '%s' "$${MACOS_SIGNING_P12_PASSWORD:-$(P12_PASSWORD)}" > "$$PWFILE"
	  if [ -n "$$NOTARY_READY" ]; then
	    IDENTITY=devid
	    echo "==> signing for notarization with the project identity"
	    # Signing a p12 reaches the network whether or not this flag is given:
	    # `sign` defaults to --timestamp-url http://timestamp.apple.com/ts01 and
	    # exits 1 if the server is unreachable (measured against a black-holed
	    # address). --for-notarization additionally REQUIRES a time-stamp, so
	    # there is no offline spelling of this arm -- which is correct, because
	    # Apple will not notarize an untimestamped signature either.
	    $(RCODESIGN) sign --p12-file "$$P12" --p12-password-file "$$PWFILE" \
	        --for-notarization "$$APP" || {
	      echo ""
	      echo "ERROR: signing --for-notarization failed."
	      echo "       If it said the certificate is not signed by Apple, then"
	      echo "       MACOS_SIGNING_P12 still holds the self-signed certificate. The"
	      echo "       notary path needs a real Developer ID Application certificate;"
	      echo "       see 'What a person has to do' at the top of this file. Clearing"
	      echo "       the three MACOS_NOTARY_* secrets returns this build to the"
	      echo "       signed-but-not-notarized path, which still produces an artifact."
	      exit 1
	    }
	  else
	    IDENTITY=cert
	    echo "==> signing with the project identity (no notary credentials)"
	    # Unscoped `runtime`, and NOT `main:runtime`. The scoped spelling is
	    # accepted, exits 0, prints nothing -- and leaves the flag off: measured
	    # with 0.29.0, `--code-signature-flags main:runtime` produced
	    # CodeSignatureFlags(ADHOC) on both slices of a fat binary where the
	    # unscoped form produced (ADHOC | RUNTIME). The guard below is what
	    # caught it and is what keeps it caught.
	    $(RCODESIGN) sign --p12-file "$$P12" --p12-password-file "$$PWFILE" \
	        --code-signature-flags runtime "$$APP"
	  fi
	  echo "    signed. The designated requirement is pinned to this certificate,"
	  echo "    so a rebuild signed with the same one keeps its location grant."
	else
	  # Ad-hoc: a valid signature, so an arm64 Mac will run it -- but the
	  # designated requirement is the code directory hash, which changes with
	  # every byte of the binary. locationd will re-prompt on every rebuild.
	  IDENTITY=adhoc
	  echo "==> signing ad-hoc (no MACOS_SIGNING_P12 / P12_FILE given)"
	  # The hardened runtime goes on here too, so that the bundle a fork pull
	  # request produces is the same SHAPE as the one that ships. Measured on
	  # macOS 26.4.1/arm64: an ad-hoc bundle with flags=0x10002 (adhoc,runtime)
	  # passed `codesign --verify --deep --strict` and executed. That was a
	  # trivial test binary, not squallar, so it proves the signature is well
	  # formed under the flag -- it does not prove squallar's own framework use
	  # is unaffected by the hardened runtime, which only a real run can.
	  $(RCODESIGN) sign --code-signature-flags runtime "$$APP"
	  echo "    NOTE: ad-hoc. This app will launch, and macOS will ask for"
	  echo "    location permission again after every rebuild."
	fi

	# ---- the guards the signing stage exists for ----
	# Both read the signature that was just written, not the arguments we think
	# we passed in. One `print-signature-info`, two questions.
	#
	# It must be this and not `rcodesign verify`: that subcommand does not
	# accept bundles at all, and -- measured with 0.29.0 -- it prints its
	# refusal and still exits 0, so gating on it would gate on nothing.
	if command -v $(RCODESIGN) >/dev/null 2>&1 && [ "$$IDENTITY" != none ]; then
	  SIGINFO="$$SCRATCH/signature-info.yaml"
	  $(RCODESIGN) print-signature-info "$$APP" > "$$SIGINFO" 2>/dev/null

	  # (1) Algorithm. Apple verifies only RSA and ECDSA; an Ed25519 key
	  # produces a structurally perfect signature that every Linux-side tool
	  # accepts and macOS rejects outright, so nothing on this side of the
	  # build notices without an explicit check. An ad-hoc signature carries no
	  # certificate, hence no algorithm; that is the labelled fallback above
	  # and must pass.
	  SIGALGO="$$(sed -n 's/^ *key_algorithm: *//p' "$$SIGINFO" | head -1)"
	  case "$$SIGALGO" in
	    "")          echo "    signature carries no certificate (ad-hoc)" ;;
	    RSA|ECDSA*)  echo "    signature algorithm: $$SIGALGO (macOS can verify this)" ;;
	    *)
	      echo "ERROR: the shipped signature uses $$SIGALGO, which macOS cannot verify."
	      echo "       Apple's Security.framework has no code-signing path for it, so"
	      echo "       'codesign --verify' reports the app as modified and Gatekeeper"
	      echo "       treats it as damaged -- a state the user cannot click through."
	      echo "       Regenerate the signing certificate with --algorithm rsa and"
	      echo "       replace the MACOS_SIGNING_P12 secret; see the note at the top"
	      echo "       of this file."
	      exit 1 ;;
	  esac

	  # (2) Hardened runtime, on EVERY Mach-O the signature covers. A fat
	  # binary is two code directories and gets two `flags:` lines; a bundle
	  # with a nested framework would get more. Counting rather than reading
	  # the first one is the difference between "the arm64 half is hardened"
	  # and "the app is hardened", and Apple's notary rejects the whole
	  # submission for the half that is not.
	  #
	  # This is the check that catches a signing invocation that was accepted
	  # and did nothing -- see the `main:runtime` note above. It runs on every
	  # arm, including the ad-hoc one, so it is exercised on every pull request
	  # from a fork and does not wait for a paid Apple account to have teeth.
	  NFLAGS="$$(grep -cE '^ *flags: CodeSignatureFlags' "$$SIGINFO" || true)"
	  NRUNTIME="$$(grep -E '^ *flags: CodeSignatureFlags' "$$SIGINFO" | grep -c RUNTIME || true)"
	  if [ "$$NFLAGS" -lt 1 ]; then
	    echo "ERROR: the signature reports no code directory at all. Either signing"
	    echo "       did not happen or print-signature-info changed its output; in"
	    echo "       both cases this guard has stopped checking anything."
	    exit 1
	  fi
	  if [ "$$NRUNTIME" != "$$NFLAGS" ]; then
	    echo "ERROR: $$NRUNTIME of $$NFLAGS signed Mach-O images carry the hardened"
	    echo "       runtime flag. Apple will not notarize the bundle, and the ones"
	    echo "       missing it would run without the protections the others have."
	    grep -E '^ *flags: CodeSignatureFlags' "$$SIGINFO" | sed 's/^/       /'
	    exit 1
	  fi
	  echo "    hardened runtime: on all $$NFLAGS signed Mach-O image(s)"
	fi

	# ---- notarize + staple ----
	# Apple issues the ticket; stapling puts it inside the .app so the check
	# works with no network. Everything below is skipped, loudly, when the
	# credentials are absent -- which is the normal state of a fork pull
	# request and was the normal state of this repository until somebody
	# created the secrets.
	#
	# ── Which failures here are fatal, and why they are not all fatal ──
	#
	# Everything above this line is fatal: the fuse, the signature, the
	# hardened-runtime count, and below, the check that stapling actually put
	# a ticket in the bundle. Those are statements about OUR code, and a
	# regression in one is a defect that must stop the build.
	#
	# The submission itself is NOT fatal, and that is a deliberate asymmetry
	# rather than leniency. A notary failure is an outage in a third-party
	# service, an expired credential or a rate limit; it is not evidence about
	# the commit that happened to be pushed when it occurred, and nobody
	# reading that commit can fix it. Reddening the build for it would stop
	# every unrelated thing this repository gates on a green board -- the web
	# deploy included -- for a reason that has no relationship to any of them.
	#
	# What replaces the hard failure is a name. A build that did not get a
	# ticket is written as `squallar-NOT-NOTARIZED.app.zip`, never as
	# `squallar.app.zip`, so the degraded path cannot quietly become the
	# normal one: it is visible in the file a person downloads, in the
	# directory listing, and in the artifact CI publishes. `NOTARIZATION` is
	# written beside the zip with the one-word reason, for the workflow to
	# read and put in the run summary.
	#
	# The one thing that IS fatal on this path: notarization reporting success
	# and stapling nothing. That is not Apple being unavailable, it is a claim
	# that disagrees with the bundle on disk.
	NOTARY_STATUS=skipped
	if [ -n "$$NOTARY_READY" ] && [ "$$IDENTITY" = devid ]; then
	  KEYP8="$$SCRATCH/AuthKey.p8"
	  # Both shapes accepted on purpose. The .p8 is PEM text and survives a
	  # GitHub secret intact, but a multi-line secret is the single easiest
	  # thing to paste wrong, and base64 is what the p12 secret next door
	  # already uses. Whichever arrives, the log says which was recognised, so
	  # a mangled secret is a named failure rather than a puzzling one.
	  case "$${MACOS_NOTARY_KEY_P8}" in
	    -----BEGIN*)
	      echo "==> notary key: PEM text"
	      printf '%s\n' "$${MACOS_NOTARY_KEY_P8}" > "$$KEYP8" ;;
	    *)
	      echo "==> notary key: base64"
	      printf '%s' "$${MACOS_NOTARY_KEY_P8}" | base64 -d > "$$KEYP8" || {
	        echo "ERROR: MACOS_NOTARY_KEY_P8 is neither PEM text starting '-----BEGIN'"
	        echo "       nor valid base64. It should be the AuthKey_<KEY_ID>.p8 file"
	        echo "       Apple gave you, either verbatim or as 'base64 -w0'."
	        exit 1
	      } ;;
	  esac
	  grep -q 'BEGIN PRIVATE KEY' "$$KEYP8" || {
	    echo "ERROR: MACOS_NOTARY_KEY_P8 decoded to something that is not a PKCS#8"
	    echo "       private key. Apple's App Store Connect key is a PEM file whose"
	    echo "       first line is '-----BEGIN PRIVATE KEY-----'."
	    exit 1
	  }
	  KEYJSON="$$SCRATCH/notary-key.json"
	  $(RCODESIGN) encode-app-store-connect-api-key -o "$$KEYJSON" \
	      "$${MACOS_NOTARY_ISSUER_ID}" "$${MACOS_NOTARY_KEY_ID}" "$$KEYP8" >/dev/null

	  # What proves a staple happened. `notary-submit --staple` writes the
	  # ticket INTO the bundle directory; rather than assert a path this file
	  # would then have to keep in step with rcodesign, take the file list
	  # before and after and require that something appeared. It cannot pass
	  # vacuously, and it prints the path, so the next person reading this
	  # learns where the ticket actually went.
	  find "$$APP" -type f | sort > "$$SCRATCH/before.txt"
	  echo "==> submitting to Apple's notary service (up to $(NOTARY_TIMEOUT)s)"
	  if $(RCODESIGN) notary-submit --api-key-file "$$KEYJSON" \
	      --staple --max-wait-seconds $(NOTARY_TIMEOUT) "$$APP"; then
	    find "$$APP" -type f | sort > "$$SCRATCH/after.txt"
	    STAPLED="$$(comm -13 "$$SCRATCH/before.txt" "$$SCRATCH/after.txt")"
	    # Fatal, and the one thing on this path that is. Apple saying yes and
	    # the bundle gaining nothing is a disagreement between a claim and the
	    # disk, not an outage.
	    [ -n "$$STAPLED" ] || {
	      echo "ERROR: notarization reported success but nothing was added to the"
	      echo "       bundle, so no ticket was stapled. The zip would ship a bundle"
	      echo "       that needs Apple reachable at first launch."
	      exit 1
	    }
	    NOTARY_STATUS=stapled
	    echo "    notarized and stapled:"
	    printf '%s\n' "$$STAPLED" | sed 's/^/      /'
	  else
	    NOTARY_STATUS=failed
	    echo ""
	    echo "WARNING: notarization did not complete, and this build is NOT fatal"
	    echo "         about that on purpose -- see the note above. The bundle is"
	    echo "         signed with the Developer ID identity and carries the"
	    echo "         hardened runtime; it has no ticket, so another Mac will"
	    echo "         refuse it. It is being written as NOT-NOTARIZED rather than"
	    echo "         thrown away, so the run leaves something to inspect."
	    echo "         A rejection is a verdict on the bundle and re-running will not"
	    echo "         change it: 'rcodesign notary-log <submission-id> --api-key-file"
	    echo "         <key.json>' prints Apple's reasons, and the submission id is in"
	    echo "         the output above. A time-out is not a verdict -- the submission"
	    echo "         stays queued and 'rcodesign notary-wait <submission-id>' picks"
	    echo "         it up. An 'Unauthenticated' is neither: it is the credentials."
	  fi
	elif [ -n "$$NOTARY_READY" ]; then
	  echo ""
	  echo "WARNING: the MACOS_NOTARY_* credentials are set, but this bundle was not"
	  echo "         signed with a Developer ID certificate ($$IDENTITY), and Apple"
	  echo "         only notarizes Developer ID signed software. Not submitting."
	elif [ "$$IDENTITY" = cert ] || [ "$$IDENTITY" = devid ]; then
	  echo ""
	  echo "NOTE: signed, NOT notarized -- the three MACOS_NOTARY_* secrets are not"
	  echo "      set. On the machine that built it this app is fine. On anyone"
	  echo "      else's Mac, Gatekeeper will refuse it and offer no plain way"
	  echo "      through; see the note at the top of this file for what to create."
	else
	  echo ""
	  echo "NOTE: not notarized (nothing to notarize an ad-hoc bundle with). Another"
	  echo "      person's Mac will refuse to open this; that is expected for a"
	  echo "      build with no secrets, and it still runs on the machine that made it."
	fi

	# The one-word outcome, written beside the zip rather than inside the .app:
	# anything added inside the bundle after signing breaks the seal. CI reads
	# this for its run summary; a person reading a build directory gets the
	# same answer without having to remember what the zip's name means.
	printf '%s\n' "$$NOTARY_STATUS" > "$$OUT/NOTARIZATION"

	if [ -n "$(ARCHIVE)" ]; then
	  # actions/upload-artifact drops the executable bit, which turns a .app
	  # into a directory macOS declines to launch. A zip carries the mode.
	  #
	  # LAST, and after stapling: a .zip cannot hold a notarization ticket
	  # ("do not know how to staple", measured), so it has to be built around a
	  # bundle that already has one. Moving this above the notarize stage would
	  # ship an un-ticketed zip and nothing would say so.
	  #
	  # The NAME is the label. `squallar.app.zip` means a ticket is stapled
	  # inside and any Mac will open it; every other outcome -- no credentials,
	  # an ad-hoc signature, Apple unreachable, Apple saying no -- produces
	  # `squallar-NOT-NOTARIZED.app.zip`. One name for the shippable thing and
	  # one for everything else, because a reader who has to work out which
	  # they have will eventually work it out wrong.
	  command -v zip >/dev/null 2>&1 || { echo "ERROR: ARCHIVE=1 but no zip on PATH"; exit 1; }
	  if [ "$$NOTARY_STATUS" = stapled ]; then ZIPNAME=squallar.app.zip; else ZIPNAME=squallar-NOT-NOTARIZED.app.zip; fi
	  ( cd "$$OUT" && rm -f squallar.app.zip squallar-NOT-NOTARIZED.app.zip && zip -qry "$$ZIPNAME" squallar.app )
	  echo "Zip: $(ROOT)/$$OUT/$$ZIPNAME"
	fi
	echo ""
	echo "App: $(ROOT)/$$APP   (notarization: $$NOTARY_STATUS)"
	echo "Run it with:  open '$(ROOT)/$$APP'"

clean:
	@rm -rf "$(ROOT)/packaging/macos/build" "$(ROOT)/$(TARGET_DIR)"
