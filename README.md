# xdg-desktop-portal-niri

[Portal](https://github.com/flatpak/xdg-desktop-portal) backend for the [niri](https://github.com/niri-wm/niri) compositor implementing `org.freedesktop.impl.portal.ScreenCast`.

## Why not xdg-desktop-portal-gnome?

niri implements the `org.gnome.Mutter.ScreenCast` D-Bus interface, which is the same API `xdg-desktop-portal-gnome` calls to start a screencast. So the GNOME portal *can* work — but it pulls in unnecessary dependencies and complexity:

| | xdg-desktop-portal-gnome | xdg-desktop-portal-niri |
|---|---|---|
| **Dependencies** | GNOME Shell runtime, libadwaita, tracker, nautilus, evolution-data-server | None (uses niri directly via D-Bus) |
| **Binary size** | ~640 KB + GNOME runtime | ~5.5 MB (static Rust binary) |
| **Source lines** | ~17,000+ (C + GTK UI) | ~2,500 (Rust) |
| **Dialog** | libadwaita-based (needs GNOME theme/toolkit) | zenity (simple GTK list) |
| **Compositor coupling** | Indirect via Mutter D-Bus API | Same API, just direct |
| **File chooser** | Pulls in nautilus | Uses xdg-desktop-portal-gtk instead |

Both ultimately call the same `org.gnome.Mutter.ScreenCast.CreateSession` on niri. This backend just does it directly without the GNOME middleware.

## Requirements

- niri (running as a session, e.g. via `niri-session`)
- PipeWire
- xdg-desktop-portal-gtk (for file chooser)
- gnome-keyring (for secrets)

## Installation

```bash
cargo build --release
sudo cp target/release/xdg-desktop-portal-niri /usr/lib/
sudo cp data/niri.portal /usr/share/xdg-desktop-portal/portals/
sudo cp data/org.freedesktop.impl.portal.desktop.niri.service /usr/share/dbus-1/services/
cp data/xdg-desktop-portal-niri.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now xdg-desktop-portal-niri.service
```

## Configuration

Place in `~/.config/xdg-desktop-portal/portals.conf`:

```ini
[preferred]
default=gtk
org.freedesktop.impl.portal.ScreenCast=niri
org.freedesktop.impl.portal.Secret=gnome-keyring
```

This routes:
- **Screencasting** → xdg-desktop-portal-niri
- **File chooser** → xdg-desktop-portal-gtk (no nautilus)
- **Secrets** → gnome-keyring

## How it works

This backend delegates to niri's built-in `org.gnome.Mutter.ScreenCast` D-Bus API — the same API used by `xdg-desktop-portal-gnome`. niri handles the GPU screen capture internally and creates a PipeWire node directly. No frame copying, no wlr-screencopy, no shm buffers.

```
App (OBS/Discord/Firefox)
  → xdg-desktop-portal
    → xdg-desktop-portal-niri
      → org.gnome.Mutter.ScreenCast (niri)
        → PipeWire node → App
```

## Source

https://github.com/pantarune/xdg-desktop-portal-niri
