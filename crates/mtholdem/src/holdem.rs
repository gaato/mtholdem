//! End-to-end deterministic rules controller for one no-limit Hold'em hand.
//!
//! Cryptography determines which cards exist; this controller determines forced
//! blinds, legal actions, street transitions, hand ranking, side pots, odd chips,
//! and final stacks.  Every peer can replay signed [`BetAction`]
//! values through the same API and compare the resulting checkpoint digest.

use std::collections::BTreeSet;

use crate::betting::{
    ActionContext, BetAction, BettingRound, HandLedger, HandSeatStatus, LegalActions,
    PotDistribution, Street, distribute_showdown, distribute_uncontested,
};
use crate::card::{Card, HandValue, evaluate_best};
use crate::types::{Chips, MAX_PLAYERS, MIN_PLAYERS, Seat};
use mental_card_core::error::{Result, rules};

/// Public phase of a rules-only Hold'em hand.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum HandProgress {
    /// A seat is expected to make an action on the named street.
    Betting(Street),
    /// The betting round ended and three flop cards must be opened.
    AwaitingFlop,
    /// The flop round ended and the turn card must be opened.
    AwaitingTurn,
    /// The turn round ended and the river card must be opened.
    AwaitingRiver,
    /// Remaining players may open or muck their cards.
    Showdown,
    /// Every other player folded; the sole live seat can receive the pot.
    Uncontested(Seat),
    /// Payouts have been computed.
    Complete,
}

/// Actual seats and amounts used for forced blinds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PostedBlinds {
    small_blind_seat: Seat,
    small_blind_amount: Chips,
    big_blind_seat: Seat,
    big_blind_amount: Chips,
}

impl PostedBlinds {
    /// Returns the small-blind seat.
    #[must_use]
    pub const fn small_blind_seat(self) -> Seat {
        self.small_blind_seat
    }

    /// Returns the actual small-blind contribution, possibly a short all-in.
    #[must_use]
    pub const fn small_blind_amount(self) -> Chips {
        self.small_blind_amount
    }

    /// Returns the big-blind seat.
    #[must_use]
    pub const fn big_blind_seat(self) -> Seat {
        self.big_blind_seat
    }

    /// Returns the actual big-blind contribution, possibly a short all-in.
    #[must_use]
    pub const fn big_blind_amount(self) -> Chips {
        self.big_blind_amount
    }
}

/// Final accounting for one hand.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandResult {
    distribution: PotDistribution,
    final_stacks: Vec<Chips>,
}

impl HandResult {
    /// Returns the main-pot and side-pot distribution.
    #[must_use]
    pub const fn distribution(&self) -> &PotDistribution {
        &self.distribution
    }

    /// Returns post-hand stacks in table seat order.
    #[must_use]
    pub fn final_stacks(&self) -> &[Chips] {
        &self.final_stacks
    }
}

/// Rules state machine for one hand.
#[derive(Clone, Debug)]
pub struct HoldemHand {
    button: Seat,
    small_blind: Chips,
    big_blind: Chips,
    maximum_hand_loss: Chips,
    blinds: PostedBlinds,
    ledger: Option<HandLedger>,
    round: Option<BettingRound>,
    board: Vec<Card>,
    progress: HandProgress,
    last_aggressor: Option<Seat>,
    result: Option<HandResult>,
}

