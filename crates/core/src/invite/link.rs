// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Invite link parsing, generation, signing, and verification.
//!
//! Wire layout (fragment-encoded, per design §1.4):
//!
//! ```text
//! skattr://invite/v1#card=<base64url(CBOR of inviter's signed ContactCard)>
//!                   &kp=<base64url(MLS KeyPackage)>
//!                   &psk=<base64url(32-byte one-time secret)>
//!                   &exp=<unix timestamp>
//!                   &sig=<base64url(Ed25519 signature over canonical CBOR of body)>
//! ```
//!
//! Both `generate` and `from_url` return a validated [`InviteLink`] —
//! the signature is verified before the type is constructed.

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{CoreError, Result};
use crate::identity::{IdentityKey, Signature};
use crate::invite::InviteErrorKind;
use crate::storage::KeyPackageRepo;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;

const URL_PREFIX: &str = "skattr://invite/v1#";

fn encode_b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn decode_b64url(s: &str) -> Option<Vec<u8>> {
    // Tolerate accidental padding: strip trailing '=' before decoding.
    let trimmed = s.trim_end_matches('=');
    URL_SAFE_NO_PAD.decode(trimmed.as_bytes()).ok()
}

/// Content that the inviter signs. Deliberately excludes the signature.
#[derive(Clone, Serialize, Deserialize)]
pub struct InviteLinkBody {
    /// Inviter's signed self-card (identity + onion + mailboxes + version).
    /// Supersedes the bare identity+onion (ADR 0008).
    pub card: crate::contact::ContactCard,
    /// Single-use MLS KeyPackage (binary, TLS-codec bytes from 1.C).
    #[serde(with = "serde_bytes")]
    pub key_package: Vec<u8>,
    /// 32-byte one-time secret mixed into Noise PSK + first MLS Commit.
    pub psk: [u8; 32],
    /// Unix timestamp (seconds) after which the invite is invalid.
    pub expires_at: i64,
}

impl std::fmt::Debug for InviteLinkBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InviteLinkBody")
            .field("identity", &self.card.body.identity)
            .field("onion", &self.card.body.onion)
            .field(
                "key_package",
                &format_args!("<{} bytes>", self.key_package.len()),
            )
            .field("psk", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Parsed + verified invite link.
pub struct InviteLink {
    /// Unsigned body fields. `body.psk` is zeroized after parse; read
    /// the PSK via `self.psk` (the Zeroizing guard).
    pub body: InviteLinkBody,
    /// Ed25519 signature over canonical CBOR of `body`.
    pub signature: Signature,
    /// Zeroizing copy of the PSK.
    pub psk: InvitePsk,
}

impl std::fmt::Debug for InviteLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InviteLink")
            .field("body", &self.body)
            .field("signature", &"[redacted]")
            .field("psk", &"[redacted]")
            .finish()
    }
}

/// A 32-byte one-time secret embedded in an invite.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct InvitePsk(pub [u8; 32]);

impl InviteLink {
    /// Build + sign a new invite.
    pub fn generate(
        inviter: &IdentityKey,
        card: crate::contact::ContactCard,
        key_package: Vec<u8>,
        psk: [u8; 32],
        ttl_secs: u64,
        now: i64,
    ) -> Result<Self> {
        if card.body.identity != inviter.public() {
            return Err(CoreError::Invite(InviteErrorKind::Other(
                "card identity != inviter".into(),
            )));
        }
        let expires_at = now
            .checked_add(i64::try_from(ttl_secs).map_err(|_| {
                CoreError::Invite(InviteErrorKind::Other("ttl overflows i64".into()))
            })?)
            .ok_or_else(|| {
                CoreError::Invite(InviteErrorKind::Other("expires_at overflows i64".into()))
            })?;

        let body = InviteLinkBody {
            card,
            key_package,
            psk,
            expires_at,
        };
        let signature = inviter
            .sign_cbor(&body)
            .map_err(|e| CoreError::Invite(InviteErrorKind::Other(format!("sign: {e}"))))?;
        Ok(Self {
            body,
            signature,
            psk: InvitePsk(psk),
        })
    }

