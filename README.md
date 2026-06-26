# mtholdem workspace - Mental Texas Hold'em for Rust

This repository is a **Sans-I/O Rust workspace** for building peer-to-peer Texas
Hold'em without a trusted dealer. It separates reusable encrypted-card
operations, Hold'em protocol rules, peer-session orchestration, byte-oriented
adapter boundaries, host-side table events, and deterministic test fixtures into
separate crates.

> **Important:** This is unaudited research and prototype software. It has not
> received an independent cryptographic audit, formal verification, or production
> penetration testing. Do not use it for real money, prizes, or redeemable assets
> without third-party review of the cryptography, transport, persistence,
> randomness, recovery mediator, and settlement contract.

Japanese README: [README.ja.md](README.ja.md)

API documentation for `main`: <https://gaato.github.io/mtholdem/>

## Workspace Layout

| crate | Purpose |
|---|---|
| [`mental-card-core`](crates/mental-card-core) | Standard-card cryptography, identities, rosters, threshold keys, shuffles, and reveals |
| [`mtholdem`](crates/mtholdem) | No-limit Texas Hold'em rules, signed wire artifacts, transcripts, checkpoints, recovery evidence, and settlement-facing state |
| [`mtholdem-peer`](crates/mtholdem-peer) | Transport-neutral peer-session orchestration |
| [`mtholdem-adapter`](crates/mtholdem-adapter) | Byte-oriented boundary shared by CLI, C ABI, and downstream WASM packages |
| [`mtholdem-bot`](crates/mtholdem-bot) | Host-side bot policy for demos and single-player shells |
| [`mtholdem-table`](crates/mtholdem-table) | Host-side table event vocabulary for CLI, C ABI, Godot, WASM, and tests |
| [`mtholdem-ffi`](crates/mtholdem-ffi) | C ABI with opaque handles, ABI versioning, error codes, and caller-owned buffers |
| [`mtholdem-cli`](crates/mtholdem-cli) | Scriptable command-line entrypoint |
| [`mtholdem-testkit`](crates/mtholdem-testkit) | Deterministic fixtures for downstream integration tests |

`mtholdem` is the Hold'em-specific rules/protocol crate. Crates that need only
generic card cryptography, identities, rosters, or the deck state machine should
depend directly on `mental-card-core`.

Future target wrappers may include a Python package for research, equity
calculation, transcript analysis, and bot diagnostics. That surface should be a
Pythonic package over the Rust crates, not a second protocol implementation; see
[ADR-017](docs/decisions/ADR-017-allow-python-package-as-research-and-analysis-wrapper.md).

Crate-boundary rationale lives in
[docs/crate-boundaries.md](docs/crate-boundaries.md).

## References

The implementation is informed by these papers and upstream implementation
sources:

- [Kaleidoscope: An Efficient Poker Protocol with Payment Distribution and
  Penalty Enforcement][paper-kaleidoscope] informs the poker-specific flow,
  private and public openings, compact checkpoints, and recovery evidence.
- [ROYALE: A Framework for Universally Composable Card Games with Financial
  Rewards and Penalties Enforcement][paper-royale] informs the separation
  between card operations, game rules, and settlement-facing recovery.
- [21 - Bringing Down the Complexity: Fast Composable Protocols for Card Games
  Without Secret State][paper-21] marks the boundary for games that do not need
  private-card state.
- [Bayer-Groth 2012, Efficient Zero-Knowledge Argument for Correctness of a
  Shuffle][paper-bg] is the shuffle-proof construction implemented under
  `mental-card-core`.
- [`ziffle` 0.1.0][ziffle] is the source of the derived Bayer-Groth
  implementation. See
  [THIRD_PARTY_NOTICES.md](crates/mental-card-core/THIRD_PARTY_NOTICES.md).

The papers' security proofs do not automatically transfer to this Rust
implementation, and this implementation does not provide a UC proof.

[paper-kaleidoscope]: https://eprint.iacr.org/2017/899
[paper-royale]: https://eprint.iacr.org/2018/157
[paper-21]: https://eprint.iacr.org/2018/303.pdf
[paper-bg]: http://www0.cs.ucl.ac.uk/staff/J.Groth/MinimalShuffle.pdf
[ziffle]: https://github.com/v26-solutions/ziffle

## Features

- 52-card encrypted deck with Bayer-Groth shuffle proofs.
- Public/private reveal shares with Chaum-Pedersen DLEQ proofs.
- Canonical wire artifacts bound to session, config, hand, phase, sender,
  sequence, and previous digest.
- Hole-card receipt commitments to exact signed private-share sets.
- Showdown opening that checks both receipts and public bundle commitments.
- Unanimous compact checkpoints plus recovery suffixes and equivocation evidence.
- No-limit Hold'em blinds, short all-ins, side pots, odd chips, muck, and
  uncontested pots.
