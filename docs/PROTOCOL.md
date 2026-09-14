# LibreSync wire protocol

This document specifies the device-to-device protocol implemented in
`crates/libresync/src/protocol.rs`, `sync.rs`, `crypto.rs` and
`discovery.rs`, in enough detail for a second implementation. Protocol
version **2** is current; version 1 is the 0.5-and-earlier exchange that version 2
peers still accept.

## 1. Transport

- TCP. Default listener port `52345` (`DEFAULT_SYNC_PORT`); any port may be
  advertised.
- One request/response conversation per TCP connection; the initiator opens
  the connection, sends the first message, and closes after the final
  message. The listener may close on any protocol error.
- Timeouts: implementations use 5 s connect and 5 s read/write timeouts by
  default; peers must tolerate slower peers by not depending on shorter values.

### 1.1 TLS

- TLS 1.2 or 1.3 (rustls 0.23 "safe default" versions and cipher suites) with
  **mutual authentication**. Both sides present exactly one self-signed X.509
  certificate: the device certificate generated at first run (`DeviceKeys`).
- Certificate contents: `CN = device_id`, `O = app_id`, `OU = user_id`,
  SANs `DNS:<device_id>` and `email:<app_id>`. Peers must not rely on these
  fields for trust; they are informational.
- Signature schemes: those of the ring provider (ECDSA P-256/P-384 SHA-256/384,
  Ed25519, RSA-PSS, RSA-PKCS1). Generated certificates use ECDSA P-256.
- Server name (SNI): the expected peer `device_id` when it is a valid DNS
  name, otherwise `libresync.local`. Listeners ignore SNI.
- **Trust rules.** There is no CA. Trust is bound to the SHA-256 fingerprint
  of the peer's leaf certificate DER, lowercase hex, 64 characters.
  - *Link*: trust on first use. The listener accepts any client certificate
    for the handshake, then asks its `DeviceHandler` whether to approve the
    identity+fingerprint pair. The client accepts any server certificate and
    receives the server's fingerprint to pin.
  - *Sync*: both sides bind the identity claimed in `Hello` to the
    fingerprint of the certificate presented in the handshake and must
    reject the connection if that device is linked with a different
    fingerprint (`Error::FingerprintMismatch`, `Event::FingerprintChanged`).
    A client that already knows the peer's fingerprint should pin it in the
    TLS verifier so the handshake fails before any application data is
    written. A fingerprint change is never accepted automatically.
- After the handshake, the peer certificate used for fingerprinting is the
  first certificate of the presented chain (the only one).

### 1.2 Framing

Messages are UTF-8 JSON objects, one per line, terminated by `\n` (`\r\n` is
tolerated on input). A message must not contain raw newlines. There is no
length prefix; readers use line-based reads. A single message must not exceed
`MAX_MESSAGE_BYTES` (256 MiB including the newline); readers abort the
connection with a `Protocol` error once that many bytes arrive without a
newline, so an unauthenticated peer cannot force unbounded allocation before
the `Hello` checks run. Unknown JSON fields must be ignored; missing optional
fields take their defaults.

Every message has the shape

```json
{"type": "<Variant>", "payload": { ... }}
```

Unit variants (`SnapshotRequest`, `Ack`) omit `payload`.

## 2. Data types

### 2.1 Identity

```json
{"device_id": "amber-river-summit", "app_id": "com.example.notes", "user_id": "calm-forest"}
```

`app_id` scopes trust and discovery: peers with a different `app_id` are
rejected at every step.

### 2.2 LamportClock

```json
{"counter": 42, "device_id": "amber-river-summit"}
```

Ordered by `counter`, then by `device_id` (byte-wise). Higher wins.

### 2.3 Entry

```json
{"key": "record:ns:schema:Todo:1", "value": [1, 17, 42, ...], "clock": {"counter": 42, "device_id": "d"}}
```

- `key`: UTF-8 string. Logical records use
  `record:<namespace>:<schema>:<entity>:<id>` with each component
  percent-escaped (bytes outside `A-Z a-z 0-9 - _ . ~` become `%XX`).
