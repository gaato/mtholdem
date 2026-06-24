# Wire format and transcript rules

## Goals

Wire artifacts are deterministic across peers, bounded before allocation, explicit about versioning,
and safe to hash or sign without a general-purpose serialization framework. The format is not
intended to be human-readable.

The 0.3.0 crate split does not alter signed bytes. Artifact versions are independent of Cargo package
versions.

## Common encoding rules

- Multi-byte integers are unsigned, fixed-width, and big-endian.
- Enums use explicit numeric tags; unknown tags are rejected.
- Variable byte strings are length-prefixed and checked against artifact-specific limits.
- Decoders reject trailing bytes.
- A magic prefix distinguishes artifact families before deeper parsing.
- Canonical encoders are the only bytes that may be signed or hashed.
- Unknown versions fail closed.

| Artifact | Magic | Version in 0.3.0 |
|---|---:|---:|
| signed public event | `MTHP` | 1 |
| signed private reveal | `MTHR` | 1 |
| showdown bundle | `MTHS` | 1 |
| checkpoint state | `MTHC` | 2 |
| checkpoint certificate | `MTHK` | 2 |
| recovery bundle / equivocation evidence | module-specific | 1 |

Checkpoint version 2 is incompatible with checkpoint version 1 because its signed meaning includes
exact hole-card receipt commitments and aggregate-key binding. Matching magic and version are never
sufficient by themselves; validation must use the current session, roster, configuration, hand, and
protocol state.

## Signed public message

A `SignedPublicMessage` commits to:

```text
wire version
session id
session-config digest
hand number
monotonic sequence number
hand phase
sender seat and PlayerId
previous public-event digest
typed payload
```

The sender is supplied as a validated `Participant`, not as an arbitrary seat number. Signing checks
that the identity secret owns that participant. Verification repeats the roster lookup.

The message digest covers the full canonical signed artifact. `PublicTranscript` requires
`sequence == next_sequence` and `previous_digest == current_anchor.digest`, verifies the signature
and declared structure, then advances to the new digest. Competing valid messages for one position
must be retained as equivocation evidence rather than resolved nondeterministically.

`HoleCardsReceived`, `ShowdownOpen`, and `Checkpoint` reject an all-zero commitment. A zero
`RecoveryRequested` checkpoint digest remains available as the explicit “no checkpoint yet” anchor.

## Private reveal

A `SignedPrivateReveal` is separate from the public transcript because broadcasting it may reveal a
hole card. It binds:

```text
session/config/hand
digest of the final verified encrypted deck
sender identity
recipient identity
one encrypted-deck position
one threshold decryption share
one Chaum--Pedersen proof
```

It must travel over an authenticated confidential channel. Signature verification alone does not
prove the DLEQ relation. The recipient should retain the original bytes but accept the share only
through `HandCryptography::verify_private_reveal` or, for both cards together,
`HandCryptography::open_private_hand`.

## Private-card receipt

A receipt is a SHA-256 commitment over the sorted, length-prefixed canonical bytes of exactly:

1. two distinct encrypted-deck positions; and
2. one signed reveal from every roster seat for each position.

It is not a hash of the card values. Hashing the low-entropy 52-card domain would permit cheap
guessing. The randomized proof bytes and signatures instead commit to the exact artifacts delivered
before betting.

The receipt-construction helper is crate-private. Applications obtain the digest from
`HandCryptography::open_private_hand`, after every signature and Chaum--Pedersen proof has been
verified and both cards have been reconstructed. Repeating the same receipt in a checkpoint is
idempotent; replacing it with another digest for the same seat is rejected.

## Showdown bundle

A `ShowdownBundle` contains the owner, two positions, and exactly one private reveal from every
roster seat for each position. `ShowdownBundle::verify` checks canonical structure, nested identity
signatures, common context, recipient, positions, and sender-position uniqueness. It does not by
itself verify the Chaum--Pedersen relations or prove that these are the shares acknowledged before
betting.

The complete acceptance path is:

1. obtain `expected_bundle_digest` from the owner's signed `PublicPayload::ShowdownOpen`;
2. obtain `expected_receipt_digest` from the owner's earlier signed
   `PublicPayload::HoleCardsReceived` or a verified checkpoint; and
3. call `HandCryptography::open_showdown_bundle`.

That method verifies the bundle digest, all nested signatures and proofs, exact deck/position
contexts, card reconstruction, and equality with the earlier receipt. A newly generated but otherwise
valid share set is therefore rejected.

## Checkpoints

Checkpoint state/certificate version 2 serializes public state only:

```text
session/config/hand/phase
public transcript anchor
optional poker ledger and expected actor
opened board positions and card values
optional canonical encrypted deck bytes
aggregate-key digest and participant count for that deck
one optional exact hole-card receipt digest per roster seat
application extension digest
one checkpoint vote per roster seat (certificate only)
```

Semantic verification against `SessionConfig` rechecks canonical seat order, the configured blind,
contribution caps, hand-status/stack consistency, board uniqueness, expected-actor eligibility,
canonical deck encoding, threshold membership count, nonzero key commitment, exact receipt-vector
size, and total chip conservation.

A certified deck can be restored without replaying old shuffle proofs only by passing both the
current `SessionConfig` and exact `AggregateMentalKey`. Persist the verified per-seat public keys and
aggregate key with the checkpoint; the checkpoint stores their commitment, not a duplicate key
transcript.

Old transcript material may be removed from the hot path only after the certificate is verified.
Archival retention remains recommended for audits and dispute forensics.

## Core cryptographic serialization

`mental-card-core` owns fixed-width serialization for keys, ciphertext decks, shuffle proofs, reveal
shares, and reveal proofs. These byte layouts remain the same as in `mtholdem` 0.2.0. They are not
standalone authenticated wire messages: the game crate must bind them into its session, hand, phase,
sender, target, and ordering context.

## Forward compatibility

Public enums are generally `#[non_exhaustive]`, but each current wire version rejects unknown tags.
A future feature that changes signed meaning must use a new tag or artifact version and must not
silently reinterpret old bytes. Deployments should negotiate all supported artifact and proof
versions before deposits are locked.
