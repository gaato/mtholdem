//! Target-neutral byte adapters for `mtholdem-peer`.
//!
//! This crate is the shared boundary for CLI, WASM, C ABI, and engine wrappers.  It
//! converts typed [`mtholdem_peer::HostEffect`] values into stable byte-oriented
//! events while leaving protocol validation, transcript ordering, and game rules in
//! `mtholdem` and `mtholdem-peer`.

#![forbid(unsafe_code)]

use mental_card_core::error::{Error, ErrorCategory, Result, protocol};
use mtholdem::betting::{BetAction, LegalActions};
use mtholdem::checkpoint::{CertifiedCheckpoint, CheckpointState};
use mtholdem::recovery::RecoveryBundle;
use mtholdem::transcript::TranscriptAnchor;
use mtholdem::wire::{ShowdownBundle, SignedPrivateReveal, SignedPublicMessage};
use mtholdem::{Chips, HandNumber, Seat};
use mtholdem_peer::{FinishedHand, HostEffect, PeerPhase, PeerStatus};

/// Major version of the stable host boundary.
pub const ABI_VERSION_MAJOR: u32 = 0;

/// Minor version of the stable host boundary.
pub const ABI_VERSION_MINOR: u32 = 1;

/// Patch version of the stable host boundary.
pub const ABI_VERSION_PATCH: u32 = 0;

/// Error codes shared by CLI, WASM, and C ABI wrappers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
pub enum BoundaryErrorCode {
    /// Operation completed successfully.
    Success = 0,
    /// A required pointer or handle was null at a host boundary.
    NullPointer = 1,
    /// The caller-provided output buffer was too small.
    BufferTooSmall = 2,
    /// A primitive argument was outside its valid domain.
    InvalidArgument = 3,
    /// Session or table configuration was invalid.
    Configuration = 10,
    /// Wire bytes were malformed, truncated, non-canonical, or too large.
    Codec = 11,
    /// Signature, identity, sender, or replay validation failed.
    Authentication = 12,
    /// Cryptographic proof, encrypted deck, or reveal share validation failed.
    Cryptography = 13,
    /// Betting or settlement rules rejected the requested transition.
    Rules = 14,
    /// A valid artifact was unexpected in the current protocol phase.
    Protocol = 15,
    /// Checkpoint or recovery evidence validation failed.
    Recovery = 16,
    /// An internal invariant failed before a typed error could be reported.
    Internal = 255,
}

impl From<ErrorCategory> for BoundaryErrorCode {
    fn from(value: ErrorCategory) -> Self {
        match value {
            ErrorCategory::Configuration => Self::Configuration,
            ErrorCategory::Codec => Self::Codec,
            ErrorCategory::Authentication => Self::Authentication,
            ErrorCategory::Cryptography => Self::Cryptography,
            ErrorCategory::Rules => Self::Rules,
            ErrorCategory::Protocol => Self::Protocol,
            ErrorCategory::Recovery => Self::Recovery,
            _ => Self::Internal,
        }
    }
}

impl From<&Error> for BoundaryErrorCode {
    fn from(value: &Error) -> Self {
        value.category().into()
    }
}

/// A compact error value suitable for non-Rust wrappers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundaryError {
    code: BoundaryErrorCode,
    message: String,
}

impl BoundaryError {
    /// Creates an error from a workspace protocol error.
    #[must_use]
    pub fn from_error(error: &Error) -> Self {
        Self {
            code: error.into(),
            message: error.message().to_owned(),
        }
    }

    /// Returns the stable error code.
    #[must_use]
    pub const fn code(&self) -> BoundaryErrorCode {
        self.code
    }

    /// Returns a redacted human-readable summary.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Host-boundary phase code for a peer session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum BoundaryPeerPhase {
    /// Waiting for key announcements.
    KeySetup = 1,
    /// Waiting for shuffle contributions.
    Shuffle = 2,
    /// Delivering private reveals and receipt commitments.
    PrivateReveal = 3,
    /// All hole-card receipts are ready and betting can start.
    HoleCardsReady = 4,
    /// A betting action is expected.
    Betting = 5,
    /// Public board-card reveal shares are expected.
    BoardReveal = 6,
    /// Showdown side artifacts are expected.
    Showdown = 7,
    /// The hand is complete.
    Complete = 8,
}