- `value`: byte array encoded as a JSON array of integers 0–255. On the wire
  the value is **always encrypted** (section 4); the plaintext value of a
  record entry is the JSON payload
  `{"fields":{...},"tombstone":false,"updated_at":123,"field_clocks":{...}}`.
- `clock`: the entry's Lamport clock.

### 2.4 Cursor (`clock` + `epoch`)

A cursor identifies a position in a peer's local *apply sequence*: the peer
increments a counter every time it writes an entry, and `epoch` is a random
string chosen when the peer's state was created. A cursor is only meaningful
to the peer that issued it. The pair `(epoch, clock)` with `clock = 0`, an
unknown `epoch`, or `clock` ahead of the issuer's current sequence means
"unknown" and yields a full snapshot.

## 3. Messages

| `type` | Direction | Payload |
| --- | --- | --- |
| `Hello` | both | `identity: Identity`, `protocol_version: u32` (absent → 1) |
| `LinkRequest` | client → listener | `identity`, `app_key: [u8;32]?`, `pairing_secret: string?` |
| `LinkResponse` | listener → client | `identity`, `accepted: bool`, `app_key: [u8;32]?` |
| `SnapshotSince` | client → listener | `clock: u64`, `epoch: string` (v2) |
| `Delta` | both | `entries: Entry[]`, `clock: u64`, `epoch: string`, `acked: u64`, `acked_epoch: string`, `full: bool` (v2) |
| `Ack` | listener → client | none |
| `SnapshotRequest` | client → listener | none (v1) |
| `Snapshot` | both | `entries: Entry[]` (v1) |

`app_key` fields are the raw 32 bytes of the shared app key as a JSON integer
array; they only ever travel inside the TLS channel during linking.

## 4. Payload encryption (`entry_aad`)

Every `Entry.value` on the wire (and in backups) is encrypted with
**XChaCha20-Poly1305** under the 32-byte app key:

```
value = 0x01 || nonce(24 bytes, random) || ciphertext_with_tag
```

The associated data binds the ciphertext to the entry's key and clock so a
ciphertext cannot be replayed under another key or clock:

```
entry_aad = u64_be(len(key)) || key
         || u64_be(clock.counter)
         || u64_be(len(clock.device_id)) || clock.device_id
```

All lengths are big-endian 64-bit byte counts. Version byte `0x01` is the
only defined entry encryption version; receivers must reject others.
Decryption failure (wrong app key, tampering) aborts the exchange.

State files and backups use the same construction (`encrypt_blob`) with a
fixed AAD string (`libresync-state`, snapshot metadata) instead of
`entry_aad`.

## 5. Handshake sequences

### 5.1 Link (trust on first use)

```
client                                   listener
  |--- TLS (mutual, any certs) ----------->|
  |--- LinkRequest{identity, app_key, ps}->|  check app_id, pairing secret,
  |                                        |  ask handler: approve(identity, client_fp)
  |<-- LinkResponse{identity, accepted, k}-|  accepted ⇒ pin client_fp
  |  accepted ⇒ pin listener_fp, adopt k   |
```

- `pairing_secret` must equal the listener's configured secret when one is
  set; otherwise `accepted=false`.
- App key agreement: the client sends its key; if the listener accepts and its
  own key differs, the listener **adopts the client's key** and echoes it. The
  client adopts whatever key the listener returns. After linking both sides
  hold the same app key.
- A rejected link is a normal `LinkResponse{accepted:false, app_key:null}`,
  not a TLS failure.

### 5.2 Sync, protocol version 2 (single connection)

