# Vendored patches

Crates here replace a crates.io release through `[patch.crates-io]` in the
workspace `Cargo.toml`. Keep each one a verbatim copy of the published crate
plus the smallest fix, and drop it once upstream ships the fix.

- `saturating-time` 0.5.0 (MIT OR Apache-2.0, The Tor Project): `find_limit`
  halves its step down to 1ns, but Windows `SystemTime` has 100ns resolution,
  so `checked_sub(1ns)` returns the same value and the search never ends.
  Arti calls it while parsing every consensus, so Tor bootstrap hung at
  100% CPU on Windows. The patch stops when a step no longer changes the value.