impl From<PeerPhase> for BoundaryPeerPhase {
    fn from(value: PeerPhase) -> Self {
        match value {
            PeerPhase::KeySetup => Self::KeySetup,
            PeerPhase::Shuffle => Self::Shuffle,
            PeerPhase::PrivateReveal => Self::PrivateReveal,
            PeerPhase::HoleCardsReady => Self::HoleCardsReady,
            PeerPhase::Betting(_) => Self::Betting,
            PeerPhase::BoardReveal(_) => Self::BoardReveal,
            PeerPhase::Showdown => Self::Showdown,
            PeerPhase::Complete => Self::Complete,
        }
    }
}

/// A transcript position encoded without Rust-only types.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoundaryTranscriptAnchor {
    next_sequence: u64,
    digest: [u8; 32],
}

impl BoundaryTranscriptAnchor {
    /// Returns the next expected public sequence number.
    #[must_use]
    pub const fn next_sequence(self) -> u64 {
        self.next_sequence
    }

    /// Returns the predecessor digest for the next public message.
    #[must_use]
    pub const fn digest(self) -> [u8; 32] {
        self.digest
    }
}

impl From<TranscriptAnchor> for BoundaryTranscriptAnchor {
    fn from(value: TranscriptAnchor) -> Self {
        Self {
            next_sequence: value.next_sequence(),
            digest: value.digest().to_bytes(),
        }
    }
}

/// Read-only peer status encoded for target wrappers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoundaryPeerStatus {
    phase: BoundaryPeerPhase,
    local_seat: u8,
    next_public_sender: Option<u8>,
    transcript_anchor: BoundaryTranscriptAnchor,
}

impl BoundaryPeerStatus {
    /// Returns the current peer phase.
    #[must_use]
    pub const fn phase(self) -> BoundaryPeerPhase {
        self.phase
    }

    /// Returns the local peer seat.
    #[must_use]
    pub const fn local_seat(self) -> u8 {
        self.local_seat
    }

    /// Returns the next public sender, if one is expected.
    #[must_use]
    pub const fn next_public_sender(self) -> Option<u8> {
        self.next_public_sender
    }

    /// Returns the current transcript anchor.
    #[must_use]
    pub const fn transcript_anchor(self) -> BoundaryTranscriptAnchor {
        self.transcript_anchor
    }
}

impl From<PeerStatus> for BoundaryPeerStatus {
    fn from(value: PeerStatus) -> Self {
        Self {
            phase: value.phase().into(),
            local_seat: value.local_seat().to_u8(),
            next_public_sender: value.next_public_sender().map(Seat::to_u8),
            transcript_anchor: value.transcript_anchor().into(),
        }
    }
}

/// Host-boundary betting action kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum BoundaryActionKind {
    /// Surrender the hand.
    Fold = 1,
    /// Decline to bet when no call is required.
    Check = 2,
    /// Match the current bet.
    Call = 3,
    /// Raise to the supplied absolute chip amount.
    RaiseTo = 4,
    /// Commit the remaining stack.
    AllIn = 5,
}

/// A betting action encoded without Rust enums carrying data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoundaryAction {
    kind: BoundaryActionKind,
    raise_to_units: u64,
}

impl BoundaryAction {
    /// Creates a host-boundary betting action.
    #[must_use]
    pub const fn new(kind: BoundaryActionKind, raise_to_units: u64) -> Self {
        Self {
            kind,
            raise_to_units,
        }
    }

    /// Returns the action kind.
    #[must_use]
    pub const fn kind(self) -> BoundaryActionKind {
        self.kind
    }

    /// Returns the raise-to amount for [`BoundaryActionKind::RaiseTo`].
    #[must_use]
    pub const fn raise_to_units(self) -> u64 {
        self.raise_to_units
    }

    /// Converts this boundary value into the canonical rules-engine action.
    #[must_use]
    pub fn to_bet_action(self) -> BetAction {
        match self.kind {
            BoundaryActionKind::Fold => BetAction::Fold,
            BoundaryActionKind::Check => BetAction::Check,
            BoundaryActionKind::Call => BetAction::Call,
            BoundaryActionKind::RaiseTo => {
                BetAction::RaiseTo(Chips::from_units(self.raise_to_units))
            }
            BoundaryActionKind::AllIn => BetAction::AllIn,
        }
    }
}

