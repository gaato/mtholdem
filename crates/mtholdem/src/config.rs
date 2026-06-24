//! Immutable Texas Hold'em session configuration.
//!
//! Roster membership is validated by [`mental_card_core::session`].  This module adds
//! the blind schedule, buy-in, timeout, and financial-penalty bounds that are specific
//! to the Hold'em protocol and commits all of them into one stable digest.

use crate::types::{Chips, Digest32, MAX_PLAYERS, MIN_PLAYERS, SessionId};
use mental_card_core::error::{Result, configuration};
use mental_card_core::session::SessionBinding;
#[doc(inline)]
pub use mental_card_core::session::{Participant, Roster};

/// Financial bounds used to make intentional aborts economically irrational.
///
/// The library validates the sufficient inequalities used by the papers:
/// `compensation > maximum_hand_loss` and
/// `security_deposit >= compensation * (players - 1)`.  Whether those amounts are
/// economically adequate in a particular currency or jurisdiction remains an
/// application policy decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EconomicSecurity {
    maximum_hand_loss: Chips,
    compensation: Chips,
    security_deposit: Chips,
}

impl EconomicSecurity {
    /// Validates and creates financial-security parameters.
    pub fn new(
        player_count: usize,
        maximum_hand_loss: Chips,
        compensation: Chips,
        security_deposit: Chips,
    ) -> Result<Self> {
        if !(MIN_PLAYERS..=MAX_PLAYERS).contains(&player_count) {
            return Err(configuration(format!(
                "economic parameters require {MIN_PLAYERS}..={MAX_PLAYERS} players"
            )));
        }
        if maximum_hand_loss.is_zero() {
            return Err(configuration("maximum hand loss must be positive"));
        }
        if compensation <= maximum_hand_loss {
            return Err(configuration(format!(
                "compensation {compensation} must exceed maximum hand loss {maximum_hand_loss}"
            )));
        }
        let others = u64::try_from(player_count - 1)
            .map_err(|_| configuration("player count did not fit the chip multiplier"))?;
        let minimum_deposit = compensation
            .checked_mul(others)
            .map_err(|_| configuration("minimum security deposit overflowed"))?;
        if security_deposit < minimum_deposit {
            return Err(configuration(format!(
                "security deposit {security_deposit} is below required minimum {minimum_deposit}"
            )));
        }
        Ok(Self {
            maximum_hand_loss,
            compensation,
            security_deposit,
        })
    }

    /// Returns the maximum amount one player may put at risk in one hand.
    #[must_use]
    pub const fn maximum_hand_loss(self) -> Chips {
        self.maximum_hand_loss
    }

    /// Returns the amount paid to each non-faulting player after a proven abort.
    #[must_use]
    pub const fn compensation(self) -> Chips {
        self.compensation
    }

    /// Returns the collateral locked by each player.
    #[must_use]
    pub const fn security_deposit(self) -> Chips {
        self.security_deposit
    }
}

/// Validated, immutable parameters shared by every peer in one table session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionConfig {
    session_id: SessionId,
    roster: Roster,
    small_blind: Chips,
    big_blind: Chips,
    buy_in: Chips,
    economics: EconomicSecurity,
    timeout_millis: u64,
    digest: Digest32,
}

impl SessionConfig {
    /// Starts a builder with values that have no safe universal defaults.
    #[must_use]
    pub fn builder(
        session_id: SessionId,
        roster: Roster,
        small_blind: Chips,
        big_blind: Chips,
        buy_in: Chips,
    ) -> SessionConfigBuilder {
        SessionConfigBuilder {
            session_id,
            roster,
            small_blind,
            big_blind,
            buy_in,
            economics: None,
            timeout_millis: 30_000,
        }
    }

    /// Returns the unique session identifier.
    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Returns the immutable roster.
    #[must_use]
    pub const fn roster(&self) -> &Roster {
        &self.roster
    }

    /// Returns the small blind in integer chip units.
    #[must_use]
    pub const fn small_blind(&self) -> Chips {
        self.small_blind
    }

    /// Returns the big blind and minimum full opening bet.
    #[must_use]
    pub const fn big_blind(&self) -> Chips {
        self.big_blind
    }

