//! Cryptographic card operations independent of any particular game rules.
//!
//! `mental-card-core` is the reusable lower layer of the `mtholdem` workspace.  It
//! provides standard 52-card identities, participant rosters, long-lived signing
//! identities, typed threshold-ElGamal card operations, and the common per-deal key /
//! shuffle / reveal state machine.
//!
//! The crate intentionally does **not** define betting actions, Texas Hold'em streets,
//! payout rules, wire messages, checkpoints, or recovery policy.  Those concepts are
//! not honestly generic: their validity depends on a game's state machine and on what
//! an external mediator can verify.  Game crates should hash all immutable rules into a
//! [`session::SessionBinding`] and build their own signed protocol vocabulary above
//! [`protocol::CardCryptography`].
//!
//! # Security status
//!
//! This is experimental research software.  The implementation has no independent
//! cryptographic audit or machine-checked proof, and the proofs in the cited mental-card
//! papers do not automatically transfer to this Rust code.
//!
//! # Typical flow
//!
//! ```no_run
//! use mental_card_core::identity::IdentitySecret;
//! use mental_card_core::protocol::{CardCryptography, RevealTarget};
//! use mental_card_core::session::{Roster, SessionBinding};
//! use mental_card_core::{CardPosition, DealNumber, Digest32, Result, Seat, SessionId};
//! use rand::rngs::OsRng;
//!
//! fn create_round() -> Result<()> {
//!     let mut rng = OsRng;
//!     let identities: Vec<_> = (0..3)
//!         .map(|_| IdentitySecret::generate(&mut rng))
//!         .collect();
//!     let roster = Roster::new(
//!         identities.iter().map(IdentitySecret::public).collect(),
//!     )?;
//!     let binding = SessionBinding::new(
//!         SessionId::random(&mut rng),
//!         roster,
//!         Digest32::hash(b"example-game-rules", &[]),
//!     );
//!     let mut cards = CardCryptography::new(binding, DealNumber::FIRST);
//!
//!     let keys: Vec<_> = (0..3)
//!         .map(|index| cards.generate_key_share(&mut rng, Seat::new(index)?))
//!         .collect::<Result<_>>()?;
//!     for (index, key) in keys.iter().enumerate() {
//!         cards.register_key(Seat::new(index)?, key.public(), key.proof())?;
//!     }
//!     cards.finalize_key_setup()?;
//!     for index in 0..3 {
//!         let seat = Seat::new(index)?;
//!         let shuffle = cards.create_shuffle(&mut rng, seat)?;
//!         cards.accept_shuffle(seat, shuffle)?;
//!     }
//!
//!     let position = CardPosition::new(0)?;
//!     let shares = keys
//!         .iter()
//!         .enumerate()
//!         .map(|(index, key)| {
//!             let seat = Seat::new(index)?;
//!             let (share, proof) = cards.create_reveal_share(
//!                 &mut rng,
//!                 seat,
//!                 RevealTarget::Public,
//!                 position,
//!                 key.secret(),
//!             )?;
//!             Ok((seat, share, proof))
//!         })
//!         .collect::<Result<Vec<_>>>()?;
//!     let _card = cards.open_card(RevealTarget::Public, position, &shares)?;
//!     Ok(())
//! }
//! # create_round()?;
//! # Ok::<(), mental_card_core::Error>(())
//! ```

#![forbid(unsafe_code)]

pub mod card;
pub mod crypto;
pub mod error;
pub mod identity;
pub mod protocol;
pub mod session;
pub mod types;

#[doc(inline)]
pub use card::{Card, Rank, Suit};
#[doc(inline)]
pub use error::{Error, ErrorCategory, Result};
#[doc(inline)]
pub use session::{Participant, Roster, SessionBinding};
#[doc(inline)]
pub use types::{CardPosition, DealNumber, Digest32, PlayerId, Seat, SessionId};

/// Workspace-private compatibility hooks used by the `mtholdem` sibling crate.
///
/// This module is public only because Rust has no friend-crate visibility.  Its items
/// are not a stable downstream API and deliberately do not appear in generated docs.
#[doc(hidden)]
pub mod __private {
    use crate::crypto::{AggregateMentalKey, VerifiedDeck};
    use crate::error::Result;

    /// Reconstructs a verified deck after an external unanimous certificate was checked.
    ///
    /// The caller must first validate that `bytes`, the exact aggregate-key digest, and
    /// participant count were included in a unanimously signed checkpoint.  Ordinary
    /// applications should use the game crate's certified-checkpoint API instead.
    pub fn restore_verified_deck_after_certification(
        bytes: &[u8],
        aggregate_key: &AggregateMentalKey,
    ) -> Result<VerifiedDeck> {
        let count = u8::try_from(aggregate_key.participant_count())
            .expect("the core roster limit fits in u8");
        VerifiedDeck::restore_from_certified_bytes(bytes, aggregate_key.digest(), count)
    }
}
