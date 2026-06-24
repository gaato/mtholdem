# Changelog

All notable changes to this project are documented here.

## 0.3.0 - 2026-06-24

### Workspace split

- Converted the single package into a three-crate Cargo workspace.
- Added `mental-card-core` for game-neutral standard-card identities, rosters, threshold key setup,
  verified shuffles, reveal shares, and per-deal cryptographic orchestration.
- Kept `mtholdem` as the Texas Hold'em facade and moved betting, deal planning, hand evaluation,
  signed wire artifacts, transcripts, checkpoints, and recovery evidence into that crate.
- Added `mtholdem-testkit` as a separate deterministic integration-fixture crate. It composes only
  public APIs and contains no proof or validation bypass.

### API and compatibility

- Added `SessionBinding`, which lets a game crate bind an opaque canonical configuration digest into
  every core proof context without exposing game rules to the cryptographic layer.
- Added game-neutral `CardCryptography` with explicit `RevealTarget::Public` and
  `RevealTarget::Private(Seat)` domains.
- Reimplemented `HandCryptography` as a Hold'em-specific wrapper over `CardCryptography`, retaining
  signed private receipts and showdown verification at the game layer.
- Preserved existing `mtholdem::crypto`, `mtholdem::identity`, `mtholdem::error`, and
  `mtholdem::types` paths through documented re-exports.
- Kept all existing signed artifact versions unchanged: checkpoint state/certificate remain version
  2 and the other existing wire families remain version 1.

### Design scope

- Deliberately did not genericize betting, wire, checkpoint, or recovery through an untyped game
  callback; their validity depends on the concrete game state machine and mediator verifier.
- Deliberately kept public-state protocols such as the 2018/303 commitment/VRF construction out of
  the encrypted-deck core. Such protocols should be implemented as sibling crates.
- Retained a standard 52-card deck instead of publishing speculative arbitrary-deck generics.

### Documentation and validation

- Added a crate-boundary decision record, updated architecture, wire, security, release, and
  validation documentation, and expanded module-level Rustdoc for local-context readers.
- Updated CI for workspace-wide formatting, Clippy, debug/release tests, Rustdoc, documentation, the
  complete Hold'em simulation, and package-content checks in dependency release order.

### Breaking changes

- The repository root is now a virtual workspace; the `mtholdem` package lives in
  `crates/mtholdem`.
- Direct imports of implementation files from the former root `src` tree must use either the
  `mtholdem` compatibility facade or the new `mental-card-core` crate.
- All workspace crates are version 0.3.0 and require Rust 1.85.

## 0.2.0 - 2026-06-23

### Security hardening

- Bound every `VerifiedDeck` to the exact aggregate threshold-key membership and participant count.
  Shuffling, revealing, and checkpoint restoration now reject a deck used with another aggregate key.
- Bound each verified reveal share to the exact verified deck digest and card position, preventing a
  valid share from being substituted across cards or independently shuffled decks.
- Replaced checkpoint hole-card receipt presence bits with the exact per-seat receipt digests.
- Added `HandCryptography::open_showdown_bundle`, which requires both the signed public
  `ShowdownOpen` commitment and the owner's earlier `HoleCardsReceived` commitment before opening
  the cards. A freshly generated but otherwise valid share set cannot replace the one acknowledged
  before betting.
- Tightened private-reveal receipt construction to require exactly two positions and exactly one
  validly signed share from every roster member for each position. The low-level receipt helper is no
  longer public; applications receive a receipt only after `open_private_hand` has verified all
  Chaum--Pedersen proofs and reconstructed both cards.
- Upgraded checkpoint state/certificate encoding to version 2. Checkpoints now carry the encrypted
  deck's aggregate-key commitment and threshold participant count, conserve all session chips, and
  retain exact per-seat private-delivery receipts.
- Rechecked semantic checkpoint invariants at capture, vote, assembly, verification, and restoration
  boundaries, including seat order, blind value, contribution caps, status/stack consistency, board
  uniqueness, expected-actor eligibility, deck encoding, key membership, and chip conservation.
- Fixed a showdown verification error that could accept a tautological comparison instead of
  checking the disclosed bundle against the previously committed receipt.
- Fixed no-limit betting edge cases: a sole actionable player may not raise over opponents who are
  already all-in, already-matched no-action streets complete automatically, and unmatched excess is
  returned to its sole contributor even when that hand is mucked.

### Validation

- Added adversarial tests for wrong aggregate keys, wrong deck positions, cross-deck share reuse,
  substituted showdown shares, tampered proofs, malformed checkpoints, and equivocation evidence.
- Added bounded random-input tests for cryptographic, wire, checkpoint, and recovery decoders.
- Added property testing for side-pot chip conservation and folded-player ineligibility.
- Strengthened CI to run locked debug and release tests for all targets/features, Clippy with warnings
  denied, Rustdoc tests, warning-free documentation, the complete simulation, and package
  verification.

### Breaking changes

- `MentalPoker::shuffle` now returns `Result<ShuffleContribution>` because aggregate-key mismatch is
  a checked error.
- `CertifiedCheckpoint::restore_verified_deck` now requires the current `SessionConfig` and exact
  `AggregateMentalKey`.
- Checkpoint state and certificate wire version 1 are intentionally rejected; regenerate version 2
  checkpoints. Public-message, private-reveal, showdown-bundle, and recovery-bundle formats remain
  at their existing version 1.
- The standalone private-reveal receipt helper is crate-private. Use
  `HandCryptography::open_private_hand` and retain the returned digest.

## 0.1.0 - 2026-06-23

### Added

- Sans-I/O mental-poker engine for a fixed 52-card Texas Hold'em deck.
- Per-hand `(n, n)` threshold ElGamal key setup with proof of secret-key possession.
- Sequential Bayer–Groth verifiable shuffles and Chaum–Pedersen reveal-share proofs.
- Signed, bounded, versioned public/private/showdown wire artifacts.
- Hash-chained public transcript, unanimous checkpoints, and recovery bundles.
- Deterministic no-limit Hold'em betting, side-pot, odd-chip, and showdown logic.
- Financial-parameter validation for compensation and collateral inequalities.
- Extensive module documentation, architecture notes, security guidance, and a complete local
  simulation example.
- Defensive rejection of identity aggregate keys, non-canonical checkpoint decks,
  impossible checkpoint seat states, and phase/payload mismatches.
- Deterministic distinct hash-to-curve derivation for public card encodings and Pedersen CRS points.

### Security notes

- Experimental and unaudited.
- No blockchain-specific settlement contract is included.
- The embedded shuffle implementation is derived from `ziffle` 0.1.0, with a modified Pedersen
  CRS derivation that avoids publicly known discrete logarithms between commitment generators.
