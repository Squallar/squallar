#!/usr/bin/env bash
#
# run_tier2.sh -- the Tier-2 browser gate: serve the built squallar-web bundle
# on a fresh port and drive the full PWA in Chromium and Firefox. FOUR
# passes per browser by default -- FIVE since the `wide` leg landed on
# 2026-08-31 (the roster grew at WO-5 and twice on 2026-08-31; a report quoting
# the old "4/4" leg count predates the gesture leg, one quoting "three passes"
# predates the long leg, and one quoting four predates `wide` -- the default
# count is 10), plus a SIXTH that is built and deliberately not in the default
# roster yet:
#
#   live      the app against LIVE network; asserts boot, canvas non-blank,
#             rAF sane, zero panics/errors, AND the worker wire: >=1
#             "rasterization worker attached" plus >=1 "took N ms off the
#             frame" job reply within 180 s (m4 -- a booted page with a dead
#             wire passes every weaker check), AND the self-hosted vector
#             basemap: >=1 archive tile body DECODED (--expect-basemap-tiles;
#             a page whose basemap decodes nothing passes every one of the
#             above, which is how one shipped).
#   doctored  serve.py --doctor-first-worker hands the FIRST /worker.js
#             request a stub posting a doctored build token; asserts the page
#             logs "rasterization worker is a different build", terminates,
#             and >=1000 ms later (first backoff rung) attaches the REAL
#             refetched worker (m5), and then the m4 round-trip on top.
#   gesture   a short leg of REAL input through the driver's W3C /actions
#             endpoint (pointer drags + wheel notches, ~4 s), asserting
#             --expect-interaction-frames: the app's own scraped
#             `frame service (interact)` count STRICTLY INCREASED. A count
#             assert and nothing else -- no ms figure gates here or anywhere
#             in this gate. This is WO-1's deferred mechanical non-vacuity
#             check: driven frames really tag as interaction, end to end
#             through the browser's input pipeline.
#   long      A HUNDRED AND FIFTY seconds of scripted time with an overlay
#             LOOP playing and
#             NO input at all, asserting --expect-frame-progress: the app's own
#             cumulative `frame cadence` count was still climbing over the last
#             20 s of the leg. Every other leg here is under 30 s and every
#             other liveness check is a pixel or a rAF delta, and that
#             combination hid a shipped freeze for the length of a campaign. A
#             98 MB infallible allocation in the MRMS decoder aborted the
#             module against wasm32's 1 GiB memory ceiling, and because this
#             target is `panic-strategy = "abort"` nothing unwound: winit's web
#             event loop kept its own `RefCell` borrowed for the life of the
#             page. The frame loop stopped for good while the canvas held its
#             last painted frame, rAF kept firing at display rate and the
#             network kept resolving, so Tier-2 read panics=0 on all six legs
#             of the build that did it. THE ONLY THING THAT CAN SEE THAT IS
#             THE APP'S OWN COUNTER STILL MOVING -- a screenshot cannot and
#             neither can rAF. The leg also fails on a wasm TRAP (`unreachable
#             executed`, or an unhandled rejection carrying a wasm stack),
#             which on this target is a DIFFERENT signal from a panic and was
#             gated nowhere before.
#   wide      The same `--expect-frame-progress` assertion at the USER'S canvas
#             -- 2878x1651, world zoom, every radar site on the glass -- on ONE
#             overlay and 60 s. A user-reported freeze ("zooming out quite a
#             bit can freeze it up") reproduced 4 of 4 there and 0 of 2 at this
#             rig's default 1280x900, so `long` above could not see it and did
#             not: the driver is the tile SPAN a wide viewport asks for, not
#             the layer count or the elapsed time. See WIDE_SEED_LS.
#   huge      NOT IN THE DEFAULT ROSTER -- opt in with
#             RIG_LEGS="live doctored gesture long wide huge". The `long` leg's
#             scene at the CANVAS SIZE A USER ACTUALLY HAS -- 2878x1651,
#             corrected for and then ASSERTED with --expect-canvas. 45 s with
#             --expect-frame-progress. Every other leg here runs at the
#             browser's default (measured on this box: 1280x757 chromium,
#             1280x815 firefox), and a winit `RefCell already borrowed` freeze
#             reproduces at 2878x1651 and not at the default, so the roster was
#             structurally blind to it rather than unlucky.
#
#             IT IS HELD OUT ON PURPOSE AND THIS IS NOT AN OVERSIGHT. The leg
#             works -- it reproduced that freeze on both browsers on both
#             attempts the first time it ran -- and the defect it finds is
#             live and owned by another lane. Putting it in the default roster
#             now would redden this gate for every session in this tree over a
#             bug none of them introduced. It goes into the default the moment
#             the scene can go green, and at that point it is the thing that
#             proves the fix. Same terms the `long` leg carried.
#
#             --expect-canvas is not decoration: without it a leg that could
#             not be made that big (a virtual screen smaller than the target --
#             see HUGE_WINDOW below) passes while claiming a size it never
#             rendered at, which is the same "verdict the rig has not earned"
#             this file's run-id binding exists to end.
#   wideloop  NOT IN THE DEFAULT ROSTER -- opt in with RIG_LEGS="wideloop".
#             THE USER'S OWN CONFIGURATION, which until this leg no row here
#             expressed: the `long` leg's scene (all seventeen layers, MRMS and
#             GMGSI among them, and a PLAYING loop) at the `wide` leg's canvas
#             AND at its world zoom -- 2878x1651 asked, 2878x1566 met, zoom 3.0
#             centred on the CONUS. Every existing leg has a strict subset of
#             that:
#
#               long       the layers and the loop, at the browser default
#                          (1280x757 chromium / 1280x815 firefox), default zoom
#               wide       the canvas and the world zoom, TWO layers, no loop
#               huge       the layers, the loop and the canvas -- and the
#                          pane's own zoom over KTLX, not the world view
#
#             The union is the configuration a user reported the app freezing
#             the whole browser in, and the gap it closes is the zoom: a
#             world-zoom viewport at 2878 points projects far more gridded
#             points and asks for a tile span neither `long` nor `huge` does,
#             on top of the loop's granules. Whether that is WORSE than `long`
#             was unmeasured, and unmeasurable, until this leg existed.
#
#             It samples. Every other leg here reads its levels at a handful
#             of moments, which is enough for a question about a level that
#             stays readable; this scene DIES, and after the trap nothing moves
#             again. Measured on the reproducing `long` runs: the overlay grids
#             saturate near 210 MiB by t+6 s, decoded loop volumes then arrive
#             at ~100 MB/s onto a page already past 600 MiB, and 1024 MiB is
#             reached at t+10.3 s -- a fatal window about four seconds wide. So
#             `--sample-tsv` writes one row every two seconds (the app's own
#             telemetry tick) through the settle AND the data window, carrying
#             the app's `frame cadence`, both `linear` figures WITH the two
#             `heap max` ceilings beside them, the four admission counters, the
#             census's overlay-grid and loop-scan bytes, and this box's
#             1-minute load average at that instant. A leg sampling slower
#             reports the death without the approach, and the approach is the
#             finding.
#
#             THE TSV CARRIES ITS OWN IDENTITY. Commit, wall clock, the sha256
#             of the seed that produced the scene and the sha256 of the wasm
#             that ran it are written into the file before the browser starts
#             (`sample_header_lines`). A name is not an identity: a directory
#             letter standing in for a commit and an mtime standing in for a
#             leg start both stood for an hour on 2026-09-07 because nothing
#             inside either artefact could contradict them.
#
#             TWO ASSERTIONS BEYOND THE ROSTER'S USUAL, and they are two
#             readings of one wall, never added: --expect-linear-headroom 64
#             (the page instance stayed 64 MiB clear of the ceiling THE APP
#             REPORTS, not of a number typed here) and
#             --expect-no-alloc-failure (no `alloc failed:` line reached either
#             ring). The trap count already gates the corpse; these two name
#             the cause.
#
#             HELD OUT OF THE DEFAULT ROSTER on `huge`'s terms and for `huge`'s
#             reason: the defect it reproduces is live and owned elsewhere, and
#             defaulting it now would redden this gate for every session in
#             this tree over a bug none of them introduced. It goes into the
#             default the day this scene can go green, and at that point it is
#             the thing that proves the fix.
#   tilecache NOT IN THE DEFAULT ROSTER -- opt in with RIG_LEGS="tilecache".
#             One pane at zoom 14 over a dense city core, the basemap on and
#             nobody touching the page, and ONE assert beyond liveness: over
#             the last 30 s the tile cache refetched nothing it had evicted,
#             the GPU store uploaded no mesh and the archive decoded no body
#             (--expect-tile-cache-settles). Two sizes, chosen by
#             RIG_TILECACHE_WINDOW: the user's 2878x1651 canvas (default; the
#             repro, asserted with --expect-canvas like `huge`) and 1280x900
#             (the control). See the TILECACHE block below for why it is held
#             out and where it writes.
#
# EVERY VERDICT IS BOUND TO THE RUN THAT PRODUCED IT (2026-08-31). Each attempt
# mints a run id, hands it to drive.py, and the summary refuses any artefact
# that does not carry it back; artefacts are wiped before a leg starts; the
# driver's exit code separates "ran and failed" from "never started". Before
# this, the summary printed whatever JSON happened to be on disk under the
# leg's name, and a leg that died on an argparse error in under a second was
# reported as PASS. See the block above `LEDGER=` for the full account -- it is
# the reason this gate is a gate. The summary now prints FOUR numbers
# (passed / failed / did-not-run / infrastructure) and only the first being
# equal to the leg count is a green run.
#
# Network posture (campaign-resolved): LIVE network, one auto-retry per pass
# as the flake-quarantine policy -- the app's own backoff machinery is part of
# what is exercised. A pass that fails twice fails the leg.
#
# The scene is pinned by seeding localStorage with a PANE on KTLX before any
# app script runs (browsers would otherwise pick different default sites).
# There is no app-wide site to seed since WO-SITE: a pane carries its own.
#
# Adapted from the 2026-08-18 measurement rig's run_smoke.sh; this copy is the
# permanent CI gate. Paths derive from the script location (this file lives at
# .github/browser-rig/ inside the repo), never from any scratchpad.
#
# Usage: run_tier2.sh [--skip-build]
#   --skip-build      serve squallar-web as-is (CI builds in its own step; the
#                     default is a fresh `wasm-pack build` first)
#
# Environment knobs (all optional):
#   SQUALLAR_WEB_DIR   dir to serve   (default <repo>/squallar-web)
#   RIG_OUT_DIR       output dir     (default <rig>/out)
#   RIG_CHROMEDRIVER  chromedriver   (default: chromedriver on PATH, else
#                                     /usr/bin/chromedriver)
#   RIG_GECKODRIVER   geckodriver    (default: $(ensure-geckodriver.sh))
#   RIG_FRAMES        rAF deltas per sample (default 120)
#   RIG_BROWSERS      "chromium firefox" (default), or a subset
#   RIG_EXPECT_TIMEOUT  seconds for the worker-wire assertions (default 180)
#
# BACKEND: each leg prints the backend the APP selected, scraped from its own
# `wgpu selected the <Backend> backend` startup line. That is a different fact
# from the `webgpu=` cap line beside it: the latter says what the browser
# offers, the former what the build took. Since WS4 the build asks for
# `BROWSER_WEBGPU | GL` and wgpu's detecting constructor settles it with a real
# requestAdapter(), so a browser that exposes `navigator.gpu` and answers null
# reports `webgpu=object-but-no-adapter` and `app selected backend=Gl`.
#   RIG_DRIVE_EXTRA   extra args appended to every drive.py call
#   RIG_SERVE_EXTRA   extra args appended to every serve.py launch
#   RIG_EXPECT_RAYON_THREADS  minimum worker rayon threads (default 2; 0 to
#                     report without gating)
#   RIG_EXPECT_ZERO_COPY  1 (default) to fail a leg whose replies were copied
#                     out of the worker's memory; 0 to report without gating
#   RIG_EXPECT_OVERLAY_RASTERS  1 (default) to fail a leg where the
#                     whole-picture overlay path never completed; 0 to report
#                     the totals without gating
#   RIG_EXPECT_BASEMAP_TILES  1 (default) to fail the LIVE leg where the
#                     self-hosted vector basemap decoded no tile; 0 to report
#                     the totals without gating
#
# WS2 BASELINE: every leg prints THREE tile/raster figures, and no two of them
# are ever added together, because they have three different denominators.
# `overlay rasters` is the whole-picture overlay dispatch alone -- radar's own
# pipeline and the loop frames are not in it. `texture uploads` is what the
# device was then made to move for EVERY egui texture, radar, basemap tiles and
# font atlas included. `basemap tiles` is archive tile BODIES DECODED, which is
# in neither of the other two: a vector body uploads no texture at all, and a
# raster body is one egui texture and so is a SUBSET of `texture uploads`
# rather than a term to add to it. These are the numbers a world-anchored tile
# grid will be judged against, so they are reported whether or not anything
# gated on them, per browser, and never pooled across browsers.
#
# WHY THE BASEMAP HAS ITS OWN GATE. A `usize`->`u64` offset widening in the
# vendored PMTiles reader made the self-hosted basemap serve ZERO tiles in a
# browser for as long as that build shipped, and this rig passed every leg the
# whole time. Nothing here read the basemap: a page with no ground under the
# map still boots, still reports a non-blank canvas (the overlays paint), still
# attaches its worker, still answers jobs off the frame, and still satisfies
# all six conjuncts of --expect-overlay-rasters, which does not include the
# basemap. --expect-basemap-tiles is the missing reading.
#
# WS3b made cross-origin isolation part of the DEFAULT posture: serve.py is
# launched with --coep on every pass, because the shipped app needs a
# SharedArrayBuffer to put rayon's pool on and production serves the headers
# (CloudFront Response Headers Policy on squallar.app). The gate then
# asserts the pool exists -- `--expect-rayon-threads 2` -- which is what keeps
# the header honest: the app degrades to a one-thread pool silently, and every
# other assertion here passes in that state.
#
# The fuller cross-origin-isolation proof (WS3a) adds the service-worker half
# -- serve the headers AND let the real SW through, then assert both sides
# rather than trusting the header was honoured:
#
#   RIG_SERVE_EXTRA="--no-block-sw" \
#   RIG_DRIVE_EXTRA="--expect-cross-origin-isolated --expect-service-worker" \
#     run_tier2.sh --skip-build
#
# Without --expect-cross-origin-isolated that run proves nothing: a browser
# that ignored the headers looks exactly like one that honoured them.
#
# AN OPEN GAP IN WHAT THIS GATE CAN DECIDE, and it is not a caveat on one leg.
#
# Because this gate is the software arm permanently and by design (see ADAPTER
# below), it is STRUCTURALLY UNABLE TO SEPARATE A PRODUCT DEFECT IN THE TEXTURE
# UPLOAD QUEUE FROM llvmpipe BEING SLOW AT A LARGE CANVAS. The mechanism is not
# subtle. On a ringless device -- all of web -- the drain moves ONE 4 MiB band
# per frame, so its throughput ceiling is `4 MiB x the frame rate`. The frame
# rate here is the software rasteriser's: measured on the `huge` firefox arm,
# prep is 933 us a pass (tessellate 577, upload apply 214, buffers 141) against
# an rAF p50 of **261 ms**, so prep is 0.36% of the frame and the other 99.6% is
# llvmpipe filling 2878x1651 = 4.75 Mpx. The chain is therefore
#
#   software rasteriser -> ~4.5 fps -> a drain ceiling of ~18 MB/s -> the queue grows
#
# and at 60 fps that ceiling is thirteen times higher. **Nothing measured on
# this rig establishes that `upload pending` builds up on a real driver at
# all**, and no leg that can be added here would establish it, because the
# adapter is the thing held fixed.
#
# THE ARM THAT CLOSES IT IS `run_gpu_arm.sh`, which is a measurement arm and not
# a gate, and it has NEVER RUN THIS SCENE. Until it has, every `upload pending`
# finding in this file is scoped to software and must say so. The two arms are
# never merged and each prints its own renderer, which is exactly why this gap
# can be stated precisely rather than argued.
#
# What a future reader can do WITHOUT a hardware arm: the sampler writes
# `upload_pending_b` and `cadence_n` on the same row, so the frame rate is a
# subtraction and the drain ceiling is derivable per row. A queue that grows
# while the frame rate is healthy is a product defect; a queue that grows only
# as the frame rate collapses is the rasteriser. That pairing is the reading to
# take, and it is why those two columns sit together.
#
# ADAPTER: this gate is the rig's SOFTWARE arm, deliberately and permanently.
# Chromium runs on SwiftShader and Firefox on Xvfb -> Mesa llvmpipe, so it is
# deterministic and runs on a CI box with no GPU. Every cap it reports
# (MAX_TEXTURE_SIZE and friends) therefore describes a software rasteriser and
# must never be promoted into a budget. The real-driver figures come from
# run_gpu_arm.sh, which is a measurement arm and not a gate. The two are never
# merged; each prints its arm and renderer beside every cap it quotes.

