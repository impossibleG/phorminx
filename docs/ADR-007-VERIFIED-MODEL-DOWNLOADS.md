# ADR-007: Explicit, verified Whisper model downloads

## Decision

Phorminx embeds the versioned model manifest from `config/model-manifest.json` in the application binary. The Settings window offers a download only after an explicit user click and identifies the pinned recommended English model and its approximate size. Phorminx does not silently pull model weights during startup.

The downloader uses HTTPS with bounded connection, response, and body-read timeouts. It streams into a uniquely created same-directory temporary file, maintains a running SHA-256 digest, caps the response at the manifest byte count, and reports percent progress without retaining the response in memory. Both exact byte count and SHA-256 must match the embedded manifest. Only then is the flushed temporary file atomically moved into the per-user model directory.

Cancellation is checked between bounded reads. Closing Settings or quitting Phorminx cancels and joins the worker, and every failure removes the temporary artifact while preserving any previously verified model. A successful download updates the model path in the still-open form; the user then confirms it through the normal Save and Restart flow.

## Consequences

- A compromised mirror, truncated transfer, error page, or unexpected artifact cannot become the active model merely because a request succeeded.
- Model weights remain separate from Git and the installer.
- The downloader adds a Rustls HTTPS dependency to the application binary, but it allocates its transfer buffer and client only for an explicit download.
- Future multilingual recommendations can extend the manifest and UI without changing the verification boundary.
