# Phorminx unified UI acceptance specification

Status: release gate for Phase 5

Date: 2026-08-31

Governing inputs: `playground/arrogant UI prompt.md`, `docs/UI-BRAND-BLUEPRINT.md`, and the current product contracts in `crates/phorminx-*`

## 1. Purpose

This document defines what “finished” means for the unified Phorminx interface. It is deliberately stricter than theme compliance. A build can use the correct colors, type, spacing, and components and still fail if it looks like a competent generic settings application with a bronze accent.

The governing emotional test is:

> Phorminx feels premium without announcing it. It is laconic, exact, and privately powerful. Every visible element appears chosen, not accumulated.

The governing product test is:

> The redesign may reorganize and clarify existing behavior, but it must not remove, obscure, weaken, or silently reinterpret any local-processing, persistence, model-selection, target-safety, or fallback contract.

## 2. Gate vocabulary and evidence

Every release-blocking row below must be marked `Pass` with named evidence. `Partial`, `Not applicable`, “looks fine,” and “works on my machine” do not pass a gate.

| Mark | Meaning |
|---|---|
| **B** | Release blocker. The release candidate does not ship when this fails. |
| **V** | Visual review with captured screenshots at the specified sizes/states. |
| **M** | Manual interaction test on physical Windows. |
| **A** | Automated unit, integration, snapshot, or accessibility-tree test. |
| **P** | Privacy/safety review against existing contracts. |

For each row, the acceptance record must name the build hash, Windows version, display scale, color mode, reviewer, evidence path, and result. Visual review must use the actual executable, not only mockups or a component gallery.

## 3. Adversarial audit of the current product

| Surface | Theme compliance today | Vibe compliance today | Contract that must survive | Principal redesign risk |
|---|---|---|---|---|
| Tray | Native and functional | Fails: exposes four unrelated utility windows | Status, Settings/History/Lexicon/Profiles entry points, Quit, Explorer restart recovery, keyboard activation | A beautiful shell is added while tray commands still spawn or duplicate child windows |
| Overlay | Operationally clear and non-activating | Fails: generic dark status rectangle and generic language | Fixed content-free statuses, persistent work states, timed result states, never steals focus | Brand animation steals focus, leaks transcript text, or obscures the active field |
| Settings | Some controls scale with DPI and validation is real | Fails: dense undifferentiated form | Every current field, explicit save semantics, verified model download, validation, launch-at-login | “Simplification” hides advanced controls or changes values live without an apply boundary |
| History | Native controls and copy actions work | Fails: sequential diagnostic viewer | Raw/normalized/cleaned/selected variants, copy raw/output, retention and clear | Timeline polish drops variants, warnings, metadata, or immediate deletion semantics |
| Lexicon | Complete CRUD | Fails: mechanical record pager | Exact alias, written form, language/app scope, four case policies, enabled state | Friendly vocabulary language implies fuzzy learning or silently normalizes exact keys |
| Profiles | Complete CRUD | Fails: another isolated form | Executable basename, five formatting modes, custom prompt, language, three insertion choices, deny | App icons/full paths/window titles creep in, or `Direct` weakens guarded insertion |
| Model setup | Verified and consent-based | Fails: machinery is scattered | Explicit path/selection, status, consented download, installed Ollama discovery, lifecycle | One-click “AI setup” silently downloads, selects, or sends content somewhere non-local |
| First run | Recovers from absence of a model | Fails: settings shown under pressure | No silent weight download, optional Ollama, successful safe commissioning | Tutorial theater, forced optional choices, or test dictation inserts unexpectedly |
| Fatal/recoverable errors | Honest but system-generic | Fails: child-window errors read as crashes | Fatal startup errors remain visible; child failures recover without process death | Branded errors hide operational detail or every failure becomes an alarming modal |

Current Win32 native controls provide some platform behavior incidentally; the custom Rust shell must prove keyboard, assistive-technology, high-contrast, and DPI behavior explicitly rather than assume parity.

## 4. Brand and vibe acceptance matrix

### 4.1 Whole-product identity

