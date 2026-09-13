---
name: iphone-safari-rig
description: Measure squallar's web memory wall on the iPhone 13 Pro (or any iOS device) -- a Safari tab driven by safaridriver on the Mac mini over USB, and the home-screen web app no driver can reach -- with both reporting over HTTPS to the Linux box. Use for M1 wall legs on iOS, for serving the TLS + COEP page a phone trusts, and for turning a no-driver leg into a row with `drive.py analyze-console`.
---

# The iPhone wall rig (M1)

## Where this stands (2026-09-13)

**Arm 1, the Safari tab driven through safaridriver: RAN.** Three calibrate runs,
then WALL1, WALL4 and WALL6 until death or 150 s, on app build b92a784f7. Rows
are in `.github/browser-rig/wall-ceilings.tsv` under device_class
`ios-iphone13pro-safari-tab`, evidence in `.github/browser-rig/wall-evidence/`.

**Arm 2, the home-screen web app: UNRUN.** It needs the user's hands every leg.

### What Arm 1 read

iPhone 13 Pro, iOS 26.6.1 (23G83), Safari 26.6.1, a Safari tab under WebDriver,
crossOriginIsolated true on every leg, served from 192.168.0.37 over TLS + COEP.

| Leg | Reading | Denominator |
|---|---|---|
| calibrate 1 | ladder 4096/2048/1024/512/256/128 MiB all construct; **death (crash) at 2304 MiB survived** | MiB of xorshift-filled linear memory touched and alive at the last `/rig/report` step (79 steps, 32 MiB each) |
| calibrate 2 | ladder all construct; **death (crash) at 2496 MiB** | same (85 steps) |
| calibrate 3 | ladder all construct; **death (crash) at 2848 MiB** | same (96 steps) |
| WALL1 | boots; no death in 150 s; page 99 / worker 125 MiB high-water, 224 on one tick; ceilings 512/256; no refusal | `budget state:` ticks (46), first page load |
| WALL4 | boots; no death; **page hit its own 512 MiB ceiling**: 3 `alloc failed … in page`, 6 traps, 1 panic, ticks stop at 43.6 s; page 510 / worker 241, 684 on one tick | 22 ticks |
| WALL6 | boots; no death; **page hit its own ceiling at 14.8 s**: 17 page refusals, 19 traps, 1 panic; page 511 / worker 189, 687 on one tick | 8 ticks |

