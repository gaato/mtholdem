use crate::{
    ActionClosure, BettingMotive, BettingSpot, BotDecision, BotDecisionContext, BotPreset,
    BotProfile, DecisionEvidence, DecisionInput, DrawClass, HoldingClass, InitiativeState,
    OpponentCountClass, OpponentRangeClass, PokerSituation, PositionClass, PotOddsClass,
    PressureClass, SizingFamily,
};
use mtholdem::betting::{BetAction, LegalActions, Street};
use mtholdem::{Card, Chips, HandCategory, Rank};
#[cfg(test)]
use mtholdem::{Seat, Suit};
use rand::Rng;
use rand::seq::SliceRandom;

const MONTE_CARLO_SAMPLES: usize = 96;

#[derive(Clone, Copy, Debug, PartialEq)]
struct TheoremApproximation {
    situation: PokerSituation,
    evidence: DecisionEvidence,
    made_hand_value: f32,
    cost_pressure: f32,
    ev_proxy: f32,
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

    /// Builds the situation, evidence, motive, sizing, and final legal action.
    pub fn decide(&self, context: &BotDecisionContext, rng: &mut impl Rng) -> BotDecision {
        let approximation = self.approximate(context, rng);
        let motive = self
            .preflop_motive(context)
            .unwrap_or_else(|| self.motive(context, approximation));
        let sizing = sizing(context, approximation, motive);
        let action = map_to_legal_action(context, motive, sizing);
        BotDecision {
            input: DecisionInput {
                situation: approximation.situation,
                evidence: approximation.evidence,
            },
            motive,
            sizing,
            action,
        }
    }

    /// Evaluates the public and local-private information available to the bot.
    #[must_use]
    pub fn evaluate(&self, context: &BotDecisionContext, rng: &mut impl Rng) -> DecisionEvidence {
        self.approximate(context, rng).evidence
    }

    fn approximate(
        &self,
        context: &BotDecisionContext,
        rng: &mut impl Rng,
    ) -> TheoremApproximation {
        let (made_hand_value, category) = made_hand_value(context);
        let equity = monte_carlo_equity(context, self.profile, rng);
        let strength = equity.map_or(made_hand_value, |equity| {
            clamp01(made_hand_value.mul_add(0.65, equity * 0.35))
        });
        let draw_value = draw_value(context);
        let cost_pressure = cost_pressure(context);
        let noise = rng.gen_range(-0.5..0.5) * self.profile.randomness;
        let multiway_penalty = context.live_opponents.map_or_else(
            || {
                let opponents = dealt_player_count(context).saturating_sub(1);
                scaled_count_penalty(opponents, 0.025)
            },
            |opponents| scaled_count_penalty(opponents, 0.030),
        );
        let looseness = (self.profile.looseness - 0.35) * 0.20;
        let ev_proxy =
            clamp01(strength + draw_value + looseness + noise - cost_pressure - multiway_penalty);
        let evidence = DecisionEvidence {
            holding: holding_class(context, category),
            category,
            draw: draw_class(context),
            draw_completion_probability: draw_completion_probability(context),
            equity,
            pot_odds: pot_odds(context),
            pot_odds_class: pot_odds_class(context),
            opponent_range: opponent_range_class(context, self.profile),
            pressure: pressure_class(context, cost_pressure),
        };
        TheoremApproximation {
            situation: poker_situation(context),
            evidence,
            made_hand_value,
            cost_pressure,
            ev_proxy,
        }
    }

    fn preflop_motive(&self, context: &BotDecisionContext) -> Option<BettingMotive> {
        if context.street != Street::Preflop {
            return None;
        }
        let hand_value = context.hole_cards.map(preflop_hand_value)?;
        let spot = preflop_spot(context)?;
        let position = position_openness(context);
        let short_handed = short_handed_openness(dealt_player_count(context));
        match spot {
            PreflopSpot::FirstIn => {
                if hand_value >= opening_range_floor(self.profile, position, short_handed) {
                    Some(BettingMotive::Steal)
                } else {
                    Some(BettingMotive::GiveUp)
                }
            }
            PreflopSpot::LimpedPot => {
                let range_floor = opening_range_floor(self.profile, position, short_handed);
                if hand_value >= range_floor + 0.03 {
                    Some(BettingMotive::Isolation)
                } else if hand_value >= range_floor - 0.16 {
                    Some(BettingMotive::Defend)
                } else {
                    Some(BettingMotive::GiveUp)
                }
            }
            PreflopSpot::BigBlindOption => {
                let limpers = context.matched_bet_opponents.unwrap_or(0).saturating_sub(1);
                if limpers > 0
                    && hand_value
                        >= 0.62 - self.profile.aggression * 0.10 - self.profile.looseness * 0.06
                {
                    Some(BettingMotive::Isolation)
                } else {
                    Some(BettingMotive::FreeCard)
                }
            }
            PreflopSpot::FacingRaise => {
                let call_floor = defense_range_floor(self.profile, context, short_handed);
                let raise_floor = value_reraise_floor(self.profile, context);
                if hand_value >= raise_floor {
                    Some(BettingMotive::Value)
                } else if hand_value >= call_floor
                    && !dominated_preflop_pressure(context, hand_value)
                {
                    Some(BettingMotive::Defend)
                } else {
                    Some(BettingMotive::GapFold)
                }
            }
        }
    }