set -u -o pipefail   # not -e: attempt every leg and still summarise

RIG_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$RIG_DIR/../.." && pwd)"
WEB_DIR="${SQUALLAR_WEB_DIR:-$REPO_ROOT/squallar-web}"
OUT_DIR="${RIG_OUT_DIR:-$RIG_DIR/out}"
CHROMEDRIVER="${RIG_CHROMEDRIVER:-$(command -v chromedriver || echo /usr/bin/chromedriver)}"
FRAMES="${RIG_FRAMES:-120}"
BROWSERS="${RIG_BROWSERS:-chromium firefox}"
# The legs each browser runs, and the DEFAULT is what "the Tier-2 gate" means.
# `huge` exists and is not in it -- see the roster note in the header for why,
# and opt in with RIG_LEGS="live doctored gesture long huge".
#
# This knob can SHRINK the roster as well as grow it, which is a hazard in its
# own right: a run of one leg still prints a tally, and "1/1 PASS" is a
# sentence somebody could quote. The summary therefore prints the roster it
# actually ran and says so loudly when it was not the default -- a reduced run
# is a debugging aid and is never the gate.
DEFAULT_LEGS="live doctored gesture long wide"
LEGS="${RIG_LEGS:-$DEFAULT_LEGS}"
EXPECT_TIMEOUT="${RIG_EXPECT_TIMEOUT:-180}"
# Set to 0 to report the pool without gating on it -- for bisecting a browser
# that will not build one, never as a way past a red leg.
EXPECT_RAYON_THREADS="${RIG_EXPECT_RAYON_THREADS:-2}"
# WS3c. On by default because the copying wire is a real fallback that keeps
# working -- a leg that quietly took it looks identical to every other Tier-2
# assertion, which is the same hole --expect-rayon-threads was added to close.
EXPECT_ZERO_COPY="${RIG_EXPECT_ZERO_COPY:-1}"
PY=python3
# The rig's own executable pins run before any leg: drive.py's windowed
# worst-frame selector is the p99 verdict's instrument, and nothing else in CI
# executes the rig's Python. A red pin fails the gate before a browser starts.
"$PY" "$RIG_DIR/drive.py" --selftest || { echo "drive.py --selftest FAILED"; exit 1; }

# UiConfig is #[serde(default)], so a config this partial parses; the key is
# the app's own real localStorage key. The site rides in a PANE, because that
# is the only place a site lives -- the app-wide `site` key was retired at
# WO-SITE, and a config carrying one seeds nothing now.
#
# Written in the OLDEST config shape on purpose: no `config_version` reads as
# version 1, so the seed walks the whole migration chain up to whatever this
# build speaks. The rig then needs no edit when CONFIG_VERSION moves, and the
# chain is exercised on every Tier-2 run. `enabled_overlays` is the v2 pane key
# that the `panes_take_layer_slots` rung consumes into `layer_slots`, so
# seeding a layer through it keeps that property.
#
# ---------------------------------------------------------------------------
# WHY A TEXTURE OVERLAY IS SEEDED (WS2)
#
# **Not** because the scene had none. It had two: `NwsAlerts` and
# `SpcDiscussions` ship `default_enabled() == true`, so
# `Gui::initialize_pane_enabled` puts them on every fresh pane, and both
# rasterize to a texture. What the scene had was two texture overlays whose
# rasters depend on the WEATHER -- an hour with no active mesoscale discussion
# and no alert in the pane's viewport produces no raster at all. MEASURED here
# 2026-08-22, both browsers, with this key removed: chromium still reached 2
# dispatched / 2 pictures / 16512000 B off the default layers alone. So the
# gate below would have been green on a stormy afternoon and red on a quiet
# one, which is not a gate.
#
# `RadarCoverage` is what makes it deterministic. It is the one texture layer
# that needs NO network: the site table is compiled into squallar-radar and
# `publish_radar_sites` pushes it through the ordinary arrival door at boot, so
# `has_data` is true on the first frame on a CI box with no weather at all.
# It also ships `default_enabled() == false`, so seeding it is a real choice
# rather than a restatement of the default -- pinned by
# `the_seeded_layer_is_one_a_fresh_pane_would_not_have`.
#
# THIS WAS `RadarSites` UNTIL THE SITE LAYER STOPPED RASTERIZING. The markers,
# the station names and the selected station's coverage ring are all lengths in
# points, and a picture placed by its geographic corners stretches whatever is
# baked into it -- so all three became per-frame screen-space painting and the
# layer became `PerFrameDirect`. It answers `prepare_job` with `None` now and
# would have taken this gate's `dispatched` to zero on a quiet CI box.
# `RadarCoverage` carries the half of that layer that really was ground, the
# network's 230 km coverage, with every property this seed depends on unchanged:
# same compiled-in table, same arrival door, same `default_enabled() == false`.
# `RadarSites` is still seeded beside it, because the wide leg's scene is a
# continental view of the station network and the markers are that scene.
#
# THE NEGATIVE CONTROL IS NOT "REMOVE THIS KEY". That was tried and it does not
# work, for the reason above. The control that does is every texture layer
# switched explicitly OFF -- a real configuration, and the one a user who
# cleared their layer stack is in:
#
#   enabled_overlays: {"RadarCoverage":false,"RadarSites":false,"NwsAlerts":false,
#                      "SpcDiscussions":false,
#                      "SpcOutlook":false,"SpcFireOutlook":false,"StormReports":false,
#                      "Lightning":false,"ModelData":false,"Mrms":false,"Gmgsi":false}
#
# `false` and not omission is load-bearing: an omitted `enabled` means "ask the
# handler", which is how the two default-on layers came back the first time.
#
# RUN 2026-08-22, both browsers, that exact seed: --expect-overlay-rasters
# FAILED on every leg and every quarantine retry (4/4 firefox attempts,
# chromium likewise) with the line never written at all, while worker
# round-trip, rayon pool and zero-copy replies stayed OK on every one of them.
# That is the property -- this assertion can go red, it goes red for the reason
# it names, and it disturbs nothing else.
# ---------------------------------------------------------------------------
# WHY `squallar.raster_telemetry` IS SEEDED
#
# The two running-total sentences this rig scrapes -- `overlay rasters:` and
# `texture uploads:` -- are `debug` on an ordinary install, because a
# monotonically growing total is not something a user who never asked for it
# should read. `console_log` boots at `Level::Info` on this target, so a
# `debug!` line does not reach the console ring this rig reads at all. This key
# is the app's own switch for that (`App::raster_telemetry_is_loud`), and
# seeding it is what keeps `--expect-overlay-rasters` able to see anything.
#
# Pinned from the Rust side by
# `raster_telemetry_line_tests::the_rig_seeds_the_key_that_makes_the_lines_loud`,
# which reads this file: a renamed key on either side is a build failure rather
# than a rig leg that reports the overlay path as `null`.
#
# `squallar.frame_telemetry` is the frame instrument's own switch, seeded for
# the same reason and pinned the same way
# (`frame_telemetry_line_tests::the_rig_seeds_the_key_that_makes_the_frame_lines_loud`):
# without it the gesture leg's --expect-interaction-frames reads the interact
# count as never-written and fails naming the missing seed.
SEED_LS='{"squallar.ui": "{\"pane_count\":1,\"panes\":[{\"site\":\"KTLX\",\"enabled_overlays\":{\"RadarSites\":true,\"RadarCoverage\":true}}]}", "squallar.raster_telemetry": "1", "squallar.frame_telemetry": "1"}'

# The `long` leg's scene, and every part of it is load-bearing. `Mrms` is the
# layer whose decoder held the 98 MB infallible allocation; `Gmgsi` is 60 MB a
# granule and rides the same 1 GiB ceiling; `loop_playback: playing` is what
# asks for granule after granule with nobody touching the page, which is all it
# takes to reach that ceiling. NO gesture script and no W3C actions: the freeze
# this leg exists to catch does not need input, and a leg that needed input
# would have said the defect belonged to the harness.
#
# ALL SEVENTEEN LAYERS, and that is measured rather than thorough-by-habit. A
# five-layer version of this same scene (Radar/Mrms/Gmgsi/BasemapTiles/
# RadarSites) at 90 s was run against the unfixed bundle on both browsers on
# 2026-08-31 and PASSED both, with the frame counter gaining 724 and 775 frames
# over its last 20 s -- a green leg over a defect that was live in the build it
# was driving. The seventeen-layer scene reached the abort on 4 of 4 legs. A
# gate is the scene that fails, not the scene that is tidy.
LONG_SEED_LS='{"squallar.ui": "{\"pane_count\":1,\"panes\":[{\"site\":\"KTLX\",\"loop_playback\":\"playing\",\"enabled_overlays\":{\"ModelData\":true,\"SpcOutlook\":true,\"Radar\":true,\"SpcDiscussions\":true,\"NwsAlerts\":true,\"StormReports\":true,\"Lightning\":true,\"Metar\":true,\"CityLabels\":true,\"RadarSites\":true,\"RadarCoverage\":true,\"UserLocation\":true,\"ColorScale\":true,\"SpcFireOutlook\":true,\"Mrms\":true,\"Gmgsi\":true,\"Terrain\":true,\"BasemapTiles\":true}}]}", "squallar.raster_telemetry": "1", "squallar.frame_telemetry": "1"}'

# Scripted seconds in the `long` leg, and the window its progress assert diffs
# over. 10 + 140 = 150 s, and the 140 is measured too: on the unfixed bundle
# the abort landed 121.8 s (Firefox) and 122.1 s (Chromium) after boot on this
# scene, so a 90 s leg stops before the thing it is looking for happens. The
# spread over other scenes ran from 7.3 s upward; 150 s clears the top of it.
# The `long` (and `huge`) leg's clock. A loop seeded on a live site lists
# whatever that site is doing today, and what KTLX is doing decides the leg:
# on 2026-09-07 the leg passed at 05:01Z on VCP 35 (clear air, one volume
# every 7 min 02 s) and failed on every push from 05:22Z, when KTLX switched
# to VCP 212 (precipitation, one every 3 min 50 s) at 05:38Z -- 1.84x the
# volumes in the same one-hour lookback at flat bytes per volume (48.3 vs
# 50.0 MB raw, decoded off the Level-2 files), and the page's 1 GiB
# linear-memory wall went with it. A gate whose outcome the atmosphere sets
# is not a gate, so the leg pins its clock (serve.py --pin-clock) to the end
# of that VCP 212 run: the seeded one-hour loop lists the same sixteen
# volumes, 06:24:07Z to 07:21:26Z, every time, from an archive bucket that
# still listed 2025-09-07 when this was written. Real time is one env var
# away for a live smoke run; the pinned window is the regression test.
LONG_PIN_CLOCK="${RIG_LONG_PIN_CLOCK:-2026-09-07T07:22:00Z}"
LONG_SETTLE="${RIG_LONG_SETTLE:-10}"
LONG_WINDOW="${RIG_LONG_WINDOW:-140}"
LONG_PROGRESS_WINDOW="${RIG_LONG_PROGRESS_WINDOW:-20}"

