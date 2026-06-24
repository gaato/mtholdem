//! Deterministic mapping from encrypted-deck positions to Hold'em cards.
//!
//! Every peer must agree which ciphertext represents each hole, burn, and community
//! card before any decryption share is sent.  The plan is derived only from public
//! seat state and therefore needs no dealer or random coordinator.

use std::collections::BTreeSet;

use crate::types::{CardPosition, MAX_PLAYERS, MIN_PLAYERS, Seat};
use mental_card_core::error::{Result, rules};

/// The two encrypted-deck positions dealt to one seat.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct HoleCardPositions {
    first: CardPosition,
    second: CardPosition,
}

impl HoleCardPositions {
    /// Returns the first dealt card.
    #[must_use]
    pub const fn first(self) -> CardPosition {
        self.first
    }

    /// Returns the second dealt card.
    #[must_use]
    pub const fn second(self) -> CardPosition {
        self.second
    }

    /// Returns both positions in deal order.
    #[must_use]
    pub const fn as_array(self) -> [CardPosition; 2] {
        [self.first, self.second]
    }
}

/// Public position plan for one standard Texas Hold'em hand.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DealPlan {
    seat_count: usize,
    button: Seat,
    deal_order: Vec<Seat>,
    hole_cards: Vec<Option<HoleCardPositions>>,
    burns: [CardPosition; 3],
    flop: [CardPosition; 3],
    turn: CardPosition,
    river: CardPosition,
}

impl DealPlan {
    /// Builds a plan for the active seats that receive hole cards.
    ///
    /// Cards are dealt clockwise beginning immediately left of the button, one card
    /// per seat for two rounds.  Standard burn cards are retained even though the
    /// encrypted deck is already hidden, keeping card-position semantics compatible
    /// with ordinary Hold'em hand histories.
    pub fn new(seat_count: usize, button: Seat, active_seats: &[Seat]) -> Result<Self> {
        if !(MIN_PLAYERS..=MAX_PLAYERS).contains(&seat_count) {
            return Err(rules(format!(
                "deal plan requires {MIN_PLAYERS}..={MAX_PLAYERS} seats"
            )));
        }
        if button.index() >= seat_count {
            return Err(rules("button is outside the table"));
        }
        if !(MIN_PLAYERS..=seat_count).contains(&active_seats.len()) {
            return Err(rules("at least two unique active seats are required"));
        }

        let active: BTreeSet<_> = active_seats.iter().copied().collect();
        if active.len() != active_seats.len() {
            return Err(rules("active seat list contains a duplicate"));
        }
        if active.iter().any(|seat| seat.index() >= seat_count) {
            return Err(rules("active seat is outside the table"));
        }

        let deal_order: Vec<_> = (1..=seat_count)
            .map(|offset| (button.index() + offset) % seat_count)
            .filter_map(|index| {
                let seat = Seat::new(index).expect("table seat index is valid");
                active.contains(&seat).then_some(seat)
            })
            .collect();

        let mut hole_cards = vec![None; seat_count];
        for (ordinal, seat) in deal_order.iter().copied().enumerate() {
            let first = CardPosition::new(ordinal)?;
            let second = CardPosition::new(ordinal + deal_order.len())?;
            hole_cards[seat.index()] = Some(HoleCardPositions { first, second });
        }

        let next = deal_order.len() * 2;
        let burns = [
            CardPosition::new(next)?,
            CardPosition::new(next + 4)?,
            CardPosition::new(next + 6)?,
        ];
        let flop = [
            CardPosition::new(next + 1)?,
            CardPosition::new(next + 2)?,
            CardPosition::new(next + 3)?,
        ];
        let turn = CardPosition::new(next + 5)?;
        let river = CardPosition::new(next + 7)?;

        Ok(Self {
            seat_count,
            button,
            deal_order,
            hole_cards,
            burns,
            flop,
            turn,
            river,
        })
    }

    /// Returns the total table seat count, including seats sitting out.
    #[must_use]
    pub const fn seat_count(&self) -> usize {
        self.seat_count
    }

    /// Returns the dealer button.
    #[must_use]
    pub const fn button(&self) -> Seat {
        self.button
    }

    /// Returns active seats in card-deal order.
    #[must_use]
    pub fn deal_order(&self) -> &[Seat] {
        &self.deal_order
    }

    /// Returns a seat's two card positions, or `None` when it is sitting out.
    #[must_use]
    pub fn hole_cards(&self, seat: Seat) -> Option<HoleCardPositions> {
        self.hole_cards.get(seat.index()).copied().flatten()
    }

    /// Returns the three burn-card positions.
    #[must_use]
    pub const fn burns(&self) -> [CardPosition; 3] {
        self.burns
    }

    /// Returns the three flop positions.
    #[must_use]
    pub const fn flop(&self) -> [CardPosition; 3] {
        self.flop
    }

    /// Returns the turn position.
    #[must_use]
    pub const fn turn(&self) -> CardPosition {
        self.turn
    }

    /// Returns the river position.
    #[must_use]
    pub const fn river(&self) -> CardPosition {
        self.river
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_three_player_positions_without_overlap() {
        let active = [
            Seat::new(0).unwrap(),
            Seat::new(1).unwrap(),
            Seat::new(2).unwrap(),
        ];
        let plan = DealPlan::new(3, Seat::new(0).unwrap(), &active).unwrap();
        assert_eq!(
            plan.deal_order(),
            &[
                Seat::new(1).unwrap(),
                Seat::new(2).unwrap(),
                Seat::new(0).unwrap()
            ]
        );
        assert_eq!(
            plan.hole_cards(Seat::new(1).unwrap())
                .unwrap()
                .first()
                .index(),
            0
        );
        assert_eq!(plan.flop()[0].index(), 7);
        assert_eq!(plan.turn().index(), 11);
        assert_eq!(plan.river().index(), 13);
    }
}
