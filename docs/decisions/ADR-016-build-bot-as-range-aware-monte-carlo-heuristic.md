# ADR-016: Build Bot as Range-Aware Monte Carlo Heuristic

## Status
Accepted

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
   - Flop and turn may use bounded Monte Carlo trials over unknown cards,
     initially with broad random opponent ranges.
   - River uses deterministic evaluation because all public cards are known.
3. **Range model** as a lightweight input to sampling.
   - Start with simple range buckets such as random, limp/call, open-raise,
     call-raise, and aggressive postflop lines.
   - Use action history and public context to weight opponent samples.
   - Keep the model inspectable and deterministic under a supplied RNG.
4. **Intent and sizing policy** after assessment.
   - Equity does not directly choose actions.
   - Continue using separate fold/check/call/bet/raise/all-in intent and
     sizing stages.
   - Bet sizing should account for pot, stack depth, blind size, limpers, and
     multiway pressure.

The first implementation may expose a synchronous decision API. A simulation
bot may spend a bounded number of trials before returning a `BotDecision`.
Async, threaded, or incremental polling APIs are not required until a host needs
frame-budgeted calculation.

Diagnostics should expose enough data for CLI and Godot debug views to explain
bot behavior: estimated equity, made-hand category, draw potential, sampled
opponent count/range class, pressure, intent, sizing family, and final legal
action.

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
