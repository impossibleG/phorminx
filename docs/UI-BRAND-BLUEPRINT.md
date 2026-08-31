# Phorminx unified UI and brand blueprint

Status: approved direction, implementation not started

Date: 2026-08-31

## 1. North star

Phorminx should feel laconic, exact, and privately powerful. It does not decorate itself to prove that it is premium; it assumes competence and removes everything that does not earn its place.

The visual idea is not “Spartan warrior software.” Helmets, red capes, faux-marble columns, laurel wreaths, and cinematic Greek typography would make the product feel generic. The useful Spartan inheritance is discipline: few words, strong hierarchy, controlled force, and no indecision.

The name supplies the more distinctive metaphor. A phorminx was an ancient Greek stringed instrument used with sung or recited performance. The product likewise takes voice, preserves its meaning, and gives it a deliberate written form. Tensioned strings, a yoke, a resonant body, and the cadence of spoken language become the visual vocabulary.

Brand proposition: **Voice, disciplined.**

Internal design test: **Could this surface belong to any competent settings app?** If yes, it is not finished.

## 2. Current-product audit

| Surface | What works | What fails the Phorminx test | Decision |
|---|---|---|---|
| Tray | Correct background-app behavior and fast access | Menu items launch unrelated windows and expose the implementation structure | Keep as an entry point; every content command routes into one shell |
| Status overlay | Non-activating, useful, and operationally clear | Generic geometry and no relationship to the product identity | Retain the behavior; redesign as the smallest expression of the system |
| Settings | Contains the real controls and validates them safely | Dense Win32 form, weak hierarchy, every option has equal visual weight | Rebuild as grouped routes inside the shell |
| History | Preserves raw and selected output | Record-at-a-time utility window feels diagnostic rather than deliberate | Rebuild as a searchable timeline with a detail inspector |
| Personal lexicon | Complete CRUD and scope controls | Navigation is mechanical and creation/editing are not visually distinct | Rebuild as a table plus persistent editor panel |
| Application profiles | Useful per-executable policy | Another isolated form; relationships between app, formatting, language, and insertion are difficult to scan | Rebuild as a master/detail policy workspace |
| Model setup | Verified and safe | Model path, download, Ollama discovery, and lifecycle are scattered among settings | Give Models a first-class route with explicit readiness states |
| First run | Recovers from missing models instead of failing | It is settings opened under pressure, not an intentional introduction | Rebuild as a short commissioning flow inside the same shell |
| Fatal errors | Honest and visible | Default system dialog reads as a crash and repeats internal context | Reserve dialogs for unrecoverable startup failure; recover child surfaces inline |

The existing state machine, persistence, validation, safety fallbacks, and local-processing boundaries are preserved. The redesign changes presentation and navigation, not those contracts.

## 3. Identity system

### 3.1 Personality

- Quiet confidence, not luxury theater.
- Severe but not hostile.
- Technical without looking like a developer tool.
- Ancient reference expressed through proportion and metaphor, not costume.
- Sparse copy that assumes the user can make decisions.

### 3.2 Primary mark: Tensioned P

The recommended mark is a custom, almost monolithic **P** built from the silhouette of a phorminx:

- The vertical stem is one instrument arm.
- The upper bowl is the yoke and curved sound box seen as one continuous form.
- Three negative vertical cuts imply strings and an audio cadence.
- The outer silhouette has one deliberate asymmetric cut, making it recognizable before the letter is read.
- No helmet, face, microphone, quotation mark, or full shield.

At 16–20 px, the mark reduces to the outer P with one string cut. At 32 px and above, it uses three cuts. The tray asset is a single-color cutout. The application icon uses the same silhouette on an iron field with a restrained bronze edge or inner plane.

Alternative concepts to explore once, then reject or promote:

1. **Stringed Lambda:** a phorminx yoke whose strings create a negative-space lambda. Historically resonant, but it risks reading as a developer or cloud logo.
2. **Resonant Shield:** a circular field interrupted by three string lines. Strong at small sizes, but less ownable and more generically Spartan.

