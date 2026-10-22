# ADR-018: Peer-to-peer messaging over Tor onion services

## Status

Accepted (alpha). Text only.

## Decision

Two Phorminx users add each other with a one-time invite string and then
send text messages directly, from the app or from the launcher. Transport is
Tor onion services through Arti, the Tor Project's Rust implementation,
embedded in-process. There is no Phorminx server, mailbox or relay; neither
side learns the other's IP address. Messaging is opt-in: until the user turns
it on, no Tor connection or listener exists.

## Design

- **Crate.** `phorminx-messaging` runs the engine on its own tokio runtime in
  a background thread and exposes a synchronous API (`Messenger`) plus an
  event callback. The network sits behind the `Transport` trait:
  `TorTransport` in the app (`tor` feature), `MemoryTransport` in tests.
- **Identity.** An Ed25519 key is created when messaging is first enabled,
  together with a display name. The private key is stored only as DPAPI
  ciphertext (`ProtectedBytes`, entropy tag `Phorminx/messaging-identity/v1`)
  and is never displayed.
- **Addresses.** A permanent *contacts* onion service runs only while the
  user has unblocked contacts. It uses Arti's restricted discovery: during
  the invite exchange each side generates an x25519 client key for the other
  and authorizes its public half, so only current contacts can even fetch
  the service descriptor. Blocking or removing a contact withdraws the key.
  Each *Copy my invite* hands out a fresh invite onion service that is
  destroyed, keys included, once it is used or after 24 hours. A new onion
  service can take from under a minute to several minutes to become
  reachable, so one spare invite service is published ahead of time while
  messaging is on (its stored secret is random and never shared) and is
  handed out on the next copy. Redeeming keeps retrying an unreachable
  invite for up to five minutes.
- **Invite string.** `name#` + lowercase base32 of
  `{version 1, onion address (35 bytes), 16-byte secret, expiry (u64 UTC
  seconds), 4-byte SHA-256 check}`. The name is only a hint; the real display
  name arrives in the handshake. The inviter stores only a hash of the secret.
- **Handshake.** The onion address authenticates the service. The caller
  sends `Hello`; a contacts service answers only a known, unblocked identity
  key and otherwise closes without revealing anything. The service sends its
  `Hello` and a 32-byte `Challenge`; the caller's `Proof` is an Ed25519
  signature over a domain tag, the challenge, the dialed onion address and
  its own key, so a proof cannot be relayed to another service.
- **Wire protocol v1.** A 4-byte big-endian length and a JSON frame of at most
  32 KiB; unknown fields and frame types are rejected; message text is at most
  4 KiB. Frames: `hello`, `challenge`, `proof`, `redeem_invite`,
  `invite_accepted`, `invite_rejected`, `message {id, sent_at, text}`, `ack`,
  `goodbye`. They carry no locale, host name, path or app version. Times are
  UTC milliseconds.
- **Delivery.** Messages are written to a sender-side outbox first. States
  are sending, waiting (an attempt failed; it will retry) and delivered (the
  recipient acknowledged it). Retries happen on start, on a 30 s to 15 min
  backoff, whenever the contact connects to us, and when we come online. After
  coming online the engine dials up to 20 recently seen contacts, so they
  flush what they queued while we were away. Receivers dedupe by message id
  and acknowledge duplicates again. Each contact is rate limited (burst 30,
  one more every 2 s); over the limit, messages are not acknowledged and the
  sender backs off.
- **Storage.** Additive tables in the existing database:
  `messaging_identity`, `messaging_invites`, `messaging_contacts` and
  `messaging_messages`. History stays local and can be deleted per contact.
  Arti's state and cache live in `tor/` under the app data directory.
- **App.** A Messages page (enable plus name, copy invite, add contact,
  contacts with rename, block, remove and launcher keys, conversations,
  delivery state, unread badge, delete history, network status). A tray
  notification names the sender but never shows message text. In the
  launcher, a key 3-9 can open a contacts panel with the same layout (contacts
  on 1-9, Esc closes). A contact can also be promoted to its own main-panel
  key. Choosing a contact records through the existing action-capture path;
  a review window then lets the user edit the text (Enter sends, Esc discards).

## Consequences

- A contact learns when you are online (they can reach your contacts
  service), as with any direct messenger.
- Both people need Phorminx running for a message to move. A message sent
  while the contact is away waits on the sender's computer.
- The first message to a contact costs a Tor rendezvous (tens of seconds
  cold). Later messages over the open session take about one second.
- Arti 0.47 needed a patch on Windows: `saturating-time` 0.5.0 looped forever
  while parsing the consensus, because Windows `SystemTime` has 100 ns
  resolution. See `vendor/README.md`. Restricted discovery is an experimental
  Arti feature, so Arti crates are pinned to exact versions.
- Live invites are capped at 20 and concurrent inbound connections at 64.
- Tearing down an onion service destroys its circuits, and Tor drops cells
  still queued on them. Invite services therefore wait for the redeemer to
  hang up (bounded) before they are destroyed.
- Dictated messages go through the meeting note capture. With history
  retention on, the dictation is saved like other notes.

## Measurements (one Windows 10 PC, two local instances over real Tor)

| What | Result |
| --- | --- |
| Cold bootstrap to online | 8–11 s |
| Warm bootstrap | 1.4 s |
| Invite redeemed right after creation (cold descriptor publication) | 55 s, and 578 s in one run |
| First message, cold dial with restricted discovery | 32 s |
| Message over an open session (send to delivered) | 0.7–0.8 s |
| Contact restarts, then receives a queued message | 15 s |
| Idle RAM of one Arti onion service process | ~34 MB working set |

Two-computer testing across different networks is still pending.
