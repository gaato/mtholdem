//! Canonical signed artifacts exchanged by peers.
//!
//! The codec is deliberately fixed-width, versioned, length-bounded, and independent
//! of Serde.  Every signature covers the session configuration, hand number, sender,
//! phase, and transcript predecessor.  Hole-card reveal shares use a separate direct
//! message that **must** travel over an application-provided confidential channel.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::betting::{BetAction, Street};
use crate::card::Card;
use crate::codec::{Decoder, Encoder};
use crate::config::{Participant, Roster};
use crate::types::{CardPosition, Chips, Digest32, HandNumber, PlayerId, Seat, SessionId};
use mental_card_core::crypto::{
    ENCRYPTED_DECK_BYTES, EncryptedDeck, KEY_OWNERSHIP_PROOF_BYTES, KeyOwnershipProof,
    MENTAL_PUBLIC_KEY_BYTES, MentalPublicKey, REVEAL_SHARE_BYTES, REVEAL_SHARE_PROOF_BYTES,
    RevealShare, RevealShareProof, SHUFFLE_PROOF_BYTES, ShuffleContribution, ShuffleProof,
};
use mental_card_core::error::{Result, authentication, codec, protocol};
use mental_card_core::identity::{IDENTITY_SIGNATURE_BYTES, IdentitySecret, IdentitySignature};

const PUBLIC_MAGIC: [u8; 4] = *b"MTHP";
const PRIVATE_MAGIC: [u8; 4] = *b"MTHR";
const SHOWDOWN_MAGIC: [u8; 4] = *b"MTHS";
const WIRE_VERSION: u16 = 1;

/// Maximum encoded size of a signed public event.
pub const MAX_PUBLIC_MESSAGE_BYTES: usize = 64 * 1024;
/// Maximum encoded size of one signed private reveal share.
pub const MAX_PRIVATE_REVEAL_BYTES: usize = 512;
/// Maximum encoded size of one showdown disclosure bundle.
pub const MAX_SHOWDOWN_BUNDLE_BYTES: usize = 64 * 1024;

/// Coarse protocol phase bound into every public signature.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum HandPhase {
    /// Ephemeral threshold-key announcements.
    KeySetup = 0,
    /// Sequential verifiable deck shuffles.
    Shuffle = 1,
    /// Signatures confirming the final deck and forced blind state.
    DeckReady = 2,
    /// Confidential distribution of hole-card reveal shares.
    HoleCards = 3,
    /// Preflop betting actions.
    Preflop = 4,
    /// Public reveal of the flop.
    FlopReveal = 5,
    /// Flop betting actions.
    Flop = 6,
    /// Public reveal of the turn.
    TurnReveal = 7,
    /// Turn betting actions.
    Turn = 8,
    /// Public reveal of the river.
    RiverReveal = 9,
    /// River betting actions.
    River = 10,
    /// Opening or mucking private cards.
    Showdown = 11,
    /// Deterministic pot-distribution agreement.
    Payout = 12,
    /// The hand has a final certified outcome.
    Complete = 13,
    /// A peer has requested mediated recovery.
    Recovery = 14,
}

impl HandPhase {
    fn from_u8(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::KeySetup),
            1 => Ok(Self::Shuffle),
            2 => Ok(Self::DeckReady),
            3 => Ok(Self::HoleCards),
            4 => Ok(Self::Preflop),
            5 => Ok(Self::FlopReveal),
            6 => Ok(Self::Flop),
            7 => Ok(Self::TurnReveal),
            8 => Ok(Self::Turn),
            9 => Ok(Self::RiverReveal),
            10 => Ok(Self::River),
            11 => Ok(Self::Showdown),
            12 => Ok(Self::Payout),
            13 => Ok(Self::Complete),
            14 => Ok(Self::Recovery),
            _ => Err(codec(format!("unknown hand phase tag {value}"))),
        }
    }
}

impl fmt::Display for HandPhase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::KeySetup => "key setup",
            Self::Shuffle => "shuffle",
            Self::DeckReady => "deck ready",
            Self::HoleCards => "hole cards",
            Self::Preflop => "preflop",
            Self::FlopReveal => "flop reveal",
            Self::Flop => "flop",
            Self::TurnReveal => "turn reveal",
            Self::Turn => "turn",
            Self::RiverReveal => "river reveal",
            Self::River => "river",
            Self::Showdown => "showdown",
            Self::Payout => "payout",
            Self::Complete => "complete",
            Self::Recovery => "recovery",
        })
    }
}

