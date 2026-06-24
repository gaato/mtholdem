//! Strong types specific to the Texas Hold'em protocol surface.
//!
//! Session, identity, seat, digest, and card-position identifiers are re-exported from
//! `mental-card-core`.  This module adds Hold'em naming and checked chip arithmetic so
//! the reusable cryptographic crate does not need to own poker economics.

use std::fmt;

use mental_card_core::error::{Result, rules};

#[doc(inline)]
pub use mental_card_core::types::{CardPosition, Digest32, PlayerId, Seat, SessionId};

/// Maximum number of seats supported by the Hold'em rules and wire bounds.
pub const MAX_PLAYERS: usize = 10;

/// Minimum number of seats required to start a Hold'em hand.
pub const MIN_PLAYERS: usize = 2;

/// Monotonically increasing Hold'em hand number within a session.
///
/// The reusable core calls the same value [`mental_card_core::types::DealNumber`].
/// Hold'em APIs use this alias so public signatures match the poker domain.
pub type HandNumber = mental_card_core::types::DealNumber;

/// A non-negative quantity of the table's smallest indivisible chip unit.
///
/// No floating-point or decimal arithmetic is used anywhere in the crate. A caller
/// that displays currency should convert to and from its own fixed-point type at the
/// API boundary.
#[derive(Clone, Copy, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Chips(u64);

impl Chips {
    /// No chips.
    pub const ZERO: Self = Self(0);

    /// The largest representable chip amount.
    pub const MAX: Self = Self(u64::MAX);

    /// Creates an amount from the table's smallest chip unit.
    #[must_use]
    pub const fn from_units(units: u64) -> Self {
        Self(units)
    }

    /// Returns the number of smallest chip units.
    #[must_use]
    pub const fn units(self) -> u64 {
        self.0
    }

    /// Returns whether the amount is zero.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// Adds two amounts with explicit overflow checking.
    pub fn checked_add(self, other: Self) -> Result<Self> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or_else(|| rules("chip addition overflow"))
    }

    /// Subtracts an amount, rejecting underflow instead of wrapping.
    pub fn checked_sub(self, other: Self) -> Result<Self> {
        self.0
            .checked_sub(other.0)
            .map(Self)
            .ok_or_else(|| rules("chip subtraction underflow"))
    }

    /// Multiplies by a count with explicit overflow checking.
    pub fn checked_mul(self, count: u64) -> Result<Self> {
        self.0
            .checked_mul(count)
            .map(Self)
            .ok_or_else(|| rules("chip multiplication overflow"))
    }

    /// Returns the smaller amount.
    #[must_use]
    pub const fn min(self, other: Self) -> Self {
        if self.0 <= other.0 { self } else { other }
    }

    /// Returns the larger amount.
    #[must_use]
    pub const fn max(self, other: Self) -> Self {
        if self.0 >= other.0 { self } else { other }
    }
}

impl fmt::Debug for Chips {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Chips").field(&self.0).finish()
    }
}

impl fmt::Display for Chips {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}
