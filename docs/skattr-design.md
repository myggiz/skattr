# Skattr: Protocol Design & Development Plan

A desktop-first, metadata-resistant, server-less (ish) chat application. Built on Tor v3 onion services + MLS + Rust.

---

## Part 1: Wire Protocol

### 1.1 Identity model

Two separate Ed25519 keypairs per user:

- **Identity key** — long-term, defines "who you are." Used to sign messages, introductions, and to authenticate transport handshakes. Backed up by the user (seed phrase).
- **Onion service key** — defines "where to reach you." Rotatable without identity loss. When a user wants a fresh network identity, they rotate this and republish contact info to their contacts; the identity key stays stable so chat history remains tied to the same "person."

Keeping these separate is a real quality-of-life feature. Don't skip it.

A **user address** is therefore `(identity_pubkey, current_onion_address)`, with the onion address replaceable via a signed rotation message.

### 1.2 Transport framing

Every connection is a single bidirectional Tor stream carrying length-prefixed frames.

```
+-----------------+----------+----------------------+
| length (u32 BE) | type (u8)|   payload (bytes)    |
+-----------------+----------+----------------------+
```

- `length` covers `type + payload`, max 16 MiB per frame
- Frames are processed in order; out-of-order delivery is not a concern (Tor gives us a reliable stream)

**Frame types:**

| Type | Name | Direction | Purpose |
|------|------|-----------|---------|
| 0x01 | NOISE_INIT | Initiator → | First Noise handshake message |
| 0x02 | NOISE_RESP | Responder → | Noise handshake response |
| 0x03 | MLS_WELCOME | Either | MLS Welcome message (adds peer to group) |
| 0x04 | MLS_COMMIT | Either | MLS Commit (ratchet, membership change) |
| 0x05 | MLS_APP | Either | Encrypted application message |
| 0x06 | ACK | Either | Delivery ACK (carries message id) |
| 0x07 | PING / 0x08 PONG | Either | Keepalive |
| 0x09 | BYE | Either | Graceful close |
| 0x0A | ERROR | Either | Typed error (invalid frame, auth failure, etc.) |

Anything above 0x20 is reserved for future extension; unknown types produce ERROR and connection close.

### 1.3 Transport handshake: Noise_XK

After the Tor stream is open, both sides run a **Noise_XK** handshake using the peers' identity keys as Noise static keys (distinct from onion service keys, see §1.1).

`XK` means: responder's static key is known to initiator in advance, initiator's static key is transmitted encrypted. Properties we get:

- Mutual authentication based on identity keys
- Forward secrecy on the transport layer
- Initiator's identity is not revealed to observers (already not visible via Tor, but belt-and-braces)
- Resistance to identity misbinding

Why Noise rather than TLS: smaller, auditable, no cert infrastructure, no ALPN/SNI leaks. Noise is what WireGuard and Signal's transport use for the same reasons.

Handshake transcript is bound into MLS exporter secrets later (see §1.5) to prevent unknown-key-share attacks across layers.

### 1.4 Contact exchange: invite links

Contact is established out-of-band via an invite link, delivered through any channel the users already trust (in person, Signal, email — the invite itself is the shared secret):

```
skattr://invite/v1#
  id=<base32(identity_pubkey)>
  &onion=<56-char onion address>
  &kp=<base64url(MLS KeyPackage)>
  &psk=<base64url(32-byte one-time secret)>
  &exp=<unix timestamp>
  &sig=<base64url(Ed25519 signature over the above)>
```

- `kp` is a pre-published MLS KeyPackage, consumed exactly once
- `psk` is mixed into the Noise handshake and the first MLS epoch as a pre-shared key. This defends against active MITM during first contact: an attacker substituting their own onion/identity would fail to know the PSK.
- `sig` is Ed25519 over the canonical serialization of the other fields, signed with `identity_pubkey`. Prevents tampering with the link contents (e.g. swapping the onion address while keeping the signed identity).
- `exp` limits the window an intercepted link is usable

Scan as QR code or paste as URL. The fragment (`#...`) keeps params out of HTTP referer logs if shared via a web link.

### 1.5 Application messaging: MLS

On top of the authenticated Noise stream, messages flow as MLS application messages.

Why MLS instead of Double Ratchet:

- Native group support — 1:1 is just a 2-member group, which means one codepath
- Better post-compromise recovery for long-offline users (new epoch via Commit heals faster than chain ratchet)
- IETF-standardized (RFC 9420), multiple interoperable implementations

