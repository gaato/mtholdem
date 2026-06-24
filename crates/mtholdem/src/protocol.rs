//! Texas Hold'em integration for the reusable mental-card state machine.
//!
//! [`mental_card_core::protocol::CardCryptography`] owns key setup, canonical
//! shuffler order, and public/private card-opening proofs.  This module adds the
//! Hold'em-specific signed artifacts and invariants that the generic layer must not
//! guess: two-card private hands, confidential signed delivery receipts, and showdown
//! disclosure tied to the exact pre-betting share set.
//!
//! Transport and public transcript sequencing remain explicit so callers can use any
//! synchronous or asynchronous runtime.

use std::collections::BTreeSet;

use crate::types::{CardPosition, Digest32, HandNumber, Seat};
use mental_card_core::card::Card;
use mental_card_core::crypto::{
    AggregateMentalKey, KeyOwnershipProof, MentalKeyShare, MentalPublicKey, MentalSecretKey,
    RevealShare, RevealShareProof, ShuffleContribution, VerifiedDeck, VerifiedRevealShare,
};
use mental_card_core::error::{Result, protocol};
use mental_card_core::protocol::{CardCryptography, RevealTarget};
use rand::{CryptoRng, RngCore};

use crate::config::SessionConfig;
use crate::identity::IdentitySecret;
use crate::wire::{PublicPayload, ShowdownBundle, SignedPrivateReveal, private_reveal_receipt};

/// Per-hand cryptographic state for Texas Hold'em.
///
/// The reusable deck state lives in [`CardCryptography`].  This wrapper is deliberately
/// small: it translates Hold'em wire payloads to core operations and verifies that
/// signed private shares belong to the exact session, hand, deck, sender, and recipient.
#[derive(Clone, Debug)]
pub struct HandCryptography {
    config: SessionConfig,
    cards: CardCryptography,
}

impl HandCryptography {
    /// Creates empty cryptographic state for a hand.
    #[must_use]
    pub fn new(config: SessionConfig, hand: HandNumber) -> Self {
        let cards = CardCryptography::new(config.cryptographic_binding(), hand);
        Self { config, cards }
    }

    /// Returns the immutable Hold'em session configuration.
    #[must_use]
    pub const fn config(&self) -> &SessionConfig {
        &self.config
    }

    /// Returns the hand number.
    #[must_use]
    pub const fn hand(&self) -> HandNumber {
        self.cards.deal()
    }

    /// Returns the reusable lower-level cryptographic state.
    ///
    /// Applications normally do not need this accessor.  It is useful for adapters
    /// that implement additional standard-card operations without duplicating key or
    /// deck state.
    #[must_use]
    pub const fn card_cryptography(&self) -> &CardCryptography {
        &self.cards
    }

    /// Generates a fresh ephemeral key share for `seat`.
    pub fn generate_key_share(
        &self,
        rng: &mut (impl RngCore + CryptoRng),
        seat: Seat,
    ) -> Result<MentalKeyShare> {
        self.cards.generate_key_share(rng, seat)
    }

    /// Converts generated key material into a public transcript payload.
    #[must_use]
    pub fn key_announcement(key: &MentalKeyShare) -> PublicPayload {
        PublicPayload::KeyAnnouncement {
            public_key: key.public(),
            proof: key.proof(),
        }
    }

    /// Verifies and registers one seat's key-ownership proof.
    pub fn register_key(
        &mut self,
        seat: Seat,
        public_key: MentalPublicKey,
        proof: KeyOwnershipProof,
    ) -> Result<()> {
        self.cards.register_key(seat, public_key, proof)
    }

    /// Registers a typed key-announcement payload from `seat`.
    pub fn register_key_payload(&mut self, seat: Seat, payload: &PublicPayload) -> Result<()> {
        match payload {
            PublicPayload::KeyAnnouncement { public_key, proof } => {
                self.register_key(seat, *public_key, *proof)
            }
            _ => Err(protocol("expected a key-announcement payload")),
        }
    }

    /// Aggregates all registered public shares in canonical seat order.
    pub fn finalize_key_setup(&mut self) -> Result<&AggregateMentalKey> {
        self.cards.finalize_key_setup()
    }

    /// Returns the next seat required to shuffle, or `None` when the deck is final.
    #[must_use]
    pub fn next_shuffler(&self) -> Option<Seat> {
        self.cards.next_shuffler()
    }