| ID | Gate | Acceptance criterion | Evidence |
|---|---|---|---|
| BV-01 | **B/V** | At 3-second exposure, the shell is identifiable as Phorminx with all text and the logo temporarily hidden. Recognition must come from hierarchy, geometry, material, and restraint—not bronze alone. | Blind comparison against two neutral Windows utility mockups; at least 4 of 5 reviewers identify the Phorminx surface and cite a non-color trait. |
| BV-02 | **B/V** | The UI feels composed, exact, and privately capable; it does not feel promotional, gamified, militaristic, or developer-only. | Reviewers choose at least 3 of `composed`, `exact`, `disciplined`, `capable`; no more than 1 chooses `marketing`, `gaming`, `aggressive`, or `template`. |
| BV-03 | **B/V** | Every route has one clear visual thesis and one primary user job. Removing the route title must not make Models, History, and Settings interchangeable card grids. | Full-route screenshot review with route titles masked. |
| BV-04 | **B/V** | Sparse composition preserves comprehension. Empty space creates hierarchy; it does not strand unlabeled controls, hide cause/effect, or force discovery through tooltips. | First-use task review with a user who has not seen the blueprint. |
| BV-05 | **B/V** | Premium is implied by precision: alignment, optical balance, typography, copy, state transitions, and fit at every size. No copy describes Phorminx as premium, exclusive, elite, Spartan, or powerful. | Screenshot and string audit. |
| BV-06 | **B/V** | Ancient reference is expressed only through the instrument-derived mark, proportion, rhythm, and tension. No helmet, shield-as-default, laurel, column, marble texture, red cape, pseudo-Greek letter substitution, or inscription font appears. | Asset and screenshot audit. |
| BV-07 | **B/V** | The `Tensioned P` reads first as an ownable mark and second as a P/instrument metaphor. It does not read primarily as a microphone, podcast app, play button, shield, lambda/cloud logo, or generic waveform. | Unprompted small-mark identification test at 32 and 48 px. |
| BV-08 | **B/V** | Bronze is a scarce active-energy signal. It is not used as a decorative wash, panel outline, ambient glow, badge color, or substitute for hierarchy. On a typical idle route bronze occupies less than roughly 5% of visible pixels. | Token audit and screenshot histogram/manual review. |
| BV-09 | **B/V** | Oxblood appears only for genuine destructive intent or failure. Moss appears only for verified readiness when shape/text also communicates the state. | Semantic-color screenshot audit. |
| BV-10 | **B/V** | Copy is short, declarative, and specific. It never cheers, apologizes theatrically, congratulates, upsells, or says “successfully” where state alone is enough. | UI string inventory review. |
| BV-11 | **B/V** | `PHORMINX` wordmark is rare: title area, onboarding, About, and release surfaces only. Ordinary navigation is sentence case and uppercase metadata is never used for prose. | Screenshot/string audit. |
| BV-12 | **B/V** | Monospace is limited to executable basenames, model identifiers, shortcut chords, timings, and similarly technical tokens. Transcript content remains a reading face. | Typography audit. |
| BV-13 | **B/V** | The design does not use a card to group every concept. Page hierarchy is carried primarily by planes, whitespace, type, and one-pixel separators. | Each route has no more containers than its information model requires; reviewer names the purpose of every container. |
| BV-14 | **B/V** | Pills are reserved for genuinely compact states/tags or segmented selection. Buttons, navigation, inputs, and arbitrary labels are not pill-shaped by default. | Component inventory review. |
| BV-15 | **B/V** | Icons share optical weight, squared geometry, and intentional cut terminals. Mixed third-party icon families, emoji, and arbitrary Unicode symbols are absent. | 16/20/24 px icon sheet. |
| BV-16 | **B/V** | Dark-first is authored rather than merely color-inverted. Light and high-contrast modes preserve hierarchy without pretending to reproduce dark material. | Dark, light, and two high-contrast screenshot sets. |
| BV-17 | **B/V** | The compact overlay is recognizably related to the shell but remains subordinate, non-promotional, and readable in peripheral vision. | Overlay state strip over light, dark, busy, and full-screen backgrounds. |
| BV-18 | **B/V** | The product does not display fake analytics, streaks, productivity scores, tips carousels, greetings, testimonials, upgrade affordances, or empty dashboard charts. | Route and string audit. |

### 4.2 Theme compliance is necessary, not sufficient

All authored components must use named tokens for color, spacing, type, radius, stroke, motion, and semantic state. However, passing token lint does not satisfy any `BV` gate. A screenshot that could belong to a mediocre SaaS product fails even if every token is correct.

## 5. Route-by-route capability parity

### 5.1 Application shell and system entry points

| ID | Gate | Required behavior |
|---|---|---|
| SH-01 | **B/A/M** | Exactly one content shell exists per user session. A second process activates/focuses the existing shell or exits safely; it never creates a second runtime or database writer. |
| SH-02 | **B/A/M** | Tray commands for Settings, History, Lexicon, and Profiles focus the shell and select the corresponding route. Repeating a command does not duplicate windows or reset unsaved edits without warning. |
| SH-03 | **B/M** | Closing the shell returns Phorminx to tray operation; Quit is explicit and tears down recording, workers, overlay, tray, and persistence safely. Standard minimize, maximize, move, resize, Alt+F4, and taskbar activation behave like a Windows application. |
| SH-04 | **B/M** | Explorer restart restores the tray icon. Sleep/resume cancels unsafe in-flight work and returns to Ready without a fatal dialog. |
| SH-05 | **B/M** | The integrated/custom title bar retains correct hit targets, system menu, snap layouts where supported, double-click maximize/restore, keyboard system menu, and monitor-aware dragging. If it cannot, use the standard title bar. |
| SH-06 | **B/A/M** | Route changes do not reconstruct runtime state, restart models, lose list selection unnecessarily, or commit form edits implicitly. Dirty-route navigation prompts or preserves a draft predictably. |
| SH-07 | **B/M** | Global dictation remains available while the shell is closed, minimized, obscured, or on another virtual desktop. Opening the shell never captures the dictation hotkey as ordinary text. |
| SH-08 | **B/V** | Persistent shell status distinguishes Local/Ready, Loading, Listening, Transcribing, Refining, and Needs attention without relying on color alone. |

