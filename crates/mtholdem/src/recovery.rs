//! Portable evidence for an external recovery or penalty mediator.
//!
//! A blockchain contract is deliberately not embedded in this crate: chains differ
//! in signature precompiles, storage costs, timeout semantics, and asset models.
//! Instead, the library emits canonical certificates and transcript suffixes that a
//! chain adapter can replay.  Message omission cannot be proven from signed bytes
//! alone; a mediator must issue its own challenge and apply its own deadline.

use crate::checkpoint::{CertifiedCheckpoint, MAX_CHECKPOINT_CERTIFICATE_BYTES};
use crate::codec::{Decoder, Encoder};
use crate::config::SessionConfig;
use crate::error::{Result, codec, recovery};
use crate::transcript::{TranscriptAnchor, verify_suffix};
use crate::types::{Digest32, PlayerId};
use crate::wire::{MAX_PUBLIC_MESSAGE_BYTES, SignedPublicMessage};

const BUNDLE_MAGIC: [u8; 4] = *b"MTHB";
const EQUIVOCATION_MAGIC: [u8; 4] = *b"MTHE";
const RECOVERY_VERSION: u16 = 1;

/// Maximum encoded size of a recovery bundle.
pub const MAX_RECOVERY_BUNDLE_BYTES: usize = 8 * 1024 * 1024;
/// Maximum number of public messages carried after one checkpoint.
pub const MAX_RECOVERY_MESSAGES: usize = 16_384;

/// Latest unanimous checkpoint plus all public events generated after it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryBundle {
    checkpoint: CertifiedCheckpoint,
    messages: Vec<SignedPublicMessage>,
}

impl RecoveryBundle {
    /// Creates a recovery bundle without verifying it.
    ///
    /// Call [`verify`](Self::verify) before submitting externally supplied content to
    /// a mediator or using its returned chain tip.
    #[must_use]
    pub const fn new(checkpoint: CertifiedCheckpoint, messages: Vec<SignedPublicMessage>) -> Self {
        Self {
            checkpoint,
            messages,
        }
    }

    /// Parses one canonical recovery bundle.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut decoder = Decoder::new(bytes, MAX_RECOVERY_BUNDLE_BYTES, "recovery bundle")?;
        let magic = decoder.fixed::<4>("recovery bundle magic")?;
        if magic != BUNDLE_MAGIC {
            return Err(codec("recovery bundle has the wrong magic"));
        }
        let version = decoder.u16("recovery bundle version")?;
        if version != RECOVERY_VERSION {
            return Err(codec(format!(
                "unsupported recovery bundle version {version}"
            )));
        }
        let checkpoint = CertifiedCheckpoint::from_bytes(
            decoder.length_prefixed(MAX_CHECKPOINT_CERTIFICATE_BYTES, "checkpoint certificate")?,
        )?;
        let count_u32 = decoder.u32("recovery message count")?;
        let count = usize::try_from(count_u32)
            .map_err(|_| codec("recovery message count does not fit this platform"))?;
        if count > MAX_RECOVERY_MESSAGES {
            return Err(codec("recovery bundle contains too many messages"));
        }
        let mut messages = Vec::with_capacity(count);
        for _ in 0..count {
            messages.push(SignedPublicMessage::from_bytes(decoder.length_prefixed(
                MAX_PUBLIC_MESSAGE_BYTES,
                "public recovery message",
            )?)?);
        }
        decoder.finish("recovery bundle")?;
        Ok(Self {
            checkpoint,
            messages,
        })
    }

    /// Verifies the checkpoint and the complete hash-chained suffix.
    pub fn verify(&self, config: &SessionConfig) -> Result<TranscriptAnchor> {
        self.checkpoint.verify(config)?;
        verify_suffix(
            config,
            self.checkpoint.state().hand(),
            self.checkpoint.transcript_anchor(),
            &self.messages,
        )
    }

    /// Encodes the bundle for persistence or contract submission.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        if self.messages.len() > MAX_RECOVERY_MESSAGES {
            return Err(codec("recovery bundle contains too many messages"));
        }
        let checkpoint = self.checkpoint.to_bytes()?;
        let mut encoder = Encoder::with_capacity(checkpoint.len() + self.messages.len() * 512 + 16);
        encoder.fixed(&BUNDLE_MAGIC);
        encoder.u16(RECOVERY_VERSION);
        encoder.length_prefixed(&checkpoint)?;
        encoder.u32(
            u32::try_from(self.messages.len())
                .map_err(|_| codec("recovery message count does not fit u32"))?,
        );
        for message in &self.messages {
            encoder.length_prefixed(&message.to_bytes()?)?;
        }
        let bytes = encoder.into_bytes();
        if bytes.len() > MAX_RECOVERY_BUNDLE_BYTES {
            return Err(codec("encoded recovery bundle exceeds size limit"));
        }
        Ok(bytes)
    }

    /// Returns the unanimous starting checkpoint.
    #[must_use]
    pub const fn checkpoint(&self) -> &CertifiedCheckpoint {
        &self.checkpoint
    }

    /// Returns the public transcript suffix.
    #[must_use]
    pub fn messages(&self) -> &[SignedPublicMessage] {
        &self.messages
    }

    /// Returns a digest for logging or a small on-chain reference.
    pub fn digest(&self) -> Result<Digest32> {
        Ok(Digest32::hash(b"recovery-bundle/v1", &[&self.to_bytes()?]))
    }
}

