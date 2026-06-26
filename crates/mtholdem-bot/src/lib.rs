//! Host-side bot policy for `mtholdem` demos and local play.
//!
//! This crate is not part of the dealerless consensus protocol. It chooses legal
//! betting actions for hosts such as the CLI or a future single-player wrapper.
//! Protocol validation, transcript ordering, and settlement-facing state remain
//! in `mtholdem` and `mtholdem-peer`.

#![forbid(unsafe_code)]

use mtholdem::betting::{ActionContext, BetAction, LegalActions, Street};
use mtholdem::{Card, Chips, HandCategory, Rank, Seat};
use rand::Rng;
use rand::seq::SliceRandom;

const MONTE_CARLO_SAMPLES: usize = 96;

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
    /// Range-aware Monte Carlo equity estimate, when enough local/public cards are known.
    pub equity: Option<f32>,
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
        let intent = self
            .preflop_intent(context)
            .unwrap_or_else(|| self.intent(context, assessment));
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
        let (made_strength, category) = made_strength(context);
        let equity = monte_carlo_equity(context, self.profile, rng);
        let strength = equity.map_or(made_strength, |equity| {
            clamp01(made_strength.mul_add(0.65, equity * 0.35))
        });
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
            equity,
            pressure,
            score,
        }
    }

    fn preflop_intent(&self, context: &BotDecisionContext) -> Option<ActionIntent> {
        if context.street != Street::Preflop {
            return None;
        }
        let score = context.hole_cards.map(preflop_score)?;
        let spot = preflop_spot(context)?;
        let position = position_openness(context);
        let short_handed = short_handed_openness(context.player_count);
        match spot {
            PreflopSpot::FirstIn => {
                if score >= open_threshold(self.profile, position, short_handed) {
                    Some(ActionIntent::Raise)
                } else {
                    Some(ActionIntent::Fold)
                }
            }
            PreflopSpot::LimpedPot => {
                let threshold = open_threshold(self.profile, position, short_handed);
                if score >= threshold + 0.03 {
                    Some(ActionIntent::Raise)
                } else if score >= threshold - 0.16 {
                    Some(ActionIntent::Call)
                } else {
                    Some(ActionIntent::Fold)
                }
            }
            PreflopSpot::BigBlindOption => {
                let limpers = context.matched_bet_opponents.unwrap_or(0).saturating_sub(1);
                if limpers > 0
                    && score
                        >= 0.62 - self.profile.aggression * 0.10 - self.profile.looseness * 0.06
                {
                    Some(ActionIntent::Raise)
                } else {
                    Some(ActionIntent::Check)
                }
            }
            PreflopSpot::FacingRaise => {
                let call_threshold = defend_threshold(self.profile, context, short_handed);
                let raise_threshold = three_bet_threshold(self.profile, context);
                if score >= raise_threshold {
                    Some(ActionIntent::Raise)
                } else if score >= call_threshold {
                    Some(ActionIntent::Call)
                } else {
                    Some(ActionIntent::Fold)
                }
            }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreflopSpot {
    FirstIn,
    LimpedPot,
    BigBlindOption,
    FacingRaise,
}

fn preflop_spot(context: &BotDecisionContext) -> Option<PreflopSpot> {
    let big_blind = context.big_blind?;
    let current_bet = context.current_bet?;
    if current_bet == big_blind {
        if context.legal_actions.can_check() {
            return Some(PreflopSpot::BigBlindOption);
        }
        if context.matched_bet_opponents.unwrap_or(1) > 1 {
            return Some(PreflopSpot::LimpedPot);
        }
        return Some(PreflopSpot::FirstIn);
    }
    if current_bet > big_blind && context.legal_actions.to_call() > Chips::ZERO {
        return Some(PreflopSpot::FacingRaise);
    }
    None
}

fn open_threshold(profile: BotProfile, position: f32, short_handed: f32) -> f32 {
    (0.74 - profile.looseness * 0.14 - position * 0.18 - short_handed * 0.12).clamp(0.36, 0.80)
}

fn defend_threshold(profile: BotProfile, context: &BotDecisionContext, short_handed: f32) -> f32 {
    let posted_blind = context
        .actor_street_contributed
        .is_some_and(|contributed| contributed > Chips::ZERO);
    let blind_discount = if posted_blind { 0.06 } else { 0.0 };
    let price_discount = call_cost_ratio(context).min(0.22) * 0.22;
    let full_ring_penalty = scaled_count_penalty(context.player_count, 0.006).min(0.04);
    let multiway_penalty = context.matched_bet_opponents.map_or(0.0, |matched| {
        scaled_count_penalty(matched, 0.020).min(0.08)
    });
    let cold_call_penalty = if posted_blind { 0.0 } else { 0.06 };
    (0.62 - profile.call_bias * 0.12 - blind_discount - short_handed * 0.08 - price_discount
        + raise_size_pressure(context)
        + full_ring_penalty
        + multiway_penalty
        + cold_call_penalty)
        .clamp(0.34, 0.86)
}

fn three_bet_threshold(profile: BotProfile, context: &BotDecisionContext) -> f32 {
    let depth_penalty = f32::from(context.street_raise_count.unwrap_or(1).saturating_sub(1)) * 0.07;
    (0.88 - profile.aggression * 0.08 + depth_penalty + raise_size_pressure(context))
        .clamp(0.72, 0.99)
}

fn raise_size_pressure(context: &BotDecisionContext) -> f32 {
    let Some(big_blind) = context.big_blind else {
        return 0.0;
    };
    let Some(current_bet) = context.current_bet else {
        return 0.0;
    };
    let big_blind = big_blind.units().max(1);
    let current_bet = current_bet.units();
    if current_bet >= big_blind.saturating_mul(12) {
        0.20
    } else if current_bet >= big_blind.saturating_mul(6) {
        0.12
    } else if current_bet >= big_blind.saturating_mul(3) {
        0.05
    } else {
        0.0
    }
}

fn position_openness(context: &BotDecisionContext) -> f32 {
    let Some(button) = context.button else {
        return 0.5;
    };
    let seats = context.player_count.max(2);
    let distance_to_button = (button.index() + seats - context.actor.index()) % seats;
    let denominator = seats.saturating_sub(1).max(1);
    1.0 - capped_ratio(distance_to_button as u64, denominator as u64)
}

fn short_handed_openness(player_count: usize) -> f32 {
    let missing = 9_usize.saturating_sub(player_count);
    capped_ratio(missing as u64, 7)
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
        && assessment.score + profile.aggression.mul_add(0.20, profile.bluff_rate * 0.10) >= 0.63
}

fn should_fold(
    profile: BotProfile,
    context: &BotDecisionContext,
    assessment: HandAssessment,
) -> bool {
    let threshold = 0.38 - (profile.call_bias * 0.14) + call_cost_ratio(context) * 0.18;
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
        BetSizing::PotFraction(fraction) => preflop_open_or_isolation_target(context)
            .unwrap_or_else(|| {
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
            }),
        BetSizing::AllIn => maximum,
        BetSizing::None => minimum,
    };
    Some(clamp_chips(target, minimum, maximum))
}

fn preflop_open_or_isolation_target(context: &BotDecisionContext) -> Option<Chips> {
    if context.street != Street::Preflop || !context.legal_actions.can_check() {
        return None;
    }
    let big_blind = context.big_blind?.units();
    if big_blind == 0 {
        return None;
    }
    let matched = context.matched_bet_opponents.unwrap_or(0) as u64;
    let target_blinds = 3_u64.saturating_add(matched);
    Some(Chips::from_units(big_blind.saturating_mul(target_blinds)))
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

fn monte_carlo_equity(
    context: &BotDecisionContext,
    profile: BotProfile,
    rng: &mut impl Rng,
) -> Option<f32> {
    let hole_cards = context.hole_cards?;
    if context.board.len() > 5 {
        return None;
    }
    let opponent_count = context
        .live_opponents
        .unwrap_or_else(|| context.player_count.saturating_sub(1))
        .min(7);
    if opponent_count == 0 {
        return Some(1.0);
    }

    let known_cards = known_cards(hole_cards, &context.board)?;
    let mut total_share = 0.0;
    let mut completed = 0_usize;
    for _ in 0..MONTE_CARLO_SAMPLES {
        let Some(share) = simulate_equity_once(context, profile, &known_cards, opponent_count, rng)
        else {
            continue;
        };
        total_share += share;
        completed += 1;
    }
    (completed > 0).then(|| {
        let completed = u16::try_from(completed).expect("sample count fits u16");
        total_share / f32::from(completed)
    })
}

fn known_cards(hole_cards: [Card; 2], board: &[Card]) -> Option<Vec<Card>> {
    let mut known = Vec::with_capacity(hole_cards.len() + board.len());
    known.extend_from_slice(&hole_cards);
    known.extend_from_slice(board);
    unique_cards(&known).then_some(known)
}

fn simulate_equity_once(
    context: &BotDecisionContext,
    profile: BotProfile,
    known_cards: &[Card],
    opponent_count: usize,
    rng: &mut impl Rng,
) -> Option<f32> {
    let hole_cards = context.hole_cards?;
    let mut deck = remaining_deck(known_cards);
    deck.shuffle(rng);
    let mut opponents = Vec::with_capacity(opponent_count);
    for _ in 0..opponent_count {
        opponents.push(draw_opponent_hand(&mut deck, context, profile, rng)?);
    }
    let mut board = context.board.clone();
    while board.len() < 5 {
        board.push(deck.pop()?);
    }

    let hero_value = evaluate_holdem(hole_cards, &board)?;
    let mut tied = 0_u8;
    for opponent in opponents {
        let opponent_value = evaluate_holdem(opponent, &board)?;
        if opponent_value > hero_value {
            return Some(0.0);
        }
        if opponent_value == hero_value {
            tied = tied.saturating_add(1);
        }
    }
    Some(1.0 / f32::from(tied.saturating_add(1)))
}

fn draw_opponent_hand(
    deck: &mut Vec<Card>,
    context: &BotDecisionContext,
    profile: BotProfile,
    rng: &mut impl Rng,
) -> Option<[Card; 2]> {
    let threshold = opponent_range_threshold(context, profile);
    for _ in 0..24 {
        let first = rng.gen_range(0..deck.len());
        let card_a = deck.swap_remove(first);
        let second = rng.gen_range(0..deck.len());
        let card_b = deck.swap_remove(second);
        let hand = [card_a, card_b];
        if preflop_score(hand) >= threshold {
            return Some(hand);
        }
        deck.push(card_a);
        deck.push(card_b);
    }
    let card_a = deck.pop()?;
    let card_b = deck.pop()?;
    Some([card_a, card_b])
}

fn opponent_range_threshold(context: &BotDecisionContext, profile: BotProfile) -> f32 {
    let looseness = profile.looseness.clamp(0.0, 1.0);
    let mut threshold = 0.82 - looseness * 0.42;
    if context.legal_actions.to_call().units() > 0 {
        threshold += 0.04;
    }
    if context.street != Street::Preflop {
        threshold -= 0.05;
    }
    if context.live_opponents.unwrap_or(1) > 2 {
        threshold += 0.03;
    }
    threshold.clamp(0.28, 0.86)
}

fn remaining_deck(known_cards: &[Card]) -> Vec<Card> {
    (0..52)
        .filter_map(|index| {
            let card = Card::from_index(index).expect("deck index is valid");
            (!known_cards.contains(&card)).then_some(card)
        })
        .collect()
}

fn evaluate_holdem(hole_cards: [Card; 2], board: &[Card]) -> Option<mtholdem::card::HandValue> {
    let mut cards = Vec::with_capacity(hole_cards.len() + board.len());
    cards.extend_from_slice(&hole_cards);
    cards.extend_from_slice(board);
    mtholdem::card::evaluate_best(&cards).ok()
}

fn unique_cards(cards: &[Card]) -> bool {
    for (index, card) in cards.iter().enumerate() {
        if cards[index + 1..].contains(card) {
            return false;
        }
    }
    true
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

    fn heads_up_button_context() -> BotDecisionContext {
        let stacks = [Chips::from_units(100), Chips::from_units(100)];
        let hand = mtholdem::holdem::HoldemHand::new(
            &stacks,
            Seat::new(0).expect("button"),
            Chips::from_units(1),
            Chips::from_units(2),
            Chips::from_units(100),
        )
        .expect("hand");
        let actor = Seat::new(0).expect("button");
        assert_eq!(hand.next_to_act(), Some(actor));
        BotDecisionContext::new(
            2,
            actor,
            Street::Preflop,
            hand.legal_actions().expect("legal"),
        )
        .with_action_context(hand.action_context().expect("context"))
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

    fn heads_up_facing_flop_bet_context(
        preflop_actions: &[(usize, BetAction)],
        flop: [Card; 3],
        bettor: usize,
        bet: u64,
        actor: usize,
    ) -> BotDecisionContext {
        let stacks = [Chips::from_units(100), Chips::from_units(100)];
        let mut hand = mtholdem::holdem::HoldemHand::new(
            &stacks,
            Seat::new(0).expect("button"),
            Chips::from_units(1),
            Chips::from_units(2),
            Chips::from_units(100),
        )
        .expect("hand");
        for (seat, action) in preflop_actions.iter().copied() {
            hand.apply_action(Seat::new(seat).expect("seat"), action)
                .expect("preflop action");
        }
        hand.open_flop(flop).expect("open flop");
        hand.apply_action(
            Seat::new(bettor).expect("bettor"),
            BetAction::RaiseTo(Chips::from_units(bet)),
        )
        .expect("bet");
        let actor = Seat::new(actor).expect("actor");
        assert_eq!(hand.next_to_act(), Some(actor));
        BotDecisionContext::new(2, actor, Street::Flop, hand.legal_actions().expect("legal"))
            .with_action_context(hand.action_context().expect("context"))
    }

    fn big_blind_after_limpers_context() -> BotDecisionContext {
        let stacks = [Chips::from_units(100); 5];
        let mut hand = mtholdem::holdem::HoldemHand::new(
            &stacks,
            Seat::new(0).expect("button"),
            Chips::from_units(1),
            Chips::from_units(2),
            Chips::from_units(100),
        )
        .expect("hand");
        for index in [3, 4, 0, 1] {
            hand.apply_action(Seat::new(index).expect("seat"), BetAction::Call)
                .expect("limp");
        }
        assert_eq!(hand.next_to_act(), Some(Seat::new(2).expect("bb")));
        BotDecisionContext::new(
            5,
            Seat::new(2).expect("bb"),
            Street::Preflop,
            hand.legal_actions().expect("legal"),
        )
        .with_action_context(hand.action_context().expect("context"))
    }

    fn six_max_unopened_context(actor: usize, folded_before: &[usize]) -> BotDecisionContext {
        let stacks = [Chips::from_units(100); 6];
        let mut hand = mtholdem::holdem::HoldemHand::new(
            &stacks,
            Seat::new(0).expect("button"),
            Chips::from_units(1),
            Chips::from_units(2),
            Chips::from_units(100),
        )
        .expect("hand");
        for index in folded_before {
            hand.apply_action(Seat::new(*index).expect("seat"), BetAction::Fold)
                .expect("fold");
        }
        let actor = Seat::new(actor).expect("actor");
        assert_eq!(hand.next_to_act(), Some(actor));
        BotDecisionContext::new(
            6,
            actor,
            Street::Preflop,
            hand.legal_actions().expect("legal"),
        )
        .with_action_context(hand.action_context().expect("context"))
    }

    fn six_max_big_blind_facing_open_context() -> BotDecisionContext {
        let stacks = [Chips::from_units(100); 6];
        let mut hand = mtholdem::holdem::HoldemHand::new(
            &stacks,
            Seat::new(0).expect("button"),
            Chips::from_units(1),
            Chips::from_units(2),
            Chips::from_units(100),
        )
        .expect("hand");
        hand.apply_action(
            Seat::new(3).expect("utg"),
            BetAction::RaiseTo(Chips::from_units(5)),
        )
        .expect("open");
        for index in [4, 5, 0, 1] {
            hand.apply_action(Seat::new(index).expect("seat"), BetAction::Fold)
                .expect("fold");
        }
        let actor = Seat::new(2).expect("bb");
        assert_eq!(hand.next_to_act(), Some(actor));
        BotDecisionContext::new(
            6,
            actor,
            Street::Preflop,
            hand.legal_actions().expect("legal"),
        )
        .with_action_context(hand.action_context().expect("context"))
    }

    fn full_ring_cold_call_facing_open_context() -> BotDecisionContext {
        let stacks = [Chips::from_units(100); 8];
        let mut hand = mtholdem::holdem::HoldemHand::new(
            &stacks,
            Seat::new(0).expect("button"),
            Chips::from_units(1),
            Chips::from_units(2),
            Chips::from_units(100),
        )
        .expect("hand");
        hand.apply_action(
            Seat::new(3).expect("utg"),
            BetAction::RaiseTo(Chips::from_units(5)),
        )
        .expect("open");
        let actor = Seat::new(4).expect("actor");
        assert_eq!(hand.next_to_act(), Some(actor));
        BotDecisionContext::new(
            8,
            actor,
            Street::Preflop,
            hand.legal_actions().expect("legal"),
        )
        .with_action_context(hand.action_context().expect("context"))
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
    fn balanced_bot_folds_trash_first_in_from_early_position() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = six_max_unopened_context(3, &[]).with_hole_cards(Some([
            Card::new(Rank::Seven, mtholdem::Suit::Clubs),
            Card::new(Rank::Two, mtholdem::Suit::Diamonds),
        ]));
        let mut rng = StdRng::seed_from_u64(1);

        assert_eq!(bot.choose_action(&context, &mut rng), BetAction::Fold);
    }

    #[test]
    fn balanced_bot_opens_late_broadway_first_in() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = six_max_unopened_context(5, &[3, 4]).with_hole_cards(Some([
            Card::new(Rank::Queen, mtholdem::Suit::Diamonds),
            Card::new(Rank::Nine, mtholdem::Suit::Diamonds),
        ]));
        let mut rng = StdRng::seed_from_u64(1);

        assert!(matches!(
            bot.choose_action(&context, &mut rng),
            BetAction::RaiseTo(_)
        ));
    }

    #[test]
    fn balanced_bot_opens_more_heads_up_buttons() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = heads_up_button_context().with_hole_cards(Some([
            Card::new(Rank::Ten, mtholdem::Suit::Hearts),
            Card::new(Rank::Four, mtholdem::Suit::Hearts),
        ]));
        let mut rng = StdRng::seed_from_u64(1);

        assert!(matches!(
            bot.choose_action(&context, &mut rng),
            BetAction::RaiseTo(_)
        ));
    }

    #[test]
    fn balanced_bot_defends_big_blind_with_playable_suited_king() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = six_max_big_blind_facing_open_context().with_hole_cards(Some([
            Card::new(Rank::King, mtholdem::Suit::Spades),
            Card::new(Rank::Nine, mtholdem::Suit::Spades),
        ]));
        let mut rng = StdRng::seed_from_u64(1);

        assert_eq!(bot.choose_action(&context, &mut rng), BetAction::Call);
    }

    #[test]
    fn balanced_bot_still_folds_big_blind_trash_against_open() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = six_max_big_blind_facing_open_context().with_hole_cards(Some([
            Card::new(Rank::Seven, mtholdem::Suit::Clubs),
            Card::new(Rank::Two, mtholdem::Suit::Diamonds),
        ]));
        let mut rng = StdRng::seed_from_u64(1);

        assert_eq!(bot.choose_action(&context, &mut rng), BetAction::Fold);
    }

    #[test]
    fn balanced_bot_folds_weak_full_ring_cold_call() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = full_ring_cold_call_facing_open_context().with_hole_cards(Some([
            Card::new(Rank::Queen, mtholdem::Suit::Clubs),
            Card::new(Rank::Seven, mtholdem::Suit::Diamonds),
        ]));
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

    #[test]
    fn balanced_bot_bets_strong_draw_when_checked_to() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = BotDecisionContext::new(
            2,
            Seat::new(0).expect("seat"),
            Street::Flop,
            open_bet_legal(),
        )
        .with_hole_cards(Some([
            Card::new(Rank::Ace, mtholdem::Suit::Hearts),
            Card::new(Rank::Five, mtholdem::Suit::Spades),
        ]))
        .with_board([
            Card::new(Rank::Four, mtholdem::Suit::Spades),
            Card::new(Rank::Three, mtholdem::Suit::Spades),
            Card::new(Rank::Eight, mtholdem::Suit::Spades),
        ])
        .with_pot(Chips::from_units(10))
        .with_live_opponents(1);
        let mut rng = StdRng::seed_from_u64(1);

        assert!(matches!(
            bot.choose_action(&context, &mut rng),
            BetAction::RaiseTo(_)
        ));
    }

    #[test]
    fn balanced_bot_bets_top_pair_when_checked_to() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = BotDecisionContext::new(
            2,
            Seat::new(0).expect("seat"),
            Street::Flop,
            open_bet_legal(),
        )
        .with_hole_cards(Some([
            Card::new(Rank::Ace, mtholdem::Suit::Diamonds),
            Card::new(Rank::Five, mtholdem::Suit::Diamonds),
        ]))
        .with_board([
            Card::new(Rank::Jack, mtholdem::Suit::Hearts),
            Card::new(Rank::Four, mtholdem::Suit::Spades),
            Card::new(Rank::Ace, mtholdem::Suit::Clubs),
        ])
        .with_pot(Chips::from_units(11))
        .with_live_opponents(1);
        let mut rng = StdRng::seed_from_u64(1);

        assert!(matches!(
            bot.choose_action(&context, &mut rng),
            BetAction::RaiseTo(_)
        ));
    }

    #[test]
    fn balanced_bot_continues_with_showdown_pair_against_small_bet() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = heads_up_facing_flop_bet_context(
            &[
                (0, BetAction::RaiseTo(Chips::from_units(5))),
                (1, BetAction::RaiseTo(Chips::from_units(11))),
                (0, BetAction::Call),
            ],
            [
                Card::new(Rank::Eight, mtholdem::Suit::Diamonds),
                Card::new(Rank::Two, mtholdem::Suit::Diamonds),
                Card::new(Rank::Jack, mtholdem::Suit::Spades),
            ],
            1,
            10,
            0,
        )
        .with_hole_cards(Some([
            Card::new(Rank::Ten, mtholdem::Suit::Spades),
            Card::new(Rank::Ten, mtholdem::Suit::Hearts),
        ]))
        .with_board([
            Card::new(Rank::Eight, mtholdem::Suit::Diamonds),
            Card::new(Rank::Two, mtholdem::Suit::Diamonds),
            Card::new(Rank::Jack, mtholdem::Suit::Spades),
        ]);
        let mut rng = StdRng::seed_from_u64(1);
        let decision = bot.decide(&context, &mut rng);

        assert_ne!(decision.action, BetAction::Fold, "{decision:?}");
    }

    #[test]
    fn balanced_bot_continues_with_top_pair_against_modest_bet() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = heads_up_facing_flop_bet_context(
            &[
                (0, BetAction::RaiseTo(Chips::from_units(5))),
                (1, BetAction::Call),
            ],
            [
                Card::new(Rank::Jack, mtholdem::Suit::Hearts),
                Card::new(Rank::Four, mtholdem::Suit::Spades),
                Card::new(Rank::Ace, mtholdem::Suit::Clubs),
            ],
            1,
            6,
            0,
        )
        .with_hole_cards(Some([
            Card::new(Rank::Ace, mtholdem::Suit::Diamonds),
            Card::new(Rank::Five, mtholdem::Suit::Diamonds),
        ]))
        .with_board([
            Card::new(Rank::Jack, mtholdem::Suit::Hearts),
            Card::new(Rank::Four, mtholdem::Suit::Spades),
            Card::new(Rank::Ace, mtholdem::Suit::Clubs),
        ]);
        let mut rng = StdRng::seed_from_u64(1);
        let decision = bot.decide(&context, &mut rng);

        assert_ne!(decision.action, BetAction::Fold, "{decision:?}");
    }

    #[test]
    fn preflop_sizing_isolates_over_limpers() {
        let context = big_blind_after_limpers_context();
        let target = sized_raise(&context, BetSizing::PotFraction(0.5)).expect("raise target");
        assert_eq!(target, Chips::from_units(14));
    }

    #[test]
    fn monte_carlo_equity_separates_strong_hands_from_air() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let board = [
            Card::new(Rank::King, mtholdem::Suit::Hearts),
            Card::new(Rank::Seven, mtholdem::Suit::Clubs),
            Card::new(Rank::Two, mtholdem::Suit::Spades),
        ];
        let strong = BotDecisionContext::new(
            3,
            Seat::new(0).expect("seat"),
            Street::Flop,
            open_bet_legal(),
        )
        .with_hole_cards(Some([
            Card::new(Rank::King, mtholdem::Suit::Clubs),
            Card::new(Rank::King, mtholdem::Suit::Diamonds),
        ]))
        .with_board(board)
        .with_live_opponents(2);
        let weak = BotDecisionContext::new(
            3,
            Seat::new(0).expect("seat"),
            Street::Flop,
            open_bet_legal(),
        )
        .with_hole_cards(Some([
            Card::new(Rank::Queen, mtholdem::Suit::Diamonds),
            Card::new(Rank::Jack, mtholdem::Suit::Diamonds),
        ]))
        .with_board(board)
        .with_live_opponents(2);

        let strong_equity = bot
            .assess(&strong, &mut StdRng::seed_from_u64(11))
            .equity
            .expect("strong equity");
        let weak_equity = bot
            .assess(&weak, &mut StdRng::seed_from_u64(11))
            .equity
            .expect("weak equity");
        assert!(strong_equity > weak_equity + 0.30);
    }

    #[test]
    fn monte_carlo_equity_rejects_duplicate_known_cards() {
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
            Card::new(Rank::Ace, mtholdem::Suit::Clubs),
            Card::new(Rank::Seven, mtholdem::Suit::Clubs),
            Card::new(Rank::Two, mtholdem::Suit::Spades),
        ]);

        assert!(
            monte_carlo_equity(
                &context,
                BotPreset::Balanced.profile(),
                &mut StdRng::seed_from_u64(3),
            )
            .is_none()
        );
    }
}
