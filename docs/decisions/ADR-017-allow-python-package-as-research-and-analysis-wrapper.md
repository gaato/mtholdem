# ADR-017: Allow a Python Package as a Research and Analysis Wrapper

## Status
Accepted

## Date
2026-06-26

## Context
Python is a natural consumer for poker research and analysis. Libraries such as
eval7 show that Python users often want hand evaluation, range parsing,
hand-vs-range equity calculation, and quick simulations from notebooks or test
harnesses. That overlaps with the next `mtholdem-bot` direction in ADR-016:
range-aware Monte Carlo estimation, bot diagnostics, transcript replay, and
policy evaluation.

This is a different pressure than Godot or the C ABI. Godot needs a native
engine wrapper over a stable C boundary. Python users usually want a
Python-shaped package with simple values, wheels, type hints, and analysis
helpers. They may not need the low-level peer-session artifact flow first.

The workspace already has target-wrapper boundaries in ADR-007 through ADR-009.
Those decisions keep engine-specific packaging out of protocol crates and keep
the C ABI portable. Python should follow the same principle: it can be a wrapper
surface, but it must not become a second protocol implementation or pull Python
runtime assumptions into production crates.

## Decision
Allow a future Python package wrapper, but treat it as a separate target
surface from the C ABI, Godot, and CLI.

The preferred implementation path is a Python package built with PyO3 and
maturin, wrapping Rust crates directly. It should not use `mtholdem-ffi` as its
default internal boundary unless a concrete packaging reason appears. Python can
own Pythonic classes, functions, wheels, type hints, and notebook-friendly
helpers while Rust crates remain the source of truth.

The first Python surface should focus on research and analysis APIs:

- card parsing and formatting;
- hand evaluation and hand-category labels;
- range parsing and range sampling;
- hand-vs-range or range-vs-range equity estimation;
- bot policy diagnostics;
- transcript or table-event replay and analysis.

Do not expose the full peer-session protocol surface first unless a real Python
host needs it. If exposed later, it should mirror the same public action context
and artifact boundaries as Rust and C, without inventing Python-only protocol
semantics.

The package may live in this repository or downstream. If it lives in this
workspace, it should be an optional wrapper crate such as `mtholdem-python`,
with packaging and CI gates separated from the normal production Rust gates
where needed. The core dependency direction remains one-way:
Python wrapper -> Rust crates. Production Rust crates must not depend on Python
packaging, PyO3, or Python runtime concepts.

## Alternatives Considered

### Use the C ABI from Python
- Pros: Reuses the same stable native boundary as Godot and C++.
- Cons: Python can bind Rust directly, and C buffer ownership makes a worse
  Python API for analysis-heavy functions.
- Rejected: The C ABI remains available for generic native consumers, but PyO3
  is the better default for a Python package.

### Put Python package code in protocol crates
- Pros: Direct access to internals and fewer wrapper modules.
- Cons: Pulls Python packaging and runtime assumptions into crates that should
  stay Sans-I/O Rust libraries.
- Rejected: Python belongs at the target-wrapper layer.

### Keep Python entirely outside this repository
- Pros: Keeps the Rust workspace smaller.
- Cons: Range/equity/bot-analysis work may evolve tightly with Rust public
  types and tests. Keeping a thin optional package nearby can prevent drift.
- Rejected: An external package is allowed, but an optional in-workspace wrapper
  is also acceptable if it stays out of production dependency paths.

### Expose every Rust type directly
- Pros: Fast to implement.
- Cons: Produces a binding rather than a useful Python package and leaks Rust
  ownership, naming, and protocol details into notebooks.
- Rejected: The package should present Pythonic APIs over stable Rust behavior.

## Consequences
- Python joins C ABI, CLI, Godot, and WASM as a possible target wrapper surface.
- `mtholdem-python` should be considered only when there is a concrete
  analysis, equity, or bot-evaluation API to expose.
- PyO3 and maturin are acceptable for the wrapper crate, but not for production
  protocol crates.
- Python package tests may need separate CI treatment so wheel building does
  not slow or destabilize ordinary Rust validation.
- Python APIs should be designed around public information boundaries. They
  must not expose hidden cards or test-only shortcuts as production analysis
  features.
