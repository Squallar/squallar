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
# The p12 that writes is one rcodesign can read back. Measured with 0.29.0:
# `openssl pkcs12 -info` on the file it wrote reports
# pbeWithSHA1And40BitRC2-CBC and pbeWithSHAAnd3-KeyTripleDES-CBC, the two
# ciphers its parser has. A p12 made with `openssl pkcs12 -export` needs
# `-legacy` to come out that way; step 3 below says why.
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
#   sign (--for-notarization) -> notary-submit -> wait -> staple -> zip
#
# `rcodesign staple` refuses a .zip outright ("do not know how to staple") and
# refuses a bare Mach-O ("cannot staple Mach-O binaries") -- both measured with
# 0.29.0 -- so the ticket goes into the .app directory and the zip is made
# afterwards, around the stapled bundle. A .dmg can also carry a ticket, but
# building one needs Apple's hdiutil, i.e. a Mac, so it is not available to
# this Linux-only pipeline. The upload does not need a zip made here either:
# `notary-submit` given a bundle path zips it itself for the upload; the
# recipe then waits for Apple's verdict and `rcodesign staple`s the directory.
#
# Stapling is what makes the ticket work OFFLINE. A notarized-but-unstapled app
# still passes Gatekeeper on a machine that can reach Apple, and fails on one
# that cannot; that difference is the whole reason the zip is built last.
#
# ── One name means shippable, every other outcome means not ─────────────
#
# `squallar.app.zip` is written only when a ticket was stapled. Every other
# outcome -- no notary credentials, an ad-hoc signature, Apple unreachable,
# Apple saying no -- writes `squallar-NOT-NOTARIZED.app.zip` instead (Apple
# has the bytes and no ticket is stapled yet is
# `squallar-PENDING-NOTARIZATION.app.zip`, a different claim; see the zip
# step), and a one-word reason lands in `NOTARIZATION` beside it. The reason
# for making the
# degraded build LOUD rather than making it fail is set out in full at the
# notarize stage further down: a notary outage is not evidence about this
# repository's code and must not stop everything that gates on a green board,
# but a build that is quietly indistinguishable from a good one is how "we
# ship notarized releases" stops being true without anybody deciding it.
#
# ── The App Store Connect API key: one key, three secrets ───────────────
#
#   APPSTORE_CONNECT_ISSUER_ID   the App Store Connect issuer ID (a UUID)
#   APPSTORE_CONNECT_KEY_ID      the API key ID (10 alphanumeric characters)
#   APPSTORE_CONNECT_KEY_P8      the AuthKey_<KEY_ID>.p8 Apple hands out ONCE:
#                                either `base64 -w0` of it, or the PEM text itself
#
# All three, or none. The same three secrets drive the iOS TestFlight upload
# (.github/scripts/appstore-upload.py), so there is one key and its role is
# **App Manager**: the Developer role notarizes but cannot upload builds, and
# App Manager does both. Two copies of one key under two names is a rotation
# hazard, which is why the earlier MACOS_NOTARY_* and APPSTORE_* spellings
# are gone rather than kept as fallbacks -- a fallback leaves "which key did
# this run use?" unanswerable. This recipe combines them with
# `rcodesign encode-app-store-connect-api-key` into the single JSON file the
# notary client wants; that file is written into a scratch directory that is
# removed on exit, success or failure.
#
# When the workflow passes NONE of them on purpose it says so, in
# APPSTORE_CONNECT_WITHHELD: `not-main-push` on any event that is not a push
# to main (the key is handed over only there), `per-arch-row` on the two
# per-architecture rows (which never notarize; the universal job does). The
# notarize stage reads that marker before it reads the secrets, so the log
# says "withheld" for a key that exists and "missing" only for one that does
# not -- and names which of the three, never the values.
#
# ── What a person has to do once the Apple Developer account is approved ─
#
#   1. Make a key, then a signing request from it. Apple never sees the
#      private key:
#
#        openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 \
#            -out squallar-signing-key.pem
#        rcodesign generate-certificate-signing-request \
#            --pem-file squallar-signing-key.pem \
#            --csr-pem-file squallar.csr
#
#      `--pem-file` READS a key; it does not write one. Measured with 0.29.0:
#      run without the genpkey line first, it exits with "I/O error: No such
#      file or directory" and writes no CSR. `--p12-file` reads the key out of
#      a p12 instead, if it already lives in one. The key must be RSA or ECDSA
#      -- see the Ed25519 note above, which is a different failure and a worse
#      one.
#
#   2. developer.apple.com -> Certificates, IDs & Profiles -> Certificates ->
#      "+" -> **Developer ID Application**. Not "Mac Development", not
#      "Mac App Distribution": those are for the App Store and for machines on
#      the team, and Gatekeeper does not accept them for software handed to
#      the public. Upload the CSR, download the .cer.
#
#   3. Put the LEAF certificate and its key -- those two and nothing else --
#      into one .p12, with `-legacy`, and set the secrets:
#
#        openssl x509 -inform DER -in developerID_application.cer \
#            -out developerID_application.pem
#        openssl pkcs12 -export -legacy \
#            -inkey squallar-signing-key.pem -in developerID_application.pem \
#            -out squallar-signing.p12
#        base64 -w0 squallar-signing.p12   -> MACOS_SIGNING_P12
#                                          -> MACOS_SIGNING_P12_PASSWORD
#
#      Two ways a correct certificate under a correct password still fails,
#      both decided by how `pkcs12 -export` is invoked, and both checked by
#      the signing stage below before rcodesign sees the file:
#
#      No `-certfile`, no `-chain`. rcodesign signs with the FIRST
#      certificate in the file, and OpenSSL writes a `-certfile` CA before
#      the leaf. Measured with 0.29.0: a throwaway CA with OU=G2 (the OU of
#      Apple's Developer ID CA G2) in front of a leaf with OU=ABCDE12345 made
#      `analyze-certificate` report Certificate 0 with Team ID G2 and made
#      `sign` emit a signature whose certificate was the CA; the same leaf
#      and key alone reported ABCDE12345. Under --for-notarization rcodesign
#      then says "doesn't appear to be such a certificate" -- the TYPE is
#      named, the ORDER is the cause. The intermediates are not needed in
#      the file: rcodesign ships Apple's CA certificates
#      (src/apple-certs/DeveloperIDG2CA.cer, DeveloperIDCA.cer, ...) and adds
#      the chain from the leaf upward when it loads the key
#      (chain_apple_certificates); `analyze-certificate` on a bare
#      Apple-issued leaf prints that chain as "Apple Issuing Chain".
#
#      `-legacy` because OpenSSL 3 encrypts a p12 with PBES2/PBKDF2/AES-256-CBC
#      by default, and rcodesign's PKCS#12 parser (the p12 crate, 0.6.3 in
#      apple-codesign 0.29.0) decrypts exactly two ciphers, the pre-OpenSSL-3
#      pbeWithSHA1And40BitRC2-CBC and pbeWithSHAAnd3-KeyTripleDES-CBC -- and a
#      bag it cannot decrypt comes out as "incorrect password given when
#      decrypting PFX data", the same words it uses for a wrong password.
#      Measured: one cert and key exported without `-legacy` fail with that
#      message under the right password; exported with it, they sign. The
#      signing stage below asks openssl about the file before rcodesign sees
#      it and names this failure by name. The x509 line first because the
#      .cer the portal downloads is DER, and `pkcs12 -export` reads PEM.
#
#      Replacing the self-signed certificate changes the app's designated
#      requirement once, which costs every existing user one more location
#      prompt. It is a one-time cost and it is the last one, provided the same
#      Developer ID certificate is then kept.
#
#   4. appstoreconnect.apple.com -> Users and Access -> Integrations ->
#      App Store Connect API -> Team Keys -> "+", role **App Manager**. The
#      Developer role notarizes but cannot upload builds, and this one key
#      also feeds the iOS TestFlight upload. Copy the issuer ID and key ID,
#      download the .p8 (Apple offers it once), and set the three
#      APPSTORE_CONNECT_* secrets above -- once, for both lanes. Any
#      MACOS_NOTARY_* or APPSTORE_* secrets still in the repository are read
#      by nothing and should be deleted.
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
# The wait for Apple's verdict is unconditional: the notarize stage polls
# until Apple returns a terminal state, and the wait is the price of an
# automatically stapled artifact. Apple offers no callback, so a runner
# asking is the only way to learn the ticket exists. The only outer bound is
# GitHub's own job timeout (360 minutes by default, left there): that is
# GitHub's bound, not ours, and it exists so a runaway is not literally
# infinite. The three below shape the asking, not its length.
#
# One `rcodesign notary-wait` call is bounded by --max-wait-seconds and ends
# on the first transient error, so the recipe calls it again until a verdict
# arrives; this is the length of one call.
NOTARY_WAIT_CALL ?= 600
# Pause before asking again after a poll that failed for a reason other than
# its own limit (the network, a 5xx), and between staple attempts.
NOTARY_RETRY_SLEEP ?= 30
# A fresh ticket can take a minute to reach Apple's public ticket service;
# staple attempts before publishing the bundle as PENDING-NOTARIZATION.
NOTARY_STAPLE_TRIES ?= 10

