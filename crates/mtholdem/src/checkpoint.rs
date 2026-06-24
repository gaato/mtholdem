//! Compact, unanimously signed checkpoints for pruning and mediated recovery.
//!
//! Kaleidoscope and Royale rely on checkpoints because future card operations depend
//! on only a small public state.  Once every hand participant signs the same state,
//! earlier transcript messages may be archived off the hot path.  A certificate is
//! also the trust boundary used to restore a typed verified encrypted deck without
//! replaying every expensive shuffle proof.

use std::collections::BTreeSet;

use crate::betting::{HandLedger, HandSeatStatus};
use crate::card::Card;
use crate::codec::{Decoder, Encoder};
use crate::config::SessionConfig;
use crate::crypto::{AggregateMentalKey, ENCRYPTED_DECK_BYTES, EncryptedDeck, VerifiedDeck};
use crate::error::{Result, authentication, codec, recovery};
use crate::identity::{IDENTITY_SIGNATURE_BYTES, IdentitySecret, IdentitySignature};
use crate::transcript::TranscriptAnchor;
use crate::types::{CardPosition, Chips, Digest32, HandNumber, PlayerId, Seat, SessionId};
use crate::wire::HandPhase;

const STATE_MAGIC: [u8; 4] = *b"MTHC";
const CERTIFICATE_MAGIC: [u8; 4] = *b"MTHK";
const CHECKPOINT_VERSION: u16 = 2;

/// Maximum canonical size of a checkpoint state.
pub const MAX_CHECKPOINT_STATE_BYTES: usize = 16 * 1024;
/// Maximum canonical size of a fully signed checkpoint certificate.
pub const MAX_CHECKPOINT_CERTIFICATE_BYTES: usize = 32 * 1024;

/// Public chip and eligibility state for one seat at a checkpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SeatCheckpoint {
    seat: Seat,
    stack: Chips,
    total_contributed: Chips,
    street_contributed: Chips,
    status: HandSeatStatus,
}

impl SeatCheckpoint {
    /// Returns the seat index.
    #[must_use]
    pub const fn seat(self) -> Seat {
        self.seat
    }

    /// Returns chips not yet committed to this hand.
    #[must_use]
    pub const fn stack(self) -> Chips {
        self.stack
    }

    /// Returns the seat's total contribution to all pots.
    #[must_use]
    pub const fn total_contributed(self) -> Chips {
        self.total_contributed
    }

    /// Returns the contribution on the current street.
    #[must_use]
    pub const fn street_contributed(self) -> Chips {
        self.street_contributed
    }

    /// Returns the seat's hand eligibility state.
    #[must_use]
    pub const fn status(self) -> HandSeatStatus {
        self.status
    }
}

/// One publicly revealed community card and its encrypted-deck position.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OpenBoardCard {
    position: CardPosition,
    card: Card,
}

impl OpenBoardCard {
    /// Creates a position-card pair.
    #[must_use]
    pub const fn new(position: CardPosition, card: Card) -> Self {
        Self { position, card }
    }

    /// Returns the encrypted-deck position.
    #[must_use]
    pub const fn position(self) -> CardPosition {
        self.position
    }

    /// Returns the reconstructed card.
    #[must_use]
    pub const fn card(self) -> Card {
        self.card
    }
}

/// Per-seat commitments to successful private-card delivery.
///
/// A bitmap only proves that a receipt existed.  This type stores the exact receipt
/// digest for every acknowledged seat so a later showdown disclosure can be checked
/// against the shares that were delivered before betting began.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HoleCardReceipts {
    entries: Vec<Option<Digest32>>,
}

impl HoleCardReceipts {
    /// Creates an empty receipt set for the configured roster.
    #[must_use]
    pub fn empty(config: &SessionConfig) -> Self {
        Self {
            entries: vec![None; config.roster().len()],
        }
    }

    /// Records a receipt for one seat.
    ///
    /// Repeating the same value is idempotent, which is useful when a host retries a
    /// delivered event.  Replacing a previously recorded value is rejected.
    pub fn record(&mut self, seat: Seat, receipt_digest: Digest32) -> Result<()> {
        if receipt_digest == Digest32::ZERO {
            return Err(recovery("hole-card receipt digest must not be zero"));
        }
        let slot = self
            .entries
            .get_mut(seat.index())
            .ok_or_else(|| recovery("hole-card receipt seat is outside roster"))?;
        match *slot {
            None => {
                *slot = Some(receipt_digest);
                Ok(())
            }
            Some(existing) if existing == receipt_digest => Ok(()),
            Some(_) => Err(recovery(
                "hole-card receipt for this seat was already committed differently",
            )),
        }
    }

    /// Returns the committed receipt for `seat`, if delivery was acknowledged.
    pub fn receipt(&self, seat: Seat) -> Result<Option<Digest32>> {
        self.entries
            .get(seat.index())
            .copied()
            .ok_or_else(|| recovery("hole-card receipt seat is outside roster"))
    }

    /// Returns the roster size this set was created for.
    #[must_use]
    pub fn seat_count(&self) -> usize {
        self.entries.len()
    }

    /// Returns the number of seats with a committed receipt.
    #[must_use]
    pub fn received_count(&self) -> usize {
        self.entries.iter().filter(|entry| entry.is_some()).count()
    }