impl From<BetAction> for BoundaryAction {
    fn from(value: BetAction) -> Self {
        match value {
            BetAction::Fold => Self::new(BoundaryActionKind::Fold, 0),
            BetAction::Check => Self::new(BoundaryActionKind::Check, 0),
            BetAction::Call => Self::new(BoundaryActionKind::Call, 0),
            BetAction::RaiseTo(amount) => Self::new(BoundaryActionKind::RaiseTo, amount.units()),
            BetAction::AllIn => Self::new(BoundaryActionKind::AllIn, 0),
        }
    }
}

/// Legal betting choices encoded for target wrappers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoundaryLegalActions {
    to_call_units: u64,
    minimum_raise_to_units: Option<u64>,
    maximum_raise_to_units: u64,
    raise_reopened: bool,
    can_check: bool,
}

impl BoundaryLegalActions {
    /// Returns the amount required to call.
    #[must_use]
    pub const fn to_call_units(self) -> u64 {
        self.to_call_units
    }

    /// Returns the minimum full raise target, if one is legal.
    #[must_use]
    pub const fn minimum_raise_to_units(self) -> Option<u64> {
        self.minimum_raise_to_units
    }

    /// Returns the maximum raise target.
    #[must_use]
    pub const fn maximum_raise_to_units(self) -> u64 {
        self.maximum_raise_to_units
    }

    /// Returns whether a full raise is currently reopened.
    #[must_use]
    pub const fn raise_reopened(self) -> bool {
        self.raise_reopened
    }

    /// Returns whether checking is legal.
    #[must_use]
    pub const fn can_check(self) -> bool {
        self.can_check
    }
}

impl From<LegalActions> for BoundaryLegalActions {
    fn from(value: LegalActions) -> Self {
        Self {
            to_call_units: value.to_call().units(),
            minimum_raise_to_units: value.minimum_raise_to().map(Chips::units),
            maximum_raise_to_units: value.maximum_raise_to().units(),
            raise_reopened: value.raise_reopened(),
            can_check: value.can_check(),
        }
    }
}

/// Host effect kind for byte-oriented adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum BoundaryEffectKind {
    /// Broadcast one signed public message.
    BroadcastPublic = 1,
    /// Send one signed private reveal to one recipient.
    SendPrivate = 2,
    /// Persist the transcript anchor and optional checkpoint bytes.
    Persist = 3,
    /// Ask the host for a legal local action.
    RequestAction = 4,
    /// Submit recovery evidence to a mediator.
    SubmitRecovery = 5,
    /// Publish one showdown side artifact.
    PublishShowdownBundle = 6,
    /// Report a final hand result.
    Finished = 7,
}

/// Final hand summary encoded for target wrappers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoundaryFinishedHand {
    hand: u64,
    transcript_anchor: BoundaryTranscriptAnchor,
}

impl BoundaryFinishedHand {
    /// Returns the completed hand number.
    #[must_use]
    pub const fn hand(self) -> u64 {
        self.hand
    }

    /// Returns the final transcript anchor.
    #[must_use]
    pub const fn transcript_anchor(self) -> BoundaryTranscriptAnchor {
        self.transcript_anchor
    }
}

impl From<FinishedHand> for BoundaryFinishedHand {
    fn from(value: FinishedHand) -> Self {
        Self {
            hand: value.hand().to_u64(),
            transcript_anchor: value.transcript_anchor().into(),
        }
    }
}

/// Byte-oriented host effect shared by CLI, WASM, C ABI, and engine wrappers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundaryEffect {
    kind: BoundaryEffectKind,
    seat: Option<u8>,
    legal_actions: Option<BoundaryLegalActions>,
    transcript_anchor: Option<BoundaryTranscriptAnchor>,
    artifact: Vec<u8>,
    checkpoint: Option<Vec<u8>>,
    finished: Option<BoundaryFinishedHand>,
}

impl BoundaryEffect {
    /// Returns the effect kind.
    #[must_use]
    pub const fn kind(&self) -> BoundaryEffectKind {
        self.kind
    }

