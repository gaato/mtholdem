# Third-party notices

## ziffle 0.1.0

`crates/mental-card-core/src/crypto/bg_shuffle.rs` is derived from `ziffle` 0.1.0 by Chris Ricketts and the
ziffle contributors:

- Project: https://github.com/v26-solutions/ziffle
- Package: `ziffle` version 0.1.0
- License: MIT OR Apache-2.0

The derived file has been modified for the `mtholdem` workspace, including API visibility, bounded wrappers,
context integration, verification trust boundaries, documentation changes, and Pedersen commitment
CRS point derivation. The upstream project explicitly describes itself as experimental and
unaudited; that warning also applies to the derived implementation.

The full MIT and Apache-2.0 license texts are distributed in `LICENSE-MIT` and
`LICENSE-APACHE`.

## Rust dependencies

The exact dependency graph and versions are recorded in `Cargo.lock`. Dependencies remain under
their respective licenses. Before redistribution in a regulated or commercial environment, run an
independent license review (for example, with `cargo-about` or an equivalent SBOM process) and
preserve all required notices.
