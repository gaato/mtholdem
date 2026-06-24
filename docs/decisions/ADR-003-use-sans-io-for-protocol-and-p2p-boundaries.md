# ADR-003: Use Sans-I/O for Protocol and P2P Boundaries

## Status
Accepted

## Date
2026-06-24

## Context
The same protocol state should run under CLI tests, WebRTC, Godot, WASM, native apps, relays, or future libp2p/QUIC transports. Transport, timers, storage, and UI differ across hosts, but consensus verification must not fork.

## Decision
Keep production protocol crates Sans-I/O. They may parse bytes, verify signatures and proofs, advance state machines, produce outbound artifacts, and report required host actions. They must not open sockets, sleep, spawn tasks, write databases, call blockchains, or depend on a specific async runtime.

## Alternatives Considered

### Built-in network runtime
- Pros: Faster demo path.
- Cons: Ties correctness to one transport and duplicates logic for other adapters.
- Rejected: Godot, WASM, and CLI would diverge.

### Frontend-owned validation
- Pros: Thin Rust core.
- Cons: Replays, proof checks, and legal actions would be reimplemented in unsafe places.
- Rejected: Verification belongs in Rust-owned state machines.

## Consequences
- Adapters own I/O and feed verified artifacts into the core crates.
- Future P2P drivers should expose effects such as broadcast, private send, persist checkpoint, request action, and finished record.
- Tests can simulate hostile transports without network dependencies.