    /// Returns each player's initial stack.
    #[must_use]
    pub const fn buy_in(&self) -> Chips {
        self.buy_in
    }

    /// Returns the validated penalty economics.
    #[must_use]
    pub const fn economics(&self) -> EconomicSecurity {
        self.economics
    }

    /// Returns the application-enforced response timeout in milliseconds.
    #[must_use]
    pub const fn timeout_millis(&self) -> u64 {
        self.timeout_millis
    }

    /// Returns a digest bound into hand keys, signed messages, and checkpoints.
    #[must_use]
    pub const fn digest(&self) -> Digest32 {
        self.digest
    }

    /// Creates the game-independent binding consumed by the cryptographic core.
    ///
    /// The returned value clones the immutable roster and carries this configuration's
    /// canonical digest.  It does not expose blind or economic fields to the lower
    /// layer, while still binding every proof to their exact values.
    #[must_use]
    pub fn cryptographic_binding(&self) -> SessionBinding {
        SessionBinding::new(self.session_id, self.roster.clone(), self.digest)
    }

    fn validate_and_build(builder: SessionConfigBuilder) -> Result<Self> {
        let SessionConfigBuilder {
            session_id,
            roster,
            small_blind,
            big_blind,
            buy_in,
            economics,
            timeout_millis,
        } = builder;

        if !(MIN_PLAYERS..=MAX_PLAYERS).contains(&roster.len()) {
            return Err(configuration(format!(
                "a Hold'em session requires {MIN_PLAYERS}..={MAX_PLAYERS} players, got {}",
                roster.len()
            )));
        }
        if small_blind.is_zero() {
            return Err(configuration("small blind must be positive"));
        }
        if big_blind <= small_blind {
            return Err(configuration(format!(
                "big blind {big_blind} must exceed small blind {small_blind}"
            )));
        }
        if buy_in < big_blind {
            return Err(configuration(format!(
                "buy-in {buy_in} must cover at least one big blind {big_blind}"
            )));
        }
        if timeout_millis == 0 {
            return Err(configuration("protocol timeout must be positive"));
        }
        let economics = economics
            .ok_or_else(|| configuration("economic security parameters must be set explicitly"))?;

        // Recompute the collateral inequality against the actual roster.  Callers may
        // construct `EconomicSecurity` before the roster is finalized, so trusting the
        // player count supplied to `EconomicSecurity::new` would permit accidentally
        // reusing two-player parameters at a larger table.
        let other_players = u64::try_from(roster.len() - 1)
            .map_err(|_| configuration("roster size did not fit the chip multiplier"))?;
        let required_deposit = economics
            .compensation
            .checked_mul(other_players)
            .map_err(|_| configuration("minimum security deposit overflowed for roster"))?;
        if economics.security_deposit < required_deposit {
            return Err(configuration(format!(
                "security deposit {} is below roster-specific minimum {required_deposit}",
                economics.security_deposit
            )));
        }
        if economics.maximum_hand_loss < big_blind {
            return Err(configuration(format!(
                "maximum hand loss {} must cover big blind {big_blind}",
                economics.maximum_hand_loss
            )));
        }
        if economics.maximum_hand_loss > buy_in {
            return Err(configuration(format!(
                "maximum hand loss {} exceeds buy-in {buy_in}",
                economics.maximum_hand_loss
            )));
        }

        let digest = Self::compute_digest(
            session_id,
            &roster,
            small_blind,
            big_blind,
            buy_in,
            economics,
            timeout_millis,
        );
        Ok(Self {
            session_id,
            roster,
            small_blind,
            big_blind,
            buy_in,
            economics,
            timeout_millis,
            digest,
        })
    }

    fn compute_digest(
        session_id: SessionId,
        roster: &Roster,
        small_blind: Chips,
        big_blind: Chips,
        buy_in: Chips,
        economics: EconomicSecurity,
        timeout_millis: u64,
    ) -> Digest32 {
        let sb = small_blind.units().to_be_bytes();
        let bb = big_blind.units().to_be_bytes();
        let buy_in = buy_in.units().to_be_bytes();
        let maximum = economics.maximum_hand_loss.units().to_be_bytes();
        let compensation = economics.compensation.units().to_be_bytes();
        let deposit = economics.security_deposit.units().to_be_bytes();
        let timeout = timeout_millis.to_be_bytes();
        Digest32::hash(
            b"session-config/v1",
            &[
                session_id.as_ref(),
                roster.digest().as_ref(),
                &sb,
                &bb,
                &buy_in,
                &maximum,
                &compensation,
                &deposit,
                &timeout,
            ],
        )
    }
}

