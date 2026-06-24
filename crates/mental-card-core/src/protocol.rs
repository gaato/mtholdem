//! Game-independent orchestration of one encrypted standard-card deck.
//!
//! [`CardCryptography`] turns the low-level primitives in [`crate::crypto`] into the
//! state machine common to dealerless games that use one shuffled 52-card deck:
//!
//! 1. every roster member proves possession of an ephemeral threshold-key share;
//! 2. those shares are aggregated in canonical seat order;
//! 3. every member contributes exactly one verifiable sequential shuffle; and
//! 4. cards are opened publicly or toward one named recipient using one verified
//!    reveal share from every roster member.
//!
//! The module deliberately has no concept of betting, streets, hole-card counts,
//! showdown, payouts, checkpoints, or recovery.  A game crate owns those rules and
//! signs the resulting low-level values in its own canonical wire format.  This is the
//! practical generalization boundary: the cryptographic card operations are reusable,
//! while game transcripts and certified state remain game-specific until they share a
//! genuinely common state model.

use std::collections::BTreeSet;

use rand::{CryptoRng, RngCore};

use crate::card::Card;
use crate::crypto::{
    AggregateMentalKey, KeyOwnershipProof, MentalKeyShare, MentalPoker, MentalPublicKey,
    MentalSecretKey, ProofContext, RevealShare, RevealShareProof, ShuffleContribution,
    VerifiedDeck, VerifiedMentalPublicKey, VerifiedRevealShare,
};
use crate::error::{Result, protocol};
use crate::session::SessionBinding;
use crate::types::{CardPosition, DealNumber, Digest32, Seat};

/// Visibility and recipient information bound into a reveal-share proof.
///
/// Using an enum rather than `Option<Seat>` makes it difficult to accidentally verify a
/// private share with the public-card domain, or vice versa.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum RevealTarget {
    /// Every peer may reconstruct the card.
    Public,
    /// Only the named seat should receive the shares over a confidential channel.
    Private(Seat),
}

impl RevealTarget {
    const fn context_domain(self) -> &'static [u8] {
        match self {
            Self::Public => b"public-reveal-proof/v1",
            Self::Private(_) => b"private-reveal-proof/v1",
        }
    }

    const fn recipient_byte(self) -> u8 {
        match self {
            Self::Public => u8::MAX,
            Self::Private(recipient) => recipient.to_u8(),
        }
    }
}

/// Verified cryptographic state for one game deal or round.
///
/// Each peer maintains its own instance and feeds it the same public key announcements
/// and shuffle contributions.  Secret key shares are never stored by this type; callers
/// retain their own [`MentalSecretKey`] and pass it only while creating a reveal share.
///
/// The type is Sans-I/O.  It neither signs nor transports messages, and it does not
/// decide when a game is allowed to request an operation.
#[derive(Clone, Debug)]
pub struct CardCryptography {
    binding: SessionBinding,
    deal: DealNumber,
    mental: MentalPoker,
    public_keys: Vec<Option<VerifiedMentalPublicKey>>,
    aggregate_key: Option<AggregateMentalKey>,
    deck: Option<VerifiedDeck>,
    next_shuffler_index: usize,
}

impl CardCryptography {
    /// Creates empty cryptographic state for one deal or round.
    #[must_use]
    pub fn new(binding: SessionBinding, deal: DealNumber) -> Self {
        Self {
            public_keys: vec![None; binding.roster().len()],
            binding,
            deal,
            mental: MentalPoker::new(),
            aggregate_key: None,
            deck: None,
            next_shuffler_index: 0,
        }
    }

    /// Returns the immutable session binding used by every proof context.
    #[must_use]
    pub const fn binding(&self) -> &SessionBinding {
        &self.binding
    }

    /// Returns the deal or round number bound into every proof context.
    #[must_use]
    pub const fn deal(&self) -> DealNumber {
        self.deal
    }

    /// Generates a fresh ephemeral threshold-key share for `seat`.
    ///
    /// The returned secret must remain available until every card that may still be
    /// opened has either been reconstructed or external recovery has terminated.
    pub fn generate_key_share(
        &self,
        rng: &mut (impl RngCore + CryptoRng),
        seat: Seat,
    ) -> Result<MentalKeyShare> {
        self.binding.roster().participant(seat)?;
        Ok(self.mental.generate_key_share(rng, self.key_context(seat)))
    }

