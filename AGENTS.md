# Repository Guidelines

## Project Structure & Module Organization

Keep the root small. This repository is a seven-crate Sans-I/O workspace.

- `README.md` summarizes the workspace and links to the architecture docs.
- `docs/decisions/` stores ADRs. Read these before changing architecture,
  public APIs, wire formats, recovery, settlement, crate boundaries, or
  packaging.
- `crates/mental-card-core` owns reusable standard-card cryptography, identity,
  roster, strong IDs, and per-deal key/shuffle/reveal orchestration.
- `crates/mtholdem` owns Texas Hold'em rules, betting, wire artifacts, public
  transcripts, checkpoints, recovery evidence, and settlement-facing semantics.
- `crates/mtholdem-peer` owns the transport-neutral peer-session driver and
  emits host effects instead of doing I/O.
- `crates/mtholdem-adapter` owns the byte-oriented boundary shared by CLI, C ABI,
  and downstream engine or WASM package consumers.
- `crates/mtholdem-cli` and `crates/mtholdem-ffi` expose in-workspace
  target-specific wrappers over the adapter boundary.
- `crates/mtholdem-testkit` owns deterministic fixtures for downstream tests.
  Keep it out of production dependency graphs.

Do not put transport, UI, storage, timer, or settlement implementation details
inside protocol crates. Host applications may use CLI, Godot, WASM/WebRTC,
libp2p, QUIC, relays, or in-process tests; the Rust state machines must remain
usable by all of them. Crate-boundary rationale belongs in ADRs, especially
ADR-001 through ADR-008.

Dependencies must remain one-way:
`mental-card-core <- mtholdem <- mtholdem-peer <- mtholdem-adapter <-
in-workspace target wrappers`, with `mtholdem-testkit` used only for
deterministic tests.
Core must not learn Hold'em terms such as blinds, streets, hole cards, pots,
muck, or showdown.

## Build, Test, and Development Commands

- `jj status` checks Jujutsu working-copy state.
- `git status --short --branch` checks Git interoperability state.
- `cargo fmt --all -- --check` verifies formatting.
- `cargo test --locked --workspace --all-targets --all-features` runs the full test suite.
- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` runs lint gates.
- `cargo doc --locked --workspace --no-deps --all-features` verifies public docs.

## Coding Style & Naming Conventions

Follow Rust API Guidelines and keep `unsafe` forbidden. Use strong domain types
for seats, players, cards, sessions, hands, digests, and chip values. Prefer
typed constructors over unchecked integers or raw byte vectors.

Write comments and Rustdoc for local readability by automated coding tools and
human maintainers, not prose mirrors. A reader may inspect one item without the
whole architecture in view; mark invariants, caller obligations,
proof-context bindings, protocol risks, and source-of-truth boundaries near
dependent code. Public APIs need Rustdoc with failure semantics.

## Testing Guidelines

Test observable behavior: proof binding, transcript hash chains, wire decoding
bounds, checkpoint semantics, recovery evidence, betting legality, chip
conservation, tamper rejection, and panic boundaries. Testkit helpers must use
public production APIs and must not expose fake proofs, bypass constructors, or
secret-inspection hooks.

## Commit & PR Guidelines

Use imperative commit messages, for example `Add crate-boundary ADRs`. Prefer small `jj commit` steps. Do not move bookmarks, push, or rewrite shared history unless explicitly requested.

PRs should include context, validation commands, known limitations, and links to relevant ADRs for architecture, wire, recovery, or security changes.

## Failure & Fallback Policy

Say when something cannot be done with the current code, data, dependencies, or
runtime. Do not implement plausible-looking fallback behavior unless it is an
explicitly designed and tested requirement. Prefer typed errors, diagnostics, or
clear unsupported-operation messages over silent degradation.

This rule exists because automated edits and hurried maintenance can preserve
momentum by inventing fallback paths when the real cause is unclear. In this
repository, that is a correctness bug: a fallback can hide cheating, desync
participants, make transcripts unreplayable, or turn a failed proof into a
false game state. Failed proof
verification, unknown wire versions, invalid actions, missing checkpoint
evidence, unavailable production capabilities, and malformed transcripts must
fail explicitly unless an ADR and tests define a recovery path.

Mock, deterministic, or experimental behavior is allowed only when the selected
testkit or fixture names it explicitly. Production-oriented paths must not
silently downgrade to deterministic, mock, experimental, or locally generated
substitute data.

## Pre-Stable Design Policy

Before the first stable release, do not preserve compatibility with weak early
Rust APIs, crate names, wire artifacts, transcript schemas, or module
boundaries. Break and redesign them when the change clearly improves the final
product. Do not add compatibility shims unless an ADR accepts the long-term
cost.

When a failure exposes a weak design, prefer finding and fixing the root cause
over adding a local workaround. Refactoring is part of completing the product,
not a cleanup phase after feature work.
