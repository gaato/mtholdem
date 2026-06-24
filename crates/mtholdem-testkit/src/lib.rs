//! Deterministic integration fixtures for applications built on `mtholdem`.
//!
//! This crate assembles only public `mtholdem` APIs.  It provides no validation bypass,
//! fake proof, or secret-inspection hook.  Its main value is removing repetitive setup
//! for a full roster, economic configuration, threshold keys, and all sequential
//! shuffles.
//!
//! # Never use the RNG in production
//!
//! [`TestTable`] uses a reproducible [`rand::rngs::StdRng`] seeded by the caller.  That
//! makes failures repeatable, but predictable randomness destroys key and shuffle
//! security.  Production code must use an operating-system CSPRNG such as
//! `rand::rngs::OsRng` and must not depend on this crate.
//!
//! # Example
//!
//! ```
//! use mtholdem::CardPosition;
//! use mtholdem_testkit::TestTable;
//!
//! let mut table = TestTable::standard(3, [7; 32])?;
//! let hand = table.prepare_hand(mtholdem::HandNumber::FIRST)?;
//! let shares = table.public_reveal_shares(&hand, CardPosition::new(0)?)?;
//! let card = hand
//!     .cryptography()
//!     .open_public_card(CardPosition::new(0)?, &shares)?;
//! assert!(card.to_index() < 52);
//! # Ok::<(), mtholdem::Error>(())
//! ```

#![forbid(unsafe_code)]

use std::fmt;

use mtholdem::config::{EconomicSecurity, Roster, SessionConfig};
use mtholdem::crypto::{MentalKeyShare, RevealShare, RevealShareProof};
use mtholdem::identity::IdentitySecret;
use mtholdem::protocol::HandCryptography;
use mtholdem::wire::SignedPrivateReveal;
use mtholdem::{CardPosition, Chips, HandNumber, Result, Seat, SessionId};
use rand::SeedableRng as _;
use rand::rngs::StdRng;

/// A deterministic roster, session configuration, identities, and test RNG.
pub struct TestTable {
    rng: StdRng,
    identities: Vec<IdentitySecret>,
    config: SessionConfig,
}

impl TestTable {
    /// Creates a conventional `1/2`, 100-chip test table with `player_count` seats.
    ///
    /// The maximum protected hand loss is 100 chips, compensation is 101 chips per
    /// non-faulting peer, and each deposit is exactly `101 * (player_count - 1)`.
    ///
    /// # Errors
    ///
    /// Returns an error when `player_count` is outside the supported roster range or a
    /// derived chip amount overflows.
    pub fn standard(player_count: usize, seed: [u8; 32]) -> Result<Self> {
        let mut rng = StdRng::from_seed(seed);
        let identities: Vec<_> = (0..player_count)
            .map(|_| IdentitySecret::generate(&mut rng))
            .collect();
        let roster = Roster::new(identities.iter().map(IdentitySecret::public).collect())?;
        let other_players = u64::try_from(player_count.saturating_sub(1))
            .map_err(|_| mtholdem::error::configuration("test player count did not fit u64"))?;
        let compensation = Chips::from_units(101);
        let deposit = compensation.checked_mul(other_players)?;
        let economics =
            EconomicSecurity::new(player_count, Chips::from_units(100), compensation, deposit)?;
        let config = SessionConfig::builder(
            SessionId::random(&mut rng),
            roster,
            Chips::from_units(1),
            Chips::from_units(2),
            Chips::from_units(100),
        )
        .economics(economics)
        .build()?;
        Ok(Self {
            rng,
            identities,
            config,
        })
    }

    /// Returns the immutable session configuration shared by every test peer.
    #[must_use]
    pub const fn config(&self) -> &SessionConfig {
        &self.config
    }

    /// Returns all secret identities in consensus seat order.
    ///
    /// The values retain their redacted `Debug` and zeroizing key storage.  Do not log
    /// or persist their exported seeds.
    #[must_use]
    pub fn identities(&self) -> &[IdentitySecret] {
        &self.identities
    }

    /// Returns the identity for `seat`.
    pub fn identity(&self, seat: Seat) -> Result<&IdentitySecret> {
        self.identities
            .get(seat.index())
            .ok_or_else(|| mtholdem::error::configuration(format!("{seat} is outside test roster")))
    }

    /// Returns the deterministic RNG used by this fixture.
    ///
    /// This escape hatch helps downstream tests exercise APIs not wrapped by the
    /// testkit.  It must never be copied into production integration code.
    #[must_use]
    pub const fn rng_mut(&mut self) -> &mut StdRng {
        &mut self.rng
    }

