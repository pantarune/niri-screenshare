# xdg-desktop-portal-niri

portal backend for niri, implements ScreenCast.

## why

niri's wiki says to install `xdg-desktop-portal-gnome` for screencasting. it works, but drags in half of GNOME for nothing. this does the same thing without the bloat.

both call `org.gnome.Mutter.ScreenCast.CreateSession` on niri either way. this one just skips the GNOME middleware.

## build

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
org.freedesktop.impl.portal.Secret=gnome-keyring
```

## depends

- niri
- pipewire
- xdg-desktop-portal-gtk (file picker)
- gnome-keyring (secrets)

## how

app calls portal → portal calls our backend → we call niri's Mutter.ScreenCast → niri creates the pipewire node. zero frame copying, niri handles the gpu capture.