# ── CHANNEL: which of the two macOS products this run is building ──────
#
#   direct  (default)  Developer ID Application, hardened runtime, notarized,
#                      ticket stapled, NOT sandboxed. This is the build on the
#                      website. Everything above in this file describes it.
#   store              Apple Distribution, App Sandbox ON, embedded
#                      provisioning profile, wrapped in a .pkg signed with a
#                      Mac Installer Distribution certificate, uploaded to App
#                      Store Connect for TestFlight. Never notarized: App
#                      Review is that channel's gate, and the notary is not.
#
# The two are different products, not one product with a flag. A store build
# is sandboxed and a direct build must never be -- sandboxing the download
# would containerise a shipping app behind the user's back -- so the sandbox
# entitlement lives in packaging/macos/entitlements-store.plist and is reached
# only from this variable's `store` value.
#
# The `$(error)` is not decoration. The direct path already names its outcomes
# honestly -- `squallar.app.zip` only with a stapled ticket, and
# `-NOT-NOTARIZED` / `-PENDING-NOTARIZATION` for every other one -- because a
# name that can assert a false fact is how a reader works it out wrong. A
# misspelled or not-yet-configured channel must fail the same way rather than
# fall through to whichever arm happens to be last: an unrecognised CHANNEL
# stops make at parse time, before any artifact exists to be mislabelled.
CHANNEL ?= direct
ifneq ($(filter-out direct store,$(CHANNEL)),)
$(error CHANNEL must be `direct` or `store`, got `$(CHANNEL)`)
endif