    /// Returns whether every roster seat has acknowledged private-card delivery.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.entries.iter().all(Option::is_some)
    }

    fn validate_for_roster(&self, roster_size: usize) -> Result<()> {
        if self.entries.len() != roster_size {
            return Err(recovery("hole-card receipt set does not match roster size"));
        }
        if self
            .entries
            .iter()
            .flatten()
            .any(|digest| *digest == Digest32::ZERO)
        {
            return Err(recovery("hole-card receipt digest must not be zero"));
        }
        Ok(())
    }

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self> {
        let count = usize::from(decoder.u8("hole-card receipt seat count")?);
        if !(crate::types::MIN_PLAYERS..=crate::types::MAX_PLAYERS).contains(&count) {
            return Err(codec(
                "hole-card receipt seat count is outside supported range",
            ));
        }
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let entry = match decoder.u8("hole-card receipt presence")? {
                0 => None,
                1 => {
                    let digest = Digest32::from_bytes(decoder.fixed("hole-card receipt digest")?);
                    if digest == Digest32::ZERO {
                        return Err(codec("hole-card receipt digest must not be zero"));
                    }
                    Some(digest)
                }
                value => {
                    return Err(codec(format!(
                        "invalid hole-card receipt presence flag {value}"
                    )));
                }
            };
            entries.push(entry);
        }
        Ok(Self { entries })
    }

    fn encode(&self, encoder: &mut Encoder) -> Result<()> {
        encoder.u8(u8::try_from(self.entries.len())
            .map_err(|_| codec("hole-card receipt seat count does not fit u8"))?);
        for entry in &self.entries {
            if let Some(digest) = entry {
                if *digest == Digest32::ZERO {
                    return Err(codec("hole-card receipt digest must not be zero"));
                }
                encoder.u8(1);
                encoder.fixed(digest.as_ref());
            } else {
                encoder.u8(0);
            }
        }
        Ok(())
    }
}

/// Canonical public state signed at a phase boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointState {
    session_id: SessionId,
    config_digest: Digest32,
    hand: HandNumber,
    phase: HandPhase,
    transcript: TranscriptAnchor,
    button: Option<Seat>,
    big_blind: Option<Chips>,
    seats: Vec<SeatCheckpoint>,
    board: Vec<OpenBoardCard>,
    encrypted_deck: Option<Vec<u8>>,
    aggregate_key_digest: Option<Digest32>,
    aggregate_key_participant_count: Option<u8>,
    hole_receipts: HoleCardReceipts,
    expected_actor: Option<Seat>,
    extension_digest: Digest32,
}

impl CheckpointState {
    /// Captures state at a protocol checkpoint.
    ///
    /// `extension_digest` lets an application commit phase-specific public data not
    /// represented directly here, such as a sorted set of showdown bundle digests.
    /// Use [`Digest32::ZERO`] when no extension exists.  When a ledger is present, its
    /// stacks plus current-hand contributions must conserve the immutable session chip
    /// supply (`buy_in * roster_size`).
    #[allow(clippy::too_many_arguments)]
    pub fn capture(
        config: &SessionConfig,
        hand: HandNumber,
        phase: HandPhase,
        transcript: TranscriptAnchor,
        ledger: Option<&HandLedger>,
        board: &[OpenBoardCard],
        deck: Option<&VerifiedDeck>,
        hole_receipts: &HoleCardReceipts,
        expected_actor: Option<Seat>,
        extension_digest: Digest32,
    ) -> Result<Self> {
        let (button, big_blind, seats) = if let Some(ledger) = ledger {
            if ledger.seat_count() != config.roster().len() {
                return Err(recovery("checkpoint ledger does not match roster size"));
            }
            let mut seats = Vec::with_capacity(ledger.seat_count());
            for index in 0..ledger.seat_count() {
                let seat = Seat::new(index)?;
                seats.push(SeatCheckpoint {
                    seat,
                    stack: ledger.stack(seat)?,
                    total_contributed: ledger.total_contributed(seat)?,
                    street_contributed: ledger.street_contributed(seat)?,
                    status: ledger.status(seat)?,
                });
            }
            (Some(ledger.button()), Some(ledger.big_blind()), seats)
        } else {
            (None, None, Vec::new())
        };

        validate_board(board)?;
        hole_receipts.validate_for_roster(config.roster().len())?;
        if expected_actor.is_some_and(|seat| seat.index() >= config.roster().len()) {
            return Err(recovery("checkpoint expected actor is outside roster"));
        }

        let encrypted_deck = deck.map(|deck| deck.to_bytes().to_vec());
        let aggregate_key_digest = deck.map(VerifiedDeck::aggregate_key_digest);
        let aggregate_key_participant_count = deck
            .map(VerifiedDeck::participant_count)
            .map(|count| u8::try_from(count).expect("supported participant count fits u8"));
        let state = Self {
            session_id: config.session_id(),
            config_digest: config.digest(),
            hand,
            phase,
            transcript,
            button,
            big_blind,
            seats,
            board: board.to_vec(),
            encrypted_deck,
            aggregate_key_digest,
            aggregate_key_participant_count,
            hole_receipts: hole_receipts.clone(),
            expected_actor,
            extension_digest,
        };
        state.validate_for_config(config)?;
        Ok(state)
    }

