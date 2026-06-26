//! No-limit Texas Hold'em betting, side pots, and deterministic payout accounting.
//!
//! The betting engine is independent of networking and cryptography.  Every peer can
//! replay the same signed actions and obtain byte-for-byte equivalent public state.
//! The implementation supports short all-ins that do not reopen raising, a detail that
//! is frequently mishandled by otherwise reasonable poker state machines.

use std::fmt;

use crate::card::HandValue;
use crate::types::{Chips, MAX_PLAYERS, MIN_PLAYERS, Seat};
use mental_card_core::error::{Result, configuration, rules};

/// A betting street in a Hold'em hand.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum Street {
    /// The betting round after hole cards and blinds.
    Preflop = 0,
    /// The betting round after the three-card flop.
    Flop = 1,
    /// The betting round after the turn card.
    Turn = 2,
    /// The betting round after the river card.
    River = 3,
}

impl fmt::Display for Street {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Preflop => "preflop",
            Self::Flop => "flop",
            Self::Turn => "turn",
            Self::River => "river",
        })
    }
}

/// Public state of a seat during one hand.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HandSeatStatus {
    /// The seat received cards and can still act.
    Playing,
    /// The seat folded and can no longer win any pot.
    Folded,
    /// The seat has no remaining stack but remains eligible for applicable pots.
    AllIn,
    /// The seat did not receive cards in this hand.
    NotDealt,
}

/// A player's action on their turn.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BetAction {
    /// Surrender the hand.
    Fold,
    /// Decline to bet when no call is required.
    Check,
    /// Match the current bet, or contribute the remaining stack if it is smaller.
    Call,
    /// Increase the seat's total contribution on this street to the given amount.
    ///
    /// The value is a **raise-to** amount, never a raise-by amount.  A target smaller
    /// than a full minimum raise is accepted only when it is exactly the player's
    /// all-in total.
    RaiseTo(Chips),
    /// Contribute the entire remaining stack.
    AllIn,
}

/// Legal choices for the seat whose turn it is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LegalActions {
    to_call: Chips,
    minimum_raise_to: Option<Chips>,
    maximum_raise_to: Chips,
    raise_reopened: bool,
}

impl LegalActions {
    /// Amount required to call.  It may exceed the player's remaining stack.
    #[must_use]
    pub const fn to_call(self) -> Chips {
        self.to_call
    }

    /// Smallest legal full raise-to amount, or `None` if no full raise is possible.
    #[must_use]
    pub const fn minimum_raise_to(self) -> Option<Chips> {
        self.minimum_raise_to
    }

    /// Largest raise-to amount, equal to the player's current street contribution plus stack.
    #[must_use]
    pub const fn maximum_raise_to(self) -> Chips {
        self.maximum_raise_to
    }

    /// Whether this player may make a full raise after the preceding action sequence.
    #[must_use]
    pub const fn raise_reopened(self) -> bool {
        self.raise_reopened
    }

    /// Returns whether checking is legal.
    #[must_use]
    pub const fn can_check(self) -> bool {
        self.to_call.is_zero()
    }
}

/// Public betting context for the seat whose turn it is.
///
/// This contains only table facts derivable from the public ledger. Hosts can pass it
/// to UIs or bots without exposing hole cards, deck order, reveal shares, or local
/// peer secrets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActionContext {
    pot: Chips,
    actor_stack: Chips,
    actor_street_contributed: Chips,
    effective_stack: Option<Chips>,
    live_opponents: usize,
    matched_bet_opponents: usize,
    big_blind: Chips,
    current_bet: Chips,
}

impl ActionContext {
    /// Current pot size before the actor chooses.
    #[must_use]
    pub const fn pot(self) -> Chips {
        self.pot
    }

    /// Actor's remaining stack before choosing.
    #[must_use]
    pub const fn actor_stack(self) -> Chips {
        self.actor_stack
    }

    /// Actor's contribution on the current betting street.
    #[must_use]
    pub const fn actor_street_contributed(self) -> Chips {
        self.actor_street_contributed
    }

    /// Smallest remaining stack among live playing opponents, if any can still act.
    #[must_use]
    pub const fn effective_stack(self) -> Option<Chips> {
        self.effective_stack
    }

    /// Number of opponents who remain eligible to contest the pot.
    #[must_use]
    pub const fn live_opponents(self) -> usize {
        self.live_opponents
    }

    /// Number of opponents whose current street contribution has matched the bet.
    #[must_use]
    pub const fn matched_bet_opponents(self) -> usize {
        self.matched_bet_opponents
    }

    /// Configured big blind, also the minimum opening bet.
    #[must_use]
    pub const fn big_blind(self) -> Chips {
        self.big_blind
    }

    /// Current highest required street contribution.
    #[must_use]
    pub const fn current_bet(self) -> Chips {
        self.current_bet
    }
}

/// Per-seat chip accounting for a hand.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandLedger {
    button: Seat,
    big_blind: Chips,
    seats: Vec<SeatLedger>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SeatLedger {
    stack: Chips,
    total_contributed: Chips,
    street_contributed: Chips,
    status: HandSeatStatus,
}

