# Linux desktop integration

Two files and a Makefile: a `.desktop` entry and three icon sizes. They give
rustdar a launcher entry, an icon, a window that groups under that entry, and
the identity a Flatpak would need.

## What this is *not* for any more

An earlier version of this file said the `.desktop` entry was **required for
location**. It is not, and the reason is worth writing down because it is the
same reason the provider was rewritten.

rustdar used to talk to [GeoClue2] directly. GeoClue refuses
`org.freedesktop.GeoClue2.Client.Start()` unless the client has set a
`DesktopId`, and where a location agent is registered that agent resolves the id
to `<DesktopId>.desktop` to find a name and an icon to show the user — so
without the file installed, the call failed with a bare `AccessDenied`.

rustdar now uses **`org.freedesktop.portal.Location`** and nothing else. The
portal identifies an unsandboxed caller with `xdp_app_info_is_host`, which
grants `GCLUE_ACCURACY_LEVEL_EXACT` with **no prompt and no app id at all** —
the portal then talks to GeoClue under its own identity. So on a normal
non-Flatpak install, location works with none of these files installed, and
installing them changes nothing about it.

Why the change, given the direct path worked: `org.freedesktop.impl.portal.Lockdown`
carries a `disable-location` property that desktops bind to their own location
switch. Reading GeoClue directly means never seeing it — answering a question
the user has already answered, and answering it the other way. It also means
never working inside a Flatpak, where there is no system bus to reach.

[GeoClue2]: https://gitlab.freedesktop.org/geoclue/geoclue

## Read this first: location is off by default on most desktops

`xdg-desktop-portal-gtk` — which almost every desktop installs, if only for its
file chooser — implements `Lockdown` by reading the GSettings key
**`org.gnome.system.location enabled`**. That key **defaults to `false`**, and
GNOME is the only desktop with a UI for it.

So on a stock KDE, Sway or Hyprland machine, the very first "Use my location"
is refused before a session is even created:

```text
org.freedesktop.portal.Error.NotAllowed: Location services disabled
```

rustdar reports that as **Denied** — a decision, reversible, not a missing
service — and the settings pane names the fix, because "check your system
settings" would point at a page that does not exist. From a terminal:

```sh
gsettings get org.gnome.system.location enabled     # false on a stock install
gsettings set org.gnome.system.location enabled true
```

It takes effect immediately; nothing needs restarting. On GNOME the same switch
is Settings → Privacy → Location.

To confirm what the portal backend is actually reporting:

```sh
busctl --user get-property org.freedesktop.impl.portal.desktop.gtk \
    /org/freedesktop/portal/desktop \
    org.freedesktop.impl.portal.Lockdown disable-location
```

`b false` means location is permitted.

If a portals **frontend** is missing entirely — no `xdg-desktop-portal`, or no
backend implementing `Location` — rustdar reports **Unavailable** instead, and
offers nothing, because there is no switch to turn on.

## Installing

```sh
make -C packaging/linux install-user     # ~/.local/share — no root, immediate
sudo make -C packaging/linux install     # /usr/local/share
```

For a distribution package:

```sh
make -C packaging/linux install PREFIX=/usr DESTDIR="$PWD/pkg"
```

`install` lays down the desktop entry and three icon sizes. It does **not**
install the binary, so it works on a machine that has not built anything; add
that separately when you want it:

```sh
cargo build --release -p rustdar-platform --bin rustdar-platform
make -C packaging/linux install-bin      # or install-bin BIN=… BINSRC=…
```

`uninstall` / `uninstall-user` remove everything the matching install target
could have written, including the binary.

## What is in the entry, and what must not drift

| field | value | why it is that value |
|---|---|---|
| file name | `dev.mcswain.rustdar.desktop` | The basename is the application id. It matches `ios/project.yml`'s bundle id and the Android `applicationId`, and a test in `os_location/linux.rs` compiles this file in by that exact path, so renaming it fails the build. |
| `Icon=` | `dev.mcswain.rustdar` | A bare identifier, not a path. The icon theme spec resolves it against the installed `hicolor` sizes; a path would pin one size and defeat the lookup. |
| `StartupWMClass=` | `rustdar-platform` | See below. |
| `Exec=` | `rustdar-platform` | The binary this repo builds. Rename it and `StartupWMClass` has to change with it. |

