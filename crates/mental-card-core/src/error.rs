//! Shared error reporting for mental-card and game-specific crates.
//!
//! The crate intentionally exposes one future-proof error struct instead of a public
//! enum.  Callers can use the `is_*` classification helpers for recovery decisions
//! without becoming coupled to every internal failure mode.

use std::backtrace::Backtrace;
use std::fmt;

/// A convenient result alias shared by the workspace crates.
pub type Result<T> = std::result::Result<T, Error>;

/// The broad category of an [`Error`].
///
/// This enum is deliberately non-exhaustive.  It is suitable for logging and broad
/// policy decisions; application code should not assume that it captures every
/// implementation detail of a failure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum ErrorCategory {
    /// A caller supplied an internally inconsistent table or session configuration.
    Configuration,
    /// A byte sequence was malformed, non-canonical, truncated, or too large.
    Codec,
    /// A digital signature, sender identity, or replay check failed.
    Authentication,
    /// A cryptographic proof, encrypted deck, or decryption share was invalid.
    Cryptography,
    /// A game action or accounting transition violated the applicable rules.
    Rules,
    /// A message was valid in isolation but unexpected in the current protocol phase.
    Protocol,
    /// A checkpoint or recovery bundle was invalid or insufficient.
    Recovery,
}

/// An error returned by the library.
///
/// `Error` stores a human-readable explanation and a lazily enabled backtrace.  Secret
/// keys, private cards, reveal shares, and raw signed messages are never included in the
/// formatted representation.
pub struct Error {
    category: ErrorCategory,
    message: String,
    backtrace: Backtrace,
}

impl Error {
    pub(crate) fn new(category: ErrorCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
            backtrace: Backtrace::capture(),
        }
    }

    /// Returns the broad category of this error.
    #[must_use]
    pub const fn category(&self) -> ErrorCategory {
        self.category
    }

    /// Returns the stable, human-readable summary.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the captured diagnostic backtrace.
    ///
    /// Backtraces are intentionally omitted from [`Display`](fmt::Display) so user-facing
    /// error text remains concise and does not unexpectedly expose process paths.
    pub const fn backtrace(&self) -> &Backtrace {
        &self.backtrace
    }

    /// Returns `true` for invalid configuration or constructor arguments.
    #[must_use]
    pub const fn is_configuration(&self) -> bool {
        matches!(self.category, ErrorCategory::Configuration)
    }

    /// Returns `true` for malformed or non-canonical wire data.
    #[must_use]
    pub const fn is_codec(&self) -> bool {
        matches!(self.category, ErrorCategory::Codec)
    }

    /// Returns `true` for signature, identity, or replay failures.
    #[must_use]
    pub const fn is_authentication(&self) -> bool {
        matches!(self.category, ErrorCategory::Authentication)
    }

    /// Returns `true` for failed cryptographic proof verification.
    #[must_use]
    pub const fn is_cryptography(&self) -> bool {
        matches!(self.category, ErrorCategory::Cryptography)
    }

    /// Returns `true` for an illegal game action or invalid accounting transition.
    #[must_use]
    pub const fn is_rules(&self) -> bool {
        matches!(self.category, ErrorCategory::Rules)
    }

    /// Returns `true` when a validly encoded message was unexpected in the current phase.
    #[must_use]
    pub const fn is_protocol(&self) -> bool {
        matches!(self.category, ErrorCategory::Protocol)
    }

    /// Returns `true` for an invalid checkpoint or recovery witness.
    #[must_use]
    pub const fn is_recovery(&self) -> bool {
        matches!(self.category, ErrorCategory::Recovery)
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Error")
            .field("category", &self.category)
            .field("message", &self.message)
            .field("backtrace", &self.backtrace)
            .finish()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// Creates an invalid-configuration error for a core or game-specific constructor.
pub fn configuration(message: impl Into<String>) -> Error {
    Error::new(ErrorCategory::Configuration, message)
}

/// Creates an error for malformed, non-canonical, truncated, or oversized bytes.
pub fn codec(message: impl Into<String>) -> Error {
    Error::new(ErrorCategory::Codec, message)
}

/// Creates an error for invalid identity, signature, or replay context.
pub fn authentication(message: impl Into<String>) -> Error {
    Error::new(ErrorCategory::Authentication, message)
}

/// Creates an error for invalid cryptographic material or proof verification.
pub fn cryptography(message: impl Into<String>) -> Error {
    Error::new(ErrorCategory::Cryptography, message)
}

/// Creates an error for a game-rule or accounting violation.
pub fn rules(message: impl Into<String>) -> Error {
    Error::new(ErrorCategory::Rules, message)
}

/// Creates an error for a value that is invalid in the current protocol state.
pub fn protocol(message: impl Into<String>) -> Error {
    Error::new(ErrorCategory::Protocol, message)
}

/// Creates an error for invalid certified state or recovery evidence.
pub fn recovery(message: impl Into<String>) -> Error {
    Error::new(ErrorCategory::Recovery, message)
}