impl HandLedger {
    /// Creates a hand ledger from table stacks.
    ///
    /// Seats with a zero stack are marked [`HandSeatStatus::NotDealt`].
    pub fn new(stacks: &[Chips], button: Seat, big_blind: Chips) -> Result<Self> {
        if !(MIN_PLAYERS..=MAX_PLAYERS).contains(&stacks.len()) {
            return Err(configuration(format!(
                "a table requires {MIN_PLAYERS}..={MAX_PLAYERS} seats, got {}",
                stacks.len()
            )));
        }
        if button.index() >= stacks.len() {
            return Err(configuration("button seat is outside the table"));
        }
        if big_blind.is_zero() {
            return Err(configuration("big blind must be positive"));
        }
        let dealt = stacks.iter().filter(|stack| !stack.is_zero()).count();
        if dealt < MIN_PLAYERS {
            return Err(configuration("at least two players need non-zero stacks"));
        }

        let seats = stacks
            .iter()
            .copied()
            .map(|stack| SeatLedger {
                stack,
                total_contributed: Chips::ZERO,
                street_contributed: Chips::ZERO,
                status: if stack.is_zero() {
                    HandSeatStatus::NotDealt
                } else {
                    HandSeatStatus::Playing
                },
            })
            .collect();
        Ok(Self {
            button,
            big_blind,
            seats,
        })
    }

    /// Returns the number of seats at the table, including seats not dealt in.
    #[must_use]
    pub fn seat_count(&self) -> usize {
        self.seats.len()
    }

    /// Returns the dealer button seat.
    #[must_use]
    pub const fn button(&self) -> Seat {
        self.button
    }

    /// Returns the configured big blind, which is also the minimum opening bet.
    #[must_use]
    pub const fn big_blind(&self) -> Chips {
        self.big_blind
    }

    /// Returns a seat's remaining stack.
    pub fn stack(&self, seat: Seat) -> Result<Chips> {
        Ok(self.seat(seat)?.stack)
    }

    /// Returns a seat's total contribution to all pots in this hand.
    pub fn total_contributed(&self, seat: Seat) -> Result<Chips> {
        Ok(self.seat(seat)?.total_contributed)
    }

    /// Returns a seat's contribution on the current street.
    pub fn street_contributed(&self, seat: Seat) -> Result<Chips> {
        Ok(self.seat(seat)?.street_contributed)
    }

    /// Returns the seat's hand status.
    pub fn status(&self, seat: Seat) -> Result<HandSeatStatus> {
        Ok(self.seat(seat)?.status)
    }

    /// Posts a forced blind, contributing as much as possible when the stack is short.
    ///
    /// The returned amount is the actual contribution, which may be smaller than the
    /// requested blind.  A player whose stack reaches zero becomes all-in.
    pub fn post_forced_bet(&mut self, seat: Seat, requested: Chips) -> Result<Chips> {
        if requested.is_zero() {
            return Err(rules("forced bet must be positive"));
        }
        let player = self.seat_mut(seat)?;
        if player.status != HandSeatStatus::Playing {
            return Err(rules(format!(
                "{seat} cannot post a blind in state {:?}",
                player.status
            )));
        }
        let actual = requested.min(player.stack);
        Self::contribute(player, actual)?;
        Ok(actual)
    }

    /// Clears street-local contributions before a postflop betting round.
    pub fn start_new_street(&mut self) {
        for seat in &mut self.seats {
            seat.street_contributed = Chips::ZERO;
        }
    }

    /// Returns the current pot size.
    pub fn pot(&self) -> Result<Chips> {
        self.seats.iter().try_fold(Chips::ZERO, |total, seat| {
            total.checked_add(seat.total_contributed)
        })
    }

    /// Returns seats that received cards and have not folded.
    #[must_use]
    pub fn live_seats(&self) -> Vec<Seat> {
        self.seats
            .iter()
            .enumerate()
            .filter(|&(_, seat)| {
                matches!(seat.status, HandSeatStatus::Playing | HandSeatStatus::AllIn)
            })
            .map(|(index, _)| Seat::new(index).expect("table seat is valid"))
            .collect()
    }

    /// Returns the sole non-folded seat, if every other dealt seat has folded.
    #[must_use]
    pub fn uncontested_winner(&self) -> Option<Seat> {
        let mut live = self.live_seats().into_iter();
        let winner = live.next()?;
        live.next().is_none().then_some(winner)
    }

    /// Returns the next dealt seat clockwise, skipping seats that sat out the hand.
    pub fn next_dealt_after(&self, seat: Seat) -> Result<Seat> {
        self.next_matching_after(seat, |state| state.status != HandSeatStatus::NotDealt)
            .ok_or_else(|| rules("no dealt seat exists after the requested seat"))
    }

    fn seat(&self, seat: Seat) -> Result<&SeatLedger> {
        self.seats
            .get(seat.index())
            .ok_or_else(|| rules(format!("{seat} is outside this table")))
    }

    fn seat_mut(&mut self, seat: Seat) -> Result<&mut SeatLedger> {
        self.seats
            .get_mut(seat.index())
            .ok_or_else(|| rules(format!("{seat} is outside this table")))
    }

    fn contribute(player: &mut SeatLedger, amount: Chips) -> Result<()> {
        player.stack = player.stack.checked_sub(amount)?;
        player.total_contributed = player.total_contributed.checked_add(amount)?;
        player.street_contributed = player.street_contributed.checked_add(amount)?;
        if player.stack.is_zero() {
            player.status = HandSeatStatus::AllIn;
        }
        Ok(())
    }

