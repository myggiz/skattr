<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->

# Running the Playwright e2e suite locally

```bash
cd crates/ui/src-svelte
docker compose -f docker-compose.e2e.yml run --rm e2e
```

That runs all of `tests/e2e` in the same Ubuntu image CI uses. First run pulls
a ~1.5 GB image and installs dependencies into a named volume; later runs reuse
both and finish in seconds.

To pass Playwright arguments through:

```bash
docker compose -f docker-compose.e2e.yml run --rm e2e --grep composer
```

## Why Docker rather than `pnpm test:e2e`

`pnpm test:e2e` works on Ubuntu — that is what CI does — but **not on every
host**, and the failure is not obvious.

On Arch (and other unsupported distributions) `playwright install chromium`
prints:

```
BEWARE: your OS is not officially supported by Playwright;
        downloading fallback build for ubuntu20.04-x64.
```

It then downloads a ~165 MiB Ubuntu 20.04 browser build. Observed on Arch: the
download completes and extraction silently produces no browser binary, leaving
`~/.cache/ms-playwright` with a directory and no executable. Even when it does
extract, the Ubuntu build may not launch without matching system libraries,
which `playwright install --with-deps` supplies via `apt` — no help on Arch.

A browser already installed on the host is not a substitute if it is packaged
as a **Flatpak**: Playwright launches browsers directly with CLI flags and
needs unsandboxed filesystem access, which a Flatpak wrapper does not give it.

So on such a host the choice is Docker or "push and let CI tell you". This file
exists so the next person does not download 165 MiB twice to discover that.

## How the compose file avoids touching your tree

The source is mounted **read-only** at `/src` and copied into the container
before building. The image runs as root, so a read-write bind mount would leave
root-owned `.svelte-kit/`, `build/` and `node_modules/` in the working tree —
which then breaks a subsequent host-side `pnpm build`. Dependencies and the
pnpm store live in named volumes, so they never collide with the host's
`node_modules` (installed against a different libc).

Verified: after a run, `git status` shows nothing new and no file in
`crates/ui/src-svelte` is root-owned.

## Keeping the image in step

The image tag **must** match `@playwright/test` in `package.json` — the image
ships the browser build that version expects. Bump both together:

| package.json            | compose image                                |
|-------------------------|----------------------------------------------|
| `"@playwright/test": "1.47.0"` | `mcr.microsoft.com/playwright:v1.47.0-jammy` |

## Notes

- pnpm is installed with `npm install -g pnpm@10` rather than corepack: the
  corepack bundled with this image is too old to verify current pnpm release
  signatures and fails with `Cannot find matching keyid`. Installing from the
  registry avoids that without disabling signature verification.
- The suite runs against the mock backend (`TAURI_MOCK=1`); no daemon, no Tor,
  no network peer is involved.
- CI remains the authority. This is for catching e2e breakage before pushing,
  not a replacement for the `ui` job.