### 5.2 Home

| ID | Gate | Required behavior |
|---|---|---|
| HM-01 | **B/V/M** | Ready state, active shortcut, microphone readiness, Whisper readiness, and optional Ollama readiness are legible without looking like a monitoring dashboard. |
| HM-02 | **B/M** | `Test dictation` is the sole dominant action and never inserts into the previously focused external target. It shows a reviewable result or no-speech/recoverable error state inside Phorminx. |
| HM-03 | **B/P** | When history is disabled, Home does not query or render transcript previews and states that history is off without coercion. |
| HM-04 | **B/M** | When history is enabled, at most the three most recent selected outputs appear; each opens its History detail. Raw text is not exposed by default. |
| HM-05 | **B/V** | Loading and degraded readiness are operational: they identify which local subsystem needs attention and what dictation will do meanwhile. No indefinite spinner exists without explanatory copy or a bounded recovery action. |

### 5.3 History

| ID | Gate | Required behavior |
|---|---|---|
| HI-01 | **B/A/M** | Records are chronological and expose created time, selected output preview, language when present, target executable basename when present, and stable selection. Full paths and window titles never appear. |
| HI-02 | **B/A/M** | Detail offers Output, Raw, Normalized, and Cleaned variants. Optional absent variants are omitted—not presented as disabled promises—and selected output remains unambiguous. |
| HI-03 | **B/M** | `Copy output` is persistent; `Copy raw` is subordinate. Copy writes exactly the chosen stored value and produces a content-free confirmation. |
| HI-04 | **B/A/M/P** | Retention values remain Disabled, 24 hours, 7 days, 30 days, and Indefinite. Choosing Disabled removes existing history in the same committed operation and prevents future writes. |
| HI-05 | **B/M/P** | `Clear history` requires an explicit confirmation that names the irreversible local deletion, has safe default focus, and clears records without changing the retention policy. It does not place transcript excerpts in dialogs or logs. |
| HI-06 | **B/A/M** | Metadata can expose audio/STT/formatting/insertion timings, warnings, and insertion result when present. Missing metadata is omitted. Internal stack traces and local paths do not appear. |
| HI-07 | **B/M** | List/detail remains usable with 0, 1, 1,000, and the maximum practical number of records; selection and scroll do not jump after background refresh or deletion. |
| HI-08 | **B/M** | Search/filter, if implemented, is local, cancellable, keyboard accessible, and clears predictably. It never implies cloud indexing. Search is not required merely to decorate an empty toolbar. |

### 5.4 Lexicon

| ID | Gate | Required behavior |
|---|---|---|
| LX-01 | **B/A/M** | List and editor preserve written form (`canonical`), spoken alias, optional language, optional executable basename scope, case policy, enabled state, and stable record identity. |
| LX-02 | **B/P/V** | Copy consistently says aliases are exact matches and that this is replacement, not model training. The UI never promises fuzzy learning, adaptation, or pronunciation inference. |
| LX-03 | **B/A/M** | Case policies remain Preserve input, Use written form, Lowercase, and Uppercase with no remapping. A preview demonstrates the saved behavior without mutating the entry. |
| LX-04 | **B/A/M/P** | Scope is `Everywhere` or a validated executable basename. Paths, wildcards, window titles, process IDs, and application content are rejected. Optional language keeps the existing validated language format. |
| LX-05 | **B/M** | New, edit, enable/disable, save, cancel/discard, and delete are keyboard operable. New and edit modes are visually distinct. Delete is separated from Save and requires confirmation for an existing entry. |
| LX-06 | **B/A/M** | Validation is inline, attached to the offending field, preserves the draft, and prevents partial persistence. Duplicate/exact-key conflicts are explained without exposing database internals. |
| LX-07 | **B/M** | Empty, filtered-empty, saving, save-failed, delete-failed, and long-value states are intentional. A 4,096-character pathological value cannot overlap controls or freeze layout. |

### 5.5 Application profiles

