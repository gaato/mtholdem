# ADR-016: Build Bot as Range-Aware Monte Carlo Heuristic

## Status
Partially superseded by ADR-018

ADR-018 supersedes the diagnostic and public API framing around
`HandAssessment`, assessment, intent, and sizing. This ADR still governs the
choice to use bounded range-aware Monte Carlo, public-information sampling, and
aggregate telemetry rather than a solver.

## Date
2026-06-26

## Context
`mtholdem-bot` is intended for CLI practice, Godot single-player, simulations,
and local convenience modes. It is not a protocol participant type and does not
belong in consensus. ADR-013 already chose an assessment, intent, and sizing
pipeline and rejected a solver-like bot for the first usable opponent.

Surveying Texas Hold'em bot approaches reinforces that split. Lightweight game
bots often combine hand evaluation, equity estimation, pot odds, position,
range assumptions, and bet sizing. Strong research bots such as DeepStack,
Libratus, and Pluribus use self-play, abstraction, continual or nested
re-solving, depth-limited search, and learned or computed value functions. They
are important reference points, but they are far beyond the complexity needed
for a responsive local opponent in this workspace.

Human-oriented no-limit Hold'em strategy material points to the same practical
baseline from another direction: start with position-aware tight-aggressive
play, widen opening ranges toward the button, prefer raising to passive calling
when entering first, use pot odds and equity for continue decisions, reduce
continuation-bet frequency in multiway pots, and adjust value/bluff frequencies
to opponent tendencies. These are not solver guarantees, but they are the right
target for a local practice opponent that should feel recognizable to humans.

The practical gap in the current bot is narrower: postflop strength is still a
static made-hand category plus small draw heuristics. That makes one-pair and
draw-heavy spots too coarse. A pair on a paired board, top pair with strong
kicker, weak pair in a multiway pot, and strong draw can collapse into similar
scores even though their showdown equity differs substantially.

## Decision
Use a range-aware Monte Carlo heuristic as the next target for `mtholdem-bot`.
Do not build a CFR, subgame-solving, neural-network, or self-play solver inside
this workspace now.

The bot architecture should remain:

1. **Public context** from the host.
   - Actor hole cards, public board, legal actions, pot, stacks, live
     opponents, action depth, blind size, and similar public action context.
   - No hidden opponent cards, even in rules-only local play.
2. **Equity estimation** inside `HandAssessment`.
   - Preflop uses a hand table plus position/player-count adjustments rather
     than raw rollout equity.
   - Unopened preflop raise-first-in decisions should come from a
     position/player-count range skeleton, not from comparing raw showdown
     equity against the number of remaining opponents.
   - Pot odds and equity are primarily for facing-bet/facing-raise continue
     decisions and postflop pressure, not the main driver for opening an
     unopened pot.
   - Flop and turn may use bounded Monte Carlo trials over unknown cards,
     initially with broad random opponent ranges.
   - River uses deterministic evaluation because all public cards are known.
3. **Range model** as a lightweight input to sampling.
   - Start with simple range buckets such as random, limp/call, open-raise,
     call-raise, and aggressive postflop lines.
   - Use action history and public context to weight opponent samples.
   - Preflop range buckets should distinguish unopened RFI, limp/call,
     call-open, 3-bet, and blind-defense situations before they feed postflop
     range-aware equity.
   - Keep the model inspectable and deterministic under a supplied RNG.
4. **Intent and sizing policy** after assessment.
   - Equity does not directly choose actions.
   - Continue using separate fold/check/call/bet/raise/all-in intent and
     sizing stages.
   - Bet sizing should account for pot, stack depth, blind size, limpers, and
     multiway pressure.
   - Default unopened preflop sizes should be in the human-standard small-open
     range, roughly 2 to 2.5 big blinds at ordinary stack depths, with limper
     isolation and short-stack exceptions handled explicitly.

The first implementation may expose a synchronous decision API. A simulation
bot may spend a bounded number of trials before returning a `BotDecision`.
Async, threaded, or incremental polling APIs are not required until a host needs
frame-budgeted calculation.

Diagnostics should expose enough per-action data for CLI and Godot debug views
to explain bot behavior: estimated equity, made-hand category, draw potential,
sampled opponent count/range class, pressure, intent, sizing family, and final
legal action.

Preset tuning must also be driven by aggregate telemetry rather than single log
read-throughs. A 31-hand transcript can suggest a problem, but it is not enough
to calibrate a bot profile. Before changing `Balanced` or other preset
thresholds, run bot-only simulations over hundreds or thousands of hands and
inspect at least VPIP, PFR, RFI, 3-bet rate, button steal rate, blind defense,
street bet rates, showdown rate, and win rate by seat. Preset names are labels
for intended tendencies; they should be validated against these observable
statistics instead of assumed from their names.

The CLI now exposes the first calibration surface with `play-bot --stats`.
That summary is intentionally a red-signal tool, not a solver objective: it
should catch broken behavior such as near-zero PFR, no RFI, no blind defense, or
fold-to-open near 100%, while leaving final profile tuning to broader manual and
simulation review.

For the initial `Balanced` target, use human baseline ranges as guardrails
rather than exact promises. At an eight-player, roughly 50 big blind table, a
reasonable first target is about 18-26% VPIP, 12-20% PFR, 30-45% button steal,
and 4-8% 3-bet, with wider ranges as the table becomes short-handed. If
telemetry shows PFR near zero, no blind defense, or repeated big-blind walks,
fix the preflop model before further postflop tuning.

