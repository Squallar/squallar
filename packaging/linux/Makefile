# Desktop integration for the Linux build.
#
# This is a menu entry, an icon and an application id -- and it used to be a
# permission too. GeoClue2 refused `Client.Start()` without a `DesktopId` a
# registered agent could resolve, so the app could not ask for a location until
# this file was installed. That is over: rustdar asks
# `org.freedesktop.portal.Location` instead, and the portal grants an
# unsandboxed caller with no app id at all. Location now works with none of
# this installed. See README.md.
#
#   make install-user                            # ~/.local/share, no root
#   sudo make install                            # /usr/local/share
#   make install PREFIX=/usr DESTDIR=$$PWD/pkg   # staged, for a distro package
#
# `install` deliberately does **not** install the binary. This directory is
# desktop metadata; `install-bin` is separate and opt-in, so that `make install`
# works on a machine that has not built anything -- which is the usual reason to
# want it in the first place.

PREFIX  ?= /usr/local
DESTDIR ?=
DATADIR ?= $(DESTDIR)$(PREFIX)/share
BINDIR  ?= $(DESTDIR)$(PREFIX)/bin

APPID   := dev.mcswain.rustdar
DESKTOP := $(APPID).desktop

# The name winit reports as the X11 `WM_CLASS`. winit sets none of its own: the
# X11 backend falls back to `basename(argv[0])`, and the Wayland backend sets no
# `app_id` at all unless one is asked for. `StartupWMClass` in the desktop entry
# must equal that basename or the window will not group under this launcher, so
# renaming the installed binary means editing the entry too.
BIN     ?= rustdar-platform
BINSRC  ?= ../../target/release/$(BIN)

# The repo's only raster art, and the reason there is no `icons/` directory
# here: the web build's PWA icons are already the right picture at three sizes
# hicolor defines, and a second copy would be a second thing to keep in step.
ICONSRC := ../../rustdar-web/icons
ICONS   := 32:favicon-32.png 192:icon-192.png 512:icon-512.png

INSTALL ?= install

.PHONY: all install install-user install-bin uninstall uninstall-user validate

all:
	@echo 'Nothing to build. See `make install-user` or `make install`.'

install:
	$(INSTALL) -Dm644 $(DESKTOP) $(DATADIR)/applications/$(DESKTOP)
	set -e; for spec in $(ICONS); do \
	    size=$${spec%%:*}; file=$${spec#*:}; \
	    $(INSTALL) -Dm644 $(ICONSRC)/$$file \
	        $(DATADIR)/icons/hicolor/$${size}x$${size}/apps/$(APPID).png; \
	done
	$(MAKE) --no-print-directory refresh-caches DATADIR="$(DATADIR)" DESTDIR="$(DESTDIR)"
	@echo "installed $(DESKTOP) under $(DATADIR)"

# The same thing into the per-user data directory -- what `$XDG_DATA_HOME`
# names, and the first place a desktop-file lookup searches. No root, and it
# takes effect for the current session immediately.
install-user:
	$(MAKE) --no-print-directory install \
	    DATADIR="$${XDG_DATA_HOME:-$$HOME/.local/share}" \
	    BINDIR="$$HOME/.local/bin" DESTDIR=

# Opt-in and separate: the desktop entry is useful without it, and the release
# binary is usually not built.
install-bin:
	$(INSTALL) -Dm755 $(BINSRC) $(BINDIR)/$(BIN)
	@echo "installed $(BIN) to $(BINDIR)"

uninstall:
	rm -f $(DATADIR)/applications/$(DESKTOP)
	set -e; for spec in $(ICONS); do \
	    size=$${spec%%:*}; \
	    rm -f $(DATADIR)/icons/hicolor/$${size}x$${size}/apps/$(APPID).png; \
	done
	rm -f $(BINDIR)/$(BIN)
	$(MAKE) --no-print-directory refresh-caches DATADIR="$(DATADIR)" DESTDIR="$(DESTDIR)"
	@echo "removed $(DESKTOP) from $(DATADIR)"

uninstall-user:
	$(MAKE) --no-print-directory uninstall \
	    DATADIR="$${XDG_DATA_HOME:-$$HOME/.local/share}" \
	    BINDIR="$$HOME/.local/bin" DESTDIR=

# Best-effort and never fatal: the caches are a lookup optimisation, the tools
# belong to packages that may not be installed, and a staged build (DESTDIR set)
# must not touch the live system's caches at all.
.PHONY: refresh-caches
refresh-caches:
	@test -n "$(DESTDIR)" && exit 0; \
	 if command -v update-desktop-database >/dev/null 2>&1; then \
	     update-desktop-database -q "$(DATADIR)/applications" || true; \
	 fi; \
	 if command -v gtk-update-icon-cache >/dev/null 2>&1 && \
	    test -f "$(DATADIR)/icons/hicolor/index.theme"; then \
	     gtk-update-icon-cache -qtf "$(DATADIR)/icons/hicolor" || true; \
	 fi

# `desktop-file-validate` ships with desktop-file-utils, which is not assumed.
validate:
	@if command -v desktop-file-validate >/dev/null 2>&1; then \
	    desktop-file-validate $(DESKTOP) && echo "$(DESKTOP) is valid"; \
	else \
	    echo 'desktop-file-validate not installed; skipping'; \
	fi
