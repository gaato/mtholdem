# ADR-014: License Workspace Under MPL-2.0

## Status
Accepted

## Date
2026-06-26

## Context
The workspace started under `MIT OR Apache-2.0`, matching common Rust
ecosystem defaults. As the project boundary became clearer, the core value
shifted from a generic reusable crate toward an open dealerless Hold'em
protocol implementation with C ABI, Godot, WASM, and custom participant
clients consuming the same state machines.

The reusable parts should remain usable from proprietary games and host
applications. At the same time, protocol, peer-session, adapter, FFI, and
Godot wrapper fixes should be easier to share back when a downstream user
modifies those files. A strong copyleft license such as GPL would put too much
pressure on game and host application code. A permissive-only license would not
express the desired file-level sharing boundary.

The workspace also contains a derived `ziffle` implementation in
`mental-card-core`. Its upstream notice is `MIT OR Apache-2.0`; those notice
files remain distributed with the affected crate.

## Decision
License the workspace under `MPL-2.0`.

Use MPL-2.0 as the intended license for core Rust crates, peer/session code,
adapter code, C ABI wrappers, and downstream Godot wrapper code. The license
boundary is file-level: modifications to MPL-covered files stay open under
MPL, while larger works such as games, Godot projects, applications, assets,
and host-specific UI code may remain under their own licenses.

Keep `LICENSE-MIT`, `LICENSE-APACHE`, and third-party notices where needed for
derived or bundled code whose upstream notice requires preservation.

Examples, templates, and sample applications may use more permissive licensing
if they are designed to be copied into downstream projects. That decision
should be made explicitly in the repository that owns those examples.

## Alternatives Considered

### Keep `MIT OR Apache-2.0`
- Pros: Lowest adoption friction in Rust and game-development ecosystems.
- Cons: Does not ask downstream protocol or binding modifications to remain
  available, even when those modifications affect interoperability and audit
  expectations.
- Rejected: The project benefits from a clearer file-level sharing boundary.

### Use `MPL-2.0 OR Apache-2.0`
- Pros: Keeps Apache-2.0 adoption friction low while offering MPL
  compatibility.
- Cons: Users can choose Apache-2.0, so the file-level sharing expectation is
  mostly aspirational.
- Rejected: Use MPL-2.0 directly unless adoption pressure later proves too
  high.

### Use `GPL-3.0-or-later OR commercial`
- Pros: Supports a traditional commercial dual-license model.
- Cons: Too heavy for a library intended to be linked from games, Godot
  wrappers, native applications, and possibly console/mobile hosts. It also
  requires contributor license management before it becomes practical.
- Rejected: Keep the host-application boundary lighter.

### Use `LGPL-3.0-or-later`
- Pros: Familiar weak copyleft for libraries and dynamic linking.
- Cons: Less natural for Rust crates, static linking, generated headers, and
  mixed host bindings than MPL's file-level boundary.
- Rejected: MPL better matches the file and wrapper boundary.

## Consequences
- Workspace package metadata uses `MPL-2.0`.
- Existing MIT/Apache license files remain only as third-party notice support
  where upstream-derived code requires them.
- Downstream Godot wrappers should normally use MPL-2.0 for wrapper and binding
  files, while games, scenes, scripts, and assets remain separately licensed.
- If future contributors are accepted and a commercial dual-license route is
  desired, the project will need contributor license terms that preserve
  relicensing authority.
