# mtholdem-peer

Transport-neutral peer-session orchestration for `mtholdem`.

This crate owns per-peer message ordering and host effects without opening sockets, writing storage,
or scheduling timers. Hosts carry the emitted effects over WebRTC, libp2p, a relay, CLI fixtures, or
future FFI/WASM/Godot wrappers, then feed received signed artifacts back into `PeerSession`.

The initial slice drives the real `mtholdem` key setup, sequential shuffle, private reveal delivery,
hole-card receipts, signed betting actions, public community-card reveal shares, board-card opening
markers, checkpoint certification/announcement, recovery-bundle submission, showdown bundle
publication, `ShowdownOpen` commitments, deterministic payout agreement, and final-hand effects
using `SignedPublicMessage`, `SignedPrivateReveal`, `ShowdownBundle`, `HandCryptography`,
`HoldemHand`, `CertifiedCheckpoint`, `RecoveryBundle`, and `PublicTranscript`.
