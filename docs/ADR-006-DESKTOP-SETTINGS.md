# ADR-006: Native desktop shell and versioned settings

## Decision

Phorminx uses a small native Win32 tray thread rather than carrying a browser or general-purpose GUI runtime in the always-resident process. The tray owns only its window, icon, menu, and status text. It emits `OpenSettings` and `QuitRequested`; the application shell retains lifecycle policy and the dictation runtime remains unaware of tray mechanics.

The tray icon is registered with notification-area version 4 semantics, restored after an Explorer restart, and removed before its owner window exits. Quit is prioritized ahead of simultaneous hotkey and transcription events. Central teardown drops an active recording before joining the transcription, tray, and overlay services, and attempts every shutdown even if an earlier one reports an error.

Machine-specific settings live at `%LOCALAPPDATA%\Phorminx\settings.toml`. The schema starts at version 1, rejects unknown fields and future versions, caps input at 64 KiB, and preserves malformed files rather than replacing them. Saves use a fully flushed same-directory temporary file and atomic Windows replacement.

Formatting strengths are persisted as `raw`, `light`, `balanced`, `strong`, or `custom`. `light` is the compatibility default and performs only the existing deterministic spacing cleanup. `raw` preserves recognizer output. The remaining strengths are reserved for the local Ollama phase and fail explicitly until that pipeline is available.

Relative model paths from a settings file are based on the settings directory. Explicit CLI paths retain current-directory semantics. CLI overrides never write themselves back.

## Consequences

- The resident shell stays small and has no WebView or GPU UI dependency.
- Explorer restart and tray-menu shutdown are normal lifecycle cases rather than recovery edge cases.
- Settings vocabulary is stable before the settings window and Ollama implementation arrive.
- Balanced, Strong, and Custom cannot silently degrade to Light; users receive an unsupported-capability error until local AI formatting is implemented.