    /// Parses one canonical checkpoint-state encoding.
    ///
    /// Configuration-dependent semantic validation is performed when a vote or
    /// certificate is verified.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut decoder = Decoder::new(bytes, MAX_CHECKPOINT_STATE_BYTES, "checkpoint state")?;
        let magic = decoder.fixed::<4>("checkpoint state magic")?;
        if magic != STATE_MAGIC {
            return Err(codec("checkpoint state has the wrong magic"));
        }
        let version = decoder.u16("checkpoint state version")?;
        if version != CHECKPOINT_VERSION {
            return Err(codec(format!(
                "unsupported checkpoint state version {version}"
            )));
        }
        let session_id = SessionId::from_bytes(decoder.fixed("session id")?)?;
        let config_digest = Digest32::from_bytes(decoder.fixed("config digest")?);
        let hand = HandNumber::from_u64(decoder.u64("hand number")?);
        let phase = decode_phase(decoder.u8("hand phase")?)?;
        let transcript = TranscriptAnchor::new(
            decoder.u64("next transcript sequence")?,
            Digest32::from_bytes(decoder.fixed("transcript digest")?),
        );
        let has_ledger = decoder.u8("ledger presence")?;
        let (button, big_blind, seats) = match has_ledger {
            0 => (None, None, Vec::new()),
            1 => {
                let button = Seat::from_u8(decoder.u8("button")?)?;
                let big_blind = Chips::from_units(decoder.u64("big blind")?);
                let seat_count = usize::from(decoder.u8("seat count")?);
                if !(crate::types::MIN_PLAYERS..=crate::types::MAX_PLAYERS).contains(&seat_count) {
                    return Err(codec("checkpoint seat count is outside supported range"));
                }
                let mut seats = Vec::with_capacity(seat_count);
                for expected_index in 0..seat_count {
                    let seat = Seat::from_u8(decoder.u8("seat index")?)?;
                    if seat.index() != expected_index {
                        return Err(codec("checkpoint seats are not canonical and contiguous"));
                    }
                    seats.push(SeatCheckpoint {
                        seat,
                        stack: Chips::from_units(decoder.u64("seat stack")?),
                        total_contributed: Chips::from_units(
                            decoder.u64("seat total contribution")?,
                        ),
                        street_contributed: Chips::from_units(
                            decoder.u64("seat street contribution")?,
                        ),
                        status: decode_status(decoder.u8("seat status")?)?,
                    });
                }
                (Some(button), Some(big_blind), seats)
            }
            value => {
                return Err(codec(format!(
                    "invalid checkpoint ledger presence flag {value}"
                )));
            }
        };

        let board_count = usize::from(decoder.u8("board card count")?);
        if board_count > 5 {
            return Err(codec("checkpoint contains more than five board cards"));
        }
        let mut board = Vec::with_capacity(board_count);
        for _ in 0..board_count {
            board.push(OpenBoardCard::new(
                CardPosition::from_u8(decoder.u8("board card position")?)?,
                Card::from_index(usize::from(decoder.u8("board card index")?))?,
            ));
        }
        validate_board(&board)?;