# The `wide` leg's scene, and its whole point is the WINDOW rather than the
# layers. Reported by a user as "zooming out quite a bit can freeze it up",
# with a screenshot of a 2878x1736 window at world zoom and every radar site on
# the glass. Reproduced 4 of 4 at that canvas and 0 of 2 at this rig's default
# 1280x900, on a scene with ONE overlay -- so the `long` leg above, which is
# seventeen layers at the default window, cannot see it and did not.
#
# What the size actually drives is the tile span: a 2878-point-wide viewport
# asks for tiles the 1280 one never requests, and one of them was a low-zoom
# vector tile whose `landcover` layer is a few features of hundreds of rings
# each. `mvt-reader` reserved the whole feature's command-integer count for
# EVERY ring (see vendor/mvt-reader/VENDORED.md), so that feature's decode
# peaked at `rings x commands`, and on wasm32 that is an infallible allocation
# against the 1 GiB module ceiling this workspace links at. Measured 2026-08-31,
# Firefox: the module held 332 MB of 1024 when a 172 KB tile asked for the rest.
#
# **Nothing unwinds through a wasm trap**, which is why the leg has to assert
# what it asserts. `handle_alloc_error` aborts inside the frame's
# requestAnimationFrame callback with winit's `Shared::runner` RefCell mutably
# borrowed, and that borrow is never released: every later event panics
# `RefCell already borrowed` and the frame loop is over. rAF stayed at 17.06 ms
# p50 and the canvas kept its last painted frame throughout, so a rAF check, a
# non-blank-canvas check and a screenshot ALL read healthy over a dead app.
# `--expect-frame-progress` against the app's own cumulative `frame cadence`
# count is the only reading that moved.
#
# Zoom 3 and a centre, not a wheel script: the pane's zoom and centre persist
# (`squallar.ui`, `panes[].zoom` / `panes[].center`), so the scene is seeded
# rather than driven. NO input at all, for the `long` leg's reason -- the freeze
# does not need any, and a leg that needed input would be testing the harness.
WIDE_SEED_LS='{"squallar.ui": "{\"pane_count\":1,\"panes\":[{\"site\":\"KTLX\",\"zoom\":3.0,\"center\":[39.83,-98.58],\"enabled_overlays\":{\"RadarSites\":true,\"RadarCoverage\":true}}]}", "squallar.raster_telemetry": "1", "squallar.frame_telemetry": "1"}'

# The user's window, and the leg is nothing without it: the same scene at the
# rig default survives. drive.py corrects the window until the canvas DRAWING
# BUFFER matches, and records whether it did, so the figure that reproduces is
# the one the leg is held to rather than a window size the chrome ate into.
WIDE_WINDOW="${RIG_WIDE_WINDOW:-2878x1651}"
WIDE_CANVAS="${RIG_WIDE_CANVAS:-2878x1566}"

# 10 + 50 = 60 s. Measured on the unfixed bundle at this canvas, the abort
# landed 14.4 to 15.7 s after boot over four legs, so 60 s is four times the
# slowest reproduction rather than a round number. It is deliberately much
# shorter than the `long` leg's 150: that one is waiting for a granule loop to
# accumulate, this one only has to draw a wide viewport once.
WIDE_SETTLE="${RIG_WIDE_SETTLE:-10}"
WIDE_WINDOW_S="${RIG_WIDE_WINDOW_S:-50}"
WIDE_PROGRESS_WINDOW="${RIG_WIDE_PROGRESS_WINDOW:-20}"

# ---------------------------------------------------------------------------
# THE `wideloop` LEG: the user's own configuration, and the union no leg had
# ---------------------------------------------------------------------------
#
# THE SEED IS COMPOSED, NOT COPIED. It is `LONG_SEED_LS` with the `wide` leg's
# zoom and centre spliced into the pane, so the layer set and the playing loop
# are LITERALLY the `long` leg's -- one source of truth for the seventeen
# layers, and a third hand-maintained copy of that list is exactly how a scene
# drifts from the scene its comment claims. The splice is checked in
# `--selftest`, offline: a bash whose substitution semantics moved would
# otherwise run the `long` scene under this leg's name and report it as a
# world-zoom finding.
WIDELOOP_PANE_ANCHOR='{\"site\":\"KTLX\",'
WIDELOOP_PANE_SPLICE='{\"site\":\"KTLX\",\"zoom\":3.0,\"center\":[39.83,-98.58],'
WIDELOOP_SEED_LS="${LONG_SEED_LS/"$WIDELOOP_PANE_ANCHOR"/"$WIDELOOP_PANE_SPLICE"}"

# The `wide` leg's geometry, BY REFERENCE and not by value: this leg's whole
# claim is that it is that canvas carrying the `long` leg's scene, and two
# numbers that have to be kept equal by hand are two numbers that will not be.
# `--expect-canvas` rides along, which `wide` does not carry: a leg whose point
# is the size must fail rather than quietly report a smaller one.
WIDELOOP_WINDOW="${RIG_WIDELOOP_WINDOW:-$WIDE_WINDOW}"
WIDELOOP_CANVAS="${RIG_WIDELOOP_CANVAS:-$WIDE_CANVAS}"

# 6 + 54 = 60 s. The deaths this scene is built from land at t+10.3 s (`long`,
# 1280 wide) and t+14-16 s (`huge`, this canvas), so 60 s is roughly 4x the
# slowest of them -- `wide`'s length, for `wide`'s reason, and not `long`'s 150
# because this leg is not waiting for granules to accumulate before the wall is
# reached. The settle is SIX and not ten: the sampler runs through the settle,
# but the rAF warm sample between settle and data window does not sample, and a
# ten-second settle would put that gap on top of the fatal window.
WIDELOOP_SETTLE="${RIG_WIDELOOP_SETTLE:-6}"
WIDELOOP_WINDOW_S="${RIG_WIDELOOP_WINDOW_S:-54}"
WIDELOOP_PROGRESS_WINDOW="${RIG_WIDELOOP_PROGRESS_WINDOW:-20}"
# Two seconds because that is the app's own telemetry tick: a faster cadence
# re-reads the same console lines and buys nothing but rows.
WIDELOOP_SAMPLE_INTERVAL="${RIG_WIDELOOP_SAMPLE_INTERVAL:-2}"
# MiB of clearance the page must keep from the ceiling IT REPORTS. 64 and not
# 0: an allocation is refused at the wall, so a leg that only failed AT 1024
# would be reporting the trap the trap counter already reports. This one is
# supposed to see the approach.
WIDELOOP_LINEAR_HEADROOM="${RIG_WIDELOOP_LINEAR_HEADROOM:-64}"

# rAF deltas in the warm sample, and this leg lowers the rig default of 120 for
# a reason that is not about rAF at all. The warm sample is the one stretch of
# the leg the census sampler cannot see into, and its LENGTH IS SET BY THE PAGE
# -- 120 deltas is 2 s at 60 Hz and was 29 s on the chromium arm of this scene,
# which draws at p50 172 ms under a 2878x1566 canvas at world zoom. Measured
# 2026-09-08: that hole contained the first allocation refusal on both
# browsers. 30 deltas keeps the rAF figure (a p50 and a p95 over 30 samples is
# still a reading) and shortens the blind stretch by 4x, and the two samples
# drive.py now takes on either side of it bound whatever is left.
WIDELOOP_FRAMES="${RIG_WIDELOOP_FRAMES:-30}"

# The identity lines written INTO the sample TSV before the browser starts.
# Every fact here is one only the runner holds -- the commit, the seed, the
# bundle -- and every one of them is hashed or read rather than named: a
# directory called `A` is not a commit and an mtime is not a leg start.
sample_header_lines() {
  # Canvas and window are PARAMETERS, not this function's guess. They were
  # `$WIDELOOP_CANVAS`/`$WIDELOOP_WINDOW` while `wideloop` was the only caller,
  # and `huge` adopting the sampler on 2026-09-08 would have stamped that leg's
  # geometry into this one's identity header -- a TSV positively asserting a
  # canvas the run never asked for, which is the exact failure the block below
  # exists to prevent.
  local tsv="$1" seed="$2" tag="$3"
  local canvas="${4:-$WIDELOOP_CANVAS}" window="${5:-$WIDELOOP_WINDOW}"
  local wasm="$WEB_DIR/pkg/squallar_web_bg.wasm"
  {
    echo "# tier2 leg=$tag rig=$RIG_DIR"
    echo "# commit=$(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || echo unknown)"
    echo "# commit_subject=$(git -C "$REPO_ROOT" log -1 --format=%s 2>/dev/null | cut -c1-120)"
    echo "# tree_dirty=$(git -C "$REPO_ROOT" status --porcelain 2>/dev/null | wc -l) file(s) modified"
    echo "# started_host_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ) started_host_epoch=$(date +%s)"
    echo "# seed_sha256=$(printf '%s' "$seed" | sha256sum | cut -d" " -f1)"
    echo "# wasm=$wasm"
    echo "# wasm_sha256=$(sha256sum "$wasm" 2>/dev/null | cut -d" " -f1)"
    echo "# wasm_bytes=$(stat -c%s "$wasm" 2>/dev/null)"
    echo "# pin_clock=$LONG_PIN_CLOCK canvas_asked=$canvas window=$window"
    echo "# host_uname=$(uname -sr) host_loadavg_at_start=$(cut -d" " -f1-3 /proc/loadavg 2>/dev/null)"
    echo "# clocks: t_host_iso is THIS BOX'\''s wall clock; t_page_ms is the PAGE'\''s"
    echo "#   Date.now(), which serve.py --pin-clock moves to $LONG_PIN_CLOCK."
    echo "#   They are two clocks and differencing across them is meaningless."
  } >> "$tsv"
}

