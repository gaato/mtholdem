# Security notes for mental-card-core

`mental-card-core` 0.3.0 is experimental and unaudited. It does not inherit a formal proof merely by
using constructions described in the mental-card literature.

Critical assumptions include a CSPRNG, immutable authenticated roster, unique session and deal
contexts, at least one honest unpredictable shuffle, confidential transport for private reveal
shares, retention of every threshold secret until the deal is irrevocably complete, and a separately
verified game state machine that decides when each card operation is legal.

The crate provides no liveness or fairness by itself. `(n, n)` decryption lets any participant stop
progress by withholding a share. A deployment needs a mediator or contract that can establish
timeouts and apply penalties.

Do not call the hidden `mental_card_core::__private` restoration hook from application code. It is a
sibling-crate bridge whose precondition is a fully verified unanimous game checkpoint.