        let (encrypted_deck, aggregate_key_digest, aggregate_key_participant_count) =
            match decoder.u8("encrypted deck presence")? {
                0 => (None, None, None),
                1 => (
                    Some(
                        decoder
                            .fixed::<ENCRYPTED_DECK_BYTES>("encrypted deck")?
                            .to_vec(),
                    ),
                    Some(Digest32::from_bytes(decoder.fixed("aggregate key digest")?)),
                    Some(decoder.u8("aggregate key participant count")?),
                ),
                value => {
                    return Err(codec(format!(
                        "invalid encrypted deck presence flag {value}"
                    )));
                }
            };
        let hole_receipts = HoleCardReceipts::decode(&mut decoder)?;
        let expected_actor = match decoder.u8("expected actor presence")? {
            0 => None,
            1 => Some(Seat::from_u8(decoder.u8("expected actor")?)?),
            value => {
                return Err(codec(format!(
                    "invalid expected actor presence flag {value}"
                )));
            }
        };
        let extension_digest = Digest32::from_bytes(decoder.fixed("extension digest")?);
        decoder.finish("checkpoint state")?;
        Ok(Self {
            session_id,
            config_digest,
            hand,
            phase,
            transcript,
            button,
            big_blind,
            seats,
            board,
            encrypted_deck,
            aggregate_key_digest,
            aggregate_key_participant_count,
            hole_receipts,
            expected_actor,
            extension_digest,
        })
    }

    /// Encodes the canonical state signed by checkpoint voters.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut encoder = Encoder::with_capacity(5_000);
        encoder.fixed(&STATE_MAGIC);
        encoder.u16(CHECKPOINT_VERSION);
        encoder.fixed(self.session_id.as_ref());
        encoder.fixed(self.config_digest.as_ref());
        encoder.u64(self.hand.to_u64());
        encoder.u8(self.phase as u8);
        encoder.u64(self.transcript.next_sequence());
        encoder.fixed(self.transcript.digest().as_ref());
        if let (Some(button), Some(big_blind)) = (self.button, self.big_blind) {
            encoder.u8(1);
            encoder.u8(button.to_u8());
            encoder.u64(big_blind.units());
            encoder.u8(u8::try_from(self.seats.len())
                .map_err(|_| codec("checkpoint seat count does not fit u8"))?);
            for seat in &self.seats {
                encoder.u8(seat.seat.to_u8());
                encoder.u64(seat.stack.units());
                encoder.u64(seat.total_contributed.units());
                encoder.u64(seat.street_contributed.units());
                encoder.u8(encode_status(seat.status));
            }
        } else if self.button.is_none() && self.big_blind.is_none() && self.seats.is_empty() {
            encoder.u8(0);
        } else {
            return Err(codec("checkpoint has a partially populated ledger"));
        }
        encoder.u8(u8::try_from(self.board.len())
            .map_err(|_| codec("checkpoint board count does not fit u8"))?);
        for card in &self.board {
            encoder.u8(card.position.to_u8());
            encoder.u8(u8::try_from(card.card.to_index()).expect("card index fits u8"));
        }
        match (
            &self.encrypted_deck,
            self.aggregate_key_digest,
            self.aggregate_key_participant_count,
        ) {
            (Some(deck), Some(aggregate_key_digest), Some(participant_count)) => {
                if deck.len() != ENCRYPTED_DECK_BYTES {
                    return Err(codec("checkpoint encrypted deck has the wrong size"));
                }
                encoder.u8(1);
                encoder.fixed(deck);
                encoder.fixed(aggregate_key_digest.as_ref());
                encoder.u8(participant_count);
            }
            (None, None, None) => encoder.u8(0),
            _ => {
                return Err(codec(
                    "checkpoint deck and aggregate-key commitment must appear together",
                ));
            }
        }
        self.hole_receipts.encode(&mut encoder)?;
        if let Some(actor) = self.expected_actor {
            encoder.u8(1);
            encoder.u8(actor.to_u8());
        } else {
            encoder.u8(0);
        }
        encoder.fixed(self.extension_digest.as_ref());
        let bytes = encoder.into_bytes();
        if bytes.len() > MAX_CHECKPOINT_STATE_BYTES {
            return Err(codec("checkpoint state exceeds size limit"));
        }
        Ok(bytes)
    }

    /// Returns a digest of the canonical state.
    pub fn digest(&self) -> Result<Digest32> {
        Ok(Digest32::hash(b"checkpoint-state/v2", &[&self.to_bytes()?]))
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

    /// Returns the phase at which this state was certified.
    #[must_use]
    pub const fn phase(&self) -> HandPhase {
        self.phase
    }

    /// Returns the public transcript anchor.
    #[must_use]
    pub const fn transcript(&self) -> TranscriptAnchor {
        self.transcript
    }

    /// Returns the dealer button when a ledger is present.
    #[must_use]
    pub const fn button(&self) -> Option<Seat> {
        self.button
    }

    /// Returns the big blind when a ledger is present.
    #[must_use]
    pub const fn big_blind(&self) -> Option<Chips> {
        self.big_blind
    }

    /// Returns immutable per-seat public accounting.
    #[must_use]
    pub fn seats(&self) -> &[SeatCheckpoint] {
        &self.seats
    }

    /// Returns opened board cards in protocol order.
    #[must_use]
    pub fn board(&self) -> &[OpenBoardCard] {
        &self.board
    }

    /// Returns whether the checkpoint includes the final encrypted deck.
    #[must_use]
    pub const fn has_encrypted_deck(&self) -> bool {
        self.encrypted_deck.is_some()
    }

    /// Returns the aggregate-key commitment for the stored encrypted deck.
    #[must_use]
    pub const fn aggregate_key_digest(&self) -> Option<Digest32> {
        self.aggregate_key_digest
    }

    /// Returns the threshold participant count for the stored encrypted deck.
    #[must_use]
    pub const fn aggregate_key_participant_count(&self) -> Option<u8> {
        self.aggregate_key_participant_count
    }

    /// Returns per-seat private-card delivery receipt commitments.
    #[must_use]
    pub const fn hole_receipts(&self) -> &HoleCardReceipts {
        &self.hole_receipts
    }

    /// Returns the next public actor, if the checkpoint records one.
    #[must_use]
    pub const fn expected_actor(&self) -> Option<Seat> {
        self.expected_actor
    }

    /// Returns the application-defined phase extension commitment.
    #[must_use]
    pub const fn extension_digest(&self) -> Digest32 {
        self.extension_digest
    }

    fn validate_for_config(&self, config: &SessionConfig) -> Result<()> {
        if self.session_id != config.session_id() || self.config_digest != config.digest() {
            return Err(recovery(
                "checkpoint belongs to another session configuration",
            ));
        }

        // A checkpoint is a trust boundary: once it is unanimously certified, callers
        // may discard the transcript that originally established this state.  Parsing
        // and signature verification are therefore not enough.  Recheck inexpensive
        // semantic invariants here before the state can become authoritative.
        match (self.button, self.big_blind, self.seats.is_empty()) {
            (None, None, true) => {}
            (Some(button), Some(big_blind), false) => {
                if self.seats.len() != config.roster().len() {
                    return Err(recovery("checkpoint seat state does not match roster size"));
                }
                if button.index() >= self.seats.len() {
                    return Err(recovery("checkpoint button exceeds roster"));
                }
                if big_blind != config.big_blind() {
                    return Err(recovery(
                        "checkpoint big blind differs from session configuration",
                    ));
                }

                let maximum_hand_loss = config.economics().maximum_hand_loss();
                let mut accounted_chips = 0_u128;
                for (index, seat) in self.seats.iter().enumerate() {
                    if seat.seat.index() != index {
                        return Err(recovery(
                            "checkpoint seat records are not in canonical order",
                        ));
                    }
                    if seat.street_contributed > seat.total_contributed {
                        return Err(recovery(
                            "checkpoint street contribution exceeds total contribution",
                        ));
                    }
                    if seat.total_contributed > maximum_hand_loss {
                        return Err(recovery(
                            "checkpoint contribution exceeds configured maximum hand loss",
                        ));
                    }
                    match seat.status {
                        HandSeatStatus::Playing if seat.stack.is_zero() => {
                            return Err(recovery(
                                "checkpoint marks a zero-stack seat as still able to act",
                            ));
                        }
                        HandSeatStatus::AllIn if !seat.stack.is_zero() => {
                            return Err(recovery(
                                "checkpoint marks a seat all-in while chips remain",
                            ));
                        }
                        HandSeatStatus::Folded if seat.stack.is_zero() => {
                            return Err(recovery(
                                "checkpoint marks a zero-stack seat as folded instead of all-in",
                            ));
                        }
                        HandSeatStatus::NotDealt
                            if !seat.stack.is_zero()
                                || !seat.total_contributed.is_zero()
                                || !seat.street_contributed.is_zero() =>
                        {
                            return Err(recovery(
                                "checkpoint gives stack or contributions to a seat not dealt in",
                            ));
                        }
                        _ => {}
                    }
                    accounted_chips = accounted_chips
                        .checked_add(u128::from(seat.stack.units()))
                        .and_then(|total| {
                            total.checked_add(u128::from(seat.total_contributed.units()))
                        })
                        .ok_or_else(|| recovery("checkpoint chip accounting overflowed"))?;
                }
                let expected_chips = u128::from(config.buy_in().units())
                    * u128::try_from(config.roster().len())
                        .map_err(|_| recovery("checkpoint roster size did not fit u128"))?;
                if accounted_chips != expected_chips {
                    return Err(recovery(
                        "checkpoint stack and contribution totals do not conserve session chips",
                    ));
                }
            }
            _ => {
                return Err(recovery(
                    "checkpoint contains a partial ledger representation",
                ));
            }
        }

        validate_board(&self.board)?;
        match (
            self.encrypted_deck.as_deref(),
            self.aggregate_key_digest,
            self.aggregate_key_participant_count,
        ) {
            (Some(bytes), Some(key_digest), Some(participant_count)) => {
                if key_digest == Digest32::ZERO {
                    return Err(recovery(
                        "checkpoint aggregate-key commitment must not be zero",
                    ));
                }
                EncryptedDeck::from_bytes(bytes).map_err(|_| {
                    recovery("checkpoint contains a non-canonical encrypted deck encoding")
                })?;
                if usize::from(participant_count) != config.roster().len() {
                    return Err(recovery(
                        "checkpoint deck threshold membership does not match roster size",
                    ));
                }
            }
            (None, None, None) => {}
            _ => {
                return Err(recovery(
                    "checkpoint deck and aggregate-key commitment must appear together",
                ));
            }
        }
        self.hole_receipts
            .validate_for_roster(config.roster().len())?;
        if self
            .expected_actor
            .is_some_and(|seat| seat.index() >= config.roster().len())
        {
            return Err(recovery("checkpoint expected actor exceeds roster"));
        }
        if let Some(actor) = self.expected_actor {
            if !self.seats.is_empty() && self.seats[actor.index()].status != HandSeatStatus::Playing
            {
                return Err(recovery(
                    "checkpoint expected actor is not eligible to take a betting action",
                ));
            }
        }
        Ok(())
    }
}

