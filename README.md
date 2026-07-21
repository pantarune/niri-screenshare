# niri-screenshare

portal backend for niri. replaces `xdg-desktop-portal-gnome` for screen sharing.

## why

niri's wiki tells you to install `xdg-desktop-portal-gnome` for screencasting. it works but pulls in half of gnome. this does the same thing without the bloat.

both call `org.gnome.Mutter.ScreenCast` on niri either way.

## install

```
paru -S niri-screenshare
```

or manually:

```
cargo build --release
sudo cp target/release/niri-screenshare /usr/lib/
sudo cp data/niri.portal /usr/share/xdg-desktop-portal/portals/
sudo cp data/org.freedesktop.impl.portal.desktop.niri.service /usr/share/dbus-1/services/
cp data/niri-screenshare.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now niri-screenshare.service
```

## config

the portal daemon (`xdg-desktop-portal`) chooses which backend to use for each interface based on `~/.config/xdg-desktop-portal/portals.conf`:

```ini
[preferred]
default=gtk
org.freedesktop.impl.portal.ScreenCast=niri
```

without this, the portal daemon would fall back to `UseIn=gnome` matching (which loads the gnome portal). this tells it to use our backend instead.

- `default=gtk` — routes stuff like file picker to the lightweight gtk portal instead of gnome's (avoids pulling nautilus)
- `ScreenCast=niri` — routes screen sharing to this backend

## depends

- niri, pipewire

## how

app → portal daemon → our backend → niri's Mutter.ScreenCast → PipeWire. niri handles the gpu capture, we just relay the pipewire node id.
