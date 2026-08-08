// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Thin wrapper over `openmls::group::MlsGroup`.

use openmls::group::{MlsGroupJoinConfig, StagedWelcome};
use openmls::prelude::*;
use openmls::prelude::{ProcessedMessageContent, ProtocolMessage};
use openmls::schedule::psk::PreSharedKeyId;
use rand::RngCore as _;
use tls_codec::{Deserialize as _, Serialize as _};

use crate::envelope::Envelope;
use crate::error::{CoreError, Result};
use crate::identity::IdentityKey;
use crate::mls::error_kind::MlsErrorKind;
use crate::mls::key_package::{
    credential_with_key, signer_from_identity, KeyPackage, MLS_CIPHERSUITE,
};
use crate::mls::provider::MlsProvider;
use crate::mls::state_machine::GroupState;
use crate::storage::MlsGroupRepo;

/// Opaque MLS group id. Skattr always generates 32 random bytes at `create_solo`
/// time; joiners inherit the id from the Welcome. The 32-byte invariant is
/// enforced by `ReceiveOutcome::New.group_id: [u8; 32]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GroupId(pub Vec<u8>);

/// Opaque Welcome blob.
pub type WelcomeBytes = Vec<u8>;

/// Opaque Commit blob.
pub type CommitBytes = Vec<u8>;

/// A single MLS group from our perspective.
pub struct Group {
    id: GroupId,
    state: GroupState,
    provider: MlsProvider,
    inner: openmls::group::MlsGroup,
}

impl Group {
    /// Create a fresh single-member group.
    ///
    /// `invite_psk` and `h_transport` are each `(kp_ref, secret)`: the 32-byte
    /// invite KeyPackageRef (which derives the per-invite-unique PSK id) and the
    /// 32-byte PSK secret. Both are registered (no proposal — solo) so the
    /// subsequent `add_member` genesis Commit can reference them. `h_transport`
    /// is the transport↔MLS binding PSK (ADR 0009).
    ///
    /// **The binding is active and mandatory.** Production always passes
    /// `Some(..)` here (`daemon::dispatch`), and the responder registers the
    /// identical transcript value before `join_from_welcome`. The parameter is
    /// `Option` only so tests can construct a group without a live Noise
    /// session — do not read it as "the binding is optional".
    pub fn create_solo(
        identity: &IdentityKey,
        invite_psk: Option<(&[u8; 32], &[u8; 32])>,
        h_transport: Option<(&[u8; 32], &[u8; 32])>,
        provider: MlsProvider,
    ) -> Result<Self> {
        let signer = signer_from_identity(identity, &provider)?;
        let cwk = credential_with_key(identity, &signer);

        // Generate a 32-byte random group ID so the id is always fixed-width.
        // This lets callers enforce the [u8; 32] invariant on the group_id
        // field of ReceiveOutcome::New without a heap allocation.
        let mut gid_bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut gid_bytes);
        let openmls_gid = openmls::prelude::GroupId::from_slice(&gid_bytes);

        let group_create_config = MlsGroupCreateConfig::builder()
            .ciphersuite(MLS_CIPHERSUITE)
            .use_ratchet_tree_extension(true)
            .build();

        let inner = openmls::group::MlsGroup::new_with_group_id(
            provider.as_openmls(),
            &signer,
            &group_create_config,
            openmls_gid,
            cwk,
        )
        .map_err(|e| CoreError::from(MlsErrorKind::Other(format!("mls: builder: {e:?}"))))?;

        let gid = GroupId(gid_bytes.to_vec());

        if let Some((kp_ref, secret)) = invite_psk {
            register_psk(&provider, b"invite", kp_ref, secret)?;
        }
        if let Some((kp_ref, secret)) = h_transport {
            register_psk(&provider, b"htransport", kp_ref, secret)?;
        }