    fn motive(
        &self,
        context: &BotDecisionContext,
        approximation: TheoremApproximation,
    ) -> BettingMotive {
        if context.legal_actions.can_check() {
            if can_apply_aggressive_motive(self.profile, context, approximation) {
                if near_all_in(context, approximation) {
                    return BettingMotive::AllInPressure;
                }
                return aggressive_motive(approximation);
            }
            return if approximation.evidence.holding == HoldingClass::Air
                && approximation.evidence.draw == DrawClass::None
            {
                BettingMotive::GiveUp
            } else if has_showdown_value(approximation) {
                BettingMotive::ShowdownValue
            } else {
                BettingMotive::FreeCard
            };
        }

        if must_release_hand(self.profile, context, approximation) {
            return if approximation.evidence.pressure == PressureClass::High {
                BettingMotive::PressureFold
            } else {
                BettingMotive::GiveUp
            };
        }
        if can_apply_raise_motive(self.profile, context, approximation) {
            if near_all_in(context, approximation) {
                return BettingMotive::AllInPressure;
            }
            return aggressive_motive(approximation);
        }
        if approximation.evidence.pot_odds_class == PotOddsClass::GoodPrice
            && has_draw_or_showdown_value(approximation)
        {
            BettingMotive::PotOddsCall
        } else if has_showdown_value(approximation) {
            BettingMotive::ShowdownValue
        } else {
            BettingMotive::GiveUp
        }
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

fn opening_range_floor(profile: BotProfile, position: f32, short_handed: f32) -> f32 {
    (0.74 - profile.looseness * 0.14 - position * 0.18 - short_handed * 0.12).clamp(0.36, 0.80)
}

fn defense_range_floor(
    profile: BotProfile,
    context: &BotDecisionContext,
    short_handed: f32,
) -> f32 {
    let blind_discount = blind_defense_discount(context);
    let posted_blind = blind_discount > 0.0;
    let price_discount = call_cost_ratio(context).min(0.22) * 0.22;
    let full_ring_penalty = scaled_count_penalty(dealt_player_count(context), 0.006).min(0.04);
    let multiway_penalty = context.matched_bet_opponents.map_or(0.0, |matched| {
        scaled_count_penalty(matched, 0.020).min(0.08)
    });
    let cold_call_penalty = if posted_blind { 0.0 } else { 0.06 };
    let raise_depth_penalty =
        f32::from(context.street_raise_count.unwrap_or(1).saturating_sub(1)) * 0.08;
    (0.62 - profile.call_bias * 0.12 - blind_discount - short_handed * 0.08 - price_discount
        + raise_size_cost(context)
        + full_ring_penalty
        + multiway_penalty
        + cold_call_penalty
        + raise_depth_penalty)
        .clamp(0.34, 0.86)
}

fn blind_defense_discount(context: &BotDecisionContext) -> f32 {
    let Some(contributed) = context.actor_street_contributed else {
        return 0.0;
    };
    if contributed.is_zero() {
        return 0.0;
    }
    if context
        .big_blind
        .is_some_and(|big_blind| contributed >= big_blind)
    {
        0.10
    } else {
        0.04
    }
}

fn value_reraise_floor(profile: BotProfile, context: &BotDecisionContext) -> f32 {
    let depth_penalty = f32::from(context.street_raise_count.unwrap_or(1).saturating_sub(1)) * 0.07;
    let short_handed_discount = short_handed_openness(dealt_player_count(context)) * 0.04;
    let blind_discount = (blind_defense_discount(context) * 0.40).min(0.04);
    (0.84 - profile.aggression * 0.10 - short_handed_discount - blind_discount
        + depth_penalty
        + raise_size_cost(context))
    .clamp(0.68, 0.99)
}

fn dominated_preflop_pressure(context: &BotDecisionContext, hand_value: f32) -> bool {
    if hand_value >= 0.92 {
        return false;
    }
    let Some(hole_cards) = context.hole_cards else {
        return false;
    };
    let raise_depth = context.street_raise_count.unwrap_or(1).saturating_sub(1);
    let full_ring = dealt_player_count(context) >= 6;
    let multiway =
        context.matched_bet_opponents.unwrap_or(1) > 1 || context.live_opponents.unwrap_or(1) > 1;
    let not_closing = action_closure(context) == ActionClosure::NotClosing;
    if raise_depth > 0 || full_ring && not_closing || full_ring && multiway {
        return dominated_offsuit_broadway(hole_cards);
    }
    not_closing && multiway && weak_offsuit_gap(hole_cards)
}

fn dominated_offsuit_broadway(cards: [Card; 2]) -> bool {
    if cards[0].rank() == cards[1].rank() || cards[0].suit() == cards[1].suit() {
        return false;
    }
    let high = cards[0].rank().value().max(cards[1].rank().value());
    let low = cards[0].rank().value().min(cards[1].rank().value());
    high == Rank::Ace.value() && low <= Rank::Jack.value()
        || high == Rank::King.value() && (Rank::Ten.value()..=Rank::Queen.value()).contains(&low)
        || high == Rank::Queen.value() && low >= Rank::Ten.value()
}

fn weak_offsuit_gap(cards: [Card; 2]) -> bool {
    if cards[0].rank() == cards[1].rank() || cards[0].suit() == cards[1].suit() {
        return false;
    }
    let high = cards[0].rank().value().max(cards[1].rank().value());
    let low = cards[0].rank().value().min(cards[1].rank().value());
    high <= Rank::Jack.value() && high.saturating_sub(low) >= 2
}

fn raise_size_cost(context: &BotDecisionContext) -> f32 {
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
    if context.street == Street::Preflop
        && context
            .actor_street_contributed
            .is_some_and(|contributed| contributed > Chips::ZERO)
        && context.current_bet == context.big_blind
        && context.legal_actions.to_call() > Chips::ZERO
        && context.live_opponents == Some(1)
    {
        return 1.0;
    }
    let Some(button) = context.button else {
        return 0.5;
    };
    let seats = context.player_count.max(2);
    let distance_to_button = (button.index() + seats - context.actor.index()) % seats;
    let denominator = seats.saturating_sub(1).max(1);
    1.0 - capped_ratio(distance_to_button as u64, denominator as u64)
}

fn dealt_player_count(context: &BotDecisionContext) -> usize {
    context
        .dealt_player_count
        .unwrap_or(context.player_count)
        .max(2)
}

fn poker_situation(context: &BotDecisionContext) -> PokerSituation {
    PokerSituation {
        betting_spot: betting_spot(context),
        position: position_class(context),
        opponents: opponent_count_class(context),
        action_closure: action_closure(context),
        initiative: initiative_state(context),
    }
}

fn betting_spot(context: &BotDecisionContext) -> BettingSpot {
    if context.street == Street::Preflop {
        return match preflop_spot(context) {
            Some(PreflopSpot::FirstIn) => BettingSpot::Unopened,
            Some(PreflopSpot::LimpedPot) => BettingSpot::Limped,
            Some(PreflopSpot::BigBlindOption) => BettingSpot::BigBlindOption,
            Some(PreflopSpot::FacingRaise) => BettingSpot::FacingRaise,
            None => BettingSpot::Unknown,
        };
    }
    if context.legal_actions.can_check() {
        BettingSpot::CheckedTo
    } else if context.street_raise_count.unwrap_or(0) > 0 {
        BettingSpot::FacingRaise
    } else {
        BettingSpot::FacingBet
    }
}

fn position_class(context: &BotDecisionContext) -> PositionClass {
    if context
        .actor_street_contributed
        .is_some_and(|contributed| contributed > Chips::ZERO)
        && context.street == Street::Preflop
    {
        return PositionClass::Blind;
    }
    if context.button.is_none() {
        return PositionClass::Unknown;
    }
    let openness = position_openness(context);
    if openness >= 0.72 {
        PositionClass::Late
    } else if openness >= 0.38 {
        PositionClass::Middle
    } else {
        PositionClass::Early
    }
}

fn opponent_count_class(context: &BotDecisionContext) -> OpponentCountClass {
    match context
        .live_opponents
        .unwrap_or_else(|| dealt_player_count(context).saturating_sub(1))
    {
        0 => OpponentCountClass::None,
        1 => OpponentCountClass::HeadsUp,
        _ => OpponentCountClass::Multiway,
    }
}

fn action_closure(context: &BotDecisionContext) -> ActionClosure {
    match (context.live_opponents, context.matched_bet_opponents) {
        (Some(live), Some(matched)) if matched >= live => ActionClosure::Closing,
        (Some(_), Some(_)) => ActionClosure::NotClosing,
        _ => ActionClosure::Unknown,
    }
}

fn initiative_state(context: &BotDecisionContext) -> InitiativeState {
    match context.postflop_initiative {
        Some(true) => InitiativeState::HasInitiative,
        Some(false) => InitiativeState::NoInitiative,
        None => InitiativeState::Unknown,
    }
}

fn short_handed_openness(player_count: usize) -> f32 {
    let missing = 9_usize.saturating_sub(player_count);
    capped_ratio(missing as u64, 7)
}

fn sizing(
    context: &BotDecisionContext,
    approximation: TheoremApproximation,
    motive: BettingMotive,
) -> SizingFamily {
    match motive {
        BettingMotive::Value
        | BettingMotive::Protection
        | BettingMotive::Bluff
        | BettingMotive::SemiBluff
        | BettingMotive::Steal
        | BettingMotive::Isolation => {
            if preflop_open_or_isolation_target(context).is_some() {
                return SizingFamily::PreflopOpenOrIsolation;
            }
            if approximation.ev_proxy >= 0.88 || motive == BettingMotive::Protection {
                SizingFamily::PotFraction(0.75)
            } else if approximation.ev_proxy >= 0.68 || motive == BettingMotive::Value {
                SizingFamily::PotFraction(0.50)
            } else {
                SizingFamily::PotFraction(0.33)
            }
        }
        BettingMotive::AllInPressure => SizingFamily::AllIn,
        BettingMotive::PotOddsCall
        | BettingMotive::Defend
        | BettingMotive::ShowdownValue
        | BettingMotive::GapFold
        | BettingMotive::PressureFold
        | BettingMotive::GiveUp
        | BettingMotive::FreeCard => SizingFamily::None,
    }
}

fn can_apply_aggressive_motive(
    profile: BotProfile,
    context: &BotDecisionContext,
    approximation: TheoremApproximation,
) -> bool {
    if high_reverse_implied_pressure(approximation)
        && matches!(
            approximation.evidence.holding,
            HoldingClass::BoardPairOnly
                | HoldingClass::WeakPair
                | HoldingClass::TopPairWeakKicker
                | HoldingClass::TopPairGoodKicker
        )
    {
        return false;
    }
    let initiative_bonus = postflop_initiative_bonus(context);
    legal_raise_bounds(context.legal_actions).is_some()
        && approximation.ev_proxy
            + profile.aggression.mul_add(0.20, profile.bluff_rate * 0.10)
            + initiative_bonus
            >= 0.63
}

fn postflop_initiative_bonus(context: &BotDecisionContext) -> f32 {
    if context.street != Street::Flop || context.postflop_initiative != Some(true) {
        return 0.0;
    }
    if context.live_opponents.unwrap_or(1) <= 1 {
        0.08
    } else {
        0.03
    }
}

fn must_release_hand(
    profile: BotProfile,
    context: &BotDecisionContext,
    approximation: TheoremApproximation,
) -> bool {
    let range_floor = 0.38 - (profile.call_bias * 0.14) + call_cost_ratio(context) * 0.18;
    approximation.ev_proxy < range_floor
}

fn can_apply_raise_motive(
    profile: BotProfile,
    context: &BotDecisionContext,
    approximation: TheoremApproximation,
) -> bool {
    if legal_raise_bounds(context.legal_actions).is_none() {
        return false;
    }
    let raise_depth = context.street_raise_count.unwrap_or(0);
    let depth_penalty = f32::from(raise_depth) * 0.10;
    let call_ratio_penalty = call_cost_ratio(context) * 0.20;
    let range_floor = 0.82 + depth_penalty + call_ratio_penalty - profile.aggression * 0.18;
    let monster_value = approximation
        .evidence
        .category
        .is_some_and(|category| category >= HandCategory::ThreeOfAKind)
        && approximation.made_hand_value >= 0.78
        && approximation.ev_proxy >= 0.66;
    approximation.ev_proxy >= range_floor
        && (approximation.made_hand_value >= 0.72
            || profile.bluff_rate > 0.18 && approximation.ev_proxy >= 0.76)
        || monster_value
}

fn near_all_in(context: &BotDecisionContext, approximation: TheoremApproximation) -> bool {
    let Some(stack) = context.actor_stack else {
        return false;
    };
    stack.units() <= 8 || approximation.ev_proxy >= 0.94 && stack.units() <= 24
}

fn aggressive_motive(approximation: TheoremApproximation) -> BettingMotive {
    if approximation.evidence.draw == DrawClass::ComboDraw
        || matches!(
            approximation.evidence.draw,
            DrawClass::FlushDraw | DrawClass::OpenEnded
        ) && approximation.evidence.holding != HoldingClass::PremiumMade
    {
        return BettingMotive::SemiBluff;
    }
    if matches!(
        approximation.evidence.holding,
        HoldingClass::TwoPairOrTrips | HoldingClass::PremiumMade
    ) {
        return BettingMotive::Value;
    }
    if matches!(
        approximation.evidence.holding,
        HoldingClass::OverPair | HoldingClass::TopPairGoodKicker
    ) && approximation.cost_pressure < 0.16
        && !high_reverse_implied_pressure(approximation)
    {
        return BettingMotive::Protection;
    }
    BettingMotive::Bluff
}

fn has_draw_or_showdown_value(approximation: TheoremApproximation) -> bool {
    approximation.evidence.draw != DrawClass::None || has_showdown_value(approximation)
}

fn has_showdown_value(approximation: TheoremApproximation) -> bool {
    match approximation.evidence.holding {
        HoldingClass::Unknown | HoldingClass::Preflop | HoldingClass::Air => false,
        HoldingClass::Overcards => approximation
            .evidence
            .equity
            .is_some_and(|equity| equity >= 0.55),
        HoldingClass::BoardPairOnly => approximation
            .evidence
            .equity
            .is_some_and(|equity| equity >= 0.45),
        HoldingClass::WeakPair
        | HoldingClass::TopPairWeakKicker
        | HoldingClass::TopPairGoodKicker
        | HoldingClass::OverPair
        | HoldingClass::TwoPairOrTrips
        | HoldingClass::PremiumMade => true,
    }
}

fn high_reverse_implied_pressure(approximation: TheoremApproximation) -> bool {
    matches!(
        approximation.situation.opponents,
        OpponentCountClass::Multiway
    ) && (matches!(
        approximation.evidence.opponent_range,
        OpponentRangeClass::Continuing | OpponentRangeClass::Tight
    ) || approximation.situation.initiative == InitiativeState::NoInitiative)
        || approximation.evidence.pressure == PressureClass::High
            && matches!(
                approximation.evidence.holding,
                HoldingClass::BoardPairOnly
                    | HoldingClass::WeakPair
                    | HoldingClass::TopPairWeakKicker
            )
}

fn map_to_legal_action(
    context: &BotDecisionContext,
    motive: BettingMotive,
    sizing: SizingFamily,
) -> BetAction {
    match motive {
        BettingMotive::GapFold | BettingMotive::PressureFold => BetAction::Fold,
        BettingMotive::GiveUp | BettingMotive::FreeCard => {
            if context.legal_actions.can_check() {
                BetAction::Check
            } else {
                BetAction::Fold
            }
        }
        BettingMotive::PotOddsCall | BettingMotive::Defend | BettingMotive::ShowdownValue => {
            if context.legal_actions.can_check() {
                BetAction::Check
            } else {
                BetAction::Call
            }
        }
        BettingMotive::Value
        | BettingMotive::Protection
        | BettingMotive::Bluff
        | BettingMotive::SemiBluff
        | BettingMotive::Steal
        | BettingMotive::Isolation => {
            sized_raise(context, sizing).map_or(BetAction::Call, BetAction::RaiseTo)
        }
        BettingMotive::AllInPressure => {
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

fn sized_raise(context: &BotDecisionContext, sizing: SizingFamily) -> Option<Chips> {
    let (minimum, maximum) = legal_raise_bounds(context.legal_actions)?;
    let target = match sizing {
        SizingFamily::PreflopOpenOrIsolation => preflop_open_or_isolation_target(context)?,
        SizingFamily::PotFraction(fraction) => {
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
        SizingFamily::AllIn => maximum,
        SizingFamily::None => minimum,
    };
    Some(clamp_chips(target, minimum, maximum))
}

fn preflop_open_or_isolation_target(context: &BotDecisionContext) -> Option<Chips> {
    if context.street != Street::Preflop || context.current_bet? != context.big_blind? {
        return None;
    }
    let big_blind = context.big_blind?.units();
    if big_blind == 0 {
        return None;
    }
    let matched = context.matched_bet_opponents.unwrap_or(0) as u64;
    let limpers = if context.legal_actions.can_check() {
        matched
    } else {
        matched.saturating_sub(1)
    };
    let target_blinds = 3_u64.saturating_add(limpers);
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

fn made_hand_value(context: &BotDecisionContext) -> (f32, Option<HandCategory>) {
    if context.street == Street::Preflop || context.board.len() < 3 {
        return (context.hole_cards.map_or(0.45, preflop_hand_value), None);
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

fn holding_class(context: &BotDecisionContext, category: Option<HandCategory>) -> HoldingClass {
    let Some(hole_cards) = context.hole_cards else {
        return HoldingClass::Unknown;
    };
    if context.street == Street::Preflop || context.board.len() < 3 {
        return HoldingClass::Preflop;
    }
    match category {
        Some(HandCategory::HighCard) => {
            if overcard_count(hole_cards, &context.board) > 0 {
                HoldingClass::Overcards
            } else {
                HoldingClass::Air
            }
        }
        Some(HandCategory::OnePair) => one_pair_holding(hole_cards, &context.board),
        Some(HandCategory::TwoPair | HandCategory::ThreeOfAKind) => HoldingClass::TwoPairOrTrips,
        Some(
            HandCategory::Straight
            | HandCategory::Flush
            | HandCategory::FullHouse
            | HandCategory::FourOfAKind
            | HandCategory::StraightFlush,
        ) => HoldingClass::PremiumMade,
        None => HoldingClass::Unknown,
    }
}

fn one_pair_holding(hole_cards: [Card; 2], board: &[Card]) -> HoldingClass {
    let [first, second] = hole_cards;
    let first_rank = first.rank().value();
    let second_rank = second.rank().value();
    let board_high = board_high_rank(board).unwrap_or(0);
    if first_rank == second_rank {
        return if first_rank > board_high {
            HoldingClass::OverPair
        } else {
            HoldingClass::WeakPair
        };
    }
    let first_pairs_board = board_contains_rank(board, first_rank);
    let second_pairs_board = board_contains_rank(board, second_rank);
    match (first_pairs_board, second_pairs_board) {
        (true, true) => HoldingClass::TwoPairOrTrips,
        (false, false) => HoldingClass::BoardPairOnly,
        (true, false) => paired_hole_card_class(first_rank, second_rank, board_high),
        (false, true) => paired_hole_card_class(second_rank, first_rank, board_high),
    }
}

fn paired_hole_card_class(pair_rank: u8, kicker_rank: u8, board_high: u8) -> HoldingClass {
    if pair_rank != board_high {
        return HoldingClass::WeakPair;
    }
    if good_top_pair_kicker(pair_rank, kicker_rank) {
        HoldingClass::TopPairGoodKicker
    } else {
        HoldingClass::TopPairWeakKicker
    }
}

fn good_top_pair_kicker(pair_rank: u8, kicker_rank: u8) -> bool {
    match pair_rank {
        14 => kicker_rank >= Rank::Queen.value(),
        13 => kicker_rank >= Rank::Jack.value(),
        _ => kicker_rank >= Rank::Ten.value(),
    }
}

fn board_high_rank(board: &[Card]) -> Option<u8> {
    board.iter().map(|card| card.rank().value()).max()
}

fn board_contains_rank(board: &[Card], rank: u8) -> bool {
    board.iter().any(|card| card.rank().value() == rank)
}

fn draw_class(context: &BotDecisionContext) -> DrawClass {
    if matches!(context.street, Street::Preflop | Street::River) || context.board.len() < 3 {
        return DrawClass::None;
    }
    let Some(hole_cards) = context.hole_cards else {
        return DrawClass::None;
    };
    let mut cards = Vec::with_capacity(hole_cards.len() + context.board.len());
    cards.extend_from_slice(&hole_cards);
    cards.extend_from_slice(&context.board);
    let flush = flush_draw(&cards);
    let straight = straight_draw(&cards);
    let overcards = overcard_count(hole_cards, &context.board);
    match (flush, straight, overcards) {
        (true, Some(_), _) | (true, None, 2) => DrawClass::ComboDraw,
        (true, None, _) => DrawClass::FlushDraw,
        (false, Some(DrawClass::OpenEnded), _) => DrawClass::OpenEnded,
        (false, Some(DrawClass::Gutshot), _) => DrawClass::Gutshot,
        (false, _, 1..) => DrawClass::Overcards,
        (false, _, 0) => DrawClass::None,
    }
}

fn draw_completion_probability(context: &BotDecisionContext) -> Option<f32> {
    if matches!(context.street, Street::Preflop | Street::River) || context.board.len() < 3 {
        return None;
    }
    let outs = draw_outs(context)?;
    if outs == 0 {
        return Some(0.0);
    }
    let known = context.hole_cards.map_or(context.board.len(), |cards| {
        cards.len().saturating_add(context.board.len())
    });
    let unseen = 52_usize.saturating_sub(known).max(1);
    let outs = outs.min(unseen);
    let one_card = ratio_usize(outs, unseen);
    if context.street == Street::Flop && context.board.len() == 3 {
        let next_unseen = unseen.saturating_sub(1).max(1);
        let misses_turn = ratio_usize(unseen.saturating_sub(outs), unseen);
        let misses_river = ratio_usize(next_unseen.saturating_sub(outs), next_unseen);
        Some(clamp01(1.0 - misses_turn * misses_river))
    } else {
        Some(clamp01(one_card))
    }
}

fn draw_outs(context: &BotDecisionContext) -> Option<usize> {
    let hole_cards = context.hole_cards?;
    let mut cards = Vec::with_capacity(hole_cards.len() + context.board.len());
    cards.extend_from_slice(&hole_cards);
    cards.extend_from_slice(&context.board);
    let mut outs = 0_usize;
    if flush_draw(&cards) {
        outs = outs.saturating_add(9);
    }
    outs = outs.saturating_add(match straight_draw(&cards) {
        Some(DrawClass::OpenEnded) => 8,
        Some(DrawClass::Gutshot) => 4,
        _ => 0,
    });
    outs = outs.saturating_add(overcard_count(hole_cards, &context.board).saturating_mul(3));
    Some(outs.min(15))
}

fn flush_draw(cards: &[Card]) -> bool {
    let mut counts = [0_u8; 4];
    for card in cards {
        counts[card.suit() as usize] += 1;
    }
    counts.into_iter().max().unwrap_or(0) == 4
}

fn straight_draw(cards: &[Card]) -> Option<DrawClass> {
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
    if best_window >= 4 {
        Some(DrawClass::OpenEnded)
    } else if best_window == 3 {
        Some(DrawClass::Gutshot)
    } else {
        None
    }
}

fn overcard_count(hole_cards: [Card; 2], board: &[Card]) -> usize {
    let Some(board_high) = board.iter().map(|card| card.rank().value()).max() else {
        return 0;
    };
    hole_cards
        .into_iter()
        .filter(|card| card.rank().value() > board_high)
        .count()
}

fn draw_value(context: &BotDecisionContext) -> f32 {
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
        .unwrap_or_else(|| dealt_player_count(context).saturating_sub(1))
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
    let range_floor = opponent_range_floor(context, profile);
    for _ in 0..24 {
        let first = rng.gen_range(0..deck.len());
        let card_a = deck.swap_remove(first);
        let second = rng.gen_range(0..deck.len());
        let card_b = deck.swap_remove(second);
        let hand = [card_a, card_b];
        if preflop_hand_value(hand) >= range_floor {
            return Some(hand);
        }
        deck.push(card_a);
        deck.push(card_b);
    }
    let card_a = deck.pop()?;
    let card_b = deck.pop()?;
    Some([card_a, card_b])
}

fn opponent_range_floor(context: &BotDecisionContext, profile: BotProfile) -> f32 {
    let looseness = profile.looseness.clamp(0.0, 1.0);
    let mut range_floor = 0.82 - looseness * 0.42;
    if context.legal_actions.to_call().units() > 0 {
        range_floor += 0.04;
    }
    if context.street != Street::Preflop {
        range_floor -= 0.05;
    }
    if context.live_opponents.unwrap_or(1) > 2 {
        range_floor += 0.03;
    }
    range_floor.clamp(0.28, 0.86)
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

fn cost_pressure(context: &BotDecisionContext) -> f32 {
    let mut pressure = call_cost_ratio(context) * 0.45;
    if context.legal_actions.to_call().units() >= 20 {
        pressure += 0.08;
    }
    pressure
}

fn pot_odds(context: &BotDecisionContext) -> Option<f32> {
    (context.legal_actions.to_call() > Chips::ZERO).then(|| call_cost_ratio(context))
}

fn pot_odds_class(context: &BotDecisionContext) -> PotOddsClass {
    let Some(price) = pot_odds(context) else {
        return PotOddsClass::NoPrice;
    };
    let draw_probability = draw_completion_probability(context).unwrap_or(0.0);
    if draw_probability >= price {
        PotOddsClass::GoodPrice
    } else if draw_probability + 0.08 >= price {
        PotOddsClass::Close
    } else {
        PotOddsClass::BadPrice
    }
}

fn opponent_range_class(context: &BotDecisionContext, profile: BotProfile) -> OpponentRangeClass {
    if context.street == Street::Preflop {
        return match preflop_spot(context) {
            Some(PreflopSpot::FirstIn | PreflopSpot::BigBlindOption) => OpponentRangeClass::Broad,
            Some(PreflopSpot::FacingRaise) => OpponentRangeClass::Tight,
            Some(PreflopSpot::LimpedPot) | None => OpponentRangeClass::Continuing,
        };
    }
    let range_floor = opponent_range_floor(context, profile);
    if range_floor >= 0.70 {
        OpponentRangeClass::Tight
    } else if range_floor >= 0.46 {
        OpponentRangeClass::Continuing
    } else {
        OpponentRangeClass::Broad
    }
}

fn pressure_class(context: &BotDecisionContext, pressure: f32) -> PressureClass {
    let raise_depth = f32::from(context.street_raise_count.unwrap_or(0)) * 0.08;
    let multiway = match opponent_count_class(context) {
        OpponentCountClass::Multiway => 0.08,
        OpponentCountClass::None | OpponentCountClass::HeadsUp => 0.0,
    };
    let pressure = pressure + raise_depth + multiway;
    if pressure >= 0.28 {
        PressureClass::High
    } else if pressure >= 0.12 {
        PressureClass::Medium
    } else {
        PressureClass::Low
    }
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

fn ratio_usize(numerator: usize, denominator: usize) -> f32 {
    let numerator = u16::try_from(numerator.min(10_000)).expect("ratio numerator is capped");
    let denominator = u16::try_from(denominator.min(10_000)).expect("ratio denominator is capped");
    f32::from(numerator) / f32::from(denominator.max(1))
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

fn preflop_hand_value(cards: [Card; 2]) -> f32 {
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
            6 => 0.59,
            5 => 0.57,
            2..=4 => 0.50,
            _ => 0.45,
        };
    }
    let mut hand_value = (f32::from(high) / 14.0) * 0.42 + (f32::from(low) / 14.0) * 0.18;
    if high == 14 {
        hand_value += 0.12;
    }
    if high == 14 && low <= 5 && !suited {
        hand_value -= 0.05;
    }
    if broadway(high) && broadway(low) {
        hand_value += 0.16;
    }
    if suited {
        hand_value += 0.06;
    }
    if high - low <= 2 {
        hand_value += 0.05;
    }
    hand_value.min(0.88)
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
        heads_up_button_context_at_table(2)
    }

    fn heads_up_button_context_at_table(table_seat_count: usize) -> BotDecisionContext {
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
            table_seat_count,
            actor,
            Street::Preflop,
            hand.legal_actions().expect("legal"),
        )
        .with_dealt_player_count(2)
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
            Card::new(Rank::Two, Suit::Clubs),
            Card::new(Rank::Seven, Suit::Diamonds),
            Card::new(Rank::Nine, Suit::Hearts),
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

    fn full_ring_big_blind_facing_open_context() -> BotDecisionContext {
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
            BetAction::RaiseTo(Chips::from_units(6)),
        )
        .expect("open");
        for index in [4, 5, 6, 7, 0, 1] {
            hand.apply_action(Seat::new(index).expect("seat"), BetAction::Fold)
                .expect("fold");
        }
        let actor = Seat::new(2).expect("big blind");
        assert_eq!(hand.next_to_act(), Some(actor));
        BotDecisionContext::new(
            8,
            actor,
            Street::Preflop,
            hand.legal_actions().expect("legal"),
        )
        .with_action_context(hand.action_context().expect("context"))
    }

    fn full_ring_cold_call_facing_three_bet_context() -> BotDecisionContext {
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
        hand.apply_action(
            Seat::new(4).expect("three-bettor"),
            BetAction::RaiseTo(Chips::from_units(11)),
        )
        .expect("three-bet");
        let actor = Seat::new(5).expect("actor");
        assert_eq!(hand.next_to_act(), Some(actor));
        BotDecisionContext::new(
            8,
            actor,
            Street::Preflop,
            hand.legal_actions().expect("legal"),
        )
        .with_action_context(hand.action_context().expect("context"))
    }

    fn full_ring_big_blind_facing_three_bet_context() -> BotDecisionContext {
        let stacks = [Chips::from_units(100); 8];
        let mut hand = mtholdem::holdem::HoldemHand::new(
            &stacks,
            Seat::new(6).expect("button"),
            Chips::from_units(1),
            Chips::from_units(2),
            Chips::from_units(100),
        )
        .expect("hand");
        for index in [1, 2] {
            hand.apply_action(Seat::new(index).expect("seat"), BetAction::Fold)
                .expect("fold");
        }
        hand.apply_action(
            Seat::new(3).expect("opener"),
            BetAction::RaiseTo(Chips::from_units(6)),
        )
        .expect("open");
        hand.apply_action(
            Seat::new(4).expect("three-bettor"),
            BetAction::RaiseTo(Chips::from_units(15)),
        )
        .expect("three-bet");
        for index in [5, 6, 7] {
            hand.apply_action(Seat::new(index).expect("seat"), BetAction::Fold)
                .expect("fold");
        }
        let actor = Seat::new(0).expect("big blind");
        assert_eq!(hand.next_to_act(), Some(actor));
        BotDecisionContext::new(
            8,
            actor,
            Street::Preflop,
            hand.legal_actions().expect("legal"),
        )
        .with_action_context(hand.action_context().expect("context"))
    }

    fn full_ring_ajo_multiway_three_bet_flop_context() -> BotDecisionContext {
        let stacks = [Chips::from_units(100); 8];
        let mut hand = mtholdem::holdem::HoldemHand::new(
            &stacks,
            Seat::new(6).expect("button"),
            Chips::from_units(1),
            Chips::from_units(2),
            Chips::from_units(100),
        )
        .expect("hand");
        for index in [1, 2] {
            hand.apply_action(Seat::new(index).expect("seat"), BetAction::Fold)
                .expect("fold");
        }
        hand.apply_action(
            Seat::new(3).expect("opener"),
            BetAction::RaiseTo(Chips::from_units(6)),
        )
        .expect("open");
        hand.apply_action(
            Seat::new(4).expect("three-bettor"),
            BetAction::RaiseTo(Chips::from_units(15)),
        )
        .expect("three-bet");
        for index in [5, 6, 7] {
            hand.apply_action(Seat::new(index).expect("seat"), BetAction::Fold)
                .expect("fold");
        }
        hand.apply_action(Seat::new(0).expect("big blind"), BetAction::Call)
            .expect("big blind call");
        hand.apply_action(Seat::new(3).expect("opener"), BetAction::Call)
            .expect("opener call");
        let flop = [
            Card::new(Rank::Ace, Suit::Hearts),
            Card::new(Rank::Three, Suit::Spades),
            Card::new(Rank::Two, Suit::Clubs),
        ];
        hand.open_flop(flop).expect("open flop");
        let actor = Seat::new(0).expect("big blind");
        assert_eq!(hand.next_to_act(), Some(actor));
        BotDecisionContext::new(8, actor, Street::Flop, hand.legal_actions().expect("legal"))
            .with_action_context(hand.action_context().expect("context"))
            .with_board(flop)
    }

    fn unopened_small_blind_context() -> BotDecisionContext {
        let stacks = [Chips::from_units(100); 8];
        let mut hand = mtholdem::holdem::HoldemHand::new(
            &stacks,
            Seat::new(7).expect("button"),
            Chips::from_units(1),
            Chips::from_units(2),
            Chips::from_units(100),
        )
        .expect("hand");
        for index in [2, 3, 4, 5, 6, 7] {
            hand.apply_action(Seat::new(index).expect("seat"), BetAction::Fold)
                .expect("fold");
        }
        let actor = Seat::new(0).expect("small blind");
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
            Card::new(Rank::Seven, Suit::Clubs),
            Card::new(Rank::Two, Suit::Diamonds),
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
            Card::new(Rank::Seven, Suit::Clubs),
            Card::new(Rank::Two, Suit::Diamonds),
        ]));
        let mut rng = StdRng::seed_from_u64(1);

        assert_eq!(bot.choose_action(&context, &mut rng), BetAction::Fold);
    }

    #[test]
    fn balanced_bot_folds_weak_offsuit_ace_first_in_from_six_max_early_position() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = six_max_unopened_context(3, &[]).with_hole_cards(Some([
            Card::new(Rank::Ace, Suit::Diamonds),
            Card::new(Rank::Three, Suit::Spades),
        ]));
        let mut rng = StdRng::seed_from_u64(1);

        assert_eq!(bot.choose_action(&context, &mut rng), BetAction::Fold);
    }

    #[test]
    fn balanced_bot_opens_middle_pair_first_in_from_six_max_early_position() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = six_max_unopened_context(3, &[]).with_hole_cards(Some([
            Card::new(Rank::Five, Suit::Clubs),
            Card::new(Rank::Five, Suit::Diamonds),
        ]));
        let mut rng = StdRng::seed_from_u64(1);

        assert!(matches!(
            bot.choose_action(&context, &mut rng),
            BetAction::RaiseTo(_)
        ));
    }

    #[test]
    fn balanced_bot_opens_late_broadway_first_in() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = six_max_unopened_context(5, &[3, 4]).with_hole_cards(Some([
            Card::new(Rank::Queen, Suit::Diamonds),
            Card::new(Rank::Nine, Suit::Diamonds),
        ]));
        let mut rng = StdRng::seed_from_u64(1);

        assert!(matches!(
            bot.choose_action(&context, &mut rng),
            BetAction::RaiseTo(_)
        ));
    }

    #[test]
    fn balanced_bot_uses_three_big_blind_open_size() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = six_max_unopened_context(3, &[]).with_hole_cards(Some([
            Card::new(Rank::Ace, Suit::Clubs),
            Card::new(Rank::Ace, Suit::Diamonds),
        ]));
        let mut rng = StdRng::seed_from_u64(1);
        let decision = bot.decide(&context, &mut rng);

        assert_eq!(decision.sizing, SizingFamily::PreflopOpenOrIsolation);
        assert_eq!(decision.motive, BettingMotive::Steal);
        assert_eq!(decision.action, BetAction::RaiseTo(Chips::from_units(6)));
    }

    #[test]
    fn decision_exposes_public_situation_and_preflop_motive() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = six_max_unopened_context(5, &[3, 4]).with_hole_cards(Some([
            Card::new(Rank::King, Suit::Diamonds),
            Card::new(Rank::Queen, Suit::Diamonds),
        ]));
        let mut rng = StdRng::seed_from_u64(1);
        let decision = bot.decide(&context, &mut rng);

        assert_eq!(decision.input.situation.betting_spot, BettingSpot::Unopened);
        assert_eq!(decision.input.situation.position, PositionClass::Late);
        assert_eq!(decision.input.evidence.holding, HoldingClass::Preflop);
        assert_eq!(
            decision.input.evidence.opponent_range,
            OpponentRangeClass::Broad
        );
        assert_eq!(decision.motive, BettingMotive::Steal);
    }

