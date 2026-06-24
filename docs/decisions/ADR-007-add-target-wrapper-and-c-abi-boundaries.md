# ADR-007: Add Target Wrapper and C ABI Boundaries

## Status
Accepted; amended by ADR-008

## Date
2026-06-24

## Context
ADR-006 added `mtholdem-peer` as the transport-neutral orchestration layer for
`PeerSession`. The next public surfaces are CLI, WASM, C ABI/FFI, and Godot integration.
Those surfaces need primitive values, byte buffers, explicit ownership rules, and stable
error codes, but they must not become new protocol implementations.

Godot support is specifically a host-engine concern. Pulling GDExtension code into this
workspace would make the core poker library depend on Godot SDK packaging, engine release
cadence, editor artifacts, and binding-specific unsafe code. The same problem would
repeat for other engines and languages if the protocol crate exposed target-specific
objects instead of one conservative host boundary.

## Decision
Add a shared `mtholdem-adapter` crate above `mtholdem-peer`. It converts typed
`HostEffect` values into byte-oriented boundary events, stable action/status structs,
stable error codes, and canonical artifact encode/decode helpers. It must not validate
protocol state independently, inspect transcript internals, or provide fallback game
logic. `mtholdem-peer` remains the source of truth for ordering, replay rejection,
betting legality, recovery, showdown, and final-hand effects.

CLI and WASM wrappers should depend on `mtholdem-adapter`. The C ABI crate should also
depend on `mtholdem-adapter` and expose opaque handles, ABI versioning, status/error
codes, caller-visible buffer lengths, and explicit allocation/free functions. `unsafe`
is allowed only in the C ABI crate and only to implement the foreign-function boundary;
protocol crates and the shared adapter keep `unsafe_code = "forbid"`.

Godot GDExtension is intentionally kept in a separate repository. That repository should
call the C ABI and translate boundary events into Godot values such as `PackedByteArray`
or dictionaries. This workspace may document the Godot-facing contract and provide test
vectors, but it must not own GDExtension build files or Godot-specific bindings.

## Alternatives Considered

### Put Godot GDExtension in this workspace
- Pros: One checkout could build the demo integration.
- Cons: Godot SDK, GDExtension build outputs, and engine-specific unsafe/binding policy
  would leak into the protocol workspace.
- Rejected: The reusable library should stay engine-independent. Godot belongs in a
  separate repo that consumes the C ABI.

### Let each wrapper parse `HostEffect` and artifacts directly
- Pros: Less upfront adapter code.
- Cons: CLI, WASM, FFI, and Godot would drift in event names, error codes, action
  encoding, and buffer ownership expectations.
- Rejected: Wrapper diversity should start after a shared byte/event contract, not before
  it.

### Expose Rust structs directly through FFI
- Pros: Fewer conversions for Rust code.
- Cons: Rust layout, ownership, panic behavior, and allocator assumptions are not stable
  C contracts.
- Rejected: The C ABI must use opaque handles, primitive fields, and byte buffers.

## Consequences
- The dependency direction becomes `mental-card-core <- mtholdem <- mtholdem-peer <-
  mtholdem-adapter <- target wrappers`.
- Wrapper crates can evolve independently while sharing one effect/action/error schema.
- C ABI implementation requires an explicit unsafe exception isolated to its crate.
- Godot support is defined as a downstream consumer of the C ABI, not as an in-workspace
  GDExtension target.
- ADR-008 later removes the in-workspace WASM helper while keeping `mtholdem-adapter` as the
  shared Rust boundary for downstream WASM/npm packages.
