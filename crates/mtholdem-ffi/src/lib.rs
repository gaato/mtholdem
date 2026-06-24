//! C ABI wrapper for `mtholdem-peer`.
//!
//! The ABI uses opaque handles, primitive values, and explicit byte buffers. Protocol
//! validation remains in `mtholdem` and `mtholdem-peer`; this crate only translates the
//! foreign-function boundary.
//!
//! C callers own the event loop. They create one opaque handle per local peer, call
//! progress/apply functions, pop queued effects, and then route or persist the returned
//! byte buffers. This crate does not open sockets, schedule timers, store checkpoints,
//! or interpret recovery outcomes.
//!
//! Ownership is explicit at the boundary: buffers returned by this library must be
//! released with [`mth_buffer_free`] or [`mth_effect_free`], and handles returned by
//! [`mth_peer_session_new`] must be released with [`mth_peer_session_free`].
//!
//! # Audience and boundary
//!
//! This crate is for C-compatible consumers such as C, C++, Zig, Python extensions,
//! Ruby extensions, and Godot `GDExtension` wrappers. It does not expose Rust structs,
//! cryptographic internals, or transport objects. Target-specific wrappers should keep
//! their own language or engine objects outside this workspace and translate them to
//! the opaque handles, primitive fields, and byte buffers defined here.
//!
//! # Host responsibilities
//!
//! The host application is responsible for authenticated public broadcast, confidential
//! private-message routing, peer discovery, retry and timeout policy, rate limiting,
//! checkpoint persistence, anti-rollback storage, and any recovery mediator or
//! settlement contract. A local timeout is only local evidence until the host records
//! the matching challenge, deadline, and transcript or recovery artifacts.
//!
//! # Basic lifecycle
//!
//! A typical host creates one handle per local peer, advances or applies protocol
//! input, drains effects, and frees every object returned by this library:
//!
//! ```c
//! MthPeerSession *session = NULL;
//! int rc = mth_peer_session_new(config, identity_seed, local_seat, hand, &session);
//! if (rc != 0) {
//!     /* Read mth_last_error_message because no handle may exist. */
//!     return rc;
//! }
//!
//! rc = mth_peer_session_progress(session);
//! if (rc != 0) {
//!     /* Read mth_peer_session_last_error(session, ...). */
//!     mth_peer_session_free(session);
//!     return rc;
//! }
//!
//! for (;;) {
//!     MthEffect effect;
//!     rc = mth_peer_session_next_effect(session, &effect);
//!     if (rc == 4) {
//!         break; /* No queued effect. */
//!     }
//!     if (rc != 0) {
//!         break;
//!     }
//!     /* Route, persist, or inspect effect fields here. */
//!     mth_effect_free(effect);
//! }
//!
//! mth_peer_session_free(session);
//! ```
//!
//! # Ownership and allocation
//!
//! `MthPeerSession` pointers, [`MthBuffer`] values, and [`MthEffect`] values are
//! owned by the caller after a successful return. Pass each one back to the matching
//! free function exactly once. Do not free Rust allocations with the host allocator,
//! modify `MthBuffer.cap`, reuse a freed handle, or copy an owned buffer and free both
//! copies.
//!
//! # Error reporting
//!
//! Functions return integer status codes from the shared boundary error-code set. A
//! zero return means success. `NoEffect` is a normal empty-queue result from
//! [`mth_peer_session_next_effect`]. For handle operations, read
//! [`mth_peer_session_last_error`] after nonzero failures. For construction failures
//! where no handle exists, read [`mth_last_error_message`].
//!
//! # Threading
//!
//! Treat a peer-session handle as single-threaded mutable state. If a host moves a
//! handle between threads or exposes it through a language runtime, the host must
//! serialize calls so that no two FFI functions access the same handle concurrently.
//! Rust callers should also treat pointer-accepting entrypoints as unsafe FFI
//! functions and uphold each function's `# Safety` contract.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::mem::size_of_val;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::slice;

use mental_card_core::error::Error;
use mental_card_core::identity::{IDENTITY_PUBLIC_KEY_BYTES, IdentityPublic, IdentitySecret};
use mtholdem::config::{EconomicSecurity, Roster, SessionConfig};
use mtholdem::{Chips, HandNumber, Seat, SessionId};
use mtholdem_adapter::{
    ABI_VERSION_MAJOR, ABI_VERSION_MINOR, ABI_VERSION_PATCH, BoundaryAction, BoundaryActionKind,
    BoundaryEffect, BoundaryEffectKind, BoundaryErrorCode, BoundaryPeerPhase,
    decode_certified_checkpoint, decode_showdown_bundle, encode_checkpoint_state, encode_effects,
    encode_recovery_bundle,
};
use mtholdem_peer::PeerSession;
use rand::rngs::OsRng;

thread_local! {
    static LAST_ERROR: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

const PANIC_ERROR_MESSAGE: &str = "Rust panic crossed the C ABI boundary";

/// Opaque peer-session handle owned by the C ABI.
///
/// The handle keeps Rust-only state behind a stable pointer: the typed peer session,
/// its CSPRNG, queued boundary effects, and the last handle-local error message.
pub struct MthPeerSession {
    session: PeerSession,
    rng: OsRng,
    pending_effects: VecDeque<BoundaryEffect>,
    last_error: Vec<u8>,
}

/// Immutable session configuration supplied to `mth_peer_session_new`.
///
/// Pointers are borrowed only for the duration of construction. After the handle is
/// created, the Rust side owns validated copies of the session id and roster keys.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct MthSessionConfig {
    /// Pointer to 32 nonzero session-id bytes.
    pub session_id_ptr: *const u8,
    /// Pointer to `player_count * 32` Ed25519 public-key bytes in seat order.
    pub roster_public_keys_ptr: *const u8,
    /// Number of public keys in `roster_public_keys_ptr`.
    pub player_count: usize,
    /// Small blind in the table's smallest chip unit.
    pub small_blind_units: u64,
    /// Big blind in the table's smallest chip unit.
    pub big_blind_units: u64,
    /// Initial stack in the table's smallest chip unit.
    pub buy_in_units: u64,
    /// Maximum protected loss for one hand.
    pub maximum_hand_loss_units: u64,
    /// Compensation paid to each non-faulting peer.
    pub fault_compensation_units: u64,
    /// Security deposit posted by each peer.
    pub security_deposit_units: u64,
    /// Application-enforced response timeout in milliseconds.
    pub timeout_millis: u64,
}

