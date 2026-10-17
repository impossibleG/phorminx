# Phorminx UI migration audit

This is a source-derived inventory, **not** a completed visual audit. Real
Windows screenshots are required before the final HTML design target is frozen.
Native-window screenshot capture is currently unavailable in this QA
environment. Do not mark a screen visually approved from a synthetic gallery
render alone.

The localhost browser render of the new Tauri HTML has been inspected for the
empty companion, Live, Saved, and Meeting settings layouts. Transcript and
Assistant view switching was checked there. This confirms CSS and DOM only;
the actual native companion, microphones, translation, commentary, and meeting
save paths still require live QA before the old shell can be retired.

The opt-in product shell now builds with Tauri routes and command bridges for
history/search, meetings, actions, lexicon, profiles, models, setup, settings,
and a separate feedback window. This is implementation progress, **not** a
visual or functional sign-off. The current installed build remains on the
legacy shell; switching the default and installer is blocked until native
window, keyboard, microphone, translation, assistant, setup, and persistence
QA completes. The local-file browser preview is disallowed by browser policy
in this environment, and no alternate browser route should be used to bypass
that restriction.

Native QA on 2026-10-09 verified that the isolated product executable opens
`product.html`, loads the Home snapshot through the app command bridge, and
reports Whisper ready on Vulkan with the desktop/Vulkan/Tauri feature set.
The first native run exposed two integration defects: the product context
loaded the architecture-preview config, and the host plugin commands were
denied by Tauri IPC. Both were corrected and retested natively. The 352 app
tests and 85 runtime tests pass with the production feature combination.
Windows screenshot capture fails at `SetIsBorderRequired` (0x80004002), and
the computer-use accessibility helper cannot obtain click geometry for
WebView2 controls. Therefore route interactions, visual parity, and live
meeting behavior are **not** signed off. The isolated QA profile was removed
and the installed legacy app restored after testing.

## Product routes

| Route | Important states to capture and verify |
| --- | --- |
| Home | Ready, listening, error, recent dictations, compact window |
| History | Empty, list, selected detail, long text, search, deletion confirmation |
| Meetings / Live | Idle, listening, partial/final transcript, translation pending/ready/error, assistant busy/answer/error |
| Meetings / Saved | Empty, list, selected session, search, long transcript, deletion confirmation |
| Meetings / Settings | Model/device options, absent device, invalid model, saved/unsaved state |
| Actions | Empty, list, action editor, template and AI-JSON modes, send/review/error |
| Setup | Missing prerequisites, installation progress, interrupted/retry, complete |
| Lexicon | Empty, list, edit/new, validation error |
| Profiles | Empty, list, edit/new, validation error |
| Models | Installed/missing/download progress, activation, readiness error |
| Settings | Shortcut, dictation, formatting, appearance, privacy, light/dark |

## Separate surfaces

- Launcher: idle, numbered actions, feedback, compact/edge-of-screen placement.
- Meeting companion: transcript, together, assistant; original, translation,
  both; wide and compact; mic/computer mute; commentary; long conversation;
  network/model error; hide/finish; light and dark.
- Feedback: capture, review, send pending, delivery success/failure.
- Update and model dialogs: confirmation, progress, failure, retry.
- Installer/bootstrap: prepare, progress, missing dependency, repair, complete.

For each surface, compare the final Tauri window with its approved HTML target
at normal and compact sizes, then separately verify every action against the
real Rust engine. UI parity is not proof of functional parity, or vice versa.
