# Validation record

This record applies to the current `mtholdem` 0.1.0 seven-crate workspace:
`mental-card-core`, `mtholdem`, `mtholdem-peer`, `mtholdem-adapter`,
`mtholdem-cli`, `mtholdem-ffi`, and `mtholdem-testkit`.
It documents checks actually performed in the reconstruction environment.
Passing them demonstrates build and test hygiene; it is not an independent
cryptographic audit, a formal implementation proof, or a production-readiness
certification.

## Current toolchain

```text
rustc 1.85.0 (4d91de4e4 2025-02-17)
cargo 1.85.0 (d73d2caf9 2024-12-31)
rustfmt 1.8.0
clippy 0.1.85
```

The current workspace declares `rust-version = "1.85"` and Rust edition 2024.

## Current commands completed successfully

```text
cargo +1.85.0 fmt --all -- --check
cargo +1.85.0 check --locked --workspace --all-targets --all-features
cargo +1.85.0 test --locked --workspace --all-targets --all-features
cargo +1.85.0 clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo +1.85.0 doc --locked --workspace --no-deps --all-features
cargo +1.85.0 run --locked -p mtholdem --example simulated_hand
cargo +1.85.0 run --locked -p mtholdem-cli -- smoke-peer
cargo +1.85.0 metadata --locked --format-version 1 --no-deps
```

## Current observed results

- 52 unit/property/adversarial tests passed across the workspace: 8 core, 32
  Hold'em, 3 adapter, 3 FFI, 4 peer, and 2 testkit.
- Clippy completed for the entire workspace, all targets, and all features with
  warnings denied.
- Rustdoc completed for the seven-crate workspace without dependencies.
- The complete three-player simulation opened a five-card board and conserved
  the 300-chip supply.
- The CLI smoke-peer path exercised the adapter/peer boundary and emitted
  `broadcast-public` and `persist` host effects.
- `cargo metadata` reported the intended dependency direction:
  `mental-card-core <- mtholdem <- mtholdem-peer <- mtholdem-adapter <-
  in-workspace target wrappers`, with `mtholdem-testkit` outside production
  dependency graphs.

The test suite covers aggregate-key/deck binding, reveal-share
deck/position/recipient binding, private-receipt and showdown substitution
rejection, checkpoint semantics and chip conservation, side-pot conservation,
all-in edge cases, equivocation attribution, malformed/untrusted decoder input,
game/core roster-boundary separation, deterministic test fixtures, and complete
key/shuffle/reveal paths through the peer/session wrappers.

## Historical reconstruction inputs

The following inputs belonged to the earlier source reconstruction and remain as
provenance for the pre-wrapper workspace.

- Original uploaded source archive SHA-256:
  `1db98265c0dee65b88444e0553e256c1aaf75a0d7dc1236fd9864ffc5f29bc3a`
- Kaleidoscope (2017/899) SHA-256:
  `28fef45dac8c4ebeadbba16875c894026e190976c59b3b20014f3a20c1711bb0`
- Royale (2018/157) SHA-256:
  `b6638dacc95a28eece1326e94b38d8b97fd41a5d4881bf66b152157538893375`
- 21 / Bringing Down the Complexity (2018/303) SHA-256:
  `b77ee3b74efa7cb2adb344172a3a25a92b0ff791ff7e0d7eae88f817c3721315`

## Earlier reconstruction toolchain

```text
rustc 1.85.0 (4d91de4e4 2025-02-17) (built from a source tarball)
cargo 1.85.0 (d73d2caf9 2024-12-31)
rustfmt 1.8.0
clippy 0.1.85
```

This earlier validation was performed with Rust 1.85 before the peer, adapter,
CLI, and FFI crates were added.

## Historical commands completed successfully

```text
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings

# Debug tests. The core suite was split only to fit the execution harness wall-clock limit.
cargo test --locked -p mental-card-core --all-targets --all-features -- \
  --test-threads=1 \
  --skip protocol::tests::three_players_can_shuffle_and_open_public_and_private_cards
cargo test --locked -p mental-card-core \
  protocol::tests::three_players_can_shuffle_and_open_public_and_private_cards -- --exact
cargo test --locked -p mtholdem --all-targets --all-features -- --test-threads=1
cargo test --locked -p mtholdem-testkit --all-targets --all-features -- --test-threads=1

# Release tests.
cargo test --release --locked -p mental-card-core --all-targets --all-features -- \
  --test-threads=1
cargo test --release --locked -p mtholdem --all-targets --all-features -- --test-threads=1
cargo test --release --locked -p mtholdem-testkit --all-targets --all-features -- \
  --test-threads=1

cargo test --locked --workspace --doc --all-features
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps --all-features
cargo run --locked -p mtholdem --example simulated_hand
cargo metadata --locked --format-version 1 --no-deps
cargo tree --locked --duplicates
git diff --check
cargo package --list -p mental-card-core --allow-dirty
cargo package --list -p mtholdem --allow-dirty
cargo package --list -p mtholdem-testkit --allow-dirty
```

The `mental-card-core` package was additionally built by Cargo from its generated, independently
extracted package tree:

```text
cargo package --locked -p mental-card-core --allow-dirty
```

The environment used an authenticated internal mirror configured outside the source tree. No mirror
credential or project-local Cargo registry configuration is distributed.

## Historical observed results

- 42 unit/property/adversarial tests passed in debug mode: 8 core, 32 Hold'em, and 2 testkit.
- The same 42 tests passed under the optimized release profile.
- 3 Rustdoc tests passed.
- Clippy completed for the entire workspace, all targets, and all features with warnings denied.
- Rustdoc completed for the then-current three crates with warnings denied.
- The complete three-player simulation opened a five-card board and conserved the 300-chip supply.
- Cargo verified `mental-card-core` from its generated package tree.
- Cargo package-content lists contained 18 core files, 27 Hold'em files, and 9 testkit files.
- `cargo metadata` reported the then-current dependency direction:
  `mental-card-core <- mtholdem <- mtholdem-testkit`.

The test suite covers aggregate-key/deck binding, reveal-share deck/position/recipient binding,
private-receipt and showdown substitution rejection, checkpoint semantics and chip conservation,
side-pot conservation, all-in edge cases, equivocation attribution, malformed/untrusted decoder
input, game/core roster-boundary separation, deterministic test fixtures, and the complete
key/shuffle/reveal path.

## Packaging caveat

Cargo cannot prepare a publishable downstream tarball against crates.io until
the exact same-version upstream package is present in that registry. Release
automation must publish and verify in dependency order, starting with:

1. `mental-card-core` 0.1.0;
2. `mtholdem` 0.1.0;
3. `mtholdem-peer` 0.1.0;
4. `mtholdem-adapter` 0.1.0;
5. in-workspace target wrappers and `mtholdem-testkit` 0.1.0 as their dependency
   constraints permit.

Before publication, package-content manifests should be checked with
`cargo package --list`; the complete source-distribution ZIP should also be
extracted and tested independently as a workspace.

## Checks not performed

`cargo-audit`, Miri, sanitizers, long-running fuzz campaigns, formal verification, differential
cross-implementation vectors, and an independent manual cryptographic audit were not completed pass
gates. Their absence is not a clean result. A value-bearing deployment should add them and audit the
external transport, persistence, mediator/contract, randomness, and operational controls.