/// One player's signature over a checkpoint-state digest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckpointVote {
    seat: Seat,
    player_id: PlayerId,
    state_digest: Digest32,
    signature: IdentitySignature,
}

impl CheckpointVote {
    /// Signs a checkpoint state as the declared seat.
    pub fn sign(
        state: &CheckpointState,
        seat: Seat,
        config: &SessionConfig,
        identity: &IdentitySecret,
    ) -> Result<Self> {
        state.validate_for_config(config)?;
        let participant = config.roster().participant(seat)?;
        if participant.player_id() != identity.public().player_id() {
            return Err(authentication(
                "checkpoint signer does not own declared seat",
            ));
        }
        let state_digest = state.digest()?;
        let signing_bytes = vote_signing_bytes(
            state.session_id,
            state.config_digest,
            state.hand,
            seat,
            participant.player_id(),
            state_digest,
        );
        Ok(Self {
            seat,
            player_id: participant.player_id(),
            state_digest,
            signature: identity.sign(&signing_bytes),
        })
    }

    /// Returns the signing seat.
    #[must_use]
    pub const fn seat(self) -> Seat {
        self.seat
    }

    /// Returns the state digest signed by this vote.
    #[must_use]
    pub const fn state_digest(self) -> Digest32 {
        self.state_digest
    }

    fn verify(&self, state: &CheckpointState, config: &SessionConfig) -> Result<()> {
        let participant = config.roster().participant(self.seat)?;
        if participant.player_id() != self.player_id {
            return Err(authentication(
                "checkpoint vote has the wrong player identity",
            ));
        }
        let expected_digest = state.digest()?;
        if self.state_digest != expected_digest {
            return Err(recovery("checkpoint vote signs another state digest"));
        }
        participant.identity().verify(
            &vote_signing_bytes(
                state.session_id,
                state.config_digest,
                state.hand,
                self.seat,
                self.player_id,
                self.state_digest,
            ),
            self.signature,
        )
    }

    fn encode(&self, encoder: &mut Encoder) {
        encoder.u8(self.seat.to_u8());
        encoder.fixed(self.player_id.as_ref());
        encoder.fixed(self.state_digest.as_ref());
        encoder.fixed(&self.signature.to_bytes());
    }

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self {
            seat: Seat::from_u8(decoder.u8("checkpoint vote seat")?)?,
            player_id: PlayerId::from_bytes(decoder.fixed("checkpoint vote player id")?),
            state_digest: Digest32::from_bytes(decoder.fixed("checkpoint vote state digest")?),
            signature: IdentitySignature::from_bytes(
                decoder.fixed::<IDENTITY_SIGNATURE_BYTES>("checkpoint vote signature")?,
            ),
        })
    }
}

/// A checkpoint state with one valid signature from every hand participant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedCheckpoint {
    state: CheckpointState,
    votes: Vec<CheckpointVote>,
}

