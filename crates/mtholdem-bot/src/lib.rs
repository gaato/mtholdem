//! Lightweight host-side bot policy for `mtholdem` demos.
//!
//! This crate is not part of the dealerless consensus protocol. It chooses legal
//! betting actions for demo hosts such as the CLI or a future single-player game
//! wrapper. Protocol validation, transcript ordering, and settlement-facing state
//! remain in `mtholdem` and `mtholdem-peer`.

#![forbid(unsafe_code)]

use mtholdem::betting::{BetAction, LegalActions, Street};
use mtholdem::{Card, Chips, Rank, Seat};
use rand::Rng;

/// A configurable lightweight bot tendency profile.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BotProfile {
    /// Wider values enter more pots.
    pub looseness: f32,
    /// Higher values prefer raises and bets.
    pub aggression: f32,
    /// Higher values prefer calls over folds or raises.
    pub call_bias: f32,
    /// Chance-like weight for betting weak holdings.
    pub bluff_rate: f32,
    /// Amount of random action-score jitter.
    pub randomness: f32,
}

impl BotProfile {
    /// Creates a profile from raw tendency values.
    #[must_use]
    pub const fn new(
        looseness: f32,
        aggression: f32,
        call_bias: f32,
        bluff_rate: f32,
        randomness: f32,
    ) -> Self {
        Self {
            looseness,
            aggression,
            call_bias,
            bluff_rate,
            randomness,
        }
    }

    /// Returns this profile with all values clamped to `0.0..=1.0`.
    #[must_use]
    pub fn clamped(self) -> Self {
        Self {
            looseness: clamp01(self.looseness),
            aggression: clamp01(self.aggression),
            call_bias: clamp01(self.call_bias),
            bluff_rate: clamp01(self.bluff_rate),
            randomness: clamp01(self.randomness),
        }
    }
}

impl Default for BotProfile {
    fn default() -> Self {
        BotPreset::Balanced.profile()
    }
}

/// Named bot tendency presets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BotPreset {
    /// Very tight and not eager to fight for marginal pots.
    Nit,
    /// Moderate ranges and aggression.
    Balanced,
    /// Enters too many pots but rarely raises.
    LoosePassive,
    /// Loose and aggressive.
    Lag,
    /// Very loose, volatile, and bluff-heavy.
    Maniac,
}

impl BotPreset {
    /// Returns the profile for this preset.
    #[must_use]
    pub const fn profile(self) -> BotProfile {
        match self {
            Self::Nit => BotProfile::new(0.20, 0.35, 0.20, 0.02, 0.08),
            Self::Balanced => BotProfile::new(0.35, 0.55, 0.35, 0.06, 0.12),
            Self::LoosePassive => BotProfile::new(0.55, 0.25, 0.65, 0.03, 0.15),
            Self::Lag => BotProfile::new(0.55, 0.75, 0.35, 0.12, 0.18),
            Self::Maniac => BotProfile::new(0.75, 0.90, 0.45, 0.25, 0.25),
        }
    }
}

/// Public context available to a host-side bot when choosing one action.
#[derive(Clone, Debug)]
pub struct BotDecisionContext {
    /// Number of dealt players in the current hand.
    pub player_count: usize,
    /// Acting seat.
    pub actor: Seat,
    /// Current betting street.
    pub street: Street,
    /// Legal actions and chip bounds for the actor.
    pub legal_actions: LegalActions,
    /// Actor's private cards, if the host knows them.
    pub hole_cards: Option<[Card; 2]>,
    /// Public community cards visible to all players.
    pub board: Vec<Card>,
}

impl BotDecisionContext {
    /// Creates a context with no known private cards or board cards.
    #[must_use]
    pub fn new(
        player_count: usize,
        actor: Seat,
        street: Street,
        legal_actions: LegalActions,
    ) -> Self {
        Self {
            player_count,
            actor,
            street,
            legal_actions,
            hole_cards: None,
            board: Vec::new(),
        }
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
}

/// Stateless bot strategy driven by a [`BotProfile`].
#[derive(Clone, Copy, Debug)]
pub struct HeuristicBot {
    profile: BotProfile,
}

impl HeuristicBot {
    /// Creates a bot from a profile.
    #[must_use]
    pub fn new(profile: BotProfile) -> Self {
        Self {
            profile: profile.clamped(),
        }
    }

