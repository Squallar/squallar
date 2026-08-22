# rustdar

**`ARCHITECTURE.md` is the authority on how this workspace is shaped.** Read §1 (crate
graph), §4 (adding a source) and §6 (ratchets) before changing structure. This file is the
short list of things that will bite you.

## Adding a data source

Mostly one crate's work: implement `SourceHandler` in `rustdar-overlays` (or
`rustdar-radar` for a radar network) and register it. `ARCHITECTURE.md` §5 is the
checklist.

**No test proves this any more.** The `fake-source` acceptance suite that asserted "zero
edits outside its crate" was deleted with the layer it tested on 2026-08-22, and nothing
replaced it as a gate. What carries the claim now is evidence: three real sources landed in
August 2026 — SPC Fire Weather (`23df4d92`), MRMS (`4cf3dd7f`), GMGSI (`93e8606d`) — each
keeping its behaviour inside its own crate. Evidence, not a gate.

And the claim was always narrower than it read: every texture layer that renders through
the job funnel needs one arm in `App::spawn_overlay_render`
(`rustdar-app/src/app_fetch.rs`), and all three real sources added a line to it. The fake
was exempt only because it fell into that match's fallback branch and was never dispatched
through it. Budget for the registration tax plus that one row; a *new kind* of arm is what
means the architecture regressed.

## Rules that fail the build if you break them

- **`fmt` and `clippy` are PACKAGE-SCOPED. Never `--all`, never `--workspace` for a write.**
  A workspace-wide format pulls another worktree's in-flight files into your tree.
- **Coupling ceilings (`rustdar-app/tests/arch_ratchets.rs`) are permanent and may only
  FALL.** Growing the app layer's reach into the UI layer is a build failure, not a review
  comment. Shed first, then land. Re-spelling a counted reach through a local binding
  (`let gui = &mut self.gui;`) is **forbidden by name** — it makes the walker read zero while
  the coupling is identical.
- **Never hand-edit `.github/coverage-baseline.tsv` or the badge.** They are a pure function
  of the last push-to-main run.
- **The radar digest suites pass UNEDITED.** A moved digest is a bug in the encoder, not a
  pin to re-record.
- **Two different sets of ten pinned suites exist** and a report naming only "ten" is
  ambiguous: the ten *loop* suites in `rustdar-app`, and the ten *digest-carrying* suites in
  `rustdar-radar`. Mechanical re-points are fine; a moved assertion value is not.

## Product rules

- Interaction is realtime; data may lag. Map movement, controls and UI never trade latency
  for data latency.
- Heavy work never lands on the frame thread. "It runs rarely" is not an exception.
- Reopen is exactly 1:1 — every piece of UI state persists. Units go through `rustdar-units`.
- A `cfg(target_arch = "wasm32")` may select a value, a dependency or a type alias. It may
  never fork behaviour inside a function body.

## Web

Firefox is first-class and governs over Chrome. "Web" is two targets — measure both, never
merge the figures. The behavioural gate is `.github/browser-rig/run_tier2.sh` (boot, canvas,
worker wire, doctored-token respawn). It does **not** cover frame time or mobile.

## Instrument gotchas that read as green

- `cargo test -p rustdar-app arch_ratchets` selects **zero tests**. Spell it
  `--test arch_ratchets`.
- The loop pin-list roster is `cargo test -p rustdar-app --lib -- --list | grep -E "loop_|frame_build_order"`.
  **The `--lib` is load-bearing** (186 rows with it, 187 without).
- A filtered test run is not self-contained: some tests share process-global state. Run a
  clean-tree control before believing a filtered red or green.
- A zero-line `git diff` over a path that does not exist is not a proof. Show the path is
  tracked and non-empty first.

## Git

Land gated work on local main continuously. Fast-forward > rebase > cherry-pick; **never
merge**. **Nothing AI-driven ever pushes.** Never land onto a red board.