# ---------------------------------------------------------------------------
# THE `huge` LEG: the size the app is actually used at
# ---------------------------------------------------------------------------
#
# Every web leg in this rig had run at the default window until 2026-08-31,
# which lands a canvas of roughly 1280x815. A user-reported freeze was then
# root-caused to a `RefCell already borrowed` panic in winit's web event-loop
# runner that depends on CANVAS SIZE and nothing else:
#
#   2878x1651 (the reporting user's window)  dies 4 of 4, at +14-16 s
#   1280x815  (this rig's default)           clean 2 of 2, through 46-53 s
#
# So the rig was not unlucky, it was STRUCTURALLY BLIND: no leg had ever been
# run at a size that could reproduce it. And afterwards rAF stays at 17.06 ms
# and the canvas holds its last painted frame, so -- exactly as with the MRMS
# abort the `long` leg exists for -- every cheap liveness signal reports health
# while the app is dead. The app's own frame counter is again the only witness,
# which is why this leg carries --expect-frame-progress rather than a
# screenshot.
#
# CORRECTION 2026-09-08: THE SIZE IS THE AMPLIFIER, NOT THE CAUSE, AND THE
# PANIC TEXT NAMES NEITHER.
#
# The two paragraphs above are observations and they hold: this scene dies at
# 2878x1651 and survives at the browser default. What does NOT follow -- and
# what this block used to invite a reader to conclude -- is that the canvas is
# an independent defect from the `long` leg's MRMS abort. It is the same wall.
# Read off the artefacts under ~/.cache/rustdar-fb-rig-out/ rather than off the
# symptom, and every recorded `huge` death is an allocation refused against a
# 1024 MiB linear memory:
#
#   wo25b-huge-2026-09-04, the refusals themselves (bytes ASKED FOR, and where
#   the heap stood when the allocator said no):
#     chromium.huge           78,389,248 B at  993 of 1024 MiB   (1 panic, 18 traps)
#     chromium.huge attempt1  73,496,033 B at  978; 49,042,152 B at 999
#     firefox.huge             7,281,678 B at 1023;  3,967,488 B at 1020
#     firefox.huge attempt1   49,042,152 B at 1018;     69,098 B at 1024 of 1024
#
#   wo30-huge-*, which carries NO refusal line at all -- a DIFFERENT reading,
#   never added to the one above: the `budget state:` line's PAGE linear level
#   at the last tick before the end.
#     chromium.huge attempt1  linear 1000/446 MiB   (page/worker)
#     firefox.huge             linear  941/580 MiB
#
# The two families are what make the attribution: the wo25b lines PREDATE the
# instance word (`... MiB linear in page` was appended later), and on a desktop
# BOTH instances are given the same 1024 MiB ceiling by squallar-web/heap.js,
# so those lines establish the WALL and cannot name the heap. The wo30 budget
# readings can, and do: it is the PAGE that arrives within 24-83 MiB of its
# ceiling.
#
# WHAT THE CANVAS DOES, STATED NO FURTHER THAN THE READINGS GO. It would be
# convenient to write that it merely amplifies the term `long` dies of, and
# that is NOT what the census says. Read the two scenes' composition at the
# wall and they are different failures reaching the same ceiling:
#
#   long  at 1280   dies with 596 MiB of LOOP SCANS -- the decoded volumes,
#                   which is the term that leg was built to catch.
#   huge  at 2878   dies with loop scans never above 99.4 MiB and overlay
#                   grids at 210, against a RESIDENT TOTAL of 699 while the
#                   page is pinned at 1024. 325 MiB that no census family
#                   names.
#
# Measured independently by the `wideloop` leg below, whose sampler reads the
# census every two seconds at a third geometry: at the last sample of each of
# its eight attempts the page stands at 965-1024 MiB while the census names
# 510-781, leaving 205-514 MiB (median ~294) unaccounted. The `huge` figure
# sits inside that range, from a different lane on a different run.
#
# So the canvas does not only make the same term bigger -- it RECRUITS A TERM
# THE CENSUS DOES NOT NAME, and that term is what a fix has to find.
#
# WHERE THAT TERM CANNOT BE, which is worth more than a guess about where it
# is. The residual is `page linear MINUS the page census families`, and both
# halves are the PAGE's by construction: `app_render.rs` writes the census with
# `linear.map(|heap| heap.page_bytes)` and the instance word `"page"`. The
# rasterization worker's heap is a DIFFERENT `WebAssembly.Memory` in a
# different instance -- `worker_heap.rs`'s "Two instances, and the third name
# is a thread", `heap_max.rs`'s "Two instances, two cells", `bridge.rs`'s "two
# instances under two ceilings", and `alloc_failure.rs` calling the worker's
# "a second 1 GiB nothing prices". Not one byte of it appears in the page's
# `byteLength`, so it CANNOT be the unnamed part of a total it was never part
# of. That is why the line prints `linear <page>/<worker>` as a pair and never
# a sum, and it is why an earlier draft of this block naming the worker as the
# strong candidate was wrong.
#
# WHAT IS LEFT IS PAGE-SIDE AND THIS FILE DOES NOT PICK AMONG IT. The tree
# already names several page-side holdings outside every census family --
# `app_render.rs`: "the module's own statics, egui's tessellation buffers, the
# decoded volumes behind the loop and every transfer in flight are on the same
# heap and in no term here". A fifth candidate fits the canvas dependence
# specifically, because it is the one that scales with the canvas SQUARED:
# egui's texture population, where each overlay picture is a canvas-scaled
# `ColorImage`, which `heap_census.rs` mentions only inside other families'
# prose (lines 283 and 604) and never as a term of its own.
#
# NOTHING IN THIS TREE IDENTIFIES WHICH, and this block stops there on purpose.
# A candidate promoted to a cause here is adopted by the next reader, and this
# correction is already the second one on this block: canvas-as-cause, then
# canvas-as-amplifier, then the worker. Each was a reasonable reading of the
# figures available and each was wrong, so the figures are what this block
# carries and the mechanism is left open for a measurement to close.
#
# WHY THE TWO EVER LOOKED LIKE SEPARATE DEFECTS, and this is the sentence this
# block most needed: this target is `panic-strategy = "abort"`, so NOTHING
# unwinds, and ANY abort inside the requestAnimationFrame callback leaves
# winit's `Shared::runner` RefCell mutably borrowed for the life of the page.
# `RefCell already borrowed` is therefore the terminal symptom of EVERY death
# in that callback and cannot discriminate between them. It is a LOCATION, not
# a cause. A reader who takes it for one sizes the fix against the canvas
# rather than against the page.
#
# AND WHAT IS STILL OPEN, stated so nobody reads the correction as more than
# it is: every artefact quoted above predates the heap-census families, so not
# one of them carries a `heap census (...)` line and there is no residue figure
# at this canvas -- verified, not assumed (no census line in any of the eight
# JSONs). Whether removing the loop's decoded volumes is ENOUGH at 2878x1651 is
# therefore unanswered. A two-arm run answers it; no projection does. The
# `wideloop` leg below samples the census every two seconds precisely so that
# the next reading of this scene does not have this hole in it.
#
# ANSWERED 2026-09-08, and the answer is NO. The memory fix's two `huge` arms
# were run: parent chromium peaked at 1024 with 2,384 B refused AT the
# ceiling and parent firefox at 1015 with 50,849,246 B refused; the arm with
# the loop's decoded volumes removed still died, at 985 chromium and 1013
# firefox. Removing them is not sufficient at this canvas -- which is what the
# unnamed 325 MiB above predicted, and why the term matters more than the loop.
#
# A CAVEAT ON EVERY `linear <page>/<WORKER>` FIGURE TAKEN BEFORE 2026-09-08,
# this leg's own included. The page held the worker's heap figures in statics
# that nothing cleared on loss, so a worker that spoke once and died read
# identically to one still speaking (fixed at `a26a54ff0`, which introduces
# `Reading::{Unread, Current, Stale}`). Every worker column in the artefacts
# under ~/.cache/rustdar-fb-rig-out/ is therefore a real reading by some
# worker at some time, with no way to say whether it was alive. The bias is
# NOT neutral: a stale figure on a dying page is most likely a last gasp, so
# it over-reads exactly where these legs read it -- near the wall. The MAXIMUM
# is the figure most exposed to this and the medians least.
#
# It does not reach the canvas finding, whose two scenes are disjoint in page
# linear and would need a canvas-CORRELATED bias to manufacture. And it does
# not reach an `alloc failed: ... linear in raster worker` line, which is a
# different reading altogether: the allocation-error hook runs INSIDE the
# instance that refused, so that line is the worker's own voice at the instant
# of refusal rather than the page's cache of it.
#
# CORRECTION 2026-09-08, SECOND ROUND: THE RESIDUAL IS NOT UNNAMED, AND MOST
# OF IT IS NOT HELD AT ALL.
#
# The block above computes its "325 MiB that no census family names" as page
# `byteLength` MINUS the census `resident total`. That difference is TWO terms
# and they want different fixes:
#
#   byteLength - live_bytes   freed-but-reserved headroom. dlmalloc reuses and
#                             a linear memory never shrinks, so this is debris,
#                             not holdings (`squallar-alloc/src/lib.rs:23`).
#   live_bytes - census       live bytes no census family names.
#
# The `budget state:` line has carried `live <page>/<worker> MiB` beside
# `linear <page>/<worker> MiB` since the arms under ~/.cache/rd-loopz-arms/ and
# ~/.cache/rustdar-fb-rig-out/huge-651d289d1-*-ctl were taken, and those arms DO
# carry `heap census` lines -- so the hole the block above records as open
# ("not one of them carries a `heap census (...)` line") is closed by artefacts
# that postdate it. Read the split at the last sample before the wall:
#
#   huge-651d289d1-chr-ctl attempt1   byteLen 1016  live 1006   freed  10.0 (1.0%)
#   parta-011849 firefox              byteLen 1013  live  995   freed  18.2 (1.8%)
#   parta-011849 chromium attempt1    byteLen 1015  live  962   freed  53.1 (5.2%)
#   huge-651d289d1-ff-ctl attempt1    byteLen 1021  live  934   freed  87.1 (8.5%)
#   parta-011849 chromium             byteLen  985  live  823   freed 162.0 (16.4%)
#
# **Live is 83-99% of byteLength at the wall on every arm that captured the
# ascent.** The page dies with the heap genuinely full. The 34-52% freed
# fractions appear ONLY in traces where byteLength was already pinned at its
# final value at t+0 and live is FALLING -- post-mortem samples, the debris of
# the trap. A retention figure averaged over those is measuring the corpse.
#
# AND THE BIG TERM IS NAMED. `upload pending` is 40-59% of the census at the
# wall -- 356.5 MiB (ff-ctl), 523.3 (parta ff), 414.8 (parta chr), 417.9
# (chr-ctl) -- and it reaches 302-579 MiB within 2.3-6 s of boot, while loop
# scans, still scans and tile cache are all still ZERO. It is not a mystery
# term; it is the texture-upload queue.
#
# WHY THE CANVAS DRIVES IT, as a number rather than a description.
# `device_has_ring` is `device.features().contains(STAGING_RING_FEATURE)` and
# is FALSE on web -- `band_cap`'s own note says "On a ringless device - all of
# web". Confirmed empirically: `staged_bytes` is 0 and `blocking_bytes` equals
# `bytes()` in every artefact here, so the ring was never used. So on web
# `bands_per_frame` is 1 and `band_cap` is BLOCKING_BAND_BYTES = 4 MiB:
# **the drain moves one 4 MiB band per frame, and its rate in bytes per second
# is therefore 4 MiB x the frame rate.** Measured off the arms:
#
#   long  at 1280   ~18-24 fps  ->  ~80 MB/s of drain, pictures ~8.5 MB
#   huge  at 2878   ~4.5 fps    ->  ~18 MB/s of drain, pictures 18.5-73.5 MB
#
# and `bands/frame` on the huge firefox arm reads 1.11, 1.00, 0.90, 1.00 --
# **the drain is saturated at its structural cap**. One 73,087,216 B picture is
# 18 bands and so 18 frames, which at 4.5 fps is four seconds for one picture;
# this leg files 25 of them. The canvas raises arrival and lowers the drain at
# the same time, which is why size is the amplifier and why `long` survives the
# same scene: at 1280 the drain keeps `upload pending` at 0-64 MiB and the
# loop's decoded volumes are what fill the heap instead.
#
# WHAT THE DRAIN ACTUALLY COSTS, MEASURED, because the obvious inference from
# the paragraph above is wrong and it was made here first.
#
# THIS IS THE THIRD INSTANCE OF ONE MISTAKE AND THE FILE NAMES IT ITSELF.
# `texture_upload.rs` records the first two -- "the whole-crossing route never
# touches the ring, and pricing it at the ring's bandwidth is the mistake this
# file already made once" -- and this is the same move a third time, in the
# other direction: pricing the BANDED drain at the whole-crossing route's
# bandwidth. A route's cost is measured on that route. It was derived here,
# amplified into a campaign-level claim about the web target before anyone
# asked how it was derived, and forwarded to another lane as settled; the
# figures below are what it should have been checked against first.
#
# `whole_budget`'s note prices 56 MiB of blocking `write_texture` at ~1 GB/s for
# ~56 ms, which divides to ~4 ms for a 4 MiB band -- the whole of
# TARGET_FRAME_SERVICE. **That pricing is for the WHOLE-crossing route and does
# not transfer to the banded drain.** `upload_apply_us` is documented as "the
# frame-thread side of texture_upload, memcpys into staging slots and any
# blocking `write_texture` included", and differenced against `bands` on four
# arms across both browsers it reads:
#
#   parent-011239 firefox        236 us/band   9.10 GB/s   215 us/pass
#   parta-011849 chromium        333 us/band  12.55 GB/s   297 us/pass
#   huge-651d289d1 chr-ctl       187 us/band  14.28 GB/s   179 us/pass
#   long-651d289d1 ff-ctl        261 us/band   9.23 GB/s   189 us/pass
#
# **180-330 us, which is 4.5-8% of the 4 ms target, not 100%.** So "the drain
# has no headroom" is REFUTED on this arm and a faster drain is back on the
# table: eight bands a frame would still cost ~1.5-2.7 ms and would raise the
# drain ceiling eightfold. Whether the cost merely RELOCATES to the browser's
# GPU process -- glTexSubImage2D can hand off asynchronously, so a low
# frame-thread figure is not a low system figure -- this instrument cannot say.
#
# AND THE FRAME RATE THIS LEG DRAINS AT IS THE SOFTWARE RASTERISER'S, NOT THE
# PRODUCT'S. On the firefox arm prep is 933 us a pass (tessellate 577, upload
# 214, buffers 141) against an rAF p50 of **261 ms** -- prep is 0.36% of the
# frame. The other 99.6% is llvmpipe filling 2878x1651 = 4.75 Mpx. So the chain
# is: software rasteriser -> ~4.5 fps -> drain ceiling of 4 MiB x 4.5 -> the
# queue grows. A hardware arm at 60 fps has a thirteenfold higher ceiling, and
# **nothing here establishes that `upload pending` builds up on a real driver
# at all.** This gate is the software arm permanently and by design; that makes
# it structurally unable to tell a product defect in the upload queue from
# llvmpipe being slow at this canvas. `run_gpu_arm.sh` is the arm that can, and
# until it has run this scene the queue finding is scoped to software.
#
# So the fix direction is NOT settled here. Three shapes are live -- a faster
# drain, a bounded queue, refusing to file what cannot be drained -- and this
# block picks none of them, for the reason the corrections above it record.
#
# 45 s of window and not 140: the death is at +14-16 s on a scene that
# reproduces, so this clears it by ~3x while keeping the leg affordable. If a
# larger margin is ever wanted, RIG_HUGE_WINDOW is the knob -- but a leg that
# has to run for two minutes to see a fourteen-second defect is a leg nobody
# runs.
#
# --window is set ABOVE the target on purpose and is not decoration. fit_canvas
# corrects the WINDOW until the BUFFER matches, and the Firefox arm's Xvfb
# screen is sized from the initial --window (`screen=(window+400, window+150)`
# in drive.py's launch): start this leg at the default 1280x900 and the virtual
# screen is 1680x1050, the window can never grow past it, and the canvas
# silently tops out around 1280 -- a leg claiming to test 2878 that tested
# 1280. --expect-canvas is what turns that from a silent mislabel into a red
# leg; see its help text in drive.py.
HUGE_CANVAS="${RIG_HUGE_CANVAS:-2878x1651}"
HUGE_WINDOW="${RIG_HUGE_WINDOW_SIZE:-3100x1900}"
HUGE_SETTLE="${RIG_HUGE_SETTLE:-8}"
HUGE_WINDOW_S="${RIG_HUGE_WINDOW:-45}"
HUGE_PROGRESS_WINDOW="${RIG_HUGE_PROGRESS_WINDOW:-15}"

# THE INSTRUMENT THIS LEG WAS MISSING, adopted wholesale from `wideloop` on
# 2026-09-08 because the reason that leg carries it is this leg's reason.
#
# `--frames` is the load-bearing one and it is not about rAF. The warm rAF
# sample is the one stretch of a leg the census sampler cannot see into, and
# ITS LENGTH IS SET BY THE PAGE: 120 deltas is 2 s at 60 Hz and was **29.8 s**
# on this leg's firefox arm (`raf_warm.wall_ms` 29832.56 at p50 261 ms, measured
# off huge-651d289d1-firefox-ctl). The wall on this scene is reached at t+10-16 s
# -- INSIDE that hole. Every recorded `huge` death therefore had its first
# allocation refusal in a stretch nothing sampled. 30 deltas keeps a p50/p95
# worth reading and shortens the blind stretch by 4x.
HUGE_FRAMES="${RIG_HUGE_FRAMES:-30}"
# The app's own telemetry tick; a faster cadence re-reads the same console
# lines and buys nothing but rows.
HUGE_SAMPLE_INTERVAL="${RIG_HUGE_SAMPLE_INTERVAL:-2}"
# MiB of clearance from the ceiling THE PAGE REPORTS, on `wideloop`'s terms:
# an allocation is refused at the wall, so a leg that only failed AT 1024 would
# be reporting the trap the trap counter already reports.
HUGE_LINEAR_HEADROOM="${RIG_HUGE_LINEAR_HEADROOM:-64}"

