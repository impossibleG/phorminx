# ADR-010: Private local persistence

## Decision

Phorminx stores optional durable state in a per-user SQLite database through a dedicated `phorminx-persistence` crate. The connection enables WAL, foreign-key enforcement, a bounded busy timeout, and transactional numbered schema migrations. SQLite is linked through `rusqlite`'s bundled feature so installation does not depend on a system SQLite DLL.

Dictation history is opt-in and defaults to disabled. Its policy can be disabled, 24 hours, 7 days, 30 days, or indefinite. Disabling history deletes existing history in the same transaction; users can also clear history immediately without changing the policy. Bounded policies purge records older than their exact cutoff while retaining records on the boundary. Each retained record can hold raw, normalized, cleaned, and selected text plus language, target executable basename, stage timings, and warnings.

The lexicon stores exact case-sensitive aliases with optional language and executable scopes, a case-output policy, and an enabled flag. Lookup does no fuzzy matching and orders more specific applicable entries ahead of global ones. Application profiles are keyed case-insensitively by executable basename and hold formatting style, optional language and custom instructions, insertion preference, and an explicit deny flag.

The crate has no logging dependency and does not expose a logging hook. It rejects filesystem paths as executable identities and has no window-title field. Database errors are wrapped without appending paths or user content. The application composition layer will choose the database's per-user local path; synchronization and cloud storage are outside this boundary.

## Consequences

- Dictated content remains on the local machine and is not persisted unless the user enables history.
- WAL permits readers without blocking ordinary writes and makes later settings/history UI work practical.
- Immediate deletion removes live rows, although SQLite/WAL files and storage media cannot promise forensic erasure; secure-erasure claims are intentionally avoided.
- Bundled SQLite increases binary size but makes behavior and migration support predictable.
- Profiles use executable basenames instead of paths or window titles, reducing sensitive metadata while remaining sufficient for normal Windows application routing.
- Future schema changes must be additive numbered migrations and must remain testable against an older database fixture.
