---
name: windows-rig
description: Run squallar builds and probes on the Windows box (ssh sim). Use when measuring or reproducing on Windows, or when touching the DXGI VRAM reader. Covers the session-0 trap that makes a headed app render nothing forever, and the vcvars PATH trap that made a working reader look like dead code.
---

# Working on the Windows box

`ssh sim` — Win11 Enterprise 10.0.26200 build 26200, 16 logical CPUs, ~68 GB RAM,
RTX 5090 (driver 591.86 / 32.0.15.9186). No VPN, no wake step. It has its own
display, always on, so it is the one headed arm never contended for by the KVM
the Linux box and the Mac share (see `mac-browser-rig`) — which changes nothing
about session 0 below, since a display is not a session.

**Provenance:** these facts were established by another session and relayed, not
verified first-hand here. The Mac equivalents in `mac-browser-rig` were verified
directly. Correct this file when you confirm or contradict something.

## The session-0 trap — read before anything else

**Windows OpenSSH lands in session 0; the desktop is session 1.** An app launched
over ssh boots, creates its window, spins its event loop, and even autosaves
`ui.json` — but **`RedrawRequested` never fires**. So `handle_redraw` is never
entered, GPU init never runs, and `budget state:`, `Budgets:` and `Loop pool:`
are never composed. Confirmed at `squallar_app=debug`, and re-confirmed launching
the exe directly rather than through `start /B`.

**Anything needing a presented frame is unreachable over plain ssh here.**
Reaching session 1 needs `schtasks /create … /it`. **That is the right tool, not
a hack** — it is how you launch into an interactive desktop session on Windows,
and if you need a presented frame here you need it or something like it.

What happened in this repo is narrower than "don't use it": the permission layer
denied it twice, and the lane **did not route around the denial**. That is a fact
about one session's permissions, not a property of the command. If you hold
permission for it, use it. If you are denied, **stop and surface it** — ask the
user to allow `schtasks` over `ssh sim`, or to run one command at the console —
rather than reaching for another way in. A denial is an answer, not an obstacle.

If a leg on this box reads "the app started and then did nothing", suspect
session 0 before suspecting the app.

**What does NOT need a session:** wgpu instance creation, adapter enumeration and
the DXGI capacity read all work windowless. Build a small probe against the
crate's `pub` API rather than trying to drive the app.

`run_measure_native.sh` must **not** be run here — it is bash and hard-depends on
`xdotool`, `xrandr` and `/proc`.

## The vcvars PATH trap — a false negative that looked confirmed

`dx12.rs` was once reported as **dead code on stock Windows**. That is **false**.
wgpu 29 defaults to `Dx12Compiler::Auto`, which already falls back to FXC, and
DX12 initialises fine on a stock box with no override.

The failure was **self-inflicted by the measuring shell**. Sourcing `vcvars64`
puts two ancient `dxcompiler.dll` on `PATH` — VS `VCPackages` 0.2.0.0 and Windows
Kits 10.0.19041.5609 — both predating `IDxcCompiler3`. `Auto`'s fallback only
catches a DLL that fails to **load**, not one that loads and is **too old**.

**The rule: build under `vcvars64`, then run the binary in a shell WITHOUT it.**

The correct reading is 32,945,209,344 B = 31,419 MiB, against WMI's truncated
4,293,918,720 B — the reader beats the naive source by 7.67×. Vulkan on the same
box reads 33,750,515,712 B, also un-truncated; `nvidia-smi` reports 34,190,917,632 B
total, so the DXGI budget is 96.4 % of the card and the Vulkan heap sum 98.7 %.

**Do not reach for `wmic` to re-read that WMI figure — Microsoft removed it.** On
build 26200.6718 it is not a missing number, it is a missing command, and in a
transcript that reads exactly like a probe that found nothing. The replacement
over the same CIM data is `Get-CimInstance Win32_OperatingSystem`. Substitute it
wherever a brief still names `wmic`.

Five cases were run on the box to establish this, same session, same binary:
stock `PATH` and the VS `VCPackages` entry alone both fall back to FXC and create
DX12; only the full `vcvars64` environment kills it, with
`source: Some(Device(Unexpected))` naming the too-old DLL. The figure never moved
across any of them, so there is no second reading to reconcile.

**Backend preference is Vulkan here regardless.** `request_adapter` returned
Vulkan / RTX 5090 in all five cases, including the two where DX12 was dead. So a
working DX12 is not what this box chooses; the real exposure of the residual hole
is a Windows machine with **no Vulkan ICD**, which falls to GL — where
`capacity/mod.rs` maps `Backend::Gl => None` and the app runs on a presumed
capacity instead of a measured one.

The residual hole itself is narrow and still open: `Auto` dies on a
`dxcompiler.dll` that loads *and* is too old, reachable via the app directory,
System32, cwd or `PATH`. No stock consumer machine has one; some third-party
installers do.

**A DX12 arm reports max 3D texture 2048 against Vulkan's 16384** (2D 16384 vs
32768). The volume grid fits under both today, so it does not bite — but it would
on a larger grid, and the difference is invisible unless you look for it.

Note the shape of this failure, because it is the dangerous kind: the workaround
that followed produced a *correct-looking number*, which made the false premise
look **confirmed** rather than exposed.

## What is already there — do not install it

Visual Studio 2019 Community 16.11.53 with MSVC 14.29.30133 and Windows SDK
10.0.19041.0. **No Visual Studio install is ever needed**; sourcing
`vcvars64.bat` is sufficient. `git` is present.

## What must be installed

`rustup` only, via the official installer, with its homes redirected into a
directory you create:

```
win.rustup.rs/x86_64  ->  rustup-init.exe -y --no-modify-path --profile minimal
RUSTUP_HOME / CARGO_HOME  ->  your own directory
```

It pulls a current stable — **1.98.1** as of 2026-09-04, *not* the repo's pin —
and then materialises the pinned **1.97.1** MSVC toolchain beside it. Two
toolchains land; the pinned one is what builds.

`cargo build --release -p squallar` takes ~4m50s, and **that figure is the whole
app**. Most work here does not need it: a capacity probe pulls a 15-crate graph
and builds in **5.31 s**. Budget the app build only when you actually need the
app.

## Cleanup is a requirement, not a courtesy

Remove only what you created, then **verify**: no `%USERPROFILE%\.cargo`, no
`.rustup`, the user `PATH` byte-identical (capture a SHA-256 before and after),
and no scheduled tasks left behind. Say what you removed.

## Reading the results

Windows figures are their **own arm** — never merged with Linux headed, Mac, or
headless Tier-2.

**The lie-guard is load-bearing on this box.** The "Microsoft Basic Render Driver"
CPU adapter reports a 33,348,044,800 B local budget — machine RAM wearing a GPU's
name. `trust_local_heaps` correctly rejects it. If you add an adapter path here,
check it against that adapter before believing any figure it hands you.
