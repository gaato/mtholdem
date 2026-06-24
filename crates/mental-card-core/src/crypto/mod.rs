//! Cryptographic card operations for the 52-card mental-poker deck.
//!
//! This module implements the paper's core operations:
//!
//! * proof-of-possession key shares to prevent trapdoor public-key attacks;
//! * additive `(n, n)` threshold `ElGamal` encryption;
//! * sequential re-randomizing shuffles with Bayer--Groth proofs; and
//! * Chaum--Pedersen proofs for private or public decryption shares.
//!
//! The high-level protocol supplies signatures, deterministic phase ordering,
//! confidential direct-message routing, checkpoints, and penalty evidence.

mod bg_shuffle;

use std::collections::BTreeSet;
use std::fmt;

use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use rand::{CryptoRng, RngCore};

use self::bg_shuffle as bg;
use crate::card::Card;
use crate::error::{Result, codec, cryptography};
use crate::types::{CardPosition, Digest32, MAX_PARTICIPANTS, MIN_PARTICIPANTS};

/// Canonical compressed byte length of a mental-poker public key.
pub const MENTAL_PUBLIC_KEY_BYTES: usize = 33;
/// Canonical compressed byte length of a key-ownership proof.
pub const KEY_OWNERSHIP_PROOF_BYTES: usize = 65;
/// Canonical compressed byte length of a 52-card encrypted deck.
pub const ENCRYPTED_DECK_BYTES: usize = 3_432;
/// Canonical compressed byte length of the one-row Bayer--Groth shuffle proof.
pub const SHUFFLE_PROOF_BYTES: usize = 5_547;
/// Canonical compressed byte length of one decryption share.
pub const REVEAL_SHARE_BYTES: usize = 33;
/// Canonical compressed byte length of one Chaum--Pedersen share proof.
pub const REVEAL_SHARE_PROOF_BYTES: usize = 98;

/// A 32-byte domain-separated context bound into a zero-knowledge proof.
///
/// Contexts must identify the session, deal, phase, sender, and card or shuffle step.
/// Reusing a context for semantically different operations risks replay and proof
/// substitution.  The protocol module derives these values automatically.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ProofContext(Digest32);

impl ProofContext {
    /// Creates a context from an application-defined digest.
    #[must_use]
    pub const fn from_digest(digest: Digest32) -> Self {
        Self(digest)
    }

    /// Returns the exact context bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 32] {
        self.0.to_bytes()
    }

    pub(crate) fn derive(domain: &[u8], parts: &[&[u8]]) -> Self {
        Self(Digest32::hash(domain, parts))
    }
}

/// A secret threshold-ElGamal key share.
///
/// This value is zeroized on drop by the underlying implementation.  It must remain
/// available for the entire deal, even after a game no longer lets the participant act, because every
/// participant is needed to reveal later community cards.
pub struct MentalSecretKey(bg::SecretKey);

impl fmt::Debug for MentalSecretKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("MentalSecretKey")
            .field(&"REDACTED")
            .finish()
    }
}

/// A player's threshold-ElGamal public-key share.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct MentalPublicKey(bg::PublicKey);

impl MentalPublicKey {
    /// Parses a canonical compressed public key.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        deserialize_exact(bytes, MENTAL_PUBLIC_KEY_BYTES).map(Self)
    }

    /// Returns canonical compressed bytes.
    #[must_use]
    pub fn to_bytes(self) -> [u8; MENTAL_PUBLIC_KEY_BYTES] {
        serialize_array(&self.0)
    }
}

impl fmt::Debug for MentalPublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("MentalPublicKey")
            .field(&Digest32::hash(b"mental-public-key", &[&self.to_bytes()]))
            .finish()
    }
}

/// A Schnorr proof that the sender knows the secret key matching a public-key share.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct KeyOwnershipProof(bg::OwnershipProof);

