# ADR-019: Add Verified Peer Presentation Events

## Status
Accepted

## Date
2026-08-21

## Context

`mtholdem-peer` already exposes `HostEffect` as its Sans-I/O contract for work that must leave the peer state machine. Those effects tell a host to broadcast public artifacts, route confidential reveals, persist transcript anchors, request a local action, publish showdown bundles, submit recovery evidence, or consume a final settlement result.

A player-facing host also needs a coherent local table view. Building that view by parsing transport effects or replaying poker rules in Godot, C++, WASM, or another wrapper would create a second state machine outside Rust. It could display an action before native rules accept it, expose an opponent's cards before cryptographic disclosure succeeds, disagree about pot or stack totals, or couple presentation code to protocol artifact formats.

The existing `mtholdem-table` event vocabulary does not solve this boundary. It belongs to a host-side multi-hand table runner and is a sibling of `mtholdem-peer`; it does not represent the verified local view of one cryptographic peer session.

## Decision

`mtholdem-peer` owns a second pull-based queue of typed `PresentationEvent` values. This queue is independent from `HostEffect`.

Presentation events report only player-visible state that `PeerSession` has accepted or derived through native validation:

- hand start after peer-session configuration, identity, stacks, and deal plan are validated;
- local hole cards after private reveal verification and reconstruction;
- betting actions after signature, transcript, actor, street, and rules validation;
- board streets after every required reveal share and opened card are verified and the rules engine accepts the complete flop, turn, or river;
- showdown hands after the public commitment, side bundle, earlier private-card receipt, signatures, and reveal shares agree;
- hand completion after deterministic settlement and any required public payout agreement.

Every event carries a complete immutable local snapshot: hand and peer phase, local seat, button, optional current street and next actor, blinds, pot, current stacks, board, the local peer's hole cards, verified showdown rows with hand categories, and final stacks when complete. Before showdown, no opponent hole cards may appear in the snapshot. Pending or merely transported showdown artifacts are not presentation state.

`HostEffect` remains the only transport and infrastructure work stream. Draining presentation events does not acknowledge effects, and draining effects does not consume presentation events. Hosts must continue to route and persist effects even when they do not render them.

`mtholdem-adapter` performs the single target-neutral conversion of typed presentation events. `mtholdem-ffi` exposes the converted stream as `MthPeerEvent`, `mth_peer_session_next_presentation_event`, and `mth_peer_event_free`, following the existing owned-buffer and `MTH_ERROR_NO_EFFECT` conventions. The expanded C contract is ABI version 0.2.0, and `MthAbiLayout` appends `peer_event_size` after the complete ABI 0.1 field prefix.

ABI layout discovery and validation use the versioned `mth_abi_layout_v2` and `mth_abi_check_layout_v2` functions. Both take an explicit caller-provided byte count and do not access the layout when that region is too small. The ABI 0.1 symbols remain only with their original pointer/by-value calling conventions so already-compiled old callers receive a safe incompatibility result instead of a buffer overwrite or cross-version call mismatch; they are not part of the ABI 0.2 header.

The C ABI also exposes fixed-size public-key derivation from a caller-supplied 32-byte Ed25519 seed. This is a narrow conversion needed to construct the public roster; it does not generate, retain, persist, log, or substitute identity material.

## Alternatives Considered

### Derive presentation from `HostEffect`

- Pros: No second queue or event types.
- Cons: Effects describe host work rather than accepted display state. Private transport artifacts, persistence requests, and public broadcasts do not form a complete table snapshot, and parsing them would duplicate protocol and poker logic in every frontend.
- Rejected: Player-visible truth must be derived from the verified native state, not inferred from requested side effects.

### Add presentation variants to `HostEffect`

- Pros: One polling stream.
- Cons: Rendering events could be mistaken for required transport work, while optional UI draining could accidentally acknowledge required persistence or delivery responsibilities. It would also mix two contracts with different ownership and reliability semantics.
- Rejected: Independent queues make transport obligations and presentation observations explicit.

### Reuse `mtholdem-table::TableEvent`

- Pros: Similar hand, action, board, showdown, and finish vocabulary already exists.
- Cons: The crates are siblings, table events are incremental host-runner summaries, and they do not carry a cryptographic peer's complete privacy-filtered snapshot.
- Rejected: Peer verification and local table running remain separate boundaries.

### Let each target wrapper query and reconstruct peer state

- Pros: Smaller Rust event surface.
- Cons: Every wrapper would need ordering, privacy, evaluation, pot, stack, and completion logic and could render rejected or unverified transitions.
- Rejected: Wrappers should translate one native source of truth rather than implement another one.

## Consequences

- Player-facing hosts can render a peer hand without parsing protocol artifacts or replaying Hold'em rules.
- Rejected actions and invalid artifacts produce no accepted presentation event.
- Opponent cards remain absent until the local peer completes cryptographic showdown verification.
- Each event is larger because it carries a complete snapshot, but consumers can recover from missed incremental UI work without reconstructing prior state.
- Hosts must drain two independent queues and preserve the stronger delivery and durability obligations attached to `HostEffect`.
- Native wrappers must compile against the ABI 0.2.0 header and verify the added `peer_event_size` layout field.
