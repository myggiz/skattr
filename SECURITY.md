# Security policy

## Reporting a vulnerability

**Do not open a public issue for security problems.** Email `security@myggiz.net`, ideally encrypted to the project PGP key below. Expect acknowledgement within 72 hours and a status update within 7 days.

### PGP key

```
-----BEGIN PGP PUBLIC KEY BLOCK-----
(placeholder — replaced before first public release)
-----END PGP PUBLIC KEY BLOCK-----
```

Fingerprint: `TBD`

## Scope

In scope:

- The `core`, `mailbox`, `cli` crates in this repository.
- The wire protocol as described in [`docs/skattr-design.md`](docs/skattr-design.md) and [`docs/PROTOCOL.md`](docs/PROTOCOL.md).
- Signed release binaries published by Myggiz AB.

Out of scope (report to upstream):

- Vulnerabilities in Arti, OpenMLS, `snow`, `ed25519-dalek`, or other third-party dependencies. We will fast-track coordinated updates once upstream has a fix.
- Tor network-level attacks (covered by the Tor Project threat model).
- Attacks that require physical access to an unlocked device running Skattr.

## What we ask

- Give us reasonable time to patch before public disclosure. "Reasonable" is usually 90 days; we will move faster for simple fixes and negotiate for complex ones.
- Do not exfiltrate data beyond what is needed to demonstrate the vulnerability.
- Do not run disruptive tests against third-party mailbox operators.

## What you can expect

- Acknowledgement of your report within 72 hours.
- Credit in the release notes for the fix, if you want it.
- A public write-up of the issue after a fix ships, with a timeline of events.
- No bounty program yet — this is planned for Phase 5.

## Known limitations (by design, not bugs)

These are documented in the design and threat model; reports about them will be closed with a pointer to the doc:

- A mailbox operator learns which identity hashes are polling them (metadata leak — mitigated by self-hosting and operator choice).
- Message "delete for everyone" is advisory — recipient clients cooperate, but we cannot force them to.
- Losing your seed phrase means losing the identity. We explicitly chose not to have a key-recovery server.
- Skattr does not protect against endpoint compromise. If your device is compromised, the attacker can read your messages.