/// Semantically typed content of a public, globally ordered event.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PublicPayload {
    /// One ephemeral threshold public-key share and Schnorr ownership proof.
    KeyAnnouncement {
        /// Threshold public-key share.
        public_key: MentalPublicKey,
        /// Proof that the sender knows its discrete logarithm.
        proof: KeyOwnershipProof,
    },
    /// One sequential encrypted-deck shuffle and Bayer--Groth proof.
    Shuffle {
        /// New encrypted deck and proof relative to the preceding deck.
        contribution: Box<ShuffleContribution>,
    },
    /// Confirmation that a player received and validated both private cards.
    HoleCardsReceived {
        /// Digest of the complete signed private-share set, not of the card values.
        receipt_digest: Digest32,
    },
    /// One no-limit betting action.
    Bet {
        /// Street on which the action is applied.
        street: Street,
        /// Action interpreted by the deterministic betting engine.
        action: BetAction,
    },
    /// One publicly broadcast threshold-decryption share.
    PublicRevealShare {
        /// Encrypted-deck position being opened.
        position: CardPosition,
        /// Decryption share.
        share: RevealShare,
        /// Chaum--Pedersen proof for the share.
        proof: RevealShareProof,
    },
    /// Consensus marker for a card reconstructed from the preceding shares.
    BoardCardOpened {
        /// Deck position that was opened.
        position: CardPosition,
        /// Result every peer must independently recompute.
        card: Card,
    },
    /// Commitment to a separately encoded showdown disclosure bundle.
    ShowdownOpen {
        /// Digest of [`ShowdownBundle::to_bytes`].
        bundle_digest: Digest32,
    },
    /// The sender declines to reveal its private cards and cannot win a pot.
    Muck,
    /// Proposed deterministic payouts in seat order.
    Payout {
        /// Chip amounts for every roster seat.
        payouts: Vec<Chips>,
    },
    /// Announcement of a fully signed checkpoint available to all peers.
    Checkpoint {
        /// Digest of the certified checkpoint bytes.
        checkpoint_digest: Digest32,
    },
    /// Request to move execution to the external recovery mediator.
    RecoveryRequested {
        /// Most recent checkpoint known to the requester.
        checkpoint_digest: Digest32,
    },
}

impl PublicPayload {
    fn validate_for_phase(&self, phase: HandPhase) -> Result<()> {
        let expected = match self {
            Self::KeyAnnouncement { .. } => Some(HandPhase::KeySetup),
            Self::Shuffle { .. } => Some(HandPhase::Shuffle),
            Self::HoleCardsReceived { .. } => Some(HandPhase::HoleCards),
            Self::Bet { street, .. } => Some(match street {
                Street::Preflop => HandPhase::Preflop,
                Street::Flop => HandPhase::Flop,
                Street::Turn => HandPhase::Turn,
                Street::River => HandPhase::River,
            }),
            Self::PublicRevealShare { .. } | Self::BoardCardOpened { .. } => {
                if matches!(
                    phase,
                    HandPhase::FlopReveal | HandPhase::TurnReveal | HandPhase::RiverReveal
                ) {
                    None
                } else {
                    return Err(protocol(format!(
                        "card-reveal payload is invalid during declared phase {phase}"
                    )));
                }
            }
            Self::ShowdownOpen { .. } | Self::Muck => Some(HandPhase::Showdown),
            Self::Payout { .. } => Some(HandPhase::Payout),
            // A checkpoint announcement names the phase whose state was certified, so
            // it is valid at any phase boundary.  The certificate itself carries and
            // authenticates the exact phase.
            Self::Checkpoint { .. } => None,
            Self::RecoveryRequested { .. } => Some(HandPhase::Recovery),
        };

        if expected.is_some_and(|expected| expected != phase) {
            return Err(protocol(format!(
                "public payload is invalid during declared phase {phase}; expected {}",
                expected.expect("checked as Some")
            )));
        }
        match self {
            Self::HoleCardsReceived { receipt_digest } if *receipt_digest == Digest32::ZERO => {
                Err(protocol("hole-card receipt commitment must not be zero"))
            }
            Self::ShowdownOpen { bundle_digest } if *bundle_digest == Digest32::ZERO => {
                Err(protocol("showdown bundle commitment must not be zero"))
            }
            Self::Checkpoint { checkpoint_digest } if *checkpoint_digest == Digest32::ZERO => {
                Err(protocol("checkpoint commitment must not be zero"))
            }
            _ => Ok(()),
        }
    }

