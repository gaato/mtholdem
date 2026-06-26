//! Host-side bot policy for `mtholdem` demos and local play.
//!
//! This crate is not part of the dealerless consensus protocol. It chooses legal
//! betting actions for hosts such as the CLI or a future single-player wrapper.
//! Protocol validation, transcript ordering, and settlement-facing state remain
//! in `mtholdem` and `mtholdem-peer`.
//!
//! The policy approximates the fundamental theorem of poker without inspecting
//! hidden opponent cards. Public diagnostics expose the observed
//! [`PokerSituation`], computed [`DecisionEvidence`], and selected
//! [`BettingMotive`] instead of a single scalar answer.

#![forbid(unsafe_code)]

mod context;
mod model;
mod policy;
mod profile;

pub use context::BotDecisionContext;
pub use model::{
    ActionClosure, BettingMotive, BettingSpot, BotDecision, DecisionEvidence, DecisionInput,
    DrawClass, HoldingClass, InitiativeState, OpponentCountClass, OpponentRangeClass,
    PokerSituation, PositionClass, PotOddsClass, PressureClass, SizingFamily,
};
pub use policy::HeuristicBot;
pub use profile::{BotPreset, BotProfile};