    /// Completes key setup and all roster shuffles for one hand.
    pub fn prepare_hand(&mut self, hand: HandNumber) -> Result<PreparedHand> {
        let mut cryptography = HandCryptography::new(self.config.clone(), hand);
        let keys: Vec<_> = (0..self.config.roster().len())
            .map(|index| {
                cryptography.generate_key_share(
                    &mut self.rng,
                    Seat::new(index).expect("validated roster index is a valid seat"),
                )
            })
            .collect::<Result<_>>()?;
        for (index, key) in keys.iter().enumerate() {
            cryptography.register_key(
                Seat::new(index).expect("validated roster index is a valid seat"),
                key.public(),
                key.proof(),
            )?;
        }
        cryptography.finalize_key_setup()?;
        for index in 0..self.config.roster().len() {
            let seat = Seat::new(index).expect("validated roster index is a valid seat");
            let contribution = cryptography.create_shuffle(&mut self.rng, seat)?;
            cryptography.accept_shuffle(seat, contribution)?;
        }
        Ok(PreparedHand { cryptography, keys })
    }

    /// Generates one public reveal share from every roster seat.
    pub fn public_reveal_shares(
        &mut self,
        hand: &PreparedHand,
        position: CardPosition,
    ) -> Result<Vec<(Seat, RevealShare, RevealShareProof)>> {
        hand.keys
            .iter()
            .enumerate()
            .map(|(index, key)| {
                let seat = Seat::new(index).expect("validated roster index is a valid seat");
                let (share, proof) = hand.cryptography.create_public_reveal_share(
                    &mut self.rng,
                    seat,
                    position,
                    key.secret(),
                )?;
                Ok((seat, share, proof))
            })
            .collect()
    }

    /// Generates signed confidential shares for one private card.
    ///
    /// Tests should treat the returned values like real confidential messages and pass
    /// them only to `recipient` until showdown.
    pub fn private_reveals(
        &mut self,
        hand: &PreparedHand,
        recipient: Seat,
        position: CardPosition,
    ) -> Result<Vec<SignedPrivateReveal>> {
        let rng = &mut self.rng;
        let identities = &self.identities;
        hand.keys
            .iter()
            .enumerate()
            .map(|(index, key)| {
                let seat = Seat::new(index).expect("validated roster index is a valid seat");
                hand.cryptography.create_private_reveal(
                    rng,
                    seat,
                    recipient,
                    position,
                    key.secret(),
                    &identities[index],
                )
            })
            .collect()
    }
}

impl fmt::Debug for TestTable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TestTable")
            .field("player_count", &self.identities.len())
            .field("session_id", &self.config.session_id())
            .field("rng", &"DETERMINISTIC-REDACTED")
            .finish_non_exhaustive()
    }
}

/// A hand whose aggregate key and final encrypted deck are ready for dealing.
#[derive(Debug)]
pub struct PreparedHand {
    cryptography: HandCryptography,
    keys: Vec<MentalKeyShare>,
}

impl PreparedHand {
    /// Returns the verified Hold'em cryptographic state.
    #[must_use]
    pub const fn cryptography(&self) -> &HandCryptography {
        &self.cryptography
    }

    /// Returns mutable cryptographic state for advanced integration tests.
    #[must_use]
    pub const fn cryptography_mut(&mut self) -> &mut HandCryptography {
        &mut self.cryptography
    }

    /// Returns one generated threshold-key share by seat.
    pub fn key_share(&self, seat: Seat) -> Result<&MentalKeyShare> {
        self.keys
            .get(seat.index())
            .ok_or_else(|| mtholdem::error::configuration(format!("{seat} is outside test roster")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_fixture_prepares_and_opens_a_deck() {
        let mut table = TestTable::standard(3, [0x42; 32]).expect("table");
        let hand = table.prepare_hand(HandNumber::FIRST).expect("hand");
        assert!(hand.cryptography().deck_ready());

        let position = CardPosition::new(0).expect("position");
        let shares = table.public_reveal_shares(&hand, position).expect("shares");
        let card = hand
            .cryptography()
            .open_public_card(position, &shares)
            .expect("card");
        assert!(card.to_index() < 52);
    }

    #[test]
    fn identical_seeds_produce_identical_public_rosters() {
        let first = TestTable::standard(3, [9; 32]).expect("table");
        let second = TestTable::standard(3, [9; 32]).expect("table");
        assert_eq!(first.config(), second.config());
    }
}
