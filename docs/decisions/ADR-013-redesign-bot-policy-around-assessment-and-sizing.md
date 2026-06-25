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
   - Bounded Monte Carlo equity on flop and turn.
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

Use Monte Carlo as an equity estimator inside `HandAssessment`, not as a direct
action selector. Preflop should stay table-driven with position and player-count
adjustments. Flop and turn may run a bounded number of deterministic trials over
unknown cards and live opponents. River does not need Monte Carlo because the
opened board and known hand can be evaluated directly.

The estimator must be explicit about what information it uses. Peer-backed play
may use only the bot seat's known hole cards, public board cards, public action
context, and random unknown opponent cards. It must not inspect hidden cards
from other seats even when an in-process local runner technically holds them.
Rules-only play may share the same estimator for consistent behavior, but must
follow the same information boundary unless a deliberately named debug or test
policy says otherwise.

Monte Carlo work must be bounded and deterministic for reproducible tests and
acceptable CLI/Godot latency. Initial defaults should be small, for example a
few hundred trials on the flop and somewhat more on the turn, with table-runner
or profile-level tuning available later.

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

### Use only static heuristics for postflop strength
- Pros: Fast and easy to make deterministic.
- Cons: Draws, overcards, multiway equity, and board runouts are difficult to
  approximate well with hand-written rules alone.
- Rejected: Use lightweight bounded Monte Carlo for equity estimates, then let
  intent and sizing policy decide the action.

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
- The first rewrite exposes assessment, intent, and sizing stages instead of
  extending the min-raise heuristic.
- Table runners should expose richer context to bot decisions where available.
  The CLI rules-only backend now supplies pot, stack, contribution, and live
  opponent context; peer-backed host loops should supply equivalent public facts
  when the peer effect stream exposes them.
- Postflop assessment may later spend bounded CPU time on Monte Carlo equity,
  so profiles or runners should be able to tune trial counts then.
- CLI and future Godot single-player can surface bot debug information such as
  assessment, intent, and sizing without depending on protocol internals.
- Existing bot presets may keep their names, but their parameters should map to
  the new assessment/intent/sizing model.
- Tests should cover "no min-raise war" and "postflop bets are not fixed to the
  minimum" as observable behavior.
