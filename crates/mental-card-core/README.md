# mental-card-core

Game-neutral cryptographic building blocks for dealerless protocols over one standard 52-card deck.

The crate owns:

- immutable identity rosters and compact strong identifiers;
- stable standard-card encoding;
- per-deal threshold-ElGamal key shares and proof of possession;
- verified sequential Bayer--Groth shuffles;
- public or recipient-bound Chaum--Pedersen reveal shares; and
- `CardCryptography`, the common key/shuffle/reveal state machine.

It intentionally does **not** own betting, Hold'em streets, payout rules, signed game messages,
checkpoints, recovery policy, networking, storage, or settlement. A game crate supplies an opaque
canonical configuration digest through `SessionBinding` and defines those semantics above the core.

The core uses `DealNumber`; a game may expose a domain-specific alias such as `HandNumber`.
Participant encoding supports more than a Hold'em table, while each game crate must enforce its own
practical limit.

This is experimental, unaudited research software. Read `SECURITY.md` and
`THIRD_PARTY_NOTICES.md` before integration. The derived shuffle implementation is not an
independently audited cryptographic library.
