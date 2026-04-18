# ADR 0005: Embed Arti vs. shell out to system tor

- **Status:** Accepted
- **Date:** 2026-04-17

## Context

Phase 0.C wires Skattr's transport layer to the Tor network. The two
realistic options were:

1. **Embed `arti-client` + `tor-hsservice`** — Arti is the Rust Tor
   implementation, actively developed by the Tor Project, already
   pinned in the workspace manifest.
2. **Shell out to system `tor`** via its controller socket — the
   mature C implementation has a wider deployment footprint and
   better-understood operational posture.

## Decision

**Embed Arti.** Concretely: `arti-client` 0.41.x with onion-service
server features on; `tor-hsservice` 0.41.x for the HS side.

The seed-derived HS-key injection path (Phase 0.C Task 5) required
enabling Arti's `experimental-api` feature to access
`launch_onion_service_with_hsid`. Plain `launch_onion_service` (the
re-use path) is not gated on `experimental-api`, so if we ever
drop the experimental feature, the Arti-managed-key path remains.

## Consequences

- **Good:** single Rust binary, no external runtime dep, reproducible
  builds, easier CI (mostly).
- **Good:** Arti's async API is a natural fit for our Tokio-based
  daemon.
- **Good:** seed-derived HS keys are possible (Phase 0.C Task 5)
  via the `experimental-api` surface — `skattr restore <seed>`
  reproduces the same `.onion` address as long as the `state_dir`
  keystore is either absent or pre-seeded with our key.
- **Bad:** Arti's onion-service surface is the youngest part of its
  public API. Upgrades may break us. We pin to specific 0.41.x
  minor versions and re-qualify at every phase exit.
- **Bad:** `experimental-api` exposes a broad unstable surface; we
  use only `launch_onion_service_with_hsid` from it but any future
  `arti_client::` use inside the crate should be grepped for on
  every Arti bump (tracked as a Phase 1 follow-up).
- **Bad:** Arti bootstrap is slower than system tor on first run
  (fresh consensus download, 30-90 s). Subsequent bootstraps
  against the same state_dir are fast.

## Fallback

If Arti blocks us in a future phase (e.g., performance regression,
upstream API removal, or a hard-to-fix onion-service bug), the
fallback is to shell out to system `tor` with a controller socket.
We have **not** architected around this fallback — `TorRuntime` is a
deliberate abstraction layer that could be reimplemented on top of a
controller socket without touching downstream code, but the current
plan is to make Arti work.

## Alternatives considered

- **`libtor` / Tor.framework:** rejected. The Rust bindings to system
  tor are less maintained than Arti and tie us to a C runtime we'd
  otherwise avoid.
- **Pluggable transports layer only:** rejected. We need to publish
  onion services, not just dial them.
