//! C ABI wrapper for `mtholdem-peer`.
//!
//! The ABI uses opaque handles, primitive values, and explicit byte buffers. Protocol
//! validation remains in `mtholdem` and `mtholdem-peer`; this crate only translates the
//! foreign-function boundary.

use std::cell::RefCell;
use std::collections::VecDeque;
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

/// Opaque peer-session handle owned by the C ABI.
pub struct Mth9PeerSession {
    session: PeerSession,
    rng: OsRng,
    pending_effects: VecDeque<BoundaryEffect>,
    last_error: Vec<u8>,
}

/// Immutable session configuration supplied to `mth9_peer_session_new`.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Mth9SessionConfig {
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
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Mth9Buffer {
    /// Pointer to the first byte, or null for an empty buffer.
    pub ptr: *mut u8,
    /// Number of initialized bytes.
    pub len: usize,
    /// Allocation capacity retained for `mth9_buffer_free`.
    pub cap: usize,
}

impl Mth9Buffer {
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
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Mth9Effect {
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
    pub artifact: Mth9Buffer,
    /// Optional certified checkpoint bytes for persist effects.
    pub checkpoint: Mth9Buffer,
    /// Whether final-hand fields are meaningful.
    pub has_finished: bool,
    /// Completed hand number.
    pub finished_hand: u64,
}

impl Mth9Effect {
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
            artifact: Mth9Buffer::empty(),
            checkpoint: Mth9Buffer::empty(),
            has_finished: false,
            finished_hand: 0,
        }
    }
}

impl From<BoundaryEffect> for Mth9Effect {
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
        }
        effect.artifact = Mth9Buffer::from_vec(value.artifact().to_vec());
        effect.checkpoint =
            Mth9Buffer::from_vec(value.checkpoint().map_or_else(Vec::new, <[u8]>::to_vec));
        effect
    }
}

/// Read-only peer status returned across the C ABI.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Mth9PeerStatus {
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
pub const extern "C" fn mth9_abi_version_major() -> u32 {
    ABI_VERSION_MAJOR
}

/// Returns the C ABI minor version.
#[unsafe(no_mangle)]
pub const extern "C" fn mth9_abi_version_minor() -> u32 {
    ABI_VERSION_MINOR
}

/// Returns the C ABI patch version.
#[unsafe(no_mangle)]
pub const extern "C" fn mth9_abi_version_patch() -> u32 {
    ABI_VERSION_PATCH
}

/// Frees a buffer returned by this library.
#[unsafe(no_mangle)]
pub extern "C" fn mth9_buffer_free(buffer: Mth9Buffer) {
    if buffer.ptr.is_null() {
        return;
    }
    // SAFETY: `Mth9Buffer` values returned by this library are created with
    // `Vec::into_raw_parts`-equivalent ownership in `Mth9Buffer::from_vec`. Callers must
    // pass each owned buffer back at most once.
    unsafe {
        drop(Vec::from_raw_parts(buffer.ptr, buffer.len, buffer.cap));
    }
}

/// Frees owned buffers inside an effect returned by this library.
#[unsafe(no_mangle)]
pub extern "C" fn mth9_effect_free(effect: Mth9Effect) {
    mth9_buffer_free(effect.artifact);
    mth9_buffer_free(effect.checkpoint);
}

