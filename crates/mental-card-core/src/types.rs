//! Strong identifiers shared by cryptographic card protocols.
//!
//! Distributed card protocols are vulnerable to accidental parameter swaps: a deal
//! number can look like a message sequence, and a seat can look like a card position.
//! These wrappers deliberately make those mistakes fail at compile time. Financial and
//! game-rule quantities belong in the game-specific crate.

use std::fmt;

use rand::{CryptoRng, RngCore};
use sha2::{Digest as _, Sha256};

use crate::error::{Result, configuration, rules};

/// Maximum participant count representable by the compact one-byte threshold count.
///
/// This is a serialization limit, not a recommendation that large groups are practical.
/// Game crates should apply their own much smaller table or game limit.
pub const MAX_PARTICIPANTS: usize = u8::MAX as usize;

/// Minimum number of mutually distrustful participants in a threshold-card protocol.
pub const MIN_PARTICIPANTS: usize = 2;

/// Number of cards in a standard deck without jokers.
pub const DECK_SIZE: usize = 52;

/// A stable 32-byte digest used for transcript chaining and checkpoint commitments.
#[derive(Clone, Copy, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Digest32([u8; 32]);

impl Digest32 {
    /// The all-zero digest used before the first message in a transcript.
    pub const ZERO: Self = Self([0_u8; 32]);

    /// Creates a digest from its exact byte representation.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the exact byte representation.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 32] {
        self.0
    }

    /// Hashes a domain label and a sequence of length-delimited byte strings.
    ///
    /// Length-prefixing prevents ambiguous concatenations such as `(ab, c)` and
    /// `(a, bc)` from producing the same input to SHA-256.
    #[must_use]
    pub fn hash(domain: &[u8], parts: &[&[u8]]) -> Self {
        let mut hasher = Sha256::new();
        // This legacy prefix is intentionally retained after the 0.3 crate split.
        // Changing it would alter every existing session, proof, wire, and checkpoint digest.
        hasher.update(b"mtholdem/digest/v1");
        hasher.update(
            u64::try_from(domain.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        hasher.update(domain);
        for part in parts {
            hasher.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
            hasher.update(part);
        }
        Self(hasher.finalize().into())
    }
}

impl AsRef<[u8]> for Digest32 {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Digest32 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Digest32")
            .field(&Hex(&self.0))
            .finish()
    }
}

impl fmt::Display for Digest32 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        Hex(&self.0).fmt(formatter)
    }
}

/// A globally unique identifier for one protocol session.
///
/// A session identifier is included in every signature and every zero-knowledge
/// proof context.  Reusing it with the same participants is prohibited because it
/// weakens replay isolation and makes recovery evidence ambiguous.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionId([u8; 32]);

impl SessionId {
    /// Generates a fresh session identifier from caller-provided secure randomness.
    pub fn random(rng: &mut (impl RngCore + CryptoRng)) -> Self {
        loop {
            let mut bytes = [0_u8; 32];
            rng.fill_bytes(&mut bytes);
            if bytes != [0_u8; 32] {
                return Self(bytes);
            }
        }
    }

    /// Creates an identifier from externally coordinated bytes.
    ///
    /// The caller is responsible for ensuring uniqueness.  An all-zero identifier is
    /// rejected because it commonly indicates an uninitialized buffer.
    pub fn from_bytes(bytes: [u8; 32]) -> Result<Self> {
        if bytes == [0_u8; 32] {
            return Err(configuration("session identifier must not be all zero"));
        }
        Ok(Self(bytes))
    }

    /// Returns the exact bytes committed into signatures and proof transcripts.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 32] {
        self.0
    }
}

impl AsRef<[u8]> for SessionId {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("SessionId")
            .field(&Hex(&self.0))
            .finish()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        Hex(&self.0).fmt(formatter)
    }
}

/// A stable identifier derived from a player's long-lived signing public key.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PlayerId([u8; 32]);

impl PlayerId {
    /// Derives a player identifier from canonical signing-key bytes.
    #[must_use]
    pub fn from_identity_key_bytes(key_bytes: &[u8; 32]) -> Self {
        Digest32::hash(b"player-id", &[key_bytes]).into()
    }

    /// Creates an identifier from its exact bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the exact byte representation.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 32] {
        self.0
    }
}

impl From<Digest32> for PlayerId {
    fn from(value: Digest32) -> Self {
        Self(value.to_bytes())
    }
}

impl AsRef<[u8]> for PlayerId {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for PlayerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("PlayerId")
            .field(&Hex(&self.0))
            .finish()
    }
}

impl fmt::Display for PlayerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        Hex(&self.0).fmt(formatter)
    }
}

/// A zero-based participant position in an immutable roster.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Seat(u8);

impl Seat {
    /// Creates a seat from a zero-based index representable by the protocol.
    pub fn new(index: usize) -> Result<Self> {
        if index >= MAX_PARTICIPANTS {
            return Err(configuration(format!(
                "seat index {index} is outside the supported range 0..{MAX_PARTICIPANTS}"
            )));
        }
        Ok(Self(
            u8::try_from(index).expect("validated participant index fits u8"),
        ))
    }

    /// Creates a seat from a wire value after validating the global table limit.
    pub fn from_u8(index: u8) -> Result<Self> {
        Self::new(usize::from(index))
    }

    /// Returns the zero-based index suitable for indexing fixed table arrays.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// Returns the compact wire representation.
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        self.0
    }
}

impl fmt::Display for Seat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "seat {}", self.0)
    }
}

/// Monotonically increasing deal or round number within a session.
///
/// A game crate may expose a more specific alias such as `HandNumber`. The numeric
/// representation is retained in proof contexts and signed game artifacts.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DealNumber(u64);

impl DealNumber {
    /// The first deal in a session.
    pub const FIRST: Self = Self(0);

    /// Creates a deal number from its wire value.
    #[must_use]
    pub const fn from_u64(value: u64) -> Self {
        Self(value)
    }

    /// Returns the wire value.
    #[must_use]
    pub const fn to_u64(self) -> u64 {
        self.0
    }

    /// Returns the next deal number, rejecting exhaustion.
    pub fn next(self) -> Result<Self> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or_else(|| configuration("deal number exhausted"))
    }
}

impl fmt::Display for DealNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A zero-based position in the encrypted 52-card deck.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CardPosition(u8);

impl CardPosition {
    /// Creates a position in `0..52`.
    pub fn new(index: usize) -> Result<Self> {
        if index >= DECK_SIZE {
            return Err(rules(format!(
                "card position {index} is outside the deck range 0..{DECK_SIZE}"
            )));
        }
        Ok(Self(u8::try_from(index).expect("DECK_SIZE fits u8")))
    }

    /// Creates a position from a compact wire value.
    pub fn from_u8(index: u8) -> Result<Self> {
        Self::new(usize::from(index))
    }

    /// Returns the zero-based index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// Returns the compact wire value.
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        self.0
    }
}

impl fmt::Display for CardPosition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

struct Hex<'a>(&'a [u8]);

impl fmt::Debug for Hex<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for Hex<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}
