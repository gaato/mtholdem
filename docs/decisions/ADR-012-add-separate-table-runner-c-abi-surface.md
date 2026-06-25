# ADR-012: Add Separate Table Runner C ABI Surface

## Status
Accepted

## Date
2026-06-25

## Context
The workspace now has two local play backends for `play-bot`: a peer-backed
path that exercises `mtholdem-peer`, and a rules-only path that uses
`mtholdem` plus `mtholdem-bot`. Both carry stacks across hands and expose a
table-oriented view: backend, button, starting stacks, board updates, action
requests, action summaries, showdown cards, and final stacks.

External hosts such as Godot, C++ tools, Unity native plugins, Python or Ruby
bindings, Zig wrappers, and downstream WASM packages should not have to drive
the CLI text loop. They need a stable event boundary that can be polled from a
game loop and converted into host-native collections.

The existing C ABI is lower-level. It exposes opaque peer-session handles,
progress/effect draining, public/private/showdown artifacts, stable integer
constants, ABI versioning, and official C headers. That surface is appropriate
for P2P multiplayer hosts that own transport and artifact routing. It is not a
good fit for a local single-player table runner.

## Decision
Add any future local table runner C ABI as a separate `mth_table_*` surface
instead of extending or overloading the existing `mth_peer_*` peer-session ABI.

The low-level and high-level boundaries are distinct:

- `mth_peer_*`: one peer, one hand, artifact routing, transport integration,
  transcript and recovery effects;
- `mth_table_*`: local table runner, explicit backend selection, bot seats,
  human action prompts, stack carry-over, button rotation, board events,
  showdown summaries, and final hand summaries.

The table ABI should follow the same C ABI discipline as ADR-009:

- opaque handles with explicit `*_free`;
- stable integer constants for event kinds, backend kinds, actions, streets,
  and error codes;
- ABI version and layout checks;
- caller-visible ownership for every allocated buffer;
- thread-local last-error reporting;
- no Rust struct layout exposure;
- no Godot SDK types, JSON dependency, callbacks, async runtime, sockets, or
  engine lifecycle hooks.

External hosts should poll events. A future C shape should look like this in
spirit, not necessarily exact spelling:

```c
mth_table_create(config, out_handle);
mth_table_free(handle);

mth_table_next_event(handle, out_event);
mth_table_submit_action(handle, seat, action);
mth_table_start_next_hand(handle);

mth_table_event_free(event);
```

Events should use a stable `kind` plus C-friendly fields and owned buffers
rather than a C union that is awkward for bindings. A table event may include
fields such as:

- backend kind;
- hand number;
- button;
- street;
- actor;
- legal action metadata such as `to_call`, minimum raise, and maximum raise;
- board cards;
- local or human hole cards when visible to that host;
- starting stacks;
- final stacks;
- showdown cards that have actually been opened;
- evaluated showdown hand categories, such as pair, straight, flush, or full
  house, computed only for opened hands.

Hand-category labels are derived display metadata, not new consensus data. The
source of truth remains the deterministic evaluator and the already-opened
cards. Table events and CLI output may include category names for usability,
but they must not evaluate or label folded, mucked, or otherwise undisclosed
hole cards.

The backend must be selected explicitly. `MTH_TABLE_BACKEND_PEER` and
`MTH_TABLE_BACKEND_RULES_ONLY` have different security and verification
properties and must not be hidden behind an automatic fallback. Peer-backed
local play exercises the P2P artifact path. Rules-only local play is lighter
and suitable for UI iteration, simulation, or purely local single-player.

Before adding `mth_table_*` to `mtholdem-ffi`, first extract a Rust table-event
boundary from the CLI runner. The CLI should consume the same Rust table events
that the C ABI later encodes. This prevents terminal wording and temporary CLI
state from becoming the ABI.

## Alternatives Considered

### Extend `mth_peer_*` with table-runner operations
- Pros: Fewer C entrypoint families.
- Cons: Blurs one-peer artifact routing with local table orchestration. P2P
  hosts and single-player hosts would see unrelated concepts on the same
  handle.
- Rejected: Peer and table runners have different responsibilities and
  lifecycles.

### Expose Godot-shaped C ABI structs
- Pros: Less conversion code in the first Godot wrapper.
- Cons: Godot collection shapes, engine lifecycle, and GDExtension conventions
  would leak into a portable C ABI used by C++, Zig, Python, Ruby, or WASM.
- Rejected: Godot remains a downstream wrapper that converts portable C events
  into Godot values.

### Use callbacks for table events
- Pros: Natural for some UI frameworks.
- Cons: Harder ownership, threading, and reentrancy rules across C, Godot,
  Unity, Python, and WASM.
- Rejected: Polling is simpler and safer for game loops and language bindings.

### Encode every table event as JSON
- Pros: Easy to inspect and bind.
- Cons: Adds parsing, allocation, schema drift, and numeric typing ambiguity at
  the ABI boundary.
- Rejected: Use stable integer constants and owned byte buffers. Hosts can
  convert to JSON or dictionaries at their own boundary.

### Add the C ABI before extracting Rust table events
- Pros: Gets Godot integration moving quickly.
- Cons: Freezes CLI-internal state and text-loop assumptions into a public ABI.
- Rejected: Prove the Rust `TableEvent` boundary first, then encode it in C.

## Consequences
- `mth_peer_*` remains the multiplayer and artifact-routing ABI.
- `mth_table_*` becomes the future local-play ABI for Godot and other external
  hosts.
- Godot single-player can use `mth_table_*` without orchestrating multiple raw
  `PeerSession` handles.
- The table ABI can expose both peer-backed and rules-only local play while
  preserving the same event vocabulary.
- A new Rust table-event boundary is the next implementation step before any C
  ABI entrypoints are added.
