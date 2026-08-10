# Linux desktop integration

Two files and a Makefile. They exist so that **"Use my location" can work**, not
so that rustdar appears in a menu — the menu entry is a side effect.

## Why a `.desktop` file is required

rustdar's Linux location provider talks to [GeoClue2] on the session bus.
GeoClue refuses `org.freedesktop.GeoClue2.Client.Start()` unless the client has
first set the `DesktopId` property, and where a **location agent** is registered
— GNOME's `gnome-shell` registers one, and geoclue ships a demo agent — that
agent resolves the id to `<DesktopId>.desktop` through `GDesktopAppInfo` so it
can put a name and an icon in front of the user before answering.

So with no file installed, on a desktop with an agent, the call fails with a
bare `org.freedesktop.DBus.Error.AccessDenied` and nothing anywhere says why.
The app checks for the file before it makes the call and logs this instead:

```
dev.mcswain.rustdar.desktop is not installed under XDG_DATA_HOME or
XDG_DATA_DIRS. ... Install it with `make -C packaging/linux install-user`.
```

The app does **not** install the file itself. Writing into a user's
`applications/` directory to obtain a permission is exactly the kind of thing an
app should be asked to do rather than do quietly, and on installs with no agent
registered the file is genuinely unnecessary — that is the configuration this
provider was first measured working in.

[GeoClue2]: https://gitlab.freedesktop.org/geoclue/geoclue

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
| file name | `dev.mcswain.rustdar.desktop` | The basename **is** the `DesktopId`. It matches `ios/project.yml`'s bundle id and the Android `applicationId`, and `os_location/linux.rs`'s `DESKTOP_ID` — a test pins those two together. |
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

GeoClue is the thing that finds the position; rustdar only asks it to. **How**
it finds one is geoclue's own configuration, and one of its methods sends data
off the machine:

* **IP-based lookup** — geoclue asks a geolocation service about the public IP
  address the request arrives from. This is what produced the measurement this
  provider was built against: **25 km accuracy**, `AvailableAccuracyLevel = 6`
  (`STREET`), on a machine with no Wi-Fi.
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

Then run the app with `RUST_LOG=rustdar_platform_lib=debug` and look for
`GeoClue2 location session started as dev.mcswain.rustdar`. If the preflight
warning appears instead, the file is not where the agent searches.
