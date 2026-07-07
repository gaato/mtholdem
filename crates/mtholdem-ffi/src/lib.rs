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
//! The official C header is distributed as `include/mtholdem.h` in this crate.
//! Host wrappers should compile against that header instead of hand-maintaining
//! declarations for structs, functions, constants, or layout checks.
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
//! Local table hosts that want a single-player or demo shell should prefer the
//! `mth_table_*` entrypoints. They expose a polling table-session handle, a backend
//! selector, local table events, and a human-action submission point without exposing
//! the Rust-only bot decision context:
//!
//! ```c
//! MthTableConfig config = {
//!     MTH_TABLE_BACKEND_RULES_ONLY,
//!     6,
//!     true,
//!     0,
//!     MTH_BOT_PRESET_BALANCED,
//!     1,
//!     2,
//!     100,
//!     false,
//!     0,
//!     7,
//! };
//! MthTableSession *table = NULL;
//! int rc = mth_table_session_new(config, &table);
//! if (rc != 0) {
//!     /* Read mth_last_error_message because no table handle exists. */
//!     return rc;
//! }
//! rc = mth_table_start_next_hand(table);
//! while (rc == 0) {
//!     MthTableEvent event;
//!     rc = mth_table_next_event(table, &event);
//!     if (rc == MTH_ERROR_NO_EFFECT) {
//!         break;
//!     }
//!     if (rc != 0) {
//!         break;
//!     }
//!     if (event.kind == MTH_TABLE_EVENT_ACTION_REQUEST) {
//!         /* Submit one legal action with mth_table_submit_action. */
//!     }
//!     mth_table_event_free(event);
//! }
//! mth_table_session_free(table);
//! ```
//!
//! The table ABI currently implements `MTH_TABLE_BACKEND_RULES_ONLY`. Asking for
//! `MTH_TABLE_BACKEND_PEER` returns a configuration error rather than silently
//! falling back, because peer-backed local tables need their own event extraction.
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
use std::mem::{size_of, size_of_val};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::slice;

use mental_card_core::error::Error;
use mental_card_core::identity::{IDENTITY_PUBLIC_KEY_BYTES, IdentityPublic, IdentitySecret};
use mtholdem::betting::{BetAction, LegalActions, Street};
use mtholdem::config::{EconomicSecurity, Roster, SessionConfig};
use mtholdem::holdem::{HandProgress, HoldemHand};
use mtholdem::types::{MAX_PLAYERS, MIN_PLAYERS};
use mtholdem::{Card, Chips, HandNumber, Rank, Seat, SessionId, Suit};
use mtholdem_adapter::{
    ABI_VERSION_MAJOR, ABI_VERSION_MINOR, ABI_VERSION_PATCH, BoundaryAction, BoundaryActionKind,
    BoundaryEffect, BoundaryEffectKind, BoundaryErrorCode, BoundaryPeerPhase,
    decode_certified_checkpoint, decode_showdown_bundle, encode_checkpoint_state, encode_effects,
    encode_recovery_bundle,
};
use mtholdem_bot::{BotDecisionContext, BotPreset, HeuristicBot};
use mtholdem_peer::PeerSession;
use mtholdem_table::{
    ActionRequest, ActionTaken, BoardOpened, HandFinished, HandStarted, HoleCards, SessionFinished,
    Showdown, TableBackend, TableEvent as RustTableEvent, TableShowdown,
};
use rand::SeedableRng;
use rand::rngs::{OsRng, StdRng};
use rand::seq::SliceRandom;

