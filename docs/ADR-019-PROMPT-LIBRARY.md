# ADR-019: Prompt library and @-references in instruction fields

- Status: accepted
- Date: 2026-10-17

## Decision

Phorminx keeps a local library of named, tagged prompts in the per-user
database (`prompts`, `prompt_versions`). Every save writes a new version.
Restoring an older version saves it again as the newest one, so history is
append-only and an accidental restore can itself be undone.

Instruction fields are ordinary text boxes (`<textarea>`) and store plain
text. Typing `@` opens suggestions at the caret. Picking one inserts a
reference written as `@[Prompt name]`, as normal text, so undo, copy and
paste, IME and screen readers behave as in any text box.

A mirror layer drawn over the textarea, with the same font, padding, wrapping
and scroll position, shows each reference as a chip; the textarea's own
glyphs are transparent, and its caret and selection show through. References
act as single units: the caret never rests inside one, a selection that
touches one covers all of it, and Backspace or Delete next to one removes it
whole, through the browser's editing so undo restores it. Clicking a chip
opens a preview with "Edit in Prompts". Under the box, a row of pills lists
the same prompts, which gives the keyboard and screen readers the same
previews. The stored form keeps settings, profiles,
the assistant configuration and Actions readable by code that does not know
about prompts. It also avoids a second source of truth.

The first test builds used a rich editor that drew references as chips
inside a `contenteditable` element and stored `@{prompt:ID}`. That editor was
removed: it was not a real text box, and it behaved differently from one.
Old `@{prompt:ID}` references still resolve, and a field shows them as
`@[Name]` once the library has loaded, so the next save rewrites them.

References resolve to the prompt's latest version when the instructions are
used, not when they are saved:

- dictation custom formatting and per-application profiles, for each dictation;
- the review panel's Reformat, including its one-off instruction;
- the meeting assistant preset, also used by commentary;
- AI-written Action payload instructions.

A reference to a prompt that does not exist resolves to nothing, is logged as
`prompt_missing`, and is shown as a red "Missing" pill until the user
removes or replaces it. Deleting a prompt first lists every place that refers
to it and needs a second press. References use names, so renaming a prompt
that is in use also lists those places first. Names cannot contain `[` or
`]`.

## Consequences

- Editing a prompt changes every workflow that uses it from the next use on.
  This is the point of a shared prompt, and the pill makes the dependency
  visible. Users who want a one-off variation type free text next to the
  reference or save a new prompt.
- Older builds see `@[Prompt name]` as literal text. They still work, just
  without the prompt's content.
- Prompt names are unique (case-insensitive), bounded to 80 characters, with
  up to 12 tags; prompt text is bounded to 4,096 characters, the same limit as
  custom instructions.
- Investigations (#8) adds an assistant question box; it should adopt the
  instruction field once it gains editable assistant instructions.