| ID | Gate | Required behavior |
|---|---|---|
| PR-01 | **B/A/M/P** | Profile identity is a validated executable basename only, compared case-insensitively. Full path, window title, icon extraction path, process ID, publisher, and usage history are not requested or persisted. |
| PR-02 | **B/A/M** | Formatting choices remain Raw, Light, Balanced, Strong, and Custom. Custom requires nonblank instructions; non-Custom does not accidentally apply stale custom text. |
| PR-03 | **B/A/M** | Language override remains optional and falls back to global behavior when blank. Insertion remains Automatic, Direct when safe, and Clipboard only. |
| PR-04 | **B/P/A** | `Direct when safe` never bypasses target identity revalidation, sensitive/unsupported target checks, held-modifier checks, or uncertain input-injection fallback. Visual preference cannot weaken the insertion contract. |
| PR-05 | **B/A/M** | `Block dictation in this application` is visually isolated, names the consequence, and wins over formatting/insertion choices. Saving a deny profile cannot look like a formatting-only change. |
| PR-06 | **B/M** | List summary reads as policy, e.g. `In code.exe: Light · English · Clipboard only`; it does not hide a deny state or reduce all profiles to app-name cards. |
| PR-07 | **B/M** | New, edit, save/apply, cancel/discard, and delete preserve restart/reload semantics. If a restart is still required, the UI explains and performs it safely; it never claims live application before runtime state has changed. |
| PR-08 | **B/M** | Empty, saving, validation-error, persistence-error, stale-selection, and very long executable/custom-instruction states are designed and keyboard usable. |

### 5.6 Models

| ID | Gate | Required behavior |
|---|---|---|
| MO-01 | **B/P/V** | Whisper and Ollama are visibly separate local systems. “AI,” “smart model,” or a single undifferentiated readiness switch does not replace their actual names and responsibilities. |
| MO-02 | **B/A/M/P** | Whisper displays configured path or installed model, language capability, verification status, and explicit Browse/Change and consented Download actions. No model weight is silently downloaded at startup or first dictation. |
| MO-03 | **B/A/M/P** | Recommended download identifies the pinned artifact and approximate size before consent, supports cancellation, verifies the final file, removes a failed temporary artifact, and does not replace a previously verified model on failure. |
| MO-04 | **B/A/M** | Ollama discovery distinguishes not running, no models installed, selected model available, selected model missing, loading/warming, and request failure. Dictation remains available through deterministic fallback. |
| MO-05 | **B/A/M/P** | Ollama selection is explicit; the app does not silently choose the first installed model. Only the fixed loopback endpoint is used, without redirects or ambient proxies. |
| MO-06 | **B/A/M** | Residency modes remain Instant, Balanced, and Memory saver with concise behavioral explanations. Changing mode cannot strand an in-flight dictation or misreport residency. |
| MO-07 | **B/A/M** | Balanced, Strong, and Custom are unavailable or clearly degraded until the explicitly selected installed model is usable. Failure copy states the exact local fallback: recognizable text is never discarded. |
| MO-08 | **B/V/M** | Long model IDs and paths truncate visually with access to the full value, never expand the route width, and never leak into diagnostics or screenshots intended for support without explicit consent. |

### 5.7 Settings

| ID | Gate | Required behavior |
|---|---|---|
| ST-01 | **B/A/M** | Input preserves Windows default plus exact enumerated microphone selection and Hold/Toggle recording modes. Missing saved devices show recovery to Windows default rather than silently rewriting preference. |
| ST-02 | **B/A/M** | Recognition preserves language and minimum RMS threshold with existing validation: language is 2–16 ASCII letters/hyphens; RMS is finite and between 0 and 1. |
| ST-03 | **B/A/M** | Formatting preserves Raw, Light, Balanced, Strong, Custom, optional custom instructions, selected Ollama model relationship, and lifecycle. Strong warns that phrasing may change while protected values remain guarded. |
| ST-04 | **B/A/M/P** | Privacy preserves all five retention choices and offers clear local-data actions. No default flips from the user’s migrated value during redesign. |
| ST-05 | **B/A/M** | Startup preserves launch-at-login and accurately distinguishes enabled, disabled, and stale/different command states. Registry failure is recoverable and does not falsify the saved checkbox. |
| ST-06 | **B/M** | Advanced exposes model path and content-free diagnostics entry without making ordinary setup depend on understanding paths. Model acquisition itself remains on Models. |
| ST-07 | **B/A/M** | Save validates the complete candidate, persists atomically, and reports whether restart/reload is required. Cancel/discard leaves runtime and disk unchanged. No partial live update occurs before successful persistence. |
| ST-08 | **B/M** | Unsaved edits survive recoverable errors and accidental route switching. Closing the shell with dirty settings produces one restrained decision surface, never silently saves. |

### 5.8 First run / commissioning

| ID | Gate | Required behavior |
|---|---|---|
| ON-01 | **B/V/P** | Step 1 states the local-processing promise plainly and accurately. It does not imply that Ollama or model acquisition is bundled when it is not. |
| ON-02 | **B/M** | Microphone and shortcut check works with keyboard only, handles denial/missing device, and allows return after fixing Windows permission. |
| ON-03 | **B/P/M** | Whisper verification/download requires informed consent. Optional Ollama and launch-at-login can be skipped without warnings, guilt copy, or reduced access to core dictation. |
| ON-04 | **B/P/M** | Test dictation never automatically inserts into another application and follows history policy explicitly. Transcript text stays inside the commissioning result surface unless the user copies it. |
| ON-05 | **B/A/M** | Completion is persisted only after required readiness is real. Interruption at every step resumes safely or starts over without corrupting settings/model artifacts. |
| ON-06 | **B/V** | Six-step progress is a restrained index; no confetti, celebration illustration, achievement language, or generic “Let’s get started!” wizard shell. |