/// Two valid signatures proving that one player forked a public transcript position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EquivocationEvidence {
    first: SignedPublicMessage,
    second: SignedPublicMessage,
}

impl EquivocationEvidence {
    /// Verifies and constructs directly attributable equivocation evidence.
    pub fn new(
        first: SignedPublicMessage,
        second: SignedPublicMessage,
        config: &SessionConfig,
    ) -> Result<Self> {
        first.verify_signature(config.roster())?;
        second.verify_signature(config.roster())?;
        if first.session_id() != config.session_id()
            || second.session_id() != config.session_id()
            || first.config_digest() != config.digest()
            || second.config_digest() != config.digest()
        {
            return Err(recovery("equivocation messages belong to another session"));
        }
        if first.hand() != second.hand()
            || first.sequence() != second.sequence()
            || first.sender() != second.sender()
            || first.sender_id() != second.sender_id()
            || first.previous_digest() != second.previous_digest()
        {
            return Err(recovery(
                "messages do not occupy the same sender transcript position",
            ));
        }
        if first.to_bytes()? == second.to_bytes()? {
            return Err(recovery("identical messages are not equivocation"));
        }
        Ok(Self { first, second })
    }

    /// Parses and validates canonical evidence.
    pub fn from_bytes(bytes: &[u8], config: &SessionConfig) -> Result<Self> {
        let mut decoder = Decoder::new(
            bytes,
            MAX_PUBLIC_MESSAGE_BYTES * 2 + 32,
            "equivocation evidence",
        )?;
        let magic = decoder.fixed::<4>("equivocation evidence magic")?;
        if magic != EQUIVOCATION_MAGIC {
            return Err(codec("equivocation evidence has the wrong magic"));
        }
        let version = decoder.u16("equivocation evidence version")?;
        if version != RECOVERY_VERSION {
            return Err(codec(format!(
                "unsupported equivocation evidence version {version}"
            )));
        }
        let first = SignedPublicMessage::from_bytes(
            decoder.length_prefixed(MAX_PUBLIC_MESSAGE_BYTES, "first public message")?,
        )?;
        let second = SignedPublicMessage::from_bytes(
            decoder.length_prefixed(MAX_PUBLIC_MESSAGE_BYTES, "second public message")?,
        )?;
        decoder.finish("equivocation evidence")?;
        Self::new(first, second, config)
    }