impl CertifiedCheckpoint {
    /// Sorts and verifies a unanimous set of checkpoint votes.
    pub fn assemble(
        state: CheckpointState,
        mut votes: Vec<CheckpointVote>,
        config: &SessionConfig,
    ) -> Result<Self> {
        state.validate_for_config(config)?;
        votes.sort_by_key(|vote| vote.seat.to_u8());
        let certificate = Self { state, votes };
        certificate.verify(config)?;
        Ok(certificate)
    }

    /// Parses a canonical certificate.  Call [`verify`](Self::verify) before trusting it.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut decoder = Decoder::new(
            bytes,
            MAX_CHECKPOINT_CERTIFICATE_BYTES,
            "checkpoint certificate",
        )?;
        let magic = decoder.fixed::<4>("certificate magic")?;
        if magic != CERTIFICATE_MAGIC {
            return Err(codec("checkpoint certificate has the wrong magic"));
        }
        let version = decoder.u16("certificate version")?;
        if version != CHECKPOINT_VERSION {
            return Err(codec(format!(
                "unsupported checkpoint certificate version {version}"
            )));
        }
        let state_bytes =
            decoder.length_prefixed(MAX_CHECKPOINT_STATE_BYTES, "checkpoint state")?;
        let state = CheckpointState::from_bytes(state_bytes)?;
        let vote_count = usize::from(decoder.u8("checkpoint vote count")?);
        if vote_count > crate::types::MAX_PLAYERS {
            return Err(codec("checkpoint has too many votes"));
        }
        let mut votes = Vec::with_capacity(vote_count);
        for _ in 0..vote_count {
            votes.push(CheckpointVote::decode(&mut decoder)?);
        }
        decoder.finish("checkpoint certificate")?;
        Ok(Self { state, votes })
    }

    /// Verifies context, unanimity, canonical seat order, and every signature.
    pub fn verify(&self, config: &SessionConfig) -> Result<()> {
        self.state.validate_for_config(config)?;
        if self.votes.len() != config.roster().len() {
            return Err(recovery(format!(
                "checkpoint requires {} votes, got {}",
                config.roster().len(),
                self.votes.len()
            )));
        }
        for (index, vote) in self.votes.iter().enumerate() {
            let expected = Seat::new(index)?;
            if vote.seat != expected {
                return Err(recovery(
                    "checkpoint votes are not canonical and contiguous",
                ));
            }
            vote.verify(&self.state, config)?;
        }
        Ok(())
    }

    /// Encodes the canonical certificate.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let state = self.state.to_bytes()?;
        let mut encoder = Encoder::with_capacity(state.len() + self.votes.len() * 129 + 16);
        encoder.fixed(&CERTIFICATE_MAGIC);
        encoder.u16(CHECKPOINT_VERSION);
        encoder.length_prefixed(&state)?;
        encoder.u8(u8::try_from(self.votes.len())
            .map_err(|_| codec("checkpoint vote count does not fit u8"))?);
        for vote in &self.votes {
            vote.encode(&mut encoder);
        }
        let bytes = encoder.into_bytes();
        if bytes.len() > MAX_CHECKPOINT_CERTIFICATE_BYTES {
            return Err(codec("checkpoint certificate exceeds size limit"));
        }
        Ok(bytes)
    }

    /// Returns the signed state.
    #[must_use]
    pub const fn state(&self) -> &CheckpointState {
        &self.state
    }

    /// Returns signatures in roster seat order.
    #[must_use]
    pub fn votes(&self) -> &[CheckpointVote] {
        &self.votes
    }

    /// Returns the transcript anchor suitable for suffix replay.
    #[must_use]
    pub const fn transcript_anchor(&self) -> TranscriptAnchor {
        self.state.transcript
    }

    /// Returns a digest suitable for a public checkpoint announcement.
    pub fn digest(&self) -> Result<Digest32> {
        Ok(Digest32::hash(
            b"checkpoint-certificate/v2",
            &[&self.to_bytes()?],
        ))
    }

    /// Restores the verified-deck type from unanimously certified bytes.
    ///
    /// This skips historical shuffle-proof replay.  The method verifies the certificate
    /// against `config` internally and requires the exact aggregate key committed by the
    /// checkpoint.  The safety argument is that at least one honest signer would not
    /// certify an unverified deck; deployments with every player malicious have no honest
    /// confidentiality or correctness target.
    pub fn restore_verified_deck(
        &self,
        config: &SessionConfig,
        aggregate_key: &AggregateMentalKey,
    ) -> Result<Option<VerifiedDeck>> {
        self.verify(config)?;
        match (
            self.state.encrypted_deck.as_deref(),
            self.state.aggregate_key_digest,
            self.state.aggregate_key_participant_count,
        ) {
            (Some(bytes), Some(committed_digest), Some(committed_count)) => {
                if aggregate_key.digest() != committed_digest
                    || aggregate_key.participant_count() != usize::from(committed_count)
                {
                    return Err(recovery(
                        "checkpoint encrypted deck is bound to another aggregate key",
                    ));
                }
                Ok(Some(
                    mental_card_core::__private::restore_verified_deck_after_certification(
                        bytes,
                        aggregate_key,
                    )?,
                ))
            }
            (None, None, None) => Ok(None),
            _ => Err(recovery(
                "checkpoint deck and aggregate-key commitment must appear together",
            )),
        }
    }
}

