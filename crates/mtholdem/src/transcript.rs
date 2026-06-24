//! Verification and storage of the globally ordered public hand transcript.
//!
//! Networking is outside this crate.  A transport delivers candidate messages to
//! every peer; this state machine verifies signatures, session binding, monotonic
//! sequence numbers, and the predecessor hash before making an event visible to the
//! poker engine.  This is a classic sans-I/O boundary: no sockets, timers, or tasks
//! are created internally.

use crate::config::SessionConfig;
use crate::error::{Result, protocol};
use crate::types::{Digest32, HandNumber};
use crate::wire::SignedPublicMessage;

/// Compact point from which a public transcript can be replayed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TranscriptAnchor {
    next_sequence: u64,
    digest: Digest32,
}

impl TranscriptAnchor {
    /// The empty transcript before sequence zero.
    pub const EMPTY: Self = Self {
        next_sequence: 0,
        digest: Digest32::ZERO,
    };

    /// Creates an anchor from a certified checkpoint.
    #[must_use]
    pub const fn new(next_sequence: u64, digest: Digest32) -> Self {
        Self {
            next_sequence,
            digest,
        }
    }

    /// Returns the sequence expected for the next appended event.
    #[must_use]
    pub const fn next_sequence(self) -> u64 {
        self.next_sequence
    }

    /// Returns the digest that the next event must name as its predecessor.
    #[must_use]
    pub const fn digest(self) -> Digest32 {
        self.digest
    }
}

/// Verified public transcript for one hand.
#[derive(Clone, Debug)]
pub struct PublicTranscript {
    config: SessionConfig,
    hand: HandNumber,
    anchor: TranscriptAnchor,
    messages: Vec<SignedPublicMessage>,
}

impl PublicTranscript {
    /// Creates an empty hand transcript.
    #[must_use]
    pub fn new(config: SessionConfig, hand: HandNumber) -> Self {
        Self {
            config,
            hand,
            anchor: TranscriptAnchor::EMPTY,
            messages: Vec::new(),
        }
    }

    /// Creates a replay transcript beginning at a fully verified checkpoint anchor.
    ///
    /// This constructor does not verify a checkpoint certificate.  Obtain the anchor
    /// from the checkpoint module only after certificate verification.
    pub(crate) fn from_certified_anchor(
        config: SessionConfig,
        hand: HandNumber,
        anchor: TranscriptAnchor,
    ) -> Self {
        Self {
            config,
            hand,
            anchor,
            messages: Vec::new(),
        }
    }

    /// Verifies and appends one public event.
    ///
    /// No semantic poker transition is applied here.  Callers should append only
    /// after (or atomically with) validating the payload against their protocol phase.
    pub fn append(&mut self, message: SignedPublicMessage) -> Result<()> {
        if message.session_id() != self.config.session_id() {
            return Err(protocol("public message belongs to another session"));
        }
        if message.config_digest() != self.config.digest() {
            return Err(protocol("public message commits to another configuration"));
        }
        if message.hand() != self.hand {
            return Err(protocol("public message belongs to another hand"));
        }
        if message.sequence() != self.anchor.next_sequence {
            return Err(protocol(format!(
                "public message sequence {} does not match expected {}",
                message.sequence(),
                self.anchor.next_sequence
            )));
        }
        if message.previous_digest() != self.anchor.digest {
            return Err(protocol("public message predecessor digest does not match"));
        }
        message.verify_signature(self.config.roster())?;
        message.validate_declared_structure(self.config.roster())?;
        let digest = message.digest()?;
        let next_sequence = self
            .anchor
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| protocol("public transcript sequence exhausted"))?;
        self.anchor = TranscriptAnchor::new(next_sequence, digest);
        self.messages.push(message);
        Ok(())
    }

    /// Returns the immutable session configuration.
    #[must_use]
    pub const fn config(&self) -> &SessionConfig {
        &self.config
    }

    /// Returns the hand number.
    #[must_use]
    pub const fn hand(&self) -> HandNumber {
        self.hand
    }

    /// Returns the current compact transcript anchor.
    #[must_use]
    pub const fn anchor(&self) -> TranscriptAnchor {
        self.anchor
    }

    /// Returns messages stored after construction or checkpoint restoration.
    #[must_use]
    pub fn messages(&self) -> &[SignedPublicMessage] {
        &self.messages
    }

    /// Removes stored messages while retaining the certified chain tip.
    ///
    /// Applications may call this after persisting a checkpoint certificate.  The
    /// digest and next sequence remain, so replay protection is not weakened.
    pub fn prune_certified_history(&mut self) {
        self.messages.clear();
    }
}

/// Verifies a contiguous message suffix from an existing anchor.
///
/// This helper is used by external recovery mediators and by peers restoring from a
/// checkpoint.  The returned anchor is the tip after all messages.
pub fn verify_suffix(
    config: &SessionConfig,
    hand: HandNumber,
    anchor: TranscriptAnchor,
    messages: &[SignedPublicMessage],
) -> Result<TranscriptAnchor> {
    let mut transcript = PublicTranscript::from_certified_anchor(config.clone(), hand, anchor);
    for message in messages {
        transcript.append(message.clone())?;
    }
    Ok(transcript.anchor())
}

#[cfg(test)]
mod tests {
    use rand::rngs::OsRng;

    use super::*;
    use crate::config::{EconomicSecurity, Roster};
    use crate::identity::IdentitySecret;
    use crate::types::{Chips, Seat, SessionId};
    use crate::wire::{HandPhase, PublicPayload};

    fn setup() -> (SessionConfig, IdentitySecret, IdentitySecret) {
        let mut rng = OsRng;
        let first = IdentitySecret::generate(&mut rng);
        let second = IdentitySecret::generate(&mut rng);
        let roster = Roster::new(vec![first.public(), second.public()]).unwrap();
        let economics = EconomicSecurity::new(
            2,
            Chips::from_units(100),
            Chips::from_units(101),
            Chips::from_units(101),
        )
        .unwrap();
        let config = SessionConfig::builder(
            SessionId::random(&mut rng),
            roster,
            Chips::from_units(1),
            Chips::from_units(2),
            Chips::from_units(100),
        )
        .economics(economics)
        .build()
        .unwrap();
        (config, first, second)
    }

    #[test]
    fn enforces_sequence_and_hash_chain() {
        let (config, first, second) = setup();
        let mut transcript = PublicTranscript::new(config.clone(), HandNumber::FIRST);
        let one = SignedPublicMessage::sign(
            config.session_id(),
            config.digest(),
            HandNumber::FIRST,
            0,
            HandPhase::Showdown,
            config.roster().participant(Seat::new(0).unwrap()).unwrap(),
            Digest32::ZERO,
            PublicPayload::Muck,
            &first,
        )
        .unwrap();
        let one_digest = one.digest().unwrap();
        transcript.append(one).unwrap();
        let two = SignedPublicMessage::sign(
            config.session_id(),
            config.digest(),
            HandNumber::FIRST,
            1,
            HandPhase::Showdown,
            config.roster().participant(Seat::new(1).unwrap()).unwrap(),
            one_digest,
            PublicPayload::Muck,
            &second,
        )
        .unwrap();
        transcript.append(two).unwrap();
        assert_eq!(transcript.anchor().next_sequence(), 2);
    }
}
