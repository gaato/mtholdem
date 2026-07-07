//! Host-side table event vocabulary for local runners.
//!
//! This crate is intentionally above the Hold'em rules crate and below target
//! wrappers. It does not own transport, storage, timers, terminal I/O, Godot
//! types, or bot policy. It provides typed event summaries that CLI, C ABI,
//! Godot, WASM, or tests can share without freezing CLI wording as an API.

use std::fmt;

use mtholdem::betting::{ActionContext, BetAction, LegalActions, Street};
use mtholdem::card::evaluate_best;
use mtholdem::{Card, Chips, HandCategory, HandNumber, Result, Seat};

/// Local table execution backend selected by a host.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TableBackend {
    /// Peer-backed in-process execution through `mtholdem-peer`.
    Peer,
    /// Lightweight local rules engine execution without cryptographic artifacts.
    RulesOnly,
}

impl TableBackend {
    /// Returns the stable CLI/config name for the backend.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Peer => "peer",
            Self::RulesOnly => "rules-only",
        }
    }
}

impl fmt::Display for TableBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// A table-oriented event emitted above the peer or rules backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TableEvent {
    /// A hand started with a selected backend, button, and carried stacks.
    HandStarted(HandStarted),
    /// Local-visible hole cards were dealt or updated for a seat.
    HoleCards(HoleCards),
    /// A human-controlled seat must choose one legal action.
    ActionRequest(ActionRequest),
    /// A seat took an action.
    ActionTaken(ActionTaken),
    /// A public board street opened.
    BoardOpened(BoardOpened),
    /// A hand reached showdown with one or more opened hands.
    Showdown(Showdown),
    /// A hand ended and produced final stacks.
    HandFinished(HandFinished),
    /// The table session ended because its hand limit or liveness condition was reached.
    SessionFinished(SessionFinished),
}

/// Start-of-hand table state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandStarted {
    /// Hand number in the current table session.
    pub hand: HandNumber,
    /// Backend used for this hand.
    pub backend: TableBackend,
    /// Dealer button for this hand.
    pub button: Seat,
    /// Small blind posted for this hand. Hosts with a blind schedule report the
    /// level actually applied, not the session's base blinds.
    pub small_blind: Chips,
    /// Big blind posted for this hand.
    pub big_blind: Chips,
    /// Starting stacks by seat index.
    pub starting_stacks: Vec<Chips>,
}

/// Local-visible private cards for one seat.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HoleCards {
    /// Hand number in the current table session.
    pub hand: HandNumber,
    /// Backend used for this hand.
    pub backend: TableBackend,
    /// Seat whose cards are visible to this local host.
    pub seat: Seat,
    /// The two visible hole cards.
    pub cards: [Card; 2],
}

/// A table action prompt for a human-controlled seat.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActionRequest {
    /// Hand number in the current table session.
    pub hand: HandNumber,
    /// Backend used for this hand.
    pub backend: TableBackend,
    /// Seat that must act.
    pub actor: Seat,
    /// Current betting street.
    pub street: Street,
    /// Legal actions for `actor`.
    pub legal_actions: LegalActions,
    /// Public betting context for UI prompts and policy decisions.
    pub action_context: ActionContext,
}

/// One public betting action taken by a seat.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActionTaken {
    /// Hand number in the current table session.
    pub hand: HandNumber,
    /// Backend used for this hand.
    pub backend: TableBackend,
    /// Seat that acted.
    pub actor: Seat,
    /// Street on which the action was taken.
    pub street: Street,
    /// Action submitted to the rules or peer backend.
    pub action: BetAction,
    /// Legal-action metadata that was current before the action was applied.
    pub legal_actions: LegalActions,
    /// Pot size after the action was applied.
    pub pot_after: Chips,
    /// Actor's remaining stack after the action was applied.
    pub actor_stack_after: Chips,
}