Use **OpenMLS** (Rust, actively maintained). Ciphersuite: `MLS_256_DHKEMX25519_AES256GCM_SHA512_Ed25519` — modern, well-analyzed primitives, no post-quantum yet but the ciphersuite can be upgraded via MLS's `extensions` mechanism when PQ variants mature.

**Binding MLS to transport:** during Noise handshake, compute a binding value `h_transport = HKDF(handshake_hash, "skattr-binding-v1")`. Include `h_transport` as external PSK input to the first MLS Commit after handshake. This prevents an attacker who somehow got the MLS state from replaying it over a different Noise session.

### 1.6 Message envelope (inside MLS `application_data`)

```
{
  "v": 1,
  "id": "<16-byte random message id>",
  "ts": <unix millis, sender's clock>,
  "reply_to": "<optional message id>",
  "kind": "text" | "file" | "typing" | "read" | "delete" | "reaction",
  "body": <kind-specific>
}
```

Canonical CBOR serialization (smaller and more deterministic than JSON — important for fuzzing and forensic reproducibility).

Receiver validates `ts` is within ±1 hour of local clock (replay resistance) and `id` is unique per sender within a sliding 24-hour window (dedup).

---

## Part 2: Onion Service Handshake — Full Sequence

Concrete flow for Alice first connecting to Bob after receiving his invite link:

```
Alice                                          Tor network                                          Bob
  |                                                                                                  |
  |-- 1. Parse invite, verify Ed25519 sig over link fields with id=Bob's identity_pubkey            |
  |                                                                                                  |
  |-- 2. Arti: build circuit, RENDEZVOUS with bob.onion -------------------------------------------->|
  |                                                                                                  |
  |<----------------------------------- rendezvous established, bidirectional stream -------------->|
  |                                                                                                  |
  |-- 3. NOISE_INIT (Noise_XK, -e) -------------------------------------------------------------->   |
  |                                                                                                  |
  |                                                  <-- 4. NOISE_RESP (-e, ee, s, es) --           |
  |                                                                                                  |
  |-- 5. NOISE_FINAL (-s, se, psk=invite_psk) ----------------------------------------------------->|
  |                                                                                                  |
  |      Both sides now have: authenticated channel, shared h_transport                              |
  |                                                                                                  |
  |-- 6. MLS_WELCOME (Alice creates 2-member group using Bob's KeyPackage from invite,              |
  |      binding h_transport as external PSK) ---------------------------------------------------->  |
  |                                                                                                  |
  |                                                  <-- 7. MLS_COMMIT (Bob processes Welcome,     |
  |                                                          acks with empty Commit) ---           |
  |                                                                                                  |
  |-- 8. MLS_APP: first real message -------------------------------------------------------------->|
```

Notes on what each step buys you:

- **Step 1** prevents a tampered link (attacker swaps onion address).
- **Step 2** — Tor rendezvous: neither guard node nor Bob learns Alice's IP; Alice never learns Bob's IP; no DNS involved; no TLS cert authority.
- **Steps 3–5** — Noise_XK with PSK: mutual identity-key authentication, forward secrecy, MITM resistance.
- **Steps 6–7** — MLS session established; future messages have FS and PCS.
- **Step 8 onward** — steady-state application traffic.

After this first handshake, Alice and Bob save each other as contacts. Future reconnects skip the invite/PSK step and use cached identity keys directly (still full Noise + MLS).

### 2.1 Reconnection

On subsequent sessions:

- Alice connects to `bob.onion`, runs Noise_XK with cached identity keys
- MLS state is resumed from local storage; the protocol handles missed Commits via the `epoch` field — if Bob has advanced while Alice was offline, he sends Commits forward until Alice catches up
- If Bob has rotated his onion address, Alice learns about it via a signed `AddressRotation` message that Bob publishes through any shared mailbox (see Part 3) or through a mutual contact

---

## Part 3: Mailbox Design

The one unavoidable piece of infrastructure. Accept it and design it properly.

### 3.1 Threat model for a mailbox

