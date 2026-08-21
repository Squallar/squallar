# rustdar

**`ARCHITECTURE.md` is the authority on how this workspace is shaped.** Read §1 (crate
graph), §4 (adding a source) and §6 (ratchets) before changing structure. This file is the
short list of things that will bite you.

## Adding a data source

One crate's work: implement `SourceHandler` in `rustdar-overlays` (or `rustdar-radar` for a
radar network) and register it. Nothing above should need an arm for it. The claim is
executable — `cargo test -p rustdar-app --test fake_source_acceptance` proves a source
lights up catalogue, parity walk, draw, its own time axis, config round-trip and the worker
wire with zero edits outside its crate. If your change makes that test need an edit
elsewhere, the architecture regressed.

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
  **The `--lib` is load-bearing** (151 rows with it, 152 without).
- A filtered test run is not self-contained: some tests share process-global state. Run a
  clean-tree control before believing a filtered red or green.
- A zero-line `git diff` over a path that does not exist is not a proof. Show the path is
  tracked and non-empty first.

## Git

Land gated work on local main continuously. Fast-forward > rebase > cherry-pick; **never
merge**. **Nothing AI-driven ever pushes.** Never land onto a red board.