# THESE TWO ASSERTIONS ARE NEW GATES AND A RED FROM THEM IS NOT COMPARABLE TO
# THE HISTORICAL `huge` RECORD. Every verdict before 2026-09-08 turned on
# `--expect-frame-progress` alone. Read `frame_progress` on its own when
# comparing against those arms; the leg's overall pass/fail no longer answers
# the same question they did.

# ---------------------------------------------------------------------------
# THE `tilecache` LEG: does the tile cache hold the tiles on the glass?
# ---------------------------------------------------------------------------
#
# A Tier-2 leg once read 3,070 mesh uploads against 2,848 evictions to hold
# 10.25 MB resident, and nothing in the rig could say why: `ground tiles:`
# counts the GPU store's uploads and evictions on an identity minted per mesh,
# so it cannot tell a tile's first sight from the same tile fetched again after
# the LRU dropped it. The `tile cache (base):` line classifies those events at
# the cache (see drive.py's `tile_cache_re`), and this leg is the scene that
# makes the classification a verdict: a static viewport, every tile arrived,
# and then thirty seconds in which a cache that holds its working set records
# nothing while a cache below it evicts, re-asks, re-decodes and re-uploads
# tiles that never left the glass.
#
# The scene is ONE pane at zoom 14 over midtown Manhattan (the densest MVT
# tiles the archive serves), `BasemapTiles` on and nothing else seeded -- the
# two default-on texture overlays still come up, as on every leg. Zoom and
# centre are seeded rather than driven, for the `wide` leg's reason. The site
# is KOKX so the pane's own site is the one under it.
#
# THE WINDOW IS THE VARIABLE. At 2878x1651 (the user's own canvas, the `huge`
# leg's target) the pane draws 104 tiles per source at a whole zoom and 187
# between zooms, 110 / 193 with the ancestor net, against a wasm32 cache of
# 100 entries -- below the working set at every zoom. At 1280x900 it fits.
# RIG_TILECACHE_WINDOW chooses: the default is the repro, `1280x900` the
# control, and any other WxH is taken as a canvas target. A canvas target is
# ASSERTED (--expect-canvas) and the window starts above it, for the reason
# HUGE_WINDOW gives: the Firefox arm's Xvfb screen is sized from the initial
# window and cannot grow past it.
#
# 15 + 45 = 60 s, and the assertion reads the last 30: the archive is fetched
# over the real network and a 2878-wide viewport asks for ~190 tiles through
# six download slots, so the first half of the leg is arrival and the second
# half is the question. --expect-frame-progress rides along because a frame
# loop that DIED is silent too, and only the app's own frame counter tells a
# settled cache from a dead page.
#
# HELD OUT OF THE DEFAULT ROSTER ON PURPOSE. The repro is expected to FAIL on
# the shipped cache -- that is the finding it exists to record -- and it goes
# into the default the day the cache holds its working set, as the proof.
# The control is expected to pass today.
#
# WHERE IT WRITES: the runner's default RIG_OUT_DIR is `<rig>/out`, inside
# the checkout. Run this leg with an out-of-tree directory, e.g.
#   RIG_LEGS=tilecache RIG_OUT_DIR=~/.cache/rustdar-fb-rig-out/tilecache-$(date +%F) \
#     RIG_BROWSERS=firefox .github/browser-rig/run_tier2.sh --skip-build
# and once more with RIG_BROWSERS=chromium: web is two targets and the two
# figures are never merged.
TILECACHE_SEED_LS='{"squallar.ui": "{\"pane_count\":1,\"panes\":[{\"site\":\"KOKX\",\"zoom\":14.0,\"center\":[40.758,-73.9855],\"enabled_overlays\":{\"BasemapTiles\":true}}]}", "squallar.raster_telemetry": "1", "squallar.frame_telemetry": "1"}'
TILECACHE_WINDOW="${RIG_TILECACHE_WINDOW:-2878x1651}"
TILECACHE_SETTLE="${RIG_TILECACHE_SETTLE:-15}"
TILECACHE_WINDOW_S="${RIG_TILECACHE_WINDOW_S:-45}"
TILECACHE_SETTLES="${RIG_TILECACHE_SETTLES:-30}"
TILECACHE_PROGRESS_WINDOW="${RIG_TILECACHE_PROGRESS_WINDOW:-15}"

# The window a canvas target is started at: the `huge` leg's margins (3100x1900
# for 2878x1651), so the default repro is exactly that leg's geometry.
tilecache_window_for_canvas() {
  local canvas="$1" w h
  w="${canvas%x*}"
  h="${canvas#*x}"
  echo "$((w + 222))x$((h + 249))"
}

# Set to 0 to report the overlay raster totals without gating on them -- for a
# measurement round, never as a way past a red leg.
EXPECT_OVERLAY_RASTERS="${RIG_EXPECT_OVERLAY_RASTERS:-1}"
# Set to 0 to report the basemap tile totals without gating on them -- same
# terms. The `basemap tiles:` line rides the same `squallar.raster_telemetry`
# seed above, so a rename there breaks this gate too and is caught from the
# Rust side by the same seam test.
EXPECT_BASEMAP_TILES="${RIG_EXPECT_BASEMAP_TILES:-1}"

SKIP_BUILD=0
SELFTEST=0
for arg in "$@"; do
  case "$arg" in
    --skip-build) SKIP_BUILD=1 ;;
    --selftest) SELFTEST=1 ;;
    *) echo "unknown argument: $arg" >&2; exit 1 ;;
  esac
done

# Preserve a failed attempt's artefacts before the quarantine retry overwrites
# them in place. Without this the retry's own JSON lands on the failure's path
# and *positively asserts* `pass: true, rig_errors: 0` -- the failure does not
# go absent, which someone would notice, it is replaced by a claim of health.
# Every quarantined failure was therefore unfalsifiable after the fact.
#
# Suffixes are enumerated and NOT globbed: the `live` leg's tag is the bare
# browser name, so `"$OUT_DIR/$tag".*` would sweep up `firefox.long.*` and
# every other leg of that browser.
#
# `.pgid` files are deliberately NOT preserved. They feed the `*.driver.pgid`
# cleanup sweep, and a stale one only widens the window in which a recycled
# process group id gets killed on behalf of a process that already exited.
preserve_attempt() {
  local tag="$1" n=0 sfx src
  for sfx in json driver.log driver.stderr xvfb.log canvas.png page.png \
             mem.tsv samples.tsv; do
    src="$OUT_DIR/$tag.$sfx"
    [ -e "$src" ] || continue
    mv "$src" "$OUT_DIR/$tag.attempt1.$sfx" && n=$((n + 1))
  done
  if [ "$n" -gt 0 ]; then
    echo "$tag: preserved $n attempt-1 artefact(s) as $tag.attempt1.*" >&2
  fi
}

# Offline selftest for the one branch a green run never executes. The retry
# path only runs after a failure, so no passing Tier-2 exercises it and its
# regression would be silent -- which is how the overwrite survived this long.
if [ "$SELFTEST" = 1 ]; then
  st_fails=0
  st_dir="$(mktemp -d)"
  OUT_DIR="$st_dir"
  printf ATTEMPT1 > "$st_dir/firefox.long.json"
  printf A1LOG    > "$st_dir/firefox.long.driver.log"
  printf 12345    > "$st_dir/firefox.long.driver.pgid"
  printf LIVE     > "$st_dir/firefox.json"

  st_chk() {
    if [ "$2" = "$3" ]; then
      echo "  ok   $1"
    else
      echo "  FAIL $1: want '$2', got '$3'"; st_fails=$((st_fails + 1))
    fi
  }

  preserve_attempt firefox.long
  printf ATTEMPT2 > "$st_dir/firefox.long.json"   # the retry writes its own

  st_chk "attempt 1 json survives the retry" \
      ATTEMPT1 "$(cat "$st_dir/firefox.long.attempt1.json" 2>/dev/null)"
  st_chk "attempt 1 driver log survives" \
      A1LOG "$(cat "$st_dir/firefox.long.attempt1.driver.log" 2>/dev/null)"
  st_chk "the retry's result is on the unsuffixed path" \
      ATTEMPT2 "$(cat "$st_dir/firefox.long.json" 2>/dev/null)"
  st_chk "pgid is NOT renamed (it feeds the cleanup sweep)" \
      12345 "$(cat "$st_dir/firefox.long.driver.pgid" 2>/dev/null)"
  st_chk "and no suffixed pgid was created" \
      "" "$(cat "$st_dir/firefox.long.attempt1.driver.pgid" 2>/dev/null)"

  # The `live` leg's tag is the bare browser name, which is a PREFIX of every
  # other leg of that browser. A glob would sweep them; enumerated suffixes
  # must not. This check fails if anyone reaches for "$OUT_DIR/$tag".* again.
  preserve_attempt firefox
  st_chk "a bare-tag preserve leaves sibling legs alone" \
      ATTEMPT2 "$(cat "$st_dir/firefox.long.json" 2>/dev/null)"
  st_chk "the live leg itself is preserved" \
      LIVE "$(cat "$st_dir/firefox.attempt1.json" 2>/dev/null)"

  rm -rf "$st_dir"

  # THE COMPOSED SCENE IS THE SCENE THIS FILE CLAIMS. `WIDELOOP_SEED_LS` is
  # `LONG_SEED_LS` with a zoom and a centre spliced in by bash parameter
  # substitution -- one source of truth for the seventeen layers, and a silent
  # no-op if that substitution ever stops matching. A leg running the `long`
  # scene under the `wideloop` name would report the default zoom's numbers as
  # a world-zoom finding, so the splice is checked here, offline, before any
  # browser starts. Checked through the JSON rather than by grepping the
  # string: a seed that no longer parses enables nothing and every downstream
  # assertion reads as an app defect.
  wl_check="$("$PY" - "$WIDELOOP_SEED_LS" "$LONG_SEED_LS" <<'EOF'
import json, sys
wide, long_ = sys.argv[1], sys.argv[2]
bad = []
try:
    pane = json.loads(json.loads(wide)["squallar.ui"])["panes"][0]
    lpane = json.loads(json.loads(long_)["squallar.ui"])["panes"][0]
except Exception as e:
    print("FAIL the composed seed is not the JSON this script claims: %s" % e)
    raise SystemExit(0)
if pane.get("zoom") != 3.0:
    bad.append("zoom is %r, not the world zoom 3.0" % (pane.get("zoom"),))
if pane.get("center") != [39.83, -98.58]:
    bad.append("centre is %r, not the CONUS centre" % (pane.get("center"),))
if pane.get("loop_playback") != "playing":
    bad.append("the loop is %r, not playing" % (pane.get("loop_playback"),))
if pane.get("enabled_overlays") != lpane.get("enabled_overlays"):
    bad.append("the layer set is NOT the long leg's")
for layer in ("Mrms", "Gmgsi"):
    if not (pane.get("enabled_overlays") or {}).get(layer):
        bad.append("%s is not enabled; it is half the scene" % layer)
if pane.get("site") != lpane.get("site"):
    bad.append("the site moved: %r vs %r" % (pane.get("site"), lpane.get("site")))
if wide == long_:
    bad.append("the splice was a NO-OP: this leg would run the long scene")
for b in bad:
    print("FAIL %s" % b)
if not bad:
    print("ok the wideloop seed is the long scene at world zoom")
EOF
)"
  while IFS= read -r line; do
    case "$line" in
      ok\ *) echo "  ${line}" ;;
      *) echo "  $line"; st_fails=$((st_fails + 1)) ;;
    esac
  done <<< "$wl_check"

  # The sampler's identity header stamps the CALLER'S geometry. Two arms and
  # not one: a check that only asserted `huge` would pass for a function that
  # had `huge`'s canvas hardcoded, which is the shape of the bug this
  # parameterisation fixed (it wrote `wideloop`'s for every caller).
  # Its own directory: `$st_dir` is removed by the preserve checks above.
  hdr_dir="$(mktemp -d)"
  sample_header_lines "$hdr_dir/h.tsv" "seed-h" huge "$HUGE_CANVAS" "$HUGE_WINDOW"
  sample_header_lines "$hdr_dir/w.tsv" "seed-w" wideloop "$WIDELOOP_CANVAS" "$WIDELOOP_WINDOW"
  st_chk "the huge sample header stamps huge's canvas" \
    "canvas_asked=$HUGE_CANVAS" \
    "$(grep -o 'canvas_asked=[0-9x]*' "$hdr_dir/h.tsv" | head -1)"
  st_chk "the wideloop sample header stamps wideloop's canvas" \
    "canvas_asked=$WIDELOOP_CANVAS" \
    "$(grep -o 'canvas_asked=[0-9x]*' "$hdr_dir/w.tsv" | head -1)"
  st_chk "and the two legs' headers are not the same geometry" \
    "differ" \
    "$([ "$(grep -o 'canvas_asked=[0-9x]*' "$hdr_dir/h.tsv" | head -1)" \
        != "$(grep -o 'canvas_asked=[0-9x]*' "$hdr_dir/w.tsv" | head -1)" ] \
        && echo differ || echo same)"

  rm -rf "$hdr_dir"

  if [ "$st_fails" -eq 0 ]; then
    echo "run_tier2 SELFTEST PASS (7 preserve checks + the wideloop seed + 3 header-identity)"; exit 0
  fi
  echo "run_tier2 SELFTEST FAIL ($st_fails)" >&2; exit 1
fi

if [ -z "${RIG_GECKODRIVER:-}" ]; then
  RIG_GECKODRIVER="$(bash "$RIG_DIR/ensure-geckodriver.sh")" || {
    echo "FATAL: ensure-geckodriver.sh failed" >&2
    exit 1
  }
fi
GECKODRIVER="$RIG_GECKODRIVER"