impl HoldemHand {
    /// Posts blinds and starts preflop action.
    ///
    /// A zero stack marks a seat as sitting out.  In heads-up play the button posts
    /// the small blind; with three or more dealt players, the first dealt seat left of
    /// the button posts it.  `maximum_hand_loss` is the economic cap protected by the
    /// session collateral and is enforced after every action.
    pub fn new(
        stacks: &[Chips],
        button: Seat,
        small_blind: Chips,
        big_blind: Chips,
        maximum_hand_loss: Chips,
    ) -> Result<Self> {
        if !(MIN_PLAYERS..=MAX_PLAYERS).contains(&stacks.len()) {
            return Err(rules(format!(
                "Hold'em hand requires {MIN_PLAYERS}..={MAX_PLAYERS} seats"
            )));
        }
        if small_blind.is_zero() || big_blind <= small_blind {
            return Err(rules("blinds must satisfy 0 < small blind < big blind"));
        }
        if maximum_hand_loss < big_blind {
            return Err(rules("maximum hand loss must cover the big blind"));
        }

        let mut ledger = HandLedger::new(stacks, button, big_blind)?;
        let dealt = ledger.live_seats().len();
        let small_blind_seat = if dealt == 2 {
            if ledger.status(button)? == HandSeatStatus::NotDealt {
                return Err(rules("heads-up button must be a dealt seat"));
            }
            button
        } else {
            ledger.next_dealt_after(button)?
        };
        let big_blind_seat = ledger.next_dealt_after(small_blind_seat)?;
        let small_blind_amount = ledger.post_forced_bet(small_blind_seat, small_blind)?;
        let big_blind_amount = ledger.post_forced_bet(big_blind_seat, big_blind)?;

        for seat in [small_blind_seat, big_blind_seat] {
            if ledger.total_contributed(seat)? > maximum_hand_loss {
                return Err(rules("forced blind exceeds maximum hand loss"));
            }
        }

        let first_to_act = ledger.next_dealt_after(big_blind_seat)?;
        let round = BettingRound::new(ledger, Street::Preflop, first_to_act, big_blind)?;
        let mut hand = Self {
            button,
            small_blind,
            big_blind,
            maximum_hand_loss,
            blinds: PostedBlinds {
                small_blind_seat,
                small_blind_amount,
                big_blind_seat,
                big_blind_amount,
            },
            ledger: None,
            round: Some(round),
            board: Vec::with_capacity(5),
            progress: HandProgress::Betting(Street::Preflop),
            last_aggressor: None,
            result: None,
        };
        hand.finish_round_if_complete()?;
        Ok(hand)
    }

    /// Returns the dealer button.
    #[must_use]
    pub const fn button(&self) -> Seat {
        self.button
    }

    /// Returns configured blind sizes.
    #[must_use]
    pub const fn blinds(&self) -> PostedBlinds {
        self.blinds
    }

    /// Returns the small blind size.
    #[must_use]
    pub const fn small_blind(&self) -> Chips {
        self.small_blind
    }

    /// Returns the big blind size.
    #[must_use]
    pub const fn big_blind(&self) -> Chips {
        self.big_blind
    }

    /// Returns the per-player contribution cap protected by collateral.
    #[must_use]
    pub const fn maximum_hand_loss(&self) -> Chips {
        self.maximum_hand_loss
    }

    /// Returns the current public progress.
    #[must_use]
    pub const fn progress(&self) -> HandProgress {
        self.progress
    }

    /// Returns community cards opened so far.
    #[must_use]
    pub fn board(&self) -> &[Card] {
        &self.board
    }

    /// Returns the current protected ledger regardless of whether a round is active.
    #[must_use]
    pub fn ledger(&self) -> &HandLedger {
        if let Some(round) = &self.round {
            round.ledger()
        } else {
            self.ledger
                .as_ref()
                .expect("rules state always retains a ledger before completion")
        }
    }

    /// Returns the seat expected to act, or `None` outside a betting phase.
    #[must_use]
    pub fn next_to_act(&self) -> Option<Seat> {
        self.round.as_ref().and_then(BettingRound::next_to_act)
    }

    /// Returns legal amounts for the expected actor.
    pub fn legal_actions(&self) -> Result<LegalActions> {
        self.round
            .as_ref()
            .ok_or_else(|| rules("the hand is not in a betting phase"))?
            .legal_actions()
    }

    /// Returns public betting context for the expected actor.
    pub fn action_context(&self) -> Result<ActionContext> {
        self.round
            .as_ref()
            .ok_or_else(|| rules("the hand is not in a betting phase"))?
            .action_context()
    }

    /// Applies a betting action and automatically advances when the street closes.
    pub fn apply_action(&mut self, actor: Seat, action: BetAction) -> Result<()> {
        let round = self
            .round
            .as_ref()
            .ok_or_else(|| rules("the hand is not in a betting phase"))?;
        let mut candidate = round.clone();
        candidate.apply(actor, action)?;
        if candidate.ledger().total_contributed(actor)? > self.maximum_hand_loss {
            return Err(rules(format!(
                "{actor}'s contribution would exceed maximum hand loss {}",
                self.maximum_hand_loss
            )));
        }
        self.round = Some(candidate);
        self.finish_round_if_complete()
    }

    /// Opens the three-card flop and begins flop action when any player can act.
    pub fn open_flop(&mut self, flop: [Card; 3]) -> Result<()> {
        if self.progress != HandProgress::AwaitingFlop {
            return Err(rules("flop is not expected in the current hand phase"));
        }
        self.ensure_new_cards(&flop)?;
        self.board.extend_from_slice(&flop);
        self.start_postflop_round(Street::Flop)
    }

