# ADR-011: Keep Table Session Runner Host-Side

## Status
Accepted

## Date
2026-06-25

## Context
The CLI `play-bot` command now runs multiple practice hands, accepts human
input, prints public board updates after folds, and summarizes showdown and
final stacks. The next useful step is to carry table state across hands:
previous final stacks, button rotation, busted seats, bot seats, and human
seats.

That state is valuable for CLI play and future Godot single-player or local
multiplayer shells, but it is not the same boundary as a single
`mtholdem-peer::PeerSession`. A peer session owns one participant's
transport-neutral protocol progress for one hand number. It emits host effects
and validates peer-visible artifacts. It should not decide how a table UI
loops, which seats are bots, when a human quits, how stacks are carried into
the next hand, or how Godot dictionaries are shaped.

Godot integration also changes the pressure on this boundary. Godot should not
need to orchestrate several raw `PeerSession` values directly just to build a
single-player mode. It wants table-oriented events such as hole-card updates,
legal-action prompts, bot actions, board updates, showdown cards, and hand
settlement summaries. CLI wants the same information rendered as text.

## Decision
Keep multi-hand table/session running as host-side orchestration above
`mtholdem-peer`. Do not fold it into `mtholdem`, `mtholdem-peer`, or
`mtholdem-bot`.

The eventual shared runner should be a target-independent host-facing layer. It
may live first as CLI-internal code while the behavior is still experimental,
but if Godot or another host starts sharing the same needs, extract it into a
Rust crate above `mtholdem-peer` and `mtholdem-bot`, such as
`mtholdem-table` or `mtholdem-host`.

The runner should be designed so the table event model can support more than
one backend:

- a peer-backed backend using `mtholdem-peer`, `mtholdem-bot`, and the same
  artifact path as P2P multiplayer;
- a rules-only backend using `mtholdem` and `mtholdem-bot` for lightweight
  local play without cryptography, peer effects, or wire artifacts.

Do not implement both backends at the same maturity level now. The peer-backed
backend is the initial reference path because it exercises the protocol path
used by P2P multiplayer. The rules-only backend is a later optimization for
fast local play, Godot ergonomics, simulations, or AI iteration once the shared
table events have stabilized.

That runner may own:

- multiple in-process `PeerSession` values for local practice or tests;
- hand-to-hand table state such as stacks, button position, seat liveness, and
  the next hand number;
- bot-seat dispatch through `mtholdem-bot`;
- human-seat prompts as abstract host events, not terminal or Godot I/O;
- table-oriented event summaries for CLI, C ABI, Godot, WASM, or tests;
- a backend choice that preserves the same host-facing event vocabulary across
  peer-backed and rules-only execution.

The runner must not:

- become part of transcript consensus;
- bypass `PeerSession` validation or submit forged artifacts;
- inspect hidden private cards except through existing local-seat and showdown
  APIs;
- own sockets, timers, storage, terminal I/O, Godot SDK types, or engine
  lifecycle;
- silently substitute deterministic, mock, or locally generated data for
  production capabilities;
- hide which backend is in use from the caller. Peer-backed and rules-only
  execution have different security and verification properties and must be
  selected explicitly.

The C ABI should continue to expose the low-level peer/session boundary until a
real downstream host proves the table-runner event shape. If exported later,
the C ABI should expose table events as a separate host-runner surface rather
than overloading the existing peer-session handles.

## Alternatives Considered

### Put table running inside `mtholdem`
- Pros: Direct access to rule state and final stacks.
- Cons: Pulls UI/session-loop policy into the rule and artifact-verification
  crate.
- Rejected: `mtholdem` should stay the Hold'em rule and protocol artifact
  crate.

### Put table running inside `mtholdem-peer`
- Pros: Reuses peer progress and host-effect machinery directly.
- Cons: `PeerSession` would start owning multiple seats, bot decisions, human
  prompts, stack carry-over, and local practice semantics.
- Rejected: `PeerSession` should remain a single-peer Sans-I/O driver that
  emits effects.

### Put table running inside `mtholdem-bot`
- Pros: Single-player play often needs bots.
- Cons: Local multiplayer, replay viewers, and tests need the same runner
  without bot policy. The bot crate would become broader than action
  selection.
- Rejected: ADR-010 keeps `mtholdem-bot` as host-side action policy only.

### Make each host implement its own table runner
- Pros: Maximum freedom for CLI, Godot, and WASM.
- Cons: Repeats subtle sequencing around `PeerSession`, showdown disclosure,
  final-stack carry-over, and public board visibility.
- Rejected: Hosts should own presentation, but shared table orchestration is
  likely reusable once at least two hosts need it.

### Export a Godot-shaped high-level ABI now
- Pros: Godot single-player could become easier immediately.
- Cons: The current Rust workspace has not proven the table event model, and
  Godot-specific collection shapes would leak into portable ABI design.
- Rejected: Keep Godot outside the workspace and wait for a concrete runner
  contract before adding a C ABI surface.

### Build only a rules-only local runner first
- Pros: Simpler and faster for single-player UI iteration.
- Cons: The CLI and Godot practice modes would stop exercising the same
  peer/session machinery as P2P multiplayer, and later peer-backed integration
  could discover mismatched event assumptions.
- Rejected: Rules-only is useful, but it should follow the peer-backed event
  model rather than define it.

### Require peer-backed execution for every local mode
- Pros: One implementation path and stronger regression coverage of P2P
  artifacts.
- Cons: Purely local bot games, simulations, and UI prototypes pay the cost and
  complexity of cryptography and peer orchestration even when they do not need
  those guarantees.
- Rejected: Keep peer-backed as the reference path, but allow a future
  explicitly selected rules-only backend.

## Consequences
- The current CLI `play-bot` runner can keep evolving quickly while it proves
  the table behavior.
- Stack carry-over, button rotation, and busted-seat handling should be
  implemented as table-runner behavior, not as `PeerSession` behavior.
- P2P multiplayer and peer-backed single-player share `mtholdem-peer`.
  Rules-only single-player may later use only `mtholdem` plus `mtholdem-bot`,
  but it must present the same table event vocabulary if it is exposed through
  the shared runner.
- Godot single-player should consume table events if a shared runner is
  exported, while still keeping Godot classes and GDExtension build files in
  the downstream Godot repository.
- `mtholdem-bot` remains reusable by the runner but does not own the runner.
- A future extraction adds a new dependency branch above `mtholdem-peer` rather
  than changing consensus crate dependencies.