/// Owned byte buffer returned across the C ABI.
///
/// A non-null pointer always represents a Rust allocation owned by the caller until it
/// is passed back to [`mth_buffer_free`]. Empty buffers use a null pointer with zero
/// length and capacity. The `cap` field is part of the allocation token needed for
/// freeing; callers must preserve it unchanged.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct MthBuffer {
    /// Pointer to the first byte, or null for an empty buffer.
    pub ptr: *mut u8,
    /// Number of initialized bytes.
    pub len: usize,
    /// Allocation capacity retained for `mth_buffer_free`; do not modify it.
    pub cap: usize,
}

impl MthBuffer {
    const fn empty() -> Self {
        Self {
            ptr: ptr::null_mut(),
            len: 0,
            cap: 0,
        }
    }

    fn from_vec(mut bytes: Vec<u8>) -> Self {
        if bytes.is_empty() {
            return Self::empty();
        }
        let buffer = Self {
            ptr: bytes.as_mut_ptr(),
            len: bytes.len(),
            cap: bytes.capacity(),
        };
        std::mem::forget(bytes);
        buffer
    }
}

/// One encoded host effect returned across the C ABI.
///
/// After a successful [`mth_peer_session_next_effect`] call, the caller owns the
/// buffers embedded in this value and must pass the whole effect to
/// [`mth_effect_free`] exactly once.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct MthEffect {
    /// One of [`BoundaryEffectKind`]'s integer values.
    pub kind: u32,
    /// Whether `seat` is meaningful.
    pub has_seat: bool,
    /// Relevant seat for send, action, or showdown effects.
    pub seat: u8,
    /// Whether legal-action fields are meaningful.
    pub has_legal_actions: bool,
    /// Amount required to call.
    pub to_call_units: u64,
    /// Whether `minimum_raise_to_units` is meaningful.
    pub has_minimum_raise_to: bool,
    /// Minimum full raise target.
    pub minimum_raise_to_units: u64,
    /// Maximum raise target.
    pub maximum_raise_to_units: u64,
    /// Whether a full raise is currently reopened.
    pub raise_reopened: bool,
    /// Whether checking is legal.
    pub can_check: bool,
    /// Whether transcript-anchor fields are meaningful.
    pub has_transcript_anchor: bool,
    /// Next expected public sequence.
    pub next_sequence: u64,
    /// Current transcript digest.
    pub transcript_digest: [u8; 32],
    /// Primary encoded artifact for message, bundle, or recovery effects.
    pub artifact: MthBuffer,
    /// Optional certified checkpoint bytes for persist effects.
    pub checkpoint: MthBuffer,
    /// Final stacks as little-endian `u64` chip units in table seat order.
    pub final_stacks: MthBuffer,
    /// Whether final-hand fields are meaningful.
    pub has_finished: bool,
    /// Completed hand number.
    pub finished_hand: u64,
}

impl MthEffect {
    const fn empty() -> Self {
        Self {
            kind: 0,
            has_seat: false,
            seat: 0,
            has_legal_actions: false,
            to_call_units: 0,
            has_minimum_raise_to: false,
            minimum_raise_to_units: 0,
            maximum_raise_to_units: 0,
            raise_reopened: false,
            can_check: false,
            has_transcript_anchor: false,
            next_sequence: 0,
            transcript_digest: [0; 32],
            artifact: MthBuffer::empty(),
            checkpoint: MthBuffer::empty(),
            final_stacks: MthBuffer::empty(),
            has_finished: false,
            finished_hand: 0,
        }
    }
}

impl From<BoundaryEffect> for MthEffect {
    fn from(value: BoundaryEffect) -> Self {
        let mut effect = Self::empty();
        effect.kind = boundary_effect_kind_code(value.kind());
        value.seat().inspect(|seat| {
            effect.has_seat = true;
            effect.seat = *seat;
        });
        if let Some(legal) = value.legal_actions() {
            effect.has_legal_actions = true;
            effect.to_call_units = legal.to_call_units();
            legal.minimum_raise_to_units().inspect(|minimum| {
                effect.has_minimum_raise_to = true;
                effect.minimum_raise_to_units = *minimum;
            });
            effect.maximum_raise_to_units = legal.maximum_raise_to_units();
            effect.raise_reopened = legal.raise_reopened();
            effect.can_check = legal.can_check();
        }
        value.transcript_anchor().inspect(|anchor| {
            effect.has_transcript_anchor = true;
            effect.next_sequence = anchor.next_sequence();
            effect.transcript_digest = anchor.digest();
        });
        if let Some(finished) = value.finished() {
            effect.has_finished = true;
            effect.finished_hand = finished.hand();
            let anchor = finished.transcript_anchor();
            effect.has_transcript_anchor = true;
            effect.next_sequence = anchor.next_sequence();
            effect.transcript_digest = anchor.digest();
            effect.final_stacks =
                MthBuffer::from_vec(encode_u64_slice(finished.final_stacks_units()));
        }
        effect.artifact = MthBuffer::from_vec(value.artifact().to_vec());
        effect.checkpoint =
            MthBuffer::from_vec(value.checkpoint().map_or_else(Vec::new, <[u8]>::to_vec));
        effect
    }
}

/// Read-only peer status returned across the C ABI.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct MthPeerStatus {
    /// One of [`BoundaryPeerPhase`]'s integer values.
    pub phase: u32,
    /// Local peer seat.
    pub local_seat: u8,
    /// Whether `next_public_sender` is meaningful.
    pub has_next_public_sender: bool,
    /// Next seat expected to broadcast a public message.
    pub next_public_sender: u8,
    /// Next expected public sequence.
    pub next_sequence: u64,
    /// Current transcript digest.
    pub transcript_digest: [u8; 32],
}

/// Returns the C ABI major version.
#[unsafe(no_mangle)]
pub const extern "C" fn mth_abi_version_major() -> u32 {
    ABI_VERSION_MAJOR
}