    /// Opens the turn and begins turn action when any player can act.
    pub fn open_turn(&mut self, turn: Card) -> Result<()> {
        if self.progress != HandProgress::AwaitingTurn {
            return Err(rules("turn is not expected in the current hand phase"));
        }
        self.ensure_new_cards(&[turn])?;
        self.board.push(turn);
        self.start_postflop_round(Street::Turn)
    }

    /// Opens the river and begins river action when any player can act.
    pub fn open_river(&mut self, river: Card) -> Result<()> {
        if self.progress != HandProgress::AwaitingRiver {
            return Err(rules("river is not expected in the current hand phase"));
        }
        self.ensure_new_cards(&[river])?;
        self.board.push(river);
        self.start_postflop_round(Street::River)
    }

    /// Settles an uncontested hand after every other dealt seat folded.
    pub fn settle_uncontested(&mut self) -> Result<&HandResult> {
        let HandProgress::Uncontested(expected) = self.progress else {
            return Err(rules("hand is not ready for uncontested settlement"));
        };
        if self.ledger().uncontested_winner() != Some(expected) {
            return Err(rules("uncontested winner does not match the ledger"));
        }
        let distribution = distribute_uncontested(self.ledger())?;
        self.complete(distribution)
    }

    /// Evaluates opened hands, builds side pots, and computes final stacks.
    ///
    /// Supply one tuple for every non-folded seat.  `None` represents a voluntary
    /// muck and makes the seat ineligible to win.  Folded seats need not be included.
    pub fn settle_showdown(
        &mut self,
        opened_or_mucked: &[(Seat, Option<[Card; 2]>)],
    ) -> Result<&HandResult> {
        if self.progress != HandProgress::Showdown {
            return Err(rules("hand is not ready for showdown settlement"));
        }
        if self.board.len() != 5 {
            return Err(rules("showdown requires all five community cards"));
        }

        let seat_count = self.ledger().seat_count();
        let mut supplied = BTreeSet::new();
        let mut globally_seen_cards: BTreeSet<_> = self.board.iter().copied().collect();
        let mut values: Vec<Option<HandValue>> = vec![None; seat_count];
        for (seat, hole) in opened_or_mucked {
            if seat.index() >= seat_count || !supplied.insert(*seat) {
                return Err(rules("showdown contains an invalid or duplicate seat"));
            }
            match self.ledger().status(*seat)? {
                HandSeatStatus::Playing | HandSeatStatus::AllIn => {
                    if let Some(hole) = hole {
                        for card in hole {
                            if !globally_seen_cards.insert(*card) {
                                return Err(rules(format!(
                                    "duplicate revealed card {card} across showdown hands"
                                )));
                            }
                        }
                        let mut cards = Vec::with_capacity(7);
                        cards.extend_from_slice(hole);
                        cards.extend_from_slice(&self.board);
                        values[seat.index()] = Some(evaluate_best(&cards)?);
                    }
                }
                HandSeatStatus::Folded | HandSeatStatus::NotDealt => {
                    return Err(rules("folded or undealt seat appeared at showdown"));
                }
            }
        }

        for seat in self.ledger().live_seats() {
            if !supplied.contains(&seat) {
                return Err(rules(format!(
                    "showdown omitted live {seat}; use None to record a muck"
                )));
            }
        }
        if values.iter().all(Option::is_none) {
            return Err(rules("every live player mucked; no pot winner exists"));
        }

        let distribution = distribute_showdown(self.ledger(), &values)?;
        self.complete(distribution)
    }

    /// Returns final accounting after settlement.
    #[must_use]
    pub const fn result(&self) -> Option<&HandResult> {
        self.result.as_ref()
    }

    /// Returns the last aggressor on the most recently completed street.
    #[must_use]
    pub const fn last_aggressor(&self) -> Option<Seat> {
        self.last_aggressor
    }

    fn start_postflop_round(&mut self, street: Street) -> Result<()> {
        let mut ledger = self
            .ledger
            .take()
            .ok_or_else(|| rules("ledger is unavailable for the next street"))?;
        if let Some(winner) = ledger.uncontested_winner() {
            self.ledger = Some(ledger);
            self.progress = HandProgress::Uncontested(winner);
            return Ok(());
        }
        ledger.start_new_street();
        let first_to_act = ledger.next_dealt_after(self.button)?;
        self.round = Some(BettingRound::new(
            ledger,
            street,
            first_to_act,
            Chips::ZERO,
        )?);
        self.progress = HandProgress::Betting(street);
        self.finish_round_if_complete()
    }