    #[test]
    fn balanced_bot_adds_limpers_to_preflop_isolation_size() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = big_blind_after_limpers_context().with_hole_cards(Some([
            Card::new(Rank::Ace, Suit::Clubs),
            Card::new(Rank::Ace, Suit::Diamonds),
        ]));
        let mut rng = StdRng::seed_from_u64(1);

        assert_eq!(
            bot.choose_action(&context, &mut rng),
            BetAction::RaiseTo(Chips::from_units(14))
        );
    }

    #[test]
    fn balanced_bot_opens_more_heads_up_buttons() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = heads_up_button_context().with_hole_cards(Some([
            Card::new(Rank::Ten, Suit::Hearts),
            Card::new(Rank::Four, Suit::Hearts),
        ]));
        let mut rng = StdRng::seed_from_u64(1);

        assert!(matches!(
            bot.choose_action(&context, &mut rng),
            BetAction::RaiseTo(_)
        ));
    }

    #[test]
    fn balanced_bot_steals_small_blind_with_playable_suited_king() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = unopened_small_blind_context().with_hole_cards(Some([
            Card::new(Rank::King, Suit::Diamonds),
            Card::new(Rank::Eight, Suit::Diamonds),
        ]));
        let mut rng = StdRng::seed_from_u64(1);

        assert!(matches!(
            bot.choose_action(&context, &mut rng),
            BetAction::RaiseTo(_)
        ));
    }

    #[test]
    fn balanced_bot_uses_dealt_count_after_seats_bust() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = heads_up_button_context_at_table(6).with_hole_cards(Some([
            Card::new(Rank::Ten, Suit::Hearts),
            Card::new(Rank::Four, Suit::Hearts),
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
            Card::new(Rank::King, Suit::Spades),
            Card::new(Rank::Nine, Suit::Spades),
        ]));
        let mut rng = StdRng::seed_from_u64(1);

        assert_eq!(bot.choose_action(&context, &mut rng), BetAction::Call);
    }

    #[test]
    fn balanced_bot_three_bets_jacks_against_open() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = full_ring_cold_call_facing_open_context().with_hole_cards(Some([
            Card::new(Rank::Jack, Suit::Spades),
            Card::new(Rank::Jack, Suit::Diamonds),
        ]));
        let mut rng = StdRng::seed_from_u64(1);

        assert!(matches!(
            bot.choose_action(&context, &mut rng),
            BetAction::RaiseTo(_)
        ));
    }

    #[test]
    fn balanced_bot_defends_big_blind_suited_connectors_full_ring() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = full_ring_big_blind_facing_open_context().with_hole_cards(Some([
            Card::new(Rank::Ten, Suit::Spades),
            Card::new(Rank::Nine, Suit::Spades),
        ]));
        let mut rng = StdRng::seed_from_u64(1);

        assert_eq!(bot.choose_action(&context, &mut rng), BetAction::Call);
    }

    #[test]
    fn balanced_bot_still_folds_big_blind_trash_against_open() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = six_max_big_blind_facing_open_context().with_hole_cards(Some([
            Card::new(Rank::Seven, Suit::Clubs),
            Card::new(Rank::Two, Suit::Diamonds),
        ]));
        let mut rng = StdRng::seed_from_u64(1);

        assert_eq!(bot.choose_action(&context, &mut rng), BetAction::Fold);
    }

    #[test]
    fn balanced_bot_still_folds_trash_big_blind_full_ring() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = full_ring_big_blind_facing_open_context().with_hole_cards(Some([
            Card::new(Rank::Seven, Suit::Clubs),
            Card::new(Rank::Two, Suit::Diamonds),
        ]));
        let mut rng = StdRng::seed_from_u64(1);

        assert_eq!(bot.choose_action(&context, &mut rng), BetAction::Fold);
    }

    #[test]
    fn balanced_bot_folds_weak_full_ring_cold_call() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = full_ring_cold_call_facing_open_context().with_hole_cards(Some([
            Card::new(Rank::Queen, Suit::Clubs),
            Card::new(Rank::Seven, Suit::Diamonds),
        ]));
        let mut rng = StdRng::seed_from_u64(1);

        assert_eq!(bot.choose_action(&context, &mut rng), BetAction::Fold);
    }

    #[test]
    fn balanced_bot_folds_dominated_offsuit_broadway_full_ring_cold_call() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = full_ring_cold_call_facing_open_context().with_hole_cards(Some([
            Card::new(Rank::King, Suit::Hearts),
            Card::new(Rank::Queen, Suit::Clubs),
        ]));
        let mut rng = StdRng::seed_from_u64(1);
        let decision = bot.decide(&context, &mut rng);

        assert_eq!(decision.action, BetAction::Fold, "{decision:?}");
        assert_eq!(decision.motive, BettingMotive::GapFold);
    }

    #[test]
    fn balanced_bot_folds_broadway_full_ring_cold_call_against_three_bet() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = full_ring_cold_call_facing_three_bet_context().with_hole_cards(Some([
            Card::new(Rank::Queen, Suit::Clubs),
            Card::new(Rank::Jack, Suit::Hearts),
        ]));
        let mut rng = StdRng::seed_from_u64(1);

        assert_eq!(bot.choose_action(&context, &mut rng), BetAction::Fold);
    }

    #[test]
    fn balanced_bot_folds_offsuit_ace_jack_against_full_ring_three_bet() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = full_ring_big_blind_facing_three_bet_context().with_hole_cards(Some([
            Card::new(Rank::Ace, Suit::Diamonds),
            Card::new(Rank::Jack, Suit::Spades),
        ]));
        let mut rng = StdRng::seed_from_u64(1);
        let decision = bot.decide(&context, &mut rng);

        assert_eq!(decision.action, BetAction::Fold, "{decision:?}");
        assert_eq!(decision.motive, BettingMotive::GapFold);
    }

    #[test]
    fn balanced_bot_folds_weak_offsuit_gap_when_blind_defense_is_not_closing() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = BotDecisionContext::new(
            3,
            Seat::new(0).expect("big blind"),
            Street::Preflop,
            full_ring_big_blind_facing_open_context().legal_actions,
        )
        .with_dealt_player_count(3)
        .with_hole_cards(Some([
            Card::new(Rank::Jack, Suit::Clubs),
            Card::new(Rank::Nine, Suit::Spades),
        ]))
        .with_pot(Chips::from_units(9))
        .with_actor_stack(Chips::from_units(94))
        .with_actor_street_contributed(Chips::from_units(2))
        .with_live_opponents(2)
        .with_matched_bet_opponents(1)
        .with_big_blind(Chips::from_units(2))
        .with_current_bet(Chips::from_units(6))
        .with_street_raise_count(1);
        let mut rng = StdRng::seed_from_u64(1);
        let decision = bot.decide(&context, &mut rng);

        assert_eq!(decision.action, BetAction::Fold, "{decision:?}");
        assert_eq!(decision.motive, BettingMotive::GapFold);
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
            Card::new(Rank::Ace, Suit::Clubs),
            Card::new(Rank::Ace, Suit::Diamonds),
        ]))
        .with_board([
            Card::new(Rank::Ace, Suit::Hearts),
            Card::new(Rank::Seven, Suit::Clubs),
            Card::new(Rank::Two, Suit::Spades),
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
            Card::new(Rank::Ace, Suit::Hearts),
            Card::new(Rank::Five, Suit::Spades),
        ]))
        .with_board([
            Card::new(Rank::Four, Suit::Spades),
            Card::new(Rank::Three, Suit::Spades),
            Card::new(Rank::Eight, Suit::Spades),
        ])
        .with_pot(Chips::from_units(10))
        .with_live_opponents(1);
        let mut rng = StdRng::seed_from_u64(1);

        let decision = bot.decide(&context, &mut rng);
        assert!(matches!(decision.action, BetAction::RaiseTo(_)));
        assert_eq!(
            decision.input.situation.betting_spot,
            BettingSpot::CheckedTo
        );
        assert_eq!(decision.input.evidence.draw, DrawClass::ComboDraw);
        assert!(
            decision
                .input
                .evidence
                .draw_completion_probability
                .is_some_and(|probability| probability >= 0.25),
            "{decision:?}"
        );
        assert_eq!(decision.motive, BettingMotive::SemiBluff);
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
            Card::new(Rank::Ace, Suit::Diamonds),
            Card::new(Rank::Five, Suit::Diamonds),
        ]))
        .with_board([
            Card::new(Rank::Jack, Suit::Hearts),
            Card::new(Rank::Four, Suit::Spades),
            Card::new(Rank::Ace, Suit::Clubs),
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
    fn balanced_bot_c_bets_overcards_with_flop_initiative() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = BotDecisionContext::new(
            2,
            Seat::new(0).expect("seat"),
            Street::Flop,
            open_bet_legal(),
        )
        .with_hole_cards(Some([
            Card::new(Rank::Ace, Suit::Clubs),
            Card::new(Rank::King, Suit::Diamonds),
        ]))
        .with_board([
            Card::new(Rank::Queen, Suit::Hearts),
            Card::new(Rank::Seven, Suit::Clubs),
            Card::new(Rank::Two, Suit::Spades),
        ])
        .with_pot(Chips::from_units(13))
        .with_live_opponents(1)
        .with_postflop_initiative(true);
        let mut rng = StdRng::seed_from_u64(1);

        assert!(matches!(
            bot.choose_action(&context, &mut rng),
            BetAction::RaiseTo(_)
        ));
    }

    #[test]
    fn board_pair_only_is_not_treated_as_a_hole_card_pair() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = BotDecisionContext::new(
            3,
            Seat::new(0).expect("seat"),
            Street::Flop,
            open_bet_legal(),
        )
        .with_hole_cards(Some([
            Card::new(Rank::Ace, Suit::Clubs),
            Card::new(Rank::King, Suit::Diamonds),
        ]))
        .with_board([
            Card::new(Rank::Seven, Suit::Diamonds),
            Card::new(Rank::Seven, Suit::Spades),
            Card::new(Rank::Four, Suit::Clubs),
        ])
        .with_pot(Chips::from_units(46))
        .with_live_opponents(2)
        .with_postflop_initiative(true);
        let mut rng = StdRng::seed_from_u64(1);
        let decision = bot.decide(&context, &mut rng);

        assert_eq!(decision.input.evidence.holding, HoldingClass::BoardPairOnly);
        assert_eq!(decision.action, BetAction::Check, "{decision:?}");
    }

    #[test]
    fn weak_high_card_check_uses_free_card_not_showdown_value() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = BotDecisionContext::new(
            3,
            Seat::new(0).expect("seat"),
            Street::Flop,
            open_bet_legal(),
        )
        .with_hole_cards(Some([
            Card::new(Rank::Ace, Suit::Hearts),
            Card::new(Rank::Two, Suit::Hearts),
        ]))
        .with_board([
            Card::new(Rank::King, Suit::Spades),
            Card::new(Rank::Queen, Suit::Diamonds),
            Card::new(Rank::Nine, Suit::Spades),
        ])
        .with_pot(Chips::from_units(18))
        .with_live_opponents(2);
        let mut rng = StdRng::seed_from_u64(1);
        let decision = bot.decide(&context, &mut rng);

        assert_eq!(decision.action, BetAction::Check, "{decision:?}");
        assert_ne!(decision.motive, BettingMotive::ShowdownValue);
    }

    #[test]
    fn top_pair_weak_kicker_does_not_protection_bet_multiway_three_bet_pot() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = full_ring_ajo_multiway_three_bet_flop_context().with_hole_cards(Some([
            Card::new(Rank::Ace, Suit::Diamonds),
            Card::new(Rank::Jack, Suit::Spades),
        ]));
        let mut rng = StdRng::seed_from_u64(1);
        let decision = bot.decide(&context, &mut rng);

        assert_eq!(
            decision.input.evidence.holding,
            HoldingClass::TopPairWeakKicker
        );
        assert_ne!(decision.motive, BettingMotive::Protection);
        assert_eq!(decision.action, BetAction::Check, "{decision:?}");
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
                Card::new(Rank::Eight, Suit::Diamonds),
                Card::new(Rank::Two, Suit::Diamonds),
                Card::new(Rank::Jack, Suit::Spades),
            ],
            1,
            10,
            0,
        )
        .with_hole_cards(Some([
            Card::new(Rank::Ten, Suit::Spades),
            Card::new(Rank::Ten, Suit::Hearts),
        ]))
        .with_board([
            Card::new(Rank::Eight, Suit::Diamonds),
            Card::new(Rank::Two, Suit::Diamonds),
            Card::new(Rank::Jack, Suit::Spades),
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
                Card::new(Rank::Jack, Suit::Hearts),
                Card::new(Rank::Four, Suit::Spades),
                Card::new(Rank::Ace, Suit::Clubs),
            ],
            1,
            6,
            0,
        )
        .with_hole_cards(Some([
            Card::new(Rank::Ace, Suit::Diamonds),
            Card::new(Rank::Five, Suit::Diamonds),
        ]))
        .with_board([
            Card::new(Rank::Jack, Suit::Hearts),
            Card::new(Rank::Four, Suit::Spades),
            Card::new(Rank::Ace, Suit::Clubs),
        ]);
        let mut rng = StdRng::seed_from_u64(1);
        let decision = bot.decide(&context, &mut rng);

        assert_ne!(decision.action, BetAction::Fold, "{decision:?}");
    }

    #[test]
    fn balanced_bot_raises_trips_for_value_against_bet() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let context = heads_up_facing_flop_bet_context(
            &[
                (0, BetAction::RaiseTo(Chips::from_units(5))),
                (1, BetAction::Call),
            ],
            [
                Card::new(Rank::Five, Suit::Diamonds),
                Card::new(Rank::Ace, Suit::Clubs),
                Card::new(Rank::Ace, Suit::Spades),
            ],
            1,
            7,
            0,
        )
        .with_hole_cards(Some([
            Card::new(Rank::Ace, Suit::Diamonds),
            Card::new(Rank::Queen, Suit::Diamonds),
        ]))
        .with_board([
            Card::new(Rank::Five, Suit::Diamonds),
            Card::new(Rank::Ace, Suit::Clubs),
            Card::new(Rank::Ace, Suit::Spades),
        ]);
        let mut rng = StdRng::seed_from_u64(1);
        let decision = bot.decide(&context, &mut rng);

        assert!(
            matches!(decision.action, BetAction::RaiseTo(_)),
            "{decision:?}"
        );
        assert_eq!(
            decision.input.situation.betting_spot,
            BettingSpot::FacingRaise
        );
        assert_eq!(
            decision.input.evidence.holding,
            HoldingClass::TwoPairOrTrips
        );
        assert_eq!(decision.motive, BettingMotive::Value);
    }

    #[test]
    fn preflop_sizing_isolates_over_limpers() {
        let context = big_blind_after_limpers_context();
        let target =
            sized_raise(&context, SizingFamily::PreflopOpenOrIsolation).expect("raise target");
        assert_eq!(target, Chips::from_units(14));
    }

    #[test]
    fn monte_carlo_equity_separates_strong_hands_from_air() {
        let bot = HeuristicBot::from_preset(BotPreset::Balanced);
        let board = [
            Card::new(Rank::King, Suit::Hearts),
            Card::new(Rank::Seven, Suit::Clubs),
            Card::new(Rank::Two, Suit::Spades),
        ];
        let strong = BotDecisionContext::new(
            3,
            Seat::new(0).expect("seat"),
            Street::Flop,
            open_bet_legal(),
        )
        .with_hole_cards(Some([
            Card::new(Rank::King, Suit::Clubs),
            Card::new(Rank::King, Suit::Diamonds),
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
            Card::new(Rank::Queen, Suit::Diamonds),
            Card::new(Rank::Jack, Suit::Diamonds),
        ]))
        .with_board(board)
        .with_live_opponents(2);

        let strong_equity = bot
            .evaluate(&strong, &mut StdRng::seed_from_u64(11))
            .equity
            .expect("strong equity");
        let weak_equity = bot
            .evaluate(&weak, &mut StdRng::seed_from_u64(11))
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
            Card::new(Rank::Ace, Suit::Clubs),
            Card::new(Rank::Ace, Suit::Diamonds),
        ]))
        .with_board([
            Card::new(Rank::Ace, Suit::Clubs),
            Card::new(Rank::Seven, Suit::Clubs),
            Card::new(Rank::Two, Suit::Spades),
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
