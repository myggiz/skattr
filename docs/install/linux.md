# Installing Skattr on Linux

Skattr ships three Linux variants in the same release:

| Format | When to use | Install |
|--------|-------------|---------|
| `.deb`     | Debian, Ubuntu, Mint, Pop!_OS, anything apt-based. Integrates with apt; future apt-style updates are simplest. | `sudo apt install ./skattr_<version>_amd64.deb` |
| AppImage   | Single-file portable; works on any glibc-based distro. No installation; just `chmod +x` and run. | `chmod +x Skattr_<version>_amd64.AppImage && ./Skattr_<version>_amd64.AppImage` |
| Flatpak (build-from-source) | Strongest sandboxing; best for hostile-network environments. Requires `flatpak-builder` and a working network. | See "Flatpak" below. |

Verify the bundle first using the steps in
[`docs/install/README.md`](README.md). The instructions below
assume you've already done so.

## Required runtime libraries

Tauri 2 + WebKitGTK 4.1 require:

- `libwebkit2gtk-4.1-0` (≥ 2.40)
- `libayatana-appindicator3-1`

The `.deb` declares these as dependencies; on AppImage you may need
to install them manually if the host distro is older. On Fedora 39+
the equivalents are `webkit2gtk4.1` and `libayatana-appindicator-gtk3`.

## Wayland tray caveat

Skattr's tray icon uses the StatusNotifier / Ayatana Indicator
protocol. On bare Wayland desktops *without* a StatusNotifier host,
the tray icon will not be displayed; close-to-tray falls back to
"quit on close" (logged at WARN).

Common StatusNotifier hosts:

- **GNOME** — works out-of-the-box (extension required on some
  GNOME versions).
- **KDE Plasma** — works out-of-the-box.
- **Sway** / **Hyprland** — install `waybar` or another
  StatusNotifier-aware bar; the tray icon appears once the bar is
  running.
- **plain Sway / no bar** — close-to-tray falls back to quit.
  This is documented behaviour, not a bug.

## `.deb` install

```bash
sudo apt install ./skattr_<version>_amd64.deb
# or, on a system without apt-add-repository network access:
sudo dpkg -i skattr_<version>_amd64.deb
sudo apt-get install -f       # pull missing deps
```

Launcher: `Skattr` appears in the Applications menu under
"Internet" (`Categories=Network;InstantMessaging;`).

CLI: `skattr-ui` is at `/usr/bin/skattr-ui`. The CLI tool
`skattr` is **not** included in the `.deb`; it ships in a
separate `.deb` planned for a later release.

## AppImage install

```bash
chmod +x Skattr_<version>_amd64.AppImage
./Skattr_<version>_amd64.AppImage
```

Optional desktop integration:

```bash
mkdir -p ~/Applications
mv Skattr_<version>_amd64.AppImage ~/Applications/
~/Applications/Skattr_<version>_amd64.AppImage --appimage-integrate
# or use `appimaged` if installed
```

## Flatpak (build-from-source)

Flathub publication is on the roadmap; for v0.1 you build from
the in-repo manifest:

```bash
git clone https://github.com/myggiz/skattr.git
cd skattr
flatpak install --user flathub org.freedesktop.Platform//23.08 \
                                org.freedesktop.Sdk//23.08 \
                                org.freedesktop.Sdk.Extension.rust-stable//23.08
flatpak-builder --user --install --force-clean build \
    packaging/flatpak/net.myggiz.skattr.yml
flatpak run net.myggiz.skattr
```

Build time: ~10–20 minutes on first run (downloads Rust + Node deps
inside the sandbox).

## `skattr://` URL handler

The `.deb`, AppImage (with `--appimage-integrate`), and Flatpak all
register `skattr://` as a URL scheme handler. Clicking a
`skattr://invite/v1#…` link in your browser launches Skattr (or
focuses an existing window) and opens the Add-Contact dialog with
the URL pre-filled.

To test:

```bash
xdg-open 'skattr://invite/v1#id=AAAA'
```

If this opens Skattr, the handler is live. If it opens a different
app or shows a "no handler" dialog, run:

```bash
xdg-mime default Skattr.desktop x-scheme-handler/skattr
```

## Logs

By default, Skattr keeps an in-memory ring buffer of recent log
records (visible in Settings → Advanced → Logs).
Enable on-disk log persistence in Settings → Advanced; logs are
written to `~/.local/share/skattr/logs/skattr.log` after a daemon
restart.
