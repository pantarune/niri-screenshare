# xdg-desktop-portal-niri

[Portal](https://github.com/flatpak/xdg-desktop-portal) backend for the [niri](https://github.com/niri-wm/niri) compositor implementing `org.freedesktop.impl.portal.ScreenCast`.

## Why this is better than xdg-desktop-portal-gnome

### The Problem

The niri wiki tells you to install `xdg-desktop-portal-gnome` for screencasting. This works because niri implements the `org.gnome.Mutter.ScreenCast` D-Bus interface — the same API GNOME Shell exposes. The GNOME portal calls this API to create screencast sessions.

But `xdg-desktop-portal-gnome` was designed for a full GNOME desktop. Installing it on niri pulls in an entire GNOME user runtime whether you want it or not: libadwaita, tracker-miners, evolution-data-server, nautilus, and GNOME Shell dependencies that will never run on niri. You get the bloat without the functionality that bloat was meant for.

It's the wrong tool for the job.

### The Comparison

| | xdg-desktop-portal-gnome | xdg-desktop-portal-niri |
|---|---|---|
| **Dependencies** | GNOME Shell runtime, libadwaita, tracker, evolution-data-server, nautilus, gvfs-goa, gnome-desktop, etc. | **None** (calls niri directly over D-Bus) |
| **Source language** | 17,000+ lines of C | **2,500 lines of Rust** |
| **Dialogs** | libadwaita/GTK4 — requires full GNOME theme stack | **zenity** — a single small GTK binary, no theme dependency |
| **File chooser** | Pulls in nautilus (GNOME file manager) | **Reuses xdg-desktop-portal-gtk** — no extra deps |
| **Compositor coupling** | Indirect: App → portal → GNOME Shell's Mutter API → niri | **Direct: App → portal → niri** (one less hop) |
| **What you actually get** | Screencasting + 500 MB of GNOME infrastructure that sits unused | **Screencasting, nothing else** |

### What the numbers mean

`xdg-desktop-portal-gnome` isn't just the binary — it's a dependency chain. On a minimal niri setup:

```
$ pacman -Qi xdg-desktop-portal-gnome | grep Depends
Depends On: gcc-libs  glibc  glib2  gtk4  libadwaita  libsoup3  ...
```

Installing that pulls tracker3, evolution-data-server, gnome-desktop-4, gvfs, and so on — packages whose sole purpose is to serve GNOME Shell features that **cannot run on niri**. You're installing a compositor's portal UI for a compositor you don't use.

This backend does the same work — calls `org.gnome.Mutter.ScreenCast.CreateSession` on niri — without pretending you're running GNOME.

### Practical advantage

- **Smaller footprint**: 2,500 LOC Rust vs 17,000+ LOC C. Easier to audit, modify, and understand.
- **No stale dependencies**: Updates to GNOME packages won't randomly break your screencasting.
- **Same capture path**: Both backends ultimately call the same niri D-Bus API — the capture quality is identical. The difference is **only** what else comes along for the ride.

## Requirements

- niri (running as a session, e.g. via `niri-session`)
- PipeWire
- xdg-desktop-portal-gtk (for file chooser dialogs)
- gnome-keyring (for the Secret portal)

## Installation

```bash
# Build
cargo build --release

# Install binary and portal files
sudo cp target/release/xdg-desktop-portal-niri /usr/lib/
sudo cp data/niri.portal /usr/share/xdg-desktop-portal/portals/
sudo cp data/org.freedesktop.impl.portal.desktop.niri.service /usr/share/dbus-1/services/
mkdir -p ~/.config/systemd/user
cp data/xdg-desktop-portal-niri.service ~/.config/systemd/user/

# Enable
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
- **File chooser** → xdg-desktop-portal-gtk (no nautilus needed)
- **Secrets** → gnome-keyring

## How it works

This backend delegates to niri's built-in `org.gnome.Mutter.ScreenCast` D-Bus API — the same API used by `xdg-desktop-portal-gnome`. niri handles the GPU screen capture internally and creates a PipeWire node directly. No frame copying, no wlr-screencopy, no shm buffers.

```
App (OBS / Discord / Firefox)
  → xdg-desktop-portal           (portal daemon)
    → xdg-desktop-portal-niri     (this backend)
      → org.gnome.Mutter.ScreenCast (niri compositor)
        → PipeWire node → App reads frames directly
```

## Source

https://github.com/pantarune/xdg-desktop-portal-niri