impl KeyOwnershipProof {
    /// Parses a canonical ownership proof.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        deserialize_exact(bytes, KEY_OWNERSHIP_PROOF_BYTES).map(Self)
    }

    /// Returns canonical compressed bytes.
    #[must_use]
    pub fn to_bytes(self) -> [u8; KEY_OWNERSHIP_PROOF_BYTES] {
        serialize_array(&self.0)
    }
}

impl fmt::Debug for KeyOwnershipProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("KeyOwnershipProof(…)")
    }
}

/// A generated mental-poker key share and its proof of possession.
pub struct MentalKeyShare {
    secret: MentalSecretKey,
    public: MentalPublicKey,
    proof: KeyOwnershipProof,
}

impl MentalKeyShare {
    /// Borrows the secret key share.
    #[must_use]
    pub const fn secret(&self) -> &MentalSecretKey {
        &self.secret
    }

    /// Returns the public key share.
    #[must_use]
    pub const fn public(&self) -> MentalPublicKey {
        self.public
    }

    /// Returns the proof of possession.
    #[must_use]
    pub const fn proof(&self) -> KeyOwnershipProof {
        self.proof
    }

    /// Consumes the generated share and returns all components.
    #[must_use]
    pub fn into_parts(self) -> (MentalSecretKey, MentalPublicKey, KeyOwnershipProof) {
        (self.secret, self.public, self.proof)
    }
}

impl fmt::Debug for MentalKeyShare {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MentalKeyShare")
            .field("secret", &"REDACTED")
            .field("public", &self.public)
            .field("proof", &self.proof)
            .finish()
    }
}

/// A public key that has passed its proof-of-possession check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedMentalPublicKey {
    public: MentalPublicKey,
    verified: bg::Verified<bg::PublicKey>,
}

impl VerifiedMentalPublicKey {
    /// Returns the verified raw public key.
    #[must_use]
    pub const fn public(self) -> MentalPublicKey {
        self.public
    }
}

/// Aggregate threshold key for all participants required to decrypt a deal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AggregateMentalKey {
    inner: bg::AggregatePublicKey,
    members: Vec<MentalPublicKey>,
    digest: Digest32,
}

impl AggregateMentalKey {
    /// Returns aggregate membership in canonical public-key order.
    #[must_use]
    pub fn members(&self) -> &[MentalPublicKey] {
        &self.members
    }

    /// Returns the number of key shares required to reveal a card.
    #[must_use]
    pub const fn participant_count(&self) -> usize {
        self.members.len()
    }

    /// Returns a stable commitment to the aggregate key and its exact membership.
    ///
    /// Verified decks carry this digest so a deck proved under one threshold key cannot
    /// accidentally be shuffled, revealed, or restored under another key.
    #[must_use]
    pub const fn digest(&self) -> Digest32 {
        self.digest
    }
}

/// A serialized ElGamal-encrypted deck before proof verification.
#[derive(Clone, Eq, PartialEq)]
pub struct EncryptedDeck(bg::MaskedDeck<52>);

impl EncryptedDeck {
    /// Parses exactly one canonical 52-card encrypted deck.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        deserialize_exact(bytes, ENCRYPTED_DECK_BYTES).map(Self)
    }

    /// Returns canonical compressed bytes.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; ENCRYPTED_DECK_BYTES] {
        serialize_array(&self.0)
    }
}

impl fmt::Debug for EncryptedDeck {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncryptedDeck")
            .field(
                "digest",
                &Digest32::hash(b"encrypted-deck", &[&self.to_bytes()]),
            )
            .finish()
    }
}

/// A Bayer--Groth proof that an encrypted deck is a permutation and rerandomization
/// of the preceding deck.
#[derive(Clone, Eq, PartialEq)]
pub struct ShuffleProof(bg::ShuffleProof<52>);

impl ShuffleProof {
    /// Parses exactly one canonical shuffle proof.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        deserialize_exact(bytes, SHUFFLE_PROOF_BYTES).map(Self)
    }

    /// Returns canonical compressed bytes.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; SHUFFLE_PROOF_BYTES] {
        serialize_array(&self.0)
    }
}

