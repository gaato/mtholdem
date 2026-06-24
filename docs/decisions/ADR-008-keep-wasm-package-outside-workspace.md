# ADR-008: Keep WASM Package Outside the Protocol Workspace

## Status
Accepted

## Date
2026-06-24

## Context
ADR-007 introduced target wrappers above `mtholdem-adapter`, including an in-workspace
`mtholdem-wasm` helper. That helper intentionally avoided `wasm-bindgen`, npm metadata,
browser transport, and bundler integration, but this also made it a thin copy of the
shared adapter boundary.

WASM packaging has the same boundary pressure as Godot integration: the useful public
surface is deployment-specific. A real WASM package must choose binding generation,
JavaScript or TypeScript shape, npm packaging, bundler assumptions, browser tests,
WebRTC or relay integration, and storage policy. Those choices should not become part of
the reusable protocol workspace.

## Decision
Remove the in-workspace `mtholdem-wasm` crate. A WASM/npm package should live in a
separate repository and consume this workspace as Rust dependencies. It should use
`mtholdem-adapter` as the shared byte/action/effect/status boundary and may depend on
`mtholdem-peer` when it needs Rust-owned peer-session state.

Godot remains a separate repository that consumes `mtholdem-ffi` through the C ABI.
WASM does not need to go through the C ABI; it should consume the Rust adapter boundary
directly unless a downstream package has a specific reason to share the C ABI.

## Alternatives Considered

### Keep `mtholdem-wasm` as a thin Rust helper
- Pros: One workspace would expose every planned wrapper crate.
- Cons: The helper duplicates adapter-level concerns without owning real WASM packaging,
  JavaScript bindings, browser tests, or transport.
- Rejected: A thin wrapper is extra API surface without a clear responsibility.

### Move WASM through the C ABI
- Pros: Same foreign boundary as Godot.
- Cons: WASM can call Rust code directly and does not need C ABI allocation and ownership
  constraints.
- Rejected: The C ABI is for C-compatible consumers, not the preferred WASM path.

## Consequences
- The workspace crate graph remains `mental-card-core <- mtholdem <- mtholdem-peer <-
  mtholdem-adapter <- in-workspace target wrappers`.
- `mtholdem-cli` and `mtholdem-ffi` stay in this workspace.
- Downstream WASM/npm packages own their binding generator, npm metadata, browser tests,
  transport, and storage policy.
- The adapter boundary remains the stable Rust-facing contract for downstream WASM work.