/// Builder for [`SessionConfig`].
///
/// The penalty economics are deliberately mandatory rather than silently inferred.
/// A deployment must choose amounts appropriate to its maximum permitted loss.
#[derive(Clone, Debug)]
pub struct SessionConfigBuilder {
    session_id: SessionId,
    roster: Roster,
    small_blind: Chips,
    big_blind: Chips,
    buy_in: Chips,
    economics: Option<EconomicSecurity>,
    timeout_millis: u64,
}

impl SessionConfigBuilder {
    /// Sets previously validated penalty economics.
    #[must_use]
    pub const fn economics(mut self, economics: EconomicSecurity) -> Self {
        self.economics = Some(economics);
        self
    }

    /// Sets the response deadline that the transport and recovery adapter enforce.
    #[must_use]
    pub const fn timeout_millis(mut self, timeout_millis: u64) -> Self {
        self.timeout_millis = timeout_millis;
        self
    }

    /// Validates all parameters and builds the immutable configuration.
    pub fn build(self) -> Result<SessionConfig> {
        SessionConfig::validate_and_build(self)
    }
}

#[cfg(test)]
mod tests {
    use rand::rngs::OsRng;

    use super::*;
    use mental_card_core::identity::IdentitySecret;

    fn roster(count: usize) -> Roster {
        let mut rng = OsRng;
        let identities = (0..count)
            .map(|_| IdentitySecret::generate(&mut rng).public())
            .collect();
        Roster::new(identities).expect("valid roster")
    }

    #[test]
    fn rejects_inadequate_collateral() {
        let result = EconomicSecurity::new(
            3,
            Chips::from_units(100),
            Chips::from_units(101),
            Chips::from_units(201),
        );
        assert!(result.is_err());
    }

    #[test]
    fn rejects_economics_validated_for_a_smaller_roster() {
        let mut rng = OsRng;
        let roster = roster(3);
        let two_player_economics = EconomicSecurity::new(
            2,
            Chips::from_units(100),
            Chips::from_units(101),
            Chips::from_units(101),
        )
        .expect("valid for two players");
        let result = SessionConfig::builder(
            SessionId::random(&mut rng),
            roster,
            Chips::from_units(1),
            Chips::from_units(2),
            Chips::from_units(100),
        )
        .economics(two_player_economics)
        .build();
        assert!(result.is_err());
    }

    #[test]
    fn rejects_a_core_roster_larger_than_a_holdem_table() {
        let mut rng = OsRng;
        let roster = roster(11);
        let economics = EconomicSecurity::new(
            MAX_PLAYERS,
            Chips::from_units(100),
            Chips::from_units(101),
            Chips::from_units(909),
        )
        .expect("valid ten-player economics");
        let result = SessionConfig::builder(
            SessionId::random(&mut rng),
            roster,
            Chips::from_units(1),
            Chips::from_units(2),
            Chips::from_units(100),
        )
        .economics(economics)
        .build();
        assert!(result.is_err());
    }

    #[test]
    fn builds_stable_config_digest() {
        let mut rng = OsRng;
        let roster = roster(3);
        let session = SessionId::random(&mut rng);
        let economics = EconomicSecurity::new(
            3,
            Chips::from_units(1_000),
            Chips::from_units(1_001),
            Chips::from_units(2_002),
        )
        .expect("valid economics");
        let first = SessionConfig::builder(
            session,
            roster.clone(),
            Chips::from_units(5),
            Chips::from_units(10),
            Chips::from_units(1_000),
        )
        .economics(economics)
        .build()
        .expect("config");
        let second = SessionConfig::builder(
            session,
            roster,
            Chips::from_units(5),
            Chips::from_units(10),
            Chips::from_units(1_000),
        )
        .economics(economics)
        .build()
        .expect("config");
        assert_eq!(first.digest(), second.digest());
    }
}