    fn encode(&self, encoder: &mut Encoder) -> Result<()> {
        match self {
            Self::KeyAnnouncement { public_key, proof } => {
                encoder.u8(1);
                encoder.fixed(&public_key.to_bytes());
                encoder.fixed(&proof.to_bytes());
            }
            Self::Shuffle { contribution } => {
                encoder.u8(2);
                encoder.fixed(&contribution.deck().to_bytes());
                encoder.fixed(&contribution.proof().to_bytes());
            }
            Self::HoleCardsReceived { receipt_digest } => {
                encoder.u8(3);
                encoder.fixed(receipt_digest.as_ref());
            }
            Self::Bet { street, action } => {
                encoder.u8(4);
                encoder.u8(*street as u8);
                encode_bet_action(encoder, *action);
            }
            Self::PublicRevealShare {
                position,
                share,
                proof,
            } => {
                encoder.u8(5);
                encoder.u8(position.to_u8());
                encoder.fixed(&share.to_bytes());
                encoder.fixed(&proof.to_bytes());
            }
            Self::BoardCardOpened { position, card } => {
                encoder.u8(6);
                encoder.u8(position.to_u8());
                encoder.u8(u8::try_from(card.to_index()).expect("card index fits u8"));
            }
            Self::ShowdownOpen { bundle_digest } => {
                encoder.u8(7);
                encoder.fixed(bundle_digest.as_ref());
            }
            Self::Muck => encoder.u8(8),
            Self::Payout { payouts } => {
                encoder.u8(9);
                let count =
                    u8::try_from(payouts.len()).map_err(|_| codec("payout vector is too long"))?;
                encoder.u8(count);
                for payout in payouts {
                    encoder.u64(payout.units());
                }
            }
            Self::Checkpoint { checkpoint_digest } => {
                encoder.u8(10);
                encoder.fixed(checkpoint_digest.as_ref());
            }
            Self::RecoveryRequested { checkpoint_digest } => {
                encoder.u8(11);
                encoder.fixed(checkpoint_digest.as_ref());
            }
        }
        Ok(())
    }

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self> {
        match decoder.u8("public payload tag")? {
            1 => Ok(Self::KeyAnnouncement {
                public_key: MentalPublicKey::from_bytes(
                    &decoder.fixed::<MENTAL_PUBLIC_KEY_BYTES>("mental public key")?,
                )?,
                proof: KeyOwnershipProof::from_bytes(
                    &decoder.fixed::<KEY_OWNERSHIP_PROOF_BYTES>("key ownership proof")?,
                )?,
            }),
            2 => {
                let deck = EncryptedDeck::from_bytes(
                    &decoder.fixed::<ENCRYPTED_DECK_BYTES>("encrypted deck")?,
                )?;
                let proof = ShuffleProof::from_bytes(
                    &decoder.fixed::<SHUFFLE_PROOF_BYTES>("shuffle proof")?,
                )?;
                Ok(Self::Shuffle {
                    contribution: Box::new(ShuffleContribution::from_parts(deck, proof)),
                })
            }
            3 => Ok(Self::HoleCardsReceived {
                receipt_digest: Digest32::from_bytes(decoder.fixed("receipt digest")?),
            }),
            4 => Ok(Self::Bet {
                street: decode_street(decoder.u8("street")?)?,
                action: decode_bet_action(decoder)?,
            }),
            5 => Ok(Self::PublicRevealShare {
                position: CardPosition::from_u8(decoder.u8("card position")?)?,
                share: RevealShare::from_bytes(
                    &decoder.fixed::<REVEAL_SHARE_BYTES>("reveal share")?,
                )?,
                proof: RevealShareProof::from_bytes(
                    &decoder.fixed::<REVEAL_SHARE_PROOF_BYTES>("reveal share proof")?,
                )?,
            }),
            6 => Ok(Self::BoardCardOpened {
                position: CardPosition::from_u8(decoder.u8("card position")?)?,
                card: Card::from_index(usize::from(decoder.u8("card index")?))?,
            }),
            7 => Ok(Self::ShowdownOpen {
                bundle_digest: Digest32::from_bytes(decoder.fixed("showdown bundle digest")?),
            }),
            8 => Ok(Self::Muck),
            9 => {
                let count = usize::from(decoder.u8("payout count")?);
                if count > crate::types::MAX_PLAYERS {
                    return Err(codec("payout vector exceeds maximum player count"));
                }
                let mut payouts = Vec::with_capacity(count);
                for _ in 0..count {
                    payouts.push(Chips::from_units(decoder.u64("payout")?));
                }
                Ok(Self::Payout { payouts })
            }
            10 => Ok(Self::Checkpoint {
                checkpoint_digest: Digest32::from_bytes(decoder.fixed("checkpoint digest")?),
            }),
            11 => Ok(Self::RecoveryRequested {
                checkpoint_digest: Digest32::from_bytes(decoder.fixed("checkpoint digest")?),
            }),
            tag => Err(codec(format!("unknown public payload tag {tag}"))),
        }
    }
}

/// A signed public event in the single, hash-chained hand transcript.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignedPublicMessage {
    session_id: SessionId,
    config_digest: Digest32,
    hand: HandNumber,
    sequence: u64,
    phase: HandPhase,
    sender: Seat,
    sender_id: PlayerId,
    previous_digest: Digest32,
    payload: PublicPayload,
    signature: IdentitySignature,
}

impl SignedPublicMessage {
    /// Creates and signs one public event.
    #[allow(clippy::too_many_arguments)]
    pub fn sign(
        session_id: SessionId,
        config_digest: Digest32,
        hand: HandNumber,
        sequence: u64,
        phase: HandPhase,
        sender: Participant,
        previous_digest: Digest32,
        payload: PublicPayload,
        identity: &IdentitySecret,
    ) -> Result<Self> {
        if sender.player_id() != identity.public().player_id() {
            return Err(authentication(
                "public-message signer does not own the declared participant",
            ));
        }
        payload.validate_for_phase(phase)?;
        let sender_id = sender.player_id();
        let mut message = Self {
            session_id,
            config_digest,
            hand,
            sequence,
            phase,
            sender: sender.seat(),
            sender_id,
            previous_digest,
            payload,
            signature: IdentitySignature::from_bytes([0_u8; IDENTITY_SIGNATURE_BYTES]),
        };
        let signing_bytes = message.signing_bytes()?;
        message.signature = identity.sign(&signing_bytes);
        Ok(message)
    }

