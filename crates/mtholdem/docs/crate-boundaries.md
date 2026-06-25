# Crate Boundary Decisions for the Initial Workspace API

This document records why the crate graph has this shape and what should be
shared when a new game or wrapper is added. It is a design contract for crate
boundaries and dependency direction.

## Decision

```text
mental-card-core
    ^
    `-- mtholdem
          ^
          |-- mtholdem-peer
          |     ^
          |     `-- mtholdem-adapter
          |           ^
          |           |-- mtholdem-cli
          |           `-- mtholdem-ffi
          |-- mtholdem-bot
          |     ^
          |     `-- mtholdem-cli
          `-- mtholdem-testkit
```

Dependencies are one-way. Core does not know Hold'em, and `mtholdem` does not
know the peer driver, adapter, wrappers, or testkit. This avoids dependency
cycles where the production verification crate accidentally pulls in transport
loops or deterministic test RNGs.

## What Belongs in `mental-card-core`

Only concepts meeting all of these conditions belong in core:

1. Their invariants can be explained without betting, turn, or payout rules.
2. Misuse can be rejected through types or constructor validation.
3. At least two plausible game crates can reuse the same semantic contract.
4. Game-specific values needed for domain separation can be passed as opaque
   digests.

The current core concepts are rosters, identities, standard-card identities,
threshold cryptography, verified decks, verified reveal shares, and per-deal
key/shuffle/reveal orchestration.

`SessionBinding` is the key boundary. Core does not interpret
`configuration_digest`, but it binds that digest into every proof context. A
higher game crate is responsible for canonicalizing all consensus-critical
immutable values beyond the roster and including them in that digest.

## What Remains in `mtholdem`

These concepts only make sense when verified with the Hold'em rule program:

- blinds, betting actions, and street transitions;
- exactly two hole cards per seat;
- flop/turn/river position plans;
- public-event phase/payload compatibility;
- side pots, eligibility, muck, and odd chips;
- ledger state needed by checkpoints;
- the next correct message a mediator asks for during recovery.

Putting these behind generic associated types or `Box<dyn GameRules>` would make
canonical serialization, exhaustive validation, and mediator parity easier to
implement inconsistently. The initial API keeps concrete types. A small shared
crate should be extracted only after another game demonstrates the same state
model.

## Why Wire, Checkpoint, and Recovery Are Not Generic Yet

[ROYALE][paper-royale] parameterizes a general functionality over a game-rule
program `GR`. Accepting arbitrary bytecode in a library API is not the same
thing as implementing that contract verifier. An implementation needs at least:

- a deterministic verifier for every transition;
- canonical, versioned, bounded artifacts;
- game-specific state invariants;
- identical off-chain and mediator judgments;
- exact payout and penalty semantics.

The first public API therefore keeps card cryptographic operations as the
reusable substrate and leaves game-execution artifacts in the Hold'em crate.

## Public-State Games Are a Sibling Protocol Family

Games such as Blackjack or Baccarat in
[21 - Bringing Down the Complexity][paper-21] do not need encrypted private
opening in the same way secret-card Hold'em does. They can select public cards
with commit-and-open coin tossing, preprocessed commitments, or VRFs.

Adding that protocol family as a `mental-card-core` feature would create several
problems:

- incompatible security assumptions and state machines in one API;
- `VerifiedDeck` would mean both "encrypted shuffled deck" and "unopened value
  multiset";
- dependency feature unification could pull in irrelevant cryptographic
  primitives;
- callers could choose the wrong protocol for the game's secret-state
  requirement.

A future public-state implementation should be a sibling crate such as
`mental-public-card`. Identity and strong ID types can be extracted into a
smaller shared crate only after actual use justifies it.

[paper-royale]: https://eprint.iacr.org/2018/157
[paper-21]: https://eprint.iacr.org/2018/303.pdf

## Why the Core Uses a Standard 52-Card Deck

Const-generic deck sizes or arbitrary card value types are technically possible,
but proof CRS, ciphertext count, canonical card encoding, wire bounds, and hand
evaluation move together. Publishing unused generic parameters would increase
API stability and audit surface.

The initial public API keeps `Card`, `Rank`, `Suit`, and the 52-card
cryptographic deck in core. If another deck appears, first factor the internals,
then consider a public abstraction after test vectors and benchmarks show
semantic equivalence.

## Why the Testkit Is a Separate Crate

`cfg(test)` helpers are unavailable to downstream integration tests. Publishing
deterministic key/shuffle helpers as a production feature would create feature
leakage and production misuse risk. A separate crate makes the dependency graph
explicit and keeps the helpers in `dev-dependencies`.

The testkit composes only public APIs. It does not expose proof-verification
bypasses or secret-inspection hooks. Passing testkit scenarios also checks that
the downstream integration surface is usable.

## Why Peer Sessions Are a Separate Crate

P2P multiplayer needs local identity, key shares, message ordering, replay
rejection, and host effects. Sockets, timers, databases, and UI differ per
deployment. `mtholdem-peer` orchestrates `SignedPublicMessage`,
`SignedPrivateReveal`, `HandCryptography`, checkpoint/recovery artifacts, and
typed `HostEffect` values without owning transport.

This lets CLI, FFI, downstream WASM/npm packages, and Godot wrappers share the
same Rust-owned peer state.

## Why Adapter and Target Wrappers Are Separate

CLI, C ABI, downstream WASM packages, and Godot bindings use the same
peer-session state, but allocator, buffer ownership, error reporting, and
language binding constraints differ by target. `mtholdem-adapter` translates
typed `mtholdem-peer` effects and actions into byte-oriented boundary values.

`mtholdem-cli` and `mtholdem-ffi` own only the outside of that adapter boundary.
The C ABI exposes opaque handles, ABI versions, and caller-visible buffer
ownership; it does not expose Rust struct layout. The C ABI release contract also
includes the official C header, C-visible constants, and layout/version checks
described in
[ADR-009](../../../docs/decisions/ADR-009-publish-c-abi-header-and-constants.md).
WASM/npm packages should live outside this workspace and consume the Rust
adapter boundary. Godot GDExtension should also live outside this workspace and
consume the C ABI.

`mtholdem-bot` is a host-side policy crate, not a protocol crate. It chooses
legal betting actions for demos from public context, known local hole cards, and
the rules engine's `LegalActions`. It does not drive peer sessions, interpret
wire artifacts, validate transcripts, or expose a C ABI surface. CLI and future
single-player shells may use it to answer `RequestAction` effects while keeping
the peer/session loop in the host. See
[ADR-010](../../../docs/decisions/ADR-010-keep-bot-policy-host-side.md) for the
single-player and human-input boundary. The bot policy itself is still
pre-stable and should be redesigned around assessment, intent, and sizing
rather than patched around the initial min-raise heuristic; see
[ADR-013](../../../docs/decisions/ADR-013-redesign-bot-policy-around-assessment-and-sizing.md).

Multi-hand table/session running is also host-side orchestration. Stack
carry-over, button rotation, busted-seat handling, human-seat prompts, bot-seat
dispatch, and Godot-friendly table events belong above `mtholdem-peer`, not
inside the peer driver or bot policy. `mtholdem-table` owns the first shared
table event vocabulary; the CLI may still keep runner control flow internally
while the behavior is experimental. The peer-backed backend is the reference
path; a rules-only backend may use `mtholdem` plus `mtholdem-bot` for
lightweight local play while preserving the same table event vocabulary. See
[ADR-011](../../../docs/decisions/ADR-011-keep-table-session-runner-host-side.md).

If this table runner is exported through C, it must use a separate
`mth_table_*` ABI surface rather than overloading the existing `mth_peer_*`
peer-session handles. `mth_peer_*` remains the P2P artifact-routing boundary;
`mth_table_*` is for local table events, explicit backend selection, bot seats,
stack carry-over, and host action prompts. See
[ADR-012](../../../docs/decisions/ADR-012-add-separate-table-runner-c-abi-surface.md).

Custom bot clients that join an open game are ordinary participants, not a
special table-runner mode. They should use `mth_peer_*`, own their identity and
transport integration, answer action effects in their host layer, and submit
the same signed artifacts as human-controlled clients. The table runner may
embed local bot seats for convenience, but it is not the external bot
participation protocol.

## Naming

- `mtholdem`: the Texas Hold'em rules/protocol crate.
- `mtholdem-peer`: transport-neutral peer-session orchestration.
- `mtholdem-adapter`: the byte/event/action/error/status boundary shared by
  target wrappers.
- `mtholdem-bot`: host-side heuristic action policy for demos and
  single-player shells.
- `mtholdem-table`: host-side table event vocabulary for local runners, CLI,
  C ABI planning, Godot wrappers, WASM, and tests.
- `mtholdem-cli`: the scriptable CLI wrapper.
- `mtholdem-ffi`: the C ABI boundary and opaque-handle owner.
- `mental-card-core`: the workspace's dependency core for standard-card
  secret-state protocols, not a universal card-game framework.
- `mtholdem-testkit`: deterministic fixtures tied to Hold'em configuration and
  wrappers.

`mental-card-core` does not promise arbitrary decks or multiple protocol
families. The word `core` means the dependency core of this workspace.

## Release and Versioning

Public workspace crates use the same workspace version. Release validation
follows the dependency graph: `mental-card-core`, `mtholdem`, `mtholdem-peer`,
`mtholdem-adapter`, in-workspace target wrappers, then `mtholdem-testkit`.
Downstream package verification runs only after its same-version dependency
crates are available from the registry.

Wire versions are independent from crate versions. A crate split does not change
signed bytes. Artifact versions change only when signed meaning changes.
