#!/usr/bin/env bash
# Staple a notarization ticket into a bundle that a build run published as
# PENDING-NOTARIZATION, and re-zip it under the stapled name.
#
#   finish-notarization.sh <download-dir> <out-dir>
#
# <download-dir> holds what actions/download-artifact unpacked: one .app.zip
# and, beside it, NOTARIZATION and NOTARY_SUBMISSION_ID as the build wrote
# them. An artifact from before those two rode along lacks them (the one
# real NOT-NOTARIZED artifact Apple had in fact accepted is such a one); the
# script then cannot name the submission and says so. <out-dir> receives
# squallar.app.zip -- the stapled bundle -- plus the two files, NOTARIZATION
# now reading `stapled`.
#
# Exit 1 with STILL PENDING when Apple's ticket service has no ticket for
# this bundle yet: the job is red on purpose, and the fix is to re-run it
# later. Exit 1 with a different message for anything else. Nothing is ever
# written under the stapled name unless the bundle demonstrably gained a
# file, the same gate packaging/macos/Makefile applies in-run.
#
# Why this needs no credential and no rebuild: `rcodesign staple` derives
# the ticket's record name from the main executable's code-directory digest
# and looks it up at Apple's public ticket service (apple-codesign 0.29.0
# src/stapling.rs staple_bundle -> lookup_ticket_for_executable_bundle,
# src/bundle_signing.rs notarization_ticket_record_name, src/ticket_lookup.rs
# lookup_notarization_tickets: a bare POST, no token). The bundle it staples
# must be byte-for-byte the one Apple examined, which the artifact is; the
# lookup answers "no such record" for any other bytes, and for these bytes
# until Apple has issued the ticket.
set -euo pipefail

IN=${1:?usage: finish-notarization.sh <download-dir> <out-dir>}
OUT=${2:?usage: finish-notarization.sh <download-dir> <out-dir>}
RCODESIGN=${RCODESIGN:-rcodesign}

fail() { echo "ERROR: $*" >&2; exit 1; }
summary() { [ -z "${GITHUB_STEP_SUMMARY:-}" ] || printf "$@" >> "$GITHUB_STEP_SUMMARY"; }

zips=("$IN"/*.zip)
[ "${#zips[@]}" -eq 1 ] && [ -f "${zips[0]}" ] \
  || fail "expected exactly one .zip in $IN, found: $(ls -1 "$IN" 2>/dev/null | tr '\n' ' ')"
ZIP=${zips[0]}
SUBMISSION_ID=$( (head -1 "$IN/NOTARY_SUBMISSION_ID" 2>/dev/null) || true)
REASON=$( (head -1 "$IN/NOTARIZATION" 2>/dev/null) || true)
echo "==> $ZIP  (NOTARIZATION=${REASON:-<absent>}, submission ${SUBMISSION_ID:-<absent>})"
case "$REASON" in
  stapled)  fail "this artifact already reads NOTARIZATION=stapled; nothing to finish." ;;
  rejected) fail "this artifact reads NOTARIZATION=rejected: Apple returned a verdict on it, and stapling cannot change that." ;;
esac

rm -rf "$OUT"; mkdir -p "$OUT"
( cd "$OUT" && unzip -q "$ZIP" )
APP="$OUT/squallar.app"
[ -d "$APP" ] || fail "$ZIP did not unpack to squallar.app"

find "$APP" -type f | sort > "$OUT/.before"
LOG="$OUT/.staple.log"
echo "==> rcodesign staple $APP"
if "$RCODESIGN" staple "$APP" 2>&1 | tee "$LOG"; then
  find "$APP" -type f | sort > "$OUT/.after"
  STAPLED=$(comm -13 "$OUT/.before" "$OUT/.after")
  [ -n "$STAPLED" ] \
    || fail "staple reported success but nothing was added to the bundle; not publishing it as stapled."
  echo "    stapled:"; printf '%s\n' "$STAPLED" | sed 's/^/      /'
  ( cd "$OUT" && rm -f squallar.app.zip && zip -qry squallar.app.zip squallar.app )
  printf 'stapled\n' > "$OUT/NOTARIZATION"
  printf '%s\n' "$SUBMISSION_ID" > "$OUT/NOTARY_SUBMISSION_ID"
  rm -rf "$APP" "$OUT/.before" "$OUT/.after" "$LOG"
  echo "==> $OUT/squallar.app.zip is the stapled bundle"
  summary '### macOS: notarized (finished)\n\nThe ticket for submission `%s` is stapled into the bundle from run `%s`; `macos-universal` on this run is the stapled `squallar.app.zip`.\n' \
    "${SUBMISSION_ID:-?}" "${SOURCE_RUN_ID:-?}"
  exit 0
fi

# The lookup answered and had no ticket for this cdhash. Apple's service
# returns a failure record (apple-codesign NotarizationLookupFailure:
# "notarization ticket lookup failure: <code>: <reason>") or no record at all
# (NotarizationRecordNotInResponse: "notarization record not in response").
# Either is "not yet": nothing was written, and the bundle is untouched.
if grep -qE 'notarization ticket lookup failure|notarization record not in response' "$LOG"; then
  rerun="gh workflow run finish-notarization -f run_id=${SOURCE_RUN_ID:-<source run id>}"
  case "${ARTIFACT:-}" in ""|macos-universal-PENDING-NOTARIZATION) ;; *) rerun="$rerun -f artifact=$ARTIFACT" ;; esac
  echo ""
  echo "STILL PENDING: Apple's ticket service has no ticket for this bundle yet."
  echo "  submission: ${SUBMISSION_ID:-<not recorded in this artifact; the source run summary names it>}"
  echo "  This job is red on purpose. Re-run it later:"
  echo "    $rerun"
  echo "  Or ask Apple directly: rcodesign notary-wait ${SUBMISSION_ID:-<id>} --api-key-file <key.json>"
  echo "  (Invalid there means rejected, and 'rcodesign notary-log ${SUBMISSION_ID:-<id>} --api-key-file <key.json>' prints why.)"
  summary '### macOS: still pending\n\nApple has no ticket yet for the bundle from run `%s` (submission `%s`). Not a verdict. Re-run later:\n\n    %s\n' \
    "${SOURCE_RUN_ID:-?}" "${SUBMISSION_ID:-?}" "$rerun"
  exit 1
fi
fail "rcodesign staple failed for a reason other than a missing ticket; its Error: line is above. Nothing was published."