/// Returns the C ABI minor version.
#[unsafe(no_mangle)]
pub const extern "C" fn mth_abi_version_minor() -> u32 {
    ABI_VERSION_MINOR
}

/// Returns the C ABI patch version.
#[unsafe(no_mangle)]
pub const extern "C" fn mth_abi_version_patch() -> u32 {
    ABI_VERSION_PATCH
}

/// Frees a buffer returned by this library.
///
/// # Safety
///
/// `buffer` must be a live buffer value returned by this library, and it must not
/// have been freed before. Callers must preserve `ptr`, `len`, and `cap` exactly.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_buffer_free(buffer: MthBuffer) {
    if buffer.ptr.is_null() {
        return;
    }
    // SAFETY: `MthBuffer` values returned by this library are created with
    // `Vec::into_raw_parts`-equivalent ownership in `MthBuffer::from_vec`. Callers must
    // pass each owned buffer back at most once.
    unsafe {
        drop(Vec::from_raw_parts(buffer.ptr, buffer.len, buffer.cap));
    }
}

/// Frees owned buffers inside an effect returned by this library.
///
/// # Safety
///
/// `effect` must be a live effect value returned by this library, and it must not
/// have been freed before. Callers must preserve embedded buffer fields exactly.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_effect_free(effect: MthEffect) {
    // SAFETY: The caller's contract for this effect also covers both embedded
    // buffers, which were allocated by this library and have not been freed before.
    unsafe {
        mth_buffer_free(effect.artifact);
        mth_buffer_free(effect.checkpoint);
        mth_buffer_free(effect.final_stacks);
    }
}

/// Creates a peer-session handle.
///
/// # Safety
///
/// `out_handle` must be writable for one pointer value when non-null. Pointers inside
/// `config` and `local_identity_seed_ptr` must be readable for the documented byte
/// lengths for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_peer_session_new(
    config: MthSessionConfig,
    local_identity_seed_ptr: *const u8,
    local_seat: u8,
    hand: u64,
    out_handle: *mut *mut MthPeerSession,
) -> i32 {
    if out_handle.is_null() {
        return code(BoundaryErrorCode::NullPointer);
    }
    // SAFETY: `out_handle` was checked for null and is only written with either null or
    // a `Box` pointer allocated by this function.
    unsafe {
        *out_handle = ptr::null_mut();
    }
    let Ok(result) = catch_unwind(AssertUnwindSafe(|| {
        build_session(config, local_identity_seed_ptr, local_seat, hand)
    })) else {
        return store_global_panic();
    };
    match result {
        Ok(handle) => {
            clear_global_error();
            // SAFETY: `out_handle` was checked above and receives ownership of the boxed handle.
            unsafe {
                *out_handle = Box::into_raw(Box::new(handle));
            }
            code(BoundaryErrorCode::Success)
        }
        Err(error) => store_global_error(&error),
    }
}

/// Frees a peer-session handle returned by `mth_peer_session_new`.
///
/// # Safety
///
/// `handle` must be null or a live pointer returned by [`mth_peer_session_new`].
/// Each non-null handle must be freed at most once and must not be used afterward.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_peer_session_free(handle: *mut MthPeerSession) {
    if handle.is_null() {
        return;
    }
    // SAFETY: The pointer must have been returned by `mth_peer_session_new` and not
    // freed yet. Null is handled above.
    unsafe {
        drop(Box::from_raw(handle));
    }
}

/// Advances local peer progress and queues resulting effects.
///
/// # Safety
///
/// `handle` must be null or a live peer-session pointer returned by this library.
/// Calls using the same handle must be serialized by the host.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_peer_session_progress(handle: *mut MthPeerSession) -> i32 {
    with_handle(handle, |handle| handle.session.progress(&mut handle.rng))
}

/// Applies one canonical public-message byte string.
///
/// # Safety
///
/// `handle` must be null or a live peer-session pointer returned by this library.
/// `bytes_ptr` must be readable for `bytes_len` bytes when `bytes_len` is nonzero.
/// Calls using the same handle must be serialized by the host.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_peer_session_apply_public(
    handle: *mut MthPeerSession,
    bytes_ptr: *const u8,
    bytes_len: usize,
) -> i32 {
    with_handle_and_bytes(handle, bytes_ptr, bytes_len, |handle, bytes| {
        handle.session.apply_public_bytes(bytes, &mut handle.rng)
    })
}

/// Applies one canonical private-reveal byte string.
///
/// # Safety
///
/// `handle` must be null or a live peer-session pointer returned by this library.
/// `bytes_ptr` must be readable for `bytes_len` bytes when `bytes_len` is nonzero.
/// Calls using the same handle must be serialized by the host.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_peer_session_apply_private(
    handle: *mut MthPeerSession,
    bytes_ptr: *const u8,
    bytes_len: usize,
) -> i32 {
    with_handle_and_bytes(handle, bytes_ptr, bytes_len, |handle, bytes| {
        handle.session.apply_private_bytes(bytes)
    })
}

/// Applies one canonical certified-checkpoint byte string.
///
/// # Safety
///
/// `handle` must be null or a live peer-session pointer returned by this library.
/// `bytes_ptr` must be readable for `bytes_len` bytes when `bytes_len` is nonzero.
/// Calls using the same handle must be serialized by the host.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_peer_session_apply_checkpoint(
    handle: *mut MthPeerSession,
    bytes_ptr: *const u8,
    bytes_len: usize,
) -> i32 {
    with_handle_and_bytes(handle, bytes_ptr, bytes_len, |handle, bytes| {
        let checkpoint = decode_certified_checkpoint(bytes)?;
        handle.session.apply_certified_checkpoint(checkpoint)
    })
}

/// Announces one canonical certified-checkpoint byte string in the public transcript.
///
/// # Safety
///
/// `handle` must be null or a live peer-session pointer returned by this library.
/// `bytes_ptr` must be readable for `bytes_len` bytes when `bytes_len` is nonzero.
/// Calls using the same handle must be serialized by the host.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_peer_session_announce_checkpoint(
    handle: *mut MthPeerSession,
    bytes_ptr: *const u8,
    bytes_len: usize,
) -> i32 {
    with_handle_and_bytes(handle, bytes_ptr, bytes_len, |handle, bytes| {
        let checkpoint = decode_certified_checkpoint(bytes)?;
        handle.session.announce_checkpoint(checkpoint)
    })
}

