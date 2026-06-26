# Architecture

This document maps the complete workspace. It deliberately repeats important Rustdoc so a reader
who sees only one file can still determine where a change belongs and which invariants are owned by
another crate.

## Design goals

The implementation separates seven concerns:

1. **Game-neutral encrypted-card state:** verified roster keys, one exact encrypted deck, and reveal
   shares valid for one exact deck position and target.
2. **Authenticated Hold'em history:** who sent which typed event in which session, hand, phase, and
   sequence, and which event immediately preceded it.
3. **Deterministic Hold'em rules:** legal actions and chip distribution, independent of networking
   and cryptography.
4. **Economic recovery evidence:** the compact state and suffix a mediator needs to resume or end a
   disputed Hold'em execution.
5. **Test assembly:** predictable fixtures that compose only the public production APIs and never
   weaken verification.
6. **Host-side demo policy:** optional bot choices that consume legal action context without
   becoming consensus rules.
7. **Host-side table running:** optional multi-hand orchestration that turns peer effects into
   table events without becoming peer-session or UI code.

All production crates are Sans-I/O. They do not open sockets, schedule timers, write databases, or
submit transactions. Applications may use Tokio, QUIC, WebRTC, libp2p, a relay, or an in-memory
harness without changing the consensus state machines.

## Crate dependency map

```text
                 mental-card-core
       ┌───────────────────────────────────┐
       │ card, types, identity, roster     │
       │ low-level crypto + verified types │
       │ CardCryptography                  │
       └─────────────────┬─────────────────┘
                         │
                         v
                    mtholdem
       ┌───────────────────────────────────┐
       │ SessionConfig / EconomicSecurity  │
       │ HandCryptography wrapper          │
       │ wire + transcript                 │
       │ betting + HoldemHand + evaluator  │
       │ checkpoint + recovery             │
       └─────────────────┬─────────────────┘
                         │
                         v
                  mtholdem-peer
       ┌───────────────────────────────────┐
       │ PeerSession + HostEffect          │
       │ transport-neutral orchestration   │
       └───────────────────────────────────┘

                   mtholdem-bot
       ┌───────────────────────────────────┐
       │ heuristic legal-action policy     │
       │ for demos and single-player hosts │
       └───────────────────────────────────┘

                  mtholdem-table
       ┌───────────────────────────────────┐
       │ host-side table event vocabulary  │
       │ for CLI / C ABI / Godot / WASM    │
       └───────────────────────────────────┘

                 mtholdem-testkit
       ┌───────────────────────────────────┐
       │ seeded public-API fixtures        │
       └───────────────────────────────────┘
```

The arrows are compile-time dependencies. `mental-card-core` has no dependency on a game crate;
`mtholdem` has no dependency on peer drivers, bot policy, or the testkit.

## `mental-card-core`

### `types`

Strong IDs for sessions, players, seats, deal numbers, card positions, and 32-byte digests. The core
uses `DealNumber`; the Hold'em crate exposes the domain alias `HandNumber`. Financial units
and the 2-to-10 table limit remain in `mtholdem::types`.

### `identity`

Long-lived Ed25519 identities. Public keys determine `PlayerId`; secret values have redacted
`Debug` and zeroization. Identity signatures authenticate artifacts but do not encrypt transports.

### `session`

`Roster` validates unique identities and immutable contiguous seat order. `SessionBinding` combines
that roster with a session ID and an opaque game-configuration digest. The core does not interpret
blind schedules or game rules, but every proof context is bound to their digest.

### `card`

Stable suit-major representation of a standard 52-card deck. It intentionally contains no poker
hand-ranking logic.

### `crypto`

Typed wrapper over the private Bayer--Groth implementation. It owns serialized-size checks, key
ownership verification, aggregate keys, shuffle verification, reveal-share verification, and card
reconstruction. A `VerifiedDeck` is bound to exact aggregate-key membership and threshold count; a
`VerifiedRevealShare` is bound to the exact deck digest and card position.

### `protocol::CardCryptography`

Game-neutral state machine for one deal: key registration, aggregate-key finalization, one canonical
shuffle per roster seat, and public/private reveal-share creation and opening. It does not sign or
transport values and does not decide whether the game is currently allowed to reveal a position.

## `mtholdem`

### `config`

Adds blinds, buy-in, timeout, and collateral bounds to the core roster. `SessionConfig::digest`
commits every immutable Hold'em interpretation value. `cryptographic_binding` passes only the
session, roster, and digest to the core.

