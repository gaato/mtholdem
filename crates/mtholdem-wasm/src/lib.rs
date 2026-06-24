//! WASM-facing helpers for `mtholdem`.
//!
//! This crate intentionally exposes dependency-free Rust functions first. A downstream
//! JavaScript package can add `wasm-bindgen` or another binding generator without
//! changing the protocol boundary.

#![forbid(unsafe_code)]

use mtholdem_adapter::{ABI_VERSION_MAJOR, ABI_VERSION_MINOR, ABI_VERSION_PATCH};

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