    /// Returns the relevant seat for send, action, or showdown effects.
    #[must_use]
    pub const fn seat(&self) -> Option<u8> {
        self.seat
    }

    /// Returns legal action metadata for [`BoundaryEffectKind::RequestAction`].
    #[must_use]
    pub const fn legal_actions(&self) -> Option<BoundaryLegalActions> {
        self.legal_actions
    }

    /// Returns the transcript anchor for [`BoundaryEffectKind::Persist`].
    #[must_use]
    pub const fn transcript_anchor(&self) -> Option<BoundaryTranscriptAnchor> {
        self.transcript_anchor
    }

    /// Returns the primary encoded protocol artifact.
    #[must_use]
    pub fn artifact(&self) -> &[u8] {
        &self.artifact
    }

    /// Returns checkpoint bytes for [`BoundaryEffectKind::Persist`], when present.
    #[must_use]
    pub fn checkpoint(&self) -> Option<&[u8]> {
        self.checkpoint.as_deref()
    }

    /// Returns the final hand summary for [`BoundaryEffectKind::Finished`].
    #[must_use]
    pub const fn finished(&self) -> Option<BoundaryFinishedHand> {
        self.finished
    }

    fn public(message: SignedPublicMessage) -> Result<Self> {
        Ok(Self {
            kind: BoundaryEffectKind::BroadcastPublic,
            seat: None,
            legal_actions: None,
            transcript_anchor: None,
            artifact: message.to_bytes()?,
            checkpoint: None,
            finished: None,
        })
    }
}

/// Converts typed peer-session effects into host-boundary effects.
pub fn encode_effect(effect: HostEffect) -> Result<BoundaryEffect> {
    match effect {
        HostEffect::BroadcastPublic(message) => BoundaryEffect::public(message),
        HostEffect::SendPrivate { recipient, message } => Ok(BoundaryEffect {
            kind: BoundaryEffectKind::SendPrivate,
            seat: Some(recipient.to_u8()),
            legal_actions: None,
            transcript_anchor: None,
            artifact: message.to_bytes(),
            checkpoint: None,
            finished: None,
        }),
        HostEffect::Persist {
            transcript_anchor,
            checkpoint,
        } => Ok(BoundaryEffect {
            kind: BoundaryEffectKind::Persist,
            seat: None,
            legal_actions: None,
            transcript_anchor: Some(transcript_anchor.into()),
            artifact: Vec::new(),
            checkpoint: checkpoint.map(|value| value.to_bytes()).transpose()?,
            finished: None,
        }),
        HostEffect::RequestAction {
            actor,
            legal_actions,
        } => Ok(BoundaryEffect {
            kind: BoundaryEffectKind::RequestAction,
            seat: Some(actor.to_u8()),
            legal_actions: Some(legal_actions.into()),
            transcript_anchor: None,
            artifact: Vec::new(),
            checkpoint: None,
            finished: None,
        }),
        HostEffect::SubmitRecovery(bundle) => Ok(BoundaryEffect {
            kind: BoundaryEffectKind::SubmitRecovery,
            seat: None,
            legal_actions: None,
            transcript_anchor: None,
            artifact: bundle.to_bytes()?,
            checkpoint: None,
            finished: None,
        }),
        HostEffect::PublishShowdownBundle { owner, bundle } => Ok(BoundaryEffect {
            kind: BoundaryEffectKind::PublishShowdownBundle,
            seat: Some(owner.to_u8()),
            legal_actions: None,
            transcript_anchor: None,
            artifact: bundle.to_bytes()?,
            checkpoint: None,
            finished: None,
        }),
        HostEffect::Finished(finished) => Ok(BoundaryEffect {
            kind: BoundaryEffectKind::Finished,
            seat: None,
            legal_actions: None,
            transcript_anchor: None,
            artifact: Vec::new(),
            checkpoint: None,
            finished: Some(finished.into()),
        }),
        _ => Err(protocol(
            "mtholdem-adapter does not know how to encode this host effect",
        )),
    }
}

/// Converts multiple typed peer-session effects into host-boundary effects.
pub fn encode_effects(
    effects: impl IntoIterator<Item = HostEffect>,
) -> Result<Vec<BoundaryEffect>> {
    effects.into_iter().map(encode_effect).collect()
}

