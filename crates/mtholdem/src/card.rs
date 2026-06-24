//! Deterministic Texas Hold'em hand evaluation.
//!
//! The standard card representation lives in [`mental_card_core::card`].  This module
//! re-exports those types so existing `mtholdem::card::Card` paths remain valid, then
//! adds only Hold'em-specific category and best-five-of-seven evaluation.
//!
//! The evaluator enumerates the at most 21 five-card subsets of a seven-card hand and
//! compares canonical category/kicker tuples.  It uses no lookup tables, generated
//! code, or platform-dependent ordering.

use std::fmt;

#[doc(inline)]
pub use mental_card_core::card::{Card, Rank, Suit};
use mental_card_core::error::{Result, rules};
use mental_card_core::types::DECK_SIZE;

/// The category of a five-card poker hand, ordered from weakest to strongest.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum HandCategory {
    /// Five unmatched cards.
    HighCard = 0,
    /// One pair.
    OnePair = 1,
    /// Two distinct pairs.
    TwoPair = 2,
    /// Three cards of one rank.
    ThreeOfAKind = 3,
    /// Five consecutive ranks.
    Straight = 4,
    /// Five cards of one suit.
    Flush = 5,
    /// Three of a kind plus a pair.
    FullHouse = 6,
    /// Four cards of one rank.
    FourOfAKind = 7,
    /// A straight in one suit.
    StraightFlush = 8,
}

impl fmt::Display for HandCategory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::HighCard => "high card",
            Self::OnePair => "one pair",
            Self::TwoPair => "two pair",
            Self::ThreeOfAKind => "three of a kind",
            Self::Straight => "straight",
            Self::Flush => "flush",
            Self::FullHouse => "full house",
            Self::FourOfAKind => "four of a kind",
            Self::StraightFlush => "straight flush",
        };
        formatter.write_str(name)
    }
}

/// A fully comparable Hold'em hand value.
///
/// `tiebreakers()` stores ranks in comparison order.  Unused positions are zero.
/// For example, a two-pair hand stores `[higher_pair, lower_pair, kicker, 0, 0]`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct HandValue {
    category: HandCategory,
    tiebreakers: [u8; 5],
}

impl HandValue {
    /// Returns the category.
    #[must_use]
    pub const fn category(self) -> HandCategory {
        self.category
    }

    /// Returns rank values in tie-breaking order.
    #[must_use]
    pub const fn tiebreakers(self) -> [u8; 5] {
        self.tiebreakers
    }
}

impl fmt::Display for HandValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {:?}", self.category, self.tiebreakers)
    }
}

/// Evaluates the strongest five-card hand contained in five, six, or seven cards.
///
/// Duplicate cards are rejected.  In Texas Hold'em, pass the two hole cards followed
/// by the five community cards.
///
/// # Errors
///
/// Returns an error if the input length is outside `5..=7` or a card is duplicated.
pub fn evaluate_best(cards: &[Card]) -> Result<HandValue> {
    if !(5..=7).contains(&cards.len()) {
        return Err(rules(format!(
            "hand evaluation requires 5 to 7 cards, got {}",
            cards.len()
        )));
    }

    let mut seen = [false; DECK_SIZE];
    for card in cards {
        let index = card.to_index();
        if std::mem::replace(&mut seen[index], true) {
            return Err(rules(format!("duplicate card {card} in hand evaluation")));
        }
    }

    let mut best = None;
    for first in 0..cards.len() - 4 {
        for second in first + 1..cards.len() - 3 {
            for third in second + 1..cards.len() - 2 {
                for fourth in third + 1..cards.len() - 1 {
                    for fifth in fourth + 1..cards.len() {
                        let value = evaluate_five([
                            cards[first],
                            cards[second],
                            cards[third],
                            cards[fourth],
                            cards[fifth],
                        ]);
                        best = Some(best.map_or(value, |current: HandValue| current.max(value)));
                    }
                }
            }
        }
    }

    best.ok_or_else(|| rules("no five-card subset was evaluated"))
}

