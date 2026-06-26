# ADR-018: Reframe Bot Policy Around Classic Poker Motives

## Status
Accepted

## Date
2026-06-26

## Context
`mtholdem-bot` is a host-side policy crate for CLI practice, Godot
single-player, simulations, and local convenience modes. It should feel like a
recognizable casual no-limit Hold'em opponent, but it is not the repository's
protocol, fairness, or solver product.

ADR-013 replaced the original min-raise heuristic with assessment, intent, and
sizing stages. ADR-016 then chose a range-aware Monte Carlo heuristic instead
of a solver. That direction improved the first broken symptoms, but recent
watch/stat runs exposed a new problem: the internal vocabulary still pushes
future work toward patching scalar fields such as score, strength,
draw_potential, and pressure. Those fields are useful measurements, but they
are weak explanations for poker decisions.

This matters because a long list of local thresholds can make the bot larger
without making it more intelligible. A strong-draw raise tweak was backed out
after it added complexity without enough observable improvement. The next
rewrite should not preserve the current diagnostic names merely because they
exist.

Human poker strategy material is already organized around more stable
concepts: pot odds, implied odds, position, value betting, protection,
bluffing, semi-bluffing, slow play, gap and sandwich effects, hand reading,
ranges, table image, opponent profiling, loose/tight and aggressive/passive
tendencies, and the different reasons to raise, call, check, bet, or fold.
Those concepts are imperfect heuristics rather than solver guarantees, but they
are a better baseline for an inspectable local opponent.

The guiding principle should be the fundamental theorem of poker: the ideal
decision is the expected-value-maximizing action one would choose with complete
knowledge of an opponent's hidden cards and strategy. `mtholdem-bot` cannot and
must not inspect hidden opponent cards. It should approximate that ideal from
the information a real seat has: its own hole cards, public board cards, public
action context, legal actions, stack and pot state, range assumptions, and
bounded probability estimates.

## Decision
Reframe `mtholdem-bot` around the fundamental theorem of poker, then express
its approximation through classic poker situation and motive vocabulary. The
current assessment/intent/sizing shape from ADR-013 is superseded as the
long-term public and diagnostic model.

The bot may still compute numeric evidence such as equity, hand class, draw
outs, draw-completion probability, pot odds, effective odds, implied and
reverse implied odds, stack pressure, and range weights. Those numbers should
feed a named poker situation and a named betting motive instead of becoming the
public reason for the action.

The next bot design should prefer concepts like:

- `PokerSituation` or `SituationAssessment`, describing public action state,
  position, opponent count, pot size, stack depth, initiative, and pressure.
- `HoldingClass`, describing whether the made hand comes from private cards,
  board texture, weak pairs, top-pair kicker quality, overpairs, or stronger
  made hands.
- `DrawClass`, describing flush draws, straight draws, overcards, combo draws,
  and weak backdoor material.
- `PotOddsClass` or equivalent continue-price classification.
- `BettingMotive`, with variants such as value, protection, bluff, semi-bluff,
  pot-odds call, blind or playable-hand defend, showdown-value check/call, gap
  fold, pressure fold, free card, and give up.
- `SizingFamily`, if sizing needs a separate named output, using poker-facing
  terms such as small pot fraction, medium value size, protection size,
  isolation size, short-stack commit, and all-in.

Equity remains important, especially for facing-bet decisions and postflop
simulation. It is evidence, not the action selector. A bot should be able to
say "semi-bluff with strong draw and fold equity" or "call because price is
good" rather than exposing only "score above threshold".

Poker probabilities are separate from showdown equity. Draw-completion
probabilities and 5-card or 7-card hand frequencies are useful for draw
classification, pot-odds decisions, evaluator sanity checks, and Monte Carlo
distribution tests. They should not be confused with win probability against a
particular opponent range.

`BotDecisionContext`, profile presets, `choose_action`, public-information
boundaries, and bounded Monte Carlo remain useful. They should be retained or
renamed only as needed. Public diagnostic types such as `HandAssessment`,
`ActionIntent`, and `BetSizing` may be replaced before a stable release if
their names or fields obscure the motive model. Do not add compatibility shims
for these early bot APIs unless a later ADR accepts that cost.

Do not put CFR, subgame solving, genetic algorithms, reinforcement learning, or
large trained models inside `mtholdem-bot`. Those may be useful future research
or downstream participant projects, but this crate should stay a small,
deterministic, inspectable policy layer. A future lab or solver crate can
consume the same public situation and motive vocabulary without becoming the
default local opponent.

CLI watch output, statistics, and future Godot diagnostics should display the
bot's situation and motive categories instead of presenting generic scalar
assessment as the explanation. Aggregate telemetry still matters, but telemetry
is a red-signal and calibration surface; it should not force every statistic
directly into policy code.

## Alternatives Considered

### Keep assessment, intent, and sizing as-is
- Pros: Matches the current code and previous ADRs.
- Cons: The names make numeric thresholds look like the design center, which
  encourages patchy local fixes.
- Rejected: Keep the useful evidence, but replace the explanatory vocabulary.

### Continue adding special cases to the current policy
- Pros: Smaller individual diffs.
- Cons: The bot grows around symptoms. The resulting behavior is hard to
  inspect, tune, or explain to CLI/Godot users.
- Rejected: The pre-stable policy should be redesigned around clearer concepts.

### Build a solver or learning system now
- Pros: More ambitious poker strength target.
- Cons: Changes the repository's product shape, adds major runtime and
  training concerns, and would still need a public situation model for
  diagnostics and integration.
- Rejected: Keep `mtholdem-bot` modest. Solver or learning work belongs in a
  separate research surface if it happens.

### Remove bot diagnostics and only tune observable behavior
- Pros: Less public API and fewer names to get wrong.
- Cons: CLI/Godot practice and automated evaluation need explanations for
  strange actions. Without diagnostics, future tuning becomes transcript
  archaeology.
- Rejected: Diagnostics are useful; the diagnostic vocabulary should improve.

## Consequences
- ADR-013 is superseded for the public/internal framing of bot decisions, but
  remains historical context for rejecting the min-raise heuristic.
- ADR-016 remains valid for bounded range-aware Monte Carlo and telemetry, but
  its references to `HandAssessment` and assessment/intent/sizing are
  superseded by this motive model.
- Future bot implementation may delete or rename existing assessment structs
  and fields instead of preserving them.
- CLI watch and stats should evolve toward situation/motive output.
- Tests should assert observable behavior and, where diagnostics are public,
  named motives such as value, protection, semi-bluff, defend, pot-odds call,
  and pressure fold.
- Bounded Monte Carlo remains an input to equity and range evidence, not a
  solver or direct action oracle.
- Tests and diagnostics should preserve the incomplete-information boundary:
  even rules-only local play must not turn the fundamental theorem into a hidden
  card inspection shortcut.

## References
- Fundamental theorem of poker: https://en.wikipedia.org/wiki/Fundamental_theorem_of_poker
- General poker strategy: https://en.wikipedia.org/wiki/Poker_strategy
- Poker probability: https://en.wikipedia.org/wiki/Poker_probability
- Poker position: https://en.wikipedia.org/wiki/Position_%28poker%29
- General Texas Hold'em strategy: https://en.wikipedia.org/wiki/Texas_hold_%27em#Strategy