A mailbox is **semi-trusted infrastructure** operated by the user or someone they trust (friend's home server, self-hosted VPS, community-run pool). The properties we want:

| Property | Guarantee |
|----------|-----------|
| Confidentiality | Mailbox **cannot** read message contents (MLS ciphertext only) |
| Sender privacy | Mailbox **cannot** learn who sent a given message (sealed sender) |
| Recipient privacy (from outsiders) | Mailbox connections are over Tor; external observer sees nothing |
| Recipient privacy (from operator) | Mailbox **does** learn which identity polls it. Mitigation: user chooses the operator. |
| Availability | Mailbox **can** withhold, delay, or drop messages — detectable via per-sender sequence numbers, not preventable. |
| Integrity | Mailbox **cannot** forge or alter messages (signed inside MLS). |
| Forward secrecy | Even if mailbox is later compromised, stored ciphertext is useless (MLS epoch keys rotate, old messages are deleted on retrieval). |

The key insight: the mailbox sees *ciphertext to a pubkey-hash* and *polling from the owner of that pubkey*. It does not see *who is talking to whom*.

### 3.2 Mailbox protocol

Mailbox is itself a Tor onion service. It exposes three operations over the same framed protocol:

**DEPOSIT** (anyone can call):
```
DEPOSIT {
  recipient_id_hash: 32 bytes,   // SHA-256(recipient_identity_pubkey)
  ciphertext: bytes,             // MLS application message envelope
  expiry: unix timestamp         // sender's requested TTL, capped by mailbox policy
}
→ OK { deposit_id } | ERROR
```

No authentication required — anyone on Tor can deposit. This is fine: ciphertext is useless without the MLS key, and spam is handled by per-sender rate-limiting at the MLS layer (mailbox can also impose anon rate limits).

**FETCH** (only the recipient can meaningfully call):
```
FETCH {
  challenge_response: Ed25519 signature over (server_nonce || timestamp)
                     by recipient_identity_privkey
}
→ MESSAGES [ { deposit_id, ciphertext, received_at }, ... ]
```

Server nonce is obtained via a prior `CHALLENGE` call. Signature proves possession of the identity privkey → server returns all pending messages for `SHA-256(recipient_pubkey)`.

**DELETE** (recipient confirms receipt):
```
DELETE { deposit_ids: [ ... ], signature }
→ OK
```

Messages hang around until DELETE'd or TTL expires, so a crashed client doesn't lose messages.

### 3.3 Who runs a mailbox?

Three viable models, in order of decentralization:

1. **Self-hosted** — user runs `mailbox` binary on a home server, old laptop, or $5 VPS. Zero trust issues. Requires technical ability. Primary target for power users.
2. **Friend-hosted** — one member of a friend group runs a mailbox for everyone. Small-scale, high-trust.
3. **Volunteer pool** — Tor-relay-style volunteer operators list themselves in a signed directory. User picks one (or several, for redundancy). Lower trust but wider access.

The app should ship with (1) as the default recommended flow and (3) as an easier onboarding path. **Never** ship with a default centralized mailbox — that's how "decentralized" apps quietly centralize.

### 3.4 Multiple mailboxes

A user can register with multiple mailboxes and publish all of them in their contact info. Senders pick one (or fan out to all). This gives redundancy against a single mailbox being offline or malicious (withholding messages).

### 3.5 What a mailbox is *not*

- **Not a relay network.** No onion routing inside the mailbox protocol — Tor does that job.
- **Not a discovery service.** Mailboxes do not help you find users. Contact is still out-of-band via invite links.
- **Not a group server.** Group state lives in MLS; mailboxes only carry ciphertext packets.
- **Not required to be online when you send.** If recipient is online, you deliver directly to their onion service. Mailboxes are the fallback, not the default path.

---

## Part 4: Phased Development Plan

Rough calendar assumes one to two experienced Rust developers working near-full-time. Adjust accordingly.

### Phase 0 — Foundations (weeks 1–4)

Goal: two processes exchange raw bytes over Tor.

- Repo scaffolding: Cargo workspace with crates `core` (protocol), `mailbox` (server), `cli` (client), `ui` (later)
- CI: fmt, clippy, test, cargo-deny, cargo-audit
- Identity key module: Ed25519 generation, passphrase-encrypted at-rest (Argon2id → XChaCha20-Poly1305)
- Arti integration: embed Arti, bootstrap, publish a v3 onion service, accept streams
- Local storage: SQLCipher (or `rusqlite` + `age`) with a clean migrations system
- Threat model v0 document committed to repo

**Exit criteria:** `skattr init` generates keys, `skattr daemon` starts Tor and publishes an onion, two daemons on two machines can open a stream and echo bytes.

### Phase 1 — 1:1 messaging, online only (weeks 5–10)

Goal: two online users can chat end-to-end encrypted over Tor.

- Frame codec (§1.2) with full fuzz coverage via `cargo-fuzz`
- Noise_XK implementation via `snow` crate, integrated with frame codec
- OpenMLS integration, 2-member groups only for now
- Invite link generation/parsing (§1.4), QR code rendering
- Contact storage (identity key, current onion, MLS state)
- CLI commands: `invite`, `add <invite>`, `send <contact> <text>`, `recv` (blocks)
- Per-message ACKs and at-least-once delivery semantics
- Local message history with full-text search (sqlite FTS5)

**Exit criteria:** two CLI users on different networks can exchange messages that survive restart. Security review of handshake code by at least one external pair of eyes.

### Phase 2 — Offline delivery + UI shell (weeks 11–16)

Goal: users can message each other without being online simultaneously. Non-developers can install and use the app.

- Mailbox protocol spec, finalized and versioned
- `mailbox` server binary: single static binary, embedded Arti, sqlite storage
- Client-side mailbox registration flow, multiple mailboxes per user
- Polling strategy: adaptive interval with jitter (cover traffic is a Phase 4 problem)
- Contact info publication (onion + mailbox list) signed by identity key
- Onion address rotation protocol
- Tauri UI skeleton: contact list, conversation view, settings
- Packaging: `.deb`, `.dmg`, `.msi` produced by CI

**Exit criteria:** a non-technical tester can install the app from a signed installer, receive an invite link, add the contact, and exchange messages that survive both parties being offline.

### Phase 3 — Groups and richer messaging (weeks 17–24)

Goal: small groups work end-to-end.

- MLS groups 3–50 members, with add/remove/leave
- Group metadata (name, avatar) encrypted and synced in MLS application messages
- File attachments: chunked, encrypted, delivered via mailboxes with separate large-blob storage (or via direct transfer when both online)
- Message kinds: replies, reactions, edits, delete-for-everyone (subject to ratchet constraints)
- Delivery and read receipts (optional, off-by-default — they're a metadata leak)
- Basic notification system (desktop native notifications, content-redactable)

**Exit criteria:** a 20-person group can have a week-long conversation with members coming and going, surviving membership changes and network partitions.

### Phase 4 — Hardening (weeks 25–32)

Goal: ready for a small public beta.

- Written threat model and security architecture document, published
- Third-party cryptographic + application security audit (budget ~$30–80k for a real one — non-negotiable for a product claiming these properties)
- Fuzzing: protocol parsers, CBOR, MLS message handlers
- Metadata minimization pass: size padding on all messages, timing jitter on sends, optional cover traffic
- Panic button / duress mode: passphrase variant that wipes keys and history
- Seed-phrase backup and restore of identity key
- Multi-device: investigate and decide — either MLS-native multi-device (each device is a separate group member) or deferred to post-1.0
- Reproducible builds
- Localization framework

**Exit criteria:** external audit complete with findings addressed, public threat model, reproducible builds verified by a third party.

### Phase 5 — Public release and beyond (weeks 33+)

- Code signing across platforms, notarization on macOS
- Signed installer distribution and signed update channel
- User documentation, FAQ, threat model in plain language
- Tor Browser-style "what it protects against and what it doesn't" page
- Bug bounty program
- Community mailbox operator documentation

**Post-launch roadmap candidates:**

- Mobile port (the hardest thing, now tackled with a mature desktop codebase as a foundation — still a major project)
- Post-quantum ciphersuite migration when MLS standardizes one
- Federated group discovery via Briar-style introduction protocol
- Voice/video (enormous scope — probably a separate product)

---

## Part 5: Pitfalls Worth Flagging Now

A few things that will bite you if you don't plan for them early.

**Arti maturity.** Arti is solid but younger than C-Tor. Test onion service performance under real load early. Have a fallback plan (shell out to system Tor) if you hit blockers.

**MLS state corruption.** MLS is stateful and unforgiving. A single bad write to MLS storage can brick a group for a user. Treat MLS state like a database: write-ahead log, checksums, recovery paths, explicit "rebuild from Welcome" flow.

**Clock skew.** Messages have timestamps. Users have wrong clocks. Don't be strict about ordering based on timestamps; use MLS generation numbers for authoritative ordering and treat timestamps as display-only.

**Delete semantics.** "Delete for everyone" is a lie in E2EE unless the recipient's client cooperates. Be honest about this in the UI. Same for screenshots, forwarding, etc.

**Metadata leaks in notifications.** Desktop OS notifications route through OS-level services that log. Make notification content configurable (sender only / generic / full).

**Identity key loss.** Without backup, losing the device means losing the identity. Design the seed phrase flow in Phase 0 and force users through it on first run — do not defer this to later.

**The "is the mailbox operator honest" question.** A curious mailbox operator learns polling patterns, which leaks online/offline status and rough activity levels. Document this. Recommend self-hosting. Consider cover polling (constant-rate dummy polls) as a Phase 4+ feature.

**Don't roll your own anything cryptographic.** No custom Noise patterns, no "slight tweaks" to MLS, no hand-rolled AEAD. Every place this document says "use X" — use X.
