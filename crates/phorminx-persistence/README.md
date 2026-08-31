# phorminx-persistence

Local SQLite persistence for Phorminx. It provides typed repositories for:

- opt-in dictation history with disabled, 24-hour, 7-day, 30-day, and indefinite retention;
- exact, optionally language- and application-scoped lexicon aliases;
- application profiles keyed by executable basename.

The crate does not log. It never accepts or stores window titles, and executable
identities must be basenames rather than filesystem paths.

Run its isolated checks with:

```powershell
cargo test --manifest-path crates/phorminx-persistence/Cargo.toml
cargo clippy --manifest-path crates/phorminx-persistence/Cargo.toml --all-targets -- -D warnings
```