/// Applies one canonical showdown-bundle byte string.
///
/// # Safety
///
/// `handle` must be null or a live peer-session pointer returned by this library.
/// `bytes_ptr` must be readable for `bytes_len` bytes when `bytes_len` is nonzero.
/// Calls using the same handle must be serialized by the host.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_peer_session_apply_showdown(
    handle: *mut MthPeerSession,
    bytes_ptr: *const u8,
    bytes_len: usize,
) -> i32 {
    with_handle_and_bytes(handle, bytes_ptr, bytes_len, |handle, bytes| {
        let bundle = decode_showdown_bundle(bytes)?;
        handle.session.apply_showdown_bundle(bundle)
    })
}

/// Applies one local betting action.
///
/// # Safety
///
/// `handle` must be null or a live peer-session pointer returned by this library.
/// Calls using the same handle must be serialized by the host.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_peer_session_submit_action(
    handle: *mut MthPeerSession,
    action_kind: u32,
    raise_to_units: u64,
) -> i32 {
    let Some(kind) = action_kind_from_code(action_kind) else {
        return code(BoundaryErrorCode::InvalidArgument);
    };
    with_handle(handle, |handle| {
        handle
            .session
            .submit_action(BoundaryAction::new(kind, raise_to_units).to_bet_action())
    })
}

/// Captures the current peer-visible checkpoint state as canonical bytes.
///
/// # Safety
///
/// `handle` must be null or a live peer-session pointer returned by this library.
/// `out_buffer` must be writable for one [`MthBuffer`] value when non-null. Calls
/// using the same handle must be serialized by the host.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_peer_session_checkpoint_state(
    handle: *mut MthPeerSession,
    out_buffer: *mut MthBuffer,
) -> i32 {
    with_output_buffer(handle, out_buffer, |handle| {
        encode_checkpoint_state(&handle.session.checkpoint_state()?)
    })
}

/// Builds the current recovery bundle as canonical bytes.
///
/// # Safety
///
/// `handle` must be null or a live peer-session pointer returned by this library.
/// `out_buffer` must be writable for one [`MthBuffer`] value when non-null. Calls
/// using the same handle must be serialized by the host.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_peer_session_recovery_bundle(
    handle: *mut MthPeerSession,
    out_buffer: *mut MthBuffer,
) -> i32 {
    with_output_buffer(handle, out_buffer, |handle| {
        encode_recovery_bundle(&handle.session.recovery_bundle()?)
    })
}

/// Requests recovery and queues the matching host effects.
///
/// # Safety
///
/// `handle` must be null or a live peer-session pointer returned by this library.
/// Calls using the same handle must be serialized by the host.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_peer_session_request_recovery(handle: *mut MthPeerSession) -> i32 {
    with_handle(handle, |handle| handle.session.request_recovery())
}

/// Copies the current peer status into `out_status`.
///
/// # Safety
///
/// `handle` must be null or a live peer-session pointer returned by this library.
/// `out_status` must be writable for one [`MthPeerStatus`] value when non-null.
/// Calls using the same handle must be serialized by the host.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_peer_session_status(
    handle: *mut MthPeerSession,
    out_status: *mut MthPeerStatus,
) -> i32 {
    if out_status.is_null() {
        return code(BoundaryErrorCode::NullPointer);
    }
    let Some(handle) = checked_handle(handle) else {
        return code(BoundaryErrorCode::NullPointer);
    };
    let status = handle.session.status();
    match catch_unwind(AssertUnwindSafe(|| {
        let anchor = status.transcript_anchor();
        let next_public_sender = status.next_public_sender();
        // SAFETY: `out_status` was checked for null and receives a plain C-compatible value.
        unsafe {
            *out_status = MthPeerStatus {
                phase: boundary_peer_phase_code(status.phase().into()),
                local_seat: status.local_seat().to_u8(),
                has_next_public_sender: next_public_sender.is_some(),
                next_public_sender: next_public_sender.map_or_else(|| 0, Seat::to_u8),
                next_sequence: anchor.next_sequence(),
                transcript_digest: anchor.digest().to_bytes(),
            };
        }
        code(BoundaryErrorCode::Success)
    })) {
        Ok(status) => status,
        Err(_) => store_panic(handle),
    }
}

/// Pops the next queued host effect into `out_effect`.
///
/// `NoEffect` means the queue is empty and is not a protocol failure. On success, the
/// caller owns the returned effect and must release it with [`mth_effect_free`].
///
/// # Safety
///
/// `handle` must be null or a live peer-session pointer returned by this library.
/// `out_effect` must be writable for one [`MthEffect`] value when non-null. Calls
/// using the same handle must be serialized by the host.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_peer_session_next_effect(
    handle: *mut MthPeerSession,
    out_effect: *mut MthEffect,
) -> i32 {
    if out_effect.is_null() {
        return code(BoundaryErrorCode::NullPointer);
    }
    let Some(handle) = checked_handle(handle) else {
        return code(BoundaryErrorCode::NullPointer);
    };
    match catch_unwind(AssertUnwindSafe(|| {
        if handle.pending_effects.is_empty() {
            match encode_effects(handle.session.drain_effects()) {
                Ok(effects) => handle.pending_effects.extend(effects),
                Err(error) => return store_error(handle, &error),
            }
        }
        let Some(effect) = handle.pending_effects.pop_front() else {
            return code(BoundaryErrorCode::NoEffect);
        };
        // SAFETY: `out_effect` was checked for null and receives a plain C-compatible value.
        unsafe {
            *out_effect = effect.into();
        }
        code(BoundaryErrorCode::Success)
    })) {
        Ok(status) => status,
        Err(_) => store_panic(handle),
    }
}