    fn finish_round_if_complete(&mut self) -> Result<()> {
        let Some(round) = self.round.as_ref() else {
            return Ok(());
        };
        if !round.is_complete() {
            return Ok(());
        }
        let street = round.street();
        let outcome = self
            .round
            .take()
            .expect("round was just observed")
            .finish()?;
        self.last_aggressor = outcome.last_aggressor();
        let ledger = outcome.into_ledger();
        if let Some(winner) = ledger.uncontested_winner() {
            self.progress = HandProgress::Uncontested(winner);
        } else {
            self.progress = match street {
                Street::Preflop => HandProgress::AwaitingFlop,
                Street::Flop => HandProgress::AwaitingTurn,
                Street::Turn => HandProgress::AwaitingRiver,
                Street::River => HandProgress::Showdown,
            };
        }
        self.ledger = Some(ledger);
        Ok(())
    }

    fn ensure_new_cards(&self, cards: &[Card]) -> Result<()> {
        let mut seen: BTreeSet<_> = self.board.iter().copied().collect();
        for card in cards {
            if !seen.insert(*card) {
                return Err(rules(format!("duplicate community card {card}")));
            }
        }
        Ok(())
    }

    fn complete(&mut self, distribution: PotDistribution) -> Result<&HandResult> {
        if distribution.total()? != self.ledger().pot()? {
            return Err(rules("payout total does not equal the pot"));
        }
        let mut final_stacks = Vec::with_capacity(self.ledger().seat_count());
        for index in 0..self.ledger().seat_count() {
            let seat = Seat::new(index).expect("table seat is valid");
            final_stacks.push(
                self.ledger()
                    .stack(seat)?
                    .checked_add(distribution.payout(seat)?)?,
            );
        }
        self.result = Some(HandResult {
            distribution,
            final_stacks,
        });
        self.progress = HandProgress::Complete;
        self.result
            .as_ref()
            .ok_or_else(|| rules("hand result was not retained"))
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    fn c(text: &str) -> Card {
        Card::from_str(text).unwrap()
    }

    #[test]
    fn plays_three_way_hand_through_side_pot_showdown() {
        let stacks = [
            Chips::from_units(100),
            Chips::from_units(60),
            Chips::from_units(20),
        ];
        let mut hand = HoldemHand::new(
            &stacks,
            Seat::new(0).unwrap(),
            Chips::from_units(1),
            Chips::from_units(2),
            Chips::from_units(100),
        )
        .unwrap();

        // Seat 0 is first preflop in a three-handed game.  The three all-ins create
        // a 60-chip main pot, an 80-chip side pot, and a 40-chip final side pot.
        hand.apply_action(Seat::new(0).unwrap(), BetAction::AllIn)
            .unwrap();
        hand.apply_action(Seat::new(1).unwrap(), BetAction::AllIn)
            .unwrap();
        hand.apply_action(Seat::new(2).unwrap(), BetAction::AllIn)
            .unwrap();
        assert_eq!(hand.progress(), HandProgress::AwaitingFlop);

        hand.open_flop([c("2c"), c("7d"), c("9h")]).unwrap();
        hand.open_turn(c("Js")).unwrap();
        hand.open_river(c("Qc")).unwrap();
        assert_eq!(hand.progress(), HandProgress::Showdown);

        let result = hand
            .settle_showdown(&[
                (Seat::new(0).unwrap(), Some([c("Ac"), c("Ad")])),
                (Seat::new(1).unwrap(), Some([c("Kc"), c("Kd")])),
                (Seat::new(2).unwrap(), Some([c("9c"), c("9d")])),
            ])
            .unwrap();
        assert_eq!(
            result.distribution().total().unwrap(),
            Chips::from_units(180)
        );
        assert_eq!(
            result.final_stacks().iter().map(|v| v.units()).sum::<u64>(),
            180
        );
    }

    #[test]
    fn economic_cap_rolls_back_excessive_action() {
        let mut hand = HoldemHand::new(
            &[Chips::from_units(100), Chips::from_units(100)],
            Seat::new(0).unwrap(),
            Chips::from_units(1),
            Chips::from_units(2),
            Chips::from_units(20),
        )
        .unwrap();
        let actor = hand.next_to_act().unwrap();
        assert!(hand.apply_action(actor, BetAction::AllIn).is_err());
        assert_eq!(
            hand.ledger().total_contributed(actor).unwrap(),
            Chips::from_units(1)
        );
    }
}