thread_local! {
    static LAST_ERROR: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

const PANIC_ERROR_MESSAGE: &str = "Rust panic crossed the C ABI boundary";

/// Operation completed successfully.
pub const MTH_ERROR_SUCCESS: i32 = BoundaryErrorCode::Success as i32;
/// A required pointer or handle was null at a host boundary.
pub const MTH_ERROR_NULL_POINTER: i32 = BoundaryErrorCode::NullPointer as i32;
/// The caller-provided output buffer was too small.
pub const MTH_ERROR_BUFFER_TOO_SMALL: i32 = BoundaryErrorCode::BufferTooSmall as i32;
/// A primitive argument was outside its valid domain.
pub const MTH_ERROR_INVALID_ARGUMENT: i32 = BoundaryErrorCode::InvalidArgument as i32;
/// No queued event or optional value was available.
pub const MTH_ERROR_NO_EFFECT: i32 = BoundaryErrorCode::NoEffect as i32;
/// Session or table configuration was invalid.
pub const MTH_ERROR_CONFIGURATION: i32 = BoundaryErrorCode::Configuration as i32;
/// Wire bytes were malformed, truncated, non-canonical, or too large.
pub const MTH_ERROR_CODEC: i32 = BoundaryErrorCode::Codec as i32;
/// Signature, identity, sender, or replay validation failed.
pub const MTH_ERROR_AUTHENTICATION: i32 = BoundaryErrorCode::Authentication as i32;
/// Cryptographic proof, encrypted deck, or reveal share validation failed.
pub const MTH_ERROR_CRYPTOGRAPHY: i32 = BoundaryErrorCode::Cryptography as i32;
/// Betting or settlement rules rejected the requested transition.
pub const MTH_ERROR_RULES: i32 = BoundaryErrorCode::Rules as i32;
/// A valid artifact was unexpected in the current protocol phase.
pub const MTH_ERROR_PROTOCOL: i32 = BoundaryErrorCode::Protocol as i32;
/// Checkpoint or recovery evidence validation failed.
pub const MTH_ERROR_RECOVERY: i32 = BoundaryErrorCode::Recovery as i32;
/// An internal invariant failed before a typed error could be reported.
pub const MTH_ERROR_INTERNAL: i32 = BoundaryErrorCode::Internal as i32;

/// Fold the current hand.
pub const MTH_ACTION_FOLD: u32 = BoundaryActionKind::Fold as u32;
/// Check when no call is required.
pub const MTH_ACTION_CHECK: u32 = BoundaryActionKind::Check as u32;
/// Call the current bet.
pub const MTH_ACTION_CALL: u32 = BoundaryActionKind::Call as u32;
/// Raise to the supplied absolute chip amount.
pub const MTH_ACTION_RAISE_TO: u32 = BoundaryActionKind::RaiseTo as u32;
/// Commit the remaining stack.
pub const MTH_ACTION_ALL_IN: u32 = BoundaryActionKind::AllIn as u32;

/// Broadcast one signed public message.
pub const MTH_EFFECT_BROADCAST_PUBLIC: u32 = BoundaryEffectKind::BroadcastPublic as u32;
/// Send one signed private reveal to one recipient.
pub const MTH_EFFECT_SEND_PRIVATE: u32 = BoundaryEffectKind::SendPrivate as u32;
/// Persist the transcript anchor and optional checkpoint bytes.
pub const MTH_EFFECT_PERSIST: u32 = BoundaryEffectKind::Persist as u32;
/// Ask the host for a legal local action.
pub const MTH_EFFECT_REQUEST_ACTION: u32 = BoundaryEffectKind::RequestAction as u32;
/// Submit recovery evidence to a mediator.
pub const MTH_EFFECT_SUBMIT_RECOVERY: u32 = BoundaryEffectKind::SubmitRecovery as u32;
/// Publish one showdown side artifact.
pub const MTH_EFFECT_PUBLISH_SHOWDOWN_BUNDLE: u32 =
    BoundaryEffectKind::PublishShowdownBundle as u32;
/// Report a final hand result.
pub const MTH_EFFECT_FINISHED: u32 = BoundaryEffectKind::Finished as u32;

/// Waiting for key announcements.
pub const MTH_PHASE_KEY_SETUP: u32 = BoundaryPeerPhase::KeySetup as u32;
/// Waiting for shuffle contributions.
pub const MTH_PHASE_SHUFFLE: u32 = BoundaryPeerPhase::Shuffle as u32;
/// Delivering private reveals and receipt commitments.
pub const MTH_PHASE_PRIVATE_REVEAL: u32 = BoundaryPeerPhase::PrivateReveal as u32;
/// All hole-card receipts are ready and betting can start.
pub const MTH_PHASE_HOLE_CARDS_READY: u32 = BoundaryPeerPhase::HoleCardsReady as u32;
/// A betting action is expected.
pub const MTH_PHASE_BETTING: u32 = BoundaryPeerPhase::Betting as u32;
/// Public board-card reveal shares are expected.
pub const MTH_PHASE_BOARD_REVEAL: u32 = BoundaryPeerPhase::BoardReveal as u32;
/// Showdown side artifacts are expected.
pub const MTH_PHASE_SHOWDOWN: u32 = BoundaryPeerPhase::Showdown as u32;
/// The hand is complete.
pub const MTH_PHASE_COMPLETE: u32 = BoundaryPeerPhase::Complete as u32;

/// Peer-backed local table execution.
pub const MTH_TABLE_BACKEND_PEER: u32 = 1;
/// Lightweight rules-only local table execution.
pub const MTH_TABLE_BACKEND_RULES_ONLY: u32 = 2;

/// A hand started.
pub const MTH_TABLE_EVENT_HAND_STARTED: u32 = 1;
/// Local-visible hole cards are available.
pub const MTH_TABLE_EVENT_HOLE_CARDS: u32 = 2;
/// A human-controlled seat must choose an action.
pub const MTH_TABLE_EVENT_ACTION_REQUEST: u32 = 3;
/// A seat took a public betting action.
pub const MTH_TABLE_EVENT_ACTION_TAKEN: u32 = 4;
/// A public board street opened.
pub const MTH_TABLE_EVENT_BOARD_OPENED: u32 = 5;
/// One or more hands opened at showdown.
pub const MTH_TABLE_EVENT_SHOWDOWN: u32 = 6;
/// A hand finished.
pub const MTH_TABLE_EVENT_HAND_FINISHED: u32 = 7;
/// The local table session finished.
pub const MTH_TABLE_EVENT_SESSION_FINISHED: u32 = 8;

/// Nit bot preset.
pub const MTH_BOT_PRESET_NIT: u32 = 1;
/// Balanced bot preset.
pub const MTH_BOT_PRESET_BALANCED: u32 = 2;
/// Loose-passive bot preset.
pub const MTH_BOT_PRESET_LOOSE_PASSIVE: u32 = 3;
/// Loose-aggressive bot preset.
pub const MTH_BOT_PRESET_LAG: u32 = 4;
/// Maniac bot preset.
pub const MTH_BOT_PRESET_MANIAC: u32 = 5;

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

/// Opaque local table-session handle owned by the C ABI.
///
/// The first implementation supports the explicit rules-only backend. Peer-backed
/// table sessions remain a separate extraction step and are rejected instead of
/// silently falling back.
pub struct MthTableSession {
    config: TableConfig,
    stacks: Vec<Chips>,
    button: Seat,
    completed_hands: u64,
    active_hand: Option<RulesOnlyHand>,
    pending_events: VecDeque<RustTableEvent>,
    awaiting_action: Option<ActionRequest>,
    bots: Vec<HeuristicBot>,
    rng: StdRng,
    last_error: Vec<u8>,
    session_finished: bool,
}

#[derive(Clone, Copy, Debug)]
struct TableConfig {
    backend: TableBackend,
    player_count: usize,
    human: Option<Seat>,
    starting_stack: Chips,
    small_blind: Chips,
    big_blind: Chips,
    hand_limit: Option<u64>,
}

#[derive(Debug)]
struct RulesOnlyHand {
    hand_number: HandNumber,
    hand: HoldemHand,
    deck: Vec<Card>,
    hole_cards: Vec<Option<[Card; 2]>>,
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

/// Configuration for a local table session.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct MthTableConfig {
    /// One of the `MTH_TABLE_BACKEND_*` constants.
    pub backend: u32,
    /// Number of seats at the table.
    pub player_count: usize,
    /// Whether `human_seat` is used.
    pub has_human_seat: bool,
    /// Human-controlled seat for action-request events.
    pub human_seat: u8,
    /// One of the `MTH_BOT_PRESET_*` constants.
    pub bot_preset: u32,
    /// Small blind in smallest chip units.
    pub small_blind_units: u64,
    /// Big blind in smallest chip units.
    pub big_blind_units: u64,
    /// Initial stack in smallest chip units.
    pub starting_stack_units: u64,
    /// Whether `hand_limit` is used.
    pub has_hand_limit: bool,
    /// Maximum number of hands to run when `has_hand_limit` is true.
    pub hand_limit: u64,
    /// Deterministic local RNG seed for rules-only local play.
    pub seed: u64,
    /// Whether `seat_presets_ptr` is used.
    pub has_seat_presets: bool,
    /// Pointer to `player_count` `MTH_BOT_PRESET_*` codes in seat order, borrowed
    /// only during construction. Every entry must be a valid preset code; the
    /// human seat's entry is validated but never drives an action.
    pub seat_presets_ptr: *const u32,
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
    /// Whether public action-context fields are meaningful.
    pub has_action_context: bool,
    /// Dealer button seat index for the current hand.
    pub button_seat: u8,
    /// Current pot size before the actor chooses.
    pub pot_units: u64,
    /// Actor's remaining stack before choosing.
    pub actor_stack_units: u64,
    /// Actor's contribution on the current street.
    pub actor_street_contributed_units: u64,
    /// Whether `effective_stack_units` is meaningful.
    pub has_effective_stack: bool,
    /// Smallest remaining playing-opponent stack.
    pub effective_stack_units: u64,
    /// Number of live opponents who can still contest the pot.
    pub live_opponents: u8,
    /// Number of opponents that have matched the current bet.
    pub matched_bet_opponents: u8,
    /// Configured big blind.
    pub big_blind_units: u64,
    /// Current highest required street contribution.
    pub current_bet_units: u64,
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

/// One local table event returned across the C ABI.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct MthTableEvent {
    /// One of the `MTH_TABLE_EVENT_*` constants.
    pub kind: u32,
    /// One of the `MTH_TABLE_BACKEND_*` constants.
    pub backend: u32,
    /// Current hand number.
    pub hand: u64,
    /// Whether `button_seat` is meaningful.
    pub has_button: bool,
    /// Dealer button seat.
    pub button_seat: u8,
    /// Whether `actor` is meaningful.
    pub has_actor: bool,
    /// Acting or reported seat.
    pub actor: u8,
    /// Current street as `mtholdem::betting::Street` discriminant.
    pub street: u32,
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
    /// Whether a full raise is reopened.
    pub raise_reopened: bool,
    /// Whether checking is legal.
    pub can_check: bool,
    /// Whether public action-context fields are meaningful.
    pub has_action_context: bool,
    /// Current pot before the actor chooses.
    pub pot_units: u64,
    /// Actor stack before choosing.
    pub actor_stack_units: u64,
    /// Actor street contribution before choosing.
    pub actor_street_contributed_units: u64,
    /// Whether `effective_stack_units` is meaningful.
    pub has_effective_stack: bool,
    /// Smallest live opponent stack.
    pub effective_stack_units: u64,
    /// Live opponent count.
    pub live_opponents: u8,
    /// Matched-bet opponent count.
    pub matched_bet_opponents: u8,
    /// Big blind.
    pub big_blind_units: u64,
    /// Current highest street contribution.
    pub current_bet_units: u64,
    /// Whether action fields are meaningful.
    pub has_action: bool,
    /// One of the `MTH_ACTION_*` constants.
    pub action_kind: u32,
    /// Raise-to amount for raise actions.
    pub raise_to_units: u64,
    /// Whether post-action totals are meaningful.
    pub has_post_action_totals: bool,
    /// Pot size after the action was applied.
    pub pot_after_units: u64,
    /// Actor's remaining stack after the action was applied.
    pub actor_stack_after_units: u64,
    /// Board cards as card indices.
    pub board_cards: MthBuffer,
    /// Visible hole cards as two card indices.
    pub hole_cards: MthBuffer,
    /// Starting stacks as little-endian `u64` units.
    pub starting_stacks: MthBuffer,
    /// Final stacks as little-endian `u64` units.
    pub final_stacks: MthBuffer,
    /// Showdown rows: seat, first card, second card, category.
    pub showdown_rows: MthBuffer,
    /// Completed hand count for session-finished events.
    pub completed_hands: u64,
}

impl MthTableEvent {
    const fn empty() -> Self {
        Self {
            kind: 0,
            backend: 0,
            hand: 0,
            has_button: false,
            button_seat: 0,
            has_actor: false,
            actor: 0,
            street: 0,
            has_legal_actions: false,
            to_call_units: 0,
            has_minimum_raise_to: false,
            minimum_raise_to_units: 0,
            maximum_raise_to_units: 0,
            raise_reopened: false,
            can_check: false,
            has_action_context: false,
            pot_units: 0,
            actor_stack_units: 0,
            actor_street_contributed_units: 0,
            has_effective_stack: false,
            effective_stack_units: 0,
            live_opponents: 0,
            matched_bet_opponents: 0,
            big_blind_units: 0,
            current_bet_units: 0,
            has_action: false,
            action_kind: 0,
            raise_to_units: 0,
            has_post_action_totals: false,
            pot_after_units: 0,
            actor_stack_after_units: 0,
            board_cards: MthBuffer::empty(),
            hole_cards: MthBuffer::empty(),
            starting_stacks: MthBuffer::empty(),
            final_stacks: MthBuffer::empty(),
            showdown_rows: MthBuffer::empty(),
            completed_hands: 0,
        }
    }
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
            has_action_context: false,
            button_seat: 0,
            pot_units: 0,
            actor_stack_units: 0,
            actor_street_contributed_units: 0,
            has_effective_stack: false,
            effective_stack_units: 0,
            live_opponents: 0,
            matched_bet_opponents: 0,
            big_blind_units: 0,
            current_bet_units: 0,
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
        if let Some(context) = value.action_context() {
            effect.has_action_context = true;
            effect.button_seat = context.button_seat();
            effect.pot_units = context.pot_units();
            effect.actor_stack_units = context.actor_stack_units();
            effect.actor_street_contributed_units = context.actor_street_contributed_units();
            context.effective_stack_units().inspect(|effective| {
                effect.has_effective_stack = true;
                effect.effective_stack_units = *effective;
            });
            effect.live_opponents = context.live_opponents();
            effect.matched_bet_opponents = context.matched_bet_opponents();
            effect.big_blind_units = context.big_blind_units();
            effect.current_bet_units = context.current_bet_units();
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

impl From<RustTableEvent> for MthTableEvent {
    fn from(value: RustTableEvent) -> Self {
        let mut event = Self::empty();
        match value {
            RustTableEvent::HandStarted(started) => {
                event.kind = MTH_TABLE_EVENT_HAND_STARTED;
                event.backend = table_backend_code(started.backend);
                event.hand = started.hand.to_u64();
                event.has_button = true;
                event.button_seat = started.button.to_u8();
                event.starting_stacks =
                    MthBuffer::from_vec(encode_chips_slice(&started.starting_stacks));
            }
            RustTableEvent::HoleCards(hole) => {
                event.kind = MTH_TABLE_EVENT_HOLE_CARDS;
                event.backend = table_backend_code(hole.backend);
                event.hand = hole.hand.to_u64();
                event.has_actor = true;
                event.actor = hole.seat.to_u8();
                event.hole_cards = MthBuffer::from_vec(encode_cards(&hole.cards));
            }
            RustTableEvent::ActionRequest(request) => {
                event.kind = MTH_TABLE_EVENT_ACTION_REQUEST;
                event.backend = table_backend_code(request.backend);
                event.hand = request.hand.to_u64();
                event.has_actor = true;
                event.actor = request.actor.to_u8();
                event.street = street_code(request.street);
                fill_legal_actions(&mut event, request.legal_actions);
                fill_action_context(&mut event, request.action_context);
            }
            RustTableEvent::ActionTaken(taken) => {
                event.kind = MTH_TABLE_EVENT_ACTION_TAKEN;
                event.backend = table_backend_code(taken.backend);
                event.hand = taken.hand.to_u64();
                event.has_actor = true;
                event.actor = taken.actor.to_u8();
                event.street = street_code(taken.street);
                fill_legal_actions(&mut event, taken.legal_actions);
                fill_action(&mut event, taken.action);
                event.has_post_action_totals = true;
                event.pot_after_units = taken.pot_after.units();
                event.actor_stack_after_units = taken.actor_stack_after.units();
            }
            RustTableEvent::BoardOpened(board) => {
                event.kind = MTH_TABLE_EVENT_BOARD_OPENED;
                event.backend = table_backend_code(board.backend);
                event.hand = board.hand.to_u64();
                event.street = street_code(board.street);
                event.board_cards = MthBuffer::from_vec(encode_cards(&board.board));
            }
            RustTableEvent::Showdown(showdown) => {
                event.kind = MTH_TABLE_EVENT_SHOWDOWN;
                event.backend = table_backend_code(showdown.backend);
                event.hand = showdown.hand.to_u64();
                event.showdown_rows = MthBuffer::from_vec(encode_showdown_rows(&showdown.showdown));
            }
            RustTableEvent::HandFinished(finished) => {
                event.kind = MTH_TABLE_EVENT_HAND_FINISHED;
                event.backend = table_backend_code(finished.backend);
                event.hand = finished.hand.to_u64();
                event.final_stacks =
                    MthBuffer::from_vec(encode_chips_slice(&finished.final_stacks));
            }
            RustTableEvent::SessionFinished(finished) => {
                event.kind = MTH_TABLE_EVENT_SESSION_FINISHED;
                event.completed_hands = finished.completed_hands;
                event.final_stacks =
                    MthBuffer::from_vec(encode_chips_slice(&finished.final_stacks));
            }
        }
        event
    }
}

fn fill_legal_actions(event: &mut MthTableEvent, legal: LegalActions) {
    event.has_legal_actions = true;
    event.to_call_units = legal.to_call().units();
    if let Some(minimum) = legal.minimum_raise_to() {
        event.has_minimum_raise_to = true;
        event.minimum_raise_to_units = minimum.units();
    }
    event.maximum_raise_to_units = legal.maximum_raise_to().units();
    event.raise_reopened = legal.raise_reopened();
    event.can_check = legal.can_check();
}

fn fill_action_context(event: &mut MthTableEvent, context: mtholdem::betting::ActionContext) {
    event.has_action_context = true;
    event.has_button = true;
    event.button_seat = context.button().to_u8();
    event.pot_units = context.pot().units();
    event.actor_stack_units = context.actor_stack().units();
    event.actor_street_contributed_units = context.actor_street_contributed().units();
    if let Some(effective) = context.effective_stack() {
        event.has_effective_stack = true;
        event.effective_stack_units = effective.units();
    }
    event.live_opponents = u8::try_from(context.live_opponents()).unwrap_or(u8::MAX);
    event.matched_bet_opponents = u8::try_from(context.matched_bet_opponents()).unwrap_or(u8::MAX);
    event.big_blind_units = context.big_blind().units();
    event.current_bet_units = context.current_bet().units();
}

fn fill_action(event: &mut MthTableEvent, action: BetAction) {
    event.has_action = true;
    let boundary = BoundaryAction::from(action);
    event.action_kind = boundary_action_kind_code(boundary.kind());
    event.raise_to_units = boundary.raise_to_units();
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

/// ABI version and layout metadata returned across the C ABI.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct MthAbiLayout {
    /// C ABI major version.
    pub version_major: u32,
    /// C ABI minor version.
    pub version_minor: u32,
    /// C ABI patch version.
    pub version_patch: u32,
    /// `sizeof(MthSessionConfig)` for the compiled Rust library.
    pub session_config_size: usize,
    /// `sizeof(MthTableConfig)` for the compiled Rust library.
    pub table_config_size: usize,
    /// `sizeof(MthBuffer)` for the compiled Rust library.
    pub buffer_size: usize,
    /// `sizeof(MthEffect)` for the compiled Rust library.
    pub effect_size: usize,
    /// `sizeof(MthTableEvent)` for the compiled Rust library.
    pub table_event_size: usize,
    /// `sizeof(MthPeerStatus)` for the compiled Rust library.
    pub peer_status_size: usize,
    /// `sizeof(MthAbiLayout)` for the compiled Rust library.
    pub abi_layout_size: usize,
}

impl MthAbiLayout {
    const fn current() -> Self {
        Self {
            version_major: ABI_VERSION_MAJOR,
            version_minor: ABI_VERSION_MINOR,
            version_patch: ABI_VERSION_PATCH,
            session_config_size: size_of::<MthSessionConfig>(),
            table_config_size: size_of::<MthTableConfig>(),
            buffer_size: size_of::<MthBuffer>(),
            effect_size: size_of::<MthEffect>(),
            table_event_size: size_of::<MthTableEvent>(),
            peer_status_size: size_of::<MthPeerStatus>(),
            abi_layout_size: size_of::<MthAbiLayout>(),
        }
    }
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

/// Copies ABI version and layout metadata into `out_layout`.
///
/// # Safety
///
/// `out_layout` must be writable for one [`MthAbiLayout`] value when non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_abi_layout(out_layout: *mut MthAbiLayout) -> i32 {
    if out_layout.is_null() {
        return code(BoundaryErrorCode::NullPointer);
    }
    // SAFETY: `out_layout` was checked for null and receives a plain C-compatible value.
    unsafe {
        *out_layout = MthAbiLayout::current();
    }
    code(BoundaryErrorCode::Success)
}

/// Checks that a host compiled against the same ABI version and struct sizes.
#[unsafe(no_mangle)]
pub const extern "C" fn mth_abi_check_layout(layout: MthAbiLayout) -> i32 {
    let current = MthAbiLayout::current();
    if layout.version_major != current.version_major
        || layout.version_minor != current.version_minor
        || layout.version_patch != current.version_patch
        || layout.session_config_size != current.session_config_size
        || layout.table_config_size != current.table_config_size
        || layout.buffer_size != current.buffer_size
        || layout.effect_size != current.effect_size
        || layout.table_event_size != current.table_event_size
        || layout.peer_status_size != current.peer_status_size
        || layout.abi_layout_size != current.abi_layout_size
    {
        return MTH_ERROR_INVALID_ARGUMENT;
    }
    MTH_ERROR_SUCCESS
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

/// Frees owned buffers inside a table event returned by this library.
///
/// # Safety
///
/// `event` must be a live event value returned by this library, and it must not
/// have been freed before. Callers must preserve embedded buffer fields exactly.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_table_event_free(event: MthTableEvent) {
    // SAFETY: The caller's contract for this event covers every embedded buffer.
    unsafe {
        mth_buffer_free(event.board_cards);
        mth_buffer_free(event.hole_cards);
        mth_buffer_free(event.starting_stacks);
        mth_buffer_free(event.final_stacks);
        mth_buffer_free(event.showdown_rows);
    }
}

/// Creates a local table-session handle.
///
/// # Safety
///
/// `out_handle` must be writable for one pointer value when non-null.
/// When `config.has_seat_presets` is true, `config.seat_presets_ptr` must point
/// at `config.player_count` readable u32 values for the duration of this call;
/// the codes are copied and the pointer is not retained.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_table_session_new(
    config: MthTableConfig,
    out_handle: *mut *mut MthTableSession,
) -> i32 {
    if out_handle.is_null() {
        return code(BoundaryErrorCode::NullPointer);
    }
    // SAFETY: `out_handle` was checked for null and receives null or an owned box pointer.
    unsafe {
        *out_handle = ptr::null_mut();
    }
    let Ok(result) = catch_unwind(AssertUnwindSafe(|| build_table_session(config))) else {
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

/// Frees a table-session handle returned by `mth_table_session_new`.
///
/// # Safety
///
/// `handle` must be null or a live pointer returned by [`mth_table_session_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_table_session_free(handle: *mut MthTableSession) {
    if handle.is_null() {
        return;
    }
    // SAFETY: The pointer must have been returned by `mth_table_session_new` and not freed yet.
    unsafe {
        drop(Box::from_raw(handle));
    }
}

/// Starts the next local hand and queues table events until a human action is needed.
///
/// # Safety
///
/// `handle` must be null or a live table-session pointer returned by this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_table_start_next_hand(handle: *mut MthTableSession) -> i32 {
    with_table_handle(handle, MthTableSession::start_next_hand)
}

/// Submits a human action for the current table action request.
///
/// # Safety
///
/// `handle` must be null or a live table-session pointer returned by this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_table_submit_action(
    handle: *mut MthTableSession,
    action_kind: u32,
    raise_to_units: u64,
) -> i32 {
    let Some(kind) = action_kind_from_code(action_kind) else {
        return code(BoundaryErrorCode::InvalidArgument);
    };
    with_table_handle(handle, |handle| {
        handle.submit_action(BoundaryAction::new(kind, raise_to_units).to_bet_action())
    })
}

/// Pops the next queued local table event.
///
/// `NoEffect` means the queue is empty and is not a protocol failure.
///
/// # Safety
///
/// `handle` must be null or a live table-session pointer returned by this library.
/// `out_event` must be writable for one [`MthTableEvent`] value when non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_table_next_event(
    handle: *mut MthTableSession,
    out_event: *mut MthTableEvent,
) -> i32 {
    if out_event.is_null() {
        return code(BoundaryErrorCode::NullPointer);
    }
    let Some(handle) = checked_table_handle(handle) else {
        return code(BoundaryErrorCode::NullPointer);
    };
    match catch_unwind(AssertUnwindSafe(|| {
        let Some(event) = handle.pending_events.pop_front() else {
            return code(BoundaryErrorCode::NoEffect);
        };
        // SAFETY: `out_event` was checked for null and receives a plain C-compatible value.
        unsafe {
            *out_event = MthTableEvent::from(event);
        }
        handle.last_error.clear();
        clear_global_error();
        code(BoundaryErrorCode::Success)
    })) {
        Ok(status) => status,
        Err(_) => store_table_panic(handle),
    }
}