    /// Parses one canonical message and rejects trailing bytes or oversized input.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut decoder = Decoder::new(bytes, MAX_PUBLIC_MESSAGE_BYTES, "public message")?;
        verify_prefix(&mut decoder, PUBLIC_MAGIC)?;
        let session_id = SessionId::from_bytes(decoder.fixed("session id")?)?;
        let config_digest = Digest32::from_bytes(decoder.fixed("config digest")?);
        let hand = HandNumber::from_u64(decoder.u64("hand number")?);
        let sequence = decoder.u64("public sequence")?;
        let phase = HandPhase::from_u8(decoder.u8("hand phase")?)?;
        let sender = Seat::from_u8(decoder.u8("sender seat")?)?;
        let sender_id = PlayerId::from_bytes(decoder.fixed("sender player id")?);
        let previous_digest = Digest32::from_bytes(decoder.fixed("previous digest")?);
        let payload = PublicPayload::decode(&mut decoder)?;
        let signature = IdentitySignature::from_bytes(decoder.fixed("identity signature")?);
        decoder.finish("public message")?;
        Ok(Self {
            session_id,
            config_digest,
            hand,
            sequence,
            phase,
            sender,
            sender_id,
            previous_digest,
            payload,
            signature,
        })
    }

    /// Encodes the complete signed event.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = self.signing_bytes()?;
        bytes.extend_from_slice(&self.signature.to_bytes());
        if bytes.len() > MAX_PUBLIC_MESSAGE_BYTES {
            return Err(codec("encoded public message exceeds size limit"));
        }
        Ok(bytes)
    }

    /// Verifies sender-seat binding and the Ed25519 signature against a roster.
    pub fn verify_signature(&self, roster: &Roster) -> Result<()> {
        let participant = roster.participant(self.sender)?;
        verify_sender(participant, self.sender_id)?;
        participant
            .identity()
            .verify(&self.signing_bytes()?, self.signature)
    }

    /// Validates phase/payload compatibility and roster-sized public vectors.
    ///
    /// This is intentionally separate from [`verify_signature`](Self::verify_signature):
    /// a mediator may first need to prove that a malicious player genuinely signed a
    /// structurally invalid action.  This method still does not apply the full poker
    /// transition; actor order, amounts, card positions, and proof contexts belong to
    /// the protocol and rules state machines.
    pub fn validate_declared_structure(&self, roster: &Roster) -> Result<()> {
        self.payload.validate_for_phase(self.phase)?;
        if let PublicPayload::Payout { payouts } = &self.payload
            && payouts.len() != roster.len()
        {
            return Err(protocol(format!(
                "payout vector has {} entries for a {}-seat roster",
                payouts.len(),
                roster.len()
            )));
        }
        Ok(())
    }

    /// Returns the session identifier.
    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Returns the committed session-configuration digest.
    #[must_use]
    pub const fn config_digest(&self) -> Digest32 {
        self.config_digest
    }

    /// Returns the hand number.
    #[must_use]
    pub const fn hand(&self) -> HandNumber {
        self.hand
    }

    /// Returns the zero-based public sequence.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the coarse protocol phase.
    #[must_use]
    pub const fn phase(&self) -> HandPhase {
        self.phase
    }

    /// Returns the sender seat.
    #[must_use]
    pub const fn sender(&self) -> Seat {
        self.sender
    }

    /// Returns the sender identity committed into the signature.
    #[must_use]
    pub const fn sender_id(&self) -> PlayerId {
        self.sender_id
    }

    /// Returns the preceding public-transcript digest.
    #[must_use]
    pub const fn previous_digest(&self) -> Digest32 {
        self.previous_digest
    }

    /// Returns the typed event payload.
    #[must_use]
    pub const fn payload(&self) -> &PublicPayload {
        &self.payload
    }

    /// Returns a digest of the complete canonical signed bytes.
    pub fn digest(&self) -> Result<Digest32> {
        Ok(Digest32::hash(b"public-message/v1", &[&self.to_bytes()?]))
    }

    fn signing_bytes(&self) -> Result<Vec<u8>> {
        let mut encoder = Encoder::with_capacity(10_000);
        encode_prefix(&mut encoder, PUBLIC_MAGIC);
        encoder.fixed(self.session_id.as_ref());
        encoder.fixed(self.config_digest.as_ref());
        encoder.u64(self.hand.to_u64());
        encoder.u64(self.sequence);
        encoder.u8(self.phase as u8);
        encoder.u8(self.sender.to_u8());
        encoder.fixed(self.sender_id.as_ref());
        encoder.fixed(self.previous_digest.as_ref());
        self.payload.encode(&mut encoder)?;
        Ok(encoder.into_bytes())
    }
}

/// One signed threshold-decryption share sent directly to a hole-card owner.
///
/// Signatures authenticate this artifact but do not encrypt it.  Exposing the bytes
/// before showdown can reveal the recipient's card when combined with other shares.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignedPrivateReveal {
    session_id: SessionId,
    config_digest: Digest32,
    hand: HandNumber,
    deck_digest: Digest32,
    sender: Seat,
    sender_id: PlayerId,
    recipient: Seat,
    recipient_id: PlayerId,
    position: CardPosition,
    share: RevealShare,
    proof: RevealShareProof,
    signature: IdentitySignature,
}

