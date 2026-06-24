//! WASM-facing helpers for `mtholdem`.
//!
//! This crate intentionally exposes dependency-free Rust functions first. A downstream
//! JavaScript package can add `wasm-bindgen` or another binding generator without
//! changing the protocol boundary.

#![forbid(unsafe_code)]

use std::collections::VecDeque;

use mental_card_core::error::{Result, configuration};
use mental_card_core::identity::{IDENTITY_PUBLIC_KEY_BYTES, IdentityPublic, IdentitySecret};
use mtholdem::config::{EconomicSecurity, Roster, SessionConfig};
use mtholdem::{Chips, HandNumber, Seat, SessionId};
use mtholdem_adapter::{
    ABI_VERSION_MAJOR, ABI_VERSION_MINOR, ABI_VERSION_PATCH, BoundaryAction, BoundaryEffect,
    BoundaryPeerStatus, decode_certified_checkpoint, decode_showdown_bundle,
    encode_checkpoint_state, encode_effects, encode_recovery_bundle,
};
use mtholdem_peer::PeerSession;
use rand::rngs::OsRng;

/// Returns the major host-boundary ABI version.
#[must_use]
pub const fn abi_version_major() -> u32 {
    ABI_VERSION_MAJOR
}

/// Returns the minor host-boundary ABI version.
#[must_use]
pub const fn abi_version_minor() -> u32 {
    ABI_VERSION_MINOR
}

/// Returns the patch host-boundary ABI version.
#[must_use]
pub const fn abi_version_patch() -> u32 {
    ABI_VERSION_PATCH
}

/// Returns the packed `major.minor.patch` host-boundary ABI version.
#[must_use]
pub const fn abi_version_tuple() -> [u32; 3] {
    [ABI_VERSION_MAJOR, ABI_VERSION_MINOR, ABI_VERSION_PATCH]
}

/// Primitive session configuration for WASM package builders.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WasmSessionConfig {
    session_id: [u8; 32],
    roster_public_keys: Vec<[u8; IDENTITY_PUBLIC_KEY_BYTES]>,
    small_blind_units: u64,
    big_blind_units: u64,
    buy_in_units: u64,
    maximum_hand_loss_units: u64,
    fault_compensation_units: u64,
    security_deposit_units: u64,
    timeout_millis: u64,
}

impl WasmSessionConfig {
    /// Creates a primitive WASM-facing session configuration.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        session_id: [u8; 32],
        roster_public_keys: Vec<[u8; IDENTITY_PUBLIC_KEY_BYTES]>,
        small_blind_units: u64,
        big_blind_units: u64,
        buy_in_units: u64,
        maximum_hand_loss_units: u64,
        fault_compensation_units: u64,
        security_deposit_units: u64,
        timeout_millis: u64,
    ) -> Self {
        Self {
            session_id,
            roster_public_keys,
            small_blind_units,
            big_blind_units,
            buy_in_units,
            maximum_hand_loss_units,
            fault_compensation_units,
            security_deposit_units,
            timeout_millis,
        }
    }

    fn into_session_config(self) -> Result<SessionConfig> {
        let identities = self
            .roster_public_keys
            .into_iter()
            .map(IdentityPublic::from_bytes)
            .collect::<Result<Vec<_>>>()?;
        let roster = Roster::new(identities)?;
        let economics = EconomicSecurity::new(
            roster.len(),
            Chips::from_units(self.maximum_hand_loss_units),
            Chips::from_units(self.fault_compensation_units),
            Chips::from_units(self.security_deposit_units),
        )?;
        SessionConfig::builder(
            SessionId::from_bytes(self.session_id)?,
            roster,
            Chips::from_units(self.small_blind_units),
            Chips::from_units(self.big_blind_units),
            Chips::from_units(self.buy_in_units),
        )
        .economics(economics)
        .timeout_millis(self.timeout_millis)
        .build()
    }
}

/// Safe WASM-facing peer-session wrapper.
pub struct WasmPeerSession {
    session: PeerSession,
    rng: OsRng,
    pending_effects: VecDeque<BoundaryEffect>,
}