fn vote_signing_bytes(
    session_id: SessionId,
    config_digest: Digest32,
    hand: HandNumber,
    seat: Seat,
    player_id: PlayerId,
    state_digest: Digest32,
) -> Vec<u8> {
    let mut encoder = Encoder::with_capacity(160);
    encoder.fixed(b"mtholdem/checkpoint-vote/v2");
    encoder.fixed(session_id.as_ref());
    encoder.fixed(config_digest.as_ref());
    encoder.u64(hand.to_u64());
    encoder.u8(seat.to_u8());
    encoder.fixed(player_id.as_ref());
    encoder.fixed(state_digest.as_ref());
    encoder.into_bytes()
}

fn validate_board(board: &[OpenBoardCard]) -> Result<()> {
    if board.len() > 5 {
        return Err(recovery("checkpoint contains more than five board cards"));
    }
    let mut positions = BTreeSet::new();
    let mut cards = BTreeSet::new();
    let mut previous_position = None;
    for open in board {
        if previous_position.is_some_and(|previous| previous >= open.position) {
            return Err(recovery(
                "checkpoint board positions are not in strictly increasing order",
            ));
        }
        previous_position = Some(open.position);
        if !positions.insert(open.position) {
            return Err(recovery("checkpoint repeats a board position"));
        }
        if !cards.insert(open.card) {
            return Err(recovery("checkpoint repeats a board card"));
        }
    }
    Ok(())
}

const fn encode_status(status: HandSeatStatus) -> u8 {
    match status {
        HandSeatStatus::Playing => 0,
        HandSeatStatus::Folded => 1,
        HandSeatStatus::AllIn => 2,
        HandSeatStatus::NotDealt => 3,
    }
}

fn decode_status(value: u8) -> Result<HandSeatStatus> {
    match value {
        0 => Ok(HandSeatStatus::Playing),
        1 => Ok(HandSeatStatus::Folded),
        2 => Ok(HandSeatStatus::AllIn),
        3 => Ok(HandSeatStatus::NotDealt),
        _ => Err(codec(format!("unknown hand-seat status tag {value}"))),
    }
}