/// Copies the last error message for this handle into a caller-owned buffer.
///
/// # Safety
///
/// `handle` must be null or a live peer-session pointer returned by this library.
/// `out_required_len` must be writable for one `usize` when non-null. `out_ptr`
/// must be writable for `out_len` bytes when the last error is non-empty. Calls
/// using the same handle must be serialized by the host.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_peer_session_last_error(
    handle: *mut MthPeerSession,
    out_ptr: *mut u8,
    out_len: usize,
    out_required_len: *mut usize,
) -> i32 {
    let Some(handle) = checked_handle(handle) else {
        return code(BoundaryErrorCode::NullPointer);
    };
    if out_required_len.is_null() {
        return code(BoundaryErrorCode::NullPointer);
    }
    // SAFETY: `out_required_len` was checked for null and receives the required byte count.
    unsafe {
        *out_required_len = handle.last_error.len();
    }
    if handle.last_error.is_empty() {
        return code(BoundaryErrorCode::Success);
    }
    if out_ptr.is_null() {
        return code(BoundaryErrorCode::NullPointer);
    }
    if out_len < handle.last_error.len() {
        return code(BoundaryErrorCode::BufferTooSmall);
    }
    // SAFETY: `out_ptr` points to at least `out_len` bytes by caller contract; the length
    // check above ensures the copy fits and the source is an owned in-process buffer.
    unsafe {
        ptr::copy_nonoverlapping(handle.last_error.as_ptr(), out_ptr, handle.last_error.len());
    }
    code(BoundaryErrorCode::Success)
}

/// Copies the last thread-local error message into a caller-owned buffer.
///
/// This is primarily for construction failures where no peer-session handle exists yet.
/// Once a handle exists, prefer [`mth_peer_session_last_error`] for failures returned
/// by handle-scoped operations.
///
/// # Safety
///
/// `out_required_len` must be writable for one `usize` when non-null. `out_ptr`
/// must be writable for `out_len` bytes when the last error is non-empty.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_last_error_message(
    out_ptr: *mut u8,
    out_len: usize,
    out_required_len: *mut usize,
) -> i32 {
    LAST_ERROR.with(|cell| copy_error_message(&cell.borrow(), out_ptr, out_len, out_required_len))
}

