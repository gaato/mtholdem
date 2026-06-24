# ADR-005: Keep Public-State Card Games as Sibling Protocols

## Status
Accepted

## Date
2026-06-24

## Context
Some card-game protocols, such as [21 - Bringing Down the Complexity][paper-21] for games like Blackjack or Baccarat, do not need encrypted private cards. They use different assumptions and lighter mechanisms than Hold'em-style secret-card protocols.

## Decision
Do not model public-state games as a feature of `mental-card-core`. If implemented, place them in a sibling crate, for example `mental-public-card`, and share only smaller primitives such as identity or strong IDs if real reuse emerges.

## Alternatives Considered

### Feature-gate public-state protocols inside `mental-card-core`
- Pros: One crate name for all card protocols.
- Cons: Mixes incompatible security assumptions and state machines.
- Rejected: Callers could select the wrong protocol family for secret-card games.

### Generalize `VerifiedDeck` to all hidden values
- Pros: Broad abstraction.
- Cons: Makes encrypted shuffled decks and public random draws look equivalent.
- Rejected: The proof obligations are materially different.

## Consequences
- `mental-card-core` remains scoped to standard 52-card secret-state protocols.
- Hold'em must never silently fall back to a public-state draw mechanism.
- Shared primitives should be extracted only after concrete sibling crates exist.

[paper-21]: https://eprint.iacr.org/2018/303.pdf