        Ok(Self {
            id: gid,
            state: GroupState::Active { epoch: 0 },
            provider,
            inner,
        })
    }

    /// Add `invitee_kp` to this group. Returns `(welcome_bytes, commit_bytes)`.
    /// Merges the pending Commit eagerly (2-member only, Phase 1.C).
    ///
    /// `invite_psk` and `h_transport` are each `(kp_ref, secret)`. When `Some`,
    /// each is registered and proposed as an external PSK (under a label-distinct,
    /// per-invite-unique id) before the genesis Commit, so the joiner's Welcome
    /// can only be processed if it holds the same secrets (ADR 0009). Two
    /// external-PSK proposals in one Commit is valid MLS.
    pub fn add_member(
        &mut self,
        invitee_kp: &KeyPackage,
        invite_psk: Option<(&[u8; 32], &[u8; 32])>,
        h_transport: Option<(&[u8; 32], &[u8; 32])>,
    ) -> Result<(WelcomeBytes, CommitBytes)> {
        // Guard against 3rd member (2-member only for 1.C).
        if self.inner.members().count() >= 2 {
            return Err(MlsErrorKind::Other("mls: add_member: already 2-member".into()).into());
        }

        if invite_psk.is_some() || h_transport.is_some() {
            let signer = load_signer(&self.provider, &own_public_key(&self.inner)?)?;
            if let Some((kp_ref, secret)) = invite_psk {
                let id = register_psk(&self.provider, b"invite", kp_ref, secret)?;
                self.inner
                    .propose_external_psk(self.provider.as_openmls(), &signer, id)
                    .map_err(|e| {
                        CoreError::from(MlsErrorKind::Other(format!(
                            "mls: propose external psk: {e:?}"
                        )))
                    })?;
            }
            if let Some((kp_ref, secret)) = h_transport {
                let id = register_psk(&self.provider, b"htransport", kp_ref, secret)?;
                self.inner
                    .propose_external_psk(self.provider.as_openmls(), &signer, id)
                    .map_err(|e| {
                        CoreError::from(MlsErrorKind::Other(format!(
                            "mls: propose external psk: {e:?}"
                        )))
                    })?;
            }
        }

        let signer = load_signer(&self.provider, &own_public_key(&self.inner)?)?;

        // OpenMLS 0.8.1 returns (commit, welcome, group_info).
        let (commit_out, welcome_out, _group_info) = self
            .inner
            .add_members(
                self.provider.as_openmls(),
                &signer,
                &[invitee_kp.as_openmls().clone()],
            )
            .map_err(|e| {
                CoreError::from(MlsErrorKind::Other(format!("mls: add_members: {e:?}")))
            })?;

        // Merge the staged Commit *before* shipping — 2-member has no
        // conflicting proposal source. Phase 2's PendingCommit state
        // removes this eager-merge.
        self.inner
            .merge_pending_commit(self.provider.as_openmls())
            .map_err(|e| {
                CoreError::from(MlsErrorKind::Other(format!(
                    "mls: merge_pending_commit: {e:?}"
                )))
            })?;

        let welcome_bytes = welcome_out.tls_serialize_detached().map_err(|e| {
            CoreError::from(MlsErrorKind::Other(format!("mls: welcome serialize: {e}")))
        })?;
        let commit_bytes = commit_out.tls_serialize_detached().map_err(|e| {
            CoreError::from(MlsErrorKind::Other(format!("mls: commit serialize: {e}")))
        })?;

        // First-contact committer: the group is NOT Active until the invited
        // peer processes the Welcome and Acks (join). Staying PendingJoin here
        // blocks app-frame sends (can_send()==false) so we never send MlsApp to
        // a peer that hasn't joined — see #93. The Ack path calls set_active().
        self.state = GroupState::PendingJoin;
        Ok((welcome_bytes, commit_bytes))
    }

    /// Transition PendingJoin -> Active on the peer's Welcome-Ack. Idempotent:
    /// returns true if it flipped, false if already Active (or Corrupt). #93.
    pub fn set_active(&mut self) -> bool {
        if matches!(self.state, GroupState::PendingJoin) {
            self.state = GroupState::Active {
                epoch: self.inner.epoch().as_u64(),
            };
            true
        } else {
            false
        }
    }

    /// Join an existing group from a TLS-serialized Welcome message.
    ///
    /// `invite_psk` and `h_transport` are each `(kp_ref, secret)` and MUST match
    /// the values the inviter proposed into the genesis Commit, or OpenMLS will
    /// fail to resolve the PSK proposals when processing the Welcome (ADR 0009).
    /// Both are registered before `StagedWelcome::new_from_welcome`.
    pub fn join_from_welcome(
        identity: &IdentityKey,
        welcome: &[u8],
        invite_psk: Option<(&[u8; 32], &[u8; 32])>,
        h_transport: Option<(&[u8; 32], &[u8; 32])>,
        provider: MlsProvider,
    ) -> Result<Self> {
        // Ensure the signer is registered in this provider so OpenMLS can
        // verify our own leaf in later ops. The signer may already be stored
        // if KeyPackage::generate was called with the same provider — ignore
        // the "already present" error and proceed.
        let _ = signer_from_identity(identity, &provider);

        if let Some((kp_ref, secret)) = invite_psk {
            register_psk(&provider, b"invite", kp_ref, secret)?;
        }
        if let Some((kp_ref, secret)) = h_transport {
            register_psk(&provider, b"htransport", kp_ref, secret)?;
        }

        let welcome_msg = MlsMessageIn::tls_deserialize_exact(welcome).map_err(|e| {
            CoreError::from(MlsErrorKind::Other(format!(
                "mls: welcome deserialize: {e}"
            )))
        })?;
        let welcome_inner = match welcome_msg.extract() {
            MlsMessageBodyIn::Welcome(w) => w,
            _ => return Err(MlsErrorKind::Other("mls: welcome: wrong message type".into()).into()),
        };

        // Note: welcome_inner.ciphersuite() is pub(crate) in openmls 0.8.1 and
        // cannot be accessed externally. Ciphersuite is validated implicitly by
        // StagedWelcome::new_from_welcome rejecting unknown suites.

        let join_config = MlsGroupJoinConfig::builder()
            .use_ratchet_tree_extension(true)
            .build();

        let staged = StagedWelcome::new_from_welcome(
            provider.as_openmls(),
            &join_config,
            welcome_inner,
            None,
        )
        .map_err(|e| {
            CoreError::from(MlsErrorKind::Other(format!("mls: welcome process: {e:?}")))
        })?;

        let inner = staged.into_group(provider.as_openmls()).map_err(|e| {
            CoreError::from(MlsErrorKind::Other(format!(
                "mls: welcome into_group: {e:?}"
            )))
        })?;

        let gid = GroupId(inner.group_id().to_vec());
        let epoch = inner.epoch().as_u64();
        Ok(Self {
            id: gid,
            state: GroupState::Active { epoch },
            provider,
            inner,
        })
    }

    /// Encrypt `envelope` as an MLS application message. Returns TLS-encoded ciphertext.
    pub fn encrypt(&mut self, envelope: &Envelope) -> Result<Vec<u8>> {
        if !self.state.can_send() {
            return Err(MlsErrorKind::Other(format!(
                "mls: encrypt: invalid state {:?}",
                self.state
            ))
            .into());
        }

        let signer = load_signer(&self.provider, &own_public_key(&self.inner)?)?;
        let plaintext = envelope.encode()?;

        let out = self
            .inner
            .create_message(self.provider.as_openmls(), &signer, &plaintext)
            .map_err(|e| CoreError::from(MlsErrorKind::Other(format!("mls: encrypt: {e:?}"))))?;

        out.tls_serialize_detached().map_err(|e| {
            CoreError::from(MlsErrorKind::Other(format!("mls: encrypt: serialize: {e}")))
        })
    }

    /// Decrypt a TLS-encoded inbound MLS message.
    ///
    /// Returns `Ok(Some(envelope))` for an application message and `Ok(None)`
    /// when the inbound message was a Commit: a Commit advances the epoch but
    /// carries no application payload, so it is merged in place (the ratchet
    /// advances on `&mut self`) and the caller MUST persist the advanced group
    /// even though there is no message row to write. This defensively tolerates
    /// an inbound Commit (e.g. a peer epoch-advance) instead of stalling
    /// delivery; v1.0 does not perform PCS, so this path is not normally hit
    /// (T2-2). Proposals are not expected in a 2-member v1.0 group and remain an
    /// error.
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Option<Envelope>> {
        if !self.state.can_receive() {
            return Err(MlsErrorKind::Other(format!(
                "mls: decrypt: invalid state {:?}",
                self.state
            ))
            .into());
        }

        let msg_in = MlsMessageIn::tls_deserialize_exact(ciphertext).map_err(|e| {
            CoreError::from(MlsErrorKind::Other(format!(
                "mls: decrypt: deserialize: {e}"
            )))
        })?;
        let protocol_message: ProtocolMessage = match msg_in.extract() {
            MlsMessageBodyIn::PrivateMessage(pm) => pm.into(),
            MlsMessageBodyIn::PublicMessage(pm) => pm.into(),
            _ => {
                return Err(
                    MlsErrorKind::Other("mls: decrypt: unsupported message type".into()).into(),
                )
            }
        };

        let processed = self
            .inner
            .process_message(self.provider.as_openmls(), protocol_message)
            .map_err(|e| {
                CoreError::from(MlsErrorKind::Other(format!(
                    "mls: authentication failed: {e:?}"
                )))
            })?;

        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(app) => {
                Ok(Some(Envelope::decode(&app.into_bytes())?))
            }
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                // A Commit advances the epoch but carries no application
                // payload — merge it and signal "no message" to the caller.
                self.merge_staged(*staged)?;
                Ok(None)
            }
            ProcessedMessageContent::ProposalMessage(_) => {
                Err(MlsErrorKind::Other("mls: decrypt: received Proposal".into()).into())
            }
            ProcessedMessageContent::ExternalJoinProposalMessage(_) => Err(MlsErrorKind::Other(
                "mls: decrypt: received ExternalJoinProposal".into(),
            )
            .into()),
        }
    }

    /// Merge an already-processed staged Commit and advance `self.state` to the
    /// new epoch. Shared by [`decrypt`](Self::decrypt) (defensive inbound-Commit
    /// tolerance) and [`process_incoming_commit`](Self::process_incoming_commit).
    fn merge_staged(&mut self, staged: openmls::group::StagedCommit) -> Result<()> {
        self.inner
            .merge_staged_commit(self.provider.as_openmls(), staged)
            .map_err(|e| {
                CoreError::from(MlsErrorKind::Other(format!(
                    "mls: merge_staged_commit: {e:?}"
                )))
            })?;
        self.state = GroupState::Active {
            epoch: self.inner.epoch().as_u64(),
        };
        Ok(())
    }

    /// Process a TLS-encoded Commit message from the peer and merge it.
    pub fn process_incoming_commit(&mut self, commit: &[u8]) -> Result<()> {
        let msg_in = MlsMessageIn::tls_deserialize_exact(commit).map_err(|e| {
            CoreError::from(MlsErrorKind::Other(format!(
                "mls: process_commit: deserialize: {e}"
            )))
        })?;
        let protocol_message: ProtocolMessage = match msg_in.extract() {
            MlsMessageBodyIn::PublicMessage(pm) => pm.into(),
            MlsMessageBodyIn::PrivateMessage(pm) => pm.into(),
            _ => {
                return Err(
                    MlsErrorKind::Other("mls: process_commit: wrong message type".into()).into(),
                );
            }
        };

        let processed = self
            .inner
            .process_message(self.provider.as_openmls(), protocol_message)
            .map_err(|e| {
                CoreError::from(MlsErrorKind::Other(format!(
                    "mls: authentication failed: {e:?}"
                )))
            })?;

        match processed.into_content() {
            ProcessedMessageContent::StagedCommitMessage(staged) => self.merge_staged(*staged),
            _ => {
                Err(MlsErrorKind::Other("mls: process_commit: not a Commit message".into()).into())
            }
        }
    }

    /// Ratchet our leaf key via a self-update Commit (PCS). Returns the TLS-encoded Commit.
    pub fn advance_epoch(&mut self) -> Result<Vec<u8>> {
        if !self.state.can_send() {
            return Err(MlsErrorKind::Other(format!(
                "mls: advance_epoch: invalid state {:?}",
                self.state
            ))
            .into());
        }

        let signer = load_signer(&self.provider, &own_public_key(&self.inner)?)?;
        let bundle = self
            .inner
            .self_update(
                self.provider.as_openmls(),
                &signer,
                LeafNodeParameters::default(),
            )
            .map_err(|e| {
                CoreError::from(MlsErrorKind::Other(format!("mls: self_update: {e:?}")))
            })?;

        let (commit_out, _welcome_opt, _group_info_opt) = bundle.into_messages();

        self.inner
            .merge_pending_commit(self.provider.as_openmls())
            .map_err(|e| {
                CoreError::from(MlsErrorKind::Other(format!(
                    "mls: merge_pending_commit: {e:?}"
                )))
            })?;

        let bytes = commit_out.tls_serialize_detached().map_err(|e| {
            CoreError::from(MlsErrorKind::Other(format!(
                "mls: advance_epoch: serialize: {e}"
            )))
        })?;
        self.state = GroupState::Active {
            epoch: self.inner.epoch().as_u64(),
        };
        Ok(bytes)
    }

    /// Persist current state. Writes the `(group_id, state_blob, epoch)`
    /// row via `MlsGroupRepo::put`. `state_blob` is the ciborium-encoded
    /// provider snapshot (HashMap of all OpenMLS internal state).
    pub fn save(&self, repo: &MlsGroupRepo<'_>) -> Result<()> {
        let blob = self.provider.snapshot()?;
        repo.put(&self.id.0, &blob, self.inner.epoch().as_u64())
    }

    /// Transactional companion to [`save`](Self::save). Writes the MLS
    /// snapshot inside the caller's `tx` without opening a new pool
    /// transaction. Use this from `daemon::dispatch::send_message` and
    /// `daemon::inbound::dispatch_for_group` so the advanced MLS ratchet
    /// and the message-row / outbox insert commit atomically.
    pub(crate) fn save_in_tx(
        &self,
        repo: &MlsGroupRepo<'_>,
        tx: &rusqlite::Transaction<'_>,
    ) -> Result<()> {
        let blob = self.provider.snapshot()?;
        repo.put_in_tx(tx, &self.id.0, &blob, self.inner.epoch().as_u64())
    }

    /// Restore from persisted state. Returns `None` if `group_id` is unknown.
    pub fn load(group_id: &GroupId, repo: &MlsGroupRepo<'_>) -> Result<Option<Self>> {
        let Some(blob) = repo.get(&group_id.0)? else {
            return Ok(None);
        };
        let provider = MlsProvider::load(&blob)?;
        let openmls_gid = openmls::prelude::GroupId::from_slice(&group_id.0);
        let inner = openmls::group::MlsGroup::load(provider.as_openmls().storage(), &openmls_gid)
            .map_err(|e| CoreError::from(MlsErrorKind::Other(format!("mls: load: {e:?}"))))?
            .ok_or_else(|| CoreError::from(MlsErrorKind::GroupCorrupt))?;

        Ok(Some(Self {
            id: group_id.clone(),
            state: GroupState::Active {
                epoch: inner.epoch().as_u64(),
            },
            provider,
            inner,
        }))
    }

    /// Returns the group identifier.
    #[must_use]
    pub fn id(&self) -> &GroupId {
        &self.id
    }

    /// Returns the current MLS epoch number.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.inner.epoch().as_u64()
    }

    /// Returns the current group state.
    #[must_use]
    pub fn state(&self) -> &GroupState {
        &self.state
    }

    /// The Ed25519 identity of the OTHER member of this 2-member group, as a
    /// [`PublicKey`]. The MLS signature key equals the Ed25519 identity under our
    /// ciphersuite (`…_Ed25519`). Errors if the group is not exactly 2-member or
    /// no distinct peer is found.
    pub(crate) fn peer_identity(&self) -> Result<crate::identity::PublicKey> {
        // Enforce the 2-member invariant: deriving an identity for an auth
        // decision must not silently pick "the first non-self member" of a
        // larger group.
        if self.inner.members().count() != 2 {
            return Err(CoreError::from(MlsErrorKind::Other(
                "mls: peer_identity: group is not exactly 2-member".into(),
            )));
        }
        let own_leaf = self.inner.own_leaf_node().ok_or_else(|| {
            CoreError::from(MlsErrorKind::Other(
                "mls: peer_identity: no own leaf".into(),
            ))
        })?;
        let own = own_leaf.signature_key().as_slice();
        for m in self.inner.members() {
            let sk = m.signature_key.as_slice();
            if sk != own {
                let bytes: [u8; 32] = sk.try_into().map_err(|_| {
                    CoreError::from(MlsErrorKind::Other(
                        "mls: peer_identity: signature key not 32 bytes".into(),
                    ))
                })?;
                return Ok(crate::identity::PublicKey(bytes));
            }
        }
        Err(CoreError::from(MlsErrorKind::Other(
            "mls: peer_identity: no distinct peer member".into(),
        )))
    }
}