impl fmt::Debug for ShuffleProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ShuffleProof(…)")
    }
}

/// A deck together with the proof that must be verified by every participant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShuffleContribution {
    deck: EncryptedDeck,
    proof: ShuffleProof,
}

impl ShuffleContribution {
    /// Reconstructs a contribution from separately decoded canonical objects.
    #[must_use]
    pub const fn from_parts(deck: EncryptedDeck, proof: ShuffleProof) -> Self {
        Self { deck, proof }
    }

    /// Borrows the encrypted deck.
    #[must_use]
    pub const fn deck(&self) -> &EncryptedDeck {
        &self.deck
    }

    /// Borrows the shuffle proof.
    #[must_use]
    pub const fn proof(&self) -> &ShuffleProof {
        &self.proof
    }

    /// Consumes the contribution.
    #[must_use]
    pub const fn into_parts(self) -> (EncryptedDeck, ShuffleProof) {
        (self.deck, self.proof)
    }
}

/// An encrypted deck whose complete shuffle proof chain has been checked.
#[derive(Clone, Debug)]
pub struct VerifiedDeck {
    raw: EncryptedDeck,
    verified: bg::Verified<bg::MaskedDeck<52>>,
    aggregate_key_digest: Digest32,
    participant_count: u8,
}

impl VerifiedDeck {
    /// Returns the canonical encrypted deck bytes for signing or checkpointing.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; ENCRYPTED_DECK_BYTES] {
        self.raw.to_bytes()
    }

    /// Returns a digest suitable for compact public state.
    #[must_use]
    pub fn digest(&self) -> Digest32 {
        Digest32::hash(
            b"verified-encrypted-deck/v2",
            &[
                self.aggregate_key_digest.as_ref(),
                &[self.participant_count],
                &self.to_bytes(),
            ],
        )
    }

    /// Returns the aggregate-key commitment under which this deck was verified.
    #[must_use]
    pub const fn aggregate_key_digest(&self) -> Digest32 {
        self.aggregate_key_digest
    }

    /// Returns the number of threshold-key participants bound to this deck.
    #[must_use]
    pub const fn participant_count(&self) -> usize {
        self.participant_count as usize
    }

    fn masked_card(&self, position: CardPosition) -> Result<bg::MaskedCard> {
        self.verified
            .get(position.index())
            .ok_or_else(|| cryptography("verified deck did not contain the requested position"))
    }

    pub(crate) fn restore_from_certified_bytes(
        bytes: &[u8],
        aggregate_key_digest: Digest32,
        participant_count: u8,
    ) -> Result<Self> {
        let raw = EncryptedDeck::from_bytes(bytes)?;
        let verified = bg::Verified::from_certified_checkpoint(raw.0);
        Ok(Self {
            raw,
            verified,
            aggregate_key_digest,
            participant_count,
        })
    }
}

/// A threshold decryption share for one encrypted card.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct RevealShare(bg::RevealToken);

impl RevealShare {
    /// Parses a canonical decryption share.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        deserialize_exact(bytes, REVEAL_SHARE_BYTES).map(Self)
    }

    /// Returns canonical compressed bytes.
    #[must_use]
    pub fn to_bytes(self) -> [u8; REVEAL_SHARE_BYTES] {
        serialize_array(&self.0)
    }
}

impl fmt::Debug for RevealShare {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RevealShare(…)")
    }
}

/// A Chaum--Pedersen proof that a decryption share used the sender's registered key.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct RevealShareProof(bg::RevealTokenProof);

impl RevealShareProof {
    /// Parses a canonical share proof.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        deserialize_exact(bytes, REVEAL_SHARE_PROOF_BYTES).map(Self)
    }

    /// Returns canonical compressed bytes.
    #[must_use]
    pub fn to_bytes(self) -> [u8; REVEAL_SHARE_PROOF_BYTES] {
        serialize_array(&self.0)
    }
}