/// Copies the last error message for this table handle into a caller-owned buffer.
///
/// # Safety
///
/// `handle` must be null or a live table-session pointer returned by this library.
/// `out_required_len` must be writable for one `usize` when non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mth_table_session_last_error(
    handle: *mut MthTableSession,
    out_ptr: *mut u8,
    out_len: usize,
    out_required_len: *mut usize,
) -> i32 {
    let Some(handle) = checked_table_handle(handle) else {
        return code(BoundaryErrorCode::NullPointer);
    };
    copy_error_message(&handle.last_error, out_ptr, out_len, out_required_len)
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

fn build_table_session(config: MthTableConfig) -> Result<MthTableSession, Error> {
    let backend = table_backend_from_code(config.backend)?;
    if backend != TableBackend::RulesOnly {
        return Err(mental_card_core::error::configuration(
            "mth_table peer backend is not implemented yet",
        ));
    }
    if !(MIN_PLAYERS..=MAX_PLAYERS).contains(&config.player_count) {
        return Err(mental_card_core::error::configuration(format!(
            "table player count must be {MIN_PLAYERS}..={MAX_PLAYERS}"
        )));
    }
    let small_blind = Chips::from_units(config.small_blind_units);
    let big_blind = Chips::from_units(config.big_blind_units);
    let starting_stack = Chips::from_units(config.starting_stack_units);
    if small_blind.is_zero() || big_blind <= small_blind {
        return Err(mental_card_core::error::configuration(
            "blinds must satisfy 0 < small blind < big blind",
        ));
    }
    if starting_stack < big_blind {
        return Err(mental_card_core::error::configuration(
            "starting stack must cover the big blind",
        ));
    }
    let human = if config.has_human_seat {
        let seat = Seat::from_u8(config.human_seat)?;
        if seat.index() >= config.player_count {
            return Err(mental_card_core::error::configuration(
                "human seat is outside the table",
            ));
        }
        Some(seat)
    } else {
        None
    };
    let hand_limit = if config.has_hand_limit {
        Some(config.hand_limit)
    } else {
        None
    };
    let table_preset = bot_preset_from_code(config.bot_preset)?;
    let seat_presets: Vec<BotPreset> = if config.has_seat_presets {
        if config.seat_presets_ptr.is_null() {
            return Err(mental_card_core::error::configuration(
                "seat presets pointer is null while has_seat_presets is set",
            ));
        }
        // SAFETY: The `mth_table_session_new` contract requires `seat_presets_ptr`
        // to point at `player_count` u32 codes when `has_seat_presets` is true, and
        // `player_count` was validated above.
        let codes = unsafe { slice::from_raw_parts(config.seat_presets_ptr, config.player_count) };
        codes
            .iter()
            .map(|&value| bot_preset_from_code(value))
            .collect::<Result<_, _>>()?
    } else {
        vec![table_preset; config.player_count]
    };
    let table_config = TableConfig {
        backend,
        player_count: config.player_count,
        human,
        starting_stack,
        small_blind,
        big_blind,
        hand_limit,
    };
    Ok(MthTableSession {
        config: table_config,
        stacks: vec![starting_stack; config.player_count],
        button: Seat::new(0)?,
        completed_hands: 0,
        active_hand: None,
        pending_events: VecDeque::new(),
        awaiting_action: None,
        bots: seat_presets
            .into_iter()
            .map(HeuristicBot::from_preset)
            .collect(),
        rng: StdRng::seed_from_u64(config.seed),
        last_error: Vec::new(),
        session_finished: false,
    })
}

impl MthTableSession {
    fn start_next_hand(&mut self) -> Result<(), Error> {
        if self.active_hand.is_some() || self.awaiting_action.is_some() {
            return Err(mental_card_core::error::protocol(
                "table hand is already active",
            ));
        }
        if self.session_should_finish() {
            self.enqueue_session_finished();
            return Ok(());
        }
        let hand_number = HandNumber::from_u64(self.completed_hands);
        let mut deck = shuffled_deck(&mut self.rng);
        let mut hole_cards = vec![None; self.config.player_count];
        for seat in active_seats_in_deal_order(self.button, &self.stacks) {
            let first = draw_card(&mut deck)?;
            let second = draw_card(&mut deck)?;
            hole_cards[seat.index()] = Some([first, second]);
        }
        let hand = HoldemHand::new(
            &self.stacks,
            self.button,
            self.config.small_blind,
            self.config.big_blind,
            self.config.starting_stack,
        )?;
        self.pending_events
            .push_back(RustTableEvent::HandStarted(HandStarted {
                hand: hand_number,
                backend: self.config.backend,
                button: self.button,
                starting_stacks: self.stacks.clone(),
            }));
        if let Some(human) = self.config.human {
            if let Some(cards) = hole_cards[human.index()] {
                self.pending_events
                    .push_back(RustTableEvent::HoleCards(HoleCards {
                        hand: hand_number,
                        backend: self.config.backend,
                        seat: human,
                        cards,
                    }));
            }
        }
        self.active_hand = Some(RulesOnlyHand {
            hand_number,
            hand,
            deck,
            hole_cards,
        });
        self.progress_until_blocked()
    }

    fn submit_action(&mut self, action: BetAction) -> Result<(), Error> {
        // The pending request is only consumed once the action is accepted.
        // A rejected action must leave the prompt pending so the caller can
        // resubmit; otherwise one illegal action would wedge the session.
        let request = self
            .awaiting_action
            .ok_or_else(|| mental_card_core::error::protocol("no table action is pending"))?;
        let Some(active) = self.active_hand.as_mut() else {
            return Err(mental_card_core::error::protocol(
                "table action has no active hand",
            ));
        };
        let actor = active
            .hand
            .next_to_act()
            .ok_or_else(|| mental_card_core::error::protocol("table action has no active actor"))?;
        if actor != request.actor {
            return Err(mental_card_core::error::protocol(
                "table action actor changed before submission",
            ));
        }
        active.hand.apply_action(actor, action)?;
        self.awaiting_action = None;
        let hand_number = active.hand_number;
        let pot_after = active.hand.ledger().pot()?;
        let actor_stack_after = active.hand.ledger().stack(actor)?;
        self.pending_events
            .push_back(RustTableEvent::ActionTaken(ActionTaken {
                hand: hand_number,
                backend: self.config.backend,
                actor,
                street: request.street,
                action,
                legal_actions: request.legal_actions,
                pot_after,
                actor_stack_after,
            }));
        self.progress_until_blocked()
    }

    fn progress_until_blocked(&mut self) -> Result<(), Error> {
        loop {
            let progress = self
                .active_hand
                .as_ref()
                .map(|active| active.hand.progress())
                .ok_or_else(|| mental_card_core::error::protocol("no active table hand"))?;
            match progress {
                HandProgress::Betting(street) => {
                    let request = self.current_action_request(street)?;
                    if self.config.human == Some(request.actor) {
                        self.awaiting_action = Some(request);
                        self.pending_events
                            .push_back(RustTableEvent::ActionRequest(request));
                        return Ok(());
                    }
                    self.apply_bot_action(request)?;
                }
                HandProgress::AwaitingFlop => {
                    let (hand, board) = {
                        let active = self.active_hand_mut()?;
                        let flop = [
                            draw_card(&mut active.deck)?,
                            draw_card(&mut active.deck)?,
                            draw_card(&mut active.deck)?,
                        ];
                        active.hand.open_flop(flop)?;
                        (active.hand_number, active.hand.board().to_vec())
                    };
                    self.pending_events
                        .push_back(RustTableEvent::BoardOpened(BoardOpened {
                            hand,
                            backend: self.config.backend,
                            street: Street::Flop,
                            board,
                        }));
                }
                HandProgress::AwaitingTurn => {
                    let (hand, board) = {
                        let active = self.active_hand_mut()?;
                        active.hand.open_turn(draw_card(&mut active.deck)?)?;
                        (active.hand_number, active.hand.board().to_vec())
                    };
                    self.pending_events
                        .push_back(RustTableEvent::BoardOpened(BoardOpened {
                            hand,
                            backend: self.config.backend,
                            street: Street::Turn,
                            board,
                        }));
                }
                HandProgress::AwaitingRiver => {
                    let (hand, board) = {
                        let active = self.active_hand_mut()?;
                        active.hand.open_river(draw_card(&mut active.deck)?)?;
                        (active.hand_number, active.hand.board().to_vec())
                    };
                    self.pending_events
                        .push_back(RustTableEvent::BoardOpened(BoardOpened {
                            hand,
                            backend: self.config.backend,
                            street: Street::River,
                            board,
                        }));
                }
                HandProgress::Showdown => self.finish_showdown()?,
                HandProgress::Uncontested(_) => self.finish_uncontested()?,
                HandProgress::Complete => self.finish_complete()?,
                _ => {
                    return Err(mental_card_core::error::protocol(
                        "rules-only backend does not support this hand phase",
                    ));
                }
            }
            if self.active_hand.is_none() {
                if self.session_should_finish() {
                    self.enqueue_session_finished();
                }
                return Ok(());
            }
        }
    }

    fn current_action_request(&self, street: Street) -> Result<ActionRequest, Error> {
        let active = self
            .active_hand
            .as_ref()
            .ok_or_else(|| mental_card_core::error::protocol("no active table hand"))?;
        let actor = active
            .hand
            .next_to_act()
            .ok_or_else(|| mental_card_core::error::protocol("rules-only hand has no actor"))?;
        Ok(ActionRequest {
            hand: active.hand_number,
            backend: self.config.backend,
            actor,
            street,
            legal_actions: active.hand.legal_actions()?,
            action_context: active.hand.action_context()?,
        })
    }

    fn apply_bot_action(&mut self, request: ActionRequest) -> Result<(), Error> {
        let active_player_count = self.active_player_count();
        let player_count = self.config.player_count;
        let (hole_cards, board) = {
            let active = self
                .active_hand
                .as_ref()
                .ok_or_else(|| mental_card_core::error::protocol("no active table hand"))?;
            (
                active.hole_cards[request.actor.index()],
                active.hand.board().to_vec(),
            )
        };
        let context = BotDecisionContext::new(
            player_count,
            request.actor,
            request.street,
            request.legal_actions,
        )
        .with_dealt_player_count(active_player_count)
        .with_hole_cards(hole_cards)
        .with_board(board)
        .with_action_context(request.action_context)
        .with_postflop_initiative(false);
        let bot = self.bots.get(request.actor.index()).ok_or_else(|| {
            mental_card_core::error::protocol("bot actor seat is outside the table")
        })?;
        let action = bot.decide(&context, &mut self.rng).action;
        let active = self.active_hand_mut()?;
        let hand_number = active.hand_number;
        active.hand.apply_action(request.actor, action)?;
        let pot_after = active.hand.ledger().pot()?;
        let actor_stack_after = active.hand.ledger().stack(request.actor)?;
        self.pending_events
            .push_back(RustTableEvent::ActionTaken(ActionTaken {
                hand: hand_number,
                backend: self.config.backend,
                actor: request.actor,
                street: request.street,
                action,
                legal_actions: request.legal_actions,
                pot_after,
                actor_stack_after,
            }));
        Ok(())
    }

    fn finish_showdown(&mut self) -> Result<(), Error> {
        let mut active = self
            .active_hand
            .take()
            .ok_or_else(|| mental_card_core::error::protocol("no active table hand"))?;
        let mut opened = Vec::new();
        for seat in active.hand.ledger().live_seats() {
            opened.push((seat, active.hole_cards[seat.index()]));
        }
        let final_stacks = active
            .hand
            .settle_showdown(&opened)?
            .final_stacks()
            .to_vec();
        let showdown = TableShowdown::from_opened(
            active.hand.board(),
            opened
                .into_iter()
                .filter_map(|(seat, cards)| cards.map(|hole| (seat, hole))),
        )?;
        if !showdown.is_empty() {
            self.pending_events
                .push_back(RustTableEvent::Showdown(Showdown {
                    hand: active.hand_number,
                    backend: self.config.backend,
                    showdown,
                }));
        }
        self.finish_hand(active.hand_number, final_stacks);
        Ok(())
    }

    fn finish_uncontested(&mut self) -> Result<(), Error> {
        let mut active = self
            .active_hand
            .take()
            .ok_or_else(|| mental_card_core::error::protocol("no active table hand"))?;
        let final_stacks = active.hand.settle_uncontested()?.final_stacks().to_vec();
        self.finish_hand(active.hand_number, final_stacks);
        Ok(())
    }

    fn finish_complete(&mut self) -> Result<(), Error> {
        let active = self
            .active_hand
            .take()
            .ok_or_else(|| mental_card_core::error::protocol("no active table hand"))?;
        let final_stacks = active
            .hand
            .result()
            .ok_or_else(|| mental_card_core::error::protocol("rules-only result missing"))?
            .final_stacks()
            .to_vec();
        self.finish_hand(active.hand_number, final_stacks);
        Ok(())
    }

    fn finish_hand(&mut self, hand: HandNumber, final_stacks: Vec<Chips>) {
        self.pending_events
            .push_back(RustTableEvent::HandFinished(HandFinished {
                hand,
                backend: self.config.backend,
                final_stacks: final_stacks.clone(),
            }));
        self.stacks = final_stacks;
        self.completed_hands = self.completed_hands.saturating_add(1);
        self.button = next_button(self.button, &self.stacks);
    }

    fn active_hand_mut(&mut self) -> Result<&mut RulesOnlyHand, Error> {
        self.active_hand
            .as_mut()
            .ok_or_else(|| mental_card_core::error::protocol("no active table hand"))
    }

    fn active_player_count(&self) -> usize {
        self.stacks.iter().filter(|stack| !stack.is_zero()).count()
    }

    fn session_should_finish(&self) -> bool {
        self.active_player_count() < 2
            || self
                .config
                .hand_limit
                .is_some_and(|limit| self.completed_hands >= limit)
    }

    fn enqueue_session_finished(&mut self) {
        if self.session_finished {
            return;
        }
        self.session_finished = true;
        self.pending_events
            .push_back(RustTableEvent::SessionFinished(SessionFinished {
                completed_hands: self.completed_hands,
                final_stacks: self.stacks.clone(),
            }));
    }
}

fn with_table_handle(
    handle: *mut MthTableSession,
    operation: impl FnOnce(&mut MthTableSession) -> Result<(), Error>,
) -> i32 {
    let Some(handle) = checked_table_handle(handle) else {
        return code(BoundaryErrorCode::NullPointer);
    };
    match catch_unwind(AssertUnwindSafe(|| operation(handle))) {
        Err(_) => store_table_panic(handle),
        Ok(Ok(())) => {
            handle.last_error.clear();
            clear_global_error();
            code(BoundaryErrorCode::Success)
        }
        Ok(Err(error)) => store_table_error(handle, &error),
    }
}

fn checked_table_handle<'a>(handle: *mut MthTableSession) -> Option<&'a mut MthTableSession> {
    if handle.is_null() {
        return None;
    }
    // SAFETY: The caller promises `handle` is a live pointer returned by this library.
    Some(unsafe { &mut *handle })
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

fn encode_chips_slice(values: &[Chips]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * size_of::<u64>());
    for value in values {
        bytes.extend_from_slice(&value.units().to_le_bytes());
    }
    bytes
}

fn encode_cards(cards: &[Card]) -> Vec<u8> {
    cards
        .iter()
        .map(|card| u8::try_from(card.to_index()).expect("card index fits u8"))
        .collect()
}

fn encode_showdown_rows(showdown: &TableShowdown) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(showdown.hands().len() * 4);
    for hand in showdown.hands() {
        bytes.push(hand.seat.to_u8());
        bytes.extend(encode_cards(&hand.hole_cards));
        bytes.push(hand.category as u8);
    }
    bytes
}

