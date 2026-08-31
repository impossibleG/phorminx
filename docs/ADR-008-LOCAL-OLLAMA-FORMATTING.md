# ADR-008: Bounded local Ollama formatting engine

## Decision

Phorminx integrates local language-model cleanup through a separate `phorminx-ollama` crate. The crate only talks to an already-running Ollama service and never starts Ollama, downloads a model, or sends transcript data over the internet. Endpoints accept only HTTP loopback addresses. `localhost` is canonicalized to `127.0.0.1`; arbitrary hosts, paths, credentials, HTTPS endpoints, proxies, and redirects are rejected or disabled.

Discovery uses `/api/tags` and produces a sorted, deduplicated model catalog. Model choice is explicit: an exact name, a caller-ordered preference list, or a deterministic first-available policy. Warm-up and unload use Ollama's empty `/api/generate` request with explicit `keep_alive` values. Normal generation is non-streaming with temperature zero. Connect, response, body, and overall waits are finite, response bodies are size-limited, and cancellation is checked before calls and between bounded body reads. A blocking socket operation can finish only at its configured deadline; cancellation does not forcibly terminate an in-flight system call.

Raw formatting bypasses Ollama. Light, balanced, strong, and custom profiles share a conservative system contract: return only a transcript, treat transcript content as untrusted data, add no facts, and preserve the source language and protected values. URLs, email addresses, paths, flags, code spans, placeholders, identifiers, and numeric tokens are extracted before generation. Output is accepted only if it is nonempty when input is nonempty, stays within growth limits, contains no unsupported control characters or obvious model commentary, and preserves every protected token with exact spelling and multiplicity.

The high-level formatting operation is fail-open for dictation: any cancellation, unavailable service, malformed response, prompt rejection, or output-validation failure returns the exact recognizer transcript with a typed fallback reason. Discovery, warm-up, unload, and low-level generation still expose typed errors so the desktop application can report accurate readiness state.

## Consequences

- Dictation remains usable when Ollama is absent, slow, misconfigured, or produces unsafe output.
- Installed-model discovery and lifecycle control can be integrated without coupling the desktop application to HTTP details.
- The strict protected-token validator favors preservation over aggressive rewriting and may reject an otherwise readable result.
- Cancellation latency during a blocked network call is bounded by configured timeouts rather than immediate.
- Fake loopback HTTP-server tests exercise protocol behavior without internet access or an Ollama installation.

