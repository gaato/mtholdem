//! Host-side bot policy for `mtholdem` demos and local play.
//!
//! This crate is not part of the dealerless consensus protocol. It chooses legal
//! betting actions for hosts such as the CLI or a future single-player wrapper.
//! Protocol validation, transcript ordering, and settlement-facing state remain
//! in `mtholdem` and `mtholdem-peer`.

#![forbid(unsafe_code)]

use mtholdem::betting::{BetAction, LegalActions, Street};
use mtholdem::{Card, Chips, HandCategory, Rank, Seat};
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
            Self::Nit => BotProfile::new(0.18, 0.32, 0.20, 0.01, 0.06),
            Self::Balanced => BotProfile::new(0.34, 0.52, 0.34, 0.05, 0.10),
            Self::LoosePassive => BotProfile::new(0.56, 0.22, 0.68, 0.02, 0.12),
            Self::Lag => BotProfile::new(0.56, 0.74, 0.32, 0.11, 0.16),
            Self::Maniac => BotProfile::new(0.72, 0.90, 0.38, 0.22, 0.22),
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
    /// Number of raises already made on this street, if known.
    pub street_raise_count: Option<u8>,
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
            actor,
            street,
            legal_actions,
            hole_cards: None,
            board: Vec::new(),
            pot: None,
            actor_stack: None,
            actor_street_contributed: None,
            effective_stack: None,
            live_opponents: None,
            street_raise_count: None,
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

    /// Adds the number of raises already made on the current street.
    #[must_use]
    pub const fn with_street_raise_count(mut self, street_raise_count: u8) -> Self {
        self.street_raise_count = Some(street_raise_count);
        self
    }
}

/// Strength and pressure assessment used before choosing an action.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HandAssessment {
    /// Normalized hand strength estimate in `0.0..=1.0`.
    pub strength: f32,
    /// Made-hand category, when the board is complete enough to evaluate.
    pub category: Option<HandCategory>,
    /// Lightweight draw and overcard potential in `0.0..=1.0`.
    pub draw_potential: f32,
    /// Cost pressure from call amount, pot odds, and multiway context.
    pub pressure: f32,
    /// Total score used for continue and aggression thresholds.
    pub score: f32,
}

/// Action intent selected before sizing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionIntent {
    /// Fold to a bet.
    Fold,
    /// Check when no call is required.
    Check,
    /// Call an existing bet.
    Call,
    /// Open the betting on a street.
    Bet,
    /// Raise an existing bet.
    Raise,
    /// Move all-in.
    AllIn,
}

/// Chosen sizing family for a bet or raise intent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BetSizing {
    /// No bet size is needed.
    None,
    /// Use a fraction of the current pot.
    PotFraction(f32),
    /// Move all-in.
    AllIn,
}

/// Complete policy decision for diagnostics and tests.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BotDecision {
    /// Public hand assessment.
    pub assessment: HandAssessment,
    /// Chosen action intent before final legality mapping.
    pub intent: ActionIntent,
    /// Chosen sizing family.
    pub sizing: BetSizing,
    /// Final legal action.
    pub action: BetAction,
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
        self.decide(context, rng).action
    }

    /// Builds the assessment, intent, sizing, and final legal action.
    pub fn decide(&self, context: &BotDecisionContext, rng: &mut impl Rng) -> BotDecision {
        let assessment = self.assess(context, rng);
        let intent = self.intent(context, assessment);
        let sizing = sizing(assessment, intent);
        let action = map_to_legal_action(context, intent, sizing);
        BotDecision {
            assessment,
            intent,
            sizing,
            action,
        }
    }

    /// Assesses the public and local-private information available to the bot.
    pub fn assess(&self, context: &BotDecisionContext, rng: &mut impl Rng) -> HandAssessment {
        let (strength, category) = made_strength(context);
        let draw_potential = draw_potential(context);
        let pressure = decision_pressure(context);
        let noise = rng.gen_range(-0.5..0.5) * self.profile.randomness;
        let multiway_penalty = context.live_opponents.map_or_else(
            || {
                let opponents = context.player_count.saturating_sub(1);
                scaled_count_penalty(opponents, 0.025)
            },
            |opponents| scaled_count_penalty(opponents, 0.030),
        );
        let looseness = (self.profile.looseness - 0.35) * 0.20;
        let score =
            clamp01(strength + draw_potential + looseness + noise - pressure - multiway_penalty);
        HandAssessment {
            strength,
            category,
            draw_potential,
            pressure,
            score,
        }
    }

    fn intent(&self, context: &BotDecisionContext, assessment: HandAssessment) -> ActionIntent {
        if context.legal_actions.can_check() {
            if should_bet(self.profile, context, assessment) {
                if near_all_in(context, assessment) {
                    return ActionIntent::AllIn;
                }
                return ActionIntent::Bet;
            }
            return ActionIntent::Check;
        }

        if should_fold(self.profile, context, assessment) {
            return ActionIntent::Fold;
        }
        if should_raise(self.profile, context, assessment) {
            if near_all_in(context, assessment) {
                return ActionIntent::AllIn;
            }
            return ActionIntent::Raise;
        }
        ActionIntent::Call
    }
}