# `--coep` is a DEFAULT as of WS3b, not an opt-in. The shipped app needs
# cross-origin isolation to have a `SharedArrayBuffer` to build rayon's pool
# on, and production supplies it (CloudFront Response Headers Policy on
# squallar.app, COOP `same-origin` + COEP `require-corp`). A rig that
# served the bundle WITHOUT the headers would be gating a configuration this
# app is never deployed in, and -- worse -- one it degrades into silently:
# every other Tier-2 assertion passes against the one-thread fallback. Paired
# with `--expect-rayon-threads` below, which is what makes the header's effect
# observable rather than assumed.
SERVE_EXTRA=(--coep)
if [ -n "${RIG_SERVE_EXTRA:-}" ]; then
  # shellcheck disable=SC2206
  SERVE_EXTRA+=($RIG_SERVE_EXTRA)
fi

mkdir -p "$OUT_DIR"

# --------------------------------------------------- binding the verdict ----
#
# WHY THIS EXISTS. Until 2026-08-31 the summary block below did exactly one
# thing to decide a leg's verdict: `os.path.isfile(out/<tag>.json)`, then print
# that file's `pass` field. NOTHING tied the file to the run. A leg that never
# started left the PREVIOUS run's JSON sitting under the same name, and the
# summary printed its verdict as this run's -- observed as
# `firefox.long PASS ... frames_live=ok` for a leg whose driver had died on an
# argparse error in under a second. A paired A/B lane hit the same shape and
# came back with twelve "independent" three-minute runs all reporting
# `total_s=179.18`, identical to the centisecond, because they were twelve
# reads of one file.
#
# That is this repo's *vacuous verification* pattern -- a check that cannot
# fail -- sitting in the landing gate rather than in a test. Every "tier-2 8/8
# PASS" quoted from this summary rested on it.
#
# THREE MECHANISMS, and the reason it is three rather than one:
#
#   1. RUN ID (primary). Each attempt mints a token, passes it to drive.py,
#      which copies it into the JSON as `run_id`; the summary demands an EXACT
#      match. This is the only one of the three that cannot be defeated by a
#      clock: not by a filesystem with one-second timestamps, not by a re-run
#      inside the same second, not by an unrelated `touch`. It also travels
#      WITH the artefact, so a comparison script that copies these files
#      elsewhere -- which is what the A/B lane was doing -- can check it too.
#   2. WIPE BEFORE START. Every artefact a tag can produce is deleted before
#      each attempt, so "the file is missing" means this leg wrote nothing,
#      rather than "some earlier run's file is still lying there". Belt to the
#      run id's braces: it makes the stale case rare instead of merely
#      detectable.
#   3. DRIVER EXIT CODE. Recorded per leg so the summary can separate "ran and
#      failed" from "never started". drive.py now spends four distinct codes
#      (0 / 2 / 64 usage / 69 infrastructure) precisely so this is possible.
#
# A gate that passed only 3 would still be a gate; the run id alone is the one
# that carries the property. The other two make the failure legible.
LEDGER="$OUT_DIR/tier2-legs.tsv"
: > "$LEDGER"

# One token per ATTEMPT, not per leg: the quarantine retry is a different run
# of the same leg and must not be able to satisfy the first attempt's check.
new_run_id() {
  local u=""
  if [ -r /proc/sys/kernel/random/uuid ]; then
    u="$(cat /proc/sys/kernel/random/uuid 2>/dev/null)"
  fi
  if [ -z "$u" ]; then
    # No uuid source. Nanoseconds + pid + two draws from $RANDOM, which is
    # seeded per shell -- enough that two attempts in the same run cannot
    # collide, which is all this has to guarantee.
    u="$(date -u +%Y%m%dT%H%M%S%N)-$$-${RANDOM}${RANDOM}"
  fi
  printf 'tier2-%s' "$u"
}

# How drive.py's exit code should be read. The `infra` fallback grep is a
# backstop for the case drive.py cannot classify itself -- it is the child
# process that ran out of room, and if the disk filled while it was writing its
# own stderr the code may not have made it out either.
classify_rc() {
  local rc="$1" errf="$2"
  case "$rc" in
    0)  printf ran ;;
    64) printf usage ;;
    69) printf infra ;;
    *)
      if [ -s "$errf" ] && grep -qE \
          "Errno (28|122)|No space left on device|Disk quota exceeded" \
          "$errf"; then
        printf infra
      else
        printf ran
      fi
      ;;
  esac
}

# ---------------------------------------------------------------- build ----
if [ "$SKIP_BUILD" -eq 0 ]; then
  echo "building squallar-web (wasm-pack; --skip-build to serve as-is)"
  # Through wasm-threads.sh, which is the ONLY supported way to build this
  # bundle since WS3b: it pins the nightly, rebuilds std against `+atomics`
  # and passes the `--shared-memory`/`--import-memory` link args wasm-bindgen
  # keys its thread glue off. A plain `wasm-pack build` does not fall back to
  # a slower module, it fails to compile -- wasm-bindgen-rayon carries a
  # `compile_error!` for a wasm32 target without atomics.
  (cd "$REPO_ROOT" &&
    .github/scripts/wasm-threads.sh \
      wasm-pack build squallar-web --target web --release --no-typescript --no-pack) || {
    echo "FATAL: wasm-pack build failed" >&2
    exit 1
  }
fi
if [ ! -f "$WEB_DIR/pkg/squallar_web_bg.wasm" ]; then
  echo "FATAL: $WEB_DIR/pkg/squallar_web_bg.wasm missing -- build first" >&2
  exit 1
fi

SERVER_PID=""
stop_server() {
  if [ -n "$SERVER_PID" ]; then
    kill "$SERVER_PID" 2>/dev/null
    wait "$SERVER_PID" 2>/dev/null
    SERVER_PID=""
  fi
}

