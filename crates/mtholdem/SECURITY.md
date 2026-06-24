# Security policy and threat model

## Status

The `mtholdem` 0.3.0 workspace is experimental research software. It has not received an independent
cryptographic audit, a production penetration test, or a machine-checked proof. Do not treat a
successful build or test run as evidence that it is safe for real-money play.

## Intended adversary model

`mtholdem` targets a statically chosen, authenticated roster of 2 to 10 mutually distrustful
players. The lower-level `mental-card-core` representation can encode a larger roster, but that does
not imply practical performance or a security analysis for such group sizes. A malicious player may send arbitrary bytes, reorder its own actions, equivocate, or stop
responding. The intended confidentiality goal is that any proper subset of players does not learn
unopened cards, provided at least one honest player contributes an unpredictable shuffle.

This implementation deliberately uses `(n, n)` threshold decryption. Every roster member is
needed to reveal a card. Consequently, cryptography alone does not guarantee liveness. Liveness
and economic fairness require an external mediator or stateful contract that can enforce deadlines,
validate evidence, and distribute collateral.

The protocol is not designed for adaptive corruption, compromised endpoints, malicious random
number generators, side-channel attackers, traffic-analysis resistance, denial-of-service
resistance, or a hostile settlement implementation.

## Critical deployment assumptions

1. **CSPRNG:** Identity keys, per-hand mental-poker keys, shuffles, and zero-knowledge proof
   randomness must come from an operating-system-backed cryptographically secure RNG.
2. **Authenticated global order:** Public protocol events must be delivered with a single agreed
   order. Every peer must independently verify signatures and append through `PublicTranscript`.
3. **Confidential private channels:** `SignedPrivateReveal` messages for hole cards must be sent
   through authenticated confidential channels. A public relay may carry ciphertext, but it must
   not learn plaintext share messages.
4. **Verified receipt path:** Publish a `HoleCardsReceived` digest only after
   `HandCryptography::open_private_hand` has verified every signature and Chaum--Pedersen proof and
   reconstructed both cards. Do not invent or independently compute a receipt from unchecked bytes.
5. **Bound showdown path:** Accept an opening only through
   `HandCryptography::open_showdown_bundle`, using the exact bundle digest from the signed
   `ShowdownOpen` event and the exact receipt digest from the owner's earlier signed
   `HoleCardsReceived` event or a verified checkpoint.
6. **Unique contexts:** Never reuse a `SessionId` for a logically different roster or economic
   agreement. Do not reuse per-hand mental-poker secret keys across hands.
7. **Durable anti-rollback state:** Persist the last accepted transcript anchor and certified
   checkpoint atomically. After restart, reject an older state even if it has valid signatures.
8. **Session key state:** Persist the verified per-seat mental-poker public keys and aggregate key
   alongside any checkpoint that contains a deck. The checkpoint binds the deck to their digest and
   count but intentionally does not duplicate the entire key-setup transcript.
9. **Secret retention:** A player must retain its per-hand threshold secret until the hand either
   completes or the mediator irrevocably terminates recovery. Folding does not release this duty.
10. **Evidence retention:** Keep the latest certified checkpoint, the current suffix, exact
    hole-card receipt digests, the underlying signed private reveals, and relevant direct messages
    until settlement is final.
11. **Chip model:** Checkpoint validation assumes an immutable roster with no rake, top-up, rebuy,
    or partial cash-out inside the session. Stacks plus current-hand contributions must always equal
    `buy_in * roster_size`. Start a new session for a membership or chip-supply change.
12. **Economic enforcement:** `EconomicSecurity` checks only sufficient arithmetic inequalities.
    It cannot ensure that collateral is escrowed, liquid, legally enforceable, or valuable enough.
13. **Mediator correctness:** The settlement layer must use exactly the same canonical bytes,
    signature rules, timeout policy, roster, config digest, and protocol transition checks.
14. **Endpoint integrity:** A peer that leaks its identity key, threshold key, hole cards, or RNG
    state loses the corresponding security guarantees.

## Known limitations

- No chain-specific smart contract or payment rail is included.
- The Rust implementation has no UC proof and does not inherit the papers' formal proofs by
  construction.
- Concurrent sessions are domain-separated, but arbitrary-composition security has not been
  established for this implementation.
- The Bayer--Groth implementation is derived from unaudited `ziffle` 0.1.0 and has been modified.
- Constant-time behavior and microarchitectural side channels have not been audited.
- Secure erasure is best effort; compiler, allocator, swap, crash dumps, and operating-system
  copies may retain secret material.
- Network metadata can reveal who is participating and when private-card messages are exchanged.
- Omission or timeout is not a self-contained signed fact. It must be established by a mediator's
  challenge and deadline. `EquivocationEvidence` only proves conflicting signed messages.
- A valid `ShowdownBundle::verify` call checks structure and identity signatures, not the
  Chaum--Pedersen proofs or earlier receipt commitment. Use `open_showdown_bundle` for the complete
  verification path.
- A contestable side pot needs at least one eligible opened hand. A host must not finalize a sequence
  of mucks that leaves a side pot without a possible winner.
- The immutable roster model does not re-key transparently after a player exits. Start a new
  session with the remaining players.
- Checkpoint wire version 2 is incompatible with version 1. Do not mix checkpoint artifacts across
  these versions.
- `mtholdem-testkit` uses predictable seeded randomness. Keep it in `dev-dependencies`; never use it
  to generate production identities, threshold keys, proof randomness, or shuffles.
- The public `mental_card_core::__private` module exists only to bridge certified deck restoration
  into the sibling Hold'em crate. Downstream applications must not call it; use
  `CertifiedCheckpoint::restore_verified_deck`.

## Reporting a vulnerability

Please report suspected vulnerabilities privately to the project owner or deployment operator.
Include the affected version, a minimal reproducer, the security property violated, and whether the
issue is believed to be exploitable remotely. Do not include live identity keys, player data, or
settlement credentials.

Until a private reporting address is established for a deployment, do not advertise this crate as
production-ready and do not accept real-value deposits.
