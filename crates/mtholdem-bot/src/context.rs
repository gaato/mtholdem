use mtholdem::betting::{ActionContext, LegalActions, Street};
use mtholdem::{Card, Chips, Seat};

/// Public context available to a host-side bot when choosing one action.
#[derive(Clone, Debug)]
pub struct BotDecisionContext {
    /// Number of seats at the table.
    pub player_count: usize,
    /// Number of seats dealt into the current hand, if known.
    pub dealt_player_count: Option<usize>,
    /// Acting seat.
    pub actor: Seat,
    /// Dealer button seat, if the host exposes it.
    pub button: Option<Seat>,
    /// Current betting street.
    pub street: Street,
    /// Legal actions and chip bounds for the actor.
    pub legal_actions: LegalActions,
    /// Actor's private cards, if the host knows them.
    pub hole_cards: Option<[Card; 2]>,
    /// Public community cards visible to all players.
    pub board: Vec<Card>,
    /// Current pot size, if the host can expose it without leaking hidden data.
    pub pot: Option<Chips>,
    /// Actor's remaining stack before acting, if known.
    pub actor_stack: Option<Chips>,
    /// Actor's contribution on this street, if known.
    pub actor_street_contributed: Option<Chips>,
    /// Smallest remaining live opponent stack, if known.
    pub effective_stack: Option<Chips>,
    /// Number of live opponents who can still contest the pot.
    pub live_opponents: Option<usize>,
    /// Number of opponents whose current street contribution has matched the bet.
    pub matched_bet_opponents: Option<usize>,
    /// Configured big blind.
    pub big_blind: Option<Chips>,
    /// Current highest required street contribution.
    pub current_bet: Option<Chips>,
    /// Number of raises already made on this street, if known.
    pub street_raise_count: Option<u8>,
    /// Whether the actor was the last preflop aggressor and can make a continuation bet.
    pub postflop_initiative: Option<bool>,
}

impl BotDecisionContext {
    /// Creates a context with no optional private, board, pot, stack, or action-depth facts.
    #[must_use]
    pub fn new(
        player_count: usize,
        actor: Seat,
        street: Street,
        legal_actions: LegalActions,
    ) -> Self {
        Self {
            player_count,
            dealt_player_count: None,
            actor,
            button: None,
            street,
            legal_actions,
            hole_cards: None,
            board: Vec::new(),
            pot: None,
            actor_stack: None,
            actor_street_contributed: None,
            effective_stack: None,
            live_opponents: None,
            matched_bet_opponents: None,
            big_blind: None,
            current_bet: None,
            street_raise_count: None,
            postflop_initiative: None,
        }
    }

    /// Adds the number of seats dealt into the current hand.
    #[must_use]
    pub const fn with_dealt_player_count(mut self, dealt_player_count: usize) -> Self {
        self.dealt_player_count = Some(dealt_player_count);
        self
    }

    /// Adds known hole cards.
    #[must_use]
    pub const fn with_hole_cards(mut self, hole_cards: Option<[Card; 2]>) -> Self {
        self.hole_cards = hole_cards;
        self
    }

    /// Adds public board cards.
    #[must_use]
    pub fn with_board(mut self, board: impl Into<Vec<Card>>) -> Self {
        self.board = board.into();
        self
    }

    /// Adds the dealer button seat.
    #[must_use]
    pub const fn with_button(mut self, button: Seat) -> Self {
        self.button = Some(button);
        self
    }

    /// Adds the current pot size.
    #[must_use]
    pub const fn with_pot(mut self, pot: Chips) -> Self {
        self.pot = Some(pot);
        self
    }

    /// Adds the actor's remaining stack before acting.
    #[must_use]
    pub const fn with_actor_stack(mut self, stack: Chips) -> Self {
        self.actor_stack = Some(stack);
        self
    }

    /// Adds the actor's contribution on the current street.
    #[must_use]
    pub const fn with_actor_street_contributed(mut self, contributed: Chips) -> Self {
        self.actor_street_contributed = Some(contributed);
        self
    }

    /// Adds the smallest remaining live opponent stack.
    #[must_use]
    pub const fn with_effective_stack(mut self, effective_stack: Chips) -> Self {
        self.effective_stack = Some(effective_stack);
        self
    }

    /// Adds the number of live opponents.
    #[must_use]
    pub const fn with_live_opponents(mut self, live_opponents: usize) -> Self {
        self.live_opponents = Some(live_opponents);
        self
    }

    /// Adds the number of opponents whose current contribution has matched the bet.
    #[must_use]
    pub const fn with_matched_bet_opponents(mut self, matched_bet_opponents: usize) -> Self {
        self.matched_bet_opponents = Some(matched_bet_opponents);
        self
    }

    /// Adds the configured big blind.
    #[must_use]
    pub const fn with_big_blind(mut self, big_blind: Chips) -> Self {
        self.big_blind = Some(big_blind);
        self
    }

    /// Adds the current highest required street contribution.
    #[must_use]
    pub const fn with_current_bet(mut self, current_bet: Chips) -> Self {
        self.current_bet = Some(current_bet);
        self
    }

    /// Adds all public action-context facts produced by the rules engine.
    #[must_use]
    pub fn with_action_context(mut self, action_context: ActionContext) -> Self {
        self.pot = Some(action_context.pot());
        self.button = Some(action_context.button());
        self.actor_stack = Some(action_context.actor_stack());
        self.actor_street_contributed = Some(action_context.actor_street_contributed());
        self.effective_stack = action_context.effective_stack();
        self.live_opponents = Some(action_context.live_opponents());
        self.matched_bet_opponents = Some(action_context.matched_bet_opponents());
        self.big_blind = Some(action_context.big_blind());
        self.current_bet = Some(action_context.current_bet());
        self.street_raise_count = Some(action_context.street_raise_count());
        self
    }

    /// Adds the number of raises already made on the current street.
    #[must_use]
    pub const fn with_street_raise_count(mut self, street_raise_count: u8) -> Self {
        self.street_raise_count = Some(street_raise_count);
        self
    }

    /// Adds whether the actor has postflop betting initiative.
    #[must_use]
    pub const fn with_postflop_initiative(mut self, has_initiative: bool) -> Self {
        self.postflop_initiative = Some(has_initiative);
        self
    }
}
