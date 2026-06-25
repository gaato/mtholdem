# ADR-013: Redesign Bot Policy Around Assessment and Sizing

## Status
Accepted

## Date
2026-06-25

## Context
`mtholdem-bot` began as a lightweight host-side policy crate for CLI demos and
single-player shells. It currently chooses legal actions from a small context:
street, legal actions, hole cards, board cards, and coarse tendency values such
as looseness, aggression, call bias, bluff rate, and randomness.

That first policy is useful for exercising table loops, but play logs show it
is too crude as an opponent. The most visible failures are:

- raise sizing is effectively `minimum_raise_to`;
- postflop opening bets are effectively the table minimum;
- heads-up bots can min-raise each other repeatedly until a stack is exhausted;
- calls are too insensitive to pot odds, river hand category, and multiway
  pressure;
- the bot does not separate "should raise" from "what size should this raise
  be".

Incrementally patching these symptoms would produce a pile of special cases on
top of the wrong model. A usable local-play opponent needs a clearer policy
shape while staying lightweight and non-consensus.

## Decision
Redesign `mtholdem-bot` around explicit assessment, intent, and sizing stages
instead of extending the current min-raise heuristic.

The redesigned policy should keep `mtholdem-bot` host-side as decided in
ADR-010, but replace its internal decision pipeline:

1. Build a `HandAssessment`.
   - Preflop hand class and rank score.
   - Made-hand category from the evaluator when enough cards exist.
   - Draw potential and overcard value.
   - Board texture and multiway pressure.
   - Showdown value, especially on the river.
2. Choose an `ActionIntent`.
   - Fold, check, call, bet, raise, or all-in.
   - Re-raise thresholds should be stricter than open-bet thresholds.
   - 4-bet-or-later behavior should be premium-heavy with rare bluff combos.
3. Choose a `BetSizing`.
   - Min-click raises should not be the default.
   - Use pot fractions such as 33%, 50%, and 75% pot when enough context is
     available.
   - Use all-in only for short-stack, very strong, or high-pressure cases.

`BotDecisionContext` should grow to include the table facts required for this:

- pot size;
- effective stack;
- current bet or amount already committed this street;
- raise count for the current street, or equivalent aggression depth;
- player count and live opponent count;
- position-like information where the table runner can provide it.

The public bot API may break before the stable release. Do not preserve the
old min-raise policy for compatibility. If useful for tests, keep a deliberately
named fixture or deterministic test policy rather than presenting it as a real
opponent profile.

## Alternatives Considered

### Patch the current heuristic in place
- Pros: Small diff and immediate reduction in min-raise loops.
- Cons: The model still lacks pot, effective stack, raise depth, and separate
  sizing. More patches would accumulate around missing concepts.
- Rejected: Fix the root decision shape rather than layering exceptions.

### Build a solver-like bot
- Pros: Much stronger poker play.
- Cons: Too much complexity for the current CLI/Godot single-player need; hard
  to keep deterministic, inspectable, and lightweight.
- Rejected: The goal is a plausible local opponent, not optimal play.

### Put bot intelligence into the table runner
- Pros: Direct access to pot, stacks, button, and live seats.
- Cons: Mixes opponent policy with host orchestration and makes bot reuse from
  CLI, Godot, simulations, or future C ABI harder.
- Rejected: The table runner should supply context; `mtholdem-bot` should make
  decisions.

### Keep the current bot as the Balanced profile
- Pros: Existing behavior remains stable.
- Cons: The current Balanced profile min-raises and min-bets too often, making
  logs look broken and play feel misleading.
- Rejected: Pre-stable policy can break. The old behavior should not define
  the product's baseline opponent.

## Consequences
- The next bot implementation should be a rewrite of the policy pipeline, not
  a sequence of ad hoc min-raise suppressions.
- The table runner must expose richer context to bot decisions.
- CLI and future Godot single-player can surface bot debug information such as
  assessment, intent, and sizing without depending on protocol internals.
- Existing bot presets may keep their names, but their parameters should map to
  the new assessment/intent/sizing model.
- Tests should cover "no min-raise war" and "postflop bets are not fixed to the
  minimum" as observable behavior.