### 5.9 Overlay, tray, notifications, and errors

| ID | Gate | Required behavior |
|---|---|---|
| OT-01 | **B/A/M/P** | Overlay accepts fixed enum-like states only and never receives or renders transcript, clipboard, window-title, executable-path, model-prompt, or error-detail content. |
| OT-02 | **B/A/M** | Loading, Listening, Transcribing, and Refining remain visible until superseded. Ready, Inserted, Copied/Ready to paste, No speech detected, and Error use stale-timer-safe auto-hide behavior. |
| OT-03 | **B/M** | Overlay is non-activating, does not alter the target snapshot, does not intercept typing/clicks, remains visible on mixed-DPI monitors, and never covers the caret by default. |
| OT-04 | **B/V** | User-facing `Cleaning` becomes `Refining`; internal state names may remain. Tray tooltip, shell, and overlay vocabulary agree. |
| OT-05 | **B/M/P** | Clipboard-only fallback clearly says output is ready to paste and, when useful, gives a content-free reason such as target changed. It never forces injection or elevation. |
| OT-06 | **B/M** | Recoverable route errors are inline and retryable. Modal dialogs are reserved for unrecoverable startup failure or platform-required confirmation. A child-surface failure never terminates the app. |
| OT-07 | **B/V/P** | Error text is operational and sparse but not vague. It can show a safe cause/action without transcript content, full paths, raw prompts, database SQL, stack traces, or secret environmental details. |

## 6. Keyboard, accessibility, DPI, contrast, and motion

### 6.1 Keyboard and focus

| ID | Gate | Acceptance criterion |
|---|---|---|
| KB-01 | **B/A/M** | Every action and route is reachable without a pointer. Tab/Shift+Tab order follows visual reading order and never enters decorative elements. |
| KB-02 | **B/M** | Arrow keys navigate sidebar, lists, tables, combo boxes, segmented variants, and radio-like choices according to Windows conventions. Selection and keyboard focus remain visually distinct. |
| KB-03 | **B/M** | Enter activates the focused primary action only when safe; Space toggles focused switches/checks; Escape closes only the topmost transient surface or cancels a draft with warning. Escape never quits the application. |
| KB-04 | **B/M** | Alt+underlined mnemonic behavior is provided where appropriate or a documented equivalent exists. Standard Ctrl+C works in selectable/read-only transcript content; Ctrl+A selects within the active text editor, not the whole page. |
| KB-05 | **B/M** | Focus is always visible at 100–200% scale, on every theme and state. Bronze-light focus has a non-color geometry/outline and at least 3:1 contrast against adjacent colors. |
| KB-06 | **B/M** | Opening a route from tray places focus on the route heading or first meaningful control; dialogs/drawers return focus to the invoker. Deleting a row moves focus predictably to the next row, previous row, or New action. |
| KB-07 | **B/A/M** | No focus trap exists in tables, multiline editors, download progress, error surfaces, drawers, or disabled-control groups. |
| KB-08 | **B/M** | Global Ctrl+Alt+Space behavior is tested while every shell control has focus. Hold and Toggle modes do not type shortcut characters into editors or leave modifiers logically stuck. |
| KB-09 | **B/A/M** | Accessibility names, roles, values, checked/expanded/selected states, errors, and live status updates are exposed for custom-drawn controls. Color swatches, icons, and the mark are not the sole accessible label. |
| KB-10 | **B/M** | Screen-reader announcements are concise: route changes, save/error results, model-download progress milestones, and dictation state changes announce once rather than on every frame. Transcript content is not announced unexpectedly. |

### 6.2 DPI, resizing, and text scale

| ID | Gate | Acceptance criterion |
|---|---|---|
| DP-01 | **B/M/V** | Test at 100%, 125%, 150%, 175%, and 200% Windows display scale, including moving the open shell and active overlay between monitors of different scales. |
| DP-02 | **B/M** | Window, hit targets, fonts, icons, separators, focus rings, shadows, and overlay re-render crisply after per-monitor DPI change; the process does not rely on a restart. |
| DP-03 | **B/M/V** | At 900×620 logical minimum, every route remains operable. Below minimum, Windows constrains resizing; content is not clipped behind unreachable actions. At 1120×760 and ultrawide sizes, content does not stretch into low-density emptiness. |
| DP-04 | **B/M** | Windows text-size settings at 100%, 150%, and 200% reflow labels and content. Fixed-height rows either grow or clamp with accessible expansion; text never overlaps or disappears. |
| DP-05 | **B/V** | One-pixel optical separators remain visible and aligned without becoming inconsistent 1/2-pixel blur at fractional scales. Small icons choose size-specific art or pixel-aligned vectors. |
| DP-06 | **B/M** | Integrated title-bar drag, resize borders, window controls, snap, and system menu hit regions stay correct at every tested scale and monitor transition. |

### 6.3 High contrast, light mode, and color independence

