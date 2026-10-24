# openidconnect 4.0.1, patched

This is openidconnect 4.0.1 (https://github.com/ramosbugs/openidconnect-rs,
MIT license — see `LICENSE`) moved to the current RustCrypto elliptic-curve
crates. 4.0.1 is the latest release and pins `p256`/`p384` 0.13, which pull
in `crypto-bigint` 0.5.5 with timing variability in its constant-time
operations (RUSTSEC-2026-0387, fixed in 0.7.6). Only 0.7 is fixed, so no
lockfile update can help: the curve crates themselves have to move.

The server uses these curves only to verify ES256/ES384 ID-token
signatures with the provider's public keys, but `cargo deny` (and good
hygiene) wants the fixed version.

Wired in through `[patch.crates-io]` in the workspace `Cargo.toml`. Changes
(marked `notekpr patch` where they are in a file):

* `Cargo.toml` (and `Cargo.toml.orig`): `p256` and `p384` 0.13 → 0.14
  (released; they bring `elliptic-curve` 0.14 and `crypto-bigint` 0.7.6).
  The code needed no change: `src/core/crypto.rs` `verify_ec_signature`
  uses `VerifyingKey::from_sec1_bytes`, `Signature::from_slice` and
  `Verifier`, which 0.14 still has. Upstream's library tests pass (68,
  among them `test_ecdsa_verification`).
* `Cargo.toml`: a `[lints.rust]` table allowing two upstream lints (as a
  path dependency its warnings would show in every build), and without the
  `[[example]]` entries; the examples, integration tests and lockfiles
  aren't copied.

Drop this directory once upstream releases a version on `p256`/`p384`
0.14 (and offer them the change).