impl SignedPrivateReveal {
    /// Creates and signs a private reveal artifact.
    #[allow(clippy::too_many_arguments)]
    pub fn sign(
        session_id: SessionId,
        config_digest: Digest32,
        hand: HandNumber,
        deck_digest: Digest32,
        sender: Participant,
        recipient: Participant,
        position: CardPosition,
        share: RevealShare,
        proof: RevealShareProof,
        identity: &IdentitySecret,
    ) -> Result<Self> {
        if sender.player_id() != identity.public().player_id() {
            return Err(authentication(
                "private-reveal signer does not own the declared participant",
            ));
        }
        let sender_id = sender.player_id();
        let mut message = Self {
            session_id,
            config_digest,
            hand,
            deck_digest,
            sender: sender.seat(),
            sender_id,
            recipient: recipient.seat(),
            recipient_id: recipient.player_id(),
            position,
            share,
            proof,
            signature: IdentitySignature::from_bytes([0_u8; IDENTITY_SIGNATURE_BYTES]),
        };
        message.signature = identity.sign(&message.signing_bytes());
        Ok(message)
    }

    /// Parses a canonical private reveal artifact.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut decoder = Decoder::new(bytes, MAX_PRIVATE_REVEAL_BYTES, "private reveal")?;
        verify_prefix(&mut decoder, PRIVATE_MAGIC)?;
        let message = Self {
            session_id: SessionId::from_bytes(decoder.fixed("session id")?)?,
            config_digest: Digest32::from_bytes(decoder.fixed("config digest")?),
            hand: HandNumber::from_u64(decoder.u64("hand number")?),
            deck_digest: Digest32::from_bytes(decoder.fixed("deck digest")?),
            sender: Seat::from_u8(decoder.u8("sender seat")?)?,
            sender_id: PlayerId::from_bytes(decoder.fixed("sender player id")?),
            recipient: Seat::from_u8(decoder.u8("recipient seat")?)?,
            recipient_id: PlayerId::from_bytes(decoder.fixed("recipient player id")?),
            position: CardPosition::from_u8(decoder.u8("card position")?)?,
            share: RevealShare::from_bytes(&decoder.fixed::<REVEAL_SHARE_BYTES>("reveal share")?)?,
            proof: RevealShareProof::from_bytes(
                &decoder.fixed::<REVEAL_SHARE_PROOF_BYTES>("reveal share proof")?,
            )?,
            signature: IdentitySignature::from_bytes(decoder.fixed("identity signature")?),
        };
        decoder.finish("private reveal")?;
        Ok(message)
    }

    /// Encodes the complete signed private artifact.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = self.signing_bytes();
        bytes.extend_from_slice(&self.signature.to_bytes());
        debug_assert!(bytes.len() <= MAX_PRIVATE_REVEAL_BYTES);
        bytes
    }

    /// Verifies sender, recipient, and signature against the immutable roster.
    pub fn verify_signature(&self, roster: &Roster) -> Result<()> {
        let sender = roster.participant(self.sender)?;
        let recipient = roster.participant(self.recipient)?;
        verify_sender(sender, self.sender_id)?;
        verify_sender(recipient, self.recipient_id)?;
        sender
            .identity()
            .verify(&self.signing_bytes(), self.signature)
    }

    /// Returns the session identifier.
    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Returns the session-configuration digest.
    #[must_use]
    pub const fn config_digest(&self) -> Digest32 {
        self.config_digest
    }

    /// Returns the hand number.
    #[must_use]
    pub const fn hand(&self) -> HandNumber {
        self.hand
    }

    /// Returns the final encrypted-deck digest.
    #[must_use]
    pub const fn deck_digest(&self) -> Digest32 {
        self.deck_digest
    }

    /// Returns the share sender.
    #[must_use]
    pub const fn sender(&self) -> Seat {
        self.sender
    }

    /// Returns the intended private recipient.
    #[must_use]
    pub const fn recipient(&self) -> Seat {
        self.recipient
    }

    /// Returns the encrypted-deck card position.
    #[must_use]
    pub const fn position(&self) -> CardPosition {
        self.position
    }

    /// Returns the threshold-decryption share.
    #[must_use]
    pub const fn share(&self) -> RevealShare {
        self.share
    }

    /// Returns the share-validity proof.
    #[must_use]
    pub const fn proof(&self) -> RevealShareProof {
        self.proof
    }

    /// Returns the digest of the complete signed bytes.
    #[must_use]
    pub fn digest(&self) -> Digest32 {
        Digest32::hash(b"private-reveal/v1", &[&self.to_bytes()])
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let mut encoder = Encoder::with_capacity(384);
        encode_prefix(&mut encoder, PRIVATE_MAGIC);
        encoder.fixed(self.session_id.as_ref());
        encoder.fixed(self.config_digest.as_ref());
        encoder.u64(self.hand.to_u64());
        encoder.fixed(self.deck_digest.as_ref());
        encoder.u8(self.sender.to_u8());
        encoder.fixed(self.sender_id.as_ref());
        encoder.u8(self.recipient.to_u8());
        encoder.fixed(self.recipient_id.as_ref());
        encoder.u8(self.position.to_u8());
        encoder.fixed(&self.share.to_bytes());
        encoder.fixed(&self.proof.to_bytes());
        encoder.into_bytes()
    }
}