# The store variant's App Sandbox entitlements. Read only when CHANNEL=store;
# the direct build signs with no entitlements file at all and must keep doing
# so. Every key in it is justified against a call site in the file itself.
STORE_ENTITLEMENTS ?= $(ROOT)/packaging/macos/entitlements-store.plist
# The Mac App Store provisioning profile for `app.squallar`, copied into the
# bundle as Contents/embedded.provisionprofile. TestFlight REQUIRES one even
# though the Mac App Store does not: a main bundle without it uploads and then
# reports ITMS-90889, "Cannot be used with TestFlight because the bundle ...
# is missing a provisioning profile", and the build sits in App Store Connect
# marked Not Available for Testing. In CI it arrives base64 in the secret
# MACOS_STORE_PROFILE.
STORE_PROFILE ?=
# Builds the flat .pkg. Split out of this file because it is thirty lines of
# cpio/mkbom/xar that have nothing to do with make, and because it is testable
# on its own against any .app.
MKPKG ?= $(ROOT)/packaging/macos/mkpkg.sh

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
	# The build number. App Store Connect refuses a build whose CFBundleVersion
	# it has already seen for that CFBundleShortVersionString, so a number that
	# lives in a file is a number that has to be bumped by hand before every
	# upload and is a duplicate the first time someone forgets. CI hands in
	# BUILD_NUMBER from the run number instead (build.yaml); project.yml's
	# CURRENT_PROJECT_VERSION is what a local build gets.
	CURRENT_PROJECT_VERSION="$${BUILD_NUMBER:-$$(yaml_val CURRENT_PROJECT_VERSION)}"
	case "$$CURRENT_PROJECT_VERSION" in
	  *[!0-9.]*|"") echo "ERROR: build number '$$CURRENT_PROJECT_VERSION' is not dotted integers (BUILD_NUMBER, else project.yml CURRENT_PROJECT_VERSION)"; exit 1 ;;
	esac
	BUILD_NUMBER_SOURCE="packaging/macos/project.yml"; [ -z "$${BUILD_NUMBER:-}" ] || BUILD_NUMBER_SOURCE="BUILD_NUMBER"
	echo "    build number: $$CURRENT_PROJECT_VERSION (from $$BUILD_NUMBER_SOURCE)"
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
	# Xcode ships every plist it bundles as a binary plist (bplist00). App
	# Store Connect's iOS validator turned out to read one key from that form
	# only -- 90065 with the key present in XML, cleared by this same
	# conversion (packaging/ios/Makefile) -- so the Mac bundle takes the shape
	# every Apple reader has actually been shown. Both channels: one bundle
	# shape, and the direct channel's readers (Gatekeeper, LaunchServices) read
	# binary plists from every Xcode-built app on the machine. After the
	# expansion guard above, which greps the XML.
	cargo run -q -p squallar-car --bin plist-binary -- "$$APP/Contents/Info.plist"
	[ "$$(head -c 8 "$$APP/Contents/Info.plist")" = "bplist00" ] \
	    || { echo "ERROR: Info.plist was not converted to a binary plist."; exit 1; }
	# Apple's privacy manifest, under Contents/Resources for macOS -- iOS puts
	# the same file at the bundle root instead, and that difference is why the
	# placement is worth reading off Apple's rule rather than assuming the two
	# bundles agree. Copied verbatim: no build settings to expand. It lands
	# before the signing stage below, which seals the bundle's resources --
	# anything added after that breaks the seal. One source for both bundles;
	# see the file's own header.
	# The app icon, rendered from packaging/icon/squallar.svg straight into
	# the bundle. Nothing derived from the drawing is committed, so this is
	# where the .icns comes into existence; CFBundleIconFile in Info.plist
	# names it without the extension, which is what Finder and the Dock read.
	cargo run -q -p squallar-icon -- --icns "$$APP/Contents/Resources/squallar.icns"
	test -s "$$APP/Contents/Resources/squallar.icns" \
	    || { echo "ERROR: the icon renderer wrote no squallar.icns."; exit 1; }
	cp packaging/PrivacyInfo.xcprivacy "$$APP/Contents/Resources/PrivacyInfo.xcprivacy"
	test -s "$$APP/Contents/Resources/PrivacyInfo.xcprivacy" || { echo "ERROR: PrivacyInfo.xcprivacy did not reach Contents/Resources"; exit 1; }

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
	if [ -n "$${APPSTORE_CONNECT_ISSUER_ID:-}" ] && [ -n "$${APPSTORE_CONNECT_KEY_ID:-}" ] && [ -n "$${APPSTORE_CONNECT_KEY_P8:-}" ]; then
	  NOTARY_READY=1
	fi
	IDENTITY=none
	if [ "$(CHANNEL)" = store ]; then
	  # ---- the store arm: sandboxed, provisioned, NOT notarized ----
	  # A different certificate, a different entitlement set and an embedded
	  # profile. It shares only the bundle layout with the direct arm, which
	  # is why it branches here and not earlier.
	  #
	  # Nothing below is a fallback. A store build that is not signed with an
	  # Apple Distribution certificate is not a degraded store build, it is an
	  # artifact that cannot be uploaded, and the packaging stage names it as
	  # one. The direct arm's ad-hoc fallback exists so a fork's pull request
	  # still produces a runnable app; there is no equivalent need here,
	  # because nobody runs a store .pkg locally.
	  command -v $(RCODESIGN) >/dev/null 2>&1 || { echo "ERROR: CHANNEL=store needs $(RCODESIGN) on PATH."; exit 1; }
	  # The profile goes in BEFORE signing. It lives inside the bundle, so the
	  # signature seals it; adding it afterwards breaks the seal exactly the
	  # way the NOTARIZATION file's comment describes further down.
	  PROFILE_SRC=
	  if [ -n "$(STORE_PROFILE)" ]; then
	    PROFILE_SRC="$(STORE_PROFILE)"
	  elif [ -n "$${MACOS_STORE_PROFILE:-}" ]; then
	    PROFILE_SRC="$$SCRATCH/store.provisionprofile"
	    printf '%s' "$${MACOS_STORE_PROFILE}" | base64 -d > "$$PROFILE_SRC" 2>/dev/null \
	      || { echo "ERROR: MACOS_STORE_PROFILE is not base64."; exit 1; }
	  fi
	  TEAM_ID=
	  if [ -n "$$PROFILE_SRC" ]; then
	    cp "$$PROFILE_SRC" "$$APP/Contents/embedded.provisionprofile"
	    echo "    embedded.provisionprofile: installed"
	    # The profile is CMS-wrapped XML. `openssl cms -verify -noverify`
	    # unwraps it without checking Apple's signature -- the check would need
	    # Apple's CA chain and proves nothing here, since a profile that is not
	    # Apple's will be rejected on upload anyway. The team id is the one
	    # value the entitlements have to agree with, and reading it out of the
	    # profile is what keeps them agreeing.
	    if command -v openssl >/dev/null 2>&1; then
	      TEAM_ID="$$(openssl cms -verify -noverify -inform DER -in "$$PROFILE_SRC" 2>/dev/null \
	        | sed -n '/<key>com.apple.developer.team-identifier<\/key>/{n;s/.*<string>\(.*\)<\/string>.*/\1/p;}' | head -1)"
	      [ -n "$$TEAM_ID" ] || TEAM_ID="$$(openssl cms -verify -noverify -inform DER -in "$$PROFILE_SRC" 2>/dev/null \
	        | sed -n '/<key>TeamIdentifier<\/key>/,/<\/array>/{s/.*<string>\(.*\)<\/string>.*/\1/p;}' | head -1)"
	    fi
	  else
	    echo "note: no MACOS_STORE_PROFILE / STORE_PROFILE, so no provisioning profile is embedded."
	  fi
	  if [ -n "$${MACOS_STORE_P12:-}" ] && [ -n "$$TEAM_ID" ]; then
	    SP12="$$SCRATCH/store.p12"
	    printf '%s' "$${MACOS_STORE_P12}" | base64 -d > "$$SP12" 2>/dev/null \
	      || { echo "ERROR: MACOS_STORE_P12 is not base64."; exit 1; }
	    SPWFILE="$$SCRATCH/store.pw"
	    printf '%s' "$${MACOS_STORE_P12_PASSWORD:-}" > "$$SPWFILE"
	    # The entitlements are rendered here, from the template, with the team
	    # id the profile just gave us. Substituting rather than typing the
	    # value is what stops the two from drifting into an upload rejection.
	    ENTS="$$SCRATCH/entitlements.plist"
	    sed -e "s|\$$(TEAM_ID)|$$TEAM_ID|g" \
	        -e "s|\$$(PRODUCT_BUNDLE_IDENTIFIER)|$$BUNDLE_ID|g" \
	        "$(STORE_ENTITLEMENTS)" > "$$ENTS"
	    echo "==> signing for the App Store (sandboxed, team $$TEAM_ID)"
	    # `--code-signature-flags runtime` for the same reason the direct arm
	    # sets it, plus one: the guard below counts the flag on every Mach-O
	    # and does not know which channel produced them. The hardened runtime
	    # is accepted on Mac App Store builds, so satisfying that guard costs
	    # this arm nothing.
	    $(RCODESIGN) sign --p12-file "$$SP12" --p12-password-file "$$SPWFILE" \
	        --code-signature-flags runtime \
	        --entitlements-xml-path "$$ENTS" "$$APP" \
	      && IDENTITY=store \
	      || { echo "ERROR: signing for the App Store failed."; exit 1; }
	    echo "    signed with the Apple Distribution certificate and the sandbox entitlements."
	  else
	    [ -n "$${MACOS_STORE_P12:-}" ] || echo "note: no MACOS_STORE_P12, so the app is not signed for the store."
	    [ -n "$$TEAM_ID" ] || echo "note: no team id could be read, so the entitlements cannot be rendered."
	  fi
	elif ! command -v $(RCODESIGN) >/dev/null 2>&1; then
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

	  # ---- can rcodesign read this p12 at all? ----
	  # rcodesign's PKCS#12 parser (the p12 crate, 0.6.3 in apple-codesign
	  # 0.29.0) decrypts exactly two ciphers -- pbeWithSHA1And40BitRC2-CBC and
	  # pbeWithSHAAnd3-KeyTripleDES-CBC, the pre-OpenSSL-3 form -- and a bag in
	  # any other cipher decrypts to the same None as a bag under the wrong
	  # password. Both surface as "incorrect password given when decrypting PFX
	  # data", so a p12 written by OpenSSL 3's default `pkcs12 -export`
	  # (PBES2/PBKDF2/AES-256-CBC) sends the reader chasing a password that is
	  # right. openssl can tell the two apart, so it is asked first when it is
	  # on PATH: the MAC check, which is what a wrong password fails, runs
	  # before the bag ciphers are printed, and the ciphers are printed before
	  # any bag is decrypted. The rust-cross image has it at /usr/bin (3.5.6,
	  # measured), and the ubuntu-24.04 runner has it too -- the ios-testflight
	  # job asserts exactly that.
	  if command -v openssl >/dev/null 2>&1; then
	    P12INFO="$$SCRATCH/p12-info"
	    # `-legacy` first, plain second, and the exit status of neither is what
	    # is read. On OpenSSL 3 the RC2-40 cipher lives in the legacy provider,
	    # which is not loaded by default, so reading a GOOD file without the
	    # flag stops after the first algorithm line with "unsupported ...
	    # RC2-40-CBC" (measured on 3.5.6 and 3.6.3); the retry is for an
	    # openssl that has no such flag. What is inspected is the text: a
	    # "Mac verify error" is the password, a PBES2/PBKDF2/AES line is the
	    # cipher, and neither is a file rcodesign can read.
	    openssl pkcs12 -info -noout -legacy -in "$$P12" -passin "file:$$PWFILE" > "$$P12INFO" 2>&1 \
	      || openssl pkcs12 -info -noout -in "$$P12" -passin "file:$$PWFILE" > "$$P12INFO" 2>&1 \
	      || true
	    if grep -q 'Mac verify error' "$$P12INFO"; then
	      echo ""
	      echo "ERROR: the p12 password is wrong. openssl could not verify the file's MAC"
	      echo "       with the password in MACOS_SIGNING_P12_PASSWORD / P12_PASSWORD=, and"
	      echo "       the MAC is checked before any cipher question arises -- so this is"
	      echo "       the password, not the file's encryption."
	      exit 1
	    elif grep -qE 'PBES2|PBKDF2|AES' "$$P12INFO"; then
	      echo ""
	      echo "ERROR: the p12 is encrypted with a cipher rcodesign cannot read:"
	      grep -E '^(PKCS7 Encrypted data|Shrouded Keybag):' "$$P12INFO" | sed 's/^/         /'
	      echo "       The password is right: openssl verified the MAC with it. OpenSSL 3"
	      echo "       writes PBES2/PBKDF2/AES-256 by default; rcodesign's parser reads only"
	      echo "       the legacy RC2-40/3DES/SHA-1 form, and reports this file as"
	      echo "       'incorrect password given when decrypting PFX data'. Re-export it:"
	      echo "         openssl pkcs12 -export -legacy -in cert.pem -inkey key.pem -out squallar-signing.p12"
	      echo "       or from this file alone -- through a file, not a pipe: -export opens"
	      echo "       its input once for the certificate and again for the key, and piped"
	      echo "       it finds no certificate (measured):"
	      echo "         openssl pkcs12 -in old.p12 -out both.pem   # the key stays encrypted"
	      echo "         openssl pkcs12 -export -legacy -in both.pem -out squallar-signing.p12"
	      echo "         rm both.pem"
	      echo "       then replace MACOS_SIGNING_P12 with 'base64 -w0' of the result."
	      exit 1
	    else
	      CIPHERS="$$(grep -E '^(PKCS7 Encrypted data|Shrouded Keybag):' "$$P12INFO" | sed 's/^[^:]*: *//; s/,.*//' | sort -u | paste -sd/ || true)"
	      echo "    p12 cipher: $${CIPHERS:-not reported by openssl; rcodesign decides}"
	    fi
	  else
	    echo "    note: no openssl on PATH, so the p12 cipher was not checked before rcodesign."
	  fi

	  # ---- is Certificate 0 the leaf, and the right kind of leaf? ----
	  # rcodesign signs with the FIRST certificate in the p12, and `openssl
	  # pkcs12 -export -certfile` writes the CA it was handed before the leaf.
	  # Measured with 0.29.0: a throwaway CA with OU=G2 -- the OU Apple's
	  # Developer ID CA G2 carries -- in front of a leaf with OU=ABCDE12345
	  # produced a signature whose certificate was the CA and whose team id
	  # was G2; the same leaf and key alone gave ABCDE12345. Under
	  # --for-notarization both throwaways fail with the same two lines ("not
	  # signed by Apple", "doesn't appear to be such a certificate"), so
	  # rcodesign's own words name the certificate's TYPE when the cause is
	  # its ORDER. The Team ID tells them apart: Apple's are ten upper-case
	  # alphanumerics and a CA's OU is not. Nothing but the leaf and its key
	  # belongs in the file: rcodesign carries Apple's CA certificates itself
	  # (src/apple-certs/DeveloperIDG2CA.cer among them) and adds the chain
	  # from the leaf upward when it loads the key (chain_apple_certificates,
	  # called from cli/certificate_source.rs); `analyze-certificate` on a bare
	  # Apple-issued leaf prints that resolved chain as "Apple Issuing Chain",
	  # measured on the leaves the crate ships for its own tests.
	  CERTINFO="$$SCRATCH/certificate-0"
	  $(RCODESIGN) analyze-certificate --p12-file "$$P12" --p12-password-file "$$PWFILE" 2>/dev/null \
	    | awk '/^# Certificate 0$$/{f=1; next} /^# Certificate [0-9]/{f=0} /^-----BEGIN/{f=0} f' > "$$CERTINFO" || true
	  [ -s "$$CERTINFO" ] || { echo "ERROR: $(RCODESIGN) analyze-certificate printed no 'Certificate 0' for the p12."; exit 1; }
	  cfield() { sed -n "s/^$$1: *//p" "$$CERTINFO" | head -1; }
	  C_SUBJECT="$$(cfield 'Subject CN')";      C_ISSUER="$$(cfield 'Issuer CN')"
	  C_TEAM="$$(cfield 'Team ID')";            C_SELF="$$(cfield 'Subject is Issuer?')"
	  C_APPLE="$$(cfield 'Signed by Apple?')";  C_PROFILE="$$(cfield 'Guessed Certificate Profile')"
	  C_INTER="$$(cfield 'Is Apple Intermediate CA?')"; C_ROOT="$$(cfield 'Is Apple Root CA?')"
	  echo "    certificate 0: $$C_SUBJECT (team id $$C_TEAM, profile $$C_PROFILE, Apple-issued: $$C_APPLE)"
	  C_LEAF=; printf '%s' "$$C_TEAM" | LC_ALL=C grep -qxE '[A-Z0-9]{10}' && C_LEAF=1
	  # A CA in slot 0 is wrong on every path, and it looks like this: Apple's own
	  # intermediates rcodesign recognises by name, anything else's carries an OU
	  # that is not a team id. A self-signed certificate with NO team id (the
	  # throwaway this file's header makes prints 'unset'; one with no OU at all
	  # prints '<missing>') is not this case; it is handled on the Apple-facing
	  # path below and is a legitimate identity on the other one.
	  if [ "$$C_INTER" = true ] || [ "$$C_ROOT" = true ] \
	     || { [ -z "$$C_LEAF" ] && [ "$$C_TEAM" != "<missing>" ] && [ "$$C_TEAM" != unset ]; }; then
	    echo ""
	    echo "ERROR: Certificate 0 in the p12 is not your leaf -- a Team ID of '$$C_TEAM' is a"
	    echo "       CA's OU; rebuild the p12 with -inkey + -in <leaf> only, no -certfile."
	    echo "       rcodesign signs with the first certificate in the file, and OpenSSL"
	    echo "       writes a -certfile CA first. Apple's intermediates do not belong in"
	    echo "       the file: rcodesign carries them and chains them at sign time."
	    echo "         openssl pkcs12 -export -legacy -inkey <key.pem> -in <leaf.pem> -out squallar-signing.p12"
	    echo "       then replace MACOS_SIGNING_P12 with 'base64 -w0' of the result."
	    exit 1
	  fi
	  if [ -n "$$NOTARY_READY" ]; then
	    # The notary path takes exactly one kind of certificate, and the three
	    # ways to hold the wrong kind are three different errands, so each is
	    # named. rcodesign would refuse all three a moment later with the same
	    # two lines; this is what makes them different messages.
	    if [ "$$C_SELF" = true ]; then
	      echo ""
	      echo "ERROR: Certificate 0 is self-signed ($$C_SUBJECT): MACOS_SIGNING_P12 holds the"
	      echo "       throwaway identity the top of this file makes for the stable-identity"
	      echo "       fallback, and Apple notarizes only Developer ID Application certificates"
	      echo "       it issued. Either request one (steps 1-3 at the top) and replace the"
	      echo "       secret, or clear the three APPSTORE_CONNECT_* secrets to get the"
	      echo "       signed-but-not-notarized build back."
	      exit 1
	    elif [ "$$C_APPLE" != true ]; then
	      echo ""
	      echo "ERROR: Certificate 0 is a leaf for team $$C_TEAM, but it was not issued by Apple"
	      echo "       (issuer: $$C_ISSUER), and Apple notarizes only what its own CA signed."
	      echo "       A Developer ID Application certificate comes from developer.apple.com"
	      echo "       against a CSR: steps 1-3 at the top of this file."
	      exit 1
	    elif [ "$$C_PROFILE" != DeveloperIdApplication ]; then
	      echo ""
	      echo "ERROR: Certificate 0 is an Apple-issued '$$C_PROFILE' certificate, not a"
	      echo "       Developer ID Application one, and Apple notarizes only the latter."
	      echo "       Apple Development, Apple Distribution and Mac App Distribution are"
	      echo "       other products of the same portal page; request Developer ID"
	      echo "       Application (step 2 at the top), then rebuild the p12 (step 3)."
	      exit 1
	    fi
	  fi

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
	      echo "       the three APPSTORE_CONNECT_* secrets returns this build to the"
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
	SUBMISSION_ID=
	if [ -n "$$NOTARY_READY" ] && [ "$$IDENTITY" = devid ]; then
	  KEYP8="$$SCRATCH/AuthKey.p8"
	  # Both shapes accepted on purpose. The .p8 is PEM text and survives a
	  # GitHub secret intact, but a multi-line secret is the single easiest
	  # thing to paste wrong, and base64 is what the p12 secret next door
	  # already uses. Whichever arrives, the log says which was recognised, so
	  # a mangled secret is a named failure rather than a puzzling one.
	  # The quotes on the pattern are load-bearing. Under .ONESHELL, make strips
	  # every leading `-`, `@` and `+` from each recipe line before the shell
	  # sees it (GNU make 5.3.1; measured on 4.4.1: `-----BEGIN*)` reaches bash
	  # as `BEGIN*)`), so the bare spelling never matched a PEM key and every
	  # PEM-text secret died in the base64 arm as "neither PEM nor base64".
	  case "$${APPSTORE_CONNECT_KEY_P8}" in
	    "-----BEGIN"*)
	      echo "==> notary key: PEM text"
	      printf '%s\n' "$${APPSTORE_CONNECT_KEY_P8}" > "$$KEYP8" ;;
	    *)
	      echo "==> notary key: base64"
	      printf '%s' "$${APPSTORE_CONNECT_KEY_P8}" | base64 -d > "$$KEYP8" || {
	        echo "ERROR: APPSTORE_CONNECT_KEY_P8 is neither PEM text starting '-----BEGIN'"
	        echo "       nor valid base64. It should be the AuthKey_<KEY_ID>.p8 file"
	        echo "       Apple gave you, either verbatim or as 'base64 -w0'."
	        exit 1
	      } ;;
	  esac
	  grep -q 'BEGIN PRIVATE KEY' "$$KEYP8" || {
	    echo "ERROR: APPSTORE_CONNECT_KEY_P8 decoded to something that is not a PKCS#8"
	    echo "       private key. Apple's App Store Connect key is a PEM file whose"
	    echo "       first line is '-----BEGIN PRIVATE KEY-----'."
	    exit 1
	  }
	  KEYJSON="$$SCRATCH/notary-key.json"
	  $(RCODESIGN) encode-app-store-connect-api-key -o "$$KEYJSON" \
	      "$${APPSTORE_CONNECT_ISSUER_ID}" "$${APPSTORE_CONNECT_KEY_ID}" "$$KEYP8" >/dev/null

	  # What proves a staple happened. `notary-submit --staple` writes the
	  # ticket INTO the bundle directory; rather than assert a path this file
	  # would then have to keep in step with rcodesign, take the file list
	  # before and after and require that something appeared. It cannot pass
	  # vacuously, and it prints the path, so the next person reading this
	  # learns where the ticket actually went.
	  find "$$APP" -type f | sort > "$$SCRATCH/before.txt"
	  # What "stapled" is allowed to mean. `staple` writes the ticket INTO the
	  # bundle directory; rather than assert a path this file would then have
	  # to keep in step with rcodesign, take the file list before and after and
	  # require that something appeared. It cannot pass vacuously, and it
	  # prints the path. Fatal when nothing appeared, and the one thing on this
	  # path that is: Apple saying yes and the bundle gaining nothing is a
	  # disagreement between a claim and the disk, not an outage.
	  record_staple() {
	    find "$$APP" -type f | sort > "$$SCRATCH/after.txt"
	    STAPLED="$$(comm -13 "$$SCRATCH/before.txt" "$$SCRATCH/after.txt")"
	    [ -n "$$STAPLED" ] || {
	      echo "ERROR: notarization reported success but nothing was added to the"
	      echo "       bundle, so no ticket was stapled. The zip would ship a bundle"
	      echo "       that needs Apple reachable at first launch."
	      exit 1
	    }
	    NOTARY_STATUS=stapled
	    echo "    notarized and stapled:"
	    printf '%s\n' "$$STAPLED" | sed 's/^/      /'
	  }
	  # ---- 1. upload ----
	  # `notary-submit` WITHOUT --wait: it creates the submission, uploads the
	  # bundle to Apple's S3 and exits 0 the moment the upload completes
	  # (apple-codesign 0.29.0 src/notarization.rs upload_s3_and_maybe_wait
	  # returns UploadId when no wait limit is given). The wait is the recipe's
	  # own loop below, because rcodesign's wait is bounded by
	  # --max-wait-seconds and ends on the first transient error
	  # (wait_on_notarization: every get_submission is `?`), and this stage
	  # waits unconditionally. Output is teed to a file because the id and the
	  # outcome are read back off it; the strings matched are the ones the
	  # binary prints, cited by source line (src/notarization.rs; its log is
	  # stderr, bare lines -- src/cli/mod.rs main_impl turns env_logger's
	  # prefixes off). src/main.rs prints `Error: <msg>` and exits 1 for every
	  # Err, so an exit status alone never says which of these happened.
	  #   created submission ID: <uuid>       :277
	  #   S3 upload completed successfully    :338; without it an id can exist
	  #     that Apple will never process, and waiting on it would never end
	  #   poll state after <N>s: <Status>     :388; Status is the Debug form of
	  #     app-store-connect 0.7.0's SubmissionResponseStatus -- InProgress,
	  #     Accepted, Invalid, Rejected, Unknown
	  #   reached wait limit after <N>s       :401; one notary-wait call ended
	  #     with Apple still working, and nothing else happened
	  #   notary log> <line>                  :432; Apple's log, fetched by the
	  #     wait on ANY terminal state, Invalid included
	  echo "==> uploading to Apple's notary service"
	  NOTARY_LOG="$$OUT/notary.log"; : > "$$NOTARY_LOG"
	  if $(RCODESIGN) notary-submit --api-key-file "$$KEYJSON" "$$APP" 2>&1 | tee "$$NOTARY_LOG"; then
	    UPLOADED=1
	  else
	    UPLOADED=
	  fi
	  SUBMISSION_ID="$$(sed -n 's/.*created submission ID: \([0-9A-Fa-f-]*\).*/\1/p' "$$NOTARY_LOG" | head -1)"
	  grep -q 'S3 upload completed successfully' "$$NOTARY_LOG" || UPLOADED=
	  if [ -n "$$UPLOADED" ] && [ -n "$$SUBMISSION_ID" ]; then
	    echo "    submission id: $$SUBMISSION_ID"
	    # The recovery record, written the moment Apple has the bytes and
	    # before a second is spent waiting: the id, the word, and -- when a zip
	    # is wanted at all -- the bundle under the name that is true of it
	    # right now. A job cut off while waiting (GitHub's job bound, a lost
	    # runner) leaves these for the workflow to publish and for
	    # finish-notarization to complete without a rebuild; a job that
	    # finishes overwrites them below.
	    printf '%s\n' "$$SUBMISSION_ID" > "$$OUT/NOTARY_SUBMISSION_ID"
	    printf 'pending\n' > "$$OUT/NOTARIZATION"
	    if [ -n "$(ARCHIVE)" ] && command -v zip >/dev/null 2>&1; then
	      ( cd "$$OUT" && rm -f squallar-PENDING-NOTARIZATION.app.zip \
	          && zip -qry squallar-PENDING-NOTARIZATION.app.zip squallar.app )
	    fi
	    # ---- 2. wait, unconditionally ----
	    # One `notary-wait` call is bounded and ends on the first transient
	    # error, so "unconditionally" is this loop: ask again after every
	    # wait-limit exit, sleep and ask again after every other failure, stop
	    # only on a terminal state. `notary-wait` exits 0 on ANY terminal
	    # state, Invalid included -- src/cli/mod.rs NotaryWait::run never calls
	    # into_result -- so its exit status says nothing about the verdict;
	    # the verdict is the last poll line, read after the loop. The only
	    # outer bound is GitHub's job timeout (see NOTARY_WAIT_CALL).
	    echo "==> waiting for Apple's verdict on $$SUBMISSION_ID (unconditionally; one notary-wait call = $(NOTARY_WAIT_CALL)s)"
	    while :; do
	      if $(RCODESIGN) notary-wait --api-key-file "$$KEYJSON" \
	          --max-wait-seconds $(NOTARY_WAIT_CALL) "$$SUBMISSION_ID" 2>&1 | tee -a "$$NOTARY_LOG"; then
	        break
	      elif tail -n 3 "$$NOTARY_LOG" | grep 'reached wait limit after' >/dev/null; then
	        echo "==> Apple is still working on $$SUBMISSION_ID; asking again"
	      else
	        echo "==> the poll itself failed (above); Apple's queue is unaffected -- asking again in $(NOTARY_RETRY_SLEEP)s"
	        sleep $(NOTARY_RETRY_SLEEP)
	      fi
	    done
	  fi
	  VERDICT="$$(sed -n 's/.*poll state after [0-9]*s: \([A-Za-z]*\).*/\1/p' "$$NOTARY_LOG" | tail -1)"
	  if [ "$$VERDICT" = Accepted ]; then
	    # ---- 3. staple ----
	    # The ticket exists; `staple` fetches it by the main executable's
	    # cdhash from Apple's public ticket service (src/stapling.rs
	    # staple_bundle, src/ticket_lookup.rs APPLE_TICKET_LOOKUP_URL): no key,
	    # no resubmission. A fresh ticket can take a minute to reach that
	    # service, hence the attempts.
	    echo "==> Accepted; stapling"
	    n=0; STAPLE_OK=
	    while [ "$$n" -lt $(NOTARY_STAPLE_TRIES) ]; do
	      n=$$((n+1))
	      if $(RCODESIGN) staple "$$APP" 2>&1 | tee -a "$$NOTARY_LOG"; then STAPLE_OK=1; break; fi
	      echo "==> staple attempt $$n of $(NOTARY_STAPLE_TRIES) failed (above); again in $(NOTARY_RETRY_SLEEP)s"
	      sleep $(NOTARY_RETRY_SLEEP)
	    done
	    if [ -n "$$STAPLE_OK" ]; then
	      record_staple
	    else
	      NOTARY_STATUS=pending
	      echo ""
	      echo "NOTE: Apple ACCEPTED submission $$SUBMISSION_ID but the ticket could not be"
	      echo "      stapled in $(NOTARY_STAPLE_TRIES) attempts; the 'Error:' lines above say why. The"
	      echo "      ticket exists and needs no resubmission, so the bundle is published as"
	      echo "      PENDING-NOTARIZATION and stapling alone finishes it, with no rebuild"
	      echo "      and no secrets:"
	      echo ""
	      echo "        gh workflow run finish-notarization -f run_id=$${GITHUB_RUN_ID:-<run id>}"
	      echo ""
	      echo "      or 'rcodesign staple squallar.app' on the unzipped artifact, then re-zip."
	    fi
	  elif [ "$$VERDICT" = Invalid ] || [ "$$VERDICT" = Rejected ]; then
	    NOTARY_STATUS=rejected
	    echo ""
	    echo "WARNING: Apple REJECTED submission $$SUBMISSION_ID. That is a verdict on this"
	    echo "         bundle, not an outage: re-running this job will not change it."
	    echo "         Apple's reasons follow. NOT fatal on purpose -- see the note above;"
	    echo "         the bundle is written as NOT-NOTARIZED so the run leaves something"
	    echo "         to inspect."
	    # Fetched here, by the recipe, so the reasons are IN the run rather than
	    # behind an instruction to go and get them. The wait already printed the
	    # same document as `notary log>` lines; this one is the raw JSON, which
	    # is what the one-line-per-issue summary reads.
	    NOTARY_ISSUES="$$SCRATCH/notary-log.json"
	    echo "==> rcodesign notary-log $$SUBMISSION_ID (Apple's notary log, verbatim)"
	    if $(RCODESIGN) notary-log --api-key-file "$$KEYJSON" "$$SUBMISSION_ID" > "$$NOTARY_ISSUES"; then
	      cat "$$NOTARY_ISSUES"
	      echo "==> issues, one per line: severity | message | path"
	      if command -v jq >/dev/null 2>&1; then
	        jq -r '"    status: \(.status // "?") -- \(.statusSummary // "")", ((.issues // [])[] | "    \(.severity // "?") | \(.message // "?") | \(.path // "-")")' "$$NOTARY_ISSUES" \
	          || echo "    (jq could not read the log above; read it raw)"
	      elif command -v python3 >/dev/null 2>&1; then
	        python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print("    status: %s -- %s" % (d.get("status","?"), d.get("statusSummary",""))); [print("    %s | %s | %s" % (i.get("severity","?"), i.get("message","?"), i.get("path") or "-")) for i in (d.get("issues") or [])]' "$$NOTARY_ISSUES" \
	          || echo "    (python3 could not read the log above; read it raw)"
	      else
	        echo "    (neither jq nor python3 on PATH; read the JSON above)"
	      fi
	    else
	      echo "WARNING: 'rcodesign notary-log' did not return the log. The 'notary log>' lines"
	      echo "         above are the same document, fetched by the wait; failing that:"
	      echo "           rcodesign notary-log $$SUBMISSION_ID --api-key-file <key.json>"
	    fi
	  else
	    NOTARY_STATUS=error
	    echo ""
	    if [ -z "$$SUBMISSION_ID" ]; then
	      echo "WARNING: notary-submit failed before a submission existed; the 'Error:' line"
	      echo "         above says how. A 401 is the credentials (APPSTORE_CONNECT_ISSUER_ID /"
	      echo "         KEY_ID / KEY_P8 do not agree, or the key was revoked); a 5xx or a"
	      echo "         connection error is Apple or the network. Nothing is queued at Apple."
	    elif [ -z "$$UPLOADED" ]; then
	      echo "WARNING: submission $$SUBMISSION_ID was created but the upload to Apple did not"
	      echo "         complete (no 'S3 upload completed successfully' above), so Apple will"
	      echo "         never process it and waiting on it would never end. Push again: a"
	      echo "         fresh submission costs nothing."
	    else
	      echo "WARNING: Apple ended submission $$SUBMISSION_ID in state '$${VERDICT:-?}', which is"
	      echo "         neither Accepted nor a rejection this recipe knows; the lines above"
	      echo "         say more, and 'rcodesign notary-log $$SUBMISSION_ID --api-key-file"
	      echo "         <key.json>' prints Apple's record."
	    fi
	    echo "         NOT fatal on purpose -- see the note above; the bundle is written as"
	    echo "         NOT-NOTARIZED so the run leaves something to inspect."
	  fi
	elif [ -n "$$NOTARY_READY" ]; then
	  echo ""
	  echo "WARNING: the APPSTORE_CONNECT_* credentials are set, but this bundle was not"
	  echo "         signed with a Developer ID certificate ($$IDENTITY), and Apple"
	  echo "         only notarizes Developer ID signed software. Not submitting."
	else
	  # Four situations end here with the same un-notarized bundle, and the
	  # old one-line wording -- "the three secrets are not set" -- was wrong
	  # for three of them: the workflow withholds the key on purpose off
	  # main, a per-architecture row never notarizes by design, and one
	  # missing name is not three. The marker is set by the workflow and by
	  # nothing else; the secret facts are read here by name, never by value.
	  # The word written to NOTARIZATION carries the same distinction so the
	  # run summary can say "withheld" and not "skipped".
	  echo ""
	  case "$${APPSTORE_CONNECT_WITHHELD:-}" in
	    per-arch-row)
	      NOTARY_STATUS=per-arch
	      echo "NOTE: per-architecture build, never notarized here by design; the"
	      echo "      macos-universal job fuses both halves and notarizes that one --"
	      echo "      look there for the notarized artifact."
	      ;;
	    not-main-push)
	      NOTARY_STATUS=withheld
	      echo "NOTE: notarization credentials withheld: this is a $${GITHUB_EVENT_NAME:-?} on"
	      echo "      $${GITHUB_REF:-?}, and they are passed only on a push to main. The"
	      echo "      secrets may well be configured; this run was never going to use them."
	      ;;
	    "")
	      if [ "$$IDENTITY" = cert ]; then
	        FACTS=""; PRESENT=""
	        for n in APPSTORE_CONNECT_ISSUER_ID APPSTORE_CONNECT_KEY_ID APPSTORE_CONNECT_KEY_P8; do
	          if [ -n "$${!n:-}" ]; then FACTS="$$FACTS; $$n: set"; PRESENT=1
	          else FACTS="$$FACTS; $$n: missing"; fi
	        done
	        if [ -n "$$PRESENT" ]; then
	          NOTARY_STATUS=partial
	          echo "NOTE: signed, NOT notarized -- the App Store Connect key is only partly"
	          echo "      configured, and it takes all three names:"
	          echo "      $${FACTS#; }"
	          echo "      Add the missing one(s); see the note at the top of this file."
	        else
	          echo "NOTE: signed, NOT notarized -- none of the three APPSTORE_CONNECT_* secrets"
	          echo "      is set. On the machine that built it this app is fine. On anyone"
	          echo "      else's Mac, Gatekeeper will refuse it and offer no plain way"
	          echo "      through; see the note at the top of this file for what to create."
	        fi
	      else
	        echo "NOTE: not notarized (nothing to notarize an ad-hoc bundle with). Another"
	        echo "      person's Mac will refuse to open this; that is expected for a"
	        echo "      build with no secrets, and it still runs on the machine that made it."
	      fi
	      ;;
	    *)
	      NOTARY_STATUS=withheld
	      echo "NOTE: notarization credentials withheld by the workflow"
	      echo "      (APPSTORE_CONNECT_WITHHELD=$${APPSTORE_CONNECT_WITHHELD}); nothing was submitted."
	      ;;
	  esac
	fi

	# ---- outcome, for the workflow ----
	# The one-word outcome, written beside the zip rather than inside the .app:
	# anything added inside the bundle after signing breaks the seal. CI reads
	# this for its run summary; a person reading a build directory gets the
	# same answer without having to remember what the zip's name means.
	printf '%s\n' "$$NOTARY_STATUS" > "$$OUT/NOTARIZATION"
	# And the submission id beside it -- empty when nothing was submitted -- so
	# the run summary can name the thing to wait on or look up without anyone
	# scrolling a fold for it. Both were already written the moment the
	# upload completed (the recovery record); this is the final word.
	printf '%s\n' "$$SUBMISSION_ID" > "$$OUT/NOTARY_SUBMISSION_ID"

	# ---- package (CHANNEL=store): a .pkg, and never a .zip ----
	# App Store Connect takes a macOS build as an installer package and has no
	# .app or .zip spelling, so this channel's artifact is not the direct
	# channel's artifact under another name. The .pkg is built by mkpkg.sh
	# (cpio + mkbom + xar; see that file for why none of it needs a Mac) and
	# then signed as a whole -- `rcodesign sign` signs a XAR pkg installer
	# directly. A pkg is a container: signing it does NOT sign the app inside
	# it, which is why the bundle was signed and provisioned above first.
	#
	# The name carries the same discipline the zip names do one branch down.
	# `squallar.pkg` asserts three things at once -- the app is signed with an
	# Apple Distribution certificate, it carries an embedded provisioning
	# profile, and the package itself is signed with a Mac Installer
	# Distribution certificate -- because all three are required and any one
	# missing is an upload App Store Connect rejects. Every other outcome is
	# `squallar-NOT-UPLOADABLE.pkg`, which says what is true of it rather than
	# leaving a reader to find out from Apple twenty minutes later.
	if [ "$(CHANNEL)" = store ]; then
	  PKG_OK=1
	  [ "$$IDENTITY" = store ] || { PKG_OK=; echo "note: app not signed for the store, so the .pkg cannot be uploaded."; }
	  [ -f "$$APP/Contents/embedded.provisionprofile" ] || { PKG_OK=; echo "note: no embedded.provisionprofile, so TestFlight would reject this (ITMS-90889)."; }
	  [ -x "$(MKPKG)" ] || { echo "ERROR: $(MKPKG) is missing or not executable."; exit 1; }
	  PKGNAME=squallar-NOT-UPLOADABLE.pkg
	  ( cd "$$OUT" && rm -f squallar.pkg squallar-NOT-UPLOADABLE.pkg )
	  # MACOS_MIN goes in as the product definition's OS floor: App Store
	  # Connect requires it to EQUAL the LSMinimumSystemVersion above (90264),
	  # and this is the same variable that was written there.
	  "$(MKPKG)" "$$APP" "$$BUNDLE_ID" "$$MARKETING_VERSION" "$$CURRENT_PROJECT_VERSION" \
	      "$$MACOS_MIN" "$$OUT/squallar-unsigned.pkg"
	  # The installer certificate is a SECOND certificate and a different kind
	  # from the one that signed the app: Apple issues "Mac Installer
	  # Distribution" (it appears in a keychain as `3rd Party Mac Developer
	  # Installer:`) only for signing packages, and an app-signing certificate
	  # cannot stand in for it. Without it the package is still well formed
	  # and still says so -- it just cannot be uploaded.
	  if [ -n "$${MACOS_INSTALLER_P12:-}" ] && [ -n "$$PKG_OK" ]; then
	    IPKG="$$SCRATCH/installer.p12"
	    printf '%s' "$${MACOS_INSTALLER_P12}" | base64 -d > "$$IPKG" 2>/dev/null \
	      || { echo "ERROR: MACOS_INSTALLER_P12 is not base64."; exit 1; }
	    IPWFILE="$$SCRATCH/installer.pw"
	    printf '%s' "$${MACOS_INSTALLER_P12_PASSWORD:-}" > "$$IPWFILE"
	    $(RCODESIGN) sign --p12-file "$$IPKG" --p12-password-file "$$IPWFILE" \
	        "$$OUT/squallar-unsigned.pkg" "$$OUT/squallar.pkg" \
	      && PKGNAME=squallar.pkg \
	      || { echo "ERROR: signing the .pkg with the installer certificate failed."; exit 1; }
	    rm -f "$$OUT/squallar-unsigned.pkg"
	  else
	    [ -n "$${MACOS_INSTALLER_P12:-}" ] || echo "note: no MACOS_INSTALLER_P12, so the .pkg is unsigned."
	    mv "$$OUT/squallar-unsigned.pkg" "$$OUT/$$PKGNAME"
	  fi
	  echo "Pkg: $(ROOT)/$$OUT/$$PKGNAME"
	  if [ "$$PKGNAME" = squallar.pkg ]; then
	    echo "Upload it with:"
	    echo "  .github/scripts/appstore-upload.py --platform MAC_OS --pkg $$OUT/squallar.pkg"
	  else
	    echo "      This package cannot be uploaded to App Store Connect. See the notes above."
	  fi
	elif [ -n "$(ARCHIVE)" ]; then
	  # actions/upload-artifact drops the executable bit, which turns a .app
	  # into a directory macOS declines to launch. A zip carries the mode.
	  #
	  # LAST, and after stapling: a .zip cannot hold a notarization ticket
	  # ("do not know how to staple", measured), so it has to be built around a
	  # bundle that already has one. Moving this above the notarize stage would
	  # ship an un-ticketed zip and nothing would say so.
	  #
	  # The NAME is the label, and it asserts only what is true of the bundle.
	  # `squallar.app.zip` means a ticket is stapled inside and any Mac will
	  # open it. `squallar-PENDING-NOTARIZATION.app.zip` means Apple has the
	  # bytes and no ticket is stapled yet: signed, submitted, notarized by
	  # stapling alone once the ticket exists (the `finish-notarization`
	  # workflow). Since the wait is unconditional, a finished job leaves it
	  # only when Apple accepted and the ticket could not be fetched; a job
	  # cut off mid-wait leaves the copy written before the wait began (the
	  # recovery record above). `squallar-NOT-NOTARIZED.app.zip` is every
	  # outcome where that word is true -- no credentials, an ad-hoc
	  # signature, Apple saying no. The pending name exists because a bundle
	  # Apple had ACCEPTED was once published as NOT-NOTARIZED with a summary
	  # saying it must not ship: a name that asserts a fact that can be false
	  # is how a reader works it out wrong.
	  command -v zip >/dev/null 2>&1 || { echo "ERROR: ARCHIVE=1 but no zip on PATH"; exit 1; }
	  case "$$NOTARY_STATUS" in
	    stapled) ZIPNAME=squallar.app.zip ;;
	    pending) ZIPNAME=squallar-PENDING-NOTARIZATION.app.zip ;;
	    *)       ZIPNAME=squallar-NOT-NOTARIZED.app.zip ;;
	  esac
	  ( cd "$$OUT" \
	      && rm -f squallar.app.zip squallar-PENDING-NOTARIZATION.app.zip squallar-NOT-NOTARIZED.app.zip \
	      && zip -qry "$$ZIPNAME" squallar.app )
	  echo "Zip: $(ROOT)/$$OUT/$$ZIPNAME"
	fi
	echo ""
	echo "App: $(ROOT)/$$APP   (notarization: $$NOTARY_STATUS$${SUBMISSION_ID:+, submission $$SUBMISSION_ID})"
	echo "Run it with:  open '$(ROOT)/$$APP'"

clean:
	@rm -rf "$(ROOT)/packaging/macos/build" "$(ROOT)/$(TARGET_DIR)"