    /// Creates a bot from a named preset.
    #[must_use]
    pub fn from_preset(preset: BotPreset) -> Self {
        Self::new(preset.profile())
    }

    /// Returns this bot's profile.
    #[must_use]
    pub const fn profile(self) -> BotProfile {
        self.profile
    }

    /// Chooses one legal betting action from the supplied public context.
    pub fn choose_action(&self, context: &BotDecisionContext, rng: &mut impl Rng) -> BetAction {
        let legal = context.legal_actions;
        if legal.can_check() {
            return self.choose_when_check_allowed(context, rng);
        }
        self.choose_when_facing_bet(context, rng)
    }

    fn choose_when_check_allowed(
        &self,
        context: &BotDecisionContext,
        rng: &mut impl Rng,
    ) -> BetAction {
        let score = self.action_score(context, rng);
        let raise_threshold = 1.35 - (self.profile.aggression * 0.15);
        if legal_raise_target(context.legal_actions).is_some()
            && (score + self.profile.aggression + self.profile.bluff_rate) >= raise_threshold
        {
            return BetAction::RaiseTo(legal_raise_target(context.legal_actions).expect("checked"));
        }
        BetAction::Check
    }

    fn choose_when_facing_bet(
        &self,
        context: &BotDecisionContext,
        rng: &mut impl Rng,
    ) -> BetAction {
        let score = self.action_score(context, rng);
        let call_threshold = 0.68 - (self.profile.call_bias * 0.25);
        if score < call_threshold {
            return BetAction::Fold;
        }
        let raise_threshold =
            1.45 + (self.profile.call_bias * 0.20) - (self.profile.aggression * 0.10);
        if let Some(target) = legal_raise_target(context.legal_actions) {
            if (score + self.profile.aggression - self.profile.call_bias)
                >= raise_threshold + call_pressure(context.legal_actions)
            {
                return BetAction::RaiseTo(target);
            }
        }
        BetAction::Call
    }