/// Re-materialize the `SignatureKeyPair` stored in this provider by
/// public key. OpenMLS state-advancing calls require a `&impl Signer`;
/// we reconstruct it from storage rather than threading a long-lived
/// reference through `Group`.
fn load_signer(
    provider: &MlsProvider,
    public_key: &[u8],
) -> Result<openmls_basic_credential::SignatureKeyPair> {
    openmls_basic_credential::SignatureKeyPair::read(
        provider.as_openmls().storage(),
        public_key,
        openmls::prelude::SignatureScheme::ED25519,
    )
    .ok_or_else(|| {
        CoreError::from(MlsErrorKind::Other(
            "mls: load_signer: missing signer".into(),
        ))
    })
}

/// Extract our own signature public key from the group.
fn own_public_key(group: &openmls::group::MlsGroup) -> Result<Vec<u8>> {
    let own_leaf = group.own_leaf_node().ok_or_else(|| {
        CoreError::from(MlsErrorKind::Other(
            "mls: own_public_key: no own leaf".into(),
        ))
    })?;
    Ok(own_leaf.signature_key().as_slice().to_vec())
}

/// Derive a per-invite-unique external PSK id. `label` distinguishes the two
/// PSKs carried in one genesis Commit ("invite" vs "htransport"); `kp_ref`
/// (the invite's 32-byte KeyPackageRef) makes every invite's PSK ids unique so
/// registering one never overwrites another invite's (ADR 0009, fixes T2-8).
///
/// The id is an unprefixed concatenation, which is unambiguous ONLY because
/// `label` is a fixed closed set of constants (`b"invite"`, `b"htransport"`)
/// and `kp_ref` is always the fixed 32-byte trailing segment. Do not pass a
/// variable-length or caller-controlled `label`.
fn psk_id(label: &[u8], kp_ref: &[u8; 32]) -> PreSharedKeyId {
    // id = "skattr-" ++ label ++ "-v1" ++ kp_ref
    let mut id = Vec::with_capacity(7 + label.len() + 3 + 32);
    id.extend_from_slice(b"skattr-");
    id.extend_from_slice(label);
    id.extend_from_slice(b"-v1");
    id.extend_from_slice(kp_ref);
    PreSharedKeyId::external(id, kp_ref.to_vec())
}

