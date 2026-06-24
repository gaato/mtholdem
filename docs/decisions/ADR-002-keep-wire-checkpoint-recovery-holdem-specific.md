# ADR-002: Keep Wire, Checkpoint, and Recovery Hold'em-Specific

## Status
Accepted

## Date
2026-06-24

## Context
Royale models a general card-game functionality parameterized by a game-rule program. In Rust, accepting arbitrary game callbacks or bytecode would not by itself implement the verifier needed for safe recovery and settlement.

## Decision
Keep signed wire payloads, public transcripts, checkpoints, recovery bundles, showdown receipts, and settlement-facing state in `mtholdem`. Do not move them into `mental-card-core` until at least two concrete game crates prove the same state model and verification semantics.

## Alternatives Considered

### Generic checkpoint/recovery traits in core
- Pros: More reuse on paper.
- Cons: Payout, legal actions, expected actor, and mediator parity are game-specific.
- Rejected: It would make unsafe behavior look reusable.

### Raw `Vec<u8>` extension hooks everywhere
- Pros: Flexible.
- Cons: Pushes consensus-critical validation to callers.
- Rejected: It weakens typed verification and makes agents likely to invent fallbacks.

## Consequences
- `mental-card-core` remains a card-operation substrate, not a complete game framework.
- Hold'em wire and checkpoint versions can evolve independently of crate versions.
- New games should define their own typed wire and recovery artifacts above `SessionBinding`.
