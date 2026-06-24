//! Transport-neutral peer sessions for dealerless Hold'em.
//!
//! `mtholdem-peer` owns local peer orchestration but no transport.  It turns verified
//! `mtholdem` state transitions into [`HostEffect`] values that a CLI, browser, Godot
//! wrapper, relay, or test harness can carry over its own network and persistence
//! layer.

#![forbid(unsafe_code)]

use std::collections::VecDeque;

use mental_card_core::crypto::{MentalKeyShare, RevealShare, RevealShareProof};
use mental_card_core::error::{Result, protocol};
use mental_card_core::identity::IdentitySecret;
use mtholdem::betting::{BetAction, LegalActions, Street};
use mtholdem::checkpoint::{
    CertifiedCheckpoint, CheckpointState, CheckpointVote, HoleCardReceipts, OpenBoardCard,
};
use mtholdem::config::SessionConfig;
use mtholdem::deal::DealPlan;
use mtholdem::holdem::{HandProgress, HoldemHand};
use mtholdem::protocol::HandCryptography;
use mtholdem::recovery::RecoveryBundle;
use mtholdem::transcript::{PublicTranscript, TranscriptAnchor};
use mtholdem::wire::{
    HandPhase, PublicPayload, ShowdownBundle, SignedPrivateReveal, SignedPublicMessage,
};
use mtholdem::{Card, CardPosition, Chips, Digest32, HandNumber, Seat};
use rand::{CryptoRng, RngCore};

/// An action the host application must perform outside the protocol crate.
///
/// Effects are deliberately typed around `mtholdem` artifacts.  FFI, WASM, and JSON
/// adapters may encode them at their boundary, but the Rust peer layer keeps the
/// canonical protocol values intact.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum HostEffect {
    /// Broadcast one signed public event to every peer in the session.
    BroadcastPublic(SignedPublicMessage),
    /// Send one confidential private reveal to a single recipient.
    SendPrivate {
        /// Recipient of the confidential artifact.
        recipient: Seat,
        /// Signed private reveal to carry over an authenticated confidential channel.
        message: SignedPrivateReveal,
    },
    /// Atomically persist the public transcript anchor and optional checkpoint.
    Persist {
        /// Latest verified public transcript anchor.
        transcript_anchor: TranscriptAnchor,
        /// Optional certified checkpoint to store with the anchor.
        checkpoint: Option<CertifiedCheckpoint>,
    },
    /// Ask the host UI or bot policy for a legal player action.
    RequestAction {
        /// Seat expected to choose an action.
        actor: Seat,
        /// Legal choices according to the deterministic rules engine.
        legal_actions: LegalActions,
    },
    /// Submit recovery evidence to an external mediator.
    SubmitRecovery(RecoveryBundle),
    /// Publish a showdown side artifact whose digest is committed in the public transcript.
    PublishShowdownBundle {
        /// Owner of the disclosed private cards.
        owner: Seat,
        /// Signed side artifact containing all private reveal shares for the owner.
        bundle: ShowdownBundle,
    },
    /// The peer has a final hand result ready for host settlement or display.
    Finished(FinishedHand),
}

/// Final peer-visible hand summary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinishedHand {
    hand: HandNumber,
    transcript_anchor: TranscriptAnchor,
}

impl FinishedHand {
    /// Creates a final hand summary.
    #[must_use]
    pub const fn new(hand: HandNumber, transcript_anchor: TranscriptAnchor) -> Self {
        Self {
            hand,
            transcript_anchor,
        }
    }

    /// Returns the completed hand number.
    #[must_use]
    pub const fn hand(self) -> HandNumber {
        self.hand
    }

    /// Returns the final public transcript anchor.
    #[must_use]
    pub const fn transcript_anchor(self) -> TranscriptAnchor {
        self.transcript_anchor
    }
}

/// Coarse progress of a peer session through the current implemented slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerPhase {
    /// Waiting for one key announcement per roster seat.
    KeySetup,
    /// Waiting for one verified shuffle contribution per roster seat.
    Shuffle,
    /// Delivering private hole-card shares and public receipt commitments.
    PrivateReveal,
    /// Every dealt seat has acknowledged its private cards.
    HoleCardsReady,
    /// Deterministic betting is waiting for the named street's actor.
    Betting(Street),
    /// Opening public community cards for the named reveal phase.
    BoardReveal(HandPhase),
    /// All community cards are open and showdown orchestration has not been completed yet.
    Showdown,
    /// The hand completed without requiring more public card reveals.
    Complete,
}

/// Read-only status for host event loops.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeerStatus {
    phase: PeerPhase,
    local_seat: Seat,
    next_public_sender: Option<Seat>,
    transcript_anchor: TranscriptAnchor,
}

#[derive(Clone, Debug)]
struct BoardRevealState {
    phase: HandPhase,
    positions: Vec<CardPosition>,
    position_index: usize,
    shares: Vec<(Seat, RevealShare, RevealShareProof)>,
    opened_cards: Vec<Card>,
}

impl BoardRevealState {
    fn new(phase: HandPhase, positions: impl IntoIterator<Item = CardPosition>) -> Self {
        Self {
            phase,
            positions: positions.into_iter().collect(),
            position_index: 0,
            shares: Vec::new(),
            opened_cards: Vec::new(),
        }
    }

    fn current_position(&self) -> Option<CardPosition> {
        self.positions.get(self.position_index).copied()
    }

    fn next_share_sender(&self) -> Seat {
        Seat::new(self.shares.len()).expect("share sender index fits in a seat")
    }

    fn all_shares_collected(&self, roster_len: usize) -> bool {
        self.shares.len() == roster_len
    }

    fn advance_position(&mut self, card: Card) {
        self.opened_cards.push(card);
        self.position_index += 1;
        self.shares.clear();
    }

    fn is_complete(&self) -> bool {
        self.position_index == self.positions.len()
    }
}

impl PeerStatus {
    /// Returns the current peer phase.
    #[must_use]
    pub const fn phase(self) -> PeerPhase {
        self.phase
    }

    /// Returns the local peer's seat.
    #[must_use]
    pub const fn local_seat(self) -> Seat {
        self.local_seat
    }

    /// Returns the next seat expected to broadcast, if any.
    #[must_use]
    pub const fn next_public_sender(self) -> Option<Seat> {
        self.next_public_sender
    }

    /// Returns the latest verified transcript anchor.
    #[must_use]
    pub const fn transcript_anchor(self) -> TranscriptAnchor {
        self.transcript_anchor
    }

    /// Returns whether the local peer can emit the next public message.
    #[must_use]
    pub fn local_can_send(self) -> bool {
        self.next_public_sender == Some(self.local_seat)
    }
}

/// Per-peer state for one hand.
///
/// A `PeerSession` owns local identity and key-share material, verifies every public
/// artifact through `mtholdem`, and reports host work as effects.  It does not open
/// sockets, wait on clocks, or write storage.
#[derive(Debug)]
pub struct PeerSession {
    config: SessionConfig,
    local_identity: IdentitySecret,
    local_seat: Seat,
    hand: HandNumber,
    deal_plan: DealPlan,
    cryptography: HandCryptography,
    transcript: PublicTranscript,
    local_key_share: Option<MentalKeyShare>,
    registered_keys: usize,
    key_setup_finalized: bool,
    sent_private_reveals: bool,
    local_private_reveals: Vec<SignedPrivateReveal>,
    local_hole_cards: Option<[Card; 2]>,
    local_receipt_digest: Option<Digest32>,
    hole_receipts: Vec<Option<Digest32>>,
    holdem: Option<HoldemHand>,
    requested_action: Option<Seat>,
    board: Vec<OpenBoardCard>,
    board_reveal: Option<BoardRevealState>,
    showdown_commitments: Vec<Option<Digest32>>,
    pending_showdown_bundles: Vec<Option<ShowdownBundle>>,
    showdown_hands: Vec<Option<[Card; 2]>>,
    payout_emitted: bool,
    latest_checkpoint: Option<CertifiedCheckpoint>,
    finished_emitted: bool,
    effects: VecDeque<HostEffect>,
}