/// Signed disclosure of all private shares needed to open one player's two cards.
///
/// The bundle is transported as a side artifact.  The public transcript records its
/// digest via [`PublicPayload::ShowdownOpen`], avoiding deeply nested event parsing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShowdownBundle {
    session_id: SessionId,
    config_digest: Digest32,
    hand: HandNumber,
    deck_digest: Digest32,
    owner: Seat,
    owner_id: PlayerId,
    positions: [CardPosition; 2],
    reveals: Vec<SignedPrivateReveal>,
    signature: IdentitySignature,
}

impl ShowdownBundle {
    /// Validates, canonicalizes, and signs a complete two-card disclosure.
    #[allow(clippy::too_many_arguments)]
    pub fn sign(
        session_id: SessionId,
        config_digest: Digest32,
        hand: HandNumber,
        deck_digest: Digest32,
        owner: Seat,
        positions: [CardPosition; 2],
        mut reveals: Vec<SignedPrivateReveal>,
        roster: &Roster,
        identity: &IdentitySecret,
    ) -> Result<Self> {
        if identity.public().player_id() != roster.participant(owner)?.player_id() {
            return Err(authentication(
                "showdown signer does not own the declared seat",
            ));
        }
        reveals.sort_by_key(|reveal| (reveal.position().to_u8(), reveal.sender().to_u8()));
        validate_showdown_reveals(
            session_id,
            config_digest,
            hand,
            deck_digest,
            owner,
            positions,
            &reveals,
            roster,
        )?;
        let mut bundle = Self {
            session_id,
            config_digest,
            hand,
            deck_digest,
            owner,
            owner_id: identity.public().player_id(),
            positions,
            reveals,
            signature: IdentitySignature::from_bytes([0_u8; IDENTITY_SIGNATURE_BYTES]),
        };
        bundle.signature = identity.sign(&bundle.signing_bytes()?);
        Ok(bundle)
    }

    /// Parses one canonical showdown bundle.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut decoder = Decoder::new(bytes, MAX_SHOWDOWN_BUNDLE_BYTES, "showdown bundle")?;
        verify_prefix(&mut decoder, SHOWDOWN_MAGIC)?;
        let session_id = SessionId::from_bytes(decoder.fixed("session id")?)?;
        let config_digest = Digest32::from_bytes(decoder.fixed("config digest")?);
        let hand = HandNumber::from_u64(decoder.u64("hand number")?);
        let deck_digest = Digest32::from_bytes(decoder.fixed("deck digest")?);
        let owner = Seat::from_u8(decoder.u8("owner seat")?)?;
        let owner_id = PlayerId::from_bytes(decoder.fixed("owner player id")?);
        let positions = [
            CardPosition::from_u8(decoder.u8("first hole position")?)?,
            CardPosition::from_u8(decoder.u8("second hole position")?)?,
        ];
        let count = usize::from(decoder.u16("private reveal count")?);
        if count > crate::types::MAX_PLAYERS * 2 {
            return Err(codec("showdown reveal count exceeds two shares per player"));
        }
        let mut reveals = Vec::with_capacity(count);
        for _ in 0..count {
            let encoded = decoder.length_prefixed(MAX_PRIVATE_REVEAL_BYTES, "private reveal")?;
            reveals.push(SignedPrivateReveal::from_bytes(encoded)?);
        }
        let signature = IdentitySignature::from_bytes(decoder.fixed("identity signature")?);
        decoder.finish("showdown bundle")?;
        Ok(Self {
            session_id,
            config_digest,
            hand,
            deck_digest,
            owner,
            owner_id,
            positions,
            reveals,
            signature,
        })
    }

    /// Verifies bundle structure, every nested signature, and the owner's signature.
    ///
    /// This does not verify the nested Chaum--Pedersen proofs or bind the bundle to the
    /// receipt acknowledged before betting.  Use
    /// [`HandCryptography::open_showdown_bundle`](crate::protocol::HandCryptography::open_showdown_bundle)
    /// for the complete acceptance path.
    pub fn verify(&self, roster: &Roster) -> Result<()> {
        verify_sender(roster.participant(self.owner)?, self.owner_id)?;
        validate_showdown_reveals(
            self.session_id,
            self.config_digest,
            self.hand,
            self.deck_digest,
            self.owner,
            self.positions,
            &self.reveals,
            roster,
        )?;
        roster
            .participant(self.owner)?
            .identity()
            .verify(&self.signing_bytes()?, self.signature)
    }

    /// Encodes the complete signed bundle.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = self.signing_bytes()?;
        bytes.extend_from_slice(&self.signature.to_bytes());
        if bytes.len() > MAX_SHOWDOWN_BUNDLE_BYTES {
            return Err(codec("encoded showdown bundle exceeds size limit"));
        }
        Ok(bytes)
    }

    /// Returns the owner seat.
    #[must_use]
    pub const fn owner(&self) -> Seat {
        self.owner
    }

    /// Returns the two disclosed positions.
    #[must_use]
    pub const fn positions(&self) -> [CardPosition; 2] {
        self.positions
    }

    /// Returns signed private shares in position-then-sender order.
    #[must_use]
    pub fn reveals(&self) -> &[SignedPrivateReveal] {
        &self.reveals
    }

    /// Returns the digest recorded in the public transcript.
    pub fn digest(&self) -> Result<Digest32> {
        Ok(Digest32::hash(b"showdown-bundle/v1", &[&self.to_bytes()?]))
    }

    fn signing_bytes(&self) -> Result<Vec<u8>> {
        let mut encoder = Encoder::with_capacity(16_000);
        encode_prefix(&mut encoder, SHOWDOWN_MAGIC);
        encoder.fixed(self.session_id.as_ref());
        encoder.fixed(self.config_digest.as_ref());
        encoder.u64(self.hand.to_u64());
        encoder.fixed(self.deck_digest.as_ref());
        encoder.u8(self.owner.to_u8());
        encoder.fixed(self.owner_id.as_ref());
        encoder.u8(self.positions[0].to_u8());
        encoder.u8(self.positions[1].to_u8());
        let count = u16::try_from(self.reveals.len())
            .map_err(|_| codec("too many private reveals in showdown bundle"))?;
        encoder.u16(count);
        for reveal in &self.reveals {
            encoder.length_prefixed(&reveal.to_bytes())?;
        }
        Ok(encoder.into_bytes())
    }
}

