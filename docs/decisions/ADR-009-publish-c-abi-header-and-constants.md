# ADR-009: Publish C ABI Header and Constants

## Status
Accepted

## Date
2026-06-25

## Context
ADR-007 defines `mtholdem-ffi` as the conservative C-compatible boundary for
Godot and other native host wrappers. A downstream Godot wrapper proved that the
opaque-handle lifecycle, Sans-I/O progress/effect/action flow, byte-oriented
artifacts, and stable integer error codes are usable from C++ and GDScript.

The same integration also exposed a packaging gap. `mtholdem-ffi` currently
exports functions and repr-C structs from Rust, but it does not publish the
complete C header and constant set that downstream wrappers must compile
against. A Godot wrapper therefore has to hand-maintain declarations for
functions, structs, effect/action/error kinds, and layout assumptions. That is a
drift risk at the exact boundary intended to be stable.

The C ABI should remain lower-level than a Godot API. Godot-specific wrappers
still need to translate effects into engine values such as dictionaries and
`PackedByteArray`, but they should not need to duplicate ABI declarations or
numeric constants.

## Decision
`mtholdem-ffi` must publish an official C header as part of the C ABI surface.
The header may be generated or checked in, but release validation must detect
drift between the Rust ABI implementation and the distributed header.

The official header must include:

- opaque handle declarations and their matching free functions;
- repr-C structs used by the ABI, including ownership and buffer-length rules;
- function declarations for session lifecycle, progress/effect draining,
  action submission, artifact application, status, error text, and ABI version
  queries;
- numeric constants for effect kinds, action kinds, error codes, phases, and
  other C-visible discriminants;
- ABI version and layout metadata sufficient for downstream wrappers to reject
  an incompatible library at startup.

`mtholdem-ffi` should provide a small runtime layout/version check surface for
host wrappers. The exact function shape can evolve before the stable release,
but it must let a wrapper verify the ABI version and the sizes or layout
assumptions it compiled against before running a session.

The C ABI namespace must use the project name, not historical import-directory
names. C symbols use the `mth_` prefix and C-visible types use the `Mth` prefix.
Before the first stable release, remove historical namespace names rather than
preserving compatibility aliases.

Godot remains outside this workspace. The Godot repository consumes the C ABI
header and converts the low-level C structs into Godot-friendly values. This
workspace should not add GDExtension classes, Godot build files, or
Godot-shaped high-level APIs.

Deterministic fixture identity material belongs to test tooling. If downstream
tests need public keys derived from deterministic seeds, expose that through
testkit fixtures, generated test vectors, or an explicitly test-only ABI
feature. Do not add silent production fallbacks that generate substitute
identity material when a host failed to provide the roster public keys required
by session creation.

## Alternatives Considered

### Keep downstream wrappers responsible for headers
- Pros: No header generation or packaging work in `mtholdem-ffi`.
- Cons: Every native wrapper can drift in struct declarations, constants,
  buffer ownership rules, and error-code interpretation.
- Rejected: The C ABI is meant to be the stable foreign boundary. Requiring
  handwritten downstream mirrors weakens that boundary.

### Add Godot GDExtension bindings to this workspace
- Pros: The first Godot wrapper could share implementation details directly.
- Cons: Godot SDK versions, editor artifacts, engine-specific build policy, and
  binding unsafe code would enter the reusable protocol workspace.
- Rejected: ADR-007 keeps Godot as a downstream consumer of the C ABI.

### Expose a high-level Godot-shaped C ABI
- Pros: Less conversion code in the Godot repository.
- Cons: The C ABI would begin to encode Godot collection shapes and UI-facing
  convenience semantics instead of a small portable foreign boundary.
- Rejected: Godot-specific ergonomics belong in the Godot wrapper. The C ABI
  should stay portable to C++, Python, Ruby, Zig, and similar hosts.

### Treat ABI version numbers as enough compatibility checking
- Pros: Simple to implement.
- Cons: A matching version does not prove that a downstream wrapper compiled
  with the same struct sizes, constants, or header declarations.
- Rejected: Native wrappers need layout checks to fail early and clearly when
  header and library artifacts are mismatched.

## Consequences
- `mtholdem-ffi` owns the C header as a release artifact, not only Rust
  `extern "C"` definitions.
- Effect/action/error/phase discriminants become documented C-visible API, even
  before the first stable release.
- CI or release validation must include a header drift check and a C or C++
  compile/link smoke test against the installed header.
- Godot wrapper maintenance becomes mostly conversion logic instead of
  declaration synchronization.
- The ABI remains intentionally lower-level than engine APIs; `MthEffect` can
  stay a C-friendly aggregate while downstream wrappers present richer host
  values.
- Historical namespace names are removed instead of carried as compatibility
  shims because the ABI has not reached a stable release.