/// Register an external PSK secret under `psk_id(label, kp_ref)`. Returns the
/// `PreSharedKeyId` that the caller embeds in the Commit's PSK proposal
/// (committer side) or that gets resolved from the Welcome (joiner side).
fn register_psk(
    provider: &MlsProvider,
    label: &[u8],
    kp_ref: &[u8; 32],
    secret: &[u8; 32],
) -> Result<PreSharedKeyId> {
    let id = psk_id(label, kp_ref);
    id.store(provider.as_openmls(), secret)
        .map_err(|e| CoreError::from(MlsErrorKind::Other(format!("mls: psk register: {e:?}"))))?;
    Ok(id)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::storage::Pool;

    fn alice() -> IdentityKey {
        IdentityKey::generate().unwrap()
    }

    #[test]
    fn create_solo_is_active_at_epoch_0() {
        let id = alice();
        let g = Group::create_solo(&id, None, None, MlsProvider::new()).unwrap();
        assert_eq!(g.epoch(), 0);
        assert!(matches!(g.state(), GroupState::Active { epoch: 0 }));
        assert!(!g.id().0.is_empty(), "group id must be set");
    }

    #[test]
    fn save_load_round_trip_preserves_epoch_and_id() {
        let pool = Pool::in_memory();
        let repo = MlsGroupRepo::new(&pool);
        let id = alice();

        let g = Group::create_solo(&id, None, None, MlsProvider::new()).unwrap();
        let gid = g.id().clone();
        g.save(&repo).unwrap();

        drop(g);

        let restored = Group::load(&gid, &repo).unwrap().expect("must load");
        assert_eq!(restored.epoch(), 0);
        assert_eq!(restored.id(), &gid);
    }

    #[test]
    fn load_missing_group_returns_none() {
        let pool = Pool::in_memory();
        let repo = MlsGroupRepo::new(&pool);
        let gid = GroupId(vec![0x99; 32]);
        assert!(Group::load(&gid, &repo).unwrap().is_none());
    }

    #[test]
    fn load_rejects_corrupt_blob() {
        let pool = Pool::in_memory();
        let repo = MlsGroupRepo::new(&pool);
        let gid = GroupId(vec![0xAA; 32]);
        repo.put(&gid.0, &[0xFFu8; 32], 7).unwrap();
        let err = match Group::load(&gid, &repo) {
            Ok(_) => panic!("garbage must fail"),
            Err(e) => e,
        };
        match err {
            CoreError::Mls(MlsErrorKind::GroupCorrupt) | CoreError::Mls(MlsErrorKind::Other(_)) => {
            }
            other => panic!("expected CoreError::Mls, got {other:?}"),
        }
    }

    #[test]
    fn add_member_emits_welcome_and_commit_and_bumps_epoch_to_1() {
        let pool = Pool::in_memory();
        let kp_repo = crate::storage::KeyPackageRepo::new(&pool);

        let alice_id = alice();
        let bob_id = IdentityKey::generate().unwrap();
        let bob_provider = MlsProvider::new();
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo).unwrap();

        let mut alice = Group::create_solo(&alice_id, None, None, MlsProvider::new()).unwrap();
        assert_eq!(alice.epoch(), 0);

        let (welcome, commit) = alice.add_member(&bob_kp, None, None).unwrap();
        assert!(!welcome.is_empty());
        assert!(!commit.is_empty());
        assert_eq!(alice.epoch(), 1);
        // Genesis committer stays PendingJoin until the peer Acks (#93).
        assert!(matches!(alice.state(), GroupState::PendingJoin));
    }

    #[test]
    fn add_member_leaves_pending_join_until_set_active() {
        let pool = Pool::in_memory();
        let kp_repo = crate::storage::KeyPackageRepo::new(&pool);

        let alice_id = alice();
        let bob_id = IdentityKey::generate().unwrap();
        let bob_provider = MlsProvider::new();
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo).unwrap();

        let mut g = Group::create_solo(&alice_id, None, None, MlsProvider::new()).unwrap();
        let _ = g.add_member(&bob_kp, None, None).unwrap();

        // Invitee/committer is NOT paired until the peer Acks the Welcome.
        assert!(
            matches!(g.state(), GroupState::PendingJoin),
            "genesis must be PendingJoin"
        );
        assert!(
            !g.state().can_send(),
            "must not be able to send app frames while pending"
        );

        // Ack transition is a CAS.
        assert!(
            g.set_active(),
            "first set_active flips PendingJoin -> Active"
        );
        assert!(matches!(g.state(), GroupState::Active { .. }));
        assert!(g.state().can_send());
        assert!(!g.set_active(), "second set_active is a no-op");
    }

    #[test]
    fn join_from_welcome_lands_at_epoch_1_with_matching_group_id() {
        let pool = Pool::in_memory();
        let kp_repo = crate::storage::KeyPackageRepo::new(&pool);

        let alice_id = alice();
        let bob_id = IdentityKey::generate().unwrap();
        let bob_provider = MlsProvider::new();
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo).unwrap();

        let mut alice = Group::create_solo(&alice_id, None, None, MlsProvider::new()).unwrap();
        let (welcome, _commit) = alice.add_member(&bob_kp, None, None).unwrap();

        let bob = Group::join_from_welcome(&bob_id, &welcome, None, None, bob_provider).unwrap();

        assert_eq!(bob.epoch(), 1);
        assert_eq!(bob.id(), alice.id(), "both sides see the same group id");
        assert!(matches!(bob.state(), GroupState::Active { epoch: 1 }));
    }

    #[test]
    fn peer_identity_returns_other_members_identity() {
        let pool = Pool::in_memory();
        let kp_repo = crate::storage::KeyPackageRepo::new(&pool);

        let alice_id = alice();
        let bob_id = IdentityKey::generate().unwrap();
        let bob_provider = MlsProvider::new();
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo).unwrap();

        let mut alice = Group::create_solo(&alice_id, None, None, MlsProvider::new()).unwrap();
        let (welcome, _commit) = alice.add_member(&bob_kp, None, None).unwrap();
        let bob = Group::join_from_welcome(&bob_id, &welcome, None, None, bob_provider).unwrap();

        // From alice's view, the distinct peer is bob.
        assert_eq!(
            alice.peer_identity().unwrap(),
            bob_id.public(),
            "alice.peer_identity must be bob"
        );
        // From bob's view, the distinct peer is alice.
        assert_eq!(
            bob.peer_identity().unwrap(),
            alice_id.public(),
            "bob.peer_identity must be alice"
        );
    }

    fn pair_no_psk() -> (Group, Group) {
        let pool = Pool::in_memory();
        let kp_repo = crate::storage::KeyPackageRepo::new(&pool);
        let alice_id = alice();
        let bob_id = IdentityKey::generate().unwrap();
        let bob_provider = MlsProvider::new();
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo).unwrap();
        let mut alice = Group::create_solo(&alice_id, None, None, MlsProvider::new()).unwrap();
        let (welcome, _commit) = alice.add_member(&bob_kp, None, None).unwrap();
        let bob = Group::join_from_welcome(&bob_id, &welcome, None, None, bob_provider).unwrap();
        // Simulate the Welcome-Ack: the committer becomes Active once the peer
        // has joined, so this helper yields a fully-paired, sendable pair (#93).
        alice.set_active();
        (alice, bob)
    }

    fn test_envelope(text: &str) -> Envelope {
        use crate::envelope::{Kind, MessageId};
        Envelope {
            v: 1,
            id: MessageId::generate(),
            ts: 0,
            reply_to: None,
            kind: Kind::Text {
                body: text.to_string(),
            },
        }
    }

    #[test]
    fn bidirectional_encrypt_decrypt() {
        let (mut alice, mut bob) = pair_no_psk();

        let msg_a = test_envelope("hi from alice");
        let ct_a = alice.encrypt(&msg_a).unwrap();
        let got_a = bob.decrypt(&ct_a).unwrap().expect("app message");
        assert_eq!(format!("{got_a:?}"), format!("{msg_a:?}"));

        let msg_b = test_envelope("hi from bob");
        let ct_b = bob.encrypt(&msg_b).unwrap();
        let got_b = alice.decrypt(&ct_b).unwrap().expect("app message");
        assert_eq!(format!("{got_b:?}"), format!("{msg_b:?}"));
    }

    fn pair_with_psk(psk_alice: [u8; 32], psk_bob: [u8; 32]) -> Result<(Group, Group)> {
        let pool = Pool::in_memory();
        let kp_repo = crate::storage::KeyPackageRepo::new(&pool);
        let alice_id = alice();
        let bob_id = IdentityKey::generate().unwrap();
        let bob_provider = MlsProvider::new();
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo)?;
        // Both sides derive the invite PSK id from the same KeyPackageRef.
        let kp_ref = crate::mls::key_package::key_package_ref(&bob_kp)?;
        let mut alice = Group::create_solo(
            &alice_id,
            Some((&kp_ref, &psk_alice)),
            None,
            MlsProvider::new(),
        )?;
        let (welcome, _commit) = alice.add_member(&bob_kp, Some((&kp_ref, &psk_alice)), None)?;
        let bob = Group::join_from_welcome(
            &bob_id,
            &welcome,
            Some((&kp_ref, &psk_bob)),
            None,
            bob_provider,
        )?;
        // Simulate the Welcome-Ack so the committer is Active (sendable) (#93).
        alice.set_active();
        Ok((alice, bob))
    }

    #[test]
    fn external_psk_match_succeeds() {
        let psk = [0xEEu8; 32];
        let (alice, bob) = pair_with_psk(psk, psk).unwrap();
        assert_eq!(alice.epoch(), 1);
        assert_eq!(bob.epoch(), 1);
        assert_eq!(alice.id(), bob.id());
    }

    #[test]
    fn external_psk_mismatch_fails_on_bob_join() {
        let result = pair_with_psk([0xAAu8; 32], [0xBBu8; 32]);
        let err = match result {
            Ok(_) => panic!("mismatched PSK must fail"),
            Err(e) => e,
        };
        match err {
            CoreError::Mls(MlsErrorKind::Other(s)) => {
                assert!(
                    s.contains("external PSK mismatch")
                        || s.contains("authentication")
                        || s.contains("welcome process"),
                    "unexpected message: {s}"
                );
            }
            other => panic!("expected CoreError::Mls(Other(_)), got {other:?}"),
        }
    }

    #[test]
    fn genesis_two_psk_commit_round_trips_and_binds() {
        // Bob (committer) builds the genesis Commit with BOTH the invite PSK and
        // h_transport, keyed by the same KeyPackageRef. Alice joins with the same
        // pair; the group binds and a round-trip message decrypts.
        let pool = Pool::in_memory();
        let kp_repo = crate::storage::KeyPackageRepo::new(&pool);
        let bob_id = alice();
        let alice_id = IdentityKey::generate().unwrap();
        let alice_provider = MlsProvider::new();
        let alice_kp = KeyPackage::generate(&alice_id, &alice_provider, &kp_repo).unwrap();
        let kp_ref = crate::mls::key_package::key_package_ref(&alice_kp).unwrap();

        let inv = [0x11u8; 32];
        let ht = [0x22u8; 32];

        let mut bob = Group::create_solo(
            &bob_id,
            Some((&kp_ref, &inv)),
            Some((&kp_ref, &ht)),
            MlsProvider::new(),
        )
        .unwrap();
        let (welcome, _commit) = bob
            .add_member(&alice_kp, Some((&kp_ref, &inv)), Some((&kp_ref, &ht)))
            .unwrap();

        let mut alice = Group::join_from_welcome(
            &alice_id,
            &welcome,
            Some((&kp_ref, &inv)),
            Some((&kp_ref, &ht)),
            alice_provider,
        )
        .unwrap();

        assert_eq!(bob.epoch(), 1);
        assert_eq!(alice.epoch(), 1);
        assert_eq!(bob.id(), alice.id());

        // Committer stays PendingJoin until the peer Acks (#93); simulate the
        // Ack so bob can send in the round-trip below.
        bob.set_active();

        // Round-trip both directions.
        let env = test_envelope("bound hello");
        let ct = bob.encrypt(&env).unwrap();
        let got = alice.decrypt(&ct).unwrap().expect("app message");
        assert_eq!(format!("{got:?}"), format!("{env:?}"));
    }

    #[test]
    fn wrong_h_transport_rejects_join() {
        // Alice joins with a DIFFERENT h_transport secret than Bob proposed;
        // OpenMLS cannot resolve the binding PSK and the join fails.
        let pool = Pool::in_memory();
        let kp_repo = crate::storage::KeyPackageRepo::new(&pool);
        let bob_id = alice();
        let alice_id = IdentityKey::generate().unwrap();
        let alice_provider = MlsProvider::new();
        let alice_kp = KeyPackage::generate(&alice_id, &alice_provider, &kp_repo).unwrap();
        let kp_ref = crate::mls::key_package::key_package_ref(&alice_kp).unwrap();

        let inv = [0x11u8; 32];
        let ht_bob = [0x22u8; 32];
        let ht_alice = [0x33u8; 32]; // mismatch

        let mut bob = Group::create_solo(
            &bob_id,
            Some((&kp_ref, &inv)),
            Some((&kp_ref, &ht_bob)),
            MlsProvider::new(),
        )
        .unwrap();
        let (welcome, _commit) = bob
            .add_member(&alice_kp, Some((&kp_ref, &inv)), Some((&kp_ref, &ht_bob)))
            .unwrap();

        let result = Group::join_from_welcome(
            &alice_id,
            &welcome,
            Some((&kp_ref, &inv)),
            Some((&kp_ref, &ht_alice)),
            alice_provider,
        );
        assert!(
            result.is_err(),
            "join must fail when h_transport does not match"
        );
    }

    #[test]
    fn distinct_kp_refs_yield_distinct_psk_ids() {
        let ref_a = [1u8; 32];
        let ref_b = [2u8; 32];

        // Different kp_ref -> different id (and different nonce).
        let id_a = psk_id(b"invite", &ref_a);
        let id_b = psk_id(b"invite", &ref_b);
        assert_ne!(
            id_a.tls_serialize_detached().unwrap(),
            id_b.tls_serialize_detached().unwrap(),
            "distinct kp_refs must yield distinct invite PSK ids"
        );

        // Different label, same kp_ref -> different id (invite vs htransport).
        let id_inv = psk_id(b"invite", &ref_a);
        let id_ht = psk_id(b"htransport", &ref_a);
        assert_ne!(
            id_inv.tls_serialize_detached().unwrap(),
            id_ht.tls_serialize_detached().unwrap(),
            "invite and htransport PSK ids must differ for the same kp_ref"
        );
    }

    #[test]
    fn no_psk_path_still_works() {
        let pool = Pool::in_memory();
        let kp_repo = crate::storage::KeyPackageRepo::new(&pool);
        let alice_id = alice();
        let bob_id = IdentityKey::generate().unwrap();
        let bob_provider = MlsProvider::new();
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo).unwrap();
        let mut alice = Group::create_solo(&alice_id, None, None, MlsProvider::new()).unwrap();
        let (welcome, _commit) = alice.add_member(&bob_kp, None, None).unwrap();
        let bob = Group::join_from_welcome(&bob_id, &welcome, None, None, bob_provider).unwrap();
        assert_eq!(alice.epoch(), 1);
        assert_eq!(bob.epoch(), 1);
    }

    #[test]
    fn advance_epoch_bumps_epoch_and_both_sides_ratchet() {
        let (mut alice, mut bob) = pair_no_psk();
        assert_eq!(alice.epoch(), 1);
        assert_eq!(bob.epoch(), 1);

        let commit = alice.advance_epoch().unwrap();
        assert_eq!(alice.epoch(), 2);

        bob.process_incoming_commit(&commit).unwrap();
        assert_eq!(bob.epoch(), 2);

        // Both sides can still encrypt/decrypt at epoch 2.
        let env = test_envelope("post-PCS");
        let ct = alice.encrypt(&env).unwrap();
        let got = bob.decrypt(&ct).unwrap().expect("app message");
        assert_eq!(format!("{got:?}"), format!("{env:?}"));
    }

    #[test]
    fn decrypt_merges_inbound_commit_instead_of_erroring() {
        // T2-2 (defensive): an inbound Commit fed to `decrypt` must be merged
        // (advancing the epoch) and return `Ok(None)` — NOT error. (The old
        // behavior errored on the StagedCommitMessage arm.)
        let (mut alice, mut bob) = pair_no_psk();
        assert_eq!(alice.epoch(), 1);
        assert_eq!(bob.epoch(), 1);

        // Bob produces a real inbound Commit via a self-update.
        let commit = bob.advance_epoch().unwrap();
        assert_eq!(bob.epoch(), 2);

        // Alice receives it through `decrypt`: merged, no payload, epoch advances.
        let out = alice.decrypt(&commit).unwrap();
        assert!(out.is_none(), "a Commit carries no application payload");
        assert_eq!(
            alice.epoch(),
            2,
            "decrypt must advance the epoch on a Commit"
        );

        // A subsequent application message still round-trips at the new epoch.
        let env = test_envelope("after merged commit");
        let ct = bob.encrypt(&env).unwrap();
        let got = alice.decrypt(&ct).unwrap().expect("app message");
        assert_eq!(format!("{got:?}"), format!("{env:?}"));
    }

    #[test]
    fn add_member_rejects_when_already_2_member() {
        let pool = Pool::in_memory();
        let kp_repo = crate::storage::KeyPackageRepo::new(&pool);
        let (mut alice, _bob) = pair_no_psk();

        let charlie_id = IdentityKey::generate().unwrap();
        let charlie_provider = MlsProvider::new();
        let charlie_kp = KeyPackage::generate(&charlie_id, &charlie_provider, &kp_repo).unwrap();

        let err = match alice.add_member(&charlie_kp, None, None) {
            Ok(_) => panic!("must reject 3rd"),
            Err(e) => e,
        };
        match err {
            CoreError::Mls(MlsErrorKind::Other(s)) => {
                assert!(s.contains("already 2-member"), "got: {s}")
            }
            other => panic!("expected CoreError::Mls(Other(_)), got {other:?}"),
        }
    }

    #[test]
    fn encrypt_rejects_corrupt_state() {
        let (mut alice, _bob) = pair_no_psk();
        // Force corrupt state directly (private field access via the
        // same module).
        alice.state = GroupState::Corrupt {
            reason: "forced for test".into(),
        };
        let env = test_envelope("should fail");
        let err = match alice.encrypt(&env) {
            Ok(_) => panic!("corrupt must reject"),
            Err(e) => e,
        };
        match err {
            CoreError::Mls(MlsErrorKind::Other(s)) => {
                assert!(s.starts_with("mls: encrypt: invalid state"))
            }
            other => panic!("expected CoreError::Mls(Other(_)), got {other:?}"),
        }
    }

    #[test]
    fn advance_epoch_rejects_corrupt_state() {
        let (mut alice, _bob) = pair_no_psk();
        alice.state = GroupState::Corrupt {
            reason: "forced".into(),
        };
        let err = match alice.advance_epoch() {
            Ok(_) => panic!("corrupt must reject"),
            Err(e) => e,
        };
        match err {
            CoreError::Mls(MlsErrorKind::Other(s)) => {
                assert!(s.starts_with("mls: advance_epoch: invalid state"))
            }
            other => panic!("expected CoreError::Mls(Other(_)), got {other:?}"),
        }
    }

    #[test]
    fn save_in_tx_rolls_back_on_abort() {
        let pool = Pool::in_memory();
        let repo = MlsGroupRepo::new(&pool);
        let id = alice();
        let group = Group::create_solo(&id, None, None, MlsProvider::new()).unwrap();

        // Run save_in_tx inside a tx we explicitly roll back.
        let result: crate::error::Result<()> = pool.transaction(|tx| {
            group.save_in_tx(&repo, tx).unwrap();
            // Sanity-check: the row is visible inside this tx.
            let n: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM mls_groups WHERE group_id = ?1",
                    rusqlite::params![&group.id().0[..]],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "snapshot must be visible inside the tx");
            // Force rollback via Err.
            Err(crate::error::CoreError::Storage(
                crate::storage::StorageErrorKind::Other("rollback test".into()),
            ))
        });
        assert!(result.is_err(), "transaction must have returned Err");

        // After rollback the row must not exist.
        let n: i64 = pool
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM mls_groups WHERE group_id = ?1",
                    rusqlite::params![&group.id().0[..]],
                    |r| r.get(0),
                )
                .map_err(|e| {
                    crate::error::CoreError::Storage(crate::storage::StorageErrorKind::Other(
                        e.to_string(),
                    ))
                })
            })
            .unwrap();
        assert_eq!(n, 0, "tx rollback must leave mls_groups empty");
    }
}