/// Creates a peer-session handle.
#[unsafe(no_mangle)]
pub extern "C" fn mth9_peer_session_new(
    config: Mth9SessionConfig,
    local_identity_seed_ptr: *const u8,
    local_seat: u8,
    hand: u64,
    out_handle: *mut *mut Mth9PeerSession,
) -> i32 {
    if out_handle.is_null() {
        return code(BoundaryErrorCode::NullPointer);
    }
    // SAFETY: `out_handle` was checked for null and is only written with either null or
    // a `Box` pointer allocated by this function.
    unsafe {
        *out_handle = ptr::null_mut();
    }
    let result = build_session(config, local_identity_seed_ptr, local_seat, hand);
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

/// Frees a peer-session handle returned by `mth9_peer_session_new`.
#[unsafe(no_mangle)]
pub extern "C" fn mth9_peer_session_free(handle: *mut Mth9PeerSession) {
    if handle.is_null() {
        return;
    }
    // SAFETY: The pointer must have been returned by `mth9_peer_session_new` and not
    // freed yet. Null is handled above.
    unsafe {
        drop(Box::from_raw(handle));
    }
}

/// Advances local peer progress and queues resulting effects.
#[unsafe(no_mangle)]
pub extern "C" fn mth9_peer_session_progress(handle: *mut Mth9PeerSession) -> i32 {
    with_handle(handle, |handle| handle.session.progress(&mut handle.rng))
}

/// Applies one canonical public-message byte string.
#[unsafe(no_mangle)]
pub extern "C" fn mth9_peer_session_apply_public(
    handle: *mut Mth9PeerSession,
    bytes_ptr: *const u8,
    bytes_len: usize,
) -> i32 {
    with_handle_and_bytes(handle, bytes_ptr, bytes_len, |handle, bytes| {
        handle.session.apply_public_bytes(bytes, &mut handle.rng)
    })
}

/// Applies one canonical private-reveal byte string.
#[unsafe(no_mangle)]
pub extern "C" fn mth9_peer_session_apply_private(
    handle: *mut Mth9PeerSession,
    bytes_ptr: *const u8,
    bytes_len: usize,
) -> i32 {
    with_handle_and_bytes(handle, bytes_ptr, bytes_len, |handle, bytes| {
        handle.session.apply_private_bytes(bytes)
    })
}

/// Applies one canonical certified-checkpoint byte string.
#[unsafe(no_mangle)]
pub extern "C" fn mth9_peer_session_apply_checkpoint(
    handle: *mut Mth9PeerSession,
    bytes_ptr: *const u8,
    bytes_len: usize,
) -> i32 {
    with_handle_and_bytes(handle, bytes_ptr, bytes_len, |handle, bytes| {
        let checkpoint = decode_certified_checkpoint(bytes)?;
        handle.session.apply_certified_checkpoint(checkpoint)
    })
}

/// Announces one canonical certified-checkpoint byte string in the public transcript.
#[unsafe(no_mangle)]
pub extern "C" fn mth9_peer_session_announce_checkpoint(
    handle: *mut Mth9PeerSession,
    bytes_ptr: *const u8,
    bytes_len: usize,
) -> i32 {
    with_handle_and_bytes(handle, bytes_ptr, bytes_len, |handle, bytes| {
        let checkpoint = decode_certified_checkpoint(bytes)?;
        handle.session.announce_checkpoint(checkpoint)
    })
}

/// Applies one canonical showdown-bundle byte string.
#[unsafe(no_mangle)]
pub extern "C" fn mth9_peer_session_apply_showdown(
    handle: *mut Mth9PeerSession,
    bytes_ptr: *const u8,
    bytes_len: usize,
) -> i32 {
    with_handle_and_bytes(handle, bytes_ptr, bytes_len, |handle, bytes| {
        let bundle = decode_showdown_bundle(bytes)?;
        handle.session.apply_showdown_bundle(bundle)
    })
}

/// Applies one local betting action.
#[unsafe(no_mangle)]
pub extern "C" fn mth9_peer_session_submit_action(
    handle: *mut Mth9PeerSession,
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
#[unsafe(no_mangle)]
pub extern "C" fn mth9_peer_session_checkpoint_state(
    handle: *mut Mth9PeerSession,
    out_buffer: *mut Mth9Buffer,
) -> i32 {
    with_output_buffer(handle, out_buffer, |handle| {
        encode_checkpoint_state(&handle.session.checkpoint_state()?)
    })
}

/// Builds the current recovery bundle as canonical bytes.
#[unsafe(no_mangle)]
pub extern "C" fn mth9_peer_session_recovery_bundle(
    handle: *mut Mth9PeerSession,
    out_buffer: *mut Mth9Buffer,
) -> i32 {
    with_output_buffer(handle, out_buffer, |handle| {
        encode_recovery_bundle(&handle.session.recovery_bundle()?)
    })
}

/// Requests recovery and queues the matching host effects.
#[unsafe(no_mangle)]
pub extern "C" fn mth9_peer_session_request_recovery(handle: *mut Mth9PeerSession) -> i32 {
    with_handle(handle, |handle| handle.session.request_recovery())
}

/// Copies the current peer status into `out_status`.
#[unsafe(no_mangle)]
pub extern "C" fn mth9_peer_session_status(
    handle: *mut Mth9PeerSession,
    out_status: *mut Mth9PeerStatus,
) -> i32 {
    if out_status.is_null() {
        return code(BoundaryErrorCode::NullPointer);
    }
    let Some(handle) = checked_handle(handle) else {
        return code(BoundaryErrorCode::NullPointer);
    };
    let status = handle.session.status();
    let anchor = status.transcript_anchor();
    let next_public_sender = status.next_public_sender();
    // SAFETY: `out_status` was checked for null and receives a plain C-compatible value.
    unsafe {
        *out_status = Mth9PeerStatus {
            phase: boundary_peer_phase_code(status.phase().into()),
            local_seat: status.local_seat().to_u8(),
            has_next_public_sender: next_public_sender.is_some(),
            next_public_sender: next_public_sender.map_or_else(|| 0, Seat::to_u8),
            next_sequence: anchor.next_sequence(),
            transcript_digest: anchor.digest().to_bytes(),
        };
    }
    code(BoundaryErrorCode::Success)
}

/// Pops the next queued host effect into `out_effect`.
#[unsafe(no_mangle)]
pub extern "C" fn mth9_peer_session_next_effect(
    handle: *mut Mth9PeerSession,
    out_effect: *mut Mth9Effect,
) -> i32 {
    if out_effect.is_null() {
        return code(BoundaryErrorCode::NullPointer);
    }
    let Some(handle) = checked_handle(handle) else {
        return code(BoundaryErrorCode::NullPointer);
    };
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
}

/// Copies the last error message for this handle into a caller-owned buffer.
#[unsafe(no_mangle)]
pub extern "C" fn mth9_peer_session_last_error(
    handle: *mut Mth9PeerSession,
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
#[unsafe(no_mangle)]
pub extern "C" fn mth9_last_error_message(
    out_ptr: *mut u8,
    out_len: usize,
    out_required_len: *mut usize,
) -> i32 {
    LAST_ERROR.with(|cell| copy_error_message(&cell.borrow(), out_ptr, out_len, out_required_len))
}

fn build_session(
    config: Mth9SessionConfig,
    local_identity_seed_ptr: *const u8,
    local_seat: u8,
    hand: u64,
) -> Result<Mth9PeerSession, Error> {
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
    Ok(Mth9PeerSession {
        session,
        rng: OsRng,
        pending_effects: VecDeque::new(),
        last_error: Vec::new(),
    })
}

fn with_handle(
    handle: *mut Mth9PeerSession,
    operation: impl FnOnce(&mut Mth9PeerSession) -> Result<(), Error>,
) -> i32 {
    let Some(handle) = checked_handle(handle) else {
        return code(BoundaryErrorCode::NullPointer);
    };
    match operation(handle) {
        Ok(()) => {
            handle.last_error.clear();
            clear_global_error();
            code(BoundaryErrorCode::Success)
        }
        Err(error) => store_error(handle, &error),
    }
}

fn with_handle_and_bytes(
    handle: *mut Mth9PeerSession,
    bytes_ptr: *const u8,
    bytes_len: usize,
    operation: impl FnOnce(&mut Mth9PeerSession, &[u8]) -> Result<(), Error>,
) -> i32 {
    let Some(handle) = checked_handle(handle) else {
        return code(BoundaryErrorCode::NullPointer);
    };
    let bytes = match read_bytes(bytes_ptr, bytes_len) {
        Ok(bytes) => bytes,
        Err(error) => return store_error(handle, &error),
    };
    match operation(handle, bytes) {
        Ok(()) => {
            handle.last_error.clear();
            clear_global_error();
            code(BoundaryErrorCode::Success)
        }
        Err(error) => store_error(handle, &error),
    }
}

fn with_output_buffer(
    handle: *mut Mth9PeerSession,
    out_buffer: *mut Mth9Buffer,
    operation: impl FnOnce(&mut Mth9PeerSession) -> Result<Vec<u8>, Error>,
) -> i32 {
    if out_buffer.is_null() {
        return code(BoundaryErrorCode::NullPointer);
    }
    let Some(handle) = checked_handle(handle) else {
        return code(BoundaryErrorCode::NullPointer);
    };
    match operation(handle) {
        Ok(bytes) => {
            clear_global_error();
            handle.last_error.clear();
            // SAFETY: `out_buffer` was checked for null and receives an owned buffer value.
            unsafe {
                *out_buffer = Mth9Buffer::from_vec(bytes);
            }
            code(BoundaryErrorCode::Success)
        }
        Err(error) => store_error(handle, &error),
    }
}

fn checked_handle<'a>(handle: *mut Mth9PeerSession) -> Option<&'a mut Mth9PeerSession> {
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

fn store_error(handle: &mut Mth9PeerSession, error: &Error) -> i32 {
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

    #[test]
    fn abi_version_matches_adapter_boundary() {
        assert_eq!(mth9_abi_version_major(), ABI_VERSION_MAJOR);
        assert_eq!(mth9_abi_version_minor(), ABI_VERSION_MINOR);
        assert_eq!(mth9_abi_version_patch(), ABI_VERSION_PATCH);
    }

    #[test]
    fn null_handle_status_returns_null_pointer() {
        let mut status = Mth9PeerStatus {
            phase: 0,
            local_seat: 0,
            has_next_public_sender: false,
            next_public_sender: 0,
            next_sequence: 0,
            transcript_digest: [0; 32],
        };

        assert_eq!(
            mth9_peer_session_status(ptr::null_mut(), &raw mut status),
            code(BoundaryErrorCode::NullPointer)
        );
    }

    #[test]
    fn constructor_failure_sets_thread_local_error() {
        let config = Mth9SessionConfig {
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
        let status = mth9_peer_session_new(config, ptr::null(), 0, 0, &raw mut handle);

        assert_eq!(status, code(BoundaryErrorCode::Configuration));
        assert!(handle.is_null());

        let mut required = 0;
        assert_eq!(
            mth9_last_error_message(ptr::null_mut(), 0, &raw mut required),
            code(BoundaryErrorCode::NullPointer)
        );
        assert!(required > 0);
    }
}
