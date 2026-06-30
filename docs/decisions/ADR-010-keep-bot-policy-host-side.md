# ADR-010: Keep Bot Policy Host-Side

## Status
Accepted

## Date
2026-06-25

## Context
The workspace now has a lightweight `mtholdem-bot` crate and a CLI
`play-bot` demo. The next natural feature is single-player play: one human
seat, several automated opponents, and a simple command-line or Godot UI loop.

This creates a boundary question. Bot behavior is useful across hosts, but it
is not part of dealerless mental-poker consensus. A bot chooses local betting
actions from a legal-action context; it does not verify transcripts, decide
canonical ordering, parse wire artifacts, or protect a local single-player app
from tampering.

The cryptographic protocol still matters for multiplayer and for exercising the
same peer/session machinery in demos. It does not make a purely local
single-player process cheat-proof. Local app anti-cheat, save-file integrity,
and UI disclosure rules belong to the application that owns the runtime.

## Decision
Keep `mtholdem-bot` as a host-side Rust policy crate. It may expose profiles,
presets, and deterministic action-selection helpers over `mtholdem` rule
types such as `LegalActions`, `Street`, cards, seats, and chips.

`mtholdem-bot` must not:

- enter the consensus dependency path;
- drive `PeerSession`;
- parse or emit wire artifacts;
- validate transcripts, checkpoints, recovery bundles, or settlement state;
- own terminal, Godot, network, storage, or timer I/O;
- expose a C ABI surface before a concrete downstream wrapper proves the need.

Human interaction belongs in host shells. The CLI `play-bot` command defaults
to a human `seat0` and keeps the in-process peer loop in `mtholdem-cli`: it
displays the local hand, board, call amount, raise range, and legal actions,
then reads an action from stdin. The default human mode starts new independent
practice hands until the player enters `q` or `quit`; `--hands N` bounds the
session. `--human seatN` selects another human seat. `--bots-only` keeps the
fully automated demo and remains bounded to one hand by default, with
`--hands N` available for longer bot runs. Non-human seats continue to answer
`HostEffect::RequestAction` through `mtholdem-bot`.

The CLI should print public board updates whenever they open, regardless of
whether the human seat is still live, has folded, or the command is running in
`--bots-only` mode. Hand summaries may print private cards only when the
protocol has publicly disclosed them through showdown. Folded or mucked bot
hole cards remain hidden.

Godot single-player should follow the same split. If Godot uses the Rust bot
policy, it should normally do so through a higher-level table-runner boundary
that owns local bot seats and emits table events. Godot should not need to
construct raw bot decision contexts for ordinary single-player play. The
protocol crates remain unaware of single-player presentation.

## Alternatives Considered

### Put bot logic inside `mtholdem`
- Pros: Direct access to rule internals and fewer crates.
- Cons: Bot tendencies would become visible to the protocol crate and blur the
  line between legal-action validation and local action preference.
- Rejected: The rules crate should stay deterministic and policy-neutral.

### Put bot logic inside `mtholdem-peer`
- Pros: A peer session could auto-answer action requests directly.
- Cons: `PeerSession` would become harder to use for real players, tests, and
  hosts that need explicit control over user prompts or remote input.
- Rejected: Peer orchestration should emit effects; hosts decide how to answer
  them.

### Expose bot helpers through the C ABI now
- Pros: Godot and other native wrappers could reuse the Rust bot immediately.
- Cons: The ABI would gain tuning/profile/action-policy surface before any
  downstream wrapper has shown the exact shape it needs.
- Rejected: Keep the C ABI focused on session progress and artifacts until a
  concrete host proves a stable bot boundary.

### Implement all bot behavior in each host
- Pros: Maximum UI-specific freedom.
- Cons: CLI and Godot would duplicate poker-ish heuristics, presets, and
  tuning vocabulary.
- Rejected: A small Rust policy crate is useful shared behavior as long as it
  stays outside protocol consensus.

### Treat local single-player cryptography as anti-cheat
- Pros: Reuses the mental-poker story for every mode.
- Cons: A user controlling the local process can inspect memory, patch code, or
  bypass UI restrictions. The cryptography does not create a trusted runtime.
- Rejected: Local anti-cheat is an application concern, not a protocol
  guarantee.

## Consequences
- `mtholdem-bot` can evolve quickly before the stable release without wire or
  ABI compatibility cost.
- CLI bot demos and future human-vs-bot modes can share the same peer/session
  loop while keeping human input in the CLI.
- CLI human mode is intentionally a practice loop over independent hands for
  now; stack carry-over, button rotation, and busted-seat handling require a
  separate session-level design.
- Godot single-player remains a host application feature, not a protocol crate
  feature.
- Bot tuning vocabulary such as looseness, aggression, call bias, bluff rate,
  and randomness is explicitly non-consensus policy.
- If a downstream host later needs raw Rust-owned bot decisions over C, add a
  new ADR for a direct `mth_bot_*` surface instead of extending the existing
  peer or table ABIs opportunistically.