## Alternatives Considered

### Keep only static heuristics
- Pros: Fast, deterministic, and simple to tune.
- Cons: Coarse categories miss equity differences in common postflop spots and
  make bot behavior feel arbitrary.
- Rejected: Keep heuristics, but feed them with bounded equity estimates.

### Use pure Monte Carlo against random hands
- Pros: Easy to implement and much better than category-only scoring.
- Cons: Ignores action history. Limp/call, raise, and multiway lines should not
  sample the same opponent distribution forever.
- Rejected: Start broad if needed, but design the estimator around range-aware
  sampling from the beginning.

### Use raw preflop equity thresholds for opening decisions
- Pros: Simple and reuses the same equity estimator across streets.
- Cons: Early-position and multiway spots are suppressed by the number of
  remaining opponents, causing excessive folds, too few raises, weak blind
  defense, and repeated walks. This is also not how human preflop opening
  ranges are normally expressed.
- Rejected: Use position/player-count/action-state range skeletons for
  preflop, then use pot odds and range-aware equity for continue and postflop
  decisions.

### Build a solver-like bot
- Pros: Better theoretical target and closer to strong poker AI research.
- Cons: Too much complexity for local practice, Godot single-player, and the
  current pre-stable library. It would add training, abstraction, large state,
  and exploitability questions that are outside this product slice.
- Rejected: Solver work belongs in a separate research agent or downstream
  participant, not the default workspace bot.

### Use an LLM as the bot policy
- Pros: Easy to make verbose and human-like in explanations.
- Cons: Hard to make deterministic, fast, testable, private-information safe,
  and available through C/Godot without network/runtime assumptions.
- Rejected: LLMs may review logs or provide commentary outside the protocol,
  but they should not be the core local bot policy.

## Consequences
- `HandAssessment` should gain an explicit equity field or equivalent source
  marker so callers can distinguish made-hand heuristic strength from simulated
  equity.
- `mtholdem-bot` should grow an estimator module that samples only unknown
  cards and public range assumptions.
- Tests must prove the estimator does not inspect hidden opponent cards in
  rules-only play.
- Trial counts, RNG seeds, and range assumptions must be configurable enough
  for deterministic tests and responsive CLI/Godot behavior.
- Public action context from ADR-015 becomes more important because simulation
  output is only useful when intent and sizing also see pot, stacks, blinds,
  live opponents, and action history.
- CLI and future host shells should grow a bot telemetry mode before further
  preset tuning. A likely shape is a bot-only simulation summary that reports
  VPIP, PFR, RFI, 3-bet, steal, blind-defense, street-bet, showdown, and
  seat-level win-rate metrics for each preset.
- Bot calibration should include human-strategy sanity tests: early-position
  ranges should be tighter than button ranges, unopened late-position steal
  attempts should exist, calling should respect pot odds and multiway pressure,
  and continuation betting should vary by board texture and opponent count.
- Preflop policy should be the next implementation focus when telemetry shows
  over-folding. Postflop Monte Carlo cannot make the opponent feel natural if
  most hands end before the flop.

## Implementation References
- eval7: https://github.com/julianandrews/pyeval7
  - Useful reference for PokerStove-style range parsing, weighted hand ranges,
    and hand-vs-range equity APIs.
- pbots_calc: https://github.com/mitpokerbots/pbots_calc
  - Useful reference for a compact ranged equity calculator interface.
- PokerStove: https://github.com/andrewprock/pokerstove
  - Useful reference for range-vs-range equity as a product concept and
    text-range vocabulary.
- PyPokerEngine: https://github.com/ishikota/PyPokerEngine
  - Useful reference for agent callbacks, valid-action prompts, round state,
    and emulator-style simulations.
- RLCard: https://rlcard.org/
  - Useful reference for separating environments, agents, and evaluation
    harnesses.
- OpenSpiel: https://github.com/google-deepmind/open_spiel
  - Useful reference for research-grade imperfect-information game APIs and
    algorithms, not as a direct dependency target.
- General Texas Hold'em strategy: https://en.wikipedia.org/wiki/Texas_hold_%27em#Strategy
  - Useful reference for the tight-aggressive and position-aware human baseline.
- General poker strategy: https://en.wikipedia.org/wiki/Poker_strategy
  - Useful reference for pot odds, gap/sandwich concepts, aggressive/passive
    play, range reading, and opponent profiling.
- Poker position: https://en.wikipedia.org/wiki/Position_%28poker%29
  - Useful reference for why late position widens playable ranges and why
    blinds are structurally difficult.

These projects are implementation references, not source-code sources. Before
copying code, run a separate license and provenance review. The initial
`mtholdem-bot` implementation should build its own small range and Monte Carlo
modules against the workspace's card, rules, and information-boundary types.

## Research References
- DeepStack: https://arxiv.org/abs/1701.01724
- Safe and nested subgame solving / Libratus: https://arxiv.org/abs/1705.02955
- DecisionHoldem: https://arxiv.org/abs/2201.11580
- RLCard: https://arxiv.org/abs/1910.04376
- OpenSpiel: https://arxiv.org/abs/1908.09453
- PokerStove overview: https://en.wikipedia.org/wiki/PokerStove