/// Parses canonical public-message bytes.
pub fn decode_public_message(bytes: &[u8]) -> Result<SignedPublicMessage> {
    SignedPublicMessage::from_bytes(bytes)
}

/// Encodes canonical public-message bytes.
pub fn encode_public_message(message: &SignedPublicMessage) -> Result<Vec<u8>> {
    message.to_bytes()
}

/// Parses canonical private-reveal bytes.
pub fn decode_private_reveal(bytes: &[u8]) -> Result<SignedPrivateReveal> {
    SignedPrivateReveal::from_bytes(bytes)
}

/// Encodes canonical private-reveal bytes.
#[must_use]
pub fn encode_private_reveal(message: &SignedPrivateReveal) -> Vec<u8> {
    message.to_bytes()
}

/// Parses canonical showdown-bundle bytes.
pub fn decode_showdown_bundle(bytes: &[u8]) -> Result<ShowdownBundle> {
    ShowdownBundle::from_bytes(bytes)
}

/// Encodes canonical showdown-bundle bytes.
pub fn encode_showdown_bundle(bundle: &ShowdownBundle) -> Result<Vec<u8>> {
    bundle.to_bytes()
}

/// Parses canonical checkpoint-state bytes.
pub fn decode_checkpoint_state(bytes: &[u8]) -> Result<CheckpointState> {
    CheckpointState::from_bytes(bytes)
}

/// Encodes canonical checkpoint-state bytes.
pub fn encode_checkpoint_state(state: &CheckpointState) -> Result<Vec<u8>> {
    state.to_bytes()
}

/// Parses canonical certified-checkpoint bytes.
pub fn decode_certified_checkpoint(bytes: &[u8]) -> Result<CertifiedCheckpoint> {
    CertifiedCheckpoint::from_bytes(bytes)
}

/// Encodes canonical certified-checkpoint bytes.
pub fn encode_certified_checkpoint(checkpoint: &CertifiedCheckpoint) -> Result<Vec<u8>> {
    checkpoint.to_bytes()
}

/// Parses canonical recovery-bundle bytes.
pub fn decode_recovery_bundle(bytes: &[u8]) -> Result<RecoveryBundle> {
    RecoveryBundle::from_bytes(bytes)
}

/// Encodes canonical recovery-bundle bytes.
pub fn encode_recovery_bundle(bundle: &RecoveryBundle) -> Result<Vec<u8>> {
    bundle.to_bytes()
}

/// Builds a hand number from a primitive boundary value.
#[must_use]
pub const fn hand_number_from_u64(value: u64) -> HandNumber {
    HandNumber::from_u64(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mental_card_core::identity::IdentitySecret;
    use mtholdem_peer::PeerSession;

    #[test]
    fn action_round_trips_raise_to() {
        let action = BoundaryAction::new(BoundaryActionKind::RaiseTo, 42);

        assert_eq!(
            BoundaryAction::from(action.to_bet_action()),
            BoundaryAction::new(BoundaryActionKind::RaiseTo, 42)
        );
    }

    #[test]
    fn error_category_maps_to_stable_code() {
        let error = SignedPublicMessage::from_bytes(&[]).expect_err("empty bytes fail");

        assert_eq!(BoundaryErrorCode::from(&error), BoundaryErrorCode::Codec);
        assert_eq!(
            BoundaryError::from_error(&error).code(),
            BoundaryErrorCode::Codec
        );
    }

    #[test]
    fn progress_effects_encode_to_boundary_bytes() -> Result<()> {
        let mut table = mtholdem_testkit::TestTable::standard(2, [9; 32])?;
        let seat = Seat::new(0)?;
        let mut peer = PeerSession::new(
            table.config().clone(),
            IdentitySecret::from_seed(table.identity(seat)?.to_seed().expose()),
            seat,
            HandNumber::FIRST,
        )?;

        peer.progress(table.rng_mut())?;
        let effects = encode_effects(peer.drain_effects())?;

        assert!(effects.iter().any(|effect| {
            effect.kind() == BoundaryEffectKind::BroadcastPublic && !effect.artifact().is_empty()
        }));
        assert!(effects.iter().any(|effect| {
            effect.kind() == BoundaryEffectKind::Persist && effect.transcript_anchor().is_some()
        }));
        Ok(())
    }
}