The Tensioned P is the working direction because it joins name and function in one metaphor and remains legible without the wordmark.

### 3.3 Wordmark

`PHORMINX` is set in a custom-spaced uppercase wordmark with a modified P matching the icon and a narrow X terminal. It appears only in the title bar, onboarding, About, and release surfaces. Ordinary navigation uses sentence case.

Avoid pseudo-Greek substitutions and inscription fonts. They turn historical reference into novelty.

### 3.4 Color tokens

The product is effectively monochrome. Bronze is tension, focus, and active energy—not decoration.

| Token | Value | Use |
|---|---:|---|
| Abyss | `#0A0C0D` | Window background |
| Iron | `#121619` | Navigation and raised work surfaces |
| Tempered | `#1B2024` | Hover, selected rows, input fields |
| Edge | `#30363B` | Hairlines and inactive controls |
| Limestone | `#E8E5DD` | Primary text and high-emphasis glyphs |
| Ash | `#A8ADB0` | Secondary text |
| Bronze | `#A87542` | Active route, listening state, primary action |
| Bronze light | `#D0A06A` | Focus rings and small highlights |
| Oxblood | `#8B3B3E` | Destructive action and genuine failure only |
| Moss | `#71816C` | Verified/ready state when a semantic color is necessary |

Large surfaces do not use gradients. A subtle material shift may appear inside the application icon, never as a dashboard background. Every semantic state must remain understandable without color.

### 3.5 Typography

- Use Segoe UI Variable for the product UI to gain excellent Windows rendering, localization, and no extra font payload.
- Use a custom vector wordmark rather than forcing personality through a novelty font.
- Use Cascadia Mono only for executable basenames, model identifiers, shortcut chords, timings, and technical tokens.
- Use tabular numerals for latency and history metadata.

Hierarchy comes from size, weight, width, and space. Uppercase is reserved for the wordmark and tiny metadata labels; it is never used for paragraphs.

### 3.6 Geometry and material

- Base spacing unit: 4 px. Common intervals: 8, 12, 16, 24, 32, 48.
- Window corner radius follows Windows; internal surfaces use 6 px or 10 px, never indiscriminate pills.
- One-pixel separators replace most cards.
- Controls are 36 px high; primary page actions are 40 px.
- Content rests on large, quiet planes. There is no “card for every setting.”
- Icons are geometric, monoline, and optically squared, with selectively cut terminals derived from the logo.

## 4. One-window product architecture