    fn next_matching_after(
        &self,
        seat: Seat,
        predicate: impl Fn(&SeatLedger) -> bool,
    ) -> Option<Seat> {
        (1..=self.seats.len()).find_map(|offset| {
            let index = (seat.index() + offset) % self.seats.len();
            predicate(&self.seats[index]).then(|| Seat::new(index).expect("valid table seat"))
        })
    }
}

/// A live no-limit betting round.
///
/// Construct the round after posting blinds (preflop) or after calling
/// [`HandLedger::start_new_street`] (postflop).  The engine owns the ledger while the
/// round is active so callers cannot mutate contributions behind its invariants.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BettingRound {
    street: Street,
    ledger: HandLedger,
    current_bet: Chips,
    minimum_raise_increment: Chips,
    has_full_wager: bool,
    next_to_act: Option<Seat>,
    pending: Vec<bool>,
    raise_reopened: Vec<bool>,
    last_action_bet: Vec<Option<Chips>>,
    reopen_increment: Vec<Chips>,
    last_aggressor: Option<Seat>,
    complete: bool,
}

impl BettingRound {
    /// Starts a betting round.
    ///
    /// `bring_in` is normally the big blind preflop and zero postflop.  Keeping it
    /// separate from the amount actually posted correctly handles a short all-in big
    /// blind: other players must still call the full bring-in.
    pub fn new(
        ledger: HandLedger,
        street: Street,
        first_to_act: Seat,
        bring_in: Chips,
    ) -> Result<Self> {
        if first_to_act.index() >= ledger.seat_count() {
            return Err(rules("first-to-act seat is outside the table"));
        }
        let maximum_contribution = ledger
            .seats
            .iter()
            .map(|seat| seat.street_contributed)
            .max()
            .unwrap_or(Chips::ZERO);
        let current_bet = maximum_contribution.max(bring_in);
        let mut pending: Vec<_> = ledger
            .seats
            .iter()
            .map(|seat| seat.status == HandSeatStatus::Playing)
            .collect();
        let mut raise_reopened = pending.clone();
        let last_action_bet = vec![None; ledger.seat_count()];
        let reopen_increment = vec![ledger.big_blind; ledger.seat_count()];

        // When every opponent is already all-in, a sole actionable player has no
        // decision unless it still owes chips to the current wager.  Avoid requiring
        // meaningless checks on later streets and do not leave a raise opportunity
        // that no opponent could answer.
        let playing_indices: Vec<_> = ledger
            .seats
            .iter()
            .enumerate()
            .filter_map(|(index, seat)| (seat.status == HandSeatStatus::Playing).then_some(index))
            .collect();
        if let [only] = playing_indices.as_slice() {
            if ledger.seats[*only].street_contributed >= current_bet {
                pending[*only] = false;
                raise_reopened[*only] = false;
            }
        }

        let complete = ledger.uncontested_winner().is_some() || !pending.iter().any(|&value| value);
        let next_to_act = if complete {
            None
        } else if pending[first_to_act.index()] {
            Some(first_to_act)
        } else {
            ledger.next_matching_after(first_to_act, |seat| seat.status == HandSeatStatus::Playing)
        };

        Ok(Self {
            street,
            minimum_raise_increment: ledger.big_blind,
            has_full_wager: !bring_in.is_zero() || current_bet >= ledger.big_blind,
            ledger,
            current_bet,
            next_to_act,
            pending,
            raise_reopened,
            last_action_bet,
            reopen_increment,
            last_aggressor: None,
            complete,
        })
    }

    /// Returns the street being played.
    #[must_use]
    pub const fn street(&self) -> Street {
        self.street
    }

    /// Returns the seat that must act next, or `None` once the round is complete.
    #[must_use]
    pub const fn next_to_act(&self) -> Option<Seat> {
        self.next_to_act
    }