    /// Creates but does not apply the expected seat's shuffle contribution.
    pub fn create_shuffle(
        &self,
        rng: &mut (impl RngCore + CryptoRng),
        seat: Seat,
    ) -> Result<ShuffleContribution> {
        self.cards.create_shuffle(rng, seat)
    }

    /// Verifies and applies the next sequential shuffle.
    pub fn accept_shuffle(&mut self, seat: Seat, contribution: ShuffleContribution) -> Result<()> {
        self.cards.accept_shuffle(seat, contribution)
    }

    /// Registers a typed shuffle payload from the expected seat.
    pub fn accept_shuffle_payload(&mut self, seat: Seat, payload: &PublicPayload) -> Result<()> {
        match payload {
            PublicPayload::Shuffle { contribution } => {
                self.accept_shuffle(seat, contribution.as_ref().clone())
            }
            _ => Err(protocol("expected a shuffle payload")),
        }
    }

    /// Returns whether every player has contributed a verified shuffle.
    #[must_use]
    pub fn deck_ready(&self) -> bool {
        self.cards.deck_ready()
    }

    /// Returns the final verified encrypted deck.
    pub fn deck(&self) -> Result<&VerifiedDeck> {
        self.cards.deck()
    }

    /// Creates a signed private decryption share for one recipient and position.
    ///
    /// The resulting bytes must be delivered through an authenticated confidential
    /// channel.  They are retained by the recipient and may later be published in a
    /// [`ShowdownBundle`].
    pub fn create_private_reveal(
        &self,
        rng: &mut (impl RngCore + CryptoRng),
        sender: Seat,
        recipient: Seat,
        position: CardPosition,
        secret_key: &MentalSecretKey,
        identity: &IdentitySecret,
    ) -> Result<SignedPrivateReveal> {
        self.require_identity(sender, identity)?;
        let (share, proof) = self.cards.create_reveal_share(
            rng,
            sender,
            RevealTarget::Private(recipient),
            position,
            secret_key,
        )?;
        SignedPrivateReveal::sign(
            self.config.session_id(),
            self.config.digest(),
            self.hand(),
            self.deck()?.digest(),
            self.config.roster().participant(sender)?,
            self.config.roster().participant(recipient)?,
            position,
            share,
            proof,
            identity,
        )
    }

    /// Verifies the signature, context, and Chaum--Pedersen proof of a private share.
    pub fn verify_private_reveal(
        &self,
        reveal: &SignedPrivateReveal,
    ) -> Result<VerifiedRevealShare> {
        reveal.verify_signature(self.config.roster())?;
        self.require_reveal_context(reveal)?;
        self.cards.verify_reveal_share(
            reveal.sender(),
            RevealTarget::Private(reveal.recipient()),
            reveal.position(),
            reveal.share(),
            reveal.proof(),
        )
    }

    /// Reconstructs one private card from exactly one signed share per player.
    pub fn open_private_card(
        &self,
        recipient: Seat,
        position: CardPosition,
        reveals: &[SignedPrivateReveal],
    ) -> Result<Card> {
        let expected = self.config.roster().len();
        if reveals.len() != expected {
            return Err(protocol(format!(
                "private card requires {expected} reveal shares, got {}",
                reveals.len()
            )));
        }
        let mut senders = BTreeSet::new();
        let mut raw = Vec::with_capacity(expected);
        for reveal in reveals {
            if reveal.recipient() != recipient || reveal.position() != position {
                return Err(protocol(
                    "private reveal belongs to another card or recipient",
                ));
            }
            if !senders.insert(reveal.sender()) {
                return Err(protocol("private card contains duplicate sender shares"));
            }
            reveal.verify_signature(self.config.roster())?;
            self.require_reveal_context(reveal)?;
            raw.push((reveal.sender(), reveal.share(), reveal.proof()));
        }
        self.cards
            .open_card(RevealTarget::Private(recipient), position, &raw)
    }

