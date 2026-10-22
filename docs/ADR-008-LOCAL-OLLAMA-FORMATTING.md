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

## Amendment (October 2026): formatting that actually reorganizes

Custom formatting rarely changed anything visible. A formatting bench
(`phorminx-bench format-bench --model <tag> --custom "<instructions>"`) runs
synthetic dictations through every profile against a local model and prints
the raw model output, the validator verdict and the inserted text. It showed
four separate causes:

- The runtime passed every AI result through the spoken-text normalizer,
  which joins all whitespace into one line. Lists and paragraphs were
  flattened before insertion. AI output now uses a layout-preserving
  normalizer (`normalize_formatted`): line breaks, one blank line between
  paragraphs and list indentation survive.
- The desktop client allowed 5 seconds overall and 4 seconds for response
  headers. Generation is not streamed, so headers arrive after the whole
  answer, and a cold 7B load alone takes about 10 seconds. Formatting
  requests now allow 45 seconds for headers (60 overall), discovery and
  identity probes keep the short limits, and the request runs on a helper
  thread so cancellation is observed while waiting.
- Plain numbers are compared by value: "12500" may become "$12,500" and "14"
  may become "14th", but not "12,000" or "fourteen". List markers the model
  adds, Markdown links and bold around a protected value are not counted as
  changes. Versions, times, URLs and identifiers still match exactly.
- The prompt listed protected values as "value (N occurrence(s))" lines and
  framed the transcript with BEGIN/END markers; small models copied both into
  the answer. The transcript is now wrapped in `<transcript>` tags, the
  values are listed inline in the system prompt, output that echoes the
  scaffolding is rejected as commentary, and the prompt states that the reply
  must stay in the dictation's language.

A **Structured** profile reorganizes the dictation into Markdown lists,
headings and short paragraphs. Structured and Custom may restructure, so
they allow more growth (4x + 1 KiB). Bench results on one machine:
qwen2.5:7b-instruct formatted 30/30 cases (23/30 before, with generous
timeouts); qwen2.5:1.5b passed validation but returned plain prose for
Structured and Custom, so the Models page recommends a 7B-class model for
those profiles.

When formatting falls back, the reason is stored with the dictation
(`ollama_fallback:*`) and shown as a short note in the status pill, in
History and in the review panel.
