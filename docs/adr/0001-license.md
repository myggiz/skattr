# ADR 0001: License choice

- **Status:** Accepted
- **Date:** 2026-04-16

## Context

Skattr is a privacy tool. Users need assurance that the client on their
device matches the published source, and that forks of the mailbox
server do not silently grow proprietary extensions that weaken the
privacy guarantees for their users. Licensing is one of the few tools
that binds these assurances over time.

Two concerns pull in different directions:

- **Client side:** we want the code to be inspectable and modifiable,
  and we want derivative clients to publish their source under the
  same terms. But we do **not** want to discourage users from
  integrating Skattr libraries into their own apps beyond what the
  GPL already requires.
- **Server side:** the mailbox is network-delivered infrastructure. A
  variant that runs on someone else's hardware is still "shipping" to
  users in every way that matters; those users still deserve source.

## Decision

- **`core`, `cli`, `tests` (and any future client crates):**
  **GPL-3.0-or-later.** Standard copyleft for desktop/CLI client code.
- **`mailbox`:** **AGPL-3.0-or-later.** The AGPL's §13 network-use
  clause closes the hosted-service loophole: any mailbox operator who
  modifies the server must make those modifications available to the
  users they serve.

Every `.rs` file in the workspace carries a SPDX header identifying
which license applies.

## Consequences

- **Good:** derivative clients must stay open. Derivative mailbox
  servers must stay open, including ones run as hosted services.
  Consistent with the project's privacy-tool framing.
- **Bad:** some downstream users (commercial apps that want to
  integrate Skattr as a library) cannot do so without also going GPL.
  We accept this — privacy tools that can be forked proprietary quickly
  stop being privacy tools.
- **Bad:** AGPL is poorly understood and sometimes blanket-banned by
  corporate legal. We accept this: the mailbox is ours to ship, and
  the community is where we want it run.

## Alternatives considered

- **MIT / Apache-2.0 everywhere:** rejected. Allows closed-source
  forks that could erode privacy guarantees over time.
- **MPL-2.0:** rejected. File-level copyleft isn't strong enough for a
  security-sensitive client where the whole assembly matters.
- **GPL-3.0 for everything:** rejected for the mailbox — GPL does not
  reach network-delivered services, which is exactly how the mailbox
  ships.