    /// Encodes both complete signed messages.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut encoder = Encoder::with_capacity(2_048);
        encoder.fixed(&EQUIVOCATION_MAGIC);
        encoder.u16(RECOVERY_VERSION);
        encoder.length_prefixed(&self.first.to_bytes()?)?;
        encoder.length_prefixed(&self.second.to_bytes()?)?;
        Ok(encoder.into_bytes())
    }

    /// Returns the cryptographically attributable player identifier.
    #[must_use]
    pub const fn culprit(&self) -> PlayerId {
        self.first.sender_id()
    }

    /// Returns the first fork branch.
    #[must_use]
    pub const fn first(&self) -> &SignedPublicMessage {
        &self.first
    }

    /// Returns the conflicting fork branch.
    #[must_use]
    pub const fn second(&self) -> &SignedPublicMessage {
        &self.second
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rand::rngs::OsRng;

    use super::*;
    use crate::checkpoint::{CheckpointState, CheckpointVote};
    use crate::config::{EconomicSecurity, Roster};
    use crate::identity::IdentitySecret;
    use crate::types::{Chips, HandNumber, Seat, SessionId};
    use crate::wire::{HandPhase, PublicPayload};

    fn setup() -> (SessionConfig, Vec<IdentitySecret>, CertifiedCheckpoint) {
        let mut rng = OsRng;
        let identities: Vec<_> = (0..2).map(|_| IdentitySecret::generate(&mut rng)).collect();
        let roster = Roster::new(identities.iter().map(IdentitySecret::public).collect()).unwrap();
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
        let receipts = crate::checkpoint::HoleCardReceipts::empty(&config);
        let state = CheckpointState::capture(
            &config,
            HandNumber::FIRST,
            HandPhase::Showdown,
            TranscriptAnchor::EMPTY,
            None,
            &[],
            None,
            &receipts,
            None,
            Digest32::ZERO,
        )
        .unwrap();
        let votes = identities
            .iter()
            .enumerate()
            .map(|(index, identity)| {
                CheckpointVote::sign(&state, Seat::new(index).unwrap(), &config, identity).unwrap()
            })
            .collect();
        let checkpoint = CertifiedCheckpoint::assemble(state, votes, &config).unwrap();
        (config, identities, checkpoint)
    }

    #[test]
    fn equivocation_evidence_is_attributable_and_canonical() {
        let (config, identities, _) = setup();
        let participant = config
            .roster()
            .participant(Seat::new(0).expect("seat"))
            .expect("participant");
        let first = SignedPublicMessage::sign(
            config.session_id(),
            config.digest(),
            HandNumber::FIRST,
            7,
            HandPhase::Showdown,
            participant,
            Digest32::hash(b"fork-parent", &[]),
            PublicPayload::Muck,
            &identities[0],
        )
        .expect("first branch");
        let second = SignedPublicMessage::sign(
            config.session_id(),
            config.digest(),
            HandNumber::FIRST,
            7,
            HandPhase::Showdown,
            participant,
            Digest32::hash(b"fork-parent", &[]),
            PublicPayload::ShowdownOpen {
                bundle_digest: Digest32::hash(b"showdown-bundle", &[]),
            },
            &identities[0],
        )
        .expect("second branch");
        let evidence = EquivocationEvidence::new(first.clone(), second, &config)
            .expect("equivocation evidence");
        assert_eq!(evidence.culprit(), participant.player_id());
        let decoded = EquivocationEvidence::from_bytes(&evidence.to_bytes().unwrap(), &config)
            .expect("decode evidence");
        assert_eq!(decoded.culprit(), participant.player_id());
        assert!(EquivocationEvidence::new(first.clone(), first, &config).is_err());
    }

    #[test]
    fn recovery_suffix_round_trips() {
        let (config, identities, checkpoint) = setup();
        let message = SignedPublicMessage::sign(
            config.session_id(),
            config.digest(),
            HandNumber::FIRST,
            0,
            HandPhase::Showdown,
            config.roster().participant(Seat::new(0).unwrap()).unwrap(),
            Digest32::ZERO,
            PublicPayload::Muck,
            &identities[0],
        )
        .unwrap();
        let bundle = RecoveryBundle::new(checkpoint, vec![message]);
        let tip = bundle.verify(&config).unwrap();
        assert_eq!(tip.next_sequence(), 1);
        let decoded = RecoveryBundle::from_bytes(&bundle.to_bytes().unwrap()).unwrap();
        assert_eq!(decoded.verify(&config).unwrap(), tip);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn bounded_recovery_decoder_never_panics_on_untrusted_bytes(
            bytes in prop::collection::vec(any::<u8>(), 0..8_192),
        ) {
            let _bundle = RecoveryBundle::from_bytes(&bytes);
        }
    }
}
