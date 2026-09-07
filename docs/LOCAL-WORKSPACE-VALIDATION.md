# Local workspace release validation

## What changed

- Four primary destinations: Home, Library, Models, Settings; vocabulary and
  application profiles remain accessible. Settings has focused subpages.
- Existing P identity and cream/bronze/dark palettes are preserved. The Arrogant
  UI prompt guided hierarchy, restrained copy and removal of competing controls.
- Configurable launcher shortcut and optional direct dictation shortcut. Main
  shortcut opens the compact palette; 1/Enter/click starts toggle dictation;
  the shortcut stops an active dictation. Escape dismisses the palette.
- Local semantic and exact-word search of retained text, returning original
  passages and original history details rather than generated answers.

## Safety and performance behavior

Shortcut-only, search and appearance edits do not reload speech models. Windows
registered-shortcut conflicts are checked on changed bindings only; local app
accelerators and future registrations cannot all be predicted. Unsupported
Windows-key chords, Alt without Ctrl, Shift-only typing chords, and F12 are
rejected instead of masking their native behavior with injected keystrokes.

The library uses one background worker and coalesced requests. It yields to the
speech workload and cancels its own SQLite scan when recording begins. It does
not acquire a lease that could deny recording. A request already executing in
Ollama cannot be guaranteed to stop server-side immediately; the client deadline
is 12 seconds. Search is optional and keyword search remains available without
an embedding model. No cloud inference or model auto-download is enabled.

Source text owns the index. Deletion, disabled history and expiry remove derived
vectors. A model identity change invalidates incompatible vectors. Adaptive
passages account for small model context windows without silent truncation.
Progress reports indexed passages, not a fabricated exact total while incomplete.
Retention is checked before searches and every 60 seconds while the worker is idle.

## Evidence so far

- Independent UI, input and search reviews found and corrected fast-selection,
  unrelated-key-release, stale-target, stale-query, Unicode-offset and retention
  races. Root settings fast-save regression was caught by existing tests and fixed.
- Headless UI: 40 unit tests plus 2 history integration tests, including all 12
  routes across 4 themes and widths 480/760/1180, shortcut capture cancellation
  and Unicode-aware asynchronous passage navigation.
- Native Instant: 4 generated-speech tests passed, including delayed startup,
  ambient input, natural endpoints, forced rollovers and 150-second sessions.
- Native semantic search: official `all-minilm:latest`, synthetic notes only.
  Four topic queries ranked the intended note first; first three had no literal
  query match. SQLite ranking took 0.385–0.847 ms in this tiny fixture. Local query
  embedding took 46–2107 ms, including cold model calls.
- Long semantic fixture: 2,216 Unicode characters, Japanese/code prefix plus an
  English topic near the end; 12 indexed passages, adaptive size 1000 to 250,
  complete coverage and correct near-end retrieval. This is not a Portuguese
  semantic-quality qualification or a large-library performance benchmark.

Final workspace pass: 705 tests passed, 8 asset-dependent tests ignored by the
default command. Seven of those ignored cases were exercised explicitly in the
native Instant, Accurate and embedding runs. Workspace/all-target Clippy with
`-D warnings` and release-asset checks passed.

Native CPU Accurate: 2/2 passed. The 219.06-second generated fixture used
147.63 seconds of incremental compute and peaked at 19.71 seconds of uncommitted
backlog (below the existing 43-second gate). Rolling output scored 56 edits over
723 reference words (7.75%), versus zero for the continuous baseline. This met
the existing 10% delta gate but is worse than an earlier 20-edit run; it is not
evidence of perfect recognition, improved accuracy, or a speed improvement.

The frozen library worker suite passed all 9 cases including native retrieval;
the product-shell integration subset passed 19 cases after adding the final
immediate-validation-feedback regression. The 705-test workspace run preceded
that last additional test; its final 19-test subset and strict all-target lint
were rerun successfully. Final formatting and release-asset checks passed.

## Packaged build

- Installer: `artifacts/installer/Phorminx-0.1.0-x64-setup.exe`
- Size: 12,925,624 bytes.
- Installer SHA-256:
  `13FADF7AC3576BB041D63D6E4887F6F480348F832929B787BCFC18FBE520216F`
- Executable SHA-256:
  `06190985E319DEEB6B5C2CEA0886DD6DC15832B947996EC0B9BA7EDC503F9E0C`
- Release configuration: desktop + Vulkan, static CRT; dependency import checks
  passed. The installer is unsigned and intended for this local test, not public
  distribution. The installed/running executable was not replaced.
- Native GPU execution was not repeated in this round; the native Accurate
  evidence above is CPU. Release compilation verifies the Vulkan feature build.

## What is not claimed

Computer Use app-access approval timed out when attempting the synthetic preview.
No native visual walkthrough or physical shortcut/paste compatibility run was
performed in this round. Headless rendering is not a substitute for those checks.
The preview is synthetic-only and remains available as a development example.
No claim of perfect recognition, universal shortcuts, zero transient compute
contention, or fully qualified public release is made.

## Local data and manual test checklist

The small official all-minilm model was downloaded to Ollama for synthetic tests.
Existing Phorminx settings, user dictations and the running installed app were
not modified by development tests. Semantic search remains opt-in in the app.

After quitting the installed tray app and installing the new build:

1. Open Settings > Shortcuts. Test a new launcher combination, reset, and optional
   direct dictation; verify normal typing, held modifiers and Escape behavior.
2. From an editable field, invoke the launcher, press 1, dictate, then stop with
   the shortcut. Confirm insertion into that original field exactly once.
3. Review Home, Library, Models and the settings subpages in light and dark mode.
4. In Models select `all-minilm:latest` for local search, allow idle indexing, then
   search Library using Meaning. Exact words works before indexing completes.
5. Test a long real-voice dictation in both Instant and Accurate modes.

The future system-audio, HTTP action, AI agent/chat and external-provider ideas
remain recorded in LOCAL-WORKSPACE-PLAN.md and are not implemented here.
