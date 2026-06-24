# ADR-006: Add a Transport-Neutral Peer Session Layer

## Status
Accepted

## Date
2026-06-24

## Context
ADR-003 keeps production protocol crates Sans-I/O and says future P2P drivers should expose host
effects such as broadcast, private send, persistence, action requests, and final results. The
`mtholdem` crate now owns real signed public messages, signed private reveals, transcripts,
cryptographic hand state, checkpoints, and recovery evidence. Frontends still need one shared
per-peer orchestration layer so CLI, FFI, WASM, Godot, and network transports do not duplicate
message ordering or replay checks.

## Decision
Add `mtholdem-peer` as a sibling crate above `mtholdem`. It owns local peer-session state and emits
typed `HostEffect` values while remaining transport-neutral. The first slice drives key setup,
sequential shuffling, private reveal delivery, hole-card receipt commitments, signed betting
actions, public community-card reveal shares, board-card opening markers, checkpoint
certification/announcement, recovery-bundle submission, showdown bundle publication,
`ShowdownOpen` commitments, deterministic payout agreement, and final-hand effects with real
`SignedPublicMessage`, `SignedPrivateReveal`, `ShowdownBundle`, `HandCryptography`, `HoldemHand`,
`CertifiedCheckpoint`, `RecoveryBundle`, and `PublicTranscript` values.

`mtholdem-peer` must not use compatibility facades, placeholder protocol references, sockets,
storage traits, timers, async runtimes, or frontend-owned protocol fallbacks. It should keep
canonical `mtholdem` artifacts intact for Rust callers; FFI, WASM, JSON, or Godot adapters may
encode those effects only at their own boundary.

## Alternatives Considered

### Put peer orchestration in `mtholdem`
- Pros: Direct access to all Hold'em internals.
- Cons: Expands the verification crate into a session driver and makes future facade dependencies
  harder to keep one-way.
- Rejected: `mtholdem` should stay the Sans-I/O rules, wire, and recovery crate.

### Put peer orchestration in CLI, FFI, WASM, or Godot wrappers
- Pros: Each target can move quickly.
- Cons: Message ordering, replay rejection, action routing, and persistence effects would diverge.
- Rejected: P2P correctness must be Rust-owned and shared by all frontends.

### Copy the older placeholder `mtholdem-peer` message-ref model
- Pros: It already sketches facade-facing peer messages.
- Cons: This workspace has real signed wire and cryptographic artifacts, so placeholder payload refs
  would add a second protocol surface.
- Rejected: The new peer layer should orchestrate the real `mtholdem` APIs directly.

## Consequences
- The dependency direction becomes `mental-card-core <- mtholdem <- mtholdem-peer`.
- Host applications carry `HostEffect` values over their own transport and feed received artifacts
  back into `PeerSession`.
- Multi-peer simulations that exchange only emitted public messages are the primary regression tests
  for peer-session readiness.
- Future wrapper crates should wrap `mtholdem-peer` instead of parsing transcript internals or
  applying game actions directly.