impl fmt::Debug for RevealShareProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RevealShareProof(…)")
    }
}

/// A decryption share whose proof has been checked for one public key and card.
#[derive(Clone, Copy, Debug)]
pub struct VerifiedRevealShare {
    public: MentalPublicKey,
    verified: bg::Verified<bg::RevealToken>,
    deck_digest: Digest32,
    position: CardPosition,
}

impl VerifiedRevealShare {
    /// Returns the public key that authenticated this share.
    #[must_use]
    pub const fn public(self) -> MentalPublicKey {
        self.public
    }

    /// Returns the verified deck commitment for this share.
    #[must_use]
    pub const fn deck_digest(self) -> Digest32 {
        self.deck_digest
    }

    /// Returns the verified encrypted-deck position for this share.
    #[must_use]
    pub const fn position(self) -> CardPosition {
        self.position
    }
}

/// Concrete cryptographic engine for the standard 52-card deck.
#[derive(Clone, Debug, Default)]
pub struct MentalPoker {
    inner: bg::Shuffle<52>,
}

impl MentalPoker {
    /// Creates the deterministic public parameters for a 52-card deck.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Generates a threshold key share and Schnorr proof of possession.
    pub fn generate_key_share(
        &self,
        rng: &mut (impl RngCore + CryptoRng),
        context: ProofContext,
    ) -> MentalKeyShare {
        let (secret, public, proof) = self.inner.keygen(rng, &context.to_bytes());
        MentalKeyShare {
            secret: MentalSecretKey(secret),
            public: MentalPublicKey(public),
            proof: KeyOwnershipProof(proof),
        }
    }

    /// Verifies a key share's proof of possession.
    pub fn verify_key_share(
        &self,
        public: MentalPublicKey,
        proof: KeyOwnershipProof,
        context: ProofContext,
    ) -> Result<VerifiedMentalPublicKey> {
        if public.0.is_identity() {
            return Err(cryptography(
                "mental-poker public key must not be the identity point",
            ));
        }
        proof
            .0
            .verify(public.0, &context.to_bytes())
            .map(|verified| VerifiedMentalPublicKey { public, verified })
            .ok_or_else(|| cryptography("mental-poker key ownership proof was invalid"))
    }

    /// Aggregates a representable set of distinct verified public-key shares.
    ///
    /// Game crates should impose a substantially smaller practical participant limit.
    pub fn aggregate_public_keys(
        &self,
        public_keys: &[VerifiedMentalPublicKey],
    ) -> Result<AggregateMentalKey> {
        if !(MIN_PARTICIPANTS..=MAX_PARTICIPANTS).contains(&public_keys.len()) {
            return Err(cryptography(format!(
                "aggregate key requires {MIN_PARTICIPANTS}..={MAX_PARTICIPANTS} shares, got {}",
                public_keys.len()
            )));
        }
        let mut keys = public_keys.to_vec();
        keys.sort_by_key(|key| key.public.to_bytes());
        if keys.windows(2).any(|pair| pair[0].public == pair[1].public) {
            return Err(cryptography(
                "aggregate key contains a duplicate public share",
            ));
        }
        let verified: Vec<_> = keys.iter().map(|key| key.verified).collect();
        let inner = bg::AggregatePublicKey::new(&verified);
        if inner.is_identity() {
            return Err(cryptography(
                "aggregate mental-poker public key must not be the identity point",
            ));
        }
        let members: Vec<_> = keys.iter().map(|key| key.public).collect();
        let mut canonical = Vec::with_capacity(1 + members.len() * MENTAL_PUBLIC_KEY_BYTES);
        canonical
            .push(u8::try_from(members.len()).expect("the supported aggregate-key size fits u8"));
        for member in &members {
            canonical.extend_from_slice(&member.to_bytes());
        }
        Ok(AggregateMentalKey {
            inner,
            members,
            digest: Digest32::hash(b"aggregate-mental-key/v1", &[&canonical]),
        })
    }