cleanup() {
  local rc=$?
  trap - EXIT INT TERM
  stop_server
  # any driver process group drive.py did not get to tear down
  # (drive.py removes its pgid file on a clean stop)
  local f pgid
  for f in "$OUT_DIR"/*.driver.pgid; do
    [ -f "$f" ] || continue
    pgid="$(cat "$f" 2>/dev/null)"
    if [ -n "$pgid" ]; then
      echo "cleanup: killing leftover driver process group $pgid ($f)" >&2
      kill -TERM -- "-$pgid" 2>/dev/null
    fi
    rm -f "$f"
  done
  sleep 0.5
  for f in "$OUT_DIR"/*.driver.pgid; do
    [ -f "$f" ] || continue
    pgid="$(cat "$f" 2>/dev/null)"
    [ -n "$pgid" ] && kill -KILL -- "-$pgid" 2>/dev/null
    rm -f "$f"
  done
  exit "$rc"
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------- serve ----
# Fresh (kernel-chosen) port every pass: no stale service-worker scope, no
# cross-run cache identity, and a fresh --doctor-first-worker arm each time.
# Sets PORT and URL; returns non-zero on failure.
start_server() {
  local ready_file="$OUT_DIR/serve.ready"
  : > "$ready_file"
  "$PY" "$RIG_DIR/serve.py" --dir "$WEB_DIR" --port 0 \
      --log "$OUT_DIR/serve.log" \
      --seed-local-storage "${SEED:-$SEED_LS}" \
      "$@" ${SERVE_EXTRA[@]+"${SERVE_EXTRA[@]}"} \
      > "$ready_file" 2>> "$OUT_DIR/serve.stderr" &
  SERVER_PID=$!
  PORT=""
  local _tag _base
  for _ in $(seq 1 100); do
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
      echo "FATAL: serve.py exited early:" >&2
      cat "$OUT_DIR/serve.stderr" >&2
      return 1
    fi
    if [ -s "$ready_file" ]; then
      read -r _tag PORT _base < "$ready_file"
      break
    fi
    sleep 0.1
  done
  if [ -z "$PORT" ]; then
    echo "FATAL: serve.py never printed its ready line" >&2
    return 1
  fi
  URL="http://127.0.0.1:$PORT/index-rig.html"
  echo "serving $WEB_DIR on port $PORT (pid $SERVER_PID) -> $URL"
  return 0
}

# ---------------------------------------------------------------- drive ----
EXTRA=()
if [ -n "${RIG_DRIVE_EXTRA:-}" ]; then
  # shellcheck disable=SC2206
  EXTRA+=($RIG_DRIVE_EXTRA)
fi
if [ ${#SERVE_EXTRA[@]} -gt 0 ]; then
  echo "serve.py extra args: ${SERVE_EXTRA[*]}"
fi
if [ ${#EXTRA[@]} -gt 0 ]; then
  echo "drive.py extra args: ${EXTRA[*]}"
fi

# run_pass <browser> <tag> <leg live|doctored|gesture|long>: one server + one
# drive.py run.
#
# Sets three globals for the caller, and they are the whole binding contract:
#   LAST_RUN_ID  the token this attempt minted and handed to drive.py
#   LAST_RC      drive.py's exit code (or 70 if the server never came up, in
#                which case drive.py was never execed at all)
#   LAST_CLASS   ran | usage | infra | nostart -- see classify_rc
LAST_RUN_ID=""
LAST_RC=0
LAST_CLASS=""
run_pass() {
  local browser="$1" tag="$2" leg="$3"
  local driver server_args=() drive_args=() SEED=""
  LAST_RUN_ID="$(new_run_id)"
  LAST_RC=0
  LAST_CLASS=""
  # Mechanism 2. Everything this tag can write goes first, so a leg that dies
  # before producing anything leaves an EMPTY slot rather than the last run's
  # verdict wearing this run's name.
  rm -f "$OUT_DIR/$tag.json" \
        "$OUT_DIR/$tag.samples.tsv" \
        "$OUT_DIR/$tag.page.png" "$OUT_DIR/$tag.canvas.png" \
        "$OUT_DIR/$tag.fail.png" "$OUT_DIR/$tag.driver.log" \
        "$OUT_DIR/$tag.driver.stderr" "$OUT_DIR/$tag.xvfb.log"
  case "$browser" in
    chromium) driver="$CHROMEDRIVER" ;;
    firefox)  driver="$GECKODRIVER" ;;
    *) echo "unknown browser: $browser" >&2; return 1 ;;
  esac
  if [ "$leg" = wideloop ]; then
    # The `long` leg's scene at the `wide` leg's canvas AND world zoom -- the
    # union a user was in, which no other leg here expresses. See the
    # WIDELOOP block above for the seed's composition and for the sampler.
    SEED="$WIDELOOP_SEED_LS"
    server_args+=(--pin-clock "$LONG_PIN_CLOCK")
    # Both ends of the pin, so a pin that did not reach the page is a named
    # failure rather than a leg that quietly measured today.
    drive_args+=(--expect-pinned-clock "$LONG_PIN_CLOCK")
    drive_args+=(--expect-loop-or-refusal)
    drive_args+=(--canvas "$WIDELOOP_CANVAS" --expect-canvas
                 --window "$WIDELOOP_WINDOW"
                 --settle "$WIDELOOP_SETTLE" --data-window "$WIDELOOP_WINDOW_S"
                 --expect-frame-progress "$WIDELOOP_PROGRESS_WINDOW"
                 --expect-no-alloc-failure
                 --expect-linear-headroom "$WIDELOOP_LINEAR_HEADROOM"
                 --sample-tsv "$OUT_DIR/$tag.samples.tsv"
                 --sample-interval "$WIDELOOP_SAMPLE_INTERVAL"
                 --frames "$WIDELOOP_FRAMES")
    sample_header_lines "$OUT_DIR/$tag.samples.tsv" "$SEED" "$tag" \
                       "$WIDELOOP_CANVAS" "$WIDELOOP_WINDOW"
  elif [ "$leg" = huge ]; then
    # The long leg's scene at the user's canvas. Same seventeen layers and the
    # same playing loop -- the freeze reproduces on a scene that is drawing,
    # and this leg differs from `long` in SIZE, which is the variable under
    # test. --expect-canvas rides along so a leg that could not be made this
    # big fails instead of quietly reporting a small one.
    SEED="$LONG_SEED_LS"
    server_args+=(--pin-clock "$LONG_PIN_CLOCK")
    # Both ends of the pin, so a pin that did not reach the page is a named
    # failure rather than a leg that quietly measured today.
    drive_args+=(--expect-pinned-clock "$LONG_PIN_CLOCK")
    drive_args+=(--expect-loop-or-refusal)
    drive_args+=(--canvas "$HUGE_CANVAS" --expect-canvas
                 --window "$HUGE_WINDOW"
                 --settle "$HUGE_SETTLE" --data-window "$HUGE_WINDOW_S"
                 --expect-frame-progress "$HUGE_PROGRESS_WINDOW"
                 --expect-no-alloc-failure
                 --expect-linear-headroom "$HUGE_LINEAR_HEADROOM"
                 --sample-tsv "$OUT_DIR/$tag.samples.tsv"
                 --sample-interval "$HUGE_SAMPLE_INTERVAL"
                 --frames "$HUGE_FRAMES")
    sample_header_lines "$OUT_DIR/$tag.samples.tsv" "$SEED" "$tag" \
                       "$HUGE_CANVAS" "$HUGE_WINDOW"
  elif [ "$leg" = tilecache ]; then
    # The static scene, then the settle assertion over its tail. See the
    # TILECACHE block above for the scene, the two sizes and the hold-out.
    SEED="$TILECACHE_SEED_LS"
    drive_args+=(--settle "$TILECACHE_SETTLE" --data-window "$TILECACHE_WINDOW_S"
                 --expect-tile-cache-settles "$TILECACHE_SETTLES"
                 --expect-frame-progress "$TILECACHE_PROGRESS_WINDOW")
    if [ "$TILECACHE_WINDOW" = 1280x900 ]; then
      # The control: the rig's default window, the canvas the browser gives.
      drive_args+=(--window "$TILECACHE_WINDOW")
    else
      # The repro, or another canvas target: asserted, and started above, for
      # HUGE_WINDOW's reason.
      drive_args+=(--canvas "$TILECACHE_WINDOW" --expect-canvas
                   --window "$(tilecache_window_for_canvas "$TILECACHE_WINDOW")")
    fi
  elif [ "$leg" = long ]; then
    # Ninety seconds, an overlay loop, nobody touching the page, and one
    # assert: the app's own frame counter was still moving at the end of it.
    # The worker-wire waits stay on the live leg -- this leg asks whether the
    # frame loop is ALIVE after a minute and a half, and stacking the boot-time
    # assertions onto it would only make a slow leg slower.
    SEED="$LONG_SEED_LS"
    server_args+=(--pin-clock "$LONG_PIN_CLOCK")
    # Both ends of the pin, so a pin that did not reach the page is a named
    # failure rather than a leg that quietly measured today.
    drive_args+=(--expect-pinned-clock "$LONG_PIN_CLOCK")
    # Played, or refused and said so. Once the web's admission doors enforce,
    # this seeded loop is refused by design; without this the leg goes green
    # for a scene that did nothing, and a green nobody can interpret is
    # worth less than a red anybody can.
    drive_args+=(--expect-loop-or-refusal)
    drive_args+=(--settle "$LONG_SETTLE" --data-window "$LONG_WINDOW"
                 --expect-frame-progress "$LONG_PROGRESS_WINDOW")
  elif [ "$leg" = wide ]; then
    # The `long` leg's assertion at the USER'S canvas on the user's scene. Same
    # one thing gated -- the app's own frame counter still climbing -- because
    # the same nothing-unwinds trap is what stops it. See WIDE_SEED_LS above
    # for why the window is the leg.
    SEED="$WIDE_SEED_LS"
    drive_args+=(--window "$WIDE_WINDOW" --canvas "$WIDE_CANVAS"
                 --settle "$WIDE_SETTLE" --data-window "$WIDE_WINDOW_S"
                 --expect-frame-progress "$WIDE_PROGRESS_WINDOW")
  elif [ "$leg" = gesture ]; then
    # The short leg: real W3C-actions input, and ONE assert -- the interact
    # COUNT grew. None of the worker-wire waits ride here (the live leg owns
    # them); boot, canvas, rAF and zero-panics still gate inside drive.py.
    drive_args+=(--expect-interaction-frames
                 --w3c-gesture pan+wheel --gesture-seconds 4
                 --settle 3 --data-window 4)
  else
    drive_args+=(--expect-worker-round-trip --expect-timeout "$EXPECT_TIMEOUT")
    # WS3b: the worker's rayon pool really came up. 2 and not the requested
    # count -- see wait_rayon_pool in drive.py; the request is
    # hardwareConcurrency-derived and so is a property of the box, while 2 is
    # the smallest number that cannot be the fallback.
    drive_args+=(--expect-rayon-threads "$EXPECT_RAYON_THREADS")
    # WS3c: the replies really arrived as views into the worker's own
    # SharedArrayBuffer rather than as copies of it. The negative control is
    # this same run with serve.py's --coep dropped from SERVE_EXTRA.
    if [ "$EXPECT_ZERO_COPY" -eq 1 ]; then
      drive_args+=(--expect-zero-copy-replies)
    fi
    # WS2: the whole-picture overlay path really ran in this browser. The
    # negative control is the every-layer-off seed described beside SEED_LS
    # above -- NOT merely dropping the seeded layer, which the two default-on
    # texture overlays cover for.
    if [ "$EXPECT_OVERLAY_RASTERS" -eq 1 ]; then
      drive_args+=(--expect-overlay-rasters)
    fi
  fi
  # THIS BROWSER really applied the scene seed. Every leg, unconditionally: it
  # costs one probe, it needs no network, and what it protects is the meaning
  # of every other figure on the row. The host-side seed tests
  # (`ui_config::rig_seed_tests`) prove the literal parses into the scene this
  # script claims and say nothing about a browser reading it -- a leg pointed
  # at /index.html gets no prelude, no localStorage write, and opens on a
  # timezone-derived site with every other assertion green. That happened.
  drive_args+=(--expect-seed-applied)
  # The self-hosted vector basemap really decoded tiles. LIVE ONLY: the archive
  # is fetched by range over the real network, and the doctored leg spends its
  # first seconds terminating and refetching a worker while the gesture leg is
  # four seconds long -- neither is the place to put a network read's floor.
  # The negative control is the shipped `usize`->`u64` offset defect itself,
  # described in the header above.
  if [ "$leg" = live ] && [ "$EXPECT_BASEMAP_TILES" -eq 1 ]; then
    drive_args+=(--expect-basemap-tiles)
  fi
  if [ "$leg" = doctored ]; then
    server_args+=(--doctor-first-worker)
    drive_args+=(--expect-doctored-respawn)
  fi
  if ! start_server ${server_args[@]+"${server_args[@]}"}; then
    # drive.py was never execed. This is emphatically NOT "the leg failed":
    # nothing was asserted, and a summary that prints FAIL here would send a
    # reader to look at the app.
    LAST_RC=70
    LAST_CLASS=nostart
    return 1
  fi
  local errf="$OUT_DIR/$tag.driver.stderr"
  : > "$errf"
  # stderr is captured rather than streamed so classify_rc can read it, then
  # replayed in full -- drive.py's own progress goes to stdout, so nothing a
  # watcher relies on stops being live.
  "$PY" "$RIG_DIR/drive.py" \
      --browser "$browser" --url "$URL" \
      --out-dir "$OUT_DIR" --tag "$tag" --run-id "$LAST_RUN_ID" \
      --driver "$driver" --frames "$FRAMES" \
      "${drive_args[@]}" \
      ${EXTRA[@]+"${EXTRA[@]}"} 2>"$errf"
  local rc=$?
  cat "$errf" >&2
  LAST_RC="$rc"
  LAST_CLASS="$(classify_rc "$rc" "$errf")"
  stop_server
  return "$rc"
}

# An argument drive.py refuses is fatal to the RUN, not to the leg. Every
# remaining leg is about to be handed the same argument list and die the same
# way, and eight copies of one usage dump is how the real sentence got lost the
# first time. Retrying is worse than useless: a rejected flag is not a flake.
abort_on_usage() {
  local tag="$1"
  [ "$LAST_CLASS" = usage ] || return 0
  echo >&2
  echo "################################################################" >&2
  echo "FATAL: drive.py REFUSED THE ARGUMENTS run_tier2.sh passed it." >&2
  echo "  leg: $tag" >&2
  grep -m1 -E "^drive.py: error:|unrecognized arguments" \
      "$OUT_DIR/$tag.driver.stderr" 2>/dev/null | sed 's/^/  /' >&2
  echo >&2
  echo "  NOTHING RAN. This is not a leg failure and not a flake, so there" >&2
  echo "  is no retry and no further leg: the rest of the roster would die" >&2
  echo "  the same way. The usual cause is run_tier2.sh and drive.py coming" >&2
  echo "  from DIFFERENT commits -- a rebase that dropped one of them is" >&2
  echo "  exactly how this was found." >&2
  echo "################################################################" >&2
  exit 64
}

overall=0
for browser in $BROWSERS; do
  for leg in $LEGS; do
    if [ "$leg" = live ]; then
      tag="$browser"
    else
      tag="$browser.$leg"
    fi
    echo
    echo "================ $tag ================"
    if run_pass "$browser" "$tag" "$leg"; then
      printf '%s\t%s\t%s\t%s\t%s\n' "$tag" "$LAST_CLASS" "$LAST_RUN_ID" \
          "$LAST_RC" 1 >> "$LEDGER"
      continue
    fi
    abort_on_usage "$tag"
    # Retry-once quarantine (live-network flake policy): a fresh server, a
    # fresh port, a fresh browser profile. A second failure fails the leg.
    echo "$tag FAILED (rc=$LAST_RC, $LAST_CLASS); one quarantine retry" \
         "(live-network flake policy)" >&2
    preserve_attempt "$tag"
    echo "================ $tag (retry) ================"
    if ! run_pass "$browser" "$tag" "$leg"; then
      abort_on_usage "$tag"
      echo "$tag failed twice (rc=$LAST_RC, $LAST_CLASS)" >&2
      overall=1
    fi
    # The row describes the LAST attempt, whose artefacts are on the unsuffixed
    # paths. The first attempt's now survive beside them as `$tag.attempt1.*`
    # rather than being overwritten, so a quarantined failure stays explicable.
    printf '%s\t%s\t%s\t%s\t%s\n' "$tag" "$LAST_CLASS" "$LAST_RUN_ID" \
        "$LAST_RC" 2 >> "$LEDGER"
  done
done

# -------------------------------------------------------------- summary ----
echo
echo "================ tier2 summary ================"
# The roster this run actually drove, printed beside its verdict. A tally
# ("N/M PASS") describes whatever roster produced it, and without this line the
# denominator is invisible -- which is the same defect as an unbound artefact,
# one level up.
echo "roster: [$LEGS] x [$BROWSERS]"
if [ "$LEGS" != "$DEFAULT_LEGS" ]; then
  echo "NOTE: RIG_LEGS replaced the default roster [$DEFAULT_LEGS]."
  echo "      This run is a debugging aid, NOT the Tier-2 gate, and its tally"
  echo "      must not be quoted as one."
fi
if [ "$BROWSERS" != "chromium firefox" ]; then
  echo "NOTE: RIG_BROWSERS replaced the default [chromium firefox]. Web is two"
  echo "      targets and one of them is missing from this run."
fi
"$PY" - "$OUT_DIR" "$LEDGER" <<'EOF'
# THREE STATES, and the distinction between the last two is the whole point:
#
#   PASS         a JSON exists, it carries THIS attempt's run id, and its
#                `pass` is true.
#   FAIL         same binding, `pass` false. The leg ran; the app was wrong.
#   DID NOT RUN  no JSON, an unreadable JSON, or -- the case this was written
#                for -- a JSON carrying somebody else's run id. Nothing was
#                asserted. It is NOT a pass and it is NOT a leg failure, and
#                printing it as either is how a dead leg shipped as green.
#   INFRA        the box ran out of room. Also nothing asserted, but the
#                repair is somewhere else, so it does not hide among the
#                did-not-runs.
#
# Every state except PASS makes this block exit non-zero, so the summary is
# itself a gate rather than a report about one.
import json, os, sys
out, ledger_path = sys.argv[1], sys.argv[2]


def _absent_because(r, family, never_moved):
    """Why a telemetry total is missing — and it is never one fact.

    A `-` here used to be printed as certainty: "the line was never written:
    no overlay raster ever moved". That sentence is FALSE for the commonest
    cause of a missing total, which is that the app wrote the line and this
    rig could not parse it — the field lists are version-coupled (`inked`
    joined the raster line at 79bad6c7, 2026-08-31) and a `--skip-build` over
    a stale squallar-web/pkg drives an older app than drive.py speaks.
    Measured 2026-08-31: that pairing printed "no overlay raster ever moved"
    for a page writing the line every frame with every counter climbing, and
    two lanes read it as a renderer regression.
    """
    seen = (r.get("telemetry_unparsed") or {}).get(family)
    if seen:
        return ("(RIG/BUNDLE SKEW: the app wrote this line and this rig "
                "could not parse it -- %r. The served squallar-web/pkg is an "
                "older build than drive.py speaks; REBUILD IT, without "
                "--skip-build. This says nothing about whether the path ran.)"
                % seen[:200])
    return ("(no line this rig could read ever reached the console ring: "
            "either the telemetry key is unseeded, or %s)" % never_moved)

legs = []
with open(ledger_path) as f:
    for line in f:
        line = line.rstrip("\n")
        if not line:
            continue
        parts = line.split("\t")
        while len(parts) < 5:
            parts.append("")
        legs.append(dict(zip(("tag", "cls", "run_id", "rc", "attempts"), parts)))

states = []
for leg in legs:
    tag, cls, want = leg["tag"], leg["cls"], leg["run_id"]
    p = os.path.join(out, tag + ".json")

    def dnr(why):
        states.append((tag, "DID NOT RUN"))
        print("%-18s DID NOT RUN  %s" % (tag, why))
        print("%-18s   (rc=%s, driver outcome=%s). NOTHING WAS ASSERTED on "
              "this leg -- it neither passed nor failed." % ("", leg["rc"], cls))

    if cls == "nostart":
        states.append((tag, "DID NOT RUN"))
        print("%-18s DID NOT RUN  serve.py never came up; drive.py was never "
              "started" % tag)
        continue
    if cls == "usage":
        dnr("drive.py refused its arguments (exit 64)")
        continue
    if cls == "infra":
        states.append((tag, "INFRA"))
        print("%-18s INFRA FAIL   the box ran out of room (rc=%s). This is not "
              "a verdict on the app." % (tag, leg["rc"]))
        continue
    if not os.path.isfile(p):
        dnr("%s was never written" % p)
        continue
    try:
        r = json.load(open(p))
    except Exception as e:
        dnr("%s is unreadable (%s)" % (p, e))
        continue
    got = r.get("run_id")
    if got != want:
        # THE STALE READ. Before 2026-08-31 this branch did not exist and the
        # line below was printed as PASS.
        dnr("%s is a STALE ARTEFACT -- it carries run_id %r, this attempt was "
            "%r" % (p, got, want))
        print("%-18s   the file on disk was left by an EARLIER run (it says "
              "started_utc=%s, total_s=%s) and describes nothing that happened "
              "just now." % ("", r.get("started_utc"), r.get("total_s")))
        continue

    states.append((tag, "PASS" if r.get("pass") else "FAIL"))
    v = r.get("verdict") or {}
    env = r.get("env") or {}
    b = r.get("canvas_final") or (r.get("boot") or {}).get("probe") or {}
    rw = r.get("raf_warm") or {}
    def raf(d):
        return ("p50=%.2f p95=%.2f" % (d["p50"], d["p95"])
                if d.get("ok") else "FAILED")
    sh = r.get("screenshots") or {}
    wrt = r.get("worker_round_trip")
    dr = r.get("doctored_respawn")
    ifr = r.get("interaction_frames")
    bmg = r.get("basemap_tiles")
    fp = r.get("frame_progress")
    lor = r.get("loop_or_refusal")
    def tri(x):
        return "-" if x is None else ("ok" if x.get("ok") else "FAIL")
    # `playing` / `refused` / `NEITHER` -- the third is the failure, and it
    # is spelled out because "-" would read as "not asserted".
    def loop_col(x):
        if x is None:
            return "-"
        if x.get("playing"):
            return "playing"
        return "refused" if x.get("refused") else "NEITHER"
    # THE SIZE, on the headline row and in DEVICE pixels. Every other figure on
    # this row -- raster bytes, texture uploads, frame times, and the liveness
    # verdict itself -- is a figure PER THIS SIZE. A leg that passed at
    # 1280x815 and one that passed at 2878x1651 are not the same evidence, and
    # a freeze that only reproduces above some size is invisible in a summary
    # that does not say which one ran. `buf` is the drawing buffer, which is
    # what was rasterized; the CSS box follows it in parentheses when they
    # differ (a device pixel ratio other than 1).
    ct = r.get("canvas_target") or {}
    buf = (v.get("canvas_buffer")
           or ("%sx%s" % (b.get("bufferWidth"), b.get("bufferHeight"))
               if b.get("bufferWidth") else "?"))
    css = "%sx%s" % (b.get("clientWidth"), b.get("clientHeight"))
    size = buf if buf == css else "%s (css %s)" % (buf, css)
    if ct:
        size += " [asked %s, met=%s]" % (ct.get("asked"), ct.get("met"))
    print("%-18s %s  boot=%s canvas=%s raf[%s] canvas_blank=%s "
          "errors=%s panics=%s traps=%s round_trip=%s respawn=%s interact=%s "
          "basemap=%s frames_live=%s loop=%s"
          % (tag, "PASS" if r.get("pass") else "FAIL",
             v.get("booted"), size,
             raf(rw),
             (sh.get("canvas") or {}).get("blank"),
             v.get("rig_error_count"), v.get("panic_count"),
             v.get("wasm_trap_count"),
             tri(wrt), tri(dr), tri(ifr), tri(bmg), tri(fp), loop_col(lor)))
    if lor is not None and not lor.get("ok"):
        print("%-18s   loop EXPECT FAILED: %s" % ("", lor.get("error")))
    cve = r.get("canvas_expect")
    if cve is not None and not cve.get("ok"):
        print("%-18s   canvas EXPECT FAILED: %s" % ("", cve.get("error")))
    tcs = r.get("tile_cache_settles")
    if tcs is not None:
        # The tilecache leg's own figure: three deltas over the settle
        # window, never added, and how many times the base source snapped
        # to the whole zoom. One snap opens the window at its own tick; two
        # fail the leg.
        fams = tcs.get("families") or {}
        print("%-18s   tile cache settles %s over %s; snap flips %s; %s"
              % ("", "OK" if tcs.get("ok") else "FAILED",
                 tcs.get("window_basis") or ("the last %ss" % tcs.get("window_s")),
                 tcs.get("snap_flips", "-"),
                 ", ".join("%s delta %s" % (name.split(":")[-1].strip(),
                                            fam.get("delta"))
                           for name, fam in fams.items()) or "no families"))
        if tcs.get("error"):
            print("%-18s   tile cache settles: %s" % ("", tcs["error"]))
        for name, fam in fams.items():
            if not fam.get("ok"):
                print("%-18s   %s: %s" % ("", name, fam.get("error")))
    if fp is not None:
        # The long leg's own figure, and the only thing it gates on: how many
        # frames the app's own counter gained over the last window, and how
        # stale its newest reading was when the leg ended. A frozen page
        # reports gained=None and a stale_ms in the tens of thousands.
        print("%-18s   frames gained %s over the last %ss "
              "(newest reading %s ms old, %s readings in window)"
              % ("", fp.get("gained"), fp.get("window_s"),
                 fp.get("stale_ms"), fp.get("in_window")))
        if fp.get("error"):
            print("%-18s   frame progress: %s" % ("", fp["error"]))
    # THE WALL AND THE APPROACH TO IT. Two readings, never added: how close the
    # page's own linear memory came to the ceiling THE APP REPORTS, and what
    # the allocator refused when it got there. Printed whenever the leg asked
    # for them, pass or fail, because the headroom is the figure the fix will
    # be judged by and a green that does not say 424 MiB is not evidence.
    lh = r.get("linear_headroom")
    if lh is not None:
        print("%-18s   linear page peak %s of %s MiB ceiling (headroom %s MiB, "
              "want >= %s) %s"
              % ("", lh.get("peak_page_mib"), lh.get("ceiling_page_mib"),
                 lh.get("headroom_mib"), lh.get("want_headroom_mib"),
                 "OK" if lh.get("ok") else "FAILED"))
        if lh.get("error"):
            print("%-18s   linear headroom: %s" % ("", lh["error"]))
    afl = r.get("alloc_failures")
    if afl is not None:
        print("%-18s   alloc refusals %s%s %s"
              % ("", afl.get("count"),
                 (" on " + ", ".join(afl.get("instances") or []))
                 if afl.get("count") else "",
                 "OK" if afl.get("ok") else "FAILED"))
        if afl.get("first"):
            print("%-18s   first refusal: %s" % ("", afl["first"][:160]))
    cs = r.get("census_samples")
    if cs is not None:
        print("%-18s   census samples %s rows every %ss over leg %s-%s s "
              "(%s probe errors) -> %s"
              % ("", cs.get("rows"), cs.get("interval_s"),
                 cs.get("first_leg_s"), cs.get("last_leg_s"),
                 cs.get("probe_errors"), cs.get("path")))
    if v.get("first_wasm_trap"):
        print("%-18s   first wasm trap: %s"
              % ("", str(v["first_wasm_trap"]).splitlines()[0][:160]))
    if ifr is not None:
        # The gesture leg's own figure: a COUNT, and the only thing the leg
        # gates on. The wheel route is named because a synthesized fallback
        # skipped the browser's input pipeline and the two are not the same
        # measurement.
        print("%-18s   interaction frames %s -> %s (wheel via %s)"
              % ("", ifr.get("before"), ifr.get("after"),
                 (r.get("w3c_gesture") or {}).get("wheel_source")))
    wg = env.get("webgpu") or {}
    if wg.get("gpu_object") is None and not wg.get("probe_error"):
        webgpu = "-"
    elif wg.get("probe_error"):
        webgpu = "probe-error"
    elif not wg.get("gpu_object"):
        webgpu = "absent"
    elif wg.get("adapter"):
        webgpu = "adapter(maxTex2D=%s)" % (
            (wg.get("adapter_limits") or {}).get("maxTextureDimension2D"))
    else:
        webgpu = "object-but-no-adapter"
    sw = r.get("service_worker") or {}
    # The caps are properties of the ADAPTER, and this gate runs the SOFTWARE
    # arm, so they describe SwiftShader / llvmpipe and not the machine. Name
    # the arm and the renderer on the same line as the numbers -- a cap quoted
    # without its adapter is how a budget gets sized for a rasteriser.
    ad = r.get("adapter") or {}
    print("%-18s   arm=%s adapter=%s (%s)"
          % ("", r.get("arm", "software"), ad.get("class", "?"),
             ad.get("renderer") or env.get("gl_renderer") or "?"))
    print("%-18s   caps [%s] max_texture=%s max_3d=%s cores=%s webgpu=%s"
          % ("", ad.get("renderer") or env.get("gl_renderer") or "?",
             env.get("max_texture_size"), env.get("max_3d_texture_size"),
             env.get("hardware_concurrency"), webgpu))
    # WHICH API the app ended up on, from its own startup log. `webgpu=` above
    # is what the BROWSER offers; this is what the BUILD took. They differ on
    # every browser where requestAdapter() answers null.
    app = r.get("app_backend") or {}
    line = app.get("backend") or ""
    head = "wgpu selected the "
    i = line.find(head)
    sel = "UNKNOWN"
    if i >= 0:
        rest = line[i + len(head):]
        j = rest.find(" backend")
        sel = rest[:j] if j > 0 else rest
    print("%-18s   app selected backend=%s" % ("", sel))
    if app.get("raster_ceiling"):
        print("%-18s   %s" % ("", app["raster_ceiling"]))
    print("%-18s   isolation crossOriginIsolated=%s SAB=%s sw_blocked=%s "
          "sw_regs=%s resource_failures=%s"
          % ("", env.get("cross_origin_isolated"),
             env.get("shared_array_buffer"), sw.get("blocked_by_rig"),
             len(sw.get("registrations") or []),
             len((r.get("resources") or {}).get("failed") or [])))
    if sw.get("expect_error"):
        print("%-18s   sw EXPECT FAILED: %s" % ("", sw["expect_error"]))
    for f in ((r.get("resources") or {}).get("failed") or [])[:8]:
        print("%-18s   resource status=%s %s" % ("", f.get("status"), f.get("u")))
    if dr and dr.get("ok"):
        print("%-18s   respawn attach %.0f ms after the doctored refusal"
              % ("", dr.get("delta_ms", -1)))
    # WS2 baseline. TWO lines with TWO denominators, never added: the first is
    # the whole-picture overlay dispatch alone, the second is every texture
    # delta this renderer was shown (radar, basemap tiles and font atlas
    # included). A single "bytes uploaded" over the union would describe
    # neither. `-` means NO LINE THIS RIG COULD READ arrived, which is a
    # different fact from zero bytes and is itself two different facts --
    # see `_absent_because`, which is what says which one this was.
    ort = r.get("overlay_raster_totals")
    if ort:
        # `inked` rides beside its own denominator, `pictures`. It is a subset
        # of that count and is never added to anything: how many of the
        # buffers handed to egui had a single non-zero byte in them.
        print("%-18s   overlay rasters [overlay dispatch only] "
              "%s dispatched -> %s arrived -> %s pictures / %s B "
              "(%s of %s inked); "
              "%s shown, %s promoted, %s dropped, %s superseded, %s cancelled"
              % ("", ort.get("dispatched"), ort.get("arrived"),
                 ort.get("pictures"), ort.get("picture_bytes"),
                 ort.get("inked"), ort.get("pictures"),
                 ort.get("shown"), ort.get("promoted"), ort.get("dropped"),
                 ort.get("superseded"), ort.get("cancelled")))
    else:
        print("%-18s   overlay rasters - %s" % ("", _absent_because(r, "rasters",
              "no overlay raster ever moved")))
    tut = r.get("texture_upload_totals")
    if tut:
        # `whole` is a routing subset of the blocking figure, never added to
        # it; the GPU total is the disjoint pair staged + blocking.
        print("%-18s   texture uploads [EVERY egui texture] %s deltas, %s B "
              "to the GPU (%s B whole; %s bands, %s B staged, %s B "
              "blocking the frame)"
              % ("", tut.get("deltas"), tut.get("bytes"),
                 tut.get("whole_bytes"), tut.get("bands"),
                 tut.get("staged_bytes"), tut.get("blocking_bytes")))
    else:
        print("%-18s   texture uploads - %s" % ("", _absent_because(r, "uploads",
              "no texture delta ever moved")))
    # The THIRD denominator: bodies DECODED, in neither figure above. `vector`
    # is the self-hosted basemap and is the one gated on; `raster` is the
    # terrain hillshade (legitimately zero with terrain off) and `sniffed` is
    # an archive that declared no tile_type, which none of ours does.
    bmtt = r.get("basemap_tile_totals")
    if bmtt:
        print("%-18s   basemap tiles [archive BODIES DECODED, in neither "
              "figure above] %s vector, %s raster, %s sniffed"
              % ("", bmtt.get("vector_tiles"), bmtt.get("raster_tiles"),
                 bmtt.get("sniffed_tiles")))
    else:
        print("%-18s   basemap tiles - %s" % ("", _absent_because(r, "basemap",
              "no archive body ever decoded")))
    bmt = r.get("basemap_tiles")
    if bmt is not None and not bmt.get("ok"):
        print("%-18s   basemap tiles EXPECT FAILED: %s" % ("", bmt.get("error")))
    for d in (wrt, dr):
        if d and not d.get("ok"):
            print("%-18s   %s" % ("", d.get("error")))
    if v.get("first_panic"):
        print("%-18s first panic: %s" % ("", v["first_panic"][:180]))
    if r.get("exception"):
        print("%-18s failed at stage %r: %s"
              % ("", r.get("failed_stage"), r.get("exception")))

# ---- the tally, and the reason it is printed as four numbers -------------
# "8/8 PASS" is the sentence this gate is quoted by, and it is only true if
# the other three columns are zero. Printing them always means nobody can
# quote the first number without the row that would contradict it.
n = len(states)
passed = sum(1 for _, s in states if s == "PASS")
failed = sum(1 for _, s in states if s == "FAIL")
dnrun = sum(1 for _, s in states if s == "DID NOT RUN")
infra = sum(1 for _, s in states if s == "INFRA")
print()
print("tier2: %d/%d PASS   (%d failed, %d DID NOT RUN, %d infrastructure)"
      % (passed, n, failed, dnrun, infra))
if n == 0:
    print("tier2 VERDICT: FAILURE -- no leg was recorded at all")
    sys.exit(1)
if passed != n:
    print("tier2 VERDICT: FAILURE")
    for tag, s in states:
        if s != "PASS":
            print("  %-18s %s" % (tag, s))
    sys.exit(1)
print("tier2 VERDICT: PASS -- every leg produced a result bound to this run")
EOF
summary_rc=$?
# The summary is a gate, not a report about one. It can redden a run the leg
# loop called green: the loop only knows drive.py's exit code, while this block
# is the only thing that checks the artefact it left behind is actually this
# run's.
if [ "$summary_rc" -ne 0 ]; then
  overall=1
fi

echo
echo "artifacts in $OUT_DIR:"
ls -l "$OUT_DIR" | sed 's/^/  /'
exit "$overall"