- **Wall.** The joined wall is the LOWEST calibrate run, 2304 MiB. Residue is wall minus the one-tick page+worker pair: WALL1 2080, WALL4 1620, WALL6 1617 MiB.
- **What killed WALL4 and WALL6.** On this phone the app died at `heap.js`'s handheld ceiling (512 MiB page), far below the OS wall, and the tab survived. No leg was killed by the OS.
- **Instantiate fallback:** 0 on every leg.
- **Signals.** Device signals (every leg's page): screen 390x844, devicePixelRatio 3, hardwareConcurrency 4, maxTouchPoints 5, platform `iPhone`, `navigator.deviceMemory` ABSENT, `performance.measureUserAgentSpecificMemory` ABSENT. The user agent reports `iPhone OS 18_7` while the device runs 26.6.1: WebKit freezes the OS version in the UA, so read the OS from devicectl, never from the UA.
- **Job costs (median / max ms, off the frame).** WALL6: `decode` 510 / 3322, `overlay/model` 412 / 3730, `radar` 399 / 3431. WALL4 `decode` 2861 / 4369. The phone printed no `render took` line (no job kind is named `render`).

## The device

Read with `xcrun devicectl device info details` on the Mac (read-only):

| Field | Value |
|---|---|
| model | iPhone14,2 (iPhone 13 Pro), hardware model D63AP, arm64e |
| iOS | 26.6.1 (23G83) |
| **hardware UDID — what `safari:deviceUDID` takes** | **`00008110-001108891192801E`** |
| CoreDevice identifier — what devicectl takes | `64A825AA-AE5A-510E-88B0-E29F52F57C1D` |
| pairingState / developerModeStatus / ddiServicesAvailable | paired / enabled / true (2026-09-13 01:36Z) |

`/home/reddragon/.cache/lane-m1-wall-rig-out/iphone/device-reported.json` holds
these facts. `RIG_DEVICE_INFO_JSON` copies them into every evidence file.

## Provisioning, in the order it was done

1. **iPhone:** Settings > Safari > Advanced > Web Inspector ON and Remote Automation ON; Auto-Lock Never; USB trust with the Mac.
2. **iPhone:** trust the mkcert root. AirDrop `$(mkcert -CAROOT)/rootCA.pem` (sha256 `63:99:B0:D3:…:8C:37`), install the profile, then Settings > General > About > Certificate Trust Settings > full trust. Never run `mkcert -install` on the Linux box.
3. **Mac:** `safaridriver --enable`.
4. **CoreDevice pairing:** `xcrun devicectl manage pair --device 64A825AA-AE5A-510E-88B0-E29F52F57C1D`. The user ran it over ssh; it asked nothing on either device and printed `available (paired)`. Before it, safaridriver said `device is not paired`.
5. **Developer Mode (the user's hands, on the phone):** Settings > Privacy & Security > Developer Mode > On > Restart; after unlock tap **Turn On** and enter the passcode. devicectl then read `developerModeStatus: enabled`.
6. **Connect via Network (the user's hands, on the Mac's Safari):** in the user's words, "open Safari on the Mac desktop, Develop menu > iPhone > connect over network". After this the session started, and the WebDriver session added an `Indirect` (network) Web Inspector connection beside the `Direct` (USB) one. **Do not assume it survives a Mac or phone restart** until a leg after one shows it does.
7. **Use the hardware UDID.** Steps 5 and 6 landed together with the one fix the logs actually named. With the CoreDevice identifier, the Mac's log (process `com.apple.WebDriver.HTTPService`) read:
   ```
   FindHosts: discarding device host candidate (…), its UDID does not match the requested device UDID (64A825AA-AE5A-510E-88B0-E29F52F57C1D).
   Session could not be created from first match candidate, due to: Could not create a session: Some devices were found, but could not be used:
   ```
   It read that at 20:32, with Developer Mode still off and the candidate already ready, and again at 20:36. `RIG_SAFARI_IOS_UDID=00008110-001108891192801E` created the session. **Which of steps 5 and 6 safaridriver strictly needs is not isolated.** Both were in place for every leg that ran.

Precondition check, read-only, before any driven leg:

```bash
ssh mac 'xcrun devicectl device info details --device 64A825AA-AE5A-510E-88B0-E29F52F57C1D' \
  | grep -E 'pairingState|developerModeStatus|ddiServicesAvailable|tunnelState'
```

Required: `paired`, `enabled`, `true`.

Traps met on the way:
- `launchctl asuser $(id -u) …` over ssh fails without root: `Could not switch to audit session …: Operation not permitted`. Not needed.
- `log` is a zsh builtin in the Mac's login shell (`zsh:log:1: too many arguments`); use `/usr/bin/log show`. Its `--start` takes whole seconds only. zsh does not word-split `$VAR` in a remote loop, so pass times as separate arguments.
- The session log that says WHY a device was refused lives in process `com.apple.WebDriver.HTTPService`, not `safaridriver`. Predicate: `process == "com.apple.WebDriver.HTTPService" AND subsystem == "com.apple.Safari"`.

## Serving

LAN leaf certificate (valid to 2028-12-13):

```bash
mkdir -p ~/.cache/rd-phone-tls
mkcert -cert-file ~/.cache/rd-phone-tls/lan.pem -key-file ~/.cache/rd-phone-tls/lan-key.pem 192.168.0.37 localhost 127.0.0.1
```

The Linux box runs no firewall. The phone reached the rig from 192.168.0.195
over Wi-Fi. Build through the box lock
(`~/.cache/rd-land-check/board.sh m1rig-wasm -- env CARGO_BUILD_JOBS=4 .github/scripts/wasm-threads.sh wasm-pack build squallar-web --target web --release --no-typescript --no-pack`).
Run every script as `bash <file>`, never `source` it, and never assign zsh
special parameters (`path`, `status`, …) in tool-shell code.

## Arm 1: Safari tab, driven through safaridriver (RAN 2026-09-13)

The driver wrapper, at `/home/reddragon/.cache/lane-m1-wall-rig-out/iphone/safaridriver-via-mac`,
runs the Mac's safaridriver over ssh and forwards drive.py's port to it:

```sh
#!/bin/sh
if [ "${1:-}" = "--version" ]; then
  exec ssh -o BatchMode=yes -o ConnectTimeout=10 mac /usr/bin/safaridriver --version
fi
if [ "${1:-}" != "-p" ] || [ -z "${2:-}" ]; then echo "usage: $0 -p PORT | --version" >&2; exit 64; fi
exec ssh -tt -o BatchMode=yes -o ConnectTimeout=10 -o ExitOnForwardFailure=yes \
  -o ServerAliveInterval=15 -L "$2:127.0.0.1:$2" mac /usr/bin/safaridriver -p "$2"
```

Killing it HUPs safaridriver on the Mac; no driver was ever left running. The
driver log's `channel 3: open failed: connect failed: Connection refused` is
drive.py polling `/status` before safaridriver listens, and is harmless.

The commands that ran:

```bash
cd /home/reddragon/.cache/lane-m1-wall-rig
I=/home/reddragon/.cache/lane-m1-wall-rig-out/iphone
export RIG_SAFARIDRIVER=$I/safaridriver-via-mac RIG_SAFARI_IOS_UDID=00008110-001108891192801E \
  RIG_TLS_CERT=$HOME/.cache/rd-phone-tls/lan.pem RIG_TLS_KEY=$HOME/.cache/rd-phone-tls/lan-key.pem \
  RIG_SERVE_HOST=0.0.0.0 RIG_URL_HOST=192.168.0.37 RIG_ARM=hardware \
  RIG_DEVICE_CLASS=ios-iphone13pro-safari-tab RIG_CAL_TIMEOUT=900
for n in 1 2 3; do RIG_OUT_DIR=$I/cal$n bash .github/browser-rig/run_wall_arm.sh --skip-build --scenes "" safari; done
RIG_OUT_DIR=$I/scenes RIG_CALIBRATE_JSONS="$I/cal1/safari.calibrate.json $I/cal2/safari.calibrate.json $I/cal3/safari.calibrate.json" \
RIG_DEVICE_INFO_JSON=$I/device-reported.json \
  bash .github/browser-rig/run_wall_arm.sh --skip-build --scenes "WALL1 WALL4 WALL6" safari
```

Run each as a background job (reaped at 3600 s) and block on its marker. Each
calibrate run took ~30 s, and the three scenes took ~9 minutes.

What the legs taught:
- **A death ends the automation session.** Every calibrate death was the page's WebContent dying. The Mac logged `matching RWIDrivable was removed … remote session was terminated`, and drive.py read `invalid session id`, filed as `crash`. The calibrate leg's same-session readback cannot run after it.
- **Automation sessions start on EMPTY website data**, even on the same origin. Probed: session A wrote a localStorage key, session B on the same URL read 0 keys. So every driven run is on fresh website data (the calibrate page's `storage_at_load.local_storage_keys` read 0). It also means **the localStorage route is unreadable on this arm after a death**: every Arm 1 calibrate figure is the `/rig/report` route, and the verdict says "only the /rig/report route answered". The home-screen arm keeps its storage, and that is where the localStorage route can be read after a death.
- **The location prompt.** squallar-location's gate asks at startup on a fresh origin: `LocationMemo` defaults to `enabled: true, attempts: 0`, the permission reads `Prompt`, and `LocationGate::step` calls `request_location` → `watchPosition`. Every driven leg is a fresh origin, so iOS raised its location prompt, and nobody can answer it while automation holds the phone. **Evidence it did not change what a leg measured:** WALL1 booted 2.2 s after navigation and ticked through the window. WALL4 and WALL6 booted in 1.1 s and 0.6 s and ticked every ~2 s until their page heaps refused an allocation (the tick stop follows the first `alloc failed` by under a second). Their panes completed 1842 and 359 jobs. Each evidence file states it as a confounder under `confounders`, with its `leg_liveness`. **A rig-side seed avoids it with no product change:** localStorage `squallar.location` = `{"attempts":2,"enabled":true}` makes `may_ask()` false. It was not used for these rows; a row that uses it must say so.
- **One WALL4 attempt lost its tab.** At 01:46:31Z the automation tab was removed 1.5 s after the session connected, before any tick. drive.py read `no such window` at stage `navigated`, and the Mac logged no alert. The rerun at 01:51:41Z produced the row. The cause is not established.
- **Do not touch the phone during a driven leg.**

## Arm 2: the home-screen web app (UNRUN; the user's hands every leg)

It needs neither pairing nor Developer Mode; readings arrive over Wi-Fi. Serve
one scene at a time on the FIXED port 8443, so the installed apps keep one
origin:

```bash
cd /home/reddragon/.cache/lane-m1-wall-rig
RIG_TLS_CERT=$HOME/.cache/rd-phone-tls/lan.pem RIG_TLS_KEY=$HOME/.cache/rd-phone-tls/lan-key.pem \
RIG_SERVE_HOST=0.0.0.0 RIG_URL_HOST=192.168.0.37 RIG_SERVE_PORT=8443 \
RIG_OUT_DIR=/home/reddragon/.cache/lane-m1-wall-rig-out/iphone-pwa \
  bash .github/browser-rig/run_wall_arm.sh --serve-only WALL1
```

`--serve-only` passes `--instrument-index`, so `/` (the manifest's `start_url`)
carries the prelude, the seed and the beacon. Stop the server by its PID, never
`pkill -f`. Changing scene means restarting it with the new name on the same
port.

The user, on the phone (same Wi-Fi as 192.168.0.37):
1. Once: Safari > `https://192.168.0.37:8443/` loads with no certificate warning > Share > **Add to Home Screen**.
2. Once: Safari > `https://192.168.0.37:8443/rig/calibrate.html` > Share > **Add to Home Screen**.
3. Calibrate, three times. Before each run, delete the calibrate icon and add it again from Safari (a home-screen app's website data goes with its icon). Close other apps, launch it, tap **Start a run**, and leave it alone until the page shows the end or the app dies. If it dies, launch it once more: it reads the dead run back through localStorage. Report the end line.
4. Scenes WALL1, WALL4, WALL6, each after the server switches. Close every app and launch the squallar icon. If the location prompt appears, tap **Don't Allow**, and do the same on every leg so the arm is consistent. Leave it for 150 s or until it dies. **A hand relaunch reads as a death**, so report any. Swipe it closed.
5. Throughout: power connected, Low Power Mode off, Auto-Lock Never.

Collection:

```bash
python3 .github/browser-rig/drive.py analyze-console \
  --log <the scene's report log> --calibrate-log <the calibrate session's report log> --calibrate-run <run id> \
  --tag ios-iphone13pro-pwa.WALL1 --out /home/reddragon/.cache/lane-m1-wall-rig-out/iphone-pwa
```

This writes `<tag>.json` (the `wall` block) and `<tag>.samples.tsv`. The runner
writes evidence files for driven legs only, so a PWA leg's evidence file is
written from its `<tag>.json`. List a log's calibrate runs by reading its
`/rig/report` bodies whose `phase` is `ended` or `readback`.

## Reading and recording

- iOS rows are their own arm. Never merge them with desktop rows, or the Safari-tab context with the home-screen context.
- `wall_mib` is the LOWEST calibrate survived figure on the same device, browser and context. `residue_mib = wall_mib - sum_hw_mib`.
- A ledger row goes in only for a leg that ran, with its evidence file under `.github/browser-rig/wall-evidence/`. `squallar-web/tests/wall_ledger.rs` holds the rules.