impl WasmPeerSession {
    /// Creates a new peer session from primitive WASM package values.
    pub fn new(
        config: WasmSessionConfig,
        local_identity_seed: [u8; 32],
        local_seat: u8,
        hand: u64,
    ) -> Result<Self> {
        let session = PeerSession::new(
            config.into_session_config()?,
            IdentitySecret::from_seed(local_identity_seed),
            Seat::from_u8(local_seat)?,
            HandNumber::from_u64(hand),
        )?;
        Ok(Self {
            session,
            rng: OsRng,
            pending_effects: VecDeque::new(),
        })
    }

    /// Returns read-only peer status.
    #[must_use]
    pub fn status(&self) -> BoundaryPeerStatus {
        self.session.status().into()
    }

    /// Advances local progress and queues resulting boundary effects.
    pub fn progress(&mut self) -> Result<()> {
        self.session.progress(&mut self.rng)
    }

    /// Applies canonical public-message bytes.
    pub fn apply_public(&mut self, bytes: &[u8]) -> Result<()> {
        self.session.apply_public_bytes(bytes, &mut self.rng)
    }

    /// Applies canonical private-reveal bytes.
    pub fn apply_private(&mut self, bytes: &[u8]) -> Result<()> {
        self.session.apply_private_bytes(bytes)
    }

    /// Applies canonical certified-checkpoint bytes.
    pub fn apply_checkpoint(&mut self, bytes: &[u8]) -> Result<()> {
        self.session
            .apply_certified_checkpoint(decode_certified_checkpoint(bytes)?)
    }

    /// Announces canonical certified-checkpoint bytes.
    pub fn announce_checkpoint(&mut self, bytes: &[u8]) -> Result<()> {
        self.session
            .announce_checkpoint(decode_certified_checkpoint(bytes)?)
    }

    /// Applies canonical showdown-bundle bytes.
    pub fn apply_showdown(&mut self, bytes: &[u8]) -> Result<()> {
        self.session
            .apply_showdown_bundle(decode_showdown_bundle(bytes)?)
    }

    /// Submits one local betting action.
    pub fn submit_action(&mut self, action: BoundaryAction) -> Result<()> {
        self.session.submit_action(action.to_bet_action())
    }

    /// Captures canonical checkpoint-state bytes.
    pub fn checkpoint_state(&self) -> Result<Vec<u8>> {
        encode_checkpoint_state(&self.session.checkpoint_state()?)
    }

    /// Builds canonical recovery-bundle bytes.
    pub fn recovery_bundle(&self) -> Result<Vec<u8>> {
        encode_recovery_bundle(&self.session.recovery_bundle()?)
    }

    /// Requests recovery and queues resulting boundary effects.
    pub fn request_recovery(&mut self) -> Result<()> {
        self.session.request_recovery()
    }

    /// Pops the next queued boundary effect.
    pub fn next_effect(&mut self) -> Result<Option<BoundaryEffect>> {
        if self.pending_effects.is_empty() {
            self.pending_effects
                .extend(encode_effects(self.session.drain_effects())?);
        }
        Ok(self.pending_effects.pop_front())
    }
}

/// Parses public keys from concatenated 32-byte values.
pub fn parse_roster_public_keys(bytes: &[u8]) -> Result<Vec<[u8; IDENTITY_PUBLIC_KEY_BYTES]>> {
    if !bytes.len().is_multiple_of(IDENTITY_PUBLIC_KEY_BYTES) {
        return Err(configuration(
            "roster public keys must be concatenated 32-byte Ed25519 keys",
        ));
    }
    Ok(bytes
        .chunks_exact(IDENTITY_PUBLIC_KEY_BYTES)
        .map(|chunk| {
            let mut key = [0_u8; IDENTITY_PUBLIC_KEY_BYTES];
            key.copy_from_slice(chunk);
            key
        })
        .collect())
}

impl std::fmt::Debug for WasmPeerSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WasmPeerSession")
            .field("status", &self.status())
            .field("pending_effect_count", &self.pending_effects.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_misaligned_public_key_bytes() {
        let error = parse_roster_public_keys(&[0; 31]).expect_err("invalid length");

        assert!(error.is_configuration());
    }
}