    /// Produces the first encrypted shuffle of the canonical open deck.
    pub fn shuffle_initial(
        &self,
        rng: &mut (impl RngCore + CryptoRng),
        aggregate_key: &AggregateMentalKey,
        context: ProofContext,
    ) -> ShuffleContribution {
        let (deck, proof) =
            self.inner
                .shuffle_initial_deck(rng, aggregate_key.inner, &context.to_bytes());
        ShuffleContribution {
            deck: EncryptedDeck(deck),
            proof: ShuffleProof(proof),
        }
    }

    /// Verifies the first shuffle and returns a typed verified deck.
    pub fn verify_initial_shuffle(
        &self,
        aggregate_key: &AggregateMentalKey,
        contribution: ShuffleContribution,
        context: ProofContext,
    ) -> Result<VerifiedDeck> {
        let (raw, proof) = contribution.into_parts();
        let verified = self
            .inner
            .verify_initial_shuffle(aggregate_key.inner, raw.0, proof.0, &context.to_bytes())
            .ok_or_else(|| cryptography("initial Bayer--Groth shuffle proof was invalid"))?;
        Ok(VerifiedDeck {
            raw,
            verified,
            aggregate_key_digest: aggregate_key.digest,
            participant_count: u8::try_from(aggregate_key.participant_count())
                .expect("supported aggregate-key size fits u8"),
        })
    }

    /// Re-randomizes and permutes an already verified encrypted deck.
    pub fn shuffle(
        &self,
        rng: &mut (impl RngCore + CryptoRng),
        aggregate_key: &AggregateMentalKey,
        previous: &VerifiedDeck,
        context: ProofContext,
    ) -> Result<ShuffleContribution> {
        require_deck_key(aggregate_key, previous)?;
        let (deck, proof) = self.inner.shuffle_deck(
            rng,
            aggregate_key.inner,
            &previous.verified,
            &context.to_bytes(),
        );
        Ok(ShuffleContribution {
            deck: EncryptedDeck(deck),
            proof: ShuffleProof(proof),
        })
    }

    /// Verifies a subsequent shuffle against the preceding verified deck.
    pub fn verify_shuffle(
        &self,
        aggregate_key: &AggregateMentalKey,
        previous: &VerifiedDeck,
        contribution: ShuffleContribution,
        context: ProofContext,
    ) -> Result<VerifiedDeck> {
        require_deck_key(aggregate_key, previous)?;
        let (raw, proof) = contribution.into_parts();
        let verified = self
            .inner
            .verify_shuffle(
                aggregate_key.inner,
                &previous.verified,
                raw.0,
                proof.0,
                &context.to_bytes(),
            )
            .ok_or_else(|| cryptography("Bayer--Groth shuffle proof was invalid"))?;
        Ok(VerifiedDeck {
            raw,
            verified,
            aggregate_key_digest: aggregate_key.digest,
            participant_count: u8::try_from(aggregate_key.participant_count())
                .expect("supported aggregate-key size fits u8"),
        })
    }

    /// Creates a decryption share and proof for one card position.
    pub fn create_reveal_share(
        &self,
        rng: &mut (impl RngCore + CryptoRng),
        secret: &MentalSecretKey,
        public: MentalPublicKey,
        deck: &VerifiedDeck,
        position: CardPosition,
        context: ProofContext,
    ) -> Result<(RevealShare, RevealShareProof)> {
        if secret.0.public_key() != public.0 {
            return Err(cryptography(
                "reveal secret does not match the declared public key",
            ));
        }
        let card = deck.masked_card(position)?;
        let (share, proof) = card.reveal_token(rng, &secret.0, public.0, &context.to_bytes());
        Ok((RevealShare(share), RevealShareProof(proof)))
    }