Phorminx becomes one durable application shell. Tray commands focus that shell and navigate it; they do not create independent windows.

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│  mark  PHORMINX                                      ● Local · Ready    — □ ×│
├───────────────┬─────────────────────────────────────────────────────────────┤
│               │  Page title                                  page action   │
│  Home         │  Quiet one-line context                                    │
│  History      │─────────────────────────────────────────────────────────────│
│  Lexicon      │                                                             │
│  Profiles     │                  routed page content                        │
│  Models       │                                                             │
│  Settings     │                                                             │
│               │                                                             │
│               │                                                             │
│  Local only   │                                                             │
│  Ctrl Alt Spc │                                                             │
└───────────────┴─────────────────────────────────────────────────────────────┘
```

Recommended initial frame: 1120 × 760 logical pixels, resizable to 900 × 620, per-monitor DPI aware. The title bar is integrated but preserves standard Windows move, resize, minimize, maximize, and close behavior.

### 4.1 Home — the instrument at rest

Purpose: show that Phorminx is ready and make the next action obvious.

Emotional tone: composed readiness.

Content:

- A large but restrained `Ready` state with the shortcut immediately beneath it.
- Compact readiness line for microphone, Whisper, and optional Ollama.
- `Test dictation` as the only dominant action.
- Last three dictations only when history is enabled; otherwise one sentence explaining that history is off.
- No analytics dashboard, greeting, productivity score, tips carousel, or promotional panel.

### 4.2 History — recovered thought, not a log dump

Purpose: recover, compare, and reuse successful transcription output.

Emotional tone: editorial control.

Structure:

- Left: chronological transcript list with time, app basename, language, and a two-line selected-output preview.
- Center: selected output in a reading surface.
- Variant switch: Output / Raw / Normalized / Cleaned. Absent variants are omitted, not disabled.
- Right or collapsible metadata rail: model, timing, warning flags, insertion result.
- Persistent `Copy output`; subordinate `Copy raw`; destructive `Clear history` lives in an overflow confirmation flow.
- Search and filters appear only when record volume makes them useful.

### 4.3 Lexicon — a deliberate vocabulary

Purpose: teach exact names and replacements without implying model training.

Emotional tone: authorship and precision.

Structure:

- Filterable table: Spoken alias, Written form, Language, Scope, Case, Enabled.
- One clear `New entry` action.
- Selecting a row opens an editor drawer without leaving the route.
- Scope is expressed as `Everywhere` or an executable basename.
- A preview sentence demonstrates case behavior before save.
- Deletion is available inside the editor, never adjacent to Save.

### 4.4 Profiles — policy by application

Purpose: make contextual behavior legible.

Emotional tone: command without complexity.

Structure:

- Application list on the left, policy editor on the right.
- A profile summary reads like a sentence: `In code.exe: Light formatting · English · Clipboard only`.
- Formatting, language, and insertion preferences are separate sections.
- `Block dictation in this application` is visually isolated and explains the consequence.
- Exact executable basenames are shown in monospace; full paths and window titles are never requested.

### 4.5 Models — local machinery made understandable

Purpose: establish readiness and ownership of local models.

Emotional tone: controlled capability.

Structure:

- Whisper and Ollama are two stacked systems, not a single generic “AI” area.
- Whisper shows installed model, language capability, file verification, and an explicit download/change action.
- Ollama shows reachability, installed models, selected model, and residency mode.
- Formatting is never presented as available until the selected Ollama model is present.
- Failure copy is operational: `Ollama is not running. Dictation will use Light output.`

### 4.6 Settings — preferences, not machinery

Purpose: hold infrequent global choices.

Emotional tone: quiet specificity.

Sections:

1. Input: microphone and recording mode.
2. Recognition: language and silence threshold.
3. Formatting: default strength and custom instruction.
4. Privacy: history retention and local-data actions.
5. Startup: launch at login.
6. Advanced: model paths and diagnostics entry point.

Model acquisition and personal data management do not live here merely because they are configurable.

### 4.7 First run — commissioning

Purpose: get to one successful dictation with the fewest irreversible choices.

Emotional tone: invitation into a capable instrument, not a tutorial.

Sequence:

1. Local processing promise.
2. Microphone and shortcut check.
3. Whisper model verification or consented download.
4. Optional Ollama detection and explicit selection.
5. Test dictation with no automatic insertion.
6. Launch-at-login choice and finish.

Progress is a thin six-step index, not a celebratory wizard. Users may skip optional Ollama and launch-at-login without warning theater.

## 5. Interaction language

### Status vocabulary

- `Ready`
- `Listening`
- `Transcribing`
- `Refining`
- `Inserted`
- `Copied`
- `No speech detected`
- `Needs attention`

“Cleaning” becomes “Refining” in user-facing copy; the internal state name may remain unchanged.

### Motion

- 120 ms opacity for hover and state changes.
- 180 ms decelerated drawer transition.
- Listening uses one low-amplitude breathing ring or three tensioned-string movements.
- No bouncing, confetti, elastic overshoot, ambient particles, or decorative looping animation.
- Reduced-motion mode changes every transition to an immediate state change.

### Copy

- Short declarative sentences.
- `Saved.` rather than `Settings saved successfully!`
- `Copied. Target changed.` rather than apologetic modal prose.
- `Strong may rewrite phrasing. Names, numbers, links, and code remain protected.`
- Local processing is stated once with confidence; it is not marketed on every page.

## 6. Component inventory

The first design pass must define and test these components before assembling pages:

- Application frame and navigation item
- Page header and contextual action
- Status seal
- Primary, secondary, quiet, and destructive buttons
- Text field, number field, select, switch, checkbox, and multiline instruction field
- Segmented variant switch
- Table/list row and selected state
- Detail drawer
- Empty state
- Inline warning and recoverable error
- Model readiness row
- Shortcut chord
- Confirmation surface
- Tooltip and focus ring
- Compact overlay

Every component requires default, hover, pressed, focused, disabled, loading, error, and high-contrast behavior where applicable.

## 7. Technical direction

Use `egui`/`eframe` for one Rust-owned shell and custom-drawn product surfaces. Keep the proven Win32 modules for tray, global hotkey, target safety, insertion, startup integration, and the non-activating overlay. Do not introduce C#, WinUI, Electron, or a browser runtime.

Why:

- One GUI event loop removes the independent child-window lifecycle that caused the current crash.
- Immediate-mode custom drawing makes the identity achievable without building every control from raw Win32 messages.
- It preserves a Rust-only repository and adds modest UI memory beside the much larger resident speech model.
- Domain crates remain UI-independent and testable.

The shell communicates with the runtime through typed commands and snapshots. It never receives audio samples, raw window titles, or mutable database handles.

## 8. Proposed implementation sequence

### Slice A — identity proofs

- Produce three black-and-white mark studies at 16, 24, 32, 48, and 256 px.
- Select one through silhouette, not presentation mockups.
- Build the token sheet and core components in a static UI gallery.
- Verify dark, light/high-contrast, 100%, 150%, and 200% scaling.

Exit: the mark and components are recognizable without page content.

### Slice B — shell and routing

- Add the single application window, custom title bar, navigation, and route state.
- Route tray commands to the active page.
- Preserve one-instance behavior and bring the existing instance forward.
- Move first-run setup into the shell.

Exit: no route creates an independent content window.

### Slice C — operational pages

- Build Models and Settings first because their controls exercise the design system.
- Add inline validation and restart/reload affordances.
- Preserve existing schema migration and save semantics.

Exit: every existing setting remains available and keyboard operable.

### Slice D — data workspaces

- Build History, Lexicon, and Profiles with shared master/detail components.
- Add search only after measuring typical data volume.
- Preserve content privacy and executable-basename boundaries.

Exit: every current CRUD and recovery action exists in the unified shell.

### Slice E — overlay, assets, and finish

- Apply the mark and status language to the overlay and tray.
- Generate multi-resolution ICO and installer assets from the approved vector master.
- Complete accessibility, DPI, keyboard, reduced-motion, empty/error, and long-content passes.
- Run screenshot regression and physical Windows UI review.

Exit: Phorminx feels like one product at every scale, not a collection of technically consistent screens.

## 9. Explicit non-goals for this redesign

- No dashboard statistics or gamification.
- No cloud account, community, marketplace, or upsell surface.
- No faux-historical ornament.
- No generic glassmorphism, neon waveform gradients, oversized rounded cards, or pill-shaped everything.
- No removal or weakening of safe insertion, local fallback, or privacy controls for visual simplicity.
- No simultaneous rewrite of recognition, Ollama, or persistence internals.

## 10. Approval decisions before build

Only three aesthetic decisions should require review before implementation:

1. Approve the Tensioned P or select one alternative after small-size studies.
2. Approve bronze as the sole brand accent, with oxblood reserved for destructive/error states.
3. Approve dark-first as the authored experience while still requiring usable light and Windows high-contrast modes.

Everything else in this document can proceed as the default design direction.

## References

- The Perseus Encyclopedia describes the phorminx within the ancient Greek lyre family and records the characteristic soundbox, arms, crossbar, and equal-length strings: https://www.perseus.tufts.edu/hopper/text?doc=Perseus%3Atext%3A1999.04.0004%3Aid%3Dlyre
- The Heraklion Archaeological Museum identifies a bronze player figurine bent over a phorminx: https://ca.heraklionmuseum.gr/ca/pawtucket/index.php/Detail/objects/625
- Microsoft’s Windows app-icon guidance informs the 48 px construction grid, small-size simplification, restrained layers, contrast, and multi-size asset plan: https://learn.microsoft.com/en-us/windows/apps/design/iconography/app-icon-design