fn build_session(
    config: MthSessionConfig,
    local_identity_seed_ptr: *const u8,
    local_seat: u8,
    hand: u64,
) -> Result<MthPeerSession, Error> {
    let session_id = SessionId::from_bytes(read_array::<32>(config.session_id_ptr)?)?;
    let keys = read_bytes(
        config.roster_public_keys_ptr,
        config
            .player_count
            .checked_mul(IDENTITY_PUBLIC_KEY_BYTES)
            .ok_or_else(|| mental_card_core::error::configuration("roster byte length overflow"))?,
    )?;
    let identities = keys
        .chunks_exact(IDENTITY_PUBLIC_KEY_BYTES)
        .map(|chunk| {
            let mut key = [0_u8; IDENTITY_PUBLIC_KEY_BYTES];
            key.copy_from_slice(chunk);
            IdentityPublic::from_bytes(key)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let roster = Roster::new(identities)?;
    let economics = EconomicSecurity::new(
        config.player_count,
        Chips::from_units(config.maximum_hand_loss_units),
        Chips::from_units(config.fault_compensation_units),
        Chips::from_units(config.security_deposit_units),
    )?;
    let session_config = SessionConfig::builder(
        session_id,
        roster,
        Chips::from_units(config.small_blind_units),
        Chips::from_units(config.big_blind_units),
        Chips::from_units(config.buy_in_units),
    )
    .economics(economics)
    .timeout_millis(config.timeout_millis)
    .build()?;
    let identity = IdentitySecret::from_seed(read_array::<32>(local_identity_seed_ptr)?);
    let session = PeerSession::new(
        session_config,
        identity,
        Seat::from_u8(local_seat)?,
        HandNumber::from_u64(hand),
    )?;
    Ok(MthPeerSession {
        session,
        rng: OsRng,
        pending_effects: VecDeque::new(),
        last_error: Vec::new(),
    })
}

fn with_handle(
    handle: *mut MthPeerSession,
    operation: impl FnOnce(&mut MthPeerSession) -> Result<(), Error>,
) -> i32 {
    let Some(handle) = checked_handle(handle) else {
        return code(BoundaryErrorCode::NullPointer);
    };
    match catch_unwind(AssertUnwindSafe(|| operation(handle))) {
        Err(_) => store_panic(handle),
        Ok(Ok(())) => {
            handle.last_error.clear();
            clear_global_error();
            code(BoundaryErrorCode::Success)
        }
        Ok(Err(error)) => store_error(handle, &error),
    }
}

fn with_handle_and_bytes(
    handle: *mut MthPeerSession,
    bytes_ptr: *const u8,
    bytes_len: usize,
    operation: impl FnOnce(&mut MthPeerSession, &[u8]) -> Result<(), Error>,
) -> i32 {
    let Some(handle) = checked_handle(handle) else {
        return code(BoundaryErrorCode::NullPointer);
    };
    let bytes = match read_bytes(bytes_ptr, bytes_len) {
        Ok(bytes) => bytes,
        Err(error) => return store_error(handle, &error),
    };
    match catch_unwind(AssertUnwindSafe(|| operation(handle, bytes))) {
        Err(_) => store_panic(handle),
        Ok(Ok(())) => {
            handle.last_error.clear();
            clear_global_error();
            code(BoundaryErrorCode::Success)
        }
        Ok(Err(error)) => store_error(handle, &error),
    }
}

fn with_output_buffer(
    handle: *mut MthPeerSession,
    out_buffer: *mut MthBuffer,
    operation: impl FnOnce(&mut MthPeerSession) -> Result<Vec<u8>, Error>,
) -> i32 {
    if out_buffer.is_null() {
        return code(BoundaryErrorCode::NullPointer);
    }
    let Some(handle) = checked_handle(handle) else {
        return code(BoundaryErrorCode::NullPointer);
    };
    match catch_unwind(AssertUnwindSafe(|| operation(handle))) {
        Err(_) => store_panic(handle),
        Ok(Ok(bytes)) => {
            clear_global_error();
            handle.last_error.clear();
            // SAFETY: `out_buffer` was checked for null and receives an owned buffer value.
            unsafe {
                *out_buffer = MthBuffer::from_vec(bytes);
            }
            code(BoundaryErrorCode::Success)
        }
        Ok(Err(error)) => store_error(handle, &error),
    }
}

fn checked_handle<'a>(handle: *mut MthPeerSession) -> Option<&'a mut MthPeerSession> {
    if handle.is_null() {
        return None;
    }
    // SAFETY: The caller promises `handle` is a live pointer returned by this library.
    // Public entrypoints check null before calling this helper.
    Some(unsafe { &mut *handle })
}

fn read_bytes<'a>(ptr: *const u8, len: usize) -> Result<&'a [u8], Error> {
    if len == 0 {
        return Ok(&[]);
    }
    if ptr.is_null() {
        return Err(mental_card_core::error::configuration(
            "non-empty byte slice has a null pointer",
        ));
    }
    // SAFETY: The caller promises `ptr` points to `len` readable bytes for the duration
    // of the call. Null with nonzero length is rejected above.
    Ok(unsafe { slice::from_raw_parts(ptr, len) })
}

fn read_array<const N: usize>(ptr: *const u8) -> Result<[u8; N], Error> {
    let bytes = read_bytes(ptr, N)?;
    let mut output = [0_u8; N];
    output.copy_from_slice(bytes);
    Ok(output)
}

fn encode_u64_slice(values: &[u64]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(size_of_val(values));
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn store_error(handle: &mut MthPeerSession, error: &Error) -> i32 {
    handle.last_error.clear();
    handle
        .last_error
        .extend_from_slice(error.message().as_bytes());
    LAST_ERROR.with(|cell| {
        let mut last_error = cell.borrow_mut();
        last_error.clear();
        last_error.extend_from_slice(error.message().as_bytes());
    });
    code(error.into())
}

fn store_global_error(error: &Error) -> i32 {
    LAST_ERROR.with(|cell| {
        let mut last_error = cell.borrow_mut();
        last_error.clear();
        last_error.extend_from_slice(error.message().as_bytes());
    });
    code(error.into())
}

fn store_panic(handle: &mut MthPeerSession) -> i32 {
    handle.last_error.clear();
    handle
        .last_error
        .extend_from_slice(PANIC_ERROR_MESSAGE.as_bytes());
    LAST_ERROR.with(|cell| {
        let mut last_error = cell.borrow_mut();
        last_error.clear();
        last_error.extend_from_slice(PANIC_ERROR_MESSAGE.as_bytes());
    });
    code(BoundaryErrorCode::Internal)
}

fn store_global_panic() -> i32 {
    LAST_ERROR.with(|cell| {
        let mut last_error = cell.borrow_mut();
        last_error.clear();
        last_error.extend_from_slice(PANIC_ERROR_MESSAGE.as_bytes());
    });
    code(BoundaryErrorCode::Internal)
}

fn clear_global_error() {
    LAST_ERROR.with(|cell| cell.borrow_mut().clear());
}

fn copy_error_message(
    message: &[u8],
    out_ptr: *mut u8,
    out_len: usize,
    out_required_len: *mut usize,
) -> i32 {
    if out_required_len.is_null() {
        return code(BoundaryErrorCode::NullPointer);
    }
    // SAFETY: `out_required_len` was checked for null and receives the required byte count.
    unsafe {
        *out_required_len = message.len();
    }
    if message.is_empty() {
        return code(BoundaryErrorCode::Success);
    }
    if out_ptr.is_null() {
        return code(BoundaryErrorCode::NullPointer);
    }
    if out_len < message.len() {
        return code(BoundaryErrorCode::BufferTooSmall);
    }
    // SAFETY: The caller promises `out_ptr` points to at least `out_len` writable bytes.
    // The length check above ensures the copy fits.
    unsafe {
        ptr::copy_nonoverlapping(message.as_ptr(), out_ptr, message.len());
    }
    code(BoundaryErrorCode::Success)
}

const fn code(value: BoundaryErrorCode) -> i32 {
    value as i32
}

const fn action_kind_from_code(value: u32) -> Option<BoundaryActionKind> {
    match value {
        1 => Some(BoundaryActionKind::Fold),
        2 => Some(BoundaryActionKind::Check),
        3 => Some(BoundaryActionKind::Call),
        4 => Some(BoundaryActionKind::RaiseTo),
        5 => Some(BoundaryActionKind::AllIn),
        _ => None,
    }
}

const fn boundary_effect_kind_code(value: BoundaryEffectKind) -> u32 {
    match value {
        BoundaryEffectKind::BroadcastPublic => 1,
        BoundaryEffectKind::SendPrivate => 2,
        BoundaryEffectKind::Persist => 3,
        BoundaryEffectKind::RequestAction => 4,
        BoundaryEffectKind::SubmitRecovery => 5,
        BoundaryEffectKind::PublishShowdownBundle => 6,
        BoundaryEffectKind::Finished => 7,
    }
}

const fn boundary_peer_phase_code(value: BoundaryPeerPhase) -> u32 {
    match value {
        BoundaryPeerPhase::KeySetup => 1,
        BoundaryPeerPhase::Shuffle => 2,
        BoundaryPeerPhase::PrivateReveal => 3,
        BoundaryPeerPhase::HoleCardsReady => 4,
        BoundaryPeerPhase::Betting => 5,
        BoundaryPeerPhase::BoardReveal => 6,
        BoundaryPeerPhase::Showdown => 7,
        BoundaryPeerPhase::Complete => 8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::mem::size_of;

    #[test]
    fn abi_version_matches_adapter_boundary() {
        assert_eq!(mth_abi_version_major(), ABI_VERSION_MAJOR);
        assert_eq!(mth_abi_version_minor(), ABI_VERSION_MINOR);
        assert_eq!(mth_abi_version_patch(), ABI_VERSION_PATCH);
    }

    #[test]
    fn null_handle_status_returns_null_pointer() {
        let mut status = MthPeerStatus {
            phase: 0,
            local_seat: 0,
            has_next_public_sender: false,
            next_public_sender: 0,
            next_sequence: 0,
            transcript_digest: [0; 32],
        };

        assert_eq!(
            // SAFETY: This test intentionally passes a null handle and a valid
            // status output pointer to verify the boundary error code.
            unsafe { mth_peer_session_status(ptr::null_mut(), &raw mut status) },
            code(BoundaryErrorCode::NullPointer)
        );
    }

    #[test]
    fn constructor_failure_sets_thread_local_error() {
        let config = MthSessionConfig {
            session_id_ptr: ptr::null(),
            roster_public_keys_ptr: ptr::null(),
            player_count: 0,
            small_blind_units: 1,
            big_blind_units: 2,
            buy_in_units: 100,
            maximum_hand_loss_units: 100,
            fault_compensation_units: 101,
            security_deposit_units: 101,
            timeout_millis: 30_000,
        };
        let mut handle = ptr::null_mut();
        // SAFETY: This test intentionally passes null construction inputs and a valid
        // out-handle pointer to verify construction failure handling.
        let status = unsafe { mth_peer_session_new(config, ptr::null(), 0, 0, &raw mut handle) };

        assert_eq!(status, code(BoundaryErrorCode::Configuration));
        assert!(handle.is_null());

        let mut required = 0;
        assert_eq!(
            // SAFETY: This test intentionally passes a null output buffer and a valid
            // required-length pointer to verify sizing/error behavior.
            unsafe { mth_last_error_message(ptr::null_mut(), 0, &raw mut required) },
            code(BoundaryErrorCode::NullPointer)
        );
        assert!(required > 0);
    }

    #[derive(Debug)]
    enum QueuedEffect {
        Public { sender_index: usize, bytes: Vec<u8> },
        Private { recipient: u8, bytes: Vec<u8> },
        Showdown { owner: u8, bytes: Vec<u8> },
    }

    #[derive(Clone, Debug)]
    struct DrainedEffect {
        kind: u32,
        seat: Option<u8>,
        has_legal_actions: bool,
        can_check: bool,
        artifact: Vec<u8>,
        final_stacks: Vec<u64>,
        has_finished: bool,
    }

    struct FfiSessions {
        handles: Vec<*mut MthPeerSession>,
    }

    impl Drop for FfiSessions {
        fn drop(&mut self) {
            for handle in self.handles.drain(..) {
                // SAFETY: These handles were returned by `mth_peer_session_new` in this
                // test and are freed exactly once here.
                unsafe {
                    mth_peer_session_free(handle);
                }
            }
        }
    }

    impl FfiSessions {
        fn standard_three_player() -> Self {
            let session_id = [0x51; 32];
            let seeds = [[1; 32], [2; 32], [3; 32]];
            let mut public_keys = Vec::new();
            for seed in seeds {
                public_keys.extend_from_slice(&IdentitySecret::from_seed(seed).public().to_bytes());
            }
            let config = MthSessionConfig {
                session_id_ptr: session_id.as_ptr(),
                roster_public_keys_ptr: public_keys.as_ptr(),
                player_count: seeds.len(),
                small_blind_units: 1,
                big_blind_units: 2,
                buy_in_units: 100,
                maximum_hand_loss_units: 100,
                fault_compensation_units: 101,
                security_deposit_units: 202,
                timeout_millis: 30_000,
            };
            let mut handles = Vec::new();
            for (index, seed) in seeds.into_iter().enumerate() {
                let mut handle = ptr::null_mut();
                // SAFETY: `config` points to live local arrays for the duration of this
                // call, `seed` is readable for 32 bytes, and `handle` is a valid out pointer.
                let status = unsafe {
                    mth_peer_session_new(
                        config,
                        seed.as_ptr(),
                        u8::try_from(index).expect("test seat index fits in u8"),
                        1,
                        &raw mut handle,
                    )
                };
                assert_eq!(status, code(BoundaryErrorCode::Success));
                assert!(!handle.is_null());
                handles.push(handle);
            }
            Self { handles }
        }
    }

    fn copy_buffer(buffer: MthBuffer) -> Vec<u8> {
        if buffer.ptr.is_null() {
            assert_eq!(buffer.len, 0);
            return Vec::new();
        }
        // SAFETY: The buffer was returned by this library and is still live while the
        // caller owns the surrounding effect.
        unsafe { slice::from_raw_parts(buffer.ptr, buffer.len).to_vec() }
    }

    fn decode_u64_buffer(buffer: MthBuffer) -> Vec<u64> {
        let bytes = copy_buffer(buffer);
        assert_eq!(bytes.len() % size_of::<u64>(), 0);
        bytes
            .chunks_exact(size_of::<u64>())
            .map(|chunk| {
                let mut value = [0_u8; size_of::<u64>()];
                value.copy_from_slice(chunk);
                u64::from_le_bytes(value)
            })
            .collect()
    }

    fn drain_effects(handle: *mut MthPeerSession) -> Vec<DrainedEffect> {
        let mut output = Vec::new();
        loop {
            let mut effect = MthEffect::empty();
            // SAFETY: `handle` is a live test handle and `effect` is a valid out pointer.
            let status = unsafe { mth_peer_session_next_effect(handle, &raw mut effect) };
            if status == code(BoundaryErrorCode::NoEffect) {
                break;
            }
            assert_eq!(status, code(BoundaryErrorCode::Success));
            let drained = DrainedEffect {
                kind: effect.kind,
                seat: effect.has_seat.then_some(effect.seat),
                has_legal_actions: effect.has_legal_actions,
                can_check: effect.can_check,
                artifact: copy_buffer(effect.artifact),
                final_stacks: decode_u64_buffer(effect.final_stacks),
                has_finished: effect.has_finished,
            };
            // SAFETY: This effect was just returned by `mth_peer_session_next_effect`
            // and is freed exactly once after copying its buffers.
            unsafe {
                mth_effect_free(effect);
            }
            output.push(drained);
        }
        output
    }

    fn progress_and_drain(
        sessions: &[MthPeerSessionHandle],
        queue: &mut VecDeque<QueuedEffect>,
        finished_stacks: &mut [Vec<Vec<u64>>],
        index: usize,
        call_or_check: bool,
    ) {
        // SAFETY: Handles are live for the duration of the test and calls are serialized.
        assert_eq!(
            unsafe { mth_peer_session_progress(sessions[index].0) },
            code(BoundaryErrorCode::Success)
        );
        handle_effects(sessions, queue, finished_stacks, index, call_or_check);
    }

    #[derive(Clone, Copy)]
    struct MthPeerSessionHandle(*mut MthPeerSession);

    fn handle_effects(
        sessions: &[MthPeerSessionHandle],
        queue: &mut VecDeque<QueuedEffect>,
        finished_stacks: &mut [Vec<Vec<u64>>],
        sender_index: usize,
        call_or_check: bool,
    ) {
        loop {
            let effects = drain_effects(sessions[sender_index].0);
            if effects.is_empty() {
                break;
            }
            for effect in effects {
                match effect.kind {
                    1 => queue.push_back(QueuedEffect::Public {
                        sender_index,
                        bytes: effect.artifact,
                    }),
                    2 => queue.push_back(QueuedEffect::Private {
                        recipient: effect.seat.expect("private effect has recipient"),
                        bytes: effect.artifact,
                    }),
                    4 => {
                        assert!(effect.has_legal_actions);
                        let action_kind = if call_or_check {
                            if effect.can_check { 2 } else { 3 }
                        } else {
                            1
                        };
                        // SAFETY: Handle is live and calls are serialized.
                        assert_eq!(
                            unsafe {
                                mth_peer_session_submit_action(
                                    sessions[sender_index].0,
                                    action_kind,
                                    0,
                                )
                            },
                            code(BoundaryErrorCode::Success)
                        );
                        // SAFETY: Handle is live and calls are serialized.
                        assert_eq!(
                            unsafe { mth_peer_session_progress(sessions[sender_index].0) },
                            code(BoundaryErrorCode::Success)
                        );
                    }
                    6 => queue.push_back(QueuedEffect::Showdown {
                        owner: effect.seat.expect("showdown effect has owner"),
                        bytes: effect.artifact,
                    }),
                    7 => {
                        assert!(effect.has_finished);
                        assert!(!effect.final_stacks.is_empty());
                        finished_stacks[sender_index].push(effect.final_stacks);
                    }
                    3 | 5 => {}
                    other => panic!("unexpected FFI effect kind {other}"),
                }
            }
        }
    }

    fn drive_ffi_hand(call_or_check: bool) -> Vec<Vec<Vec<u64>>> {
        let sessions = FfiSessions::standard_three_player();
        let handles: Vec<_> = sessions
            .handles
            .iter()
            .copied()
            .map(MthPeerSessionHandle)
            .collect();
        let mut queue = VecDeque::new();
        let mut finished_stacks = vec![Vec::new(); handles.len()];

        for index in 0..handles.len() {
            progress_and_drain(
                &handles,
                &mut queue,
                &mut finished_stacks,
                index,
                call_or_check,
            );
        }

        while let Some(effect) = queue.pop_front() {
            match effect {
                QueuedEffect::Public {
                    sender_index,
                    bytes,
                } => {
                    for index in 0..handles.len() {
                        if index == sender_index {
                            continue;
                        }
                        // SAFETY: Handle is live and the byte slice is readable for this call.
                        assert_eq!(
                            unsafe {
                                mth_peer_session_apply_public(
                                    handles[index].0,
                                    bytes.as_ptr(),
                                    bytes.len(),
                                )
                            },
                            code(BoundaryErrorCode::Success)
                        );
                        progress_and_drain(
                            &handles,
                            &mut queue,
                            &mut finished_stacks,
                            index,
                            call_or_check,
                        );
                    }
                }
                QueuedEffect::Private { recipient, bytes } => {
                    let index = usize::from(recipient);
                    // SAFETY: Handle is live and the byte slice is readable for this call.
                    assert_eq!(
                        unsafe {
                            mth_peer_session_apply_private(
                                handles[index].0,
                                bytes.as_ptr(),
                                bytes.len(),
                            )
                        },
                        code(BoundaryErrorCode::Success)
                    );
                    progress_and_drain(
                        &handles,
                        &mut queue,
                        &mut finished_stacks,
                        index,
                        call_or_check,
                    );
                }
                QueuedEffect::Showdown { owner, bytes } => {
                    for index in 0..handles.len() {
                        if index == usize::from(owner) {
                            continue;
                        }
                        // SAFETY: Handle is live and the byte slice is readable for this call.
                        assert_eq!(
                            unsafe {
                                mth_peer_session_apply_showdown(
                                    handles[index].0,
                                    bytes.as_ptr(),
                                    bytes.len(),
                                )
                            },
                            code(BoundaryErrorCode::Success)
                        );
                        progress_and_drain(
                            &handles,
                            &mut queue,
                            &mut finished_stacks,
                            index,
                            call_or_check,
                        );
                    }
                }
            }
        }

        for handle in &handles {
            let mut status = MthPeerStatus {
                phase: 0,
                local_seat: 0,
                has_next_public_sender: false,
                next_public_sender: 0,
                next_sequence: 0,
                transcript_digest: [0; 32],
            };
            // SAFETY: Handle is live and `status` is a valid out pointer.
            assert_eq!(
                unsafe { mth_peer_session_status(handle.0, &raw mut status) },
                code(BoundaryErrorCode::Success)
            );
            assert_eq!(
                status.phase,
                boundary_peer_phase_code(BoundaryPeerPhase::Complete)
            );
        }

        finished_stacks
    }

    #[test]
    fn ffi_peers_exchange_messages_through_uncontested_finish() {
        let finished_stacks = drive_ffi_hand(false);
        assert!(finished_stacks.iter().all(|effects| !effects.is_empty()));
        let final_stacks: Vec<_> = finished_stacks
            .iter()
            .map(|effects| effects.last().expect("finished stacks").clone())
            .collect();
        assert!(final_stacks.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(final_stacks[0].iter().sum::<u64>(), 300);
    }

    #[test]
    fn ffi_peers_reveal_board_and_finish_contested_showdown() {
        let finished_stacks = drive_ffi_hand(true);
        assert!(finished_stacks.iter().all(|effects| !effects.is_empty()));
        let final_stacks: Vec<_> = finished_stacks
            .iter()
            .map(|effects| effects.last().expect("finished stacks").clone())
            .collect();
        assert!(final_stacks.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(final_stacks[0].iter().sum::<u64>(), 300);
    }
}