    /// Verifies one player's decryption share for a specific card and context.
    pub fn verify_reveal_share(
        &self,
        public: VerifiedMentalPublicKey,
        deck: &VerifiedDeck,
        position: CardPosition,
        share: RevealShare,
        proof: RevealShareProof,
        context: ProofContext,
    ) -> Result<VerifiedRevealShare> {
        let card = deck.masked_card(position)?;
        proof
            .0
            .verify(public.verified, share.0, card, &context.to_bytes())
            .map(|verified| VerifiedRevealShare {
                public: public.public,
                verified,
                deck_digest: deck.digest(),
                position,
            })
            .ok_or_else(|| cryptography("Chaum--Pedersen reveal-share proof was invalid"))
    }

    /// Combines exactly one verified share from each aggregate-key member and reveals a card.
    pub fn reveal_card(
        &self,
        aggregate_key: &AggregateMentalKey,
        deck: &VerifiedDeck,
        position: CardPosition,
        shares: &[VerifiedRevealShare],
    ) -> Result<Card> {
        require_deck_key(aggregate_key, deck)?;
        if shares.len() != aggregate_key.participant_count() {
            return Err(cryptography(format!(
                "card reveal requires {} shares, got {}",
                aggregate_key.participant_count(),
                shares.len()
            )));
        }
        let provided: BTreeSet<_> = shares.iter().map(|share| share.public.to_bytes()).collect();
        let expected: BTreeSet<_> = aggregate_key
            .members
            .iter()
            .map(|key| key.to_bytes())
            .collect();
        if provided != expected {
            return Err(cryptography(
                "card reveal shares do not match aggregate-key membership",
            ));
        }
        let deck_digest = deck.digest();
        if shares
            .iter()
            .any(|share| share.deck_digest != deck_digest || share.position != position)
        {
            return Err(cryptography(
                "card reveal contains a share verified for another deck or position",
            ));
        }
        let verified: Vec<_> = shares.iter().map(|share| share.verified).collect();
        let aggregate = bg::AggregateRevealToken::new(&verified);
        let card_index = self
            .inner
            .reveal_card(aggregate, deck.masked_card(position)?)
            .ok_or_else(|| cryptography("aggregate reveal shares did not decode a deck card"))?;
        Card::from_index(card_index)
    }
}

fn require_deck_key(aggregate_key: &AggregateMentalKey, deck: &VerifiedDeck) -> Result<()> {
    let expected_key_digest = aggregate_key.digest;
    let actual_key_digest = deck.aggregate_key_digest;
    let expected_participant_count = aggregate_key.participant_count();
    let actual_participant_count = deck.participant_count();

    if actual_key_digest != expected_key_digest
        || actual_participant_count != expected_participant_count
    {
        return Err(cryptography(
            "verified deck is bound to another aggregate mental-poker key",
        ));
    }
    Ok(())
}

fn serialize_array<T: CanonicalSerialize, const N: usize>(value: &T) -> [u8; N] {
    let mut output = [0_u8; N];
    let mut writer = &mut output[..];
    value
        .serialize_compressed(&mut writer)
        .expect("serialization into a correctly sized fixed buffer is infallible");
    assert!(
        writer.is_empty(),
        "cryptographic serialization size constant is stale"
    );
    output
}