    /// Returns whether no further action is required on this street.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.complete
    }

    /// Returns the current highest required street contribution.
    #[must_use]
    pub const fn current_bet(&self) -> Chips {
        self.current_bet
    }

    /// Returns the most recent seat that increased the current bet.
    #[must_use]
    pub const fn last_aggressor(&self) -> Option<Seat> {
        self.last_aggressor
    }

    /// Borrows the protected hand ledger for display or checkpointing.
    #[must_use]
    pub const fn ledger(&self) -> &HandLedger {
        &self.ledger
    }

    /// Computes legal amounts for the seat whose turn it is.
    pub fn legal_actions(&self) -> Result<LegalActions> {
        let seat = self
            .next_to_act
            .ok_or_else(|| rules("the betting round is complete"))?;
        let player = self.ledger.seat(seat)?;
        let to_call = self.current_bet.checked_sub(player.street_contributed)?;
        let maximum_raise_to = player.street_contributed.checked_add(player.stack)?;
        let minimum_full_raise = if self.has_full_wager {
            self.current_bet.checked_add(self.minimum_raise_increment)?
        } else {
            self.ledger.big_blind
        };
        let minimum_raise_to = (self.raise_reopened[seat.index()]
            && self.has_actionable_opponent(seat)
            && maximum_raise_to >= minimum_full_raise)
            .then_some(minimum_full_raise);
        Ok(LegalActions {
            to_call,
            minimum_raise_to,
            maximum_raise_to,
            raise_reopened: self.raise_reopened[seat.index()],
        })
    }

    /// Returns public betting context for the seat whose turn it is.
    pub fn action_context(&self) -> Result<ActionContext> {
        let actor = self
            .next_to_act
            .ok_or_else(|| rules("the betting round is complete"))?;
        let player = self.ledger.seat(actor)?;
        let mut live_opponents = 0;
        let mut matched_bet_opponents = 0;
        let mut effective_stack = None;

        for index in 0..self.ledger.seat_count() {
            let seat = Seat::new(index).expect("ledger seat count fits seats");
            if seat == actor {
                continue;
            }
            let opponent = self.ledger.seat(seat)?;
            if matches!(
                opponent.status,
                HandSeatStatus::Playing | HandSeatStatus::AllIn
            ) {
                live_opponents += 1;
                if opponent.street_contributed >= self.current_bet {
                    matched_bet_opponents += 1;
                }
            }
            if opponent.status == HandSeatStatus::Playing {
                effective_stack = Some(
                    effective_stack
                        .map_or(opponent.stack, |current: Chips| current.min(opponent.stack)),
                );
            }
        }

        Ok(ActionContext {
            pot: self.ledger.pot()?,
            actor_stack: player.stack,
            actor_street_contributed: player.street_contributed,
            effective_stack,
            live_opponents,
            matched_bet_opponents,
            big_blind: self.ledger.big_blind,
            current_bet: self.current_bet,
        })
    }

    /// Applies one action from the expected seat and advances turn order.
    ///
    /// # Errors
    ///
    /// Returns an error for an out-of-turn actor, an illegal check or raise, chip
    /// underflow/overflow, or an action after the round completed.
    pub fn apply(&mut self, actor: Seat, action: BetAction) -> Result<()> {
        let expected = self
            .next_to_act
            .ok_or_else(|| rules("the betting round is already complete"))?;
        if actor != expected {
            return Err(rules(format!(
                "out-of-turn action from {actor}; expected {expected}"
            )));
        }
        if self.ledger.seat(actor)?.status != HandSeatStatus::Playing {
            return Err(rules(format!("{actor} is not able to act")));
        }

        match action {
            BetAction::Fold => self.fold(actor)?,
            BetAction::Check => self.check(actor)?,
            BetAction::Call => self.call(actor)?,
            BetAction::RaiseTo(target) => self.raise_to(actor, target)?,
            BetAction::AllIn => {
                let player = self.ledger.seat(actor)?;
                let target = player.street_contributed.checked_add(player.stack)?;
                if target > self.current_bet {
                    self.raise_to(actor, target)?;
                } else {
                    self.call(actor)?;
                }
            }
        }

        self.advance_after(actor);
        Ok(())
    }

    /// Finishes the round and returns its ledger and metadata.
    pub fn finish(self) -> Result<BettingRoundOutcome> {
        if !self.complete {
            return Err(rules("cannot finish an incomplete betting round"));
        }
        Ok(BettingRoundOutcome {
            ledger: self.ledger,
            last_aggressor: self.last_aggressor,
        })
    }

    fn fold(&mut self, actor: Seat) -> Result<()> {
        self.ledger.seat_mut(actor)?.status = HandSeatStatus::Folded;
        self.mark_acted(actor);
        Ok(())
    }

    fn check(&mut self, actor: Seat) -> Result<()> {
        let player = self.ledger.seat(actor)?;
        if player.street_contributed != self.current_bet {
            return Err(rules(format!("{actor} cannot check while facing a bet")));
        }
        self.mark_acted(actor);
        Ok(())
    }

    fn call(&mut self, actor: Seat) -> Result<()> {
        let player = self.ledger.seat(actor)?;
        let to_call = self.current_bet.checked_sub(player.street_contributed)?;
        if to_call.is_zero() {
            return Err(rules(format!("{actor} has nothing to call; use check")));
        }
        let amount = to_call.min(player.stack);
        HandLedger::contribute(self.ledger.seat_mut(actor)?, amount)?;
        self.mark_acted(actor);
        Ok(())
    }

    fn raise_to(&mut self, actor: Seat, target: Chips) -> Result<()> {
        let player = *self.ledger.seat(actor)?;
        let maximum = player.street_contributed.checked_add(player.stack)?;
        if !self.has_actionable_opponent(actor) {
            return Err(rules(format!(
                "{actor} cannot raise when no opponent can take another action"
            )));
        }
        if target <= self.current_bet {
            return Err(rules(format!(
                "raise-to amount {target} must exceed current bet {}",
                self.current_bet
            )));
        }
        if target > maximum {
            return Err(rules(format!(
                "raise-to amount {target} exceeds {actor}'s all-in total {maximum}"
            )));
        }
        if !self.raise_reopened[actor.index()] {
            return Err(rules(format!(
                "raising has not been reopened for {actor} after a short all-in"
            )));
        }

        let all_in = target == maximum;
        let (full_raise, next_increment) = if self.has_full_wager {
            let increment = target.checked_sub(self.current_bet)?;
            (increment >= self.minimum_raise_increment, increment)
        } else {
            // An all-in opening wager smaller than the big blind is incomplete.  A
            // later player may complete it to the big blind rather than having to add
            // a full big blind on top of the short amount.  Once a full opening wager
            // is made, its total size becomes the minimum increment for the next raise.
            (target >= self.ledger.big_blind, target)
        };
        if !full_raise && !all_in {
            let minimum = if self.has_full_wager {
                self.current_bet.checked_add(self.minimum_raise_increment)?
            } else {
                self.ledger.big_blind
            };
            return Err(rules(format!(
                "raise-to amount {target} is below the full minimum {minimum} and is not all-in"
            )));
        }

        let contribution = target.checked_sub(player.street_contributed)?;
        HandLedger::contribute(self.ledger.seat_mut(actor)?, contribution)?;
        self.current_bet = target;
        self.last_aggressor = Some(actor);

        if full_raise {
            self.has_full_wager = true;
            self.minimum_raise_increment = next_increment;
            for index in 0..self.ledger.seat_count() {
                let can_act = self.ledger.seats[index].status == HandSeatStatus::Playing;
                self.pending[index] = can_act;
                self.raise_reopened[index] = can_act;
                self.last_action_bet[index] = None;
                self.reopen_increment[index] = self.minimum_raise_increment;
            }
            self.mark_acted(actor);
        } else {
            // A short all-in changes the amount owed.  It does not by itself reopen
            // raising for a player who already acted while facing a full wager, but
            // multiple short all-ins can cumulatively reach the prior full-raise size.
            // Before any full opening wager, a player who checked retains the right to
            // complete or raise over an incomplete all-in opening bet.
            self.mark_acted(actor);
            for index in 0..self.ledger.seat_count() {
                let seat = self.ledger.seats[index];
                if seat.status != HandSeatStatus::Playing {
                    continue;
                }
                if seat.street_contributed < self.current_bet {
                    self.pending[index] = true;
                }
                if !self.has_full_wager {
                    self.raise_reopened[index] = true;
                } else if !self.raise_reopened[index] {
                    if let Some(last_bet) = self.last_action_bet[index] {
                        let increase = self
                            .current_bet
                            .checked_sub(last_bet)
                            .expect("current bet never decreases within a round");
                        if increase >= self.reopen_increment[index] {
                            self.raise_reopened[index] = true;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn has_actionable_opponent(&self, actor: Seat) -> bool {
        self.ledger
            .seats
            .iter()
            .enumerate()
            .any(|(index, seat)| index != actor.index() && seat.status == HandSeatStatus::Playing)
    }

    fn mark_acted(&mut self, actor: Seat) {
        self.pending[actor.index()] = false;
        self.raise_reopened[actor.index()] = false;
        self.last_action_bet[actor.index()] = Some(self.current_bet);
        self.reopen_increment[actor.index()] = self.minimum_raise_increment;
    }

    fn advance_after(&mut self, actor: Seat) {
        if self.ledger.uncontested_winner().is_some() {
            self.complete = true;
            self.next_to_act = None;
            return;
        }

        for index in 0..self.ledger.seat_count() {
            if self.ledger.seats[index].status != HandSeatStatus::Playing {
                self.pending[index] = false;
            }
        }
        if !self.pending.iter().any(|pending| *pending) {
            self.complete = true;
            self.next_to_act = None;
            return;
        }

        self.next_to_act = (1..=self.ledger.seat_count()).find_map(|offset| {
            let index = (actor.index() + offset) % self.ledger.seat_count();
            self.pending[index].then(|| Seat::new(index).expect("table seat index is valid"))
        });
        self.complete = self.next_to_act.is_none();
    }
}

/// Result of a completed betting round.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BettingRoundOutcome {
    ledger: HandLedger,
    last_aggressor: Option<Seat>,
}

impl BettingRoundOutcome {
    /// Returns the updated hand ledger.
    #[must_use]
    pub const fn ledger(&self) -> &HandLedger {
        &self.ledger
    }

    /// Consumes the outcome and returns the updated hand ledger.
    #[must_use]
    pub fn into_ledger(self) -> HandLedger {
        self.ledger
    }

    /// Returns the final aggressor on this street, if any.
    #[must_use]
    pub const fn last_aggressor(&self) -> Option<Seat> {
        self.last_aggressor
    }
}

/// One main-pot, side-pot, or unmatched-wager return.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PotAward {
    amount: Chips,
    winners: Vec<Seat>,
}

impl PotAward {
    /// Returns the pot amount.
    #[must_use]
    pub const fn amount(&self) -> Chips {
        self.amount
    }

    /// Returns tied winners in odd-chip order, or the sole contributor for a return.
    #[must_use]
    pub fn winners(&self) -> &[Seat] {
        &self.winners
    }
}

/// Deterministic payout result for a completed hand.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PotDistribution {
    payouts: Vec<Chips>,
    awards: Vec<PotAward>,
}

impl PotDistribution {
    /// Returns the payout for one seat.
    pub fn payout(&self, seat: Seat) -> Result<Chips> {
        self.payouts
            .get(seat.index())
            .copied()
            .ok_or_else(|| rules(format!("{seat} is outside this payout vector")))
    }

    /// Returns all payouts in seat order.
    #[must_use]
    pub fn payouts(&self) -> &[Chips] {
        &self.payouts
    }

    /// Returns the main pot followed by side pots from shallowest to deepest.
    #[must_use]
    pub fn awards(&self) -> &[PotAward] {
        &self.awards
    }

    /// Returns the total amount distributed.
    pub fn total(&self) -> Result<Chips> {
        self.payouts
            .iter()
            .copied()
            .try_fold(Chips::ZERO, Chips::checked_add)
    }
}

/// Awards the entire pot when every player but one has folded.
pub fn distribute_uncontested(ledger: &HandLedger) -> Result<PotDistribution> {
    let winner = ledger
        .uncontested_winner()
        .ok_or_else(|| rules("the hand is not uncontested"))?;
    let pot = ledger.pot()?;
    let mut payouts = vec![Chips::ZERO; ledger.seat_count()];
    payouts[winner.index()] = pot;
    Ok(PotDistribution {
        payouts,
        awards: vec![PotAward {
            amount: pot,
            winners: vec![winner],
        }],
    })
}

/// Builds main and side pots and splits them among the strongest revealed hands.
///
/// `values[seat]` must contain a hand value for every non-folded seat that remains
/// eligible at showdown.  A mucked hand is represented by `None` and cannot win.
/// Folded contributions still fund the pots.
///
/// Tied remainders are awarded one chip at a time clockwise from the seat immediately
/// left of the button, a deterministic and commonly used odd-chip rule.
pub fn distribute_showdown(
    ledger: &HandLedger,
    values: &[Option<HandValue>],
) -> Result<PotDistribution> {
    if values.len() != ledger.seat_count() {
        return Err(rules("hand-value vector length does not match the table"));
    }

    let mut levels: Vec<Chips> = ledger
        .seats
        .iter()
        .map(|seat| seat.total_contributed)
        .filter(|amount| !amount.is_zero())
        .collect();
    levels.sort_unstable();
    levels.dedup();

    let mut payouts = vec![Chips::ZERO; ledger.seat_count()];
    let mut awards = Vec::with_capacity(levels.len());
    let mut previous = Chips::ZERO;

    for level in levels {
        let contributors: Vec<usize> = ledger
            .seats
            .iter()
            .enumerate()
            .filter_map(|(index, seat)| (seat.total_contributed >= level).then_some(index))
            .collect();
        let tier = level.checked_sub(previous)?;
        let amount =
            tier.checked_mul(u64::try_from(contributors.len()).expect("seat count fits u64"))?;
        previous = level;
        if amount.is_zero() {
            continue;
        }

        // A tier funded by only one seat is an unmatched wager, not a contestable
        // side pot.  Return it to that contributor even if the hand was later mucked;
        // otherwise a valid showdown could have no eligible hand for that tier.
        if let [only] = contributors.as_slice() {
            let seat = Seat::new(*only).expect("table seat is valid");
            payouts[*only] = payouts[*only].checked_add(amount)?;
            awards.push(PotAward {
                amount,
                winners: vec![seat],
            });
            continue;
        }

        let eligible: Vec<(Seat, HandValue)> = contributors
            .iter()
            .filter_map(|index| {
                let seat_state = ledger.seats[*index];
                if seat_state.status == HandSeatStatus::Folded
                    || seat_state.status == HandSeatStatus::NotDealt
                {
                    return None;
                }
                values[*index].map(|value| (Seat::new(*index).expect("table seat is valid"), value))
            })
            .collect();
        let best = eligible
            .iter()
            .map(|(_, value)| *value)
            .max()
            .ok_or_else(|| rules("a pot has no eligible revealed hand"))?;
        let mut winners: Vec<Seat> = eligible
            .into_iter()
            .filter_map(|(seat, value)| (value == best).then_some(seat))
            .collect();
        winners.sort_by_key(|seat| clockwise_distance(ledger.button, *seat, ledger.seat_count()));

        let winner_count = u64::try_from(winners.len()).expect("winner count fits u64");
        let equal_share = Chips::from_units(amount.units() / winner_count);
        let remainder =
            usize::try_from(amount.units() % winner_count).expect("remainder fits usize");
        winners
            .iter()
            .enumerate()
            .try_for_each(|(ordinal, winner)| {
                let odd = Chips::from_units(u64::from(ordinal < remainder));
                let share = equal_share.checked_add(odd)?;
                payouts[winner.index()] = payouts[winner.index()].checked_add(share)?;
                Ok::<_, crate::Error>(())
            })?;
        awards.push(PotAward { amount, winners });
    }

    let distribution = PotDistribution { payouts, awards };
    if distribution.total()? != ledger.pot()? {
        return Err(rules("internal side-pot accounting did not conserve chips"));
    }
    Ok(distribution)
}

fn clockwise_distance(button: Seat, seat: Seat, seat_count: usize) -> usize {
    (seat.index() + seat_count - button.index() - 1) % seat_count
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use crate::card::evaluate_best;

    use super::*;

    fn chips(value: u64) -> Chips {
        Chips::from_units(value)
    }

    fn seat(index: usize) -> Seat {
        Seat::new(index).expect("seat")
    }

    #[test]
    fn short_all_in_requires_calls_but_does_not_reopen_raising() {
        let mut ledger = HandLedger::new(&[chips(100), chips(100), chips(25)], seat(0), chips(10))
            .expect("ledger");
        ledger
            .post_forced_bet(seat(1), chips(5))
            .expect("small blind");
        ledger
            .post_forced_bet(seat(2), chips(10))
            .expect("big blind");
        let mut round =
            BettingRound::new(ledger, Street::Preflop, seat(0), chips(10)).expect("round");

        round
            .apply(seat(0), BetAction::RaiseTo(chips(20)))
            .expect("full raise");
        round.apply(seat(1), BetAction::Call).expect("call");
        round
            .apply(seat(2), BetAction::AllIn)
            .expect("short all-in raise to 25");

        assert_eq!(round.next_to_act(), Some(seat(0)));
        assert!(!round.legal_actions().expect("legal").raise_reopened());
        assert!(round.apply(seat(0), BetAction::RaiseTo(chips(40))).is_err());
        round
            .apply(seat(0), BetAction::Call)
            .expect("call short raise");
        round
            .apply(seat(1), BetAction::Call)
            .expect("call short raise");
        assert!(round.is_complete());
    }

    #[test]
    fn cumulative_short_all_ins_reopen_raising() {
        let mut ledger = HandLedger::new(
            &[chips(100), chips(100), chips(25), chips(30)],
            seat(0),
            chips(10),
        )
        .expect("ledger");
        ledger
            .post_forced_bet(seat(2), chips(10))
            .expect("bring-in");
        let mut round =
            BettingRound::new(ledger, Street::Preflop, seat(0), chips(10)).expect("round");

        round
            .apply(seat(0), BetAction::RaiseTo(chips(20)))
            .expect("full raise");
        round.apply(seat(1), BetAction::Call).expect("call");
        round
            .apply(seat(2), BetAction::AllIn)
            .expect("all-in to 25");
        round
            .apply(seat(3), BetAction::AllIn)
            .expect("all-in to 30");

        assert_eq!(round.next_to_act(), Some(seat(0)));
        let legal = round.legal_actions().expect("legal actions");
        assert!(legal.raise_reopened());
        assert_eq!(legal.minimum_raise_to(), Some(chips(40)));
        round
            .apply(seat(0), BetAction::RaiseTo(chips(40)))
            .expect("cumulative short raises reopened action");
    }

    #[test]
    fn incomplete_opening_all_in_can_be_completed_to_big_blind() {
        let ledger = HandLedger::new(&[chips(100), chips(5), chips(100)], seat(0), chips(10))
            .expect("ledger");
        let mut round =
            BettingRound::new(ledger, Street::Flop, seat(0), Chips::ZERO).expect("round");

        round.apply(seat(0), BetAction::Check).expect("check");
        round
            .apply(seat(1), BetAction::AllIn)
            .expect("short opening");
        let legal = round.legal_actions().expect("legal actions");
        assert_eq!(legal.to_call(), chips(5));
        assert_eq!(legal.minimum_raise_to(), Some(chips(10)));
        round
            .apply(seat(2), BetAction::RaiseTo(chips(10)))
            .expect("complete to minimum opening bet");

        assert_eq!(round.next_to_act(), Some(seat(0)));
        let reopened = round.legal_actions().expect("reopened actions");
        assert!(reopened.raise_reopened());
        assert_eq!(reopened.minimum_raise_to(), Some(chips(20)));
    }

    #[test]
    fn action_context_reports_only_public_betting_facts() {
        let mut ledger = HandLedger::new(&[chips(100); 5], seat(0), chips(2)).expect("ledger");
        ledger.post_forced_bet(seat(1), chips(1)).expect("sb");
        ledger.post_forced_bet(seat(2), chips(2)).expect("bb");
        let mut round =
            BettingRound::new(ledger, Street::Preflop, seat(3), chips(2)).expect("round");

        round.apply(seat(3), BetAction::Call).expect("seat3 call");
        round.apply(seat(4), BetAction::Call).expect("seat4 call");
        round.apply(seat(0), BetAction::Call).expect("button call");
        round.apply(seat(1), BetAction::Call).expect("sb call");

        assert_eq!(round.next_to_act(), Some(seat(2)));
        let context = round.action_context().expect("context");
        assert_eq!(context.pot(), chips(10));
        assert_eq!(context.actor_stack(), chips(98));
        assert_eq!(context.actor_street_contributed(), chips(2));
        assert_eq!(context.effective_stack(), Some(chips(98)));
        assert_eq!(context.live_opponents(), 4);
        assert_eq!(context.matched_bet_opponents(), 4);
        assert_eq!(context.big_blind(), chips(2));
        assert_eq!(context.current_bet(), chips(2));
    }

    #[test]
    fn lone_actionable_player_cannot_raise_over_an_all_in() {
        let mut ledger =
            HandLedger::new(&[chips(100), chips(20)], seat(0), chips(2)).expect("ledger");
        HandLedger::contribute(ledger.seat_mut(seat(1)).expect("seat"), chips(20))
            .expect("all-in contribution");
        let mut round =
            BettingRound::new(ledger, Street::Flop, seat(0), Chips::ZERO).expect("round");

        let legal = round.legal_actions().expect("legal actions");
        assert_eq!(legal.to_call(), chips(20));
        assert_eq!(legal.minimum_raise_to(), None);
        assert!(round.apply(seat(0), BetAction::RaiseTo(chips(40))).is_err());
        round.apply(seat(0), BetAction::Call).expect("call");
        assert!(round.is_complete());
    }

    #[test]
    fn unmatched_excess_is_returned_even_if_the_contributor_mucks() {
        let mut ledger =
            HandLedger::new(&[chips(100), chips(20)], seat(0), chips(2)).expect("ledger");
        HandLedger::contribute(ledger.seat_mut(seat(0)).expect("seat"), chips(100))
            .expect("large all-in");
        HandLedger::contribute(ledger.seat_mut(seat(1)).expect("seat"), chips(20))
            .expect("short call");
        let hand = evaluate_best(&parse("As Ad 2c 3d 4h 8s 9c")).expect("hand");
        let distribution = distribute_showdown(&ledger, &[None, Some(hand)]).expect("distribution");

        assert_eq!(distribution.payout(seat(0)).expect("refund"), chips(80));
        assert_eq!(distribution.payout(seat(1)).expect("pot"), chips(40));
        assert_eq!(distribution.total().expect("total"), chips(120));
    }

    #[test]
    fn side_pots_conserve_chips_and_respect_eligibility() {
        let mut ledger = HandLedger::new(&[chips(100), chips(60), chips(20)], seat(0), chips(2))
            .expect("ledger");
        for (index, amount) in [100, 60, 20].into_iter().enumerate() {
            HandLedger::contribute(ledger.seat_mut(seat(index)).expect("seat"), chips(amount))
                .expect("contribution");
        }

        let values = [
            Some(evaluate_best(&parse("As Ad 2c 3d 4h 8s 9c")).expect("hand")),
            Some(evaluate_best(&parse("Ks Kd 2c 3d 4h 8s 9c")).expect("hand")),
            Some(evaluate_best(&parse("Qs Qd Qc 3d 4h 8s 9c")).expect("hand")),
        ];
        let distribution = distribute_showdown(&ledger, &values).expect("distribution");

        // Seat 2 wins the 60-chip main pot, seat 0 wins the 80-chip first side pot
        // and the uncontested 40-chip final side pot.
        assert_eq!(distribution.payout(seat(0)).expect("payout"), chips(120));
        assert_eq!(distribution.payout(seat(1)).expect("payout"), chips(0));
        assert_eq!(distribution.payout(seat(2)).expect("payout"), chips(60));
        assert_eq!(distribution.total().expect("total"), chips(180));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn randomized_side_pots_conserve_chips_and_never_pay_folded_seats(
            player_count in MIN_PLAYERS..=MAX_PLAYERS,
            raw_contributions in any::<[u8; MAX_PLAYERS]>(),
            folded_flags in any::<[bool; MAX_PLAYERS]>(),
            raw_button in any::<u8>(),
        ) {
            let button = seat(usize::from(raw_button) % player_count);
            let stacks = vec![chips(100); player_count];
            let mut ledger = HandLedger::new(&stacks, button, chips(2)).expect("ledger");
            let mut contributions: Vec<_> = raw_contributions[..player_count]
                .iter()
                .map(|value| 1_u64 + u64::from(*value % 100))
                .collect();
            let maximum = contributions
                .iter()
                .copied()
                .max()
                .expect("at least two contributions");
            // Keeping seat zero live at the deepest contribution ensures that every
            // generated side pot is reachable and has at least one eligible hand.
            contributions[0] = maximum;

            for (index, amount) in contributions.iter().copied().enumerate() {
                HandLedger::contribute(
                    ledger.seat_mut(seat(index)).expect("seat"),
                    chips(amount),
                )
                .expect("bounded contribution");
                if index != 0 && folded_flags[index] && amount < 100 {
                    ledger.seat_mut(seat(index)).expect("seat").status =
                        HandSeatStatus::Folded;
                }
            }

            let tie = evaluate_best(&parse("As Kd Qc Jh 9s 2d 3c")).expect("hand");
            let values: Vec<_> = ledger
                .seats
                .iter()
                .map(|state| {
                    matches!(state.status, HandSeatStatus::Playing | HandSeatStatus::AllIn)
                        .then_some(tie)
                })
                .collect();
            let pot = ledger.pot().expect("pot");
            let distribution = distribute_showdown(&ledger, &values).expect("distribution");

            prop_assert_eq!(distribution.total().expect("total"), pot);
            let award_total = distribution
                .awards()
                .iter()
                .try_fold(Chips::ZERO, |total, award| total.checked_add(award.amount()))
                .expect("award total");
            prop_assert_eq!(award_total, pot);
            for (index, state) in ledger.seats.iter().enumerate() {
                if state.status == HandSeatStatus::Folded {
                    prop_assert_eq!(
                        distribution.payout(seat(index)).expect("payout"),
                        Chips::ZERO
                    );
                }
            }
        }
    }

    #[test]
    fn odd_chip_goes_clockwise_left_of_button() {
        let mut ledger =
            HandLedger::new(&[chips(5), chips(5), chips(5)], seat(0), chips(1)).expect("ledger");
        for index in 0..3 {
            HandLedger::contribute(ledger.seat_mut(seat(index)).expect("seat"), chips(5))
                .expect("contribution");
        }
        let tie = evaluate_best(&parse("As Kd Qc Jh 9s 2d 3c")).expect("hand");
        let weaker = evaluate_best(&parse("Ks Qd Jc Th 8s 2d 3c")).expect("hand");
        let distribution = distribute_showdown(&ledger, &[Some(tie), Some(tie), Some(weaker)])
            .expect("distribution");
        assert_eq!(distribution.payout(seat(0)).expect("payout"), chips(7));
        assert_eq!(distribution.payout(seat(1)).expect("payout"), chips(8));
    }

    fn parse(text: &str) -> Vec<crate::Card> {
        text.split_whitespace()
            .map(|card| card.parse().expect("card"))
            .collect()
    }
}