fn shuffled_deck(rng: &mut StdRng) -> Vec<Card> {
    let mut deck = Vec::with_capacity(52);
    for suit in [Suit::Clubs, Suit::Diamonds, Suit::Hearts, Suit::Spades] {
        for rank in [
            Rank::Two,
            Rank::Three,
            Rank::Four,
            Rank::Five,
            Rank::Six,
            Rank::Seven,
            Rank::Eight,
            Rank::Nine,
            Rank::Ten,
            Rank::Jack,
            Rank::Queen,
            Rank::King,
            Rank::Ace,
        ] {
            deck.push(Card::new(rank, suit));
        }
    }
    deck.shuffle(rng);
    deck
}

fn draw_card(deck: &mut Vec<Card>) -> Result<Card, Error> {
    deck.pop()
        .ok_or_else(|| mental_card_core::error::protocol("rules-only deck is empty"))
}

fn active_seats_in_deal_order(button: Seat, stacks: &[Chips]) -> Vec<Seat> {
    (1..=stacks.len())
        .map(|offset| (button.index() + offset) % stacks.len())
        .filter(|index| !stacks[*index].is_zero())
        .map(|index| Seat::new(index).expect("stack index fits in a seat"))
        .collect()
}

fn next_button(current: Seat, stacks: &[Chips]) -> Seat {
    for offset in 1..=stacks.len() {
        let index = (current.index() + offset) % stacks.len();
        if !stacks[index].is_zero() {
            return Seat::new(index).expect("stack index fits in a seat");
        }
    }
    current
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

fn store_table_error(handle: &mut MthTableSession, error: &Error) -> i32 {
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

fn store_table_panic(handle: &mut MthTableSession) -> i32 {
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

fn table_backend_from_code(value: u32) -> Result<TableBackend, Error> {
    match value {
        MTH_TABLE_BACKEND_PEER => Ok(TableBackend::Peer),
        MTH_TABLE_BACKEND_RULES_ONLY => Ok(TableBackend::RulesOnly),
        _ => Err(mental_card_core::error::configuration(
            "unknown table backend code",
        )),
    }
}

fn bot_preset_from_code(value: u32) -> Result<BotPreset, Error> {
    match value {
        MTH_BOT_PRESET_NIT => Ok(BotPreset::Nit),
        MTH_BOT_PRESET_BALANCED => Ok(BotPreset::Balanced),
        MTH_BOT_PRESET_LOOSE_PASSIVE => Ok(BotPreset::LoosePassive),
        MTH_BOT_PRESET_LAG => Ok(BotPreset::Lag),
        MTH_BOT_PRESET_MANIAC => Ok(BotPreset::Maniac),
        _ => Err(mental_card_core::error::configuration(
            "unknown bot preset code",
        )),
    }
}

const fn table_backend_code(value: TableBackend) -> u32 {
    match value {
        TableBackend::Peer => MTH_TABLE_BACKEND_PEER,
        TableBackend::RulesOnly => MTH_TABLE_BACKEND_RULES_ONLY,
    }
}

const fn street_code(value: Street) -> u32 {
    value as u32
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

const fn boundary_action_kind_code(value: BoundaryActionKind) -> u32 {
    match value {
        BoundaryActionKind::Fold => 1,
        BoundaryActionKind::Check => 2,
        BoundaryActionKind::Call => 3,
        BoundaryActionKind::RaiseTo => 4,
        BoundaryActionKind::AllIn => 5,
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
    fn c_visible_constants_match_adapter_boundary() {
        assert_eq!(MTH_ERROR_SUCCESS, code(BoundaryErrorCode::Success));
        assert_eq!(MTH_ERROR_NULL_POINTER, code(BoundaryErrorCode::NullPointer));
        assert_eq!(
            MTH_ERROR_BUFFER_TOO_SMALL,
            code(BoundaryErrorCode::BufferTooSmall)
        );
        assert_eq!(
            MTH_ERROR_INVALID_ARGUMENT,
            code(BoundaryErrorCode::InvalidArgument)
        );
        assert_eq!(MTH_ERROR_NO_EFFECT, code(BoundaryErrorCode::NoEffect));
        assert_eq!(
            MTH_ERROR_CONFIGURATION,
            code(BoundaryErrorCode::Configuration)
        );
        assert_eq!(MTH_ERROR_CODEC, code(BoundaryErrorCode::Codec));
        assert_eq!(
            MTH_ERROR_AUTHENTICATION,
            code(BoundaryErrorCode::Authentication)
        );
        assert_eq!(
            MTH_ERROR_CRYPTOGRAPHY,
            code(BoundaryErrorCode::Cryptography)
        );
        assert_eq!(MTH_ERROR_RULES, code(BoundaryErrorCode::Rules));
        assert_eq!(MTH_ERROR_PROTOCOL, code(BoundaryErrorCode::Protocol));
        assert_eq!(MTH_ERROR_RECOVERY, code(BoundaryErrorCode::Recovery));
        assert_eq!(MTH_ERROR_INTERNAL, code(BoundaryErrorCode::Internal));

        assert_eq!(
            MTH_ACTION_FOLD,
            boundary_action_kind_code(BoundaryActionKind::Fold)
        );
        assert_eq!(
            MTH_ACTION_CHECK,
            boundary_action_kind_code(BoundaryActionKind::Check)
        );
        assert_eq!(
            MTH_ACTION_CALL,
            boundary_action_kind_code(BoundaryActionKind::Call)
        );
        assert_eq!(
            MTH_ACTION_RAISE_TO,
            boundary_action_kind_code(BoundaryActionKind::RaiseTo)
        );
        assert_eq!(
            MTH_ACTION_ALL_IN,
            boundary_action_kind_code(BoundaryActionKind::AllIn)
        );

        assert_eq!(
            MTH_EFFECT_BROADCAST_PUBLIC,
            boundary_effect_kind_code(BoundaryEffectKind::BroadcastPublic)
        );
        assert_eq!(
            MTH_EFFECT_SEND_PRIVATE,
            boundary_effect_kind_code(BoundaryEffectKind::SendPrivate)
        );
        assert_eq!(
            MTH_EFFECT_PERSIST,
            boundary_effect_kind_code(BoundaryEffectKind::Persist)
        );
        assert_eq!(
            MTH_EFFECT_REQUEST_ACTION,
            boundary_effect_kind_code(BoundaryEffectKind::RequestAction)
        );
        assert_eq!(
            MTH_EFFECT_SUBMIT_RECOVERY,
            boundary_effect_kind_code(BoundaryEffectKind::SubmitRecovery)
        );
        assert_eq!(
            MTH_EFFECT_PUBLISH_SHOWDOWN_BUNDLE,
            boundary_effect_kind_code(BoundaryEffectKind::PublishShowdownBundle)
        );
        assert_eq!(
            MTH_EFFECT_FINISHED,
            boundary_effect_kind_code(BoundaryEffectKind::Finished)
        );

        assert_eq!(
            MTH_PHASE_KEY_SETUP,
            boundary_peer_phase_code(BoundaryPeerPhase::KeySetup)
        );
        assert_eq!(
            MTH_PHASE_SHUFFLE,
            boundary_peer_phase_code(BoundaryPeerPhase::Shuffle)
        );
        assert_eq!(
            MTH_PHASE_PRIVATE_REVEAL,
            boundary_peer_phase_code(BoundaryPeerPhase::PrivateReveal)
        );
        assert_eq!(
            MTH_PHASE_HOLE_CARDS_READY,
            boundary_peer_phase_code(BoundaryPeerPhase::HoleCardsReady)
        );
        assert_eq!(
            MTH_PHASE_BETTING,
            boundary_peer_phase_code(BoundaryPeerPhase::Betting)
        );
        assert_eq!(
            MTH_PHASE_BOARD_REVEAL,
            boundary_peer_phase_code(BoundaryPeerPhase::BoardReveal)
        );
        assert_eq!(
            MTH_PHASE_SHOWDOWN,
            boundary_peer_phase_code(BoundaryPeerPhase::Showdown)
        );
        assert_eq!(
            MTH_PHASE_COMPLETE,
            boundary_peer_phase_code(BoundaryPeerPhase::Complete)
        );
    }

    #[test]
    fn abi_layout_reports_and_checks_current_struct_sizes() {
        let mut layout = MthAbiLayout {
            version_major: 0,
            version_minor: 0,
            version_patch: 0,
            session_config_size: 0,
            table_config_size: 0,
            buffer_size: 0,
            effect_size: 0,
            table_event_size: 0,
            peer_status_size: 0,
            abi_layout_size: 0,
        };

        assert_eq!(
            // SAFETY: `layout` is a valid out pointer for one `MthAbiLayout` value.
            unsafe { mth_abi_layout(&raw mut layout) },
            code(BoundaryErrorCode::Success)
        );
        assert_eq!(layout.version_major, ABI_VERSION_MAJOR);
        assert_eq!(layout.version_minor, ABI_VERSION_MINOR);
        assert_eq!(layout.version_patch, ABI_VERSION_PATCH);
        assert_eq!(layout.session_config_size, size_of::<MthSessionConfig>());
        assert_eq!(layout.table_config_size, size_of::<MthTableConfig>());
        assert_eq!(layout.buffer_size, size_of::<MthBuffer>());
        assert_eq!(layout.effect_size, size_of::<MthEffect>());
        assert_eq!(layout.table_event_size, size_of::<MthTableEvent>());
        assert_eq!(layout.peer_status_size, size_of::<MthPeerStatus>());
        assert_eq!(layout.abi_layout_size, size_of::<MthAbiLayout>());
        assert_eq!(
            mth_abi_check_layout(layout),
            code(BoundaryErrorCode::Success)
        );

        layout.effect_size += 1;
        assert_eq!(
            mth_abi_check_layout(layout),
            code(BoundaryErrorCode::InvalidArgument)
        );
        assert_eq!(
            // SAFETY: This intentionally passes a null out pointer to verify the error code.
            unsafe { mth_abi_layout(ptr::null_mut()) },
            code(BoundaryErrorCode::NullPointer)
        );
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

    #[test]
    fn table_session_runs_one_rules_only_bot_hand_through_c_abi() {
        let mut handle = ptr::null_mut();
        let config = table_config(false, 0, Some(1));
        assert_eq!(
            // SAFETY: `handle` is a valid out pointer.
            unsafe { mth_table_session_new(config, &raw mut handle) },
            code(BoundaryErrorCode::Success)
        );
        assert!(!handle.is_null());
        assert_eq!(
            // SAFETY: `handle` is a live table session returned above.
            unsafe { mth_table_start_next_hand(handle) },
            code(BoundaryErrorCode::Success)
        );

        let mut saw_started = false;
        let mut saw_finished = false;
        let mut saw_session_finished = false;
        loop {
            let mut event = MthTableEvent::empty();
            let status = unsafe { mth_table_next_event(handle, &raw mut event) };
            if status == code(BoundaryErrorCode::NoEffect) {
                break;
            }
            assert_eq!(status, code(BoundaryErrorCode::Success));
            match event.kind {
                MTH_TABLE_EVENT_HAND_STARTED => saw_started = true,
                MTH_TABLE_EVENT_HAND_FINISHED => {
                    saw_finished = true;
                    let stacks = decode_u64_buffer(event.final_stacks);
                    assert_eq!(stacks.iter().sum::<u64>(), 600);
                }
                MTH_TABLE_EVENT_SESSION_FINISHED => {
                    saw_session_finished = true;
                    assert_eq!(event.completed_hands, 1);
                }
                _ => {}
            }
            // SAFETY: `event` was returned by this library and is freed exactly once.
            unsafe { mth_table_event_free(event) };
        }
        assert!(saw_started);
        assert!(saw_finished);
        assert!(saw_session_finished);
        // SAFETY: `handle` was returned by this library and is freed exactly once.
        unsafe { mth_table_session_free(handle) };
    }

    #[test]
    fn table_session_requests_and_accepts_human_action() {
        let mut handle = ptr::null_mut();
        let mut config = table_config(true, 0, None);
        config.player_count = 2;
        assert_eq!(
            // SAFETY: `handle` is a valid out pointer.
            unsafe { mth_table_session_new(config, &raw mut handle) },
            code(BoundaryErrorCode::Success)
        );
        assert_eq!(
            // SAFETY: `handle` is a live table session returned above.
            unsafe { mth_table_start_next_hand(handle) },
            code(BoundaryErrorCode::Success)
        );

        let (can_check, _to_call) = loop {
            let mut event = MthTableEvent::empty();
            let status = unsafe { mth_table_next_event(handle, &raw mut event) };
            assert_ne!(status, code(BoundaryErrorCode::NoEffect));
            assert_eq!(status, code(BoundaryErrorCode::Success));
            if event.kind == MTH_TABLE_EVENT_ACTION_REQUEST {
                let request = (event.can_check, event.to_call_units);
                unsafe { mth_table_event_free(event) };
                break request;
            }
            unsafe { mth_table_event_free(event) };
        };
        let action = if can_check {
            MTH_ACTION_CHECK
        } else {
            MTH_ACTION_CALL
        };
        assert_eq!(
            // SAFETY: `handle` is live and an action request is pending.
            unsafe { mth_table_submit_action(handle, action, 0) },
            code(BoundaryErrorCode::Success)
        );
        let mut saw_action = false;
        loop {
            let mut event = MthTableEvent::empty();
            let status = unsafe { mth_table_next_event(handle, &raw mut event) };
            if status == code(BoundaryErrorCode::NoEffect) {
                break;
            }
            assert_eq!(status, code(BoundaryErrorCode::Success));
            if event.kind == MTH_TABLE_EVENT_ACTION_TAKEN && event.actor == 0 {
                saw_action = true;
                assert!(event.has_post_action_totals);
                // Blinds are already in the pot when the first action resolves.
                assert!(event.pot_after_units >= 3);
                assert!(event.actor_stack_after_units <= 100);
            }
            unsafe { mth_table_event_free(event) };
        }
        assert!(saw_action);
        // SAFETY: `handle` was returned by this library and is freed exactly once.
        unsafe { mth_table_session_free(handle) };
    }

    #[test]
    fn table_session_keeps_prompt_pending_after_rejected_action() {
        let mut handle = ptr::null_mut();
        let mut config = table_config(true, 0, None);
        config.player_count = 2;
        assert_eq!(
            // SAFETY: `handle` is a valid out pointer.
            unsafe { mth_table_session_new(config, &raw mut handle) },
            code(BoundaryErrorCode::Success)
        );
        assert_eq!(
            // SAFETY: `handle` is a live table session returned above.
            unsafe { mth_table_start_next_hand(handle) },
            code(BoundaryErrorCode::Success)
        );

        let (can_check, _to_call) = loop {
            let mut event = MthTableEvent::empty();
            let status = unsafe { mth_table_next_event(handle, &raw mut event) };
            assert_ne!(status, code(BoundaryErrorCode::NoEffect));
            assert_eq!(status, code(BoundaryErrorCode::Success));
            if event.kind == MTH_TABLE_EVENT_ACTION_REQUEST {
                let request = (event.can_check, event.to_call_units);
                unsafe { mth_table_event_free(event) };
                break request;
            }
            unsafe { mth_table_event_free(event) };
        };

        // A raise to zero units is never legal; the engine must reject it
        // without consuming the pending prompt.
        assert_eq!(
            // SAFETY: `handle` is live and an action request is pending.
            unsafe { mth_table_submit_action(handle, MTH_ACTION_RAISE_TO, 0) },
            code(BoundaryErrorCode::Rules)
        );

        let action = if can_check {
            MTH_ACTION_CHECK
        } else {
            MTH_ACTION_CALL
        };
        assert_eq!(
            // SAFETY: the prompt must still be pending after the rejection.
            unsafe { mth_table_submit_action(handle, action, 0) },
            code(BoundaryErrorCode::Success)
        );
        let mut saw_action = false;
        loop {
            let mut event = MthTableEvent::empty();
            let status = unsafe { mth_table_next_event(handle, &raw mut event) };
            if status == code(BoundaryErrorCode::NoEffect) {
                break;
            }
            assert_eq!(status, code(BoundaryErrorCode::Success));
            if event.kind == MTH_TABLE_EVENT_ACTION_TAKEN && event.actor == 0 {
                saw_action = true;
            }
            unsafe { mth_table_event_free(event) };
        }
        assert!(saw_action);
        // SAFETY: `handle` was returned by this library and is freed exactly once.
        unsafe { mth_table_session_free(handle) };
    }

    #[test]
    fn table_session_rejects_peer_backend_without_fallback() {
        let mut handle = ptr::null_mut();
        let mut config = table_config(false, 0, Some(1));
        config.backend = MTH_TABLE_BACKEND_PEER;
        assert_eq!(
            // SAFETY: `handle` is a valid out pointer.
            unsafe { mth_table_session_new(config, &raw mut handle) },
            code(BoundaryErrorCode::Configuration)
        );
        assert!(handle.is_null());
    }

    fn table_config(has_human: bool, human_seat: u8, hand_limit: Option<u64>) -> MthTableConfig {
        MthTableConfig {
            backend: MTH_TABLE_BACKEND_RULES_ONLY,
            player_count: 6,
            has_human_seat: has_human,
            human_seat,
            bot_preset: MTH_BOT_PRESET_BALANCED,
            small_blind_units: 1,
            big_blind_units: 2,
            starting_stack_units: 100,
            has_hand_limit: hand_limit.is_some(),
            hand_limit: hand_limit.unwrap_or(0),
            seed: 7,
            has_seat_presets: false,
            seat_presets_ptr: ptr::null(),
        }
    }

    #[test]
    fn table_session_runs_with_per_seat_presets() {
        let presets = [
            MTH_BOT_PRESET_NIT,
            MTH_BOT_PRESET_BALANCED,
            MTH_BOT_PRESET_LOOSE_PASSIVE,
            MTH_BOT_PRESET_LAG,
            MTH_BOT_PRESET_MANIAC,
            MTH_BOT_PRESET_BALANCED,
        ];
        let mut config = table_config(false, 0, Some(1));
        config.has_seat_presets = true;
        config.seat_presets_ptr = presets.as_ptr();
        let mut handle = ptr::null_mut();
        assert_eq!(
            // SAFETY: `handle` is a valid out pointer and `presets` outlives the call.
            unsafe { mth_table_session_new(config, &raw mut handle) },
            code(BoundaryErrorCode::Success)
        );
        assert!(!handle.is_null());
        assert_eq!(
            // SAFETY: `handle` is a live table session returned above.
            unsafe { mth_table_start_next_hand(handle) },
            code(BoundaryErrorCode::Success)
        );
        let mut saw_session_finished = false;
        loop {
            let mut event = MthTableEvent::empty();
            // SAFETY: `handle` is live and `event` is a valid out pointer.
            let status = unsafe { mth_table_next_event(handle, &raw mut event) };
            if status == code(BoundaryErrorCode::NoEffect) {
                break;
            }
            assert_eq!(status, code(BoundaryErrorCode::Success));
            if event.kind == MTH_TABLE_EVENT_SESSION_FINISHED {
                saw_session_finished = true;
            }
            // SAFETY: `event` was returned by this library and is freed exactly once.
            unsafe { mth_table_event_free(event) };
        }
        assert!(saw_session_finished);
        // SAFETY: `handle` was returned by this library and is freed exactly once.
        unsafe { mth_table_session_free(handle) };
    }

    #[test]
    fn table_session_rejects_unknown_seat_preset_code() {
        let presets = [
            MTH_BOT_PRESET_NIT,
            99,
            MTH_BOT_PRESET_BALANCED,
            MTH_BOT_PRESET_BALANCED,
            MTH_BOT_PRESET_BALANCED,
            MTH_BOT_PRESET_BALANCED,
        ];
        let mut config = table_config(false, 0, Some(1));
        config.has_seat_presets = true;
        config.seat_presets_ptr = presets.as_ptr();
        let mut handle = ptr::null_mut();
        assert_eq!(
            // SAFETY: `handle` is a valid out pointer and `presets` outlives the call.
            unsafe { mth_table_session_new(config, &raw mut handle) },
            code(BoundaryErrorCode::Configuration)
        );
        assert!(handle.is_null());
    }

    #[test]
    fn table_session_rejects_null_seat_presets_pointer() {
        let mut config = table_config(false, 0, Some(1));
        config.has_seat_presets = true;
        config.seat_presets_ptr = ptr::null();
        let mut handle = ptr::null_mut();
        assert_eq!(
            // SAFETY: This test intentionally passes a null presets pointer with a
            // valid out-handle pointer to verify construction failure handling.
            unsafe { mth_table_session_new(config, &raw mut handle) },
            code(BoundaryErrorCode::Configuration)
        );
        assert!(handle.is_null());
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
