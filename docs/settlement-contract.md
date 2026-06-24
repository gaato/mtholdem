# Settlement and recovery adapter requirements

`mtholdem` provides evidence and deterministic verification code, but does not implement a specific
blockchain contract. This document describes the minimum behavior expected from a stateful mediator
that follows the Kaleidoscope/Royale penalty pattern.

## Funds and immutable setup

Before off-chain play, the mediator records:

- the exact roster and identity verification keys;
- the `SessionId` and `SessionConfig::digest()`;
- small blind, big blind, buy-in, maximum hand loss, compensation, and deposit;
- a response timeout and an unambiguous clock/block-height policy;
- each player's locked stake plus security deposit;
- supported wire/proof versions and cryptographic verification rules.

The adapter must reject check-in unless all deposits are actually controlled by the mediator. The
library's arithmetic checks do not lock funds.

## Happy-path payout

Peers play entirely off-chain. At a valid terminal state they produce a unanimous payout or complete
checkpoint. The mediator verifies:

1. session/config identity and supported artifact versions;
2. canonical decoding and all roster signatures;
3. payout and checkpoint chip conservation plus per-seat bounds;
4. exact hole-card receipt commitments and, when a deck is present, its aggregate-key
   commitment/count;
5. that no newer accepted settlement exists;
6. replay protection for the hand/session.

It then returns deposits and transfers each final payout exactly once.

## Recovery initiation

Any player may submit a `RecoveryBundle` containing the latest certified checkpoint known to it and
the full public suffix after that checkpoint. The mediator:

1. verifies checkpoint version 2, the certificate, and all semantic state invariants;
2. resolves the exact verified aggregate key needed by a checkpointed encrypted deck and checks its
   digest/count;
3. verifies the suffix from the checkpoint's `TranscriptAnchor`;
4. chooses the most recent valid unanimous checkpoint among all responses;
5. publishes the chosen state and exact next required protocol step;
6. gives every required actor a deterministic deadline.

A player that supplies an older but valid checkpoint is not automatically guilty; network races can
produce different local knowledge. The mediator chooses the newest certificate that is valid and
consistent with submitted evidence.

## Classifying faults

### Signed invalid action

A signed message can prove a fault when it has a valid identity signature but violates the state
transition, declared phase, actor order, chip cap, proof relation, or canonical context. Preserve the
original bytes and the prior certified state so all verifiers reach the same conclusion.

### Equivocation

`EquivocationEvidence` proves that one identity signed two different public messages for the same
session, hand, sequence, sender, and previous transcript digest. The payload or declared phase may
differ; that difference is the fork. Both signatures must verify.

### Substituted showdown shares

A structurally valid `ShowdownBundle` is not enough. The mediator must obtain the bundle digest from
the owner's signed `ShowdownOpen`, obtain the earlier receipt digest from the owner's signed
`HoleCardsReceived` or certified checkpoint, and apply the same checks as
`HandCryptography::open_showdown_bundle`. This prevents a player from generating a fresh valid share
set after observing betting outcomes.

### Omission or abort

Absence has no signature. The mediator must issue a challenge for one exact message, make that
challenge observable to all parties, wait according to the configured deadline policy, and record
that no valid response arrived. A peer's local timeout alone is insufficient to confiscate funds.

### Invalid recovery complaint

A complainant should not be penalized merely because the next step eventually succeeds; complaints
can result from packet loss. Penalize only behavior defined in the immutable settlement policy, and
ensure that malicious complaint spam cannot indefinitely reset deadlines.

## Penalty distribution

For a proven faulty player `i`, the papers' intended pattern compensates every non-faulting active
player from `i`'s collateral, while preserving already-accounted balances and bets. The deployment
must define:

- whether folded or checked-out players receive compensation;
- how fees are paid;
- whether multiple proven offenders share liability;
- how leftover collateral is handled;
- whether the current pot is refunded or preserved;
- rounding and smallest-unit behavior.

`EconomicSecurity` enforces the sufficient inequalities used by the papers:

```text
compensation > maximum_hand_loss
security_deposit >= compensation * (player_count - 1)
```

These conditions are not a game-theoretic proof for every token, fee market, or legal environment.

## On-chain verification cost

Bayer–Groth shuffle verification and elliptic-curve operations may be expensive or unavailable in a
particular VM. A practical adapter can mediate only from the latest compact checkpoint and current
phase, but it still needs a sound way to verify every operation it may be asked to adjudicate.
Possible designs include a native verifier, a dedicated rollup/validity proof, an optimistic dispute
game, or a permissioned arbitrator. Replacing verification with a trusted assertion changes the
threat model and must be disclosed.

## Required tests for an adapter

At minimum, test happy payout, stale checkpoint submission, conflicting checkpoints, checkpoint
version mismatch, wrong aggregate key, non-conserving chip state, conflicting receipt replacement,
substituted showdown shares, signature forgery, malformed lengths, wrong session/config, invalid
shuffle/reveal proof, illegal bet, contribution-cap violation, equivocation, actor timeout, responder
timeout, replayed payout, and integer overflow. Run differential tests against the Rust verifier
using the exact same canonical vectors.
