# Repository Guidelines

## Project Structure & Module Organization

This workspace is split by trust boundary and reuse scope.

- `crates/mental-card-core` owns reusable standard-card cryptography, identity, roster, strong IDs, and per-deal key/shuffle/reveal orchestration.
- `crates/mtholdem` owns Texas Hold'em rules, betting, wire artifacts, public transcripts, checkpoints, recovery evidence, and settlement-facing semantics.
- `crates/mtholdem-testkit` owns deterministic fixtures for downstream tests. Keep it out of production dependency graphs.
- `docs/decisions/` stores ADRs. Read them before changing architecture, public APIs, wire formats, recovery, settlement, or crate boundaries.

Dependencies must remain one-way: `mental-card-core <- mtholdem <- mtholdem-testkit`. Core must not learn Hold'em terms such as blinds, streets, hole cards, pots, muck, or showdown.

## Build, Test, and Development Commands

- `jj status` checks Jujutsu working-copy state.
- `git status --short --branch` checks Git interoperability state.
- `cargo fmt --all -- --check` verifies formatting.
- `cargo test --locked --workspace --all-targets --all-features` runs the full test suite.
- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` runs lint gates.
- `cargo doc --locked --workspace --no-deps --all-features` verifies public docs.

## Coding Style & Naming Conventions

Follow Rust API Guidelines and keep `unsafe` forbidden. Use strong domain types for seats, players, cards, sessions, hands, digests, and chip values. Prefer typed constructors over unchecked integers or raw byte vectors.

Write Rustdoc and comments for future local readers and AI agents: mark invariants, caller obligations, proof-context bindings, and source-of-truth boundaries. Do not comment obvious code or leave commented-out implementations.

## Testing Guidelines

Test observable consensus behavior: proof binding, transcript hash chains, wire decoding bounds, checkpoint semantics, recovery evidence, betting legality, chip conservation, and tamper rejection. Testkit helpers must use public production APIs and must not expose fake proofs, bypass constructors, or secret-inspection hooks.

## Commit & PR Guidelines

Use imperative commit messages, for example `Add crate-boundary ADRs`. Prefer small `jj commit` steps. Do not move bookmarks, push, or rewrite shared history unless explicitly requested.

PRs should include context, validation commands, known limitations, and links to relevant ADRs for architecture, wire, recovery, or security changes.

## Failure & Fallback Policy

Do not add plausible fallback behavior. Failed proof verification, unknown wire versions, invalid actions, missing checkpoint evidence, unavailable production capabilities, and malformed transcripts must fail explicitly. Mock, deterministic, or experimental behavior is allowed only when the selected testkit or fixture names it clearly.
