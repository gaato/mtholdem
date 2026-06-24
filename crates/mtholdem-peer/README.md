# mtholdem-peer

Transport-neutral peer-session orchestration for `mtholdem`.

This crate owns per-peer message ordering and host effects without opening sockets, writing storage,
or scheduling timers. Hosts carry the emitted effects over WebRTC, libp2p, a relay, CLI fixtures,
or adapter-backed wrappers, then feed received signed artifacts back into `PeerSession`. Godot
GDExtension code lives outside this workspace and should consume the C ABI instead of duplicating
peer-session validation. WASM/npm packages also live outside this workspace and should consume the
Rust adapter boundary directly.

The initial slice drives the real `mtholdem` key setup, sequential shuffle, private reveal delivery,
hole-card receipts, signed betting actions, public community-card reveal shares, board-card opening
markers, checkpoint certification/announcement, recovery-bundle submission, showdown bundle
publication, `ShowdownOpen` commitments, deterministic payout agreement, and final-hand effects
using `SignedPublicMessage`, `SignedPrivateReveal`, `ShowdownBundle`, `HandCryptography`,
`HoldemHand`, `CertifiedCheckpoint`, `RecoveryBundle`, and `PublicTranscript`.