/// Computes the non-secret receipt committed after private-card delivery.
///
/// The digest covers complete signatures and randomized proof bytes.  It intentionally
/// does not hash the two card values directly, which would permit inexpensive brute
/// force over the small set of possible Hold'em hands.
pub(crate) fn private_reveal_receipt(
    reveals: &[SignedPrivateReveal],
    roster: &Roster,
) -> Result<Digest32> {
    validate_private_receipt_shape(reveals, roster)?;
    let mut sorted = reveals.to_vec();
    sorted.sort_by_key(|reveal| (reveal.position().to_u8(), reveal.sender().to_u8()));
    let mut canonical = Encoder::with_capacity(sorted.len() * 384);
    let count =
        u16::try_from(sorted.len()).map_err(|_| codec("too many private reveals for a receipt"))?;
    canonical.u16(count);
    for reveal in sorted {
        canonical.length_prefixed(&reveal.to_bytes())?;
    }
    Ok(Digest32::hash(
        b"private-reveal-receipt/v2",
        &[&canonical.into_bytes()],
    ))
}

fn validate_private_receipt_shape(reveals: &[SignedPrivateReveal], roster: &Roster) -> Result<()> {
    let expected_count = roster
        .len()
        .checked_mul(2)
        .ok_or_else(|| protocol("private-card receipt share count overflow"))?;
    if reveals.len() != expected_count {
        return Err(protocol(format!(
            "private-card receipt requires {expected_count} reveal shares, got {}",
            reveals.len()
        )));
    }

    let first = reveals
        .first()
        .ok_or_else(|| protocol("cannot acknowledge an empty private reveal set"))?;
    let mut by_position: BTreeMap<CardPosition, BTreeSet<Seat>> = BTreeMap::new();
    let mut unique = BTreeSet::new();
    for reveal in reveals {
        reveal.verify_signature(roster)?;
        if reveal.session_id() != first.session_id()
            || reveal.config_digest() != first.config_digest()
            || reveal.hand() != first.hand()
            || reveal.deck_digest() != first.deck_digest()
            || reveal.recipient() != first.recipient()
        {
            return Err(protocol(
                "private-card receipt contains shares from different contexts",
            ));
        }
        if !unique.insert((reveal.position(), reveal.sender())) {
            return Err(protocol(
                "private-card receipt contains a duplicate sender-position share",
            ));
        }
        by_position
            .entry(reveal.position())
            .or_default()
            .insert(reveal.sender());
    }

    if by_position.len() != 2 {
        return Err(protocol(
            "private-card receipt must commit exactly two distinct card positions",
        ));
    }
    let expected_senders: BTreeSet<_> = (0..roster.len()).map(Seat::new).collect::<Result<_>>()?;
    if by_position
        .values()
        .any(|senders| senders != &expected_senders)
    {
        return Err(protocol(
            "private-card receipt must contain every roster sender exactly once for both cards",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_showdown_reveals(
    session_id: SessionId,
    config_digest: Digest32,
    hand: HandNumber,
    deck_digest: Digest32,
    owner: Seat,
    positions: [CardPosition; 2],
    reveals: &[SignedPrivateReveal],
    roster: &Roster,
) -> Result<()> {
    if positions[0] == positions[1] {
        return Err(protocol("showdown bundle repeats one hole-card position"));
    }
    let expected_count = roster
        .len()
        .checked_mul(2)
        .ok_or_else(|| protocol("showdown reveal count overflow"))?;
    if reveals.len() != expected_count {
        return Err(protocol(format!(
            "showdown bundle requires {expected_count} private shares, got {}",
            reveals.len()
        )));
    }

    let allowed_positions: BTreeSet<_> = positions.into_iter().collect();
    let mut unique = BTreeSet::new();
    for reveal in reveals {
        reveal.verify_signature(roster)?;
        if reveal.session_id() != session_id
            || reveal.config_digest() != config_digest
            || reveal.hand() != hand
            || reveal.deck_digest() != deck_digest
        {
            return Err(protocol(
                "showdown private reveal belongs to another context",
            ));
        }
        if reveal.recipient() != owner {
            return Err(protocol("showdown private reveal has the wrong recipient"));
        }
        if !allowed_positions.contains(&reveal.position()) {
            return Err(protocol(
                "showdown private reveal has an unexpected position",
            ));
        }
        if !unique.insert((reveal.position(), reveal.sender())) {
            return Err(protocol(
                "showdown bundle contains a duplicate sender-position share",
            ));
        }
    }
    Ok(())
}

fn verify_sender(participant: Participant, claimed: PlayerId) -> Result<()> {
    if participant.player_id() != claimed {
        return Err(authentication(format!(
            "claimed player {claimed} does not occupy {}",
            participant.seat()
        )));
    }
    Ok(())
}

fn encode_prefix(encoder: &mut Encoder, magic: [u8; 4]) {
    encoder.fixed(&magic);
    encoder.u16(WIRE_VERSION);
}

fn verify_prefix(decoder: &mut Decoder<'_>, magic: [u8; 4]) -> Result<()> {
    let actual = decoder.fixed::<4>("wire magic")?;
    if actual != magic {
        return Err(codec("wire artifact has the wrong magic prefix"));
    }
    let version = decoder.u16("wire version")?;
    if version != WIRE_VERSION {
        return Err(codec(format!(
            "unsupported wire version {version}; expected {WIRE_VERSION}"
        )));
    }
    Ok(())
}

fn encode_bet_action(encoder: &mut Encoder, action: BetAction) {
    match action {
        BetAction::Fold => encoder.u8(0),
        BetAction::Check => encoder.u8(1),
        BetAction::Call => encoder.u8(2),
        BetAction::RaiseTo(amount) => {
            encoder.u8(3);
            encoder.u64(amount.units());
        }
        BetAction::AllIn => encoder.u8(4),
    }
}

fn decode_bet_action(decoder: &mut Decoder<'_>) -> Result<BetAction> {
    match decoder.u8("bet action")? {
        0 => Ok(BetAction::Fold),
        1 => Ok(BetAction::Check),
        2 => Ok(BetAction::Call),
        3 => Ok(BetAction::RaiseTo(Chips::from_units(
            decoder.u64("raise-to amount")?,
        ))),
        4 => Ok(BetAction::AllIn),
        tag => Err(codec(format!("unknown bet action tag {tag}"))),
    }
}

fn decode_street(value: u8) -> Result<Street> {
    match value {
        0 => Ok(Street::Preflop),
        1 => Ok(Street::Flop),
        2 => Ok(Street::Turn),
        3 => Ok(Street::River),
        _ => Err(codec(format!("unknown betting street tag {value}"))),
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rand::rngs::OsRng;

    use super::*;
    use crate::config::Roster;

    #[test]
    fn signed_public_message_round_trips_and_detects_tampering() {
        let mut rng = OsRng;
        let secret = IdentitySecret::generate(&mut rng);
        let roster = Roster::new(vec![
            secret.public(),
            IdentitySecret::generate(&mut rng).public(),
        ])
        .unwrap();
        let session = SessionId::random(&mut rng);
        let message = SignedPublicMessage::sign(
            session,
            Digest32::hash(b"config", &[]),
            HandNumber::FIRST,
            0,
            HandPhase::Showdown,
            roster.participant(Seat::new(0).unwrap()).unwrap(),
            Digest32::ZERO,
            PublicPayload::Muck,
            &secret,
        )
        .unwrap();
        message.verify_signature(&roster).unwrap();
        let bytes = message.to_bytes().unwrap();
        let decoded = SignedPublicMessage::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, message);

        let mut tampered = bytes;
        tampered[20] ^= 1;
        assert!(
            SignedPublicMessage::from_bytes(&tampered).is_err()
                || SignedPublicMessage::from_bytes(&tampered)
                    .unwrap()
                    .verify_signature(&roster)
                    .is_err()
        );
    }
    #[test]
    fn rejects_payloads_declared_in_the_wrong_phase() {
        let mut rng = OsRng;
        let secret = IdentitySecret::generate(&mut rng);
        let roster = Roster::new(vec![
            secret.public(),
            IdentitySecret::generate(&mut rng).public(),
        ])
        .unwrap();
        let result = SignedPublicMessage::sign(
            SessionId::random(&mut rng),
            Digest32::hash(b"config", &[]),
            HandNumber::FIRST,
            0,
            HandPhase::KeySetup,
            roster.participant(Seat::new(0).unwrap()).unwrap(),
            Digest32::ZERO,
            PublicPayload::Muck,
            &secret,
        );
        assert!(result.is_err());

        let zero_receipt = SignedPublicMessage::sign(
            SessionId::random(&mut rng),
            Digest32::hash(b"config", &[]),
            HandNumber::FIRST,
            0,
            HandPhase::HoleCards,
            roster.participant(Seat::new(0).unwrap()).unwrap(),
            Digest32::ZERO,
            PublicPayload::HoleCardsReceived {
                receipt_digest: Digest32::ZERO,
            },
            &secret,
        );
        assert!(zero_receipt.is_err());
    }

    proptest! {
        #[test]
        fn bounded_decoders_never_panic_on_untrusted_bytes(
            bytes in prop::collection::vec(any::<u8>(), 0..2_048),
        ) {
            let _public = SignedPublicMessage::from_bytes(&bytes);
            let _private = SignedPrivateReveal::from_bytes(&bytes);
            let _showdown = ShowdownBundle::from_bytes(&bytes);
        }
    }
}
