//! Immutable participant rosters and cryptographic session bindings.
//!
//! This module stops at the game-independent boundary.  It validates identity
//! membership and seat order, then lets a game crate bind its own canonical
//! configuration digest without exposing game rules to the cryptographic layer.

use std::collections::BTreeSet;
use std::fmt;

use crate::error::{Result, configuration};
use crate::identity::IdentityPublic;
use crate::types::{Digest32, MAX_PARTICIPANTS, MIN_PARTICIPANTS, PlayerId, Seat, SessionId};

/// One player in the immutable seat order agreed before a session starts.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct Participant {
    seat: Seat,
    identity: IdentityPublic,
}

impl Participant {
    /// Returns the player's zero-based seat.
    #[must_use]
    pub const fn seat(self) -> Seat {
        self.seat
    }

    /// Returns the player's Ed25519 verification key.
    #[must_use]
    pub const fn identity(self) -> IdentityPublic {
        self.identity
    }

    /// Returns the identifier derived from the verification key.
    #[must_use]
    pub fn player_id(self) -> PlayerId {
        self.identity.player_id()
    }
}

impl fmt::Debug for Participant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Participant")
            .field("seat", &self.seat)
            .field("identity", &self.identity)
            .field("player_id", &self.player_id())
            .finish()
    }
}

/// The immutable, contiguous seat roster for a session.
///
/// Input order is seat order.  Sorting identities after construction would change
/// dealer positions and invalidate signatures, so no sorting API is provided.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Roster {
    participants: Vec<Participant>,
    digest: Digest32,
}

impl Roster {
    /// Creates a roster from public keys in seat order.
    ///
    /// # Errors
    ///
    /// Returns an error unless there are `2..=255` unique identities.
    pub fn new(identities_in_seat_order: Vec<IdentityPublic>) -> Result<Self> {
        if !(MIN_PARTICIPANTS..=MAX_PARTICIPANTS).contains(&identities_in_seat_order.len()) {
            return Err(configuration(format!(
                "a roster requires {MIN_PARTICIPANTS}..={MAX_PARTICIPANTS} participants, got {}",
                identities_in_seat_order.len()
            )));
        }

        let mut seen = BTreeSet::new();
        let mut participants = Vec::with_capacity(identities_in_seat_order.len());
        for (index, identity) in identities_in_seat_order.into_iter().enumerate() {
            let player_id = identity.player_id();
            if !seen.insert(player_id) {
                return Err(configuration(format!(
                    "duplicate player identity {player_id} in roster"
                )));
            }
            participants.push(Participant {
                seat: Seat::new(index)?,
                identity,
            });
        }

        let digest = Self::compute_digest(&participants);
        Ok(Self {
            participants,
            digest,
        })
    }

    /// Returns the number of seated players.
    #[must_use]
    pub fn len(&self) -> usize {
        self.participants.len()
    }

    /// Returns `false`; a valid roster always has at least two players.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// Returns participants in consensus seat order.
    #[must_use]
    pub fn participants(&self) -> &[Participant] {
        &self.participants
    }

    /// Returns the participant occupying `seat`.
    pub fn participant(&self, seat: Seat) -> Result<Participant> {
        self.participants
            .get(seat.index())
            .copied()
            .ok_or_else(|| configuration(format!("{seat} is outside the roster")))
    }

    /// Finds a participant by stable player identifier.
    #[must_use]
    pub fn by_player_id(&self, player_id: PlayerId) -> Option<Participant> {
        self.participants
            .iter()
            .copied()
            .find(|participant| participant.player_id() == player_id)
    }

    /// Returns a digest of seat numbers and canonical identity public keys.
    #[must_use]
    pub const fn digest(&self) -> Digest32 {
        self.digest
    }

    fn compute_digest(participants: &[Participant]) -> Digest32 {
        let mut canonical = Vec::with_capacity(participants.len() * 33);
        for participant in participants {
            canonical.push(participant.seat.to_u8());
            canonical.extend_from_slice(&participant.identity.to_bytes());
        }
        Digest32::hash(b"roster/v1", &[&canonical])
    }
}

/// Immutable game-independent context bound into cryptographic proofs.
///
/// The core crate deliberately does not interpret game rules, blind schedules,
/// settlement economics, or checkpoint layouts.  A game crate validates those values,
/// hashes their canonical representation into `configuration_digest`, and supplies the
/// resulting binding here.  Consequently, changing any consensus-critical game value
/// must create a new binding and normally a new [`SessionId`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionBinding {
    session_id: SessionId,
    roster: Roster,
    configuration_digest: Digest32,
}

impl SessionBinding {
    /// Creates a binding from an already validated session and configuration digest.
    ///
    /// This constructor is intentionally infallible: [`SessionId`] and [`Roster`] have
    /// already validated their local invariants, while the meaning of
    /// `configuration_digest` belongs to the game-specific crate.  Callers must use a
    /// collision-resistant digest of every immutable value that influences protocol
    /// interpretation.
    #[must_use]
    pub const fn new(
        session_id: SessionId,
        roster: Roster,
        configuration_digest: Digest32,
    ) -> Self {
        Self {
            session_id,
            roster,
            configuration_digest,
        }
    }

    /// Returns the globally unique session identifier.
    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Returns the immutable participant roster in consensus seat order.
    #[must_use]
    pub const fn roster(&self) -> &Roster {
        &self.roster
    }

    /// Returns the opaque digest of game-specific immutable configuration.
    #[must_use]
    pub const fn configuration_digest(&self) -> Digest32 {
        self.configuration_digest
    }
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng as _;
    use rand::rngs::StdRng;

    use super::*;
    use crate::identity::IdentitySecret;

    #[test]
    fn core_roster_can_exceed_holdem_table_size() {
        let mut rng = StdRng::from_seed([0x45; 32]);
        let identities = (0..11)
            .map(|_| IdentitySecret::generate(&mut rng).public())
            .collect();
        let roster = Roster::new(identities).expect("core roster");
        assert_eq!(roster.len(), 11);
    }
}