    /// Opens both private cards and returns the safe public receipt digest.
    pub fn open_private_hand(
        &self,
        recipient: Seat,
        positions: [CardPosition; 2],
        reveals: &[SignedPrivateReveal],
    ) -> Result<([Card; 2], Digest32)> {
        if positions[0] == positions[1] {
            return Err(protocol("private hand repeats one deck position"));
        }
        let first: Vec<_> = reveals
            .iter()
            .filter(|reveal| reveal.position() == positions[0])
            .cloned()
            .collect();
        let second: Vec<_> = reveals
            .iter()
            .filter(|reveal| reveal.position() == positions[1])
            .cloned()
            .collect();
        if first.len() + second.len() != reveals.len() {
            return Err(protocol("private hand contains an unrelated reveal share"));
        }
        let cards = [
            self.open_private_card(recipient, positions[0], &first)?,
            self.open_private_card(recipient, positions[1], &second)?,
        ];
        Ok((
            cards,
            private_reveal_receipt(reveals, self.config.roster())?,
        ))
    }

    /// Creates one public decryption share and proof.
    pub fn create_public_reveal_share(
        &self,
        rng: &mut (impl RngCore + CryptoRng),
        sender: Seat,
        position: CardPosition,
        secret_key: &MentalSecretKey,
    ) -> Result<(RevealShare, RevealShareProof)> {
        self.cards
            .create_reveal_share(rng, sender, RevealTarget::Public, position, secret_key)
    }

    /// Verifies a public decryption share from one seat.
    pub fn verify_public_reveal_share(
        &self,
        sender: Seat,
        position: CardPosition,
        share: RevealShare,
        proof: RevealShareProof,
    ) -> Result<VerifiedRevealShare> {
        self.cards
            .verify_reveal_share(sender, RevealTarget::Public, position, share, proof)
    }

    /// Reconstructs a public card from one verified share per roster seat.
    pub fn open_public_card(
        &self,
        position: CardPosition,
        shares: &[(Seat, RevealShare, RevealShareProof)],
    ) -> Result<Card> {
        self.cards.open_card(RevealTarget::Public, position, shares)
    }

    /// Verifies and opens both cards in a public showdown disclosure bundle.
    ///
    /// `expected_bundle_digest` must come from the signed
    /// [`PublicPayload::ShowdownOpen`] event. `expected_receipt_digest` must come from
    /// the owner's earlier signed [`PublicPayload::HoleCardsReceived`] event or a
    /// certified checkpoint that preserved that exact digest.
    pub fn open_showdown_bundle(
        &self,
        bundle: &ShowdownBundle,
        expected_bundle_digest: Digest32,
        expected_receipt_digest: Digest32,
    ) -> Result<[Card; 2]> {
        bundle.verify(self.config.roster())?;
        if bundle.digest()? != expected_bundle_digest {
            return Err(protocol(
                "showdown bundle does not match the public transcript commitment",
            ));
        }
        let (cards, actual_receipt_digest) =
            self.open_private_hand(bundle.owner(), bundle.positions(), bundle.reveals())?;
        if actual_receipt_digest != expected_receipt_digest {
            return Err(protocol(
                "showdown shares differ from the private-card delivery receipt",
            ));
        }
        Ok(cards)
    }

    fn require_identity(&self, seat: Seat, identity: &IdentitySecret) -> Result<()> {
        let expected = self.config.roster().participant(seat)?.player_id();
        if identity.public().player_id() != expected {
            return Err(protocol(format!(
                "identity key does not belong to declared {seat}"
            )));
        }
        Ok(())
    }

