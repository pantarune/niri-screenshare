PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/lib
DBUSDIR ?= $(PREFIX)/share/dbus-1/services
PORTALDIR ?= $(PREFIX)/share/xdg-desktop-portal/portals

all: build

build:
	cargo build --release

install: build
	install -Dm755 target/release/xdg-desktop-portal-niri $(DESTDIR)$(BINDIR)/xdg-desktop-portal-niri
	install -Dm644 data/niri.portal $(DESTDIR)$(PORTALDIR)/niri.portal
	install -Dm644 data/org.freedesktop.impl.portal.desktop.niri.service $(DESTDIR)$(DBUSDIR)/org.freedesktop.impl.portal.desktop.niri.service
	install -Dm644 data/xdg-desktop-portal-niri.service $(DESTDIR)$(PREFIX)/lib/systemd/user/xdg-desktop-portal-niri.service

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/xdg-desktop-portal-niri
	rm -f $(DESTDIR)$(PORTALDIR)/niri.portal
	rm -f $(DESTDIR)$(DBUSDIR)/org.freedesktop.impl.portal.desktop.niri.service
	rm -f $(DESTDIR)$(PREFIX)/lib/systemd/user/xdg-desktop-portal-niri.service

.PHONY: all build install uninstall
