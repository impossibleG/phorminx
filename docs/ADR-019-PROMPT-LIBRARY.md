# ADR-019: Prompt library and @-references in instruction fields

- Status: accepted
- Date: 2026-10-17

## Decision

Phorminx keeps a local library of named, tagged prompts in the per-user
database (`prompts`, `prompt_versions`). Every save writes a new version.
Restoring an older version saves it again as the newest one, so history is
append-only and an accidental restore can itself be undone.

Instruction fields store plain text. A prompt inserted with `@` is written
as `@{prompt:ID}` and shown as a chip. The stored form keeps settings,
profiles, the assistant configuration and Actions readable by code that does
not know about prompts. It also avoids a second source of truth.

References resolve to the prompt's latest version when the instructions are
used, not when they are saved:

- dictation custom formatting and per-application profiles, for each dictation;
- the review panel's Reformat, including its one-off instruction;
- the meeting assistant preset, also used by commentary;
- AI-written Action payload instructions.

A reference to a deleted prompt resolves to nothing, is logged as
`prompt_missing`, and is shown as a dashed "Missing prompt" chip until the
user removes or replaces it. Deleting a prompt first lists every place that
refers to it and needs a second press.

## Consequences

- Editing a prompt changes every workflow that uses it from the next use on.
  This is the point of a shared prompt, and the chip makes the dependency
  visible. Users who want a one-off variation type free text next to the chip
  or save a new prompt.
- Older builds see `@{prompt:ID}` as literal text. They still work, just
  without the prompt's content.
- Prompt names are unique (case-insensitive), bounded to 80 characters, with
  up to 12 tags; prompt text is bounded to 4,096 characters, the same limit as
  custom instructions.
- Investigations (#8) adds an assistant question box; it should adopt the
  instruction field once it gains editable assistant instructions.