- Deterministic hand evaluation from 5, 6, or 7 cards.
- Configuration checks for `compensation > maximum_hand_loss` and
  `deposit >= compensation * (players - 1)`.

## Non-Goals

The workspace is not a network server or settlement system. A host application
still needs at least:

- authenticated broadcast with one total public order;
- authenticated confidential channels for private reveals;
- retry, timeout, peer discovery, NAT traversal, rate limiting, and DoS controls;
- anti-rollback persistence and secret storage/deletion;
- a mediator or smart contract that checks timeouts and evidence before
  distributing collateral;
- legal compliance, age checks, licensing, KYC/AML, tax handling, and responsible
  gaming controls.

Because the cryptographic threshold is `(n, n)`, any participant can stop
off-chain progress by withholding a share. Availability requires an external
mediator that records challenges and deadlines, identifies aborting participants,
and applies penalties.

## Quick Start

Rust 1.85 or newer is required.

```bash
cargo test --locked --workspace --all-targets --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo run --locked -p mtholdem --example simulated_hand
cargo run --locked -p mtholdem-cli -- abi-version
cargo run --locked -p mtholdem-cli -- play-bot --players 3 --hands 1
cargo run --locked -p mtholdem-cli -- play-bot --bots-only --players 3 --seed 7
cargo run --locked -p mtholdem-cli -- play-bot --bots-only --players 3 --hands 2 --backend rules-only
cargo run --locked -p mtholdem-cli -- play-bot --bots-only --players 6 --hands 200 --backend rules-only --stats
cargo run --locked -p mtholdem-cli -- play-bot --players 6 --hands 10 --backend rules-only --watch-bot seat0
```

`play-bot` prints public board updates as flop, turn, and river even after the
human seat folds. Bot hole cards are shown only when publicly opened at
showdown. Multi-hand runs carry final stacks into the next hand and print the
selected backend, starting stacks, and button at each hand boundary.
`--stats` adds bot-action telemetry for VPIP, PFR, RFI, 3-bet,
fold-to-open, and blind defense.
`--watch-bot seatN` runs that seat as a bot and prints its private hand plus
decision diagnostics before each watched action.

Minimal Hold'em session configuration:

```rust
use mental_card_core::identity::IdentitySecret;
use mtholdem::config::{EconomicSecurity, Roster, SessionConfig};
use mtholdem::{Chips, Result, SessionId};
use rand::rngs::OsRng;

fn make_config() -> Result<SessionConfig> {
    let mut rng = OsRng;
    let identities: Vec<_> = (0..3)
        .map(|_| IdentitySecret::generate(&mut rng))
        .collect();
    let roster = Roster::new(
        identities.iter().map(IdentitySecret::public).collect(),
    )?;
    let economics = EconomicSecurity::new(
        roster.len(),
        Chips::from_units(100),
        Chips::from_units(101),
        Chips::from_units(202),
    )?;

    SessionConfig::builder(
        SessionId::random(&mut rng),
        roster,
        Chips::from_units(1),
        Chips::from_units(2),
        Chips::from_units(100),
    )
    .economics(economics)
    .timeout_millis(30_000)
    .build()
}
```

A local simulation covering cryptographic shuffling, hole-card delivery, board
opening, betting, and showdown lives in
[crates/mtholdem/examples/simulated_hand.rs](crates/mtholdem/examples/simulated_hand.rs).

## Testkit

Integration tests can prepare a deterministic table:

```rust
use mtholdem::{CardPosition, HandNumber};
use mtholdem_testkit::TestTable;

let mut table = TestTable::standard(3, [7; 32])?;
let hand = table.prepare_hand(HandNumber::FIRST)?;
let shares = table.public_reveal_shares(&hand, CardPosition::new(0)?)?;
let card = hand
    .cryptography()
    .open_public_card(CardPosition::new(0)?, &shares)?;
# Ok::<(), mtholdem::Error>(())
```

The seeded RNG in `mtholdem-testkit` is predictable. Never use it for production
keys or shuffles. Keeping the testkit in a separate crate also keeps it out of
production dependency graphs unless a downstream crate opts into it.

## Security and Validation

- Threat model and deployment assumptions: [SECURITY.md](SECURITY.md)
- Wire formats: [docs/wire-format.md](docs/wire-format.md)
- Settlement contract boundary: [docs/settlement-contract.md](docs/settlement-contract.md)
- Validation record: [VALIDATION.md](VALIDATION.md)
- Third-party code notice: [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)

Licensed under the Mozilla Public License 2.0. Third-party code notices are
listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
