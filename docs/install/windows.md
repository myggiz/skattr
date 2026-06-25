# Installing Skattr on Windows

> Windows 11 (x64) is the supported runtime target. Windows 10 may
> work but is not CI-tested.

## 1. Download

Grab the latest release artifacts from
<https://github.com/myggiz/skattr/releases>:

- `Skattr_<version>_x64_en-US.msi`
- `SHA256SUMS`
- `SHA256SUMS.minisig`
- `minisign.pub` (the maintainer's public verification key — pinned
  in the repo at `docs/install/minisign.pub`)

## 2. Verify

Two complementary checks. Either is sufficient; together they're
defence-in-depth.

**SHA256:**

```powershell
Get-FileHash -Algorithm SHA256 .\Skattr_*.msi
```

The output line's hash must match the corresponding line in
`SHA256SUMS`.

**Minisign signature** (recommended):

Download `minisign-win32` from
<https://jedisct1.github.io/minisign/> and put `minisign.exe`
on PATH. Then:

```powershell
minisign.exe -V -m SHA256SUMS -p minisign.pub
```

Expected: `Signature and comment signature verified` (or similar). (Note: the published minisign key is a placeholder until v0.1.0; verification is not active yet.)

## 3. Install

Double-click the `.msi`. Microsoft Defender SmartScreen will gate
the unsigned installer with "Windows protected your PC":

1. Click **More info**.
2. Click **Run anyway**.
3. Walk the WiX installer prompts. Default install path is
   `C:\Program Files\Skattr\`.

The `.msi` registers the `skattr://` URL handler and adds a Start
menu entry. It does **not** add `skattr-ui.exe` to `PATH`.

> Code signing (Authenticode) is planned for v0.2 — Phase 5 in the
> roadmap. SmartScreen will reappear on every download until the
> bundle accumulates "reputation" on Microsoft's reputation system,
> which only signed bundles do.

## 4. First-run

Launch Skattr from the Start menu. The first-run wizard walks four
steps:

1. Welcome.
2. Set a passphrase (zxcvbn strength ≥ 3 required).
3. Type back your 24-word BIP39 seed.
4. Wait for Tor bootstrap (up to 240 s on first run; subsequent
   launches are faster as the consensus is cached).

Once Tor reaches Ready, you're at the contact list — empty until
you add your first contact via an invite link.

You can verify the install end-to-end without going through the
GUI:

```powershell
& "C:\Program Files\Skattr\skattr-ui.exe" --smoke-test `
  --data-dir "$env:TEMP\skattr-smoke" --timeout-secs 240
```

Expected exit code: 0. The smoke test creates a throwaway identity
in `%TEMP%\skattr-smoke`, boots the daemon, waits for Tor Ready,
then exits.

## 5. `skattr://` URL handler

Paste this URL into the Edge address bar to test:
`skattr://invite/v1#test`. Edge will prompt "Open Skattr?" — click
**Open**. The app should focus and open the "Add contact" dialog
(it will reject the test invite as malformed; that's expected).

If the prompt never appears, the URL handler registration didn't
take. Re-running the `.msi` install (Repair) typically fixes it.

## 6. Uninstall

Settings → Apps → Skattr → Uninstall.

User data under `%APPDATA%\myggiz\skattr` is **not** removed by
uninstall by design — re-installing preserves your identity. To
fully wipe, delete `%APPDATA%\myggiz\skattr` manually after
uninstall.

## Troubleshooting

- **SmartScreen reappears every download.** Expected for unsigned
  bundles; Phase 5 Authenticode signing will silence it.
- **Daemon won't start; UI hangs at "starting".** Check
  `%APPDATA%\myggiz\skattr\ipc.endpoint` exists. If yes, delete it
  and relaunch — a stale entry from a prior crash can't be reused
  cross-process.
- **"This app can't run on your PC".** You downloaded the x64
  `.msi` on an ARM64 Windows machine. ARM64 Windows isn't supported
  for v0.1.
- **First-run hangs at Tor bootstrap.** Some networks throttle Tor
  guards. Increase `--timeout-secs` or use a `tor` bridge (UI
  setting under "Network").