    /// Verifies and registers one seat's key-ownership proof.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown seat, duplicate announcement, invalid proof, or
    /// an announcement received after aggregate-key or deck construction began.
    pub fn register_key(
        &mut self,
        seat: Seat,
        public_key: MentalPublicKey,
        proof: KeyOwnershipProof,
    ) -> Result<()> {
        self.binding.roster().participant(seat)?;
        if self.aggregate_key.is_some() || self.deck.is_some() {
            return Err(protocol("key setup is already finalized"));
        }
        if self.public_keys[seat.index()].is_some() {
            return Err(protocol(format!("duplicate key announcement from {seat}")));
        }
        let verified = self
            .mental
            .verify_key_share(public_key, proof, self.key_context(seat))?;
        self.public_keys[seat.index()] = Some(verified);
        Ok(())
    }

    /// Aggregates all registered public shares in canonical roster order.
    ///
    /// Calling this method repeatedly is idempotent and returns the same typed key.
    pub fn finalize_key_setup(&mut self) -> Result<&AggregateMentalKey> {
        if self.aggregate_key.is_none() {
            let verified: Vec<_> = self
                .public_keys
                .iter()
                .copied()
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| protocol("cannot finalize before every key is registered"))?;
            self.aggregate_key = Some(self.mental.aggregate_public_keys(&verified)?);
        }
        self.aggregate_key
            .as_ref()
            .ok_or_else(|| protocol("aggregate key was not created"))
    }

    /// Returns the aggregate threshold key after key setup is complete.
    pub fn aggregate_key(&self) -> Result<&AggregateMentalKey> {
        self.aggregate_key
            .as_ref()
            .ok_or_else(|| protocol("aggregate mental-card key is unavailable"))
    }

    /// Returns one verified public key share.
    pub fn verified_public_key(&self, seat: Seat) -> Result<VerifiedMentalPublicKey> {
        self.public_keys
            .get(seat.index())
            .copied()
            .flatten()
            .ok_or_else(|| protocol(format!("no verified mental-card key for {seat}")))
    }

    /// Returns one public key share after its ownership proof was verified.
    pub fn public_key(&self, seat: Seat) -> Result<MentalPublicKey> {
        Ok(self.verified_public_key(seat)?.public())
    }

    /// Returns the next seat required to shuffle, or `None` when the deck is final.
    #[must_use]
    pub fn next_shuffler(&self) -> Option<Seat> {
        (self.next_shuffler_index < self.binding.roster().len()).then(|| {
            Seat::new(self.next_shuffler_index)
                .expect("a validated roster never exceeds the global seat limit")
        })
    }

    /// Creates but does not apply the expected seat's shuffle contribution.
    ///
    /// The caller should broadcast and then pass the exact contribution to
    /// [`accept_shuffle`](Self::accept_shuffle).  Local and remote values consequently
    /// traverse the same verification path.
    pub fn create_shuffle(
        &self,
        rng: &mut (impl RngCore + CryptoRng),
        seat: Seat,
    ) -> Result<ShuffleContribution> {
        self.require_shuffler(seat)?;
        let aggregate = self.aggregate_key()?;
        let context = self.shuffle_context(seat);
        if let Some(previous) = &self.deck {
            self.mental.shuffle(rng, aggregate, previous, context)
        } else {
            Ok(self.mental.shuffle_initial(rng, aggregate, context))
        }
    }

    /// Verifies and applies the next sequential shuffle.
    pub fn accept_shuffle(&mut self, seat: Seat, contribution: ShuffleContribution) -> Result<()> {
        self.require_shuffler(seat)?;
        let aggregate = self.aggregate_key()?;
        let context = self.shuffle_context(seat);
        let verified = if let Some(previous) = &self.deck {
            self.mental
                .verify_shuffle(aggregate, previous, contribution, context)?
        } else {
            self.mental
                .verify_initial_shuffle(aggregate, contribution, context)?
        };
        self.deck = Some(verified);
        self.next_shuffler_index = self
            .next_shuffler_index
            .checked_add(1)
            .ok_or_else(|| protocol("shuffler index overflow"))?;
        Ok(())
    }

    /// Returns whether every roster member contributed one verified shuffle.
    #[must_use]
    pub fn deck_ready(&self) -> bool {
        self.next_shuffler_index == self.binding.roster().len() && self.deck.is_some()
    }

    /// Returns the final verified encrypted deck.
    pub fn deck(&self) -> Result<&VerifiedDeck> {
        if !self.deck_ready() {
            return Err(protocol("encrypted deck is not final"));
        }
        self.deck
            .as_ref()
            .ok_or_else(|| protocol("final deck is absent"))
    }

    /// Creates one threshold-decryption share and Chaum--Pedersen proof.
    ///
    /// `target` is included in the proof context.  Private shares must be transported
    /// only to their named recipient over an authenticated confidential channel; this
    /// core method does not perform that transport or add an identity signature.
    pub fn create_reveal_share(
        &self,
        rng: &mut (impl RngCore + CryptoRng),
        sender: Seat,
        target: RevealTarget,
        position: CardPosition,
        secret_key: &MentalSecretKey,
    ) -> Result<(RevealShare, RevealShareProof)> {
        self.validate_target(target)?;
        let public = self.public_key(sender)?;
        self.mental.create_reveal_share(
            rng,
            secret_key,
            public,
            self.deck()?,
            position,
            self.reveal_context(sender, target, position),
        )
    }

    /// Verifies one reveal share under the exact deck, position, sender, and target.
    pub fn verify_reveal_share(
        &self,
        sender: Seat,
        target: RevealTarget,
        position: CardPosition,
        share: RevealShare,
        proof: RevealShareProof,
    ) -> Result<VerifiedRevealShare> {
        self.validate_target(target)?;
        self.mental.verify_reveal_share(
            self.verified_public_key(sender)?,
            self.deck()?,
            position,
            share,
            proof,
            self.reveal_context(sender, target, position),
        )
    }

    /// Reconstructs a card from exactly one valid share per roster seat.
    ///
    /// The caller remains responsible for ensuring that private shares were delivered
    /// only to the recipient and, when needed, for retaining their signed byte form as
    /// later recovery or game-specific disclosure evidence.
    pub fn open_card(
        &self,
        target: RevealTarget,
        position: CardPosition,
        shares: &[(Seat, RevealShare, RevealShareProof)],
    ) -> Result<Card> {
        self.validate_target(target)?;
        let expected = self.binding.roster().len();
        if shares.len() != expected {
            return Err(protocol(format!(
                "card opening requires {expected} reveal shares, got {}",
                shares.len()
            )));
        }

        let mut senders = BTreeSet::new();
        let mut verified = Vec::with_capacity(expected);
        for (sender, share, proof) in shares {
            if !senders.insert(*sender) {
                return Err(protocol("card opening contains duplicate sender shares"));
            }
            verified.push(self.verify_reveal_share(*sender, target, position, *share, *proof)?);
        }
        self.mental
            .reveal_card(self.aggregate_key()?, self.deck()?, position, &verified)
    }

    fn require_shuffler(&self, seat: Seat) -> Result<()> {
        let expected = self
            .next_shuffler()
            .ok_or_else(|| protocol("all roster seats have already shuffled"))?;
        if seat != expected {
            return Err(protocol(format!(
                "shuffle from {seat} is out of order; expected {expected}"
            )));
        }
        Ok(())
    }

    fn validate_target(&self, target: RevealTarget) -> Result<()> {
        if let RevealTarget::Private(recipient) = target {
            self.binding.roster().participant(recipient)?;
        }
        Ok(())
    }

    fn key_context(&self, seat: Seat) -> ProofContext {
        let deal = self.deal.to_u64().to_be_bytes();
        let seat = [seat.to_u8()];
        ProofContext::derive(
            b"mental-key/v1",
            &[
                self.binding.session_id().as_ref(),
                self.binding.configuration_digest().as_ref(),
                &deal,
                &seat,
            ],
        )
    }

    fn shuffle_context(&self, seat: Seat) -> ProofContext {
        let deal = self.deal.to_u64().to_be_bytes();
        let seat = [seat.to_u8()];
        let step = u64::try_from(self.next_shuffler_index)
            .expect("the global roster limit fits u64")
            .to_be_bytes();
        let previous = self
            .deck
            .as_ref()
            .map_or(Digest32::ZERO, VerifiedDeck::digest);
        ProofContext::derive(
            b"deck-shuffle/v1",
            &[
                self.binding.session_id().as_ref(),
                self.binding.configuration_digest().as_ref(),
                &deal,
                &seat,
                &step,
                previous.as_ref(),
            ],
        )
    }

    fn reveal_context(
        &self,
        sender: Seat,
        target: RevealTarget,
        position: CardPosition,
    ) -> ProofContext {
        let deal = self.deal.to_u64().to_be_bytes();
        let sender = [sender.to_u8()];
        let recipient = [target.recipient_byte()];
        let position = [position.to_u8()];
        let deck_digest = self
            .deck
            .as_ref()
            .map_or(Digest32::ZERO, VerifiedDeck::digest);
        ProofContext::derive(
            target.context_domain(),
            &[
                self.binding.session_id().as_ref(),
                self.binding.configuration_digest().as_ref(),
                &deal,
                deck_digest.as_ref(),
                &sender,
                &recipient,
                &position,
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng as _;
    use rand::rngs::StdRng;

    use super::*;
    use crate::identity::IdentitySecret;
    use crate::session::Roster;
    use crate::types::SessionId;

    fn binding(identities: &[IdentitySecret], rng: &mut StdRng) -> SessionBinding {
        let roster = Roster::new(identities.iter().map(IdentitySecret::public).collect())
            .expect("valid roster");
        SessionBinding::new(
            SessionId::random(rng),
            roster,
            Digest32::hash(b"test-game-config", &[]),
        )
    }

    #[test]
    fn three_players_can_shuffle_and_open_public_and_private_cards() {
        let mut rng = StdRng::from_seed([0x55; 32]);
        let identities: Vec<_> = (0..3).map(|_| IdentitySecret::generate(&mut rng)).collect();
        let mut protocol = CardCryptography::new(binding(&identities, &mut rng), DealNumber::FIRST);
        let keys: Vec<_> = (0..3)
            .map(|index| {
                protocol
                    .generate_key_share(&mut rng, Seat::new(index).expect("seat"))
                    .expect("key")
            })
            .collect();
        for (index, key) in keys.iter().enumerate() {
            protocol
                .register_key(Seat::new(index).expect("seat"), key.public(), key.proof())
                .expect("register");
        }
        protocol.finalize_key_setup().expect("aggregate");
        for index in 0..3 {
            let seat = Seat::new(index).expect("seat");
            let contribution = protocol
                .create_shuffle(&mut rng, seat)
                .expect("shuffle contribution");
            protocol
                .accept_shuffle(seat, contribution)
                .expect("shuffle verification");
        }

        let public_position = CardPosition::new(0).expect("position");
        let public_shares: Vec<_> = keys
            .iter()
            .enumerate()
            .map(|(index, key)| {
                let seat = Seat::new(index).expect("seat");
                let (share, proof) = protocol
                    .create_reveal_share(
                        &mut rng,
                        seat,
                        RevealTarget::Public,
                        public_position,
                        key.secret(),
                    )
                    .expect("public share");
                (seat, share, proof)
            })
            .collect();
        let public_card = protocol
            .open_card(RevealTarget::Public, public_position, &public_shares)
            .expect("public card");

        let recipient = Seat::new(1).expect("recipient");
        let private_position = CardPosition::new(1).expect("position");
        let private_shares: Vec<_> = keys
            .iter()
            .enumerate()
            .map(|(index, key)| {
                let seat = Seat::new(index).expect("seat");
                let (share, proof) = protocol
                    .create_reveal_share(
                        &mut rng,
                        seat,
                        RevealTarget::Private(recipient),
                        private_position,
                        key.secret(),
                    )
                    .expect("private share");
                (seat, share, proof)
            })
            .collect();
        let private_card = protocol
            .open_card(
                RevealTarget::Private(recipient),
                private_position,
                &private_shares,
            )
            .expect("private card");

        assert_ne!(public_card, private_card);
        assert!(
            protocol
                .open_card(RevealTarget::Public, private_position, &private_shares)
                .is_err(),
            "private-domain shares must not verify as public shares"
        );
    }
}