const fn sizing(assessment: HandAssessment, intent: ActionIntent) -> BetSizing {
    match intent {
        ActionIntent::Bet | ActionIntent::Raise => {
            if assessment.score >= 0.88 {
                BetSizing::PotFraction(0.75)
            } else if assessment.score >= 0.68 {
                BetSizing::PotFraction(0.50)
            } else {
                BetSizing::PotFraction(0.33)
            }
        }
        ActionIntent::AllIn => BetSizing::AllIn,
        ActionIntent::Fold | ActionIntent::Check | ActionIntent::Call => BetSizing::None,
    }
}

fn should_bet(
    profile: BotProfile,
    context: &BotDecisionContext,
    assessment: HandAssessment,
) -> bool {
    legal_raise_bounds(context.legal_actions).is_some()
        && assessment.score + profile.aggression.mul_add(0.20, profile.bluff_rate * 0.10) >= 0.72
}

fn should_fold(
    profile: BotProfile,
    context: &BotDecisionContext,
    assessment: HandAssessment,
) -> bool {
    let threshold = 0.47 - (profile.call_bias * 0.14) + call_cost_ratio(context) * 0.28;
    assessment.score < threshold
}

fn should_raise(
    profile: BotProfile,
    context: &BotDecisionContext,
    assessment: HandAssessment,
) -> bool {
    if legal_raise_bounds(context.legal_actions).is_none() {
        return false;
    }
    let raise_depth = context.street_raise_count.unwrap_or(0);
    let depth_penalty = f32::from(raise_depth) * 0.10;
    let call_ratio_penalty = call_cost_ratio(context) * 0.20;
    let threshold = 0.82 + depth_penalty + call_ratio_penalty - profile.aggression * 0.18;
    assessment.score >= threshold
        && (assessment.strength >= 0.72 || profile.bluff_rate > 0.18 && assessment.score >= 0.76)
}

fn near_all_in(context: &BotDecisionContext, assessment: HandAssessment) -> bool {
    let Some(stack) = context.actor_stack else {
        return false;
    };
    stack.units() <= 8 || assessment.score >= 0.94 && stack.units() <= 24
}

fn map_to_legal_action(
    context: &BotDecisionContext,
    intent: ActionIntent,
    sizing: BetSizing,
) -> BetAction {
    match intent {
        ActionIntent::Fold => BetAction::Fold,
        ActionIntent::Check => {
            if context.legal_actions.can_check() {
                BetAction::Check
            } else {
                BetAction::Fold
            }
        }
        ActionIntent::Call => BetAction::Call,
        ActionIntent::Bet | ActionIntent::Raise => {
            sized_raise(context, sizing).map_or(BetAction::Call, BetAction::RaiseTo)
        }
        ActionIntent::AllIn => {
            if context.legal_actions.maximum_raise_to() > Chips::ZERO {
                BetAction::AllIn
            } else if context.legal_actions.can_check() {
                BetAction::Check
            } else {
                BetAction::Call
            }
        }
    }
}

fn sized_raise(context: &BotDecisionContext, sizing: BetSizing) -> Option<Chips> {
    let (minimum, maximum) = legal_raise_bounds(context.legal_actions)?;
    let target = match sizing {
        BetSizing::PotFraction(fraction) => {
            let pot = context.pot.map_or_else(
                || {
                    minimum
                        .units()
                        .saturating_add(default_raise_span(minimum, maximum))
                },
                Chips::units,
            );
            let desired = minimum.units().saturating_add(scale_units(pot, fraction));
            Chips::from_units(desired)
        }
        BetSizing::AllIn => maximum,
        BetSizing::None => minimum,
    };
    Some(clamp_chips(target, minimum, maximum))
}