| ID | Gate | Acceptance criterion |
|---|---|---|
| HC-01 | **B/M/V** | Test authored dark, usable light, Windows High Contrast Black, and Windows High Contrast White (or current equivalent contrast themes). High contrast uses system colors where needed rather than forcing Abyss/Iron. |
| HC-02 | **B/A/V** | Normal text meets 4.5:1; large text and meaningful graphics meet 3:1; focused, selected, hovered, error, destructive, disabled, ready, and active-route states remain distinguishable without color. |
| HC-03 | **B/M** | OS high-contrast changes apply while the app is running. No restart is required and custom backgrounds do not erase system selection/focus cues. |
| HC-04 | **B/V** | Bronze, moss, and oxblood retain semantic distinction through label, icon, shape, or placement under grayscale and common color-vision simulations. |
| HC-05 | **B/V** | Disabled controls remain readable but unmistakably unavailable. They do not drop below necessary text legibility or masquerade as secondary copy. |

### 6.4 Reduced motion and animation stability

| ID | Gate | Acceptance criterion |
|---|---|---|
| RM-01 | **B/A/M** | When Windows animation/reduced-motion preference disables animation, every route transition, drawer, hover fade, status transition, and listening visualization becomes immediate. |
| RM-02 | **B/M** | With motion enabled, hover/state opacity is about 120 ms and drawers about 180 ms with no bounce, elastic overshoot, parallax, zoom, confetti, ambient particles, or decorative infinite loops. |
| RM-03 | **B/M** | Listening motion is low amplitude and state-bound; it stops immediately after Listening. Loading animation cannot create high-frequency flicker and respects reduced motion. |
| RM-04 | **B/A/M** | Animation never controls state completion. Interrupting, navigating, sleeping, minimizing, changing DPI, or closing during a transition leaves deterministic state and focus. |

## 7. State and content stress matrix

Every applicable route/component must be captured and exercised in the following states. “Not applicable” needs a written reason.

| State | Required proof |
|---|---|
| Empty | Explains the state, offers at most one earned next action, and does not fill space with illustration, tips, fake sample data, or marketing. |
| First item | Layout does not depend on a populated list; master/detail selection and focus are deterministic. |
| Typical | Realistic English content demonstrates normal density and hierarchy. |
| Dense | 1,000 list rows or all setting sections remain responsive; virtualized/scroll behavior is stable. |
| Loading | Skeleton/spinner, if any, resembles the final geometry, announces status once, and has bounded retry/cancel where the operation permits. |
| Slow | Simulated 5-second model discovery/save/load does not freeze painting, window movement, keyboard focus, or Quit. |
| Recoverable error | Cause and next action are inline; input/draft survives; retry does not duplicate writes or downloads. |
| Fatal startup error | Native/branded surface is readable in every mode, safely summarizes the error, and exits deterministically without exposing content. |
| Offline/local service absent | Core transcription remains usable when Ollama is absent; UI states exact local fallback rather than showing total app failure. |
| Partial optional data | Missing Normalized/Cleaned/metadata variants are omitted cleanly; no `null`, blank panels, or em dash graveyard. |
| Long single token | 512-character device name, 256-character model ID, long basename candidate, URL-like transcript token, and path do not force horizontal page growth. |
| Long prose | Maximum custom instructions and multi-page transcripts wrap, scroll, select, and copy without obscuring actions or causing frame drops. |
| Unicode | English, Brazilian Portuguese diacritics, combining marks, emoji in transcript content, RTL sample content, and unsupported control characters follow their validated contracts without mojibake. UI chrome remains localized-ready even if v1 ships English. |
| Concurrent change | Device removal, Ollama stop/model eviction, history clear, target focus change, sleep/resume, and shell close during work produce safe, content-preserving outcomes. |
| Stale data | A record/profile deleted or changed between load and action reports a recoverable conflict; selection does not silently jump to unrelated data. |

## 8. Privacy and safety non-regression matrix