fn evaluate_five(cards: [Card; 5]) -> HandValue {
    let flush = cards.iter().all(|card| card.suit() == cards[0].suit());
    let mut counts = [0_u8; 15];
    for card in cards {
        counts[usize::from(card.rank().value())] += 1;
    }

    let straight_high = straight_high(&counts);
    if flush && let Some(high) = straight_high {
        return value(HandCategory::StraightFlush, [high, 0, 0, 0, 0]);
    }

    let mut groups = Vec::with_capacity(5);
    for rank in (2_u8..=14).rev() {
        let count = counts[usize::from(rank)];
        if count != 0 {
            groups.push((count, rank));
        }
    }
    groups.sort_unstable_by(|left, right| right.cmp(left));

    if groups[0].0 == 4 {
        return value(
            HandCategory::FourOfAKind,
            [groups[0].1, groups[1].1, 0, 0, 0],
        );
    }
    if groups[0].0 == 3 && groups[1].0 == 2 {
        return value(HandCategory::FullHouse, [groups[0].1, groups[1].1, 0, 0, 0]);
    }

    let descending = descending_ranks(&counts);
    if flush {
        return value(HandCategory::Flush, descending);
    }
    if let Some(high) = straight_high {
        return value(HandCategory::Straight, [high, 0, 0, 0, 0]);
    }
    if groups[0].0 == 3 {
        return value(
            HandCategory::ThreeOfAKind,
            [groups[0].1, groups[1].1, groups[2].1, 0, 0],
        );
    }
    if groups[0].0 == 2 && groups[1].0 == 2 {
        return value(
            HandCategory::TwoPair,
            [groups[0].1, groups[1].1, groups[2].1, 0, 0],
        );
    }
    if groups[0].0 == 2 {
        return value(
            HandCategory::OnePair,
            [groups[0].1, groups[1].1, groups[2].1, groups[3].1, 0],
        );
    }
    value(HandCategory::HighCard, descending)
}

const fn value(category: HandCategory, tiebreakers: [u8; 5]) -> HandValue {
    HandValue {
        category,
        tiebreakers,
    }
}

fn descending_ranks(counts: &[u8; 15]) -> [u8; 5] {
    let mut result = [0_u8; 5];
    let mut next = 0;
    for rank in (2_u8..=14).rev() {
        for _ in 0..counts[usize::from(rank)] {
            result[next] = rank;
            next += 1;
        }
    }
    result
}

fn straight_high(counts: &[u8; 15]) -> Option<u8> {
    for high in (6_u8..=14).rev() {
        if (0..5).all(|offset| counts[usize::from(high - offset)] != 0) {
            return Some(high);
        }
    }
    let wheel =
        counts[14] != 0 && counts[2] != 0 && counts[3] != 0 && counts[4] != 0 && counts[5] != 0;
    wheel.then_some(5)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn cards(text: &str) -> Vec<Card> {
        text.split_whitespace()
            .map(|card| card.parse().expect("valid test card"))
            .collect()
    }

    #[test]
    fn stable_index_round_trip_covers_entire_deck() {
        for index in 0..DECK_SIZE {
            let card = Card::from_index(index).expect("valid index");
            assert_eq!(card.to_index(), index);
        }
    }

    #[test]
    fn evaluator_orders_categories_and_kickers() {
        let straight_flush = evaluate_best(&cards("As Ks Qs Js Ts 2d 3c")).expect("hand");
        let quads = evaluate_best(&cards("Ah Ad Ac As Kd 2c 3h")).expect("hand");
        let pair_aces = evaluate_best(&cards("Ah Ad Kc Qs Jd 2c 3h")).expect("hand");
        let pair_kings = evaluate_best(&cards("Kh Kd Ac Qs Jd 2c 3h")).expect("hand");

        assert!(straight_flush > quads);
        assert!(pair_aces > pair_kings);
        assert_eq!(straight_flush.category(), HandCategory::StraightFlush);
    }

    #[test]
    fn wheel_is_five_high_straight() {
        let wheel = evaluate_best(&cards("As 2d 3c 4h 5s Kd Qd")).expect("hand");
        assert_eq!(wheel.category(), HandCategory::Straight);
        assert_eq!(wheel.tiebreakers()[0], 5);
    }

    #[test]
    fn duplicates_are_rejected() {
        let error = evaluate_best(&cards("As As Ks Qs Js Ts 9s")).expect_err("duplicate");
        assert!(error.is_rules());
    }
    proptest! {
        #[test]
        fn best_hand_is_independent_of_input_order(
            indices in prop::collection::btree_set(0_usize..52, 7),
        ) {
            let cards: Vec<_> = indices
                .into_iter()
                .map(|index| Card::from_index(index).expect("generated index is valid"))
                .collect();
            let expected = evaluate_best(&cards).expect("seven unique cards evaluate");

            let mut reversed = cards;
            reversed.reverse();
            prop_assert_eq!(
                evaluate_best(&reversed).expect("reordered unique cards evaluate"),
                expected,
            );
        }
    }
}