fn legal_raise_bounds(legal: LegalActions) -> Option<(Chips, Chips)> {
    let minimum = legal.minimum_raise_to()?;
    let maximum = legal.maximum_raise_to();
    (minimum <= maximum).then_some((minimum, maximum))
}

fn default_raise_span(minimum: Chips, maximum: Chips) -> u64 {
    let span = maximum.units().saturating_sub(minimum.units());
    span.clamp(2, 12)
}

fn scale_units(units: u64, fraction: f32) -> u64 {
    let fraction = clamp01(fraction);
    let scaled = if fraction <= 0.34 {
        units.saturating_add(2) / 3
    } else if fraction <= 0.51 {
        units.saturating_add(1) / 2
    } else {
        units.saturating_mul(3).saturating_add(2) / 4
    };
    scaled.max(1)
}

fn clamp_chips(value: Chips, minimum: Chips, maximum: Chips) -> Chips {
    Chips::from_units(value.units().clamp(minimum.units(), maximum.units()))
}

fn made_strength(context: &BotDecisionContext) -> (f32, Option<HandCategory>) {
    if context.street == Street::Preflop || context.board.len() < 3 {
        return (context.hole_cards.map_or(0.45, preflop_score), None);
    }
    let Some(hole_cards) = context.hole_cards else {
        return (0.45, None);
    };
    let mut cards = Vec::with_capacity(hole_cards.len() + context.board.len());
    cards.extend_from_slice(&hole_cards);
    cards.extend_from_slice(&context.board);
    if let Ok(value) = mtholdem::card::evaluate_best(&cards) {
        let category = value.category();
        return (category_strength(category, context.street), Some(category));
    }
    (0.45, None)
}

fn category_strength(category: HandCategory, street: Street) -> f32 {
    match category {
        HandCategory::HighCard => {
            if street == Street::River {
                0.22
            } else {
                0.34
            }
        }
        HandCategory::OnePair => {
            if street == Street::River {
                0.46
            } else {
                0.52
            }
        }
        HandCategory::TwoPair => 0.70,
        HandCategory::ThreeOfAKind => 0.80,
        HandCategory::Straight | HandCategory::Flush => 0.88,
        HandCategory::FullHouse => 0.94,
        HandCategory::FourOfAKind | HandCategory::StraightFlush => 1.0,
    }
}

fn draw_potential(context: &BotDecisionContext) -> f32 {
    if matches!(context.street, Street::Preflop | Street::River) || context.board.len() < 3 {
        return 0.0;
    }
    let Some(hole_cards) = context.hole_cards else {
        return 0.0;
    };
    let mut cards = Vec::with_capacity(hole_cards.len() + context.board.len());
    cards.extend_from_slice(&hole_cards);
    cards.extend_from_slice(&context.board);
    let suited = suit_count_potential(&cards);
    let straight = straight_potential(&cards);
    let overcards = overcard_potential(hole_cards, &context.board);
    (suited + straight + overcards).min(0.18)
}

fn suit_count_potential(cards: &[Card]) -> f32 {
    let mut counts = [0_u8; 4];
    for card in cards {
        counts[card.suit() as usize] += 1;
    }
    match counts.into_iter().max().unwrap_or(0) {
        4 => 0.10,
        5.. => 0.14,
        _ => 0.0,
    }
}

fn straight_potential(cards: &[Card]) -> f32 {
    let mut ranks = [false; 15];
    for card in cards {
        let value = usize::from(card.rank().value());
        ranks[value] = true;
        if value == 14 {
            ranks[1] = true;
        }
    }
    let best_window = (1..=10)
        .map(|start| (start..start + 5).filter(|rank| ranks[*rank]).count())
        .max()
        .unwrap_or(0);
    if best_window >= 4 { 0.08 } else { 0.0 }
}

fn overcard_potential(hole_cards: [Card; 2], board: &[Card]) -> f32 {
    let Some(board_high) = board.iter().map(|card| card.rank().value()).max() else {
        return 0.0;
    };
    let overcards = hole_cards
        .into_iter()
        .filter(|card| card.rank().value() > board_high)
        .count();
    if overcards == 2 { 0.04 } else { 0.0 }
}