### `protocol::HandCryptography`

A thin Hold'em wrapper over `CardCryptography`. It adds signed private reveals, exactly-two-card
private receipts, and the complete showdown acceptance path. Use this wrapper from Hold'em
applications unless implementing a different game crate.

### `wire`

Canonical signed Hold'em artifacts. Every public event binds session, config digest, hand, sequence,
phase, sender, previous event digest, and typed payload. Private reveals additionally bind deck,
recipient, and card position. Decoders are bounded and reject trailing bytes.

### `transcript`

Verifier/store for the public hash chain. It checks sequence, predecessor, context, structure, and
identity signature before advancing its anchor. It does not pick between competing valid events;
those must be retained as equivocation evidence.

### `deal`

Pure mapping from button and active-seat state to encrypted deck positions, including standard burn
positions.

### `card`, `betting`, `holdem`

Pure deterministic Hold'em logic. `card` evaluates hands, `betting` validates one no-limit round and
constructs pots, and `holdem` controls streets and settlement. No secret information or network
timing enters these modules.

### `checkpoint`

Unanimously signed public state. Verification rechecks canonical seat order, blind value,
contribution caps, stack/status consistency, board uniqueness, expected-actor eligibility, complete
chip conservation, exact private-receipt commitments, canonical deck bytes, and aggregate-key
commitment/count. A certificate can restore a typed verified deck only after semantic verification
and only with the exact current aggregate key.

The core exposes one hidden restoration hook because Rust has no friend-crate visibility. It is not a
normal downstream API; `CertifiedCheckpoint::restore_verified_deck` is the supported trust boundary.

### `recovery`

Latest certificate plus the complete signed public suffix, or signed equivocation evidence. A
timeout is a mediator-observed fact, not a self-contained cryptographic proof.

## `mtholdem-testkit`

`TestTable` creates deterministic identities and configuration, then executes genuine key proofs and
all genuine shuffles through public APIs. It offers no fake proof or unchecked constructor. The
seeded RNG is deliberately predictable and therefore suitable only for tests.

## `mtholdem-bot`

`HeuristicBot` converts a `BotDecisionContext` and a host-provided RNG into one legal `BetAction`.
The profile values model looseness, aggression, call bias, bluffing, and randomness for demos. The
crate does not drive `PeerSession`, parse wire artifacts, verify transcripts, or expose an ABI; hosts
use it only when answering `HostEffect::RequestAction`. The current policy is still pre-stable, but
it now exposes assessment, intent, and sizing stages before final legality mapping. Postflop
assessment includes bounded Monte Carlo equity over unknown cards and a lightweight opponent range
threshold while respecting public information boundaries.
Peer-backed and rules-only hosts should feed it the same public action context
so sizing does not depend on backend internals. See
[ADR-013](../../../docs/decisions/ADR-013-redesign-bot-policy-around-assessment-and-sizing.md)
and [ADR-015](../../../docs/decisions/ADR-015-include-public-action-context-in-action-requests.md).
The opponent-quality target remains a range-aware Monte Carlo heuristic, not a
solver or self-play system; see
[ADR-016](../../../docs/decisions/ADR-016-build-bot-as-range-aware-monte-carlo-heuristic.md).

## Host table events and future runner

Multi-hand table orchestration stays above `mtholdem-peer` and
`mtholdem-bot`. `mtholdem-table` owns the first shared event vocabulary for
backend selection, board openings, showdown summaries, and final hand
summaries. The CLI still owns the experimental runner control flow for stack
carry-over, button rotation, busted-seat handling, human prompts, and bot
dispatch. Move that control flow into `mtholdem-table` only after another host
needs the same behavior. Peer-backed execution is the reference path because it
exercises P2P artifacts; a rules-only backend may reuse the same table events
for lightweight local play. Showdown hand-category labels are derived only from
opened hands and are display metadata, not consensus inputs. See
[ADR-011](../../../docs/decisions/ADR-011-keep-table-session-runner-host-side.md).
If exported to C, table running uses a separate polling-oriented `mth_table_*`
surface instead of `mth_peer_*`; encode the `mtholdem-table` event vocabulary
for external hosts. See
[ADR-012](../../../docs/decisions/ADR-012-add-separate-table-runner-c-abi-surface.md).
Custom bots that join open games use the same `mth_peer_*` participant boundary
as human clients; the table runner is only for local orchestration and
single-player convenience.

## `mtholdem-peer`