**`StartupWMClass` is the fragile one.** winit sets no window class of its own:
its X11 backend falls back to `basename(argv[0])`, and its Wayland backend calls
`set_app_id` only when an application name was explicitly requested — which
rustdar does not do, so **a native Wayland window carries no `app_id` at all**.
The consequence is worth knowing before filing a bug about it:

* Under X11 or XWayland the window's `WM_CLASS` is `rustdar-platform`, which is
  what the entry claims, and grouping works.
* Under native Wayland there is nothing for a compositor to match on, so the
  window will not group under this launcher whatever the entry says.

Fixing the second properly means calling winit's
`WindowAttributesExtWayland::with_name` in `rustdar-frontend`; it is out of
scope here and is noted rather than papered over.

## Icons

There is no `icons/` directory here on purpose. The Makefile installs
`rustdar-web/icons/{favicon-32,icon-192,icon-512}.png` — the repo's only raster
art — into `hicolor/{32x32,192x192,512x512}/apps/dev.mcswain.rustdar.png`. A
copy would be a second set of files to keep in step with the first.

## No AppStream metadata

There is deliberately no `metainfo.xml`. Nothing in this repository ships to a
software centre, and an AppStream component that no store indexes is metadata
with no reader and a second identity to keep correct.

## Network egress — read this before enabling location

**This has not changed, and the portal does not change it.** The portal does not
find positions itself: it proxies to the same GeoClue daemon, so everything
GeoClue does to answer, it still does. **How** it finds a position is geoclue's
own configuration, and one of its methods sends data off the machine:

* **IP-based lookup** — geoclue asks a geolocation service about the public IP
  address the request arrives from. This is what produced the measurement this
  provider was built against: **25 km accuracy**, reported by geoclue as
  `GeoIP (ichnaea)`, on a machine with no Wi-Fi in use.
* **Wi-Fi lookup** — geoclue scans for nearby access points and **POSTs their
  BSSIDs (MAC addresses) to `https://api.beacondb.net/v1/geolocate`**. That is a
  list of your neighbours' routers, sent to a third party, in exchange for a
  better position.

rustdar neither makes nor can suppress that request — it is `geoclue.conf`'s
`[wifi] enable=` and the `url=` beside it that decide — but it happens *because
this app asked for a location*, so it is disclosed here, and the settings pane
says the same thing next to the button. If you want the position without the
scan, set `enable=false` under `[wifi]` in `/etc/geoclue/geoclue.conf` and
restart `geoclue.service`; the IP path still works.

The accuracy this provider gets is **not** GPS accuracy, and the app never
labels it as such: fixes are reported as `Device`, which is the honest name for
"the platform fused something and declined to say what".

## Checking the install

```sh
make -C packaging/linux validate                 # desktop-file-validate, if present
ls ~/.local/share/applications/dev.mcswain.rustdar.desktop
gio info ~/.local/share/applications/dev.mcswain.rustdar.desktop   # optional
```

Then run the app with `RUST_LOG=rustdar_platform_lib=debug` and press "Use my
location". A working session logs:

```text
portal location session started
OS location fix: 35.4689, -97.5195 (±25000 m) from GeoIP (ichnaea)
```

A refusal names the key instead:

```text
the location portal refused a session: … The desktop's location switch is off.
xdg-desktop-portal-gtk reads it from the GSettings key
`org.gnome.system.location enabled` …
```

Note that none of that depends on the files this directory installs.

## One failure mode that looks like a rustdar bug and is not

If the portal answers with response code 2 — rustdar logs "the portal accepted
the request and could not carry it out" and stays retryable — check the portal's
own log:

```sh
journalctl --user -u xdg-desktop-portal.service --since -5min
```

`Starting GeoClue client failed: … Geolocation disabled for UID 1000` with the
lockdown key already `true` means the portal is holding a GeoClue client whose
cached view of the geoclue **agent** went stale, which is what happens when the
agent (`geoclue-demo-agent`, or your desktop's own) restarts underneath a
long-lived portal. Restarting the portal rebuilds it:

```sh
systemctl --user restart xdg-desktop-portal.service
```

This was observed on the development machine and is a portal/geoclue
interaction; rustdar has no part in it beyond reporting the failure honestly and
letting the user try again.
