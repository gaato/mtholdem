# Security Hardening Notes

These notes summarize security-sensitive checks that the Rust implementation keeps explicit. They
are not a formal proof, audit report, or production readiness statement.

## Scope

The implementation is compared against the concrete protocol obligations described by
[Kaleidoscope][paper-kaleidoscope] and [ROYALE][paper-royale]. The main trust-boundary risks are
rogue or mismatched keys, proof substitution, replay across positions or decks, showdown-share
replacement, malformed certified state, betting edge cases, decoder robustness, and recovery
attribution.

The result is a hardened research prototype, not a proof that the implementation realizes either
paper's ideal functionality. The external stateful mediator, transport, persistence, endpoint
security, and deployment economics remain outside this crate.

## Main findings and changes

### 1. Verified encrypted decks were not explicitly bound to their threshold key

A shuffle proof is meaningful only relative to the aggregate public key under which the encrypted
deck was formed. A typed `VerifiedDeck` previously represented proof verification but did not carry
a first-class commitment to the aggregate-key membership. That made accidental cross-key use and
unsafe checkpoint restoration easier to express.

`AggregateMentalKey` now has a stable digest over its sorted, exact member keys and participant
count. Every `VerifiedDeck` carries that digest and count. Subsequent shuffles, reveals, and
checkpoint restoration reject a different aggregate key, including a different key set of the same
size.

### 2. Verified reveal shares could be reused by API shape outside their original card

The Chaum--Pedersen proof verifies a share for one ciphertext and proof context. The Rust wrapper now
preserves that fact in the type itself: `VerifiedRevealShare` stores the exact verified deck digest
and card position. `reveal_card` rejects a share verified for another position or another independently
shuffled deck, even under the same aggregate key.

### 3. Hole-card checkpoints recorded delivery presence, not the exact acknowledged artifacts

A presence bit is insufficient to prove that the shares disclosed at showdown are the same shares
accepted before betting. The checkpoint format now stores one optional exact receipt digest for each
roster seat. Recording is idempotent for the same digest and rejects replacement by a different
digest.

The receipt commits to exactly two positions and exactly one signed private reveal from every roster
seat for each position. It covers the complete canonical signed artifacts, including randomized
proof bytes, rather than low-entropy card values.

### 4. Showdown needed an end-to-end commitment check

A structurally valid `ShowdownBundle` can contain a newly generated, proof-valid set of shares for
the same cards. That is not enough: the protocol must prove that the disclosed set is the one the
owner acknowledged before betting.

`HandCryptography::open_showdown_bundle` now requires:

1. the digest committed by the owner's signed `ShowdownOpen` public event; and
2. the digest committed by the owner's earlier signed `HoleCardsReceived` event or certified
   checkpoint.

It verifies the bundle, all nested signatures and DLEQ proofs, exact hand/deck/position contexts,
card reconstruction, and equality with the earlier receipt. A regression test generates a second
fully valid share set and confirms that substitution is rejected.

The lower-level receipt helper is crate-private so application code cannot accidentally publish a
receipt before cryptographic verification. `open_private_hand` is the supported receipt-producing
path.

### 5. Certified checkpoint validation needed stronger semantic invariants

A unanimous signature does not make malformed state safe if honest clients are willing to sign it.
Checkpoint state is now revalidated at capture, vote, assembly, verification, and restoration.
Version 2 checks include:

- exact session/config identity and roster-sized canonical seat order;
- configured big blind;
- street contribution not exceeding total contribution;
- per-seat maximum-hand-loss cap;
- consistency of `Playing`, `AllIn`, `Folded`, and `NotDealt` with stack/contribution state;
- full session chip conservation:
  `sum(stack + total_contributed) == buy_in * roster_size`;
- board position/card uniqueness;
- canonical encrypted-deck encoding;
- nonzero aggregate-key commitment and exact participant count;
- exact receipt-vector size and nonzero stored receipt digests; and
- expected actor within the roster and eligible to act.

Deck restoration verifies the certificate internally and requires the exact current
`SessionConfig` and `AggregateMentalKey`.

### 6. No-limit betting and side-pot edge cases

The rules engine was hardened in three places:

- A sole player still able to act cannot raise when every opponent is already all-in; the player may
  only satisfy an outstanding call or fold.
- A later street with one actionable player who is already matched completes automatically rather
  than demanding a meaningless check.
- A contribution tier funded by only one player is returned as unmatched excess even if that hand
  later mucks. It is not treated as a contestable side pot requiring an eligible revealed hand.

Property tests generate randomized ledgers and verify that side-pot payouts conserve the full pot
and never award contestable chips to folded seats.

### 7. Recovery and untrusted-byte robustness

`EquivocationEvidence` now has explicit round-trip and attribution coverage. Bounded random-input
tests exercise all public cryptographic, wire, checkpoint, and recovery decoders and assert that
malformed inputs return errors rather than panic.

## API and wire effects

- Checkpoint state/certificate format is version 2 and rejects version 1.
- Public messages, private reveals, showdown bundles, recovery bundles, and equivocation evidence
  retain version 1.
- `MentalPoker::shuffle` returns `Result` so key mismatch is represented as a checked failure.
- `CertifiedCheckpoint::restore_verified_deck` requires `SessionConfig` and `AggregateMentalKey`.
- Applications must use `open_private_hand` to obtain a receipt and
  `open_showdown_bundle` for complete showdown verification.

## Validation performed

The release validation covers formatting, Clippy with warnings denied, locked dependency resolution,
debug and release tests for all targets/features, Rustdoc tests and warning-free documentation, the
complete three-player simulation, package verification, and compilation from Cargo's extracted
package tree. See `VALIDATION.md` for the exact commands and observed results.

## Residual risks

The following remain outside the assurance provided by this review:

- no independent cryptographic audit or formal implementation proof;
- no UC claim for the Rust code;
- no chain-specific contract, timeout oracle, payment rail, or collateral custody;
- no adaptive-corruption, side-channel, traffic-analysis, or denial-of-service guarantee;
- best-effort rather than formally verified secret erasure;
- an unaudited, modified Bayer--Groth implementation derived from `ziffle`;
- required persistence of verified key setup state alongside checkpoints; and
- operational rules for legal compliance, fees, rake, rebuys, cash-outs, and membership changes.

A deployment handling value should commission independent review of the complete system and create
cross-implementation test vectors for every artifact and mediator transition before accepting funds.

[paper-kaleidoscope]: https://eprint.iacr.org/2017/899
[paper-royale]: https://eprint.iacr.org/2018/157
