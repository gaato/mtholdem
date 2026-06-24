# ADR-001: Split Reusable Card Core from Hold'em

## Status
Accepted

## Date
2026-06-24

## Context
The workspace needs reusable cryptographic card operations while preserving a concrete Texas Hold'em implementation. The same low-level card protocol can support future secret-card games, but betting, streets, payouts, and settlement evidence depend on Hold'em rules.

## Decision
Use three crates with one-way dependencies: `mental-card-core <- mtholdem <- mtholdem-testkit`. Put roster, identity, strong IDs, standard-card representation, threshold ElGamal, key ownership proofs, verifiable shuffles, reveal shares, and `CardCryptography` in `mental-card-core`. Keep Hold'em rules and artifacts in `mtholdem`.

## Alternatives Considered

### Single `mtholdem` crate
- Pros: Simpler package layout.
- Cons: Reusable cryptography remains mixed with Hold'em semantics.
- Rejected: It encourages future games to depend on poker-specific APIs.

### Fully generic game framework
- Pros: Appears extensible.
- Cons: Generic traits would hide game-specific verification and mediator semantics.
- Rejected: The security boundary is not honestly generic yet.

## Consequences
- Future secret-card games can depend on `mental-card-core` directly.
- Core APIs must not contain Hold'em vocabulary.
- Public documentation must explain which invariants belong to core and which belong to the game crate.
