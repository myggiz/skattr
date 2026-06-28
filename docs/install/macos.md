# Installing Skattr on macOS

Skattr v0.1 is unsigned and unnotarised. macOS Gatekeeper will
warn you on first launch; the workaround is documented below.
Code signing + notarisation are tracked for Phase 5.

## Supported macOS versions

- macOS 12 (Monterey) and newer.
- Apple Silicon only for v0.1. Intel-Mac (`x86_64`) bundles are
  tracked for a follow-up release.

## Verify first

See [`docs/install/README.md`](README.md) for the
SHA256 + minisign verification steps. The rest of this guide
assumes both checks passed.

## Install

1. Double-click `Skattr_<version>_arm64.dmg` to mount it.
2. Drag `Skattr.app` into `/Applications`.
3. Eject the DMG.

## First-launch Gatekeeper warning

The first time you launch `Skattr.app`, macOS shows:

> "Skattr.app" can't be opened because it is from an
> unidentified developer.

This is expected on an unsigned bundle. To bypass:

### Option A — right-click → Open

1. **Right-click** (or Control-click) `Skattr.app` in
   `/Applications`.
2. Choose **Open** from the context menu.
3. macOS shows a slightly different dialog with an **Open**
   button. Click it.
4. macOS remembers your decision; subsequent launches don't
   re-prompt.

### Option B — Terminal (power user)

Strip the quarantine flag:

```bash
xattr -d com.apple.quarantine /Applications/Skattr.app
open /Applications/Skattr.app
```

### Option C — System Settings (macOS 13+)

If the right-click trick fails on an MDM-managed Mac:

1. Try to launch Skattr; the warning dialog appears.
2. Open **System Settings** → **Privacy & Security**.
3. Scroll to the bottom; click **Open Anyway** next to the
   Skattr line.
4. Re-launch.

## What macOS sees

Without notarisation, Skattr is a "downloaded by Safari" app
without a Developer ID. We're aware this is a friction point
and Phase 5 will close it.

For now, the *signature* you should trust is the
minisign signature on `SHA256SUMS` — the same supply-chain
guarantee Linux users get. The macOS Gatekeeper warning is
about Apple's signing chain, which is orthogonal.

## `skattr://` URL handler

The `.dmg` registers `skattr://` as a URL scheme handler. Click
`skattr://invite/v1#…` in any macOS app and Skattr launches
(or focuses) with the Add-Contact dialog open.

To test from Terminal:

```bash
open 'skattr://invite/v1#id=AAAA'
```

## Logs

By default, Skattr keeps an in-memory ring buffer of recent log
records (visible in Settings → Advanced → Logs).
Enable on-disk log persistence in Settings → Advanced; logs are
written to `~/Library/Application Support/skattr/logs/skattr.log`
after a daemon restart.
