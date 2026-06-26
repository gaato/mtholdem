use mtholdem::HandCategory;
use mtholdem::betting::BetAction;

/// Public-information situation used to approximate the fundamental theorem of poker.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PokerSituation {
    /// Current betting spot.
    pub betting_spot: BettingSpot,
    /// Coarse position class for the acting seat.
    pub position: PositionClass,
    /// Whether the pot is heads-up or multiway.
    pub opponents: OpponentCountClass,
    /// Whether this action probably closes the betting round.
    pub action_closure: ActionClosure,
    /// Whether the actor has betting initiative.
    pub initiative: InitiativeState,
}

/// Coarse public betting state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BettingSpot {
    /// Preflop pot folded to the actor.
    Unopened,
    /// Preflop limped pot.
    Limped,
    /// Big blind can check or isolate limpers.
    BigBlindOption,
    /// Actor is facing a bet or call price.
    FacingBet,
    /// Actor is facing a raise.
    FacingRaise,
    /// Postflop action has checked to the actor.
    CheckedTo,
    /// Public facts are insufficient to classify the spot.
    Unknown,
}

/// Coarse seat position relative to the button.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PositionClass {
    /// Position is not known.
    Unknown,
    /// Blind or posted-blind defense spot.
    Blind,
    /// Early position.
    Early,
    /// Middle position.
    Middle,
    /// Late position.
    Late,
}

/// Coarse live-opponent count.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpponentCountClass {
    /// No opponent can contest the pot.
    None,
    /// One live opponent.
    HeadsUp,
    /// More than one live opponent.
    Multiway,
}

/// Whether the actor is likely closing action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionClosure {
    /// No later actor is expected to reopen action.
    Closing,
    /// Later actors may still act.
    NotClosing,
    /// Public facts are insufficient.
    Unknown,
}

/// Coarse initiative state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitiativeState {
    /// Actor has initiative.
    HasInitiative,
    /// Actor does not have initiative.
    NoInitiative,
    /// Initiative is unknown or not relevant.
    Unknown,
}

/// Situation and evidence visible to the decision policy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecisionInput {
    /// Public situation used by the policy.
    pub situation: PokerSituation,
    /// Public and local-private evidence used by the policy.
    pub evidence: DecisionEvidence,
}

/// Public and local-private evidence used by the policy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecisionEvidence {
    /// Made-hand and showdown-value class.
    pub holding: HoldingClass,
    /// Made-hand category, when the board is complete enough to evaluate.
    pub category: Option<HandCategory>,
    /// Draw class derived from known cards only.
    pub draw: DrawClass,
    /// Probability that the known draw improves by the river or next street.
    pub draw_completion_probability: Option<f32>,
    /// Range-aware Monte Carlo equity estimate, when enough local/public cards are known.
    pub equity: Option<f32>,
    /// Current call price as a fraction of pot after calling.
    pub pot_odds: Option<f32>,
    /// Coarse call-price classification.
    pub pot_odds_class: PotOddsClass,
    /// Coarse opponent range assumption used by simulation.
    pub opponent_range: OpponentRangeClass,
    /// Coarse cost and multiway pressure.
    pub pressure: PressureClass,
}

/// Coarse made-hand and showdown-value class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HoldingClass {
    /// No private hand is known.
    Unknown,
    /// Preflop holding before board evidence exists.
    Preflop,
    /// No meaningful made hand or overcard value.
    Air,
    /// High cards that may improve against the board.
    Overcards,
    /// The best pair is already on the board.
    BoardPairOnly,
    /// A weak pair or underpair.
    WeakPair,
    /// Top pair with a vulnerable kicker.
    TopPairWeakKicker,
    /// Top pair with a good kicker.
    TopPairGoodKicker,
    /// Pocket pair above the board.
    OverPair,
    /// Two pair or trips.
    TwoPairOrTrips,
    /// Straight, flush, or better.
    PremiumMade,
}

/// Coarse draw class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrawClass {
    /// No meaningful draw.
    None,
    /// Overcards only.
    Overcards,
    /// Gutshot or weak straight material.
    Gutshot,
    /// Open-ended straight draw.
    OpenEnded,
    /// Flush draw.
    FlushDraw,
    /// Multiple meaningful draw paths.
    ComboDraw,
}

/// Coarse call-price class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PotOddsClass {
    /// No call price is required.
    NoPrice,
    /// Price is favorable for continuing with draw or showdown value.
    GoodPrice,
    /// Price is close and depends on implied odds or range assumptions.
    Close,
    /// Price is poor without a strong range/equity reason.
    BadPrice,
}

/// Coarse range assumption used for opponent sampling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpponentRangeClass {
    /// Opponent range is broad.
    Broad,
    /// Opponent range is a normal continuing range.
    Continuing,
    /// Opponent range is tight.
    Tight,
}

/// Coarse pressure class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PressureClass {
    /// Low cost or little multiway pressure.
    Low,
    /// Meaningful price or multiway uncertainty.
    Medium,
    /// High cost, raise depth, or multiway pressure.
    High,
}

/// Human-readable reason for the selected action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BettingMotive {
    /// Bet or raise for value.
    Value,
    /// Bet or raise to deny equity.
    Protection,
    /// Bluff with little immediate showdown value.
    Bluff,
    /// Bet or raise a draw with fold equity.
    SemiBluff,
    /// Continue because the call price is justified.
    PotOddsCall,
    /// Defend a blind or playable preflop hand against a raise.
    Defend,
    /// Check or call with showdown value.
    ShowdownValue,
    /// Enter first and try to steal dead money.
    Steal,
    /// Isolate limpers.
    Isolation,
    /// Fold because the hand is too weak to call a raise.
    GapFold,
    /// Fold because price and pressure are too high.
    PressureFold,
    /// Give up without meaningful equity or fold equity.
    GiveUp,
    /// Take the free card or free showdown.
    FreeCard,
    /// Move all-in because stack pressure dominates sizing.
    AllInPressure,
}

/// Chosen sizing family for a bet or raise motive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SizingFamily {
    /// No bet size is needed.
    None,
    /// Use a standard preflop open or isolation size.
    PreflopOpenOrIsolation,
    /// Use a fraction of the current pot.
    PotFraction(f32),
    /// Move all-in.
    AllIn,
}

/// Complete policy decision for diagnostics and tests.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BotDecision {
    /// Situation and evidence used by the policy.
    pub input: DecisionInput,
    /// Human-readable reason for the chosen action.
    pub motive: BettingMotive,
    /// Chosen sizing family.
    pub sizing: SizingFamily,
    /// Final legal action.
    pub action: BetAction,
}