fn decode_phase(value: u8) -> Result<HandPhase> {
    // Keep this explicit rather than transmuting a repr(u8) enum.  Besides avoiding
    // unsafe code, it preserves forward-compatible rejection of unknown tags.
    match value {
        0 => Ok(HandPhase::KeySetup),
        1 => Ok(HandPhase::Shuffle),
        2 => Ok(HandPhase::DeckReady),
        3 => Ok(HandPhase::HoleCards),
        4 => Ok(HandPhase::Preflop),
        5 => Ok(HandPhase::FlopReveal),
        6 => Ok(HandPhase::Flop),
        7 => Ok(HandPhase::TurnReveal),
        8 => Ok(HandPhase::Turn),
        9 => Ok(HandPhase::RiverReveal),
        10 => Ok(HandPhase::River),
        11 => Ok(HandPhase::Showdown),
        12 => Ok(HandPhase::Payout),
        13 => Ok(HandPhase::Complete),
        14 => Ok(HandPhase::Recovery),
        _ => Err(codec(format!("unknown checkpoint phase tag {value}"))),
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rand::rngs::OsRng;

    use super::*;
    use crate::config::{EconomicSecurity, Roster};
    use crate::crypto::{MentalPoker, ProofContext};
    use crate::identity::IdentitySecret;

    fn setup() -> (SessionConfig, Vec<IdentitySecret>) {
        let mut rng = OsRng;
        let identities: Vec<_> = (0..3).map(|_| IdentitySecret::generate(&mut rng)).collect();
        let roster = Roster::new(identities.iter().map(IdentitySecret::public).collect()).unwrap();
        let economics = EconomicSecurity::new(
            3,
            Chips::from_units(100),
            Chips::from_units(101),
            Chips::from_units(202),
        )
        .unwrap();
        let config = SessionConfig::builder(
            SessionId::random(&mut rng),
            roster,
            Chips::from_units(1),
            Chips::from_units(2),
            Chips::from_units(100),
        )
        .economics(economics)
        .build()
        .unwrap();
        (config, identities)
    }

    #[test]
    fn certificate_validation_rechecks_semantic_ledger_invariants() {
        let (config, identities) = setup();
        let ledger = HandLedger::new(
            &[Chips::from_units(100); 3],
            Seat::new(0).unwrap(),
            Chips::from_units(2),
        )
        .unwrap();
        let receipts = HoleCardReceipts::empty(&config);
        let non_conserving_ledger = HandLedger::new(
            &[
                Chips::from_units(100),
                Chips::from_units(100),
                Chips::from_units(99),
            ],
            Seat::new(0).unwrap(),
            Chips::from_units(2),
        )
        .unwrap();
        assert!(
            CheckpointState::capture(
                &config,
                HandNumber::FIRST,
                HandPhase::Preflop,
                TranscriptAnchor::EMPTY,
                Some(&non_conserving_ledger),
                &[],
                None,
                &receipts,
                Some(Seat::new(0).unwrap()),
                Digest32::ZERO,
            )
            .is_err()
        );

        let base = CheckpointState::capture(
            &config,
            HandNumber::FIRST,
            HandPhase::Preflop,
            TranscriptAnchor::EMPTY,
            Some(&ledger),
            &[],
            None,
            &receipts,
            Some(Seat::new(0).unwrap()),
            Digest32::ZERO,
        )
        .unwrap();

        let mut impossible_all_in = base.clone();
        impossible_all_in.seats[0].status = HandSeatStatus::AllIn;
        assert!(
            CheckpointVote::sign(
                &impossible_all_in,
                Seat::new(0).unwrap(),
                &config,
                &identities[0],
            )
            .is_err()
        );

        let mut excessive_contribution = base.clone();
        excessive_contribution.seats[0].total_contributed = Chips::from_units(101);
        assert!(
            CheckpointVote::sign(
                &excessive_contribution,
                Seat::new(0).unwrap(),
                &config,
                &identities[0],
            )
            .is_err()
        );

        let mut non_conserving = base.clone();
        non_conserving.seats[0].stack = Chips::from_units(99);
        assert!(
            CheckpointVote::sign(
                &non_conserving,
                Seat::new(0).unwrap(),
                &config,
                &identities[0],
            )
            .is_err()
        );

        let mut partial_ledger = CheckpointState::capture(
            &config,
            HandNumber::FIRST,
            HandPhase::KeySetup,
            TranscriptAnchor::EMPTY,
            None,
            &[],
            None,
            &receipts,
            None,
            Digest32::ZERO,
        )
        .unwrap();
        partial_ledger.button = Some(Seat::new(0).unwrap());
        assert!(
            CheckpointVote::sign(
                &partial_ledger,
                Seat::new(0).unwrap(),
                &config,
                &identities[0],
            )
            .is_err()
        );
    }

    #[test]
    fn certified_deck_restoration_is_bound_to_threshold_membership() {
        let (config, identities) = setup();
        let mut rng = OsRng;
        let mental = MentalPoker::new();
        let key_context = ProofContext::from_digest(Digest32::hash(b"checkpoint-key", &[]));
        let keys: Vec<_> = (0..config.roster().len())
            .map(|_| mental.generate_key_share(&mut rng, key_context))
            .collect();
        let verified: Vec<_> = keys
            .iter()
            .map(|key| {
                mental
                    .verify_key_share(key.public(), key.proof(), key_context)
                    .expect("ownership proof")
            })
            .collect();
        let aggregate = mental.aggregate_public_keys(&verified).expect("aggregate");
        let shuffle_context = ProofContext::from_digest(Digest32::hash(b"checkpoint-shuffle", &[]));
        let contribution = mental.shuffle_initial(&mut rng, &aggregate, shuffle_context);
        let deck = mental
            .verify_initial_shuffle(&aggregate, contribution, shuffle_context)
            .expect("shuffle proof");

        let receipts = HoleCardReceipts::empty(&config);
        let state = CheckpointState::capture(
            &config,
            HandNumber::FIRST,
            HandPhase::DeckReady,
            TranscriptAnchor::EMPTY,
            None,
            &[],
            Some(&deck),
            &receipts,
            None,
            Digest32::ZERO,
        )
        .expect("checkpoint state");
        let votes = identities
            .iter()
            .enumerate()
            .map(|(index, identity)| {
                CheckpointVote::sign(&state, Seat::new(index).expect("seat"), &config, identity)
                    .expect("vote")
            })
            .collect();
        let certificate =
            CertifiedCheckpoint::assemble(state, votes, &config).expect("certificate");
        let restored = certificate
            .restore_verified_deck(&config, &aggregate)
            .expect("restore")
            .expect("deck");
        assert_eq!(restored.digest(), deck.digest());

        let other_keys: Vec<_> = (0..config.roster().len())
            .map(|_| mental.generate_key_share(&mut rng, key_context))
            .collect();
        let other_verified: Vec<_> = other_keys
            .iter()
            .map(|key| {
                mental
                    .verify_key_share(key.public(), key.proof(), key_context)
                    .expect("ownership proof")
            })
            .collect();
        let other_aggregate = mental
            .aggregate_public_keys(&other_verified)
            .expect("aggregate");
        assert!(
            certificate
                .restore_verified_deck(&config, &other_aggregate)
                .is_err()
        );
    }

    #[test]
    fn certificate_round_trips_and_requires_every_vote() {
        let (config, identities) = setup();
        let mut receipts = HoleCardReceipts::empty(&config);
        let receipt_digest = Digest32::hash(b"test-hole-card-receipt", &[]);
        receipts
            .record(Seat::new(1).unwrap(), receipt_digest)
            .unwrap();
        assert!(
            receipts
                .record(
                    Seat::new(1).unwrap(),
                    Digest32::hash(b"conflicting-receipt", &[]),
                )
                .is_err()
        );
        let state = CheckpointState::capture(
            &config,
            HandNumber::FIRST,
            HandPhase::KeySetup,
            TranscriptAnchor::EMPTY,
            None,
            &[],
            None,
            &receipts,
            None,
            Digest32::ZERO,
        )
        .unwrap();
        let votes: Vec<_> = identities
            .iter()
            .enumerate()
            .map(|(index, identity)| {
                CheckpointVote::sign(&state, Seat::new(index).unwrap(), &config, identity).unwrap()
            })
            .collect();
        let certificate = CertifiedCheckpoint::assemble(state, votes, &config).unwrap();
        let bytes = certificate.to_bytes().unwrap();
        let decoded = CertifiedCheckpoint::from_bytes(&bytes).unwrap();
        decoded.verify(&config).unwrap();
        assert_eq!(
            decoded
                .state()
                .hole_receipts()
                .receipt(Seat::new(1).unwrap())
                .unwrap(),
            Some(receipt_digest)
        );
        assert_eq!(decoded.digest().unwrap(), certificate.digest().unwrap());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn bounded_checkpoint_decoders_never_panic_on_untrusted_bytes(
            bytes in prop::collection::vec(any::<u8>(), 0..8_192),
        ) {
            let _state = CheckpointState::from_bytes(&bytes);
            let _certificate = CertifiedCheckpoint::from_bytes(&bytes);
        }
    }
}