`PeerSession` owns local peer identity, per-hand key material, transcript ordering, and host effects.
It emits typed `HostEffect` values such as public broadcasts and persistence requests, but it does
not open sockets, write storage, or schedule deadlines. The initial slice drives key setup,
sequential shuffling, private reveal delivery, hole-card receipts, signed betting actions, and
public community-card reveal shares, board-card opening markers, checkpoint
certification/announcement, recovery-bundle submission, showdown bundle publication,
`ShowdownOpen` commitments, deterministic payout agreement, and final-hand effects with real
`SignedPublicMessage`, `SignedPrivateReveal`, `ShowdownBundle`, `HandCryptography`, `HoldemHand`,
`CertifiedCheckpoint`, `RecoveryBundle`, and `PublicTranscript` values.
Action requests carry legal actions plus public betting context derived from
the rules ledger. Hosts may use that context for UI prompts and bot decisions,
but peer consensus still comes from submitted signed artifacts and rule
validation. See
[ADR-015](../../../docs/decisions/ADR-015-include-public-action-context-in-action-requests.md).

## Per-hand flow

### 1. Session and escrow

Peers agree on one immutable `SessionConfig`, verify its digest, and lock buy-in plus collateral in
an external settlement system. A membership or chip-supply change starts a new session.

### 2. Ephemeral threshold key

Every seat generates a per-hand key share and ownership proof. Every peer registers announcements in
seat order and obtains the same aggregate key. Ownership proofs prevent a rogue key algebraically
chosen to reveal the aggregate secret.

### 3. Sequential verifiable shuffle

Every roster seat rerandomizes and permutes all 52 ciphertexts and proves shuffle correctness. Each
contribution is verified before the next is accepted. At least one honest unpredictable shuffle is
needed to hide final order from a colluding proper subset.

### 4. Hole-card delivery

`DealPlan` determines two positions per active seat. Every roster member sends that owner a signed
private reveal over an authenticated confidential channel. `open_private_hand` requires exactly one
valid share per sender and position, reconstructs both cards, and returns a commitment to the exact
signed share set.

### 5. Public play

Betting actions enter one signed total-order transcript and are replayed through `HoldemHand`. Flop,
turn, and river positions are opened with one public share from every roster member. A folded player
still owns an indispensable threshold key share and must continue participating cryptographically.

### 6. Showdown and payout

An opening player publishes the original private-share set in `ShowdownBundle` and commits its digest
in a signed public event. `open_showdown_bundle` also requires the earlier private-delivery receipt,
rejecting a new proof-valid share set generated after betting. Deterministic evaluation produces
main/side-pot payouts and odd-chip order. Peers certify identical final state before settlement.

## Trust boundaries

### Bytes to parsed value

`from_bytes` proves only canonical structure and bounds. Signature, context, and state-transition
verification remain mandatory.

### Parsed cryptography to verified type

Only successful proof verification creates `VerifiedMentalPublicKey`, `VerifiedDeck`, or
`VerifiedRevealShare`. Callers cannot construct those values from raw bytes through public APIs.

### Game config to core binding

The core trusts the game crate to hash every immutable consensus value. Omitting a rule from the
configuration digest can create cross-configuration replay even though the lower-level proof is
cryptographically valid.

### Local state to certificate

A state digest alone is not a certificate. Every roster member must sign the exact same semantically
valid state. `sum(stack + total_contributed)` must equal `buy_in * roster_size` in the immutable
session model.

### Recovery bundle to penalty

Signed evidence establishes messages, not wall-clock omission. The mediator must issue a challenge,
record delivery and deadline, request the next exact transition, and only then classify a timeout.

## Concurrency

There is no global mutable state. Separate sessions and hands may run in separate tasks. Within one
hand, the host must serialize public events and atomically persist transcript anchor plus checkpoint.
Do not feed competing message orders into one mutable state machine.

## Extension policy

- Put game-neutral card cryptography in `mental-card-core` only when its invariant is independent of
  game flow.
- Add new game crates above `SessionBinding` rather than adding untyped callbacks to `mtholdem`.
- Keep public-state protocols such as
  [21 - Bringing Down the Complexity][paper-21] in sibling crates.
- Add cross-game abstraction only after two concrete consumers demonstrate identical semantics.
- Store application-defined public commitments in `CheckpointState::extension_digest`; do not weaken
  existing checkpoint invariants.

[paper-21]: https://eprint.iacr.org/2018/303.pdf
