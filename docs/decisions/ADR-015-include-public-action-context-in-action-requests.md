# ADR-015: Include Public Action Context in Action Requests

## Status
Accepted

## Date
2026-06-26

## Context
`mtholdem-bot` now separates hand assessment, action intent, and bet sizing.
That policy needs public table facts such as pot size, live opponents,
effective stack, and prior street contribution to avoid minimum-size bets and
raises. The CLI rules-only backend can derive those facts directly from
`HoldemHand::ledger`, but the peer-backed backend currently answers
`HostEffect::RequestAction` with only the actor and `LegalActions`.

This makes the two local-play backends diverge. Rules-only bots can use richer
context, while peer-backed bots and future C/Godot hosts see only legal ranges,
hole cards, and board cards. The peer path should remain the reference path for
local play because it exercises P2P artifacts, so it needs the same public
decision context without leaking hidden cards or moving bot policy into the
protocol layer.

The same gap affects external hosts. A Godot wrapper, custom UI, or C client
can display better prompts and run better local bots if action requests carry
public table context. That context is not consensus data; it is derived from
the deterministic rules ledger already maintained by `mtholdem`.

## Decision
Expose a public action-context value alongside legal actions at the action
request boundary.

The context should be produced by `mtholdem` from the public betting ledger and
carried through `mtholdem-peer::HostEffect::RequestAction`. It may include:

- current pot size;
- actor stack;
- actor contribution on the current street;
- effective stack against live opponents;
- number of live opponents;
- number of opponents who have matched the current bet or blind level;
- big blind.

`mtholdem-bot` should consume this context through `BotDecisionContext`. The
bot may use it for limper-aware preflop isolation sizing, pot-relative postflop
sizing, pressure assessment, and future bounded Monte Carlo tuning. The bot
must still use only the acting seat's known hole cards, public board cards,
legal actions, and public action context.

The peer effect remains low-level and transport-neutral. It does not choose a
bot action, advise a human, expose hidden opponent cards, or become a table
runner. It only gives the host enough public facts to answer the action request
well.

Foreign boundaries should preserve the same split:

- `mth_peer_*` request-action effects expose legal actions plus public action
  context for one peer seat;
- future `mth_table_*` events may present the same context as part of a
  higher-level local table prompt;
- Godot converts the portable C fields into engine values outside this
  workspace.

Because the workspace is pre-stable, the Rust effect and adapter/FFI shapes may
break now. Do not add compatibility shims for the old context-free action
request.

## Alternatives Considered

### Keep context only in CLI rules-only code
- Pros: Smallest change and no public API churn.
- Cons: Peer-backed local play remains worse than rules-only play, and external
  hosts cannot build natural bot or prompt behavior from the peer ABI.
- Rejected: The peer-backed path should be the reference path, not the least
  informed backend.

### Let each host reconstruct context
- Pros: Keeps `HostEffect::RequestAction` small.
- Cons: Hosts would need duplicated ledger interpretation, and foreign hosts
  would need extra APIs to reconstruct values the rules engine already knows.
- Rejected: The rules crate should derive the public facts once and expose them
  as typed data.

### Put bot sizing policy in `mtholdem-peer`
- Pros: Peer-backed bot behavior could improve without adding context fields.
- Cons: Mixes host policy with protocol orchestration and prevents Godot,
  simulations, and custom bots from reusing the same public context.
- Rejected: ADR-010 keeps bot policy host-side.

### Wait for `mth_table_*` before exposing context
- Pros: Avoids changing the low-level peer ABI now.
- Cons: `mth_peer_*` is also the custom participant boundary, and action
  prompts need this context even without a local table runner.
- Rejected: Public action context belongs at both peer and future table
  boundaries.

## Consequences
- Peer-backed and rules-only local play can feed the same public facts to
  `mtholdem-bot`.
- Bot preflop sizing can account for limpers and blinds without inspecting
  hidden opponent cards.
- C ABI and Godot wrappers can show richer prompts and support local bots
  without owning betting-ledger reconstruction.
- `HostEffect::RequestAction`, adapter effect encoding, and C header/getters
  will need tests whenever the context shape changes.
- The context is derived display and policy input, not new consensus data; the
  transcript and settlement semantics remain unchanged.