impl PeerSession {
    /// Creates local peer state for one hand.
    pub fn new(
        config: SessionConfig,
        local_identity: IdentitySecret,
        local_seat: Seat,
        hand: HandNumber,
    ) -> Result<Self> {
        let active_seats = (0..config.roster().len())
            .map(|index| Seat::new(index).expect("roster index fits in a seat"))
            .collect::<Vec<_>>();
        let deal_plan = DealPlan::new(
            config.roster().len(),
            Seat::new(0).expect("zero is a valid seat"),
            &active_seats,
        )?;
        Self::with_deal_plan(config, local_identity, local_seat, hand, deal_plan)
    }

    /// Creates local peer state for one hand with an explicit public deal plan.
    pub fn with_deal_plan(
        config: SessionConfig,
        local_identity: IdentitySecret,
        local_seat: Seat,
        hand: HandNumber,
        deal_plan: DealPlan,
    ) -> Result<Self> {
        let expected = config.roster().participant(local_seat)?.player_id();
        if local_identity.public().player_id() != expected {
            return Err(protocol(format!(
                "local identity does not belong to declared {local_seat}"
            )));
        }
        Self::validate_deal_plan(&config, &deal_plan)?;
        let roster_len = config.roster().len();
        Ok(Self {
            cryptography: HandCryptography::new(config.clone(), hand),
            transcript: PublicTranscript::new(config.clone(), hand),
            config,
            local_identity,
            local_seat,
            hand,
            deal_plan,
            local_key_share: None,
            registered_keys: 0,
            key_setup_finalized: false,
            sent_private_reveals: false,
            local_private_reveals: Vec::new(),
            local_hole_cards: None,
            local_receipt_digest: None,
            hole_receipts: vec![None; roster_len],
            holdem: None,
            requested_action: None,
            board: Vec::new(),
            board_reveal: None,
            showdown_commitments: vec![None; roster_len],
            pending_showdown_bundles: vec![None; roster_len],
            showdown_hands: vec![None; roster_len],
            payout_emitted: false,
            latest_checkpoint: None,
            finished_emitted: false,
            effects: VecDeque::new(),
        })
    }

    /// Starts or resumes local progress, emitting a message when it is this peer's turn.
    pub fn progress(&mut self, rng: &mut (impl RngCore + CryptoRng)) -> Result<()> {
        loop {
            let emitted = match self.status().phase() {
                PeerPhase::KeySetup if self.status().local_can_send() => {
                    self.emit_local_key_announcement(rng)?;
                    true
                }
                PeerPhase::Shuffle if self.status().local_can_send() => {
                    self.emit_local_shuffle(rng)?;
                    true
                }
                PeerPhase::PrivateReveal => {
                    let mut emitted = if self.sent_private_reveals {
                        false
                    } else {
                        self.emit_local_private_reveals(rng)?;
                        true
                    };
                    if self.next_receipt_sender() == Some(self.local_seat)
                        && self.local_receipt_digest.is_some()
                    {
                        self.emit_local_hole_receipt()?;
                        emitted = true;
                    }
                    emitted
                }
                PeerPhase::HoleCardsReady => {
                    self.start_betting_hand()?;
                    true
                }
                PeerPhase::Betting(_) => self.emit_action_request()?,
                PeerPhase::BoardReveal(_) => {
                    if self.board_reveal.is_none() {
                        self.start_board_reveal()?;
                        true
                    } else if self.next_public_sender() == Some(self.local_seat) {
                        self.emit_local_board_reveal(rng)?;
                        true
                    } else {
                        false
                    }
                }
                PeerPhase::Showdown => {
                    if self.next_public_sender() == Some(self.local_seat) {
                        self.emit_local_showdown_message()?;
                        true
                    } else {
                        false
                    }
                }
                PeerPhase::Complete => self.emit_finished(),
                PeerPhase::KeySetup | PeerPhase::Shuffle => false,
            };
            if !emitted {
                break;
            }
        }
        Ok(())
    }

    /// Signs, applies, and broadcasts the local player's chosen betting action.
    pub fn submit_action(&mut self, action: BetAction) -> Result<()> {
        let Some(actor) = self.holdem.as_ref().and_then(HoldemHand::next_to_act) else {
            return Err(protocol("no betting action is currently expected"));
        };
        if actor != self.local_seat {
            return Err(protocol(format!("expected action from {actor}")));
        }
        let HandProgress::Betting(street) = self
            .holdem
            .as_ref()
            .expect("actor exists only while betting")
            .progress()
        else {
            return Err(protocol("hand is not in a betting phase"));
        };
        let payload = PublicPayload::Bet { street, action };
        let message = self.sign_public(hand_phase_for_street(street), payload)?;
        self.apply_verified_public_message(message.clone())?;
        self.effects.push_back(HostEffect::BroadcastPublic(message));
        self.persist_anchor();
        self.progress_without_rng()
    }

    /// Applies one public message delivered by the host transport.
    pub fn apply_public_message(
        &mut self,
        message: SignedPublicMessage,
        rng: &mut (impl RngCore + CryptoRng),
    ) -> Result<()> {
        self.apply_verified_public_message(message)?;
        self.progress(rng)
    }

    /// Parses and applies one canonical public-message byte string.
    pub fn apply_public_bytes(
        &mut self,
        bytes: &[u8],
        rng: &mut (impl RngCore + CryptoRng),
    ) -> Result<()> {
        self.apply_public_message(SignedPublicMessage::from_bytes(bytes)?, rng)
    }

    /// Applies one confidential private reveal delivered by the host transport.
    pub fn apply_private_message(&mut self, message: SignedPrivateReveal) -> Result<()> {
        if self.phase() != PeerPhase::PrivateReveal {
            return Err(protocol("private reveal is not expected in this phase"));
        }
        if message.recipient() != self.local_seat {
            return Err(protocol("private reveal is addressed to another peer"));
        }
        let positions = self
            .deal_plan
            .hole_cards(self.local_seat)
            .ok_or_else(|| protocol("local peer is not dealt into this hand"))?
            .as_array();
        if !positions.contains(&message.position()) {
            return Err(protocol("private reveal is for another card position"));
        }
        self.cryptography.verify_private_reveal(&message)?;
        if self.local_private_reveals.iter().any(|existing| {
            existing.sender() == message.sender() && existing.position() == message.position()
        }) {
            return Err(protocol("duplicate private reveal for sender and position"));
        }
        self.local_private_reveals.push(message);
        self.try_open_local_private_hand()?;
        if self.next_receipt_sender() == Some(self.local_seat)
            && self.local_receipt_digest.is_some()
        {
            self.emit_local_hole_receipt()?;
        }
        Ok(())
    }

