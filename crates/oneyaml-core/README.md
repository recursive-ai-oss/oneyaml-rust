# oneyaml-core

The stable vocabulary of [oneyaml](https://crates.io/crates/oneyaml):
spans, `Y####` diagnostic codes and their registry, `Diagnostic`,
`LineIndex` (byte offsets to line and column in UTF-8 bytes, characters,
or UTF-16 units), `Path` and `path!`, the input `Encoding`, and the
`Settings`, `Limits`, `Emit`, and `Compat` value types.

No runtime dependencies. Most users depend on `oneyaml`, which re-exports
everything here; start with its
[README](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.0.1/crates/oneyaml/README.md).
The resource-limit profiles, including `Limits::untrusted()` for input
from the network, are documented on
[`Limits`](https://docs.rs/oneyaml-core/latest/oneyaml_core/struct.Limits.html).

Follows semver together with `oneyaml`. MSRV: Rust 1.85 (rolling policy
in the
[oneyaml README](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.0.1/crates/oneyaml/README.md#minimum-supported-rust-version)).
License: MIT OR Apache-2.0.