/// Public board opening summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoardOpened {
    /// Hand number in the current table session.
    pub hand: HandNumber,
    /// Backend used for this hand.
    pub backend: TableBackend,
    /// Street that was opened.
    pub street: Street,
    /// Board cards visible after the opening.
    pub board: Vec<Card>,
}

/// Showdown table event with hand context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Showdown {
    /// Hand number in the current table session.
    pub hand: HandNumber,
    /// Backend used for this hand.
    pub backend: TableBackend,
    /// Opened hand summary.
    pub showdown: TableShowdown,
}

/// Showdown summary containing only hands that have actually opened.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TableShowdown {
    hands: Vec<ShowdownHand>,
}

impl TableShowdown {
    /// Creates a showdown summary from already-opened hands.
    ///
    /// Folded, mucked, or otherwise undisclosed hands must be omitted by the
    /// caller. This constructor evaluates only the supplied hole cards against
    /// the public board.
    ///
    /// # Errors
    ///
    /// Returns an error if an opened hand and board do not form a valid five to
    /// seven card evaluator input, or if the cards contain duplicates.
    pub fn from_opened(
        board: &[Card],
        opened: impl IntoIterator<Item = (Seat, [Card; 2])>,
    ) -> Result<Self> {
        let mut hands = Vec::new();
        for (seat, hole_cards) in opened {
            hands.push(ShowdownHand::opened(seat, hole_cards, board)?);
        }
        Ok(Self { hands })
    }

    /// Returns true when no hand was opened.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hands.is_empty()
    }

    /// Returns opened hand summaries in seat order supplied by the caller.
    #[must_use]
    pub fn hands(&self) -> &[ShowdownHand] {
        &self.hands
    }
}

/// One opened hand at showdown.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShowdownHand {
    /// Seat that opened the hand.
    pub seat: Seat,
    /// Opened hole cards.
    pub hole_cards: [Card; 2],
    /// Best five-card category derived from the opened hand and public board.
    pub category: HandCategory,
}

impl ShowdownHand {
    /// Evaluates one opened hand against the public board.
    ///
    /// # Errors
    ///
    /// Returns an error if the evaluator input is invalid or contains duplicate
    /// cards.
    pub fn opened(seat: Seat, hole_cards: [Card; 2], board: &[Card]) -> Result<Self> {
        let mut cards = Vec::with_capacity(hole_cards.len() + board.len());
        cards.extend_from_slice(&hole_cards);
        cards.extend_from_slice(board);
        let category = evaluate_best(&cards)?.category();
        Ok(Self {
            seat,
            hole_cards,
            category,
        })
    }
}

/// End-of-hand table state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandFinished {
    /// Completed hand number.
    pub hand: HandNumber,
    /// Backend used for this hand.
    pub backend: TableBackend,
    /// Final stacks by seat index.
    pub final_stacks: Vec<Chips>,
}

/// End-of-session summary for a local table runner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionFinished {
    /// Number of fully completed hands.
    pub completed_hands: u64,
    /// Final stacks by seat index.
    pub final_stacks: Vec<Chips>,
}

#[cfg(test)]
mod tests {
    use mtholdem::{Card, Rank, Seat, Suit};

    use super::TableShowdown;

    #[test]
    fn showdown_summary_evaluates_only_opened_hands() {
        let board = [
            Card::new(Rank::Ace, Suit::Hearts),
            Card::new(Rank::Four, Suit::Clubs),
            Card::new(Rank::Four, Suit::Spades),
            Card::new(Rank::Three, Suit::Hearts),
            Card::new(Rank::Six, Suit::Diamonds),
        ];
        let showdown = TableShowdown::from_opened(
            &board,
            [(
                Seat::new(2).expect("seat"),
                [
                    Card::new(Rank::Ten, Suit::Hearts),
                    Card::new(Rank::Ace, Suit::Spades),
                ],
            )],
        )
        .expect("showdown summary");

        assert_eq!(showdown.hands().len(), 1);
        assert_eq!(showdown.hands()[0].category.to_string(), "two pair");
    }
}