    /// Parses and applies one canonical private-reveal byte string.
    pub fn apply_private_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        self.apply_private_message(SignedPrivateReveal::from_bytes(bytes)?)
    }

    /// Captures the current peer-visible state as a checkpoint candidate.
    pub fn checkpoint_state(&self) -> Result<CheckpointState> {
        CheckpointState::capture(
            &self.config,
            self.hand,
            self.checkpoint_phase(),
            self.transcript.anchor(),
            self.holdem.as_ref().map(HoldemHand::ledger),
            &self.open_board_cards(),
            self.cryptography.deck().ok(),
            &self.hole_card_receipts()?,
            self.holdem.as_ref().and_then(HoldemHand::next_to_act),
            Digest32::ZERO,
        )
    }

    /// Signs this peer's current checkpoint candidate.
    pub fn sign_checkpoint_vote(&self) -> Result<CheckpointVote> {
        CheckpointVote::sign(
            &self.checkpoint_state()?,
            self.local_seat,
            &self.config,
            &self.local_identity,
        )
    }

    /// Assembles a certified checkpoint for the current state from unanimous votes.
    pub fn certify_checkpoint(
        &mut self,
        votes: Vec<CheckpointVote>,
    ) -> Result<CertifiedCheckpoint> {
        let checkpoint =
            CertifiedCheckpoint::assemble(self.checkpoint_state()?, votes, &self.config)?;
        self.latest_checkpoint = Some(checkpoint.clone());
        self.effects.push_back(HostEffect::Persist {
            transcript_anchor: self.transcript.anchor(),
            checkpoint: Some(checkpoint.clone()),
        });
        Ok(checkpoint)
    }

    /// Verifies and stores a certified checkpoint supplied by the host.
    pub fn apply_certified_checkpoint(&mut self, checkpoint: CertifiedCheckpoint) -> Result<()> {
        checkpoint.verify(&self.config)?;
        if checkpoint.state().hand() != self.hand {
            return Err(protocol("checkpoint belongs to another hand"));
        }
        if checkpoint.transcript_anchor().next_sequence() > self.transcript.anchor().next_sequence()
        {
            return Err(protocol("checkpoint is ahead of the local transcript"));
        }
        self.latest_checkpoint = Some(checkpoint.clone());
        self.effects.push_back(HostEffect::Persist {
            transcript_anchor: self.transcript.anchor(),
            checkpoint: Some(checkpoint),
        });
        Ok(())
    }

    /// Broadcasts a public commitment to an already certified checkpoint.
    pub fn announce_checkpoint(&mut self, checkpoint: CertifiedCheckpoint) -> Result<()> {
        checkpoint.verify(&self.config)?;
        if checkpoint.transcript_anchor() != self.transcript.anchor() {
            return Err(protocol(
                "checkpoint announcement must certify the current transcript anchor",
            ));
        }
        let checkpoint_digest = checkpoint.digest()?;
        let message = self.sign_public(
            checkpoint.state().phase(),
            PublicPayload::Checkpoint { checkpoint_digest },
        )?;
        self.apply_verified_public_message(message.clone())?;
        self.latest_checkpoint = Some(checkpoint.clone());
        self.effects.push_back(HostEffect::BroadcastPublic(message));
        self.effects.push_back(HostEffect::Persist {
            transcript_anchor: self.transcript.anchor(),
            checkpoint: Some(checkpoint),
        });
        Ok(())
    }

    /// Builds recovery evidence from the latest certified checkpoint.
    pub fn recovery_bundle(&self) -> Result<RecoveryBundle> {
        let checkpoint = self
            .latest_checkpoint
            .clone()
            .ok_or_else(|| protocol("no certified checkpoint is available for recovery"))?;
        let start = checkpoint.transcript_anchor().next_sequence();
        let messages = self
            .transcript
            .messages()
            .iter()
            .filter(|message| message.sequence() >= start)
            .cloned()
            .collect();
        let bundle = RecoveryBundle::new(checkpoint, messages);
        bundle.verify(&self.config)?;
        Ok(bundle)
    }

    /// Broadcasts a recovery request and submits the matching evidence bundle to the host.
    pub fn request_recovery(&mut self) -> Result<()> {
        let checkpoint_digest = self
            .latest_checkpoint
            .as_ref()
            .ok_or_else(|| protocol("no certified checkpoint is available for recovery"))?
            .digest()?;
        let message = self.sign_public(
            HandPhase::Recovery,
            PublicPayload::RecoveryRequested { checkpoint_digest },
        )?;
        self.apply_verified_public_message(message.clone())?;
        let bundle = self.recovery_bundle()?;
        self.effects.push_back(HostEffect::BroadcastPublic(message));
        self.effects.push_back(HostEffect::SubmitRecovery(bundle));
        self.persist_anchor();
        Ok(())
    }

    /// Applies one showdown side artifact delivered by the host transport.
    pub fn apply_showdown_bundle(&mut self, bundle: ShowdownBundle) -> Result<()> {
        bundle.verify(self.config.roster())?;
        let owner = bundle.owner();
        if owner.index() >= self.config.roster().len() {
            return Err(protocol("showdown bundle owner is outside roster"));
        }
        if !self.is_live_showdown_seat(owner)? {
            return Err(protocol("showdown bundle owner is not live at showdown"));
        }
        self.pending_showdown_bundles[owner.index()] = Some(bundle);
        self.try_open_showdown_bundle(owner)
    }

    /// Returns immutable session configuration.
    #[must_use]
    pub const fn config(&self) -> &SessionConfig {
        &self.config
    }

    /// Returns the local peer seat.
    #[must_use]
    pub const fn local_seat(&self) -> Seat {
        self.local_seat
    }

    /// Returns the hand number.
    #[must_use]
    pub const fn hand(&self) -> HandNumber {
        self.hand
    }

    /// Returns the current verified public transcript anchor.
    #[must_use]
    pub fn transcript_anchor(&self) -> TranscriptAnchor {
        self.transcript.anchor()
    }

    /// Returns verified public messages retained after the current anchor.
    #[must_use]
    pub fn public_messages(&self) -> &[SignedPublicMessage] {
        self.transcript.messages()
    }

    /// Returns the current peer status.
    #[must_use]
    pub fn status(&self) -> PeerStatus {
        PeerStatus {
            phase: self.phase(),
            local_seat: self.local_seat,
            next_public_sender: self.next_public_sender(),
            transcript_anchor: self.transcript.anchor(),
        }
    }

    /// Returns whether key setup and sequential shuffling are complete.
    #[must_use]
    pub fn deck_ready(&self) -> bool {
        self.cryptography.deck_ready()
    }

    /// Returns the locally opened private cards, if all required shares arrived.
    #[must_use]
    pub const fn local_hole_cards(&self) -> Option<[Card; 2]> {
        self.local_hole_cards
    }

    /// Returns the local private-card receipt digest, if available.
    #[must_use]
    pub const fn local_receipt_digest(&self) -> Option<Digest32> {
        self.local_receipt_digest
    }

    /// Returns community cards opened through public reveal shares.
    #[must_use]
    pub fn board(&self) -> &[OpenBoardCard] {
        &self.board
    }

    /// Returns a disclosed showdown hand, if that seat opened successfully.
    #[must_use]
    pub fn showdown_hand(&self, seat: Seat) -> Option<[Card; 2]> {
        self.showdown_hands.get(seat.index()).copied().flatten()
    }

    /// Removes and returns all pending host effects in emission order.
    pub fn drain_effects(&mut self) -> Vec<HostEffect> {
        self.effects.drain(..).collect()
    }

    fn phase(&self) -> PeerPhase {
        if self.registered_keys < self.config.roster().len() {
            PeerPhase::KeySetup
        } else if !self.cryptography.deck_ready() {
            PeerPhase::Shuffle
        } else if self.hole_receipts.iter().any(Option::is_none) {
            PeerPhase::PrivateReveal
        } else if let Some(holdem) = &self.holdem {
            if holdem.progress() == HandProgress::Complete && self.showdown_payout_pending() {
                return PeerPhase::Showdown;
            }
            match holdem.progress() {
                HandProgress::Betting(street) => PeerPhase::Betting(street),
                HandProgress::AwaitingFlop => PeerPhase::BoardReveal(HandPhase::FlopReveal),
                HandProgress::AwaitingTurn => PeerPhase::BoardReveal(HandPhase::TurnReveal),
                HandProgress::AwaitingRiver => PeerPhase::BoardReveal(HandPhase::RiverReveal),
                HandProgress::Showdown => PeerPhase::Showdown,
                _ => PeerPhase::Complete,
            }
        } else {
            PeerPhase::HoleCardsReady
        }
    }

    fn next_public_sender(&self) -> Option<Seat> {
        match self.phase() {
            PeerPhase::KeySetup => {
                Some(Seat::new(self.registered_keys).expect("roster index fits in a seat"))
            }
            PeerPhase::Shuffle => self.cryptography.next_shuffler(),
            PeerPhase::PrivateReveal => self.next_receipt_sender(),
            PeerPhase::Betting(_) => self.holdem.as_ref().and_then(HoldemHand::next_to_act),
            PeerPhase::BoardReveal(_) => self.next_board_reveal_sender(),
            PeerPhase::Showdown => self.next_showdown_sender(),
            PeerPhase::HoleCardsReady | PeerPhase::Complete => None,
        }
    }

    fn next_receipt_sender(&self) -> Option<Seat> {
        self.hole_receipts
            .iter()
            .position(Option::is_none)
            .map(|index| Seat::new(index).expect("receipt index fits in a seat"))
    }

    fn emit_local_key_announcement(&mut self, rng: &mut (impl RngCore + CryptoRng)) -> Result<()> {
        if self.local_key_share.is_some() {
            return Err(protocol("local key announcement has already been emitted"));
        }
        let key_share = self.cryptography.generate_key_share(rng, self.local_seat)?;
        let payload = HandCryptography::key_announcement(&key_share);
        let message = self.sign_public(HandPhase::KeySetup, payload)?;
        self.local_key_share = Some(key_share);
        self.apply_verified_public_message(message.clone())?;
        self.effects.push_back(HostEffect::BroadcastPublic(message));
        self.persist_anchor();
        Ok(())
    }

    fn emit_local_shuffle(&mut self, rng: &mut (impl RngCore + CryptoRng)) -> Result<()> {
        let contribution = self.cryptography.create_shuffle(rng, self.local_seat)?;
        let payload = PublicPayload::Shuffle {
            contribution: Box::new(contribution),
        };
        let message = self.sign_public(HandPhase::Shuffle, payload)?;
        self.apply_verified_public_message(message.clone())?;
        self.effects.push_back(HostEffect::BroadcastPublic(message));
        self.persist_anchor();
        Ok(())
    }

    fn emit_local_private_reveals(&mut self, rng: &mut (impl RngCore + CryptoRng)) -> Result<()> {
        let key_share = self
            .local_key_share
            .as_ref()
            .ok_or_else(|| protocol("local key share is not available"))?;
        for recipient_index in 0..self.config.roster().len() {
            let recipient = Seat::new(recipient_index).expect("roster index fits in a seat");
            let positions = self
                .deal_plan
                .hole_cards(recipient)
                .ok_or_else(|| protocol("recipient is not dealt into this hand"))?
                .as_array();
            for position in positions {
                let message = self.cryptography.create_private_reveal(
                    rng,
                    self.local_seat,
                    recipient,
                    position,
                    key_share.secret(),
                    &self.local_identity,
                )?;
                self.effects
                    .push_back(HostEffect::SendPrivate { recipient, message });
            }
        }
        self.sent_private_reveals = true;
        Ok(())
    }

    fn try_open_local_private_hand(&mut self) -> Result<()> {
        if self.local_receipt_digest.is_some() {
            return Ok(());
        }
        let positions = self
            .deal_plan
            .hole_cards(self.local_seat)
            .ok_or_else(|| protocol("local peer is not dealt into this hand"))?
            .as_array();
        let expected = self
            .config
            .roster()
            .len()
            .checked_mul(2)
            .ok_or_else(|| protocol("private reveal count overflow"))?;
        if self.local_private_reveals.len() != expected {
            return Ok(());
        }
        let (cards, receipt_digest) = self.cryptography.open_private_hand(
            self.local_seat,
            positions,
            &self.local_private_reveals,
        )?;
        self.local_hole_cards = Some(cards);
        self.local_receipt_digest = Some(receipt_digest);
        Ok(())
    }

    fn emit_local_hole_receipt(&mut self) -> Result<()> {
        let receipt_digest = self
            .local_receipt_digest
            .ok_or_else(|| protocol("local private-card receipt is not available"))?;
        let payload = PublicPayload::HoleCardsReceived { receipt_digest };
        let message = self.sign_public(HandPhase::HoleCards, payload)?;
        self.apply_verified_public_message(message.clone())?;
        self.effects.push_back(HostEffect::BroadcastPublic(message));
        self.persist_anchor();
        Ok(())
    }

    fn sign_public(&self, phase: HandPhase, payload: PublicPayload) -> Result<SignedPublicMessage> {
        SignedPublicMessage::sign(
            self.config.session_id(),
            self.config.digest(),
            self.hand,
            self.transcript.anchor().next_sequence(),
            phase,
            self.config.roster().participant(self.local_seat)?,
            self.transcript.anchor().digest(),
            payload,
            &self.local_identity,
        )
    }

    fn apply_verified_public_message(&mut self, message: SignedPublicMessage) -> Result<()> {
        match message.payload() {
            PublicPayload::Checkpoint { .. } | PublicPayload::RecoveryRequested { .. } => {
                self.transcript.append(message)?;
                self.persist_anchor();
                return Ok(());
            }
            _ => {}
        }
        match self.phase() {
            PeerPhase::KeySetup => self.apply_key_announcement(message),
            PeerPhase::Shuffle => self.apply_shuffle(message),
            PeerPhase::PrivateReveal => self.apply_hole_receipt(message),
            PeerPhase::Betting(_) => self.apply_bet(message),
            PeerPhase::BoardReveal(_) => self.apply_board_reveal(message),
            PeerPhase::Showdown => self.apply_showdown_public_message(message),
            PeerPhase::HoleCardsReady | PeerPhase::Complete => Err(protocol(
                "no public message is expected in the current peer phase",
            )),
        }
    }

    fn apply_key_announcement(&mut self, message: SignedPublicMessage) -> Result<()> {
        let expected = Seat::new(self.registered_keys).expect("roster index fits in a seat");
        if message.phase() != HandPhase::KeySetup || message.sender() != expected {
            return Err(protocol(format!(
                "expected key announcement from {expected}, got {} during {}",
                message.sender(),
                message.phase()
            )));
        }
        let mut cryptography = self.cryptography.clone();
        cryptography.register_key_payload(message.sender(), message.payload())?;
        self.transcript.append(message)?;
        self.cryptography = cryptography;
        self.registered_keys += 1;
        if self.registered_keys == self.config.roster().len() && !self.key_setup_finalized {
            self.cryptography.finalize_key_setup()?;
            self.key_setup_finalized = true;
        }
        Ok(())
    }

    fn apply_shuffle(&mut self, message: SignedPublicMessage) -> Result<()> {
        let expected = self
            .cryptography
            .next_shuffler()
            .ok_or_else(|| protocol("no shuffle is currently expected"))?;
        if message.phase() != HandPhase::Shuffle || message.sender() != expected {
            return Err(protocol(format!(
                "expected shuffle from {expected}, got {} during {}",
                message.sender(),
                message.phase()
            )));
        }
        let mut cryptography = self.cryptography.clone();
        cryptography.accept_shuffle_payload(message.sender(), message.payload())?;
        self.transcript.append(message)?;
        self.cryptography = cryptography;
        Ok(())
    }

    fn apply_hole_receipt(&mut self, message: SignedPublicMessage) -> Result<()> {
        let expected = self
            .next_receipt_sender()
            .ok_or_else(|| protocol("no hole-card receipt is currently expected"))?;
        if message.phase() != HandPhase::HoleCards || message.sender() != expected {
            return Err(protocol(format!(
                "expected hole-card receipt from {expected}, got {} during {}",
                message.sender(),
                message.phase()
            )));
        }
        let receipt_digest = match message.payload() {
            PublicPayload::HoleCardsReceived { receipt_digest } => *receipt_digest,
            _ => return Err(protocol("expected a hole-card receipt payload")),
        };
        self.transcript.append(message)?;
        self.hole_receipts[expected.index()] = Some(receipt_digest);
        Ok(())
    }

    fn start_betting_hand(&mut self) -> Result<()> {
        if self.holdem.is_some() {
            return Ok(());
        }
        let stacks = vec![self.config.buy_in(); self.config.roster().len()];
        let hand = HoldemHand::new(
            &stacks,
            self.deal_plan.button(),
            self.config.small_blind(),
            self.config.big_blind(),
            self.config.economics().maximum_hand_loss(),
        )?;
        self.holdem = Some(hand);
        self.requested_action = None;
        Ok(())
    }

    fn emit_action_request(&mut self) -> Result<bool> {
        let Some(holdem) = self.holdem.as_ref() else {
            return Ok(false);
        };
        let Some(actor) = holdem.next_to_act() else {
            return Ok(false);
        };
        if actor != self.local_seat || self.requested_action == Some(actor) {
            return Ok(false);
        }
        let legal_actions = holdem.legal_actions()?;
        self.effects.push_back(HostEffect::RequestAction {
            actor,
            legal_actions,
        });
        self.requested_action = Some(actor);
        Ok(true)
    }

    fn emit_finished(&mut self) -> bool {
        if self.finished_emitted {
            return false;
        }
        self.effects
            .push_back(HostEffect::Finished(FinishedHand::new(
                self.hand,
                self.transcript.anchor(),
            )));
        self.finished_emitted = true;
        true
    }

    fn apply_bet(&mut self, message: SignedPublicMessage) -> Result<()> {
        let Some(holdem) = self.holdem.as_ref() else {
            return Err(protocol("betting hand is not initialized"));
        };
        let Some(expected) = holdem.next_to_act() else {
            return Err(protocol("no betting action is currently expected"));
        };
        let PublicPayload::Bet { street, action } = message.payload() else {
            return Err(protocol("expected a betting payload"));
        };
        if message.sender() != expected {
            return Err(protocol(format!(
                "expected betting action from {expected}, got {}",
                message.sender()
            )));
        }
        if message.phase() != hand_phase_for_street(*street) {
            return Err(protocol("betting message phase does not match its street"));
        }
        if holdem.progress() != HandProgress::Betting(*street) {
            return Err(protocol("betting action is for another street"));
        }

        let mut candidate = holdem.clone();
        candidate.apply_action(expected, *action)?;
        self.transcript.append(message)?;
        self.holdem = Some(candidate);
        self.requested_action = None;
        self.settle_uncontested_if_ready()?;
        Ok(())
    }

    fn start_board_reveal(&mut self) -> Result<()> {
        if self.board_reveal.is_some() {
            return Ok(());
        }
        let Some(holdem) = self.holdem.as_ref() else {
            return Err(protocol("betting hand is not initialized"));
        };
        let reveal = match holdem.progress() {
            HandProgress::AwaitingFlop => {
                BoardRevealState::new(HandPhase::FlopReveal, self.deal_plan.flop())
            }
            HandProgress::AwaitingTurn => {
                BoardRevealState::new(HandPhase::TurnReveal, [self.deal_plan.turn()])
            }
            HandProgress::AwaitingRiver => {
                BoardRevealState::new(HandPhase::RiverReveal, [self.deal_plan.river()])
            }
            _ => return Err(protocol("community-card reveal is not expected")),
        };
        self.board_reveal = Some(reveal);
        Ok(())
    }

    fn next_board_reveal_sender(&self) -> Option<Seat> {
        let reveal = self.board_reveal.as_ref()?;
        reveal.current_position()?;
        if reveal.all_shares_collected(self.config.roster().len()) {
            Some(Seat::new(0).expect("zero is a valid seat"))
        } else {
            Some(reveal.next_share_sender())
        }
    }

    fn emit_local_board_reveal(&mut self, rng: &mut (impl RngCore + CryptoRng)) -> Result<()> {
        self.start_board_reveal()?;
        let reveal = self
            .board_reveal
            .as_ref()
            .ok_or_else(|| protocol("community-card reveal is not initialized"))?;
        let position = reveal
            .current_position()
            .ok_or_else(|| protocol("community-card reveal is complete"))?;
        if reveal.all_shares_collected(self.config.roster().len()) {
            self.emit_local_board_card_opened(position)
        } else {
            self.emit_local_public_reveal_share(rng, position)
        }
    }

    fn emit_local_public_reveal_share(
        &mut self,
        rng: &mut (impl RngCore + CryptoRng),
        position: CardPosition,
    ) -> Result<()> {
        let key_share = self
            .local_key_share
            .as_ref()
            .ok_or_else(|| protocol("local key share is not available"))?;
        let reveal = self
            .board_reveal
            .as_ref()
            .ok_or_else(|| protocol("community-card reveal is not initialized"))?;
        let (share, proof) = self.cryptography.create_public_reveal_share(
            rng,
            self.local_seat,
            position,
            key_share.secret(),
        )?;
        let message = self.sign_public(
            reveal.phase,
            PublicPayload::PublicRevealShare {
                position,
                share,
                proof,
            },
        )?;
        self.apply_verified_public_message(message.clone())?;
        self.effects.push_back(HostEffect::BroadcastPublic(message));
        self.persist_anchor();
        Ok(())
    }

    fn emit_local_board_card_opened(&mut self, position: CardPosition) -> Result<()> {
        let reveal = self
            .board_reveal
            .as_ref()
            .ok_or_else(|| protocol("community-card reveal is not initialized"))?;
        let card = self
            .cryptography
            .open_public_card(position, &reveal.shares)?;
        let message = self.sign_public(
            reveal.phase,
            PublicPayload::BoardCardOpened { position, card },
        )?;
        self.apply_verified_public_message(message.clone())?;
        self.effects.push_back(HostEffect::BroadcastPublic(message));
        self.persist_anchor();
        Ok(())
    }

    fn apply_board_reveal(&mut self, message: SignedPublicMessage) -> Result<()> {
        self.start_board_reveal()?;
        match message.payload().clone() {
            PublicPayload::PublicRevealShare {
                position,
                share,
                proof,
            } => self.apply_public_reveal_share(message, position, share, proof),
            PublicPayload::BoardCardOpened { position, card } => {
                self.apply_board_card_opened(message, position, card)
            }
            _ => Err(protocol("expected a community-card reveal payload")),
        }
    }

    fn apply_public_reveal_share(
        &mut self,
        message: SignedPublicMessage,
        position: CardPosition,
        share: RevealShare,
        proof: RevealShareProof,
    ) -> Result<()> {
        let reveal = self
            .board_reveal
            .as_mut()
            .ok_or_else(|| protocol("community-card reveal is not initialized"))?;
        if message.phase() != reveal.phase {
            return Err(protocol("public reveal share is for another phase"));
        }
        if reveal.current_position() != Some(position) {
            return Err(protocol("public reveal share is for another card position"));
        }
        if reveal.all_shares_collected(self.config.roster().len()) {
            return Err(protocol("all public reveal shares are already present"));
        }
        let expected = reveal.next_share_sender();
        if message.sender() != expected {
            return Err(protocol(format!(
                "expected public reveal share from {expected}, got {}",
                message.sender()
            )));
        }
        self.cryptography
            .verify_public_reveal_share(message.sender(), position, share, proof)?;
        self.transcript.append(message)?;
        reveal.shares.push((expected, share, proof));
        Ok(())
    }

    fn apply_board_card_opened(
        &mut self,
        message: SignedPublicMessage,
        position: CardPosition,
        card: Card,
    ) -> Result<()> {
        let reveal = self
            .board_reveal
            .as_mut()
            .ok_or_else(|| protocol("community-card reveal is not initialized"))?;
        if message.phase() != reveal.phase {
            return Err(protocol("opened board card is for another phase"));
        }
        if message.sender() != Seat::new(0).expect("zero is a valid seat") {
            return Err(protocol("opened board card must be announced by seat 0"));
        }
        if reveal.current_position() != Some(position) {
            return Err(protocol("opened board card is for another position"));
        }
        if !reveal.all_shares_collected(self.config.roster().len()) {
            return Err(protocol(
                "opened board card arrived before all reveal shares",
            ));
        }
        let expected = self
            .cryptography
            .open_public_card(position, &reveal.shares)?;
        if card != expected {
            return Err(protocol("opened board card does not match reveal shares"));
        }
        self.transcript.append(message)?;
        self.board.push(OpenBoardCard::new(position, card));
        reveal.advance_position(card);
        if reveal.is_complete() {
            self.finish_board_reveal()?;
        }
        Ok(())
    }

    fn finish_board_reveal(&mut self) -> Result<()> {
        let reveal = self
            .board_reveal
            .take()
            .ok_or_else(|| protocol("community-card reveal is not initialized"))?;
        let holdem = self
            .holdem
            .as_mut()
            .ok_or_else(|| protocol("betting hand is not initialized"))?;
        match reveal.phase {
            HandPhase::FlopReveal => {
                let flop: [Card; 3] = reveal
                    .opened_cards
                    .try_into()
                    .map_err(|_| protocol("flop reveal did not open three cards"))?;
                holdem.open_flop(flop)?;
            }
            HandPhase::TurnReveal => {
                let [turn] = reveal
                    .opened_cards
                    .try_into()
                    .map_err(|_| protocol("turn reveal did not open one card"))?;
                holdem.open_turn(turn)?;
            }
            HandPhase::RiverReveal => {
                let [river] = reveal
                    .opened_cards
                    .try_into()
                    .map_err(|_| protocol("river reveal did not open one card"))?;
                holdem.open_river(river)?;
            }
            _ => return Err(protocol("unexpected community-card reveal phase")),
        }
        Ok(())
    }

    fn next_showdown_sender(&self) -> Option<Seat> {
        let live = self.live_showdown_seats().ok()?;
        if let Some(seat) = live
            .iter()
            .copied()
            .find(|seat| self.showdown_commitments[seat.index()].is_none())
        {
            return Some(seat);
        }
        if live
            .iter()
            .all(|seat| self.showdown_hands[seat.index()].is_some())
            && !self.payout_emitted
        {
            return Some(Seat::new(0).expect("zero is a valid seat"));
        }
        None
    }

    fn emit_local_showdown_message(&mut self) -> Result<()> {
        let live = self.live_showdown_seats()?;
        if live
            .iter()
            .copied()
            .any(|seat| self.showdown_commitments[seat.index()].is_none())
        {
            self.emit_local_showdown_open()
        } else {
            self.emit_local_payout()
        }
    }

    fn emit_local_showdown_open(&mut self) -> Result<()> {
        if !self.is_live_showdown_seat(self.local_seat)? {
            return Err(protocol("local seat is not live at showdown"));
        }
        if self.showdown_commitments[self.local_seat.index()].is_some() {
            return Err(protocol("local showdown hand is already committed"));
        }
        let positions = self
            .deal_plan
            .hole_cards(self.local_seat)
            .ok_or_else(|| protocol("local peer is not dealt into this hand"))?
            .as_array();
        let bundle = ShowdownBundle::sign(
            self.config.session_id(),
            self.config.digest(),
            self.hand,
            self.cryptography.deck()?.digest(),
            self.local_seat,
            positions,
            self.local_private_reveals.clone(),
            self.config.roster(),
            &self.local_identity,
        )?;
        let bundle_digest = bundle.digest()?;
        let message = self.sign_public(
            HandPhase::Showdown,
            PublicPayload::ShowdownOpen { bundle_digest },
        )?;
        self.apply_verified_public_message(message.clone())?;
        self.apply_showdown_bundle(bundle.clone())?;
        self.effects.push_back(HostEffect::PublishShowdownBundle {
            owner: self.local_seat,
            bundle,
        });
        self.effects.push_back(HostEffect::BroadcastPublic(message));
        self.persist_anchor();
        Ok(())
    }

    fn apply_showdown_public_message(&mut self, message: SignedPublicMessage) -> Result<()> {
        match message.payload().clone() {
            PublicPayload::ShowdownOpen { bundle_digest } => {
                self.apply_showdown_open(message, bundle_digest)
            }
            PublicPayload::Payout { payouts } => self.apply_payout(message, &payouts),
            _ => Err(protocol("expected a showdown or payout payload")),
        }
    }

    fn apply_showdown_open(
        &mut self,
        message: SignedPublicMessage,
        bundle_digest: Digest32,
    ) -> Result<()> {
        if message.phase() != HandPhase::Showdown {
            return Err(protocol("showdown open used the wrong phase"));
        }
        let expected = self
            .next_showdown_sender()
            .ok_or_else(|| protocol("no showdown opening is expected"))?;
        if message.sender() != expected {
            return Err(protocol(format!(
                "expected showdown opening from {expected}, got {}",
                message.sender()
            )));
        }
        self.transcript.append(message)?;
        self.showdown_commitments[expected.index()] = Some(bundle_digest);
        self.try_open_showdown_bundle(expected)
    }

    fn try_open_showdown_bundle(&mut self, owner: Seat) -> Result<()> {
        if self.showdown_hands[owner.index()].is_some() {
            return Ok(());
        }
        let Some(bundle_digest) = self.showdown_commitments[owner.index()] else {
            return Ok(());
        };
        let Some(bundle) = self.pending_showdown_bundles[owner.index()].as_ref() else {
            return Ok(());
        };
        let expected_receipt_digest = self.hole_receipts[owner.index()]
            .ok_or_else(|| protocol("showdown owner has no hole-card receipt"))?;
        let cards = self.cryptography.open_showdown_bundle(
            bundle,
            bundle_digest,
            expected_receipt_digest,
        )?;
        self.showdown_hands[owner.index()] = Some(cards);
        self.settle_showdown_if_ready()
    }

    fn settle_showdown_if_ready(&mut self) -> Result<()> {
        let live = self.live_showdown_seats()?;
        if !live
            .iter()
            .all(|seat| self.showdown_hands[seat.index()].is_some())
        {
            return Ok(());
        }
        let Some(holdem) = self.holdem.as_mut() else {
            return Err(protocol("betting hand is not initialized"));
        };
        if holdem.progress() != HandProgress::Showdown {
            return Ok(());
        }
        let opened = live
            .iter()
            .map(|seat| {
                (
                    *seat,
                    self.showdown_hands[seat.index()]
                        .ok_or_else(|| protocol("showdown hand is missing")),
                )
            })
            .map(|(seat, cards)| cards.map(|cards| (seat, Some(cards))))
            .collect::<Result<Vec<_>>>()?;
        holdem.settle_showdown(&opened)?;
        Ok(())
    }

    fn emit_local_payout(&mut self) -> Result<()> {
        if self.local_seat != Seat::new(0).expect("zero is a valid seat") {
            return Err(protocol("only seat 0 announces deterministic payouts"));
        }
        if self.payout_emitted {
            return Err(protocol("payout has already been emitted"));
        }
        let payouts = self.current_payouts()?;
        let message = self.sign_public(HandPhase::Payout, PublicPayload::Payout { payouts })?;
        self.apply_verified_public_message(message.clone())?;
        self.effects.push_back(HostEffect::BroadcastPublic(message));
        self.persist_anchor();
        self.progress_without_rng()
    }

    fn apply_payout(&mut self, message: SignedPublicMessage, payouts: &[Chips]) -> Result<()> {
        if message.phase() != HandPhase::Payout {
            return Err(protocol("payout message used the wrong phase"));
        }
        if message.sender() != Seat::new(0).expect("zero is a valid seat") {
            return Err(protocol("payout must be announced by seat 0"));
        }
        if self.payout_emitted {
            return Err(protocol("payout has already been applied"));
        }
        let expected = self.current_payouts()?;
        if payouts != expected.as_slice() {
            return Err(protocol(
                "payout vector does not match deterministic settlement",
            ));
        }
        self.transcript.append(message)?;
        self.payout_emitted = true;
        Ok(())
    }

    fn current_payouts(&self) -> Result<Vec<Chips>> {
        let result = self
            .holdem
            .as_ref()
            .and_then(HoldemHand::result)
            .ok_or_else(|| protocol("hand result is not available"))?;
        Ok(result.distribution().payouts().to_vec())
    }

    fn live_showdown_seats(&self) -> Result<Vec<Seat>> {
        let holdem = self
            .holdem
            .as_ref()
            .ok_or_else(|| protocol("betting hand is not initialized"))?;
        Ok(holdem.ledger().live_seats())
    }

    fn is_live_showdown_seat(&self, seat: Seat) -> Result<bool> {
        Ok(self.live_showdown_seats()?.contains(&seat))
    }

    fn showdown_payout_pending(&self) -> bool {
        !self.payout_emitted
            && self.holdem.as_ref().and_then(HoldemHand::result).is_some()
            && self.showdown_hands.iter().any(Option::is_some)
    }

    fn settle_uncontested_if_ready(&mut self) -> Result<()> {
        let Some(holdem) = self.holdem.as_mut() else {
            return Ok(());
        };
        if matches!(holdem.progress(), HandProgress::Uncontested(_)) {
            holdem.settle_uncontested()?;
        }
        Ok(())
    }

    fn progress_without_rng(&mut self) -> Result<()> {
        loop {
            let emitted = match self.status().phase() {
                PeerPhase::Betting(_) => self.emit_action_request()?,
                PeerPhase::Complete => self.emit_finished(),
                _ => false,
            };
            if !emitted {
                break;
            }
        }
        Ok(())
    }

    fn checkpoint_phase(&self) -> HandPhase {
        self.holdem.as_ref().map_or_else(
            || match self.phase() {
                PeerPhase::KeySetup => HandPhase::KeySetup,
                PeerPhase::Shuffle => HandPhase::Shuffle,
                PeerPhase::PrivateReveal | PeerPhase::HoleCardsReady => HandPhase::HoleCards,
                PeerPhase::Betting(street) => hand_phase_for_street(street),
                PeerPhase::BoardReveal(phase) => phase,
                PeerPhase::Showdown => HandPhase::Showdown,
                PeerPhase::Complete => HandPhase::Complete,
            },
            |holdem| match holdem.progress() {
                HandProgress::Betting(street) => hand_phase_for_street(street),
                HandProgress::AwaitingFlop => HandPhase::FlopReveal,
                HandProgress::AwaitingTurn => HandPhase::TurnReveal,
                HandProgress::AwaitingRiver => HandPhase::RiverReveal,
                HandProgress::Showdown => HandPhase::Showdown,
                _ => HandPhase::Complete,
            },
        )
    }

    fn hole_card_receipts(&self) -> Result<HoleCardReceipts> {
        let mut receipts = HoleCardReceipts::empty(&self.config);
        for (index, receipt) in self.hole_receipts.iter().copied().enumerate() {
            if let Some(receipt) = receipt {
                receipts.record(
                    Seat::new(index).expect("roster index fits in a seat"),
                    receipt,
                )?;
            }
        }
        Ok(receipts)
    }

    fn open_board_cards(&self) -> Vec<OpenBoardCard> {
        self.board.clone()
    }

    fn persist_anchor(&mut self) {
        self.effects.push_back(HostEffect::Persist {
            transcript_anchor: self.transcript.anchor(),
            checkpoint: None,
        });
    }

    fn validate_deal_plan(config: &SessionConfig, deal_plan: &DealPlan) -> Result<()> {
        if deal_plan.seat_count() != config.roster().len() {
            return Err(protocol("deal plan seat count does not match roster"));
        }
        if deal_plan.deal_order().len() != config.roster().len() {
            return Err(protocol(
                "initial peer sessions currently require every roster seat to receive cards",
            ));
        }
        for index in 0..config.roster().len() {
            let seat = Seat::new(index).expect("roster index fits in a seat");
            if deal_plan.hole_cards(seat).is_none() {
                return Err(protocol("deal plan omits a roster seat"));
            }
        }
        Ok(())
    }
}