    fn action_score(&self, context: &BotDecisionContext, rng: &mut impl Rng) -> f32 {
        let base = match context.street {
            Street::Preflop => {
                context.hole_cards.map_or(0.45, preflop_score)
                    + short_handed_bonus(context.player_count)
            }
            Street::Flop | Street::Turn | Street::River => {
                postflop_score(context.hole_cards, &context.board)
            }
        };
        base + self.profile.looseness + (rng.gen_range(-0.5..0.5) * self.profile.randomness)
            - call_pressure(context.legal_actions)
    }
}

fn legal_raise_target(legal: LegalActions) -> Option<Chips> {
    legal
        .minimum_raise_to()
        .filter(|minimum| *minimum <= legal.maximum_raise_to())
}

fn call_pressure(legal: LegalActions) -> f32 {
    if legal.to_call().is_zero() {
        0.0
    } else {
        let capped_units =
            u16::try_from(legal.to_call().units().min(35)).expect("capped call units fit in u16");
        f32::from(capped_units) / 100.0
    }
}

fn short_handed_bonus(player_count: usize) -> f32 {
    let missing = 9_usize.saturating_sub(player_count.min(9));
    let missing = u8::try_from(missing).expect("missing seats are capped below nine");
    f32::from(missing) * 0.04
}

fn preflop_score(cards: [Card; 2]) -> f32 {
    let first = cards[0].rank().value();
    let second = cards[1].rank().value();
    let high = first.max(second);
    let low = first.min(second);
    let suited = cards[0].suit() == cards[1].suit();
    if high == low {
        return match high {
            14 => 1.00,
            13 => 0.97,
            12 => 0.94,
            11 => 0.86,
            10 | 9 => 0.78,
            8 | 7 => 0.68,
            2..=6 => 0.58,
            _ => 0.50,
        };
    }
    let mut score = (f32::from(high) / 14.0) * 0.45 + (f32::from(low) / 14.0) * 0.20;
    if high == 14 {
        score += 0.15;
    }
    if broadway(high) && broadway(low) {
        score += 0.18;
    }
    if suited {
        score += 0.08;
    }
    if high - low <= 2 {
        score += 0.07;
    }
    score.min(0.92)
}

fn postflop_score(hole_cards: Option<[Card; 2]>, board: &[Card]) -> f32 {
    let Some(hole_cards) = hole_cards else {
        return 0.45;
    };
    if board.len() < 3 {
        return preflop_score(hole_cards);
    }
    let mut cards = Vec::with_capacity(hole_cards.len() + board.len());
    cards.extend_from_slice(&hole_cards);
    cards.extend_from_slice(board);
    if cards.len() >= 5 {
        if let Ok(value) = mtholdem::card::evaluate_best(&cards) {
            return match value.category() {
                mtholdem::HandCategory::HighCard => 0.35,
                mtholdem::HandCategory::OnePair => 0.52,
                mtholdem::HandCategory::TwoPair => 0.68,
                mtholdem::HandCategory::ThreeOfAKind => 0.78,
                mtholdem::HandCategory::Straight | mtholdem::HandCategory::Flush => 0.86,
                mtholdem::HandCategory::FullHouse => 0.93,
                mtholdem::HandCategory::FourOfAKind | mtholdem::HandCategory::StraightFlush => 1.0,
            };
        }
    }
    0.45
}

const fn broadway(rank: u8) -> bool {
    rank >= Rank::Ten.value()
}

fn clamp01(value: f32) -> f32 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn legal(to_call: u64, minimum_raise_to: Option<u64>, maximum_raise_to: u64) -> LegalActions {
        // Exercise the public betting API to obtain a real LegalActions value.
        let stacks = [Chips::from_units(100), Chips::from_units(100)];
        let mut hand = mtholdem::holdem::HoldemHand::new(
            &stacks,
            Seat::new(0).expect("seat"),
            Chips::from_units(1),
            Chips::from_units(2),
            Chips::from_units(100),
        )
        .expect("hand");
        if to_call == 0 {
            hand.apply_action(Seat::new(0).expect("seat"), BetAction::Call)
                .expect("call");
            hand.apply_action(Seat::new(1).expect("seat"), BetAction::Check)
                .expect("check");
            hand.open_flop([
                Card::new(Rank::Two, mtholdem::Suit::Clubs),
                Card::new(Rank::Seven, mtholdem::Suit::Diamonds),
                Card::new(Rank::Nine, mtholdem::Suit::Hearts),
            ])
            .expect("open flop");
            return hand.legal_actions().expect("postflop legal");
        }
        let legal = hand.legal_actions().expect("preflop legal");
        assert_eq!(legal.to_call().units(), to_call);
        assert_eq!(legal.minimum_raise_to().map(Chips::units), minimum_raise_to);
        assert_eq!(legal.maximum_raise_to().units(), maximum_raise_to);
        legal
    }

    #[test]
    fn presets_are_clamped_profiles() {
        for preset in [
            BotPreset::Nit,
            BotPreset::Balanced,
            BotPreset::LoosePassive,
            BotPreset::Lag,
            BotPreset::Maniac,
        ] {
            let profile = preset.profile().clamped();
            assert!((0.0..=1.0).contains(&profile.looseness));
            assert!((0.0..=1.0).contains(&profile.aggression));
            assert!((0.0..=1.0).contains(&profile.call_bias));
            assert!((0.0..=1.0).contains(&profile.bluff_rate));
            assert!((0.0..=1.0).contains(&profile.randomness));
        }
    }

    #[test]
    fn bot_never_calls_when_check_is_legal() {
        let bot = HeuristicBot::from_preset(BotPreset::Maniac);
        let context = BotDecisionContext::new(
            2,
            Seat::new(0).expect("seat"),
            Street::Flop,
            legal(0, Some(2), 100),
        );
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..100 {
            assert!(!matches!(
                bot.choose_action(&context, &mut rng),
                BetAction::Call
            ));
        }
    }

    #[test]
    fn tight_bot_folds_weak_hands_against_a_bet() {
        let bot = HeuristicBot::from_preset(BotPreset::Nit);
        let context = BotDecisionContext::new(
            9,
            Seat::new(0).expect("seat"),
            Street::Preflop,
            legal(1, Some(4), 100),
        )
        .with_hole_cards(Some([
            Card::new(Rank::Seven, mtholdem::Suit::Clubs),
            Card::new(Rank::Two, mtholdem::Suit::Diamonds),
        ]));
        let mut rng = StdRng::seed_from_u64(1);
        assert_eq!(bot.choose_action(&context, &mut rng), BetAction::Fold);
    }
}
