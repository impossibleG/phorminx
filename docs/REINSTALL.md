# Reinstall Phorminx

"Reinstall Phorminx" (bottom of the sidebar, or the **Repair Phorminx** Start
menu shortcut, which runs `phorminx-app.exe --reinstall`) puts a fresh copy of
the current signed release in place and resets app state. Use it when
something is broken and the individual repairs did not help.

## Flow

1. The confirmation lists what is reset and what is kept. Starting it needs an
   internet connection.
2. Phorminx fetches the signed release manifest. Unlike an update, the release
   may be the installed version (`updates::verify_for_reinstall`), never an
   older one, and the rollback floor in `updates\preferences.json` is applied
   and never lowered. The installer is downloaded into `updates\packages` and
   checked against the signed size and SHA-256.
3. A reinstall is refused while a dictation or meeting recording is running or
   setup is working.
4. Phorminx creates `backups\reinstall-<UTC>\` with `manifest.json`, starts a
   hidden PowerShell helper and exits.
5. The helper waits for Phorminx to exit and checks the installer again. It
   moves the program folder aside to `Phorminx.old-<UTC>` and runs the
   installer silently into the same folder.
   - On success, it deletes the old program folder and **moves** the reset
     entries into `backups\reinstall-<UTC>\state\`.
   - On failure, it puts the old program folder back and touches no data.
   - Either way, it writes `receipt.json` and reopens Phorminx with
     `--after-reinstall`.
6. After a successful reinstall, Phorminx opens Setup & repair and runs
   first-run setup on its own. A banner offers **Restore my previous
   settings**. Restoring validates the saved files, moves the current ones
   into the backup, copies the saved ones back, installs any missing Ollama
   models they use, and restarts Phorminx.

## What is reset and what is kept

The classifier lives in `crates/phorminx-app/src/reinstall.rs` (`RESET`,
`KEEP`, `classify`). Anything not listed under `RESET` is kept.

| Reset (moved into the backup) | Kept |
|---|---|
| `settings.toml`, `assistant.json` | `phorminx.db` (+ `-wal`, `-shm`, `-journal`): history, meetings, messages, investigations, prompts |
| `settings.schema-3.backup.toml`, `performance-recommendation-rollback.toml` | `investigations\` (screenshots) |
| `setup-status.json`, `setup-downloads\`, `managed-setup-v1\`, `runtime\` | `tor\` (messaging identity keys) |
| `window-geometry.json`, `eframe-v2.ron` | `models\`, `ocr\` (setup checks their hashes again) |
| `temp\` except `temp\audio-spool` | `temp\audio-spool` (unsaved recordings) |
| `%LOCALAPPDATA%\net.phorminx.app` (WebView2 profile) | `updates\` (preferences, rollback floor), `backups\`, `logs\`, `vosk-assets-*`, unknown files |

Nothing is deleted except the moved-aside program folder after a successful
install.

## QA builds

A build with the `reinstall-qa` cargo feature
(`scripts\Build-PhorminxInstaller.ps1 -QaReinstall -AllowMissingUpdateKey`)
installs the local installer named by the `PHORMINX_QA_INSTALLER` environment
variable instead of downloading the release. That build refuses to compile
when `PHORMINX_UPDATE_PUBLIC_KEY` is set, and the script refuses to combine
`-QaReinstall` with a release key or `-RequireSignedBinary`. Never publish it.

The QA installer installs a separate app, **Phorminx QA**
(`Programs\Phorminx-QA`, its own AppId, shortcuts and startup entry), so it
never replaces or uninstalls the real Phorminx. A QA reinstall refuses to run
unless `LOCALAPPDATA` points at a throwaway folder, so it never touches the
real `%USERPROFILE%\AppData\Local\Phorminx`.