fn decision_pressure(context: &BotDecisionContext) -> f32 {
    let mut pressure = call_cost_ratio(context) * 0.45;
    if context.legal_actions.to_call().units() >= 20 {
        pressure += 0.08;
    }
    pressure
}

fn call_cost_ratio(context: &BotDecisionContext) -> f32 {
    let to_call = context.legal_actions.to_call().units();
    if to_call == 0 {
        return 0.0;
    }
    if let Some(pot) = context.pot {
        let denominator = pot.units().saturating_add(to_call).max(1);
        return capped_ratio(to_call, denominator);
    }
    let denominator = context.actor_stack.map_or(100, Chips::units).max(1);
    capped_ratio(to_call, denominator)
}

fn capped_ratio(numerator: u64, denominator: u64) -> f32 {
    let numerator = u16::try_from(numerator.min(10_000)).expect("ratio numerator is capped");
    let denominator = u16::try_from(denominator.min(10_000)).expect("ratio denominator is capped");
    f32::from(numerator) / f32::from(denominator.max(1))
}

fn scaled_count_penalty(count: usize, unit: f32) -> f32 {
    let count = u8::try_from(count.min(9)).expect("count is capped below ten");
    f32::from(count.saturating_sub(1)) * unit
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
            13 => 0.96,
            12 => 0.91,
            11 => 0.82,
            10 | 9 => 0.72,
            8 | 7 => 0.62,
            2..=6 => 0.50,
            _ => 0.45,
        };
    }
    let mut score = (f32::from(high) / 14.0) * 0.42 + (f32::from(low) / 14.0) * 0.18;
    if high == 14 {
        score += 0.12;
    }
    if broadway(high) && broadway(low) {
        score += 0.16;
    }
    if suited {
        score += 0.06;
    }
    if high - low <= 2 {
        score += 0.05;
    }
    score.min(0.88)
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

    fn preflop_legal() -> LegalActions {
        let stacks = [Chips::from_units(100), Chips::from_units(100)];
        let hand = mtholdem::holdem::HoldemHand::new(
            &stacks,
            Seat::new(0).expect("seat"),
            Chips::from_units(1),
            Chips::from_units(2),
            Chips::from_units(100),
        )
        .expect("hand");
        hand.legal_actions().expect("preflop legal")
    }

    fn open_bet_legal() -> LegalActions {
        let stacks = [Chips::from_units(100), Chips::from_units(100)];
        let mut hand = mtholdem::holdem::HoldemHand::new(
            &stacks,
            Seat::new(0).expect("seat"),
            Chips::from_units(1),
            Chips::from_units(2),
            Chips::from_units(100),
        )
        .expect("hand");
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
        hand.legal_actions().expect("postflop legal")
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
            open_bet_legal(),
        )
        .with_pot(Chips::from_units(4));
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
            preflop_legal(),
        )
        .with_hole_cards(Some([
            Card::new(Rank::Seven, mtholdem::Suit::Clubs),
            Card::new(Rank::Two, mtholdem::Suit::Diamonds),
        ]))
        .with_pot(Chips::from_units(3))
        .with_actor_stack(Chips::from_units(99));
        let mut rng = StdRng::seed_from_u64(1);
        assert_eq!(bot.choose_action(&context, &mut rng), BetAction::Fold);
    }

    #[test]
    fn strong_open_bets_use_more_than_the_minimum() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = BotDecisionContext::new(
            2,
            Seat::new(0).expect("seat"),
            Street::Flop,
            open_bet_legal(),
        )
        .with_hole_cards(Some([
            Card::new(Rank::Ace, mtholdem::Suit::Clubs),
            Card::new(Rank::Ace, mtholdem::Suit::Diamonds),
        ]))
        .with_board([
            Card::new(Rank::Ace, mtholdem::Suit::Hearts),
            Card::new(Rank::Seven, mtholdem::Suit::Clubs),
            Card::new(Rank::Two, mtholdem::Suit::Spades),
        ])
        .with_pot(Chips::from_units(4));
        let mut rng = StdRng::seed_from_u64(1);
        let action = bot.choose_action(&context, &mut rng);
        assert!(matches!(action, BetAction::RaiseTo(amount) if amount.units() > 2));
    }
}
