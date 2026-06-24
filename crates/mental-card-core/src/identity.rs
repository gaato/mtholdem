//! Long-lived Ed25519 identities used to authenticate protocol messages.
//!
//! Mental-poker keys are ephemeral and protect card confidentiality.  Identity keys
//! have a different role: they make messages attributable, prevent transcript
//! equivocation, and produce evidence that an external penalty contract can verify.

use std::fmt;

use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use rand::{CryptoRng, RngCore};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{Result, authentication, configuration};
use crate::types::PlayerId;

/// Canonical length of an Ed25519 public key.
pub const IDENTITY_PUBLIC_KEY_BYTES: usize = 32;

/// Canonical length of an Ed25519 signature.
pub const IDENTITY_SIGNATURE_BYTES: usize = 64;

/// A secret Ed25519 identity key.
///
/// The key material is erased on drop by `ed25519-dalek`.  The custom `Debug`
/// implementation never prints secret bytes.
pub struct IdentitySecret {
    key: SigningKey,
}

impl IdentitySecret {
    /// Generates a fresh identity using caller-provided cryptographically secure randomness.
    pub fn generate(rng: &mut (impl RngCore + CryptoRng)) -> Self {
        Self {
            key: SigningKey::generate(rng),
        }
    }

    /// Restores an identity from a 32-byte Ed25519 seed.
    ///
    /// The input is copied into protected key storage and should be erased by the
    /// caller as soon as practical.  Never derive this seed from a password without a
    /// dedicated password-based key derivation function and authenticated storage.
    #[must_use]
    pub fn from_seed(mut seed: [u8; 32]) -> Self {
        let key = SigningKey::from_bytes(&seed);
        seed.zeroize();
        Self { key }
    }

    /// Exports the 32-byte Ed25519 seed for encrypted persistent storage.
    ///
    /// This method is intentionally explicit.  Treat the returned value as a secret;
    /// anyone who obtains it can impersonate the player and create penalty evidence.
    #[must_use]
    pub fn to_seed(&self) -> IdentitySeed {
        IdentitySeed(self.key.to_bytes())
    }

    /// Returns the public half of this identity.
    #[must_use]
    pub fn public(&self) -> IdentityPublic {
        IdentityPublic(self.key.verifying_key())
    }

    /// Signs already domain-separated canonical protocol bytes.
    ///
    /// This primitive intentionally accepts bytes rather than a serializable value.
    /// Higher-level crates must define one unambiguous canonical encoding and include
    /// session, protocol, and message-kind separation before calling it.
    #[must_use]
    pub fn sign(&self, message: &[u8]) -> IdentitySignature {
        IdentitySignature(self.key.sign(message).to_bytes())
    }
}

impl fmt::Debug for IdentitySecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IdentitySecret")
            .field("public", &self.public())
            .field("secret", &"REDACTED")
            .finish()
    }
}

/// A secret seed returned by [`IdentitySecret::to_seed`].
///
/// The bytes are zeroized on drop and omitted from `Debug` output.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct IdentitySeed([u8; 32]);

impl IdentitySeed {
    /// Borrows the seed bytes for immediate encrypted persistence.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Consumes this wrapper and returns a copy of the seed bytes.
    ///
    /// The returned array is no longer automatically zeroized.  Prefer [`as_bytes`](Self::as_bytes)
    /// when the storage API can work with a borrowed slice.
    #[must_use]
    pub fn expose(mut self) -> [u8; 32] {
        let mut bytes = [0_u8; 32];
        bytes.copy_from_slice(&self.0);
        self.0.zeroize();
        bytes
    }
}

impl fmt::Debug for IdentitySeed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("IdentitySeed")
            .field(&"REDACTED")
            .finish()
    }
}

/// A validated Ed25519 public key.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct IdentityPublic(VerifyingKey);

impl IdentityPublic {
    /// Parses and validates canonical Ed25519 public-key bytes.
    pub fn from_bytes(bytes: [u8; IDENTITY_PUBLIC_KEY_BYTES]) -> Result<Self> {
        VerifyingKey::from_bytes(&bytes)
            .map(Self)
            .map_err(|_| configuration("invalid Ed25519 public key"))
    }

    /// Returns canonical public-key bytes.
    #[must_use]
    pub fn to_bytes(self) -> [u8; IDENTITY_PUBLIC_KEY_BYTES] {
        self.0.to_bytes()
    }

    /// Returns the stable player identifier derived from this key.
    #[must_use]
    pub fn player_id(self) -> PlayerId {
        PlayerId::from_identity_key_bytes(&self.to_bytes())
    }

    /// Verifies a signature over already domain-separated canonical protocol bytes.
    ///
    /// The caller must verify the same semantic context that was encoded by the signer;
    /// signature validity alone does not make a message appropriate for a session.
    pub fn verify(self, message: &[u8], signature: IdentitySignature) -> Result<()> {
        let signature = Signature::from_bytes(&signature.0);
        self.0
            .verify_strict(message, &signature)
            .map_err(|_| authentication("Ed25519 signature verification failed"))
    }
}

impl fmt::Debug for IdentityPublic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IdentityPublic")
            .field("player_id", &self.player_id())
            .finish()
    }
}

impl fmt::Display for IdentityPublic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.player_id().fmt(formatter)
    }
}

/// A canonical Ed25519 signature attached to a protocol message.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct IdentitySignature([u8; IDENTITY_SIGNATURE_BYTES]);

impl IdentitySignature {
    /// Creates a signature wrapper from canonical wire bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; IDENTITY_SIGNATURE_BYTES]) -> Self {
        Self(bytes)
    }

    /// Returns canonical wire bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; IDENTITY_SIGNATURE_BYTES] {
        self.0
    }
}

impl fmt::Debug for IdentitySignature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Signatures are not secret, but printing 64 bytes in ordinary debug logs is
        // noisy.  The prefix is sufficient to correlate messages during diagnostics.
        write!(
            formatter,
            "IdentitySignature({:02x}{:02x}{:02x}{:02x}…)",
            self.0[0], self.0[1], self.0[2], self.0[3]
        )
    }
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng as _;
    use rand::rngs::StdRng;

    use super::*;

    #[test]
    fn identity_signatures_verify_and_detect_tampering() {
        let mut rng = StdRng::from_seed([0x44; 32]);
        let secret = IdentitySecret::generate(&mut rng);
        let public = secret.public();
        let signature = secret.sign(b"message");

        public
            .verify(b"message", signature)
            .expect("valid signature");
        assert!(public.verify(b"different", signature).is_err());
    }

    #[test]
    fn debug_output_never_contains_secret_seed() {
        let seed = [0x5a; 32];
        let secret = IdentitySecret::from_seed(seed);
        let rendered = format!("{secret:?}");
        assert!(!rendered.contains("5a5a5a"));
        assert!(rendered.contains("REDACTED"));
    }
}