| ID | Gate | Invariant and proof |
|---|---|---|
| PS-01 | **B/P/A** | Speech recognition, deterministic formatting, alias replacement, and optional Ollama formatting remain local. Network inspection shows only explicit model download traffic and loopback Ollama traffic. |
| PS-02 | **B/P/A** | The shell-to-runtime boundary carries typed commands/snapshots; it does not receive audio samples, mutable database handles, raw window titles, or focused-control text. |
| PS-03 | **B/P/A** | History is governed by the user’s retained policy. Disabled means no future writes and immediate removal of existing records in the same transaction. Retention purge boundary behavior remains tested. |
| PS-04 | **B/P/A** | Persisted target identity is executable basename only. Full executable paths never leave the resolver; window titles and focused content are neither persisted nor displayed. |
| PS-05 | **B/P/A/M** | Automatic/Direct insertion revalidates target identity immediately before injection and falls back to clipboard for unavailable, changed, unsupported/sensitive, modifiers-held, elevated, or uncertain targets. |
| PS-06 | **B/P/A** | Formatting output is accepted only after protected tokens, control-character, commentary, emptiness, and growth validation. Failure returns the exact recognizer transcript. UI copy never promises stronger rewriting than this contract allows. |
| PS-07 | **B/P/A** | Ollama remains loopback HTTP only, proxy-free, redirect-free, time-bounded, explicitly selected, and fail-open to deterministic transcript output. |
| PS-08 | **B/P/A/M** | Model download is user-initiated, pinned, size-bounded, cancellable, checksum-verified, and atomic. Partial downloads are removed without destroying an existing verified artifact. |
| PS-09 | **B/P/A** | Default diagnostics exclude audio, transcripts, formatted results, clipboard content, window titles, focused text, custom instructions, model names, prompts, raw logs, environment variables, usernames, machine names, and filesystem paths. UI error reporting does not reintroduce them. |
| PS-10 | **B/P/M** | Clipboard confirmations and errors never echo copied transcript content. Clipboard restoration policy is not silently changed by the redesign. |
| PS-11 | **B/P/A** | Destructive actions—disable history, clear history, delete alias/profile, replace model—have precise targets, safe defaults, atomic behavior where applicable, and cannot be triggered by route navigation or a stale animation callback. |
| PS-12 | **B/P/A/M** | Shell screenshots, crash handling, telemetry hooks, and accessibility announcements do not become new transcript/model-path disclosure channels. No telemetry is added as part of UI polish. |

## 9. Icon, mark, and asset tests

The icon is a functional Windows asset, not a logo pasted into a rounded square.

| ID | Gate | Acceptance criterion |
|---|---|---|
| IC-01 | **B/V** | Black-and-white silhouette is reviewed at 16, 20, 24, 32, 40, 48, 64, and 256 px at 100% display scale. The mark remains recognizable before bronze/material is added. |
| IC-02 | **B/V** | Size-specific simplification is deliberate: 16–20 px uses the outer P plus one string cut; 24–48 px uses only cuts that remain open; larger sizes may use three cuts/material planes. Downsampling alone is insufficient. |
| IC-03 | **B/V** | At 16 px, no string closes, no interior counter fills, stem remains at least one clear device pixel, and the asymmetric cut survives. Inspect with nearest-neighbor enlargement and at actual size. |
| IC-04 | **B/V** | Mark is tested on white, black, Windows light/dark taskbars, accent-colored taskbars, selected desktop tile, Start search, Alt+Tab, title bar, tray overflow, and installer/uninstaller surfaces. |
| IC-05 | **B/V** | Single-color tray variants remain legible in light, dark, and both high-contrast modes. The tray icon does not depend on bronze, gradients, transparency subtleties, or three tiny strings. |
| IC-06 | **B/V** | ICO contains verified multi-resolution raster entries (at minimum 16, 20/24 where supported, 32, 40/48, 64, 128, 256) with correct alpha; executable, shortcuts, installer, uninstaller, and Add/Remove Programs use the intended assets. |
| IC-07 | **B/V** | No accidental resemblance dominates: test against microphone, podcast, play, Pinterest-like P, shield, Spartan helmet, lambda, and waveform icon sets. At least 4 of 5 reviewers do not choose one of those as the primary reading. |
| IC-08 | **B/V** | Wordmark optical spacing is inspected at title-bar and onboarding sizes. Modified P and X remain readable; no pseudo-Greek substitution or novelty inscription styling appears. |
| IC-09 | **B/A** | Source vector is the single master, deterministic export is documented, ICO/PNG assets are reproducible, and release checks fail on missing/stale assets. |

## 10. Screenshot and physical-review checklist

### 10.1 Required canonical captures

Capture the following at 1120×760 logical px in dark mode at 100% and 150%. Repeat the marked accessibility set in light, High Contrast Black, High Contrast White, and 200% text size.

- Shell: Ready on Home; active Listening; Needs attention; narrow minimum window; maximized ultrawide.
- Home: history disabled; three recent dictations; Whisper missing; Ollama unavailable with deterministic fallback.
- History: empty; typical list/detail; each available variant; metadata rail; 1,000-row dense list; long transcript; clear confirmation; load error.
- Lexicon: empty; populated table; new editor; editing disabled entry; validation error; delete confirmation; long/Unicode values.
- Profiles: empty; typical policy; deny profile; Custom formatting; validation error; delete confirmation; long basename/instructions.
- Models: Whisper verified; download consent; active download; cancelled download; checksum failure; Ollama absent; no models; selected ready; selected missing; long model identifier.
- Settings: each section; Custom formatting disclosure; missing microphone; dirty navigation; save validation error; save/restart result.
- Commissioning: all six steps; skipped optional Ollama; microphone unavailable; model download; test dictation result; interrupted/resumed flow.
- Overlay: every fixed state over light text, dark text, busy image, full-screen app, and mixed-DPI secondary monitor.
- Tray/icon: normal/overflow menu, all status tooltips, taskbar, Start search, Alt+Tab, desktop shortcut, installer, uninstaller, and high-contrast variants.

### 10.2 Screenshot review questions

Each screenshot must be reviewed at actual size and 200% zoom:

