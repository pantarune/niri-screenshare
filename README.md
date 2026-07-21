# niri-screenshare

portal backend for niri, implements ScreenCast.

## vs xdg-desktop-portal-gnome

| | gnome | niri-screenshare |
|---|---|---|
| depends | gnome-shell, libadwaita, tracker, nautilus, evolution-data-server, gvfs | nothing |
| language | 17k+ lines C | 450 lines Rust |
| binary | 640KB + gnome runtime | 1.5MB static |
| selector | libadwaita dialog | none (auto-selects) |

both call `org.gnome.Mutter.ScreenCast` on niri either way.

## install

```
paru -S niri-screenshare
```

or build manually:

```
cargo build --release
sudo cp target/release/xdg-desktop-portal-niri /usr/lib/
sudo cp data/niri.portal /usr/share/xdg-desktop-portal/portals/
sudo cp data/org.freedesktop.impl.portal.desktop.niri.service /usr/share/dbus-1/services/
cp data/xdg-desktop-portal-niri.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now xdg-desktop-portal-niri.service
```

## config

```
[preferred]
default=gtk
org.freedesktop.impl.portal.ScreenCast=niri
```

## depends

- niri, pipewire

## how

app → portal → our backend → niri's Mutter.ScreenCast → PipeWire node. zero frame copying, niri handles the gpu capture.