    /// Parse + verify a `skattr://invite/v1#...` URL.
    pub fn from_url(url: &str, now: i64) -> Result<Self> {
        use zeroize::Zeroize as _;

        let fragment = url.strip_prefix(URL_PREFIX).ok_or_else(|| {
            CoreError::Invite(InviteErrorKind::Other("unsupported scheme".into()))
        })?;

        // Parse key=value pairs. Unknown keys are ignored for forward-compat.
        let mut card_str: Option<&str> = None;
        let mut kp_str: Option<&str> = None;
        let mut psk_str: Option<&str> = None;
        let mut exp_str: Option<&str> = None;
        let mut sig_str: Option<&str> = None;
        for pair in fragment.split('&') {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next().unwrap_or_default();
            let value = parts.next().unwrap_or_default();
            match key {
                "card" => card_str = Some(value),
                "kp" => kp_str = Some(value),
                "psk" => psk_str = Some(value),
                "exp" => exp_str = Some(value),
                "sig" => sig_str = Some(value),
                _ => {}
            }
        }

        let kp_str = kp_str
            .ok_or_else(|| CoreError::Invite(InviteErrorKind::Other("missing field kp".into())))?;
        let psk_str = psk_str
            .ok_or_else(|| CoreError::Invite(InviteErrorKind::Other("missing field psk".into())))?;
        let exp_str = exp_str
            .ok_or_else(|| CoreError::Invite(InviteErrorKind::Other("missing field exp".into())))?;
        let sig_str = sig_str
            .ok_or_else(|| CoreError::Invite(InviteErrorKind::Other("missing field sig".into())))?;

        let key_package = decode_b64url(kp_str)
            .ok_or_else(|| CoreError::Invite(InviteErrorKind::Other("malformed kp".into())))?;

        let psk_bytes = decode_b64url(psk_str)
            .ok_or_else(|| CoreError::Invite(InviteErrorKind::Other("malformed psk".into())))?;
        if psk_bytes.len() != 32 {
            return Err(CoreError::Invite(InviteErrorKind::Other(
                "malformed psk".into(),
            )));
        }
        let mut psk = [0u8; 32];
        psk.copy_from_slice(&psk_bytes);

        let expires_at: i64 = exp_str
            .parse()
            .map_err(|_| CoreError::Invite(InviteErrorKind::Other("malformed exp".into())))?;

        let sig_bytes = decode_b64url(sig_str)
            .ok_or_else(|| CoreError::Invite(InviteErrorKind::Other("malformed sig".into())))?;
        if sig_bytes.len() != 64 {
            return Err(CoreError::Invite(InviteErrorKind::Other(
                "malformed sig".into(),
            )));
        }
        let mut sig_arr = [0u8; 64];
        sig_arr.copy_from_slice(&sig_bytes);
        let signature = Signature(sig_arr);

        let card_str = card_str.ok_or_else(|| {
            CoreError::Invite(InviteErrorKind::Other("missing field card".into()))
        })?;
        let card_blob = decode_b64url(card_str)
            .ok_or_else(|| CoreError::Invite(InviteErrorKind::Other("malformed card".into())))?;
        let card: crate::contact::ContactCard = ciborium::de::from_reader(&card_blob[..])
            .map_err(|e| CoreError::Invite(InviteErrorKind::Other(format!("card decode: {e}"))))?;

        let body = InviteLinkBody {
            card,
            key_package,
            psk,
            expires_at,
        };

        // Verify signature.
        IdentityKey::verify_cbor(&body.card.body.identity, &body, &signature)
            .map_err(|_| CoreError::Invite(InviteErrorKind::SignatureInvalid))?;

        // Expiry check.
        if now > body.expires_at {
            return Err(CoreError::Invite(InviteErrorKind::Expired));
        }

        // Move PSK into guard, zero body copy.
        let guard = InvitePsk(body.psk);
        let mut body = body;
        body.psk.zeroize();
        Ok(Self {
            body,
            signature,
            psk: guard,
        })
    }

    /// Re-serialize to a URL.
    pub fn to_url(&self) -> Result<String> {
        let mut card_blob = Vec::new();
        ciborium::ser::into_writer(&self.body.card, &mut card_blob)
            .map_err(|e| CoreError::Invite(InviteErrorKind::Other(format!("card cbor: {e}"))))?;
        let card = encode_b64url(&card_blob);
        let kp = encode_b64url(&self.body.key_package);
        let psk = encode_b64url(&self.psk.0);
        let sig = encode_b64url(&self.signature.0);
        Ok(format!(
            "{prefix}card={card}&kp={kp}&psk={psk}&exp={exp}&sig={sig}",
            prefix = URL_PREFIX,
            card = card,
            kp = kp,
            psk = psk,
            exp = self.body.expires_at,
            sig = sig,
        ))
    }

