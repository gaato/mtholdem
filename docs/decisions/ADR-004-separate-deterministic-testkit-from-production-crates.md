# ADR-004: Separate Deterministic Testkit from Production Crates

## Status
Accepted

## Date
2026-06-24

## Context
Downstream integration tests need deterministic identities, rosters, keys, shuffles, and fixture setup. Putting those helpers behind production features risks accidental use in real sessions, while `cfg(test)` helpers are not available to downstream crates.

## Decision
Provide deterministic fixture assembly in `mtholdem-testkit`. The testkit depends on `mtholdem`, uses public production APIs, and offers no proof bypass, fake proof, unchecked constructor, or secret-inspection hook.

## Alternatives Considered

### `cfg(test)` helpers inside production crates
- Pros: Keeps package count smaller.
- Cons: Not reusable by downstream integration tests.
- Rejected: It does not solve external validation needs.

### Production feature flag for deterministic helpers
- Pros: Easy to enable.
- Cons: Feature leakage can put predictable randomness into production builds.
- Rejected: Deterministic setup must be visible in the dependency graph.

## Consequences
- Applications use `mtholdem-testkit` only as a dev-dependency.
- Testkit fixtures validate that real public APIs are usable.
- Production crates remain free of seeded fixture shortcuts.
