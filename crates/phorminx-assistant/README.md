# User-triggered assistant transports

This crate supplies transport and configuration only. It does not record audio, run
tools, retrieve memories, select transcript boundaries, or choose a provider on the
user's behalf. The application owns conversation state and generation IDs.

## Chat contract

`stream_chat(config, messages, cancellation, on_delta)` is blocking and belongs on a
worker. A successful result requires an explicit, successful terminal event and a
nonempty answer. Partial text remains useful after an error, but must be labelled
incomplete, never complete. No retries or cross-provider fallback occur.

Supported protocols:

- Ollama `/api/chat` NDJSON. Before every request, `/api/show` receives only the model
  identifier and verifies local completion capability. Cloud suffixes and reported
  remote aliases are rejected before conversation text is submitted. Endpoints must
  use literal loopback IP addresses, not DNS names.
- OpenAI `https://api.openai.com/v1/responses` SSE. Explicit model identifiers only;
  `store: false`, no hosted tools, no server-side conversation or response ID reuse.
  This does **not** claim zero provider retention or waive provider data policies.
- Anthropic `https://api.anthropic.com/v1/messages` SSE. System instructions are
  separated from user/assistant turns, and max-token/incomplete stops are errors.

Cancellation shuts down connected TCP sockets beneath ureq's existing rustls TLS
layer, including a socket blocked waiting for response headers or data. DNS and TCP
connect attempts are separately bounded by ten-second timeouts. The host must still
discard obsolete generation events: a callback already delivered before cancellation
cannot be recalled. Closing a connection is not a provider billing or compute-stop
guarantee.

Network policy disables proxies, redirects and connection pooling. TLS verification
is not weakened. No errors include bodies, prompts, keys or URLs. Secret HTTP headers
are marked sensitive; application logs must not explicitly print submitted values.

Transport bounds: 512 KiB input text, 1,024 messages, 512 KiB generated text, 256 KiB
per event/line, 16 MiB total streamed bytes. These are allocation protections, **not**
a model-context guarantee. The host must manage context explicitly; the crate never
silently truncates conversation text. HTTP operations have bounded timeouts; these
do not impose a recording-duration limit.

## HTTP actions

`execute_action` makes one explicit attempt. It sends a stable `Idempotency-Key` that
the caller must retain for deliberate retries. Duplicate prevention depends on the
receiving service implementing that header. A connection loss is labelled uncertain
because the server may already have accepted the note. A received non-success status
is reported without exposing the response body or automatically retrying.

POST, PUT, PATCH and DELETE send JSON. The template is parsed before replacing
`{{text}}` and `{{delivery_id}}` inside string values, so dictated quotes and token-like
text cannot alter JSON structure or execute further template substitutions. Expansion
is bounded before large allocations. GET sends percent-encoded `text` and
`delivery_id` query parameters; it is unsuitable for sensitive notes because URLs
may appear in destination logs. HTTPS is required except for literal loopback HTTP.
Credentials belong in encrypted headers, never URL query strings or payload literals.

## Storage

`ProtectedSecret` uses current-account Windows DPAPI plus application entropy, without
machine-wide scope. Configuration serialization contains ciphertext only. Credentials
fail closed on unsupported platforms or another account. This protects persisted
credentials, not a compromised account or arbitrary memory inspection. Temporary
exposed buffers are overwritten on drop, but the HTTP stack may own credential copies
for the request lifetime.

`ConfigStore` validates versioned configuration, reads at most 4 MiB, and replaces
configuration through a same-directory temporary file after flushing/syncing it.
Corrupt settings are not silently reset. Credential replacement and removal must be
explicit in the host UI; failed saves must retain entered drafts for correction.

## Verification

`cargo test -p phorminx-assistant` uses only in-memory fixtures, temporary directories
and synthetic loopback servers. It covers cancellation while headers/body stall,
local-model preflight and cloud alias rejection, stream completion/truncation,
credential protection/redacted debugging, JSON injection/expansion, redirects,
uncertain delivery, and atomic save rejection. Cloud schemas have fixture coverage;
no paid calls or actual cloud credentials were used during development.

Protocol references inspected during implementation:

- https://docs.ollama.com/api/chat
- https://developers.openai.com/api/reference/typescript/resources/responses/methods/create
- https://platform.claude.com/docs/en/build-with-claude/streaming

ureq is pinned because the cancellable TCP connector uses its explicitly unversioned
transport extension API; upgrading it requires rerunning the cancellation fixtures.