const fn hand_phase_for_street(street: Street) -> HandPhase {
    match street {
        Street::Preflop => HandPhase::Preflop,
        Street::Flop => HandPhase::Flop,
        Street::Turn => HandPhase::Turn,
        Street::River => HandPhase::River,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mtholdem::config::{EconomicSecurity, Roster};
    use mtholdem::{Chips, SessionId};
    use rand::SeedableRng as _;
    use rand::rngs::StdRng;

    enum QueuedEffect {
        Public {
            sender_index: usize,
            message: SignedPublicMessage,
        },
        Private {
            recipient: Seat,
            message: SignedPrivateReveal,
        },
        Showdown {
            owner: Seat,
            bundle: ShowdownBundle,
        },
    }

    #[derive(Clone, Copy)]
    enum ActionPolicy {
        Fold,
        CallOrCheck,
    }

    fn config(seeds: &[[u8; 32]], rng: &mut StdRng) -> SessionConfig {
        let identities: Vec<_> = seeds
            .iter()
            .copied()
            .map(IdentitySecret::from_seed)
            .collect();
        let roster = Roster::new(identities.iter().map(IdentitySecret::public).collect()).unwrap();
        let economics = EconomicSecurity::new(
            seeds.len(),
            Chips::from_units(100),
            Chips::from_units(101),
            Chips::from_units(101 * u64::try_from(seeds.len() - 1).unwrap()),
        )
        .unwrap();
        SessionConfig::builder(
            SessionId::random(rng),
            roster,
            Chips::from_units(1),
            Chips::from_units(2),
            Chips::from_units(100),
        )
        .economics(economics)
        .build()
        .unwrap()
    }

    fn sessions() -> Vec<PeerSession> {
        let mut rng = StdRng::from_seed([0x91; 32]);
        let seeds = [[1; 32], [2; 32], [3; 32]];
        let config = config(&seeds, &mut rng);
        seeds
            .into_iter()
            .enumerate()
            .map(|(index, seed)| {
                PeerSession::new(
                    config.clone(),
                    IdentitySecret::from_seed(seed),
                    Seat::new(index).unwrap(),
                    HandNumber::FIRST,
                )
                .unwrap()
            })
            .collect()
    }

    fn handle_effects(
        sessions: &mut [PeerSession],
        queue: &mut VecDeque<QueuedEffect>,
        rng: &mut StdRng,
        sender_index: usize,
        action_policy: ActionPolicy,
        mut effects: Vec<HostEffect>,
    ) {
        while !effects.is_empty() {
            let mut follow_up = Vec::new();
            for effect in effects {
                match effect {
                    HostEffect::BroadcastPublic(message) => {
                        queue.push_back(QueuedEffect::Public {
                            sender_index,
                            message,
                        });
                    }
                    HostEffect::SendPrivate { recipient, message } => {
                        queue.push_back(QueuedEffect::Private { recipient, message });
                    }
                    HostEffect::PublishShowdownBundle { owner, bundle } => {
                        queue.push_back(QueuedEffect::Showdown { owner, bundle });
                    }
                    HostEffect::RequestAction {
                        actor,
                        legal_actions,
                    } => {
                        assert_eq!(actor.index(), sender_index);
                        let action = match action_policy {
                            ActionPolicy::Fold => BetAction::Fold,
                            ActionPolicy::CallOrCheck if legal_actions.can_check() => {
                                BetAction::Check
                            }
                            ActionPolicy::CallOrCheck => BetAction::Call,
                        };
                        sessions[sender_index].submit_action(action).unwrap();
                        sessions[sender_index].progress(rng).unwrap();
                        follow_up.extend(sessions[sender_index].drain_effects());
                    }
                    HostEffect::Persist { .. }
                    | HostEffect::SubmitRecovery(_)
                    | HostEffect::Finished(_) => {}
                }
            }
            effects = follow_up;
        }
    }

    fn drive(action_policy: ActionPolicy) -> Vec<PeerSession> {
        let mut sessions = sessions();
        let mut rng = StdRng::from_seed([0xA5; 32]);
        let mut queue = VecDeque::new();

        for index in 0..sessions.len() {
            sessions[index].progress(&mut rng).unwrap();
            let effects = sessions[index].drain_effects();
            handle_effects(
                &mut sessions,
                &mut queue,
                &mut rng,
                index,
                action_policy,
                effects,
            );
        }

        while let Some(effect) = queue.pop_front() {
            match effect {
                QueuedEffect::Public {
                    sender_index,
                    message,
                } => {
                    for index in 0..sessions.len() {
                        if index == sender_index {
                            continue;
                        }
                        sessions[index]
                            .apply_public_message(message.clone(), &mut rng)
                            .unwrap();
                        let effects = sessions[index].drain_effects();
                        handle_effects(
                            &mut sessions,
                            &mut queue,
                            &mut rng,
                            index,
                            action_policy,
                            effects,
                        );
                    }
                }
                QueuedEffect::Private { recipient, message } => {
                    let recipient_index = recipient.index();
                    sessions[recipient_index]
                        .apply_private_message(message)
                        .unwrap();
                    sessions[recipient_index].progress(&mut rng).unwrap();
                    let effects = sessions[recipient_index].drain_effects();
                    handle_effects(
                        &mut sessions,
                        &mut queue,
                        &mut rng,
                        recipient_index,
                        action_policy,
                        effects,
                    );
                }
                QueuedEffect::Showdown { owner, bundle } => {
                    for index in 0..sessions.len() {
                        if index == owner.index() {
                            continue;
                        }
                        sessions[index]
                            .apply_showdown_bundle(bundle.clone())
                            .unwrap();
                        sessions[index].progress(&mut rng).unwrap();
                        let effects = sessions[index].drain_effects();
                        handle_effects(
                            &mut sessions,
                            &mut queue,
                            &mut rng,
                            index,
                            action_policy,
                            effects,
                        );
                    }
                }
            }
        }
        sessions
    }

    fn drive_to_uncontested_finish() -> Vec<PeerSession> {
        drive(ActionPolicy::Fold)
    }

    #[test]
    fn peers_exchange_messages_through_uncontested_finish() {
        let sessions = drive_to_uncontested_finish();
        assert!(sessions.iter().all(PeerSession::deck_ready));
        assert!(
            sessions
                .iter()
                .all(|session| session.status().phase() == PeerPhase::Complete)
        );
        assert!(
            sessions
                .iter()
                .all(|session| session.local_hole_cards().is_some())
        );
        assert!(
            sessions
                .iter()
                .all(|session| session.local_receipt_digest().is_some())
        );
        let anchors: Vec<_> = sessions
            .iter()
            .map(PeerSession::transcript_anchor)
            .collect();
        assert!(anchors.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn certified_checkpoint_can_be_announced_and_used_for_recovery() {
        let mut sessions = drive_to_uncontested_finish();
        let votes = sessions
            .iter()
            .map(PeerSession::sign_checkpoint_vote)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let checkpoint = sessions[0].certify_checkpoint(votes).unwrap();
        sessions[0].drain_effects();
        sessions[0].announce_checkpoint(checkpoint.clone()).unwrap();

        let mut rng = StdRng::from_seed([0xC7; 32]);
        let announcement = sessions[0]
            .drain_effects()
            .into_iter()
            .find_map(|effect| match effect {
                HostEffect::BroadcastPublic(message) => Some(message),
                _ => None,
            })
            .expect("checkpoint announcement");
        for session in sessions.iter_mut().skip(1) {
            session
                .apply_public_message(announcement.clone(), &mut rng)
                .unwrap();
            session
                .apply_certified_checkpoint(checkpoint.clone())
                .unwrap();
            session.drain_effects();
        }

        sessions[1].request_recovery().unwrap();
        let bundle = sessions[1]
            .drain_effects()
            .into_iter()
            .find_map(|effect| match effect {
                HostEffect::SubmitRecovery(bundle) => Some(bundle),
                _ => None,
            })
            .expect("recovery bundle");
        let anchor = bundle.verify(sessions[1].config()).unwrap();
        assert_eq!(anchor, sessions[1].transcript_anchor());
        assert_eq!(
            bundle.checkpoint().digest().unwrap(),
            checkpoint.digest().unwrap()
        );
        assert!(!bundle.messages().is_empty());
    }

    #[test]
    fn peers_reveal_board_and_finish_contested_showdown() {
        let sessions = drive(ActionPolicy::CallOrCheck);
        assert!(
            sessions
                .iter()
                .all(|session| session.status().phase() == PeerPhase::Complete)
        );
        let anchors: Vec<_> = sessions
            .iter()
            .map(PeerSession::transcript_anchor)
            .collect();
        assert!(anchors.windows(2).all(|pair| pair[0] == pair[1]));
        assert!(
            sessions
                .iter()
                .all(|session| session.open_board_cards().len() == 5)
        );
        assert!(sessions.iter().all(|session| {
            (0..session.config().roster().len()).all(|index| {
                session
                    .showdown_hand(Seat::new(index).expect("seat"))
                    .is_some()
            })
        }));
    }

    #[test]
    fn rejects_replayed_public_message() {
        let mut sessions = sessions();
        let mut rng = StdRng::from_seed([0xB6; 32]);
        sessions[0].progress(&mut rng).unwrap();
        let message = sessions[0]
            .drain_effects()
            .into_iter()
            .find_map(|effect| match effect {
                HostEffect::BroadcastPublic(message) => Some(message),
                _ => None,
            })
            .expect("initial broadcast");

        sessions[1]
            .apply_public_message(message.clone(), &mut rng)
            .unwrap();
        assert!(sessions[1].apply_public_message(message, &mut rng).is_err());
    }
}