fn deserialize_exact<T: CanonicalDeserialize>(bytes: &[u8], expected: usize) -> Result<T> {
    if bytes.len() != expected {
        return Err(codec(format!(
            "cryptographic object has {} bytes; expected exactly {expected}",
            bytes.len()
        )));
    }
    let mut reader = bytes;
    let value = T::deserialize_compressed(&mut reader)
        .map_err(|error| codec(format!("invalid canonical cryptographic encoding: {error}")))?;
    if !reader.is_empty() {
        return Err(codec("cryptographic encoding had trailing bytes"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rand::SeedableRng as _;
    use rand::rngs::StdRng;

    use super::*;

    #[test]
    fn three_party_shuffle_and_reveal_round_trip() {
        let mut rng = StdRng::from_seed([0x11; 32]);
        let mental = MentalPoker::new();
        let key_context = ProofContext::from_digest(Digest32::hash(b"test-key", &[]));
        let keys: Vec<_> = (0..3)
            .map(|_| mental.generate_key_share(&mut rng, key_context))
            .collect();
        let verified: Vec<_> = keys
            .iter()
            .map(|key| {
                mental
                    .verify_key_share(key.public(), key.proof(), key_context)
                    .expect("proof")
            })
            .collect();
        let aggregate = mental.aggregate_public_keys(&verified).expect("aggregate");

        let first_context = ProofContext::from_digest(Digest32::hash(b"shuffle-0", &[]));
        let first = mental.shuffle_initial(&mut rng, &aggregate, first_context);
        let mut tampered_proof_bytes = first.proof().to_bytes();
        tampered_proof_bytes[tampered_proof_bytes.len() - 1] ^= 1;
        if let Ok(tampered_proof) = ShuffleProof::from_bytes(&tampered_proof_bytes) {
            assert!(
                mental
                    .verify_initial_shuffle(
                        &aggregate,
                        ShuffleContribution::from_parts(first.deck().clone(), tampered_proof),
                        first_context,
                    )
                    .is_err()
            );
        }
        let mut deck = mental
            .verify_initial_shuffle(&aggregate, first, first_context)
            .expect("initial proof");
        for step in 1_u8..3 {
            let step_bytes = [step];
            let context = ProofContext::from_digest(Digest32::hash(b"shuffle", &[&step_bytes]));
            let contribution = mental
                .shuffle(&mut rng, &aggregate, &deck, context)
                .expect("compatible aggregate key");
            deck = mental
                .verify_shuffle(&aggregate, &deck, contribution, context)
                .expect("shuffle proof");
        }

        let position = CardPosition::new(0).expect("position");
        let reveal_context = ProofContext::from_digest(Digest32::hash(b"reveal", &[]));
        assert!(
            mental
                .create_reveal_share(
                    &mut rng,
                    keys[0].secret(),
                    keys[1].public(),
                    &deck,
                    position,
                    reveal_context,
                )
                .is_err()
        );
        let shares: Vec<_> = keys
            .iter()
            .zip(&verified)
            .map(|(key, public)| {
                let (share, proof) = mental
                    .create_reveal_share(
                        &mut rng,
                        key.secret(),
                        key.public(),
                        &deck,
                        position,
                        reveal_context,
                    )
                    .expect("share");
                mental
                    .verify_reveal_share(*public, &deck, position, share, proof, reveal_context)
                    .expect("share proof")
            })
            .collect();
        let card = mental
            .reveal_card(&aggregate, &deck, position, &shares)
            .expect("reveal");
        assert!(card.to_index() < 52);

        let another_position = CardPosition::new(1).expect("position");
        assert!(
            mental
                .reveal_card(&aggregate, &deck, another_position, &shares)
                .is_err(),
            "verified shares must remain bound to their original deck position"
        );

        let alternate_context =
            ProofContext::from_digest(Digest32::hash(b"alternate-shuffle", &[]));
        let alternate = mental.shuffle_initial(&mut rng, &aggregate, alternate_context);
        let alternate_deck = mental
            .verify_initial_shuffle(&aggregate, alternate, alternate_context)
            .expect("alternate shuffle proof");
        assert!(
            mental
                .reveal_card(&aggregate, &alternate_deck, position, &shares)
                .is_err(),
            "verified shares must remain bound to the exact encrypted deck"
        );

        let other_keys: Vec<_> = (0..2)
            .map(|_| mental.generate_key_share(&mut rng, key_context))
            .collect();
        let other_verified: Vec<_> = other_keys
            .iter()
            .map(|key| {
                mental
                    .verify_key_share(key.public(), key.proof(), key_context)
                    .expect("proof")
            })
            .collect();
        let other_aggregate = mental
            .aggregate_public_keys(&other_verified)
            .expect("aggregate");
        assert!(
            mental
                .shuffle(&mut rng, &other_aggregate, &deck, first_context)
                .is_err(),
            "verified decks must remain bound to the aggregate key used for proof verification"
        );
        assert!(
            mental
                .reveal_card(&other_aggregate, &deck, position, &shares)
                .is_err(),
            "a deck must not be opened under another aggregate key"
        );
    }

    #[test]
    fn two_party_shuffle_reveals_every_card_exactly_once() {
        let mut rng = StdRng::from_seed([0x22; 32]);
        let mental = MentalPoker::new();
        let keys: Vec<_> = (0_u8..2)
            .map(|seat| {
                let context =
                    ProofContext::from_digest(Digest32::hash(b"full-deck-key", &[&[seat]]));
                mental.generate_key_share(&mut rng, context)
            })
            .collect();
        let verified: Vec<_> = keys
            .iter()
            .enumerate()
            .map(|(seat, key)| {
                let seat = [u8::try_from(seat).expect("test seat fits u8")];
                let context = ProofContext::from_digest(Digest32::hash(b"full-deck-key", &[&seat]));
                mental
                    .verify_key_share(key.public(), key.proof(), context)
                    .expect("ownership proof")
            })
            .collect();
        let aggregate = mental.aggregate_public_keys(&verified).expect("aggregate");

        let first_context = ProofContext::from_digest(Digest32::hash(b"full-deck-shuffle-0", &[]));
        let first = mental.shuffle_initial(&mut rng, &aggregate, first_context);
        let deck = mental
            .verify_initial_shuffle(&aggregate, first, first_context)
            .expect("initial shuffle");
        let second_context = ProofContext::from_digest(Digest32::hash(b"full-deck-shuffle-1", &[]));
        let second = mental
            .shuffle(&mut rng, &aggregate, &deck, second_context)
            .expect("compatible aggregate key");
        let deck = mental
            .verify_shuffle(&aggregate, &deck, second, second_context)
            .expect("second shuffle");

        let mut revealed = BTreeSet::new();
        for index in 0..52 {
            let position = CardPosition::new(index).expect("deck position");
            let index_byte = [u8::try_from(index).expect("card index fits u8")];
            let context =
                ProofContext::from_digest(Digest32::hash(b"full-deck-reveal", &[&index_byte]));
            let shares: Vec<_> = keys
                .iter()
                .zip(&verified)
                .map(|(key, public)| {
                    let (share, proof) = mental
                        .create_reveal_share(
                            &mut rng,
                            key.secret(),
                            key.public(),
                            &deck,
                            position,
                            context,
                        )
                        .expect("share");
                    mental
                        .verify_reveal_share(*public, &deck, position, share, proof, context)
                        .expect("share proof")
                })
                .collect();
            let card = mental
                .reveal_card(&aggregate, &deck, position, &shares)
                .expect("reveal card");
            assert!(revealed.insert(card.to_index()), "card was duplicated");
        }
        assert_eq!(revealed, (0_usize..52).collect());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn cryptographic_decoders_never_panic_on_untrusted_bytes(
            bytes in prop::collection::vec(any::<u8>(), 0..6_000),
        ) {
            let _public_key = MentalPublicKey::from_bytes(&bytes);
            let _key_proof = KeyOwnershipProof::from_bytes(&bytes);
            let _deck = EncryptedDeck::from_bytes(&bytes);
            let _shuffle_proof = ShuffleProof::from_bytes(&bytes);
            let _reveal_share = RevealShare::from_bytes(&bytes);
            let _reveal_proof = RevealShareProof::from_bytes(&bytes);
        }
    }

    #[test]
    fn serialized_sizes_match_wire_constants() {
        let mut rng = StdRng::from_seed([0x33; 32]);
        let mental = MentalPoker::new();
        let context = ProofContext::from_digest(Digest32::hash(b"sizes", &[]));
        let key = mental.generate_key_share(&mut rng, context);
        assert_eq!(key.public().to_bytes().len(), MENTAL_PUBLIC_KEY_BYTES);
        assert_eq!(key.proof().to_bytes().len(), KEY_OWNERSHIP_PROOF_BYTES);
    }
}