```
client (C)                                          listener (L)
  |--- TLS (mutual; C pins L_fp if known) ------------>|
  |--- Hello{C, 2} ----------------------------------->|  app_id ok? linked(C, C_fp)?
  |<-- Hello{L, 2} ------------------------------------|
  |  app_id ok? linked(L, L_fp)? (FingerprintChanged if pinned ≠ L_fp)
  |--- SnapshotSince{clock: cur_L, epoch: cur_L.epoch}->|  cur_L = C's stored cursor for L
  |                                                     |  since = resolve(cursor); full = unknown?
  |<-- Delta{entries: L.delta(since, excl C), clock: L.seq, epoch: L.epoch,
  |          acked: L.cursor_for(C).clock, acked_epoch: ..., full} --|
  |  outbound = full(acked)? C.snapshot : C.delta(acked, excl L)     (computed BEFORE merging)
  |  merge entries (origin=L); store cursor_for(L) = (epoch, clock)
  |--- Delta{entries: outbound, clock: C.seq, epoch: C.epoch, acked: cursor_for(L).clock, acked_epoch, full} -->|
  |                                                     |  merge (origin=C); store cursor_for(C)
  |<-- Ack ---------------------------------------------|
```

Rules:

1. `delta(since, excl P)` = every entry whose current version was applied
   locally after sequence `since`, **excluding** versions that were received
   from `P` itself (so nothing is echoed back). A full snapshot never
   excludes anything.
2. Each side computes its outbound delta *before* merging the inbound one and
   reports the sequence it had at that moment as `clock`; entries merged from
   the peer therefore fall after the reported cursor but are excluded by
   origin.
3. Merging applies each entry through the adapter that owns its key
   (field-level policies for records); unowned keys use last-writer-wins on
   the clock. The merge is idempotent: re-sending entries is harmless.
4. `Ack` confirms the listener merged the client's `Delta`. Only after `Ack`
   may the client consider the listener's `acked` cursor advanced.
5. Any unexpected message type aborts the connection.

### 5.3 Sync, protocol version 1 (legacy, two connections)

Used when either `Hello` carries `protocol_version` 1 (or none).

Push: `Hello` ⇄ `Hello`, then client sends `Snapshot{entries}` with its full
state; listener merges and answers `Ack`.
Pull: on a new connection, `Hello` ⇄ `Hello`, client sends
`SnapshotRequest`, listener answers `Snapshot{entries}` with its full state.
The client checks that identity and fingerprint match the push connection.

Version 2 listeners must keep answering `Snapshot` and `SnapshotRequest`;
version 2 clients must fall back to this sequence when the listener's `Hello`
reports version 1.

## 6. Discovery (mDNS)

- Service type: `_libresync._tcp.local.`
- Instance name: the `device_id`; host name `<device_id>.local.`; port: the
  listener port; addresses: all non-loopback interface addresses (or the
  bound address when not unspecified).
- TXT keys (all required): `app_id`, `device_id`, `user_id`.
- Browsers ignore records whose `app_id` differs and prefer IPv4, then
  global IPv6, then link-local IPv6 addresses.
- Discovery is unauthenticated and only yields candidates; linking is the
  trust gate. Overlay peers (Tailscale, `LIBRESYNC_OVERLAY_PEERS`) are
  additional candidates with synthesized identities and no TXT data.

## 7. Errors and edge cases

- App id mismatch in `Hello` or `LinkRequest`: reject (`Protocol("app id
  mismatch")`), no further messages.
- Unlinked device in `Hello`: reject (`Protocol("device not linked")`).
- Linked device with a different fingerprint: reject with
  `FingerprintMismatch`; emit `FingerprintChanged`; never re-pin silently.
- Decrypt failure: `Crypto` error, abort.
- Malformed JSON, unknown `type`, EOF before a complete line, or a line longer
  than `MAX_MESSAGE_BYTES`: `Protocol` or `Serde` error, abort.
- Cancellation: a client may close the socket at any point; the listener
  must treat a truncated exchange as if it never happened (no cursor update
  without `Ack`).

## 8. Versioning

`PROTOCOL_VERSION` is advertised in `Hello`. A peer must use the lower of
the two advertised versions. Adding message fields is backwards compatible
(unknown fields ignored, missing fields defaulted); new message types or
changed semantics require a version bump.
