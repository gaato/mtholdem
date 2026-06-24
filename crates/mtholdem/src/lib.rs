//! Dealerless Sans-I/O protocol and rules for peer-to-peer Texas Hold'em.
//!
//! `mtholdem` is the Hold'em-specific layer of a small Cargo workspace.  The sibling
//! [`mental_card_core`] crate owns reusable standard-card cryptography, participant
//! identities, and per-deal key/shuffle/reveal state.  This crate owns the parts whose
//! validity depends on Texas Hold'em: betting streets, private two-card receipts,
//! showdown, side pots, signed wire payloads, compact checkpoints, and recovery
//! evidence.
//!
//! The crate performs no network, clock, storage, or blockchain I/O.  A host
//! application supplies those facilities and can use the same verified state machines
//! over QUIC, WebRTC, libp2p, a relay, or an in-process test transport.
//!
//! # Security status
//!
//! This is experimental research software.  It has not received an independent
//! cryptographic audit or a machine-checked proof, and it does not automatically
//! inherit the security proofs of Kaleidoscope, Royale, or the card-games-without-
//! secret-state paper.  Do not use it for real-money play without reviewing the
//! distributed `SECURITY.md`, auditing the complete deployment, and implementing a
//! sound external recovery and settlement mechanism.
//!
//! # Crate boundaries
//!
//! - [`mental_card_core`] contains game-independent card identities, signatures,
//!   threshold keys, verifiable shuffles, reveal shares, and
//!   [`mental_card_core::protocol::CardCryptography`].
//! - This crate's [`protocol::HandCryptography`] adds signed private-card delivery and
//!   Hold'em showdown commitments.
//! - [`betting`], [`card`], and [`holdem`] implement deterministic no-limit rules,
//!   hand evaluation, side pots, odd chips, and settlement.
//! - [`wire`] and [`transcript`] define the Hold'em public protocol vocabulary and its
//!   global authenticated order.
//! - [`checkpoint`] and [`recovery`] certify Hold'em state and package evidence for an
//!   external mediator.
//! - The optional sibling crate `mtholdem-testkit` provides deterministic integration
//!   fixtures and is never a production dependency of this crate.
//!
//! The split intentionally stops before attempting a generic checkpoint or generic
//! game-rule trait.  The three papers show that card operations are composable building
//! blocks, but they also make the game-rule program and recovery verifier part of the
//! security statement.  Hiding those semantics behind an unconstrained Rust trait would
//! create false confidence rather than reusable security.
//!
//! # Confidential and public channels
//!
//! [`wire::SignedPublicMessage`] values belong in one authenticated, globally ordered
//! public transcript.  [`wire::SignedPrivateReveal`] values reveal threshold
//! information for a hole card and must be delivered only to their named recipient over
//! an authenticated confidential channel.  At showdown, the recipient may publish the
//! same signed shares in a [`wire::ShowdownBundle`].
//!
//! # Liveness and penalties
//!
//! Every roster member is required to reveal an `(n, n)` threshold-encrypted card.  A
//! folded player must therefore continue contributing board-card shares.  A peer can
//! always halt off-chain execution by withholding a required message.  Liveness comes
//! from an external stateful mediator that issues observable challenges, enforces
//! deadlines, validates this crate's evidence, and distributes collateral.  A local
//! timeout by itself is not cryptographic proof of omission.
//!
//! # Example
//!
//! ```no_run
//! use mental_card_core::identity::IdentitySecret;
//! use mtholdem::config::{EconomicSecurity, Roster, SessionConfig};
//! use mtholdem::{Chips, Result, SessionId};
//! use rand::rngs::OsRng;
//!
//! fn build_session() -> Result<SessionConfig> {
//!     let mut rng = OsRng;
//!     let identities: Vec<_> = (0..3)
//!         .map(|_| IdentitySecret::generate(&mut rng))
//!         .collect();
//!     let roster = Roster::new(
//!         identities.iter().map(IdentitySecret::public).collect(),
//!     )?;
//!     let economics = EconomicSecurity::new(
//!         roster.len(),
//!         Chips::from_units(100),
//!         Chips::from_units(101),
//!         Chips::from_units(202),
//!     )?;
//!
//!     SessionConfig::builder(
//!         SessionId::random(&mut rng),
//!         roster,
//!         Chips::from_units(1),
//!         Chips::from_units(2),
//!         Chips::from_units(100),
//!     )
//!     .economics(economics)
//!     .build()
//! }
//! # build_session()?;
//! # Ok::<(), mtholdem::Error>(())
//! ```

#![forbid(unsafe_code)]

pub mod betting;
pub mod card;
pub mod checkpoint;
mod codec;
pub mod config;
pub mod deal;
pub mod holdem;
pub mod protocol;
pub mod recovery;
pub mod transcript;
pub mod types;
pub mod wire;

#[doc(inline)]
pub use card::{Card, HandCategory, HandValue, Rank, Suit};
#[doc(inline)]
pub use mental_card_core::{Error, ErrorCategory, Result};
#[doc(inline)]
pub use types::{CardPosition, Chips, Digest32, HandNumber, PlayerId, Seat, SessionId};