1. What is the single first visual read? Is that the page’s intended job?
2. Which element could be removed without reducing comprehension or capability? Remove or justify it.
3. Could this screenshot belong to a generic settings/SaaS product after changing the logo and accent? If yes, fail it.
4. Does any decorative treatment compete with transcript content or operational state?
5. Is bronze expressing active tension/focus, or merely decorating the page?
6. Are destructive, failure, readiness, selected, focused, and disabled meanings distinct without color?
7. Are alignment and spacing optically balanced at the edges, not only numerically token-correct?
8. Is all copy exact, sparse, and calm? Could any sentence be shorter without becoming vague?
9. Does the route preserve every parity item assigned to it?
10. Does the screenshot expose content or metadata that the existing privacy contract would withhold?

### 10.3 Physical Windows pass

- Windows 10 supported baseline and current Windows 11.
- Keyboard-only and screen-reader smoke pass.
- Mouse, precision touchpad, touch if supported, and 200% text size.
- Single monitor at 100%, single monitor at 200%, and mixed 100%/150% or 100%/200% monitors.
- Dark, light, High Contrast Black, and High Contrast White.
- Sleep/resume while idle, listening, transcribing, refining, downloading, saving, and with a drawer open.
- Explorer restart, microphone unplug/replug, Ollama stop/eviction, target focus change, elevated target, and clipboard race.
- Installer, first run as a non-developer, launch at login, update/reinstall, and uninstall.

## 11. Explicit generic-SaaS failure patterns

Any one of the following requires redesign before visual acceptance, even if implementation is correct:

1. **Card confetti:** every setting or status lives in its own rounded rectangle.
2. **Dashboard reflex:** Home accumulates stats, streaks, “time saved,” charts, greetings, or recent-activity widgets because the canvas looked empty.
3. **Bronze theming:** generic controls are declared branded solely because borders, selected tabs, and icons are bronze.
4. **Pill inflation:** navigation, buttons, tags, status, filters, and inputs all become capsules.
5. **Gradient AI theater:** neon waveform, purple/blue/bronze glow, glass panels, or aurora backgrounds signal “AI.”
6. **Greek costume:** helmet, shield, laurel, column, marble, red cape, pseudo-Greek characters, or inscription typography replaces actual identity thinking.
7. **Logo wallpaper:** oversized mark/watermark is used to make empty layouts feel authored.
8. **Copy theater:** “Welcome,” “Unlock,” “Elevate,” “Supercharge,” “Magic,” “Powerful AI,” “You’re all set!”, exclamation marks, or congratulatory save messages.
9. **Badge taxonomy:** every local/ready/model/mode/value becomes a colored badge instead of readable hierarchy.
10. **Icon-library collage:** unrelated line icons, filled icons, emoji, and platform glyphs coexist without optical normalization.
11. **Settings dump in disguise:** a sidebar plus the same full dense form is called a unified product experience.
12. **Modal reflex:** validation, save result, missing Ollama, and child-route failures all interrupt with dialogs.
13. **Empty-state illustration:** decorative art and verbose coaching replace a precise sentence and one earned action.
14. **Animation as personality:** drawers bounce, route content slides dramatically, listening pulses continuously, or hover motion attracts attention at rest.
15. **Luxury cosplay:** excessive spacing, low-contrast tiny text, vague labels, and hidden controls masquerade as premium minimalism.
16. **Developer-console drift:** raw model IDs, paths, timing dumps, SQL/errors, and technical toggles dominate ordinary routes.
17. **Faux-native chrome:** a custom title bar looks distinctive but loses snap, system menu, resizing, DPI, or keyboard behavior.
18. **Privacy by slogan:** `Local only` is repeated decoratively while new UI logs, previews, accessibility announcements, or diagnostics leak content.
19. **Feature burial:** infrequent but important controls disappear into unsearchable “Advanced” drawers for visual cleanliness.
20. **Prototype polish:** the happy path is beautiful while empty, error, loading, long-content, high-contrast, and keyboard states fall back to framework defaults.

## 12. Release decision

Phase 5 UI is accepted only when all of the following are true:

- Every `B` row has evidence tied to the release candidate hash.
- Automated domain/runtime tests remain green and include new typed shell boundaries.
- Route parity is signed off independently from visual review; a beautiful regression does not pass.
- The canonical screenshot set contains no unresolved generic-SaaS failure pattern.
- Physical Windows keyboard, DPI, contrast, motion, lifecycle, and compatibility passes are complete.
- The 16–48 px mark succeeds in monochrome before application mockups are considered.
- Privacy/safety review confirms no new content-bearing logs, diagnostics, UI channels, network paths, or unsafe insertion behavior.
- The final user review answers both questions affirmatively: **“Does this feel unmistakably like Phorminx?”** and **“Does it make the existing product clearer without making it less trustworthy?”**

“100% complete” is not claimed from unit tests or attractive screenshots alone. The release candidate must pass the full behavior, vibe, accessibility, asset, privacy, and physical-Windows matrix above.