    fn require_reveal_context(&self, reveal: &SignedPrivateReveal) -> Result<()> {
        if reveal.session_id() != self.config.session_id()
            || reveal.config_digest() != self.config.digest()
            || reveal.hand() != self.hand()
            || reveal.deck_digest() != self.deck()?.digest()
        {
            return Err(protocol("private reveal belongs to another hand or deck"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng as _;
    use rand::rngs::StdRng;

    use super::*;
    use crate::config::{EconomicSecurity, Roster};
    use crate::deal::DealPlan;
    use crate::identity::IdentitySecret;
    use crate::types::{Chips, SessionId};

    fn config(identities: &[IdentitySecret], rng: &mut StdRng) -> SessionConfig {
        let roster = Roster::new(identities.iter().map(IdentitySecret::public).collect()).unwrap();
        let economics = EconomicSecurity::new(
            identities.len(),
            Chips::from_units(100),
            Chips::from_units(101),
            Chips::from_units(101 * u64::try_from(identities.len() - 1).unwrap()),
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

    #[test]
    fn complete_private_distribution_and_public_reveal() {
        let mut rng = StdRng::from_seed([0x53; 32]);
        let identities: Vec<_> = (0..3).map(|_| IdentitySecret::generate(&mut rng)).collect();
        let config = config(&identities, &mut rng);
        let mut protocol = HandCryptography::new(config.clone(), HandNumber::FIRST);
        let keys: Vec<_> = (0..3)
            .map(|index| {
                protocol
                    .generate_key_share(&mut rng, Seat::new(index).unwrap())
                    .unwrap()
            })
            .collect();
        for (index, key) in keys.iter().enumerate() {
            protocol
                .register_key(Seat::new(index).unwrap(), key.public(), key.proof())
                .unwrap();
        }
        protocol.finalize_key_setup().unwrap();
        for index in 0..3 {
            let seat = Seat::new(index).unwrap();
            let contribution = protocol.create_shuffle(&mut rng, seat).unwrap();
            protocol.accept_shuffle(seat, contribution).unwrap();
        }

        let plan = DealPlan::new(
            3,
            Seat::new(0).unwrap(),
            &[
                Seat::new(0).unwrap(),
                Seat::new(1).unwrap(),
                Seat::new(2).unwrap(),
            ],
        )
        .unwrap();
        let owner = Seat::new(1).unwrap();
        let positions = plan.hole_cards(owner).unwrap().as_array();
        let mut reveals = Vec::new();
        for position in positions {
            for sender_index in 0..3 {
                let sender = Seat::new(sender_index).unwrap();
                reveals.push(
                    protocol
                        .create_private_reveal(
                            &mut rng,
                            sender,
                            owner,
                            position,
                            keys[sender_index].secret(),
                            &identities[sender_index],
                        )
                        .unwrap(),
                );
            }
        }
        let (hole, receipt_digest) = protocol
            .open_private_hand(owner, positions, &reveals)
            .unwrap();
        assert_ne!(hole[0], hole[1]);
        assert!(private_reveal_receipt(&reveals[..3], config.roster()).is_err());
        let mut duplicate_receipts = reveals.clone();
        duplicate_receipts[1] = duplicate_receipts[0].clone();
        assert!(private_reveal_receipt(&duplicate_receipts, config.roster()).is_err());

        let bundle = ShowdownBundle::sign(
            config.session_id(),
            config.digest(),
            HandNumber::FIRST,
            protocol.deck().unwrap().digest(),
            owner,
            positions,
            reveals.clone(),
            config.roster(),
            &identities[owner.index()],
        )
        .unwrap();
        let bundle_digest = bundle.digest().unwrap();
        assert_eq!(
            protocol
                .open_showdown_bundle(&bundle, bundle_digest, receipt_digest)
                .unwrap(),
            hole
        );
        assert!(
            protocol
                .open_showdown_bundle(
                    &bundle,
                    Digest32::hash(b"wrong-showdown-bundle", &[]),
                    receipt_digest,
                )
                .is_err()
        );
        assert!(
            protocol
                .open_showdown_bundle(
                    &bundle,
                    bundle_digest,
                    Digest32::hash(b"wrong-private-receipt", &[]),
                )
                .is_err()
        );

        let mut substituted_reveals = Vec::new();
        for position in positions {
            for sender_index in 0..3 {
                let sender = Seat::new(sender_index).unwrap();
                substituted_reveals.push(
                    protocol
                        .create_private_reveal(
                            &mut rng,
                            sender,
                            owner,
                            position,
                            keys[sender_index].secret(),
                            &identities[sender_index],
                        )
                        .unwrap(),
                );
            }
        }
        let substituted_bundle = ShowdownBundle::sign(
            config.session_id(),
            config.digest(),
            HandNumber::FIRST,
            protocol.deck().unwrap().digest(),
            owner,
            positions,
            substituted_reveals,
            config.roster(),
            &identities[owner.index()],
        )
        .unwrap();
        assert!(
            protocol
                .open_showdown_bundle(
                    &substituted_bundle,
                    substituted_bundle.digest().unwrap(),
                    receipt_digest,
                )
                .is_err(),
            "a fresh valid share set must not replace the shares acknowledged before betting"
        );

        let board_position = plan.flop()[0];
        let mut shares = Vec::new();
        for (index, key) in keys.iter().enumerate() {
            let seat = Seat::new(index).unwrap();
            let (share, proof) = protocol
                .create_public_reveal_share(&mut rng, seat, board_position, key.secret())
                .unwrap();
            shares.push((seat, share, proof));
        }
        let board = protocol.open_public_card(board_position, &shares).unwrap();
        assert!(!hole.contains(&board));
    }
}