    /// SHA-256 of `body.key_package`.
    pub fn kp_hash(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&self.body.key_package);
        h.finalize().into()
    }

    /// Record this received invite's KP under `direction='theirs'`.
    pub fn record_received(&self, kp_repo: &KeyPackageRepo<'_>) -> Result<()> {
        let hash = self.kp_hash();
        // Idempotent: if already present, no-op.
        if kp_repo.get(&hash)?.is_some() {
            return Ok(());
        }
        kp_repo.insert(&hash, &self.body.key_package, "theirs")
    }

    /// Whether this invite's KP has been marked consumed in the repo.
    pub fn is_consumed(&self, kp_repo: &KeyPackageRepo<'_>) -> Result<bool> {
        let hash = self.kp_hash();
        match kp_repo.get(&hash)? {
            Some((_, consumed)) => Ok(consumed),
            None => Ok(false),
        }
    }

    /// Flip `consumed=1` for this invite's KP.
    pub fn mark_consumed(&self, kp_repo: &KeyPackageRepo<'_>) -> Result<()> {
        let hash = self.kp_hash();
        if kp_repo.get(&hash)?.is_none() {
            return Err(CoreError::Invite(InviteErrorKind::Other(
                "unknown: not recorded".into(),
            )));
        }
        kp_repo.mark_consumed(&hash)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn fixed_kp() -> Vec<u8> {
        (0..64u8).collect()
    }

    /// Build a self-card signed by `inviter` carrying `onion`.
    fn card_for(inviter: &IdentityKey, onion: &str) -> crate::contact::ContactCard {
        crate::contact::ContactCard::sign(inviter, onion.into(), vec![], 1, 86_400, 1_000).unwrap()
    }

    #[test]
    fn generate_populates_body_and_signature() {
        let inviter = IdentityKey::generate().unwrap();
        let psk = [0xAA; 32];
        let invite = InviteLink::generate(
            &inviter,
            card_for(&inviter, "abc.onion"),
            fixed_kp(),
            psk,
            3600,
            1_000_000,
        )
        .unwrap();

        assert_eq!(invite.body.card.body.identity, inviter.public());
        assert_eq!(invite.body.card.body.onion, "abc.onion");
        assert_eq!(invite.body.key_package, fixed_kp());
        assert_eq!(invite.body.psk, psk);
        assert_eq!(invite.body.expires_at, 1_000_000 + 3600);
        assert_eq!(invite.signature.0.len(), 64);
        assert_eq!(invite.psk.0, psk);
    }

    #[test]
    fn generate_signature_verifies_via_identity() {
        let inviter = IdentityKey::generate().unwrap();
        let invite = InviteLink::generate(
            &inviter,
            card_for(&inviter, "xyz.onion"),
            fixed_kp(),
            [0xBB; 32],
            3600,
            1_000_000,
        )
        .unwrap();
        IdentityKey::verify_cbor(&invite.body.card.body.identity, &invite.body, &invite.signature)
            .expect("body signature must verify against its embedded identity");
    }

    #[test]
    fn to_url_has_expected_prefix_and_all_params() {
        let inviter = IdentityKey::generate().unwrap();
        let invite = InviteLink::generate(
            &inviter,
            card_for(&inviter, "abc.onion"),
            fixed_kp(),
            [0xAA; 32],
            3600,
            1_000_000,
        )
        .unwrap();

        let url = invite.to_url().unwrap();
        assert!(url.starts_with("skattr://invite/v1#"));
        let fragment = url.strip_prefix("skattr://invite/v1#").unwrap();
        let keys: Vec<&str> = fragment
            .split('&')
            .map(|p| p.split('=').next().unwrap())
            .collect();
        assert_eq!(keys, &["card", "kp", "psk", "exp", "sig"]);
    }

    #[test]
    fn to_url_is_deterministic() {
        let inviter = IdentityKey::generate().unwrap();
        let invite = InviteLink::generate(
            &inviter,
            card_for(&inviter, "a.onion"),
            fixed_kp(),
            [0xAA; 32],
            3600,
            1_000_000,
        )
        .unwrap();
        assert_eq!(invite.to_url().unwrap(), invite.to_url().unwrap());
    }

    #[test]
    fn from_url_round_trip_valid() {
        let inviter = IdentityKey::generate().unwrap();
        let invite = InviteLink::generate(
            &inviter,
            card_for(&inviter, "xyz.onion"),
            fixed_kp(),
            [0xCC; 32],
            3600,
            1_000_000,
        )
        .unwrap();
        let url = invite.to_url().unwrap();

        let parsed = InviteLink::from_url(&url, 1_000_500).unwrap();
        assert_eq!(parsed.body.card.body.identity, invite.body.card.body.identity);
        assert_eq!(parsed.body.card.body.onion, invite.body.card.body.onion);
        assert_eq!(parsed.body.key_package, invite.body.key_package);
        assert_eq!(parsed.body.expires_at, invite.body.expires_at);
        // PSK moved into the Zeroizing guard; body.psk cleared.
        assert_eq!(parsed.psk.0, [0xCC; 32]);
        assert_eq!(parsed.body.psk, [0u8; 32]);
    }

    #[test]
    fn from_url_rejects_unsupported_scheme() {
        let err = InviteLink::from_url("https://example.com/?id=x", 0).expect_err("bad scheme");
        match err {
            crate::error::CoreError::Invite(InviteErrorKind::Other(s)) => {
                assert!(s.contains("unsupported scheme"), "got: {s}");
            }
            other => panic!("expected Invite(Other), got {other:?}"),
        }
    }

    #[test]
    fn from_url_rejects_missing_field() {
        let inviter = IdentityKey::generate().unwrap();
        let url = InviteLink::generate(
            &inviter,
            card_for(&inviter, "a.onion"),
            fixed_kp(),
            [0xDD; 32],
            3600,
            1_000_000,
        )
        .unwrap()
        .to_url()
        .unwrap();

        // Drop the `&kp=...` segment entirely.
        let fragment = url.strip_prefix("skattr://invite/v1#").unwrap();
        let trimmed: String = fragment
            .split('&')
            .filter(|p| !p.starts_with("kp="))
            .collect::<Vec<_>>()
            .join("&");
        let bad = format!("skattr://invite/v1#{trimmed}");

        let err = InviteLink::from_url(&bad, 1_000_000).expect_err("missing kp");
        match err {
            crate::error::CoreError::Invite(InviteErrorKind::Other(s)) => {
                assert!(s.contains("missing field kp"), "got: {s}");
            }
            other => panic!("expected Invite(Other), got {other:?}"),
        }
    }

    /// Find the byte offset of the first character inside `&sig=` value.
    fn sig_value_offset(bytes: &[u8]) -> usize {
        let needle = b"&sig=";
        for i in 0..bytes.len().saturating_sub(needle.len()) {
            if &bytes[i..i + needle.len()] == needle {
                return i + needle.len();
            }
        }
        panic!("no &sig= in URL")
    }

    #[test]
    fn from_url_rejects_tampered_signature() {
        let inviter = IdentityKey::generate().unwrap();
        let url = InviteLink::generate(
            &inviter,
            card_for(&inviter, "a.onion"),
            fixed_kp(),
            [0xEE; 32],
            3600,
            1_000_000,
        )
        .unwrap()
        .to_url()
        .unwrap();
        // Flip one character in the sig segment.
        let mut bytes = url.into_bytes();
        let sig_start = sig_value_offset(&bytes);
        bytes[sig_start] ^= 0x01;
        // Ensure we didn't produce an out-of-alphabet char.
        if !bytes[sig_start].is_ascii_alphanumeric()
            && bytes[sig_start] != b'-'
            && bytes[sig_start] != b'_'
        {
            bytes[sig_start] = b'A';
        }
        let tampered = String::from_utf8(bytes).unwrap();

        let err = InviteLink::from_url(&tampered, 1_000_000).expect_err("tampered");
        match err {
            crate::error::CoreError::Invite(InviteErrorKind::SignatureInvalid) => {}
            crate::error::CoreError::Invite(InviteErrorKind::Other(s)) => {
                assert!(s.contains("malformed"), "got: {s}");
            }
            other => panic!("expected Invite(SignatureInvalid) or Invite(Other), got {other:?}"),
        }
    }

    #[test]
    fn from_url_rejects_expired() {
        let inviter = IdentityKey::generate().unwrap();
        let url = InviteLink::generate(
            &inviter,
            card_for(&inviter, "a.onion"),
            fixed_kp(),
            [0xFF; 32],
            3600,
            1_000_000,
        )
        .unwrap()
        .to_url()
        .unwrap();

        let err = InviteLink::from_url(&url, 1_003_601).expect_err("expired");
        match err {
            crate::error::CoreError::Invite(InviteErrorKind::Expired) => {}
            other => panic!("expected Invite(Expired), got {other:?}"),
        }
    }

    use crate::storage::{KeyPackageRepo, Pool};

    fn make_invite() -> InviteLink {
        let inviter = IdentityKey::generate().unwrap();
        InviteLink::generate(
            &inviter,
            card_for(&inviter, "a.onion"),
            fixed_kp(),
            [0xAA; 32],
            3600,
            1_000_000,
        )
        .unwrap()
    }

    #[test]
    fn kp_hash_matches_sha256_of_key_package() {
        let invite = make_invite();
        let hash = invite.kp_hash();

        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&invite.body.key_package);
        let expected: [u8; 32] = h.finalize().into();
        assert_eq!(hash, expected);
    }

    #[test]
    fn record_received_then_is_consumed_false_then_mark_then_true() {
        let pool = Pool::in_memory();
        let kp_repo = KeyPackageRepo::new(&pool);
        let invite = make_invite();

        invite.record_received(&kp_repo).unwrap();
        assert!(!invite.is_consumed(&kp_repo).unwrap());

        invite.mark_consumed(&kp_repo).unwrap();
        assert!(invite.is_consumed(&kp_repo).unwrap());
    }

    #[test]
    fn record_received_is_idempotent() {
        let pool = Pool::in_memory();
        let kp_repo = KeyPackageRepo::new(&pool);
        let invite = make_invite();

        invite.record_received(&kp_repo).unwrap();
        invite.record_received(&kp_repo).unwrap();

        let hash = invite.kp_hash();
        let row = kp_repo.get(&hash).unwrap().unwrap();
        assert_eq!(row.0, invite.body.key_package);
    }

    #[test]
    fn mark_consumed_on_unrecorded_errors() {
        let pool = Pool::in_memory();
        let kp_repo = KeyPackageRepo::new(&pool);
        let invite = make_invite();
        let err = invite.mark_consumed(&kp_repo).expect_err("unrecorded");
        match err {
            crate::error::CoreError::Invite(InviteErrorKind::Other(s)) => {
                assert!(s.contains("unknown"), "got: {s}");
            }
            other => panic!("expected Invite(Other), got {other:?}"),
        }
    }

    #[test]
    fn is_consumed_returns_false_for_unrecorded() {
        let pool = Pool::in_memory();
        let kp_repo = KeyPackageRepo::new(&pool);
        let invite = make_invite();
        assert!(!invite.is_consumed(&kp_repo).unwrap());
    }

    #[test]
    fn to_url_after_from_url_round_trips_psk_from_guard() {
        let inviter = IdentityKey::generate().unwrap();
        let original = InviteLink::generate(
            &inviter,
            card_for(&inviter, "a.onion"),
            fixed_kp(),
            [0x77; 32],
            3600,
            1_000_000,
        )
        .unwrap();
        let url1 = original.to_url().unwrap();

        // Parse, then re-emit. The PSK lives in the guard after from_url;
        // to_url must pick it up from there, not from the (zeroed) body.
        let parsed = InviteLink::from_url(&url1, 1_000_500).unwrap();
        let url2 = parsed.to_url().unwrap();

        assert_eq!(url1, url2, "to_url must be idempotent across from_url");
    }

    #[test]
    fn invite_round_trips_embedded_card() {
        let inviter = IdentityKey::generate().unwrap();
        let card =
            crate::contact::ContactCard::sign(&inviter, "inviter.onion".into(), vec![], 1, 86_400, 1_000)
                .unwrap();
        let link = InviteLink::generate(&inviter, card, vec![9u8; 4], [7u8; 32], 600, 1_000).unwrap();
        let url = link.to_url().unwrap();
        assert!(url.starts_with(URL_PREFIX));
        let parsed = InviteLink::from_url(&url, 1_100).unwrap();
        assert_eq!(parsed.body.card.body.identity, inviter.public());
        assert_eq!(parsed.body.card.body.onion, "inviter.onion");
        assert_eq!(parsed.body.key_package, vec![9u8; 4]);
        let attacker = IdentityKey::generate().unwrap();
        let bad_card = crate::contact::ContactCard::sign(
            &attacker,
            "evil.onion".into(),
            vec![],
            1,
            86_400,
            1_000,
        )
        .unwrap();
        assert!(
            InviteLink::generate(&inviter, bad_card, vec![9u8; 4], [7u8; 32], 600, 1_000).is_err(),
            "generate must reject a card whose identity != inviter"
        );
    }
}
