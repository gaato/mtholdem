# mtholdem

Sans-I/O protocol state, authenticated wire artifacts, recovery evidence, and deterministic
no-limit Texas Hold'em rules for dealerless P2P applications.

The sibling `mental-card-core` crate owns the game-neutral encrypted-card layer. Existing
`mtholdem::crypto`, `mtholdem::identity`, `mtholdem::error`, and `mtholdem::types` paths remain
available through the compatibility facade; new non-Hold'em game crates may depend on the core
directly.

`mtholdem` owns the semantics that are not honestly generic: blind and betting rules, two-card
private receipts, flop/turn/river planning, showdown, side pots, signed public ordering, compact
Hold'em checkpoints, and mediator evidence.

```bash
cargo test -p mtholdem --all-targets --all-features
cargo run -p mtholdem --example simulated_hand
```

This is experimental, unaudited research software. It is not a network stack, timer service,
settlement contract, payment rail, or production gambling system. See `SECURITY.md` and the files in
`docs/` before integrating it with assets of value.
