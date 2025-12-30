# D2D Local-Only Sync Framework (Wi-Fi + Optional Tailscale/Headscale Overlay)

*A research and design document (Markdown edition)*

## Executive summary

You are proposing an **embeddable, cross-platform, device-to-device structured-data sync framework** that:

- **Auto-discovers peers** on the same local network (zero-config) and supports **manual peer add**.
- Operates with **no dependency on cloud or public internet** (direct device connections only).
- Provides **real-time sync + eventual consistency**, with **fast, resilient conflict handling**.
- Is **AGPLv3-only** and explicitly values **privacy and security as rights**.
- Starts with **iOS + macOS**, then expands broadly (Linux/Windows/Android/CLI, etc.).
- Optionally works over **Tailscale/Headscale** as a private overlay network.

This is feasible. The main engineering risks are (1) cross-platform discovery/connectivity constraints (especially iOS), (2) conflict-resolution strategy for arbitrary structured data at scale, and (3) DX (developer experience) across multiple platforms without duplicating logic.

The strongest approach is a **shared Rust core** with **thin native bindings**, a **pluggable discovery layer**, and **pluggable data adapters** (JSON/CRDT, SQLite/CRR, etc.).

---

## Problem framing and non-goals

### What this is

- A **library/framework** app developers embed to sync application data across trusted devices.
- A “local-first” sync engine: each device has a full local copy; sync reconciles changes.

### What this is not

- Not file sharing.
- Not a cloud sync service.
- Not a generic distributed database with global internet discovery.

---

## Requirements and values translated into technical constraints

### Goals (hard requirements)

1. **Zero config**
   - LAN peers are discovered automatically.
   - Manual node addition exists and requires **mutual approval**.
2. **No internet at all**
   - No third-party servers.
   - No external rendezvous, no STUN/TURN, no telemetry by default.
   - Direct peer connections only (LAN or explicitly configured overlay like Tailscale).
3. **Fast + resilient conflict handling**
   - Real-time propagation while online.
   - Eventual consistency under partitions/offline edits.
   - Deterministic convergence.

### Values (design principles)

1. **AGPLv3-only**
   - Viral copyleft is intentional; influences adoption but guarantees ecosystem openness.
2. **Privacy**
   - Minimize metadata leakage; encrypt in transit; keep everything local by default.
3. **Security**
   - Mutual auth, encryption, revocation, sane defaults, secure key storage.

---

## Feasibility assessment

### It’s feasible because:

- LAN discovery and direct connections are well-understood.
- Modern conflict-resolution (CRDTs / op logs + vector clocks) provides deterministic convergence.
- Rust can target iOS/macOS/Linux/Windows/Android, and can expose stable C ABI for bindings.

### Main practical challenges:

- **iOS LAN constraints**: Local Network permission prompts and background execution limits.
- **Network topology quirks**: Some Wi-Fi networks isolate clients (AP isolation), breaking peer-to-peer.
- **Scaling and compaction**: CRDT metadata growth; large datasets; many nodes.

### Bottom line:

Feasible, with careful scope management:

- Start with **2–5 peers**, LAN discovery, one data adapter, and strong security primitives.
- Expand to more complex adapters and topologies after core correctness is proven.

---

## Existing adjacent solutions (and why they don’t fully match)

### Nearby, but not your target:

- **Syncthing / Resilio**: decentralized *file* sync; not embeddable structured-data sync.
- **Automerge / Yjs**: excellent CRDT engines for JSON/text, but not a full P2P discovery + secure transport + multi-platform SDK.
- **Database sync products**: often cloud-mediated, proprietary, or oriented around client↔server, not pure LAN peer sync.

Your opportunity: combine **LAN discovery + secure transport + structured-data convergence** into a reusable, embeddable library under AGPL.

---

## Architecture recommendation (high level)

### Strongly recommended approach

**Shared Rust core + platform bindings**, not separate native frameworks per platform.

- Rust core provides:
  - discovery abstraction
  - transport + session management
  - auth + key management
  - sync protocol
  - storage adapters
  - conflict resolution primitives
- Native layers provide:
  - permission handling (especially iOS local network prompts)
  - lifecycle integration (foreground/background)
  - UI/UX hooks for “approve device” flows
  - platform-secure key storage integration

This avoids duplicating the hardest logic (sync correctness) across Swift/Kotlin/C#.

---

## From CLI POC to SDK-ready architecture

The CLI proved the protocol works end-to-end. The next phase is to turn the core into a **stable, embeddable SDK** and make the CLI a consumer of that SDK. The goal is to let apps import the Rust library directly (Rust) or through thin native wrappers (Swift/Kotlin), while keeping platform-specific logic at the edges.

### Layering model

1) **Rust core (libresync)**  
   - Owns protocol, sync engine, adapters, storage, crypto, networking, and discovery traits.
   - Exposes a stable API for clients (Rust API and C ABI for wrappers).

2) **Platform integration layer (Swift/Kotlin wrappers)**  
   - Handles permissions, lifecycle, and UI/UX flows (pairing prompts).
   - Bridges platform key storage (Keychain/Keystore).
   - Provides idiomatic APIs and async model to the host language.

3) **App SDK surface (high-level)**  
   - Hides protocol details and provides “sync intent” APIs.
   - Exposes events and sync state in a friendly, reactive style.

The CLI becomes a **first-class integration test** and example app that exercises the same SDK APIs a native app would use.

---

## SDK API shape (high-level)

### Core runtime objects

- `Engine`: top-level runtime owning discovery, transport, and sync tasks.
- `DeviceIdentity`: keypair + device metadata; app ID is the trust boundary.
- `PeerStore`: allowlist and last-seen info; supports revoke and reset.
- `SyncSession`: per-peer connection state and flow control.
- `DataAdapter`: pluggable adapter for JSON or SQLite (see below).
- `EventStream`: typed events (pairing requests, sync status, errors).

### Suggested Rust API (conceptual)

- `Engine::new(config, adapters, event_sink)`  
  Creates a runtime with a background task executor.
- `engine.start_listening()` / `engine.stop_listening()`
- `engine.discover_peers()` / `engine.add_peer(address)`
- `engine.request_pair(peer)` / `engine.accept_pair(peer, decision)`
- `engine.sync_now(peer)` / `engine.watch(adapter_id)`

### Event model

Expose events that app UIs can map to UX:

- `PairingRequested { peer, metadata }`
- `PairingDecisionRequired { peer, code }`
- `SyncStarted { peer, adapter }`
- `SyncProgress { peer, adapter, received, total }`
- `SyncFinished { peer, adapter, result }`
- `PeerSeen { peer, address, last_seen }`
- `PeerOffline { peer }`
- `Error { scope, error }`

For FFI, this becomes a **callback-based event stream** or **pollable queue**.

---

## FFI and native wrapper strategy

### Recommendation

Use a **C ABI boundary** with **thin language wrappers**, and keep the Rust core as the single source of truth.

Two viable tooling paths:

1) **UniFFI-based bindings**  
   - Rust: define an interface layer using UniFFI-compatible types.
   - Swift/Kotlin: generate idiomatic bindings with minimal manual glue.
   - Pros: faster iteration, less boilerplate, consistent APIs.
   - Cons: some constraints on types and async surfaces.

2) **Manual C ABI + custom wrappers**  
   - C-exported functions with explicit handles and memory management.
   - Swift: wrap in an XCFramework + Swift module map + helper types.
   - Kotlin: JNI/NDK bridge packaged in an AAR.
   - Pros: full control and stability guarantees.
   - Cons: more boilerplate; higher maintenance.

Start with UniFFI unless it blocks a required API. If it does, fall back to a manual C ABI.

### FFI design principles

- **Opaque handles** for long-lived objects (engine, adapter, peer).
- **Explicit ownership**: caller frees strings and buffers.
- **Stable enums** for event types and error codes.
- **Async surfaced as callbacks** or **pollable queues** (no cross-thread FFI calls from Rust).
- **Versioned ABI**: include a `libresync_abi_version()` function.

---

## Data adapters and app-facing abstractions

### Adapter interfaces (v1)

1) **Document adapter** (JSON-like CRDT)  
   - Primary adapter for app state, preferences, and structured lists.
   - Exposes `get_document()` / `apply_patch()` / `subscribe_changes()`.

2) **SQLite adapter** (scoped)  
   - Start with primary-keyed tables and deterministic per-column merge.
   - Expose `register_table()`, `export_changes()`, `apply_changes()`.

### App-level API (intent focused)

Rather than exposing protocol primitives, the SDK should provide:

- `sync.enable("notes")` / `sync.disable("notes")`
- `sync.syncNow("notes")`
- `sync.observeStatus() -> stream`
- `sync.observeChanges("notes") -> stream`

Adapters should hide the op-log and merge semantics from app code.

---

## Packaging and distribution

### Rust consumers

- Publish `libresync` as a crate with a stable semver API.
- Provide a `libresync-sdk` facade crate if needed to stabilize high-level API.

### Swift (iOS/macOS)

- Build a **static or dynamic XCFramework** containing the Rust core + wrapper.
- Use a Swift Package Manager manifest to distribute the XCFramework.
- Include a small Swift helper layer for permissions and lifecycle.

### Kotlin (Android)

- Build a **JNI bridge** + `.so` via `cargo-ndk`.
- Package as an **AAR** with Kotlin wrapper types.

---

## Migration plan (CLI POC → SDK)

### Phase 1: Core refactor

- Split CLI-only glue into `libresync-cli`.
- Introduce a stable `Engine` API in `libresync`.
- Add an event system and adapter registry.

### Phase 2: C ABI / bindings

- Define FFI-safe types and functions.
- Generate Swift/Kotlin bindings (UniFFI or manual).
- Build minimal demo apps that pair + sync a document.

### Phase 3: Platform hardening

- Key storage integration (Keychain/Keystore).
- Permission flows (local network prompts).
- Background/lifecycle behaviors (suspend/resume).

### Phase 4: Higher-level APIs and SDK polish

- Idiomatic SDK surfaces for “sync intents.”
- Better error typing and diagnostics.
- Developer docs and sample apps.

---

## Core components

### 1) Discovery layer (Zero config + manual)

**Goal:** find peers on LAN without setup, and allow manual addition.

Recommended structure:

- `DiscoveryProvider` trait with pluggable implementations:
  - `MdnsDiscovery` (LAN)
  - `StaticDiscovery` (manual addresses / “known peers”)
  - `OverlayDiscovery` (Tailscale/Headscale mode: not broadcast; see later)

mDNS is the most natural for “zero config” on LAN.

Manual pairing still matters for:

- networks where mDNS/broadcast is blocked
- cross-subnet LANs
- Tailscale/Headscale (no broadcast)

### 2) Transport layer (Secure direct connections)

**Goal:** encrypted, authenticated, multiplexed sessions.

Two reasonable paths:

- **QUIC (TLS 1.3)**: great for multiplexing streams, NAT-friendliness, and future transport flexibility.
- **TCP + TLS**: simpler, extremely portable, less moving parts.

For iOS/macOS-first, either works. QUIC tends to be “future-proof,” but TCP+TLS may reduce integration risk.

### 3) Identity, authentication, and trust

**Non-negotiable:** mutual authentication and explicit trust decisions.

Recommended:

- Each device generates a long-term keypair.
- Device ID = hash of public key.
- Pairing workflow:
  - show QR code / short code containing device ID + ephemeral handshake info
  - peer scans/enters code
  - both sides prompt: “Approve this device?”
- Store allow-list of trusted device IDs.
- All sessions require mutual auth; unknown peers are rejected.

Add revocation:

- remove peer from allow-list
- optionally rotate cluster keys or data keys if needed for stronger revocation semantics

### 4) Sync protocol

You need:

- initial reconciliation (who has what?)
- delta exchange (send missing ops)
- live streaming of changes (real-time)
- reliability and replay protection

At minimum:

- `Hello` / `Auth` / `Capabilities`
- `StateSummary` (version vector / sync state)
- `RequestDeltas`
- `DeltaBatch`
- `Ack` + `Watermark`
- `Ping` / `Pong` + keepalives

Keep protocol **binary** (CBOR/Protobuf) to reduce overhead and simplify forward compatibility.

### 5) Data model and conflict resolution (Adapters)

You said: “Any structured data: JSON, SQL, etc.”  
That implies **adapters**, not one universal representation.

Recommended adapter interface:

- `DataAdapter` defines:
  - how to encode/decode changes
  - how to compute state summary
  - how to apply remote deltas
  - how to compact / garbage collect

Provide multiple adapters over time:

#### Adapter A: JSON/document store (CRDT-backed)

- Best for app state, notes, lists, preferences.
- Use a JSON CRDT engine for:
  - maps/objects
  - lists
  - counters
  - text

Conflict behavior:

- deterministic merges for concurrent edits
- avoids “conflicted copies”
- generally great DX for collaborative-style apps

#### Adapter B: SQLite/relational

Two viable models:

1) **Row-level op log with deterministic merge policy**
   - adds triggers/change tables
   - merges by version vector and per-field strategy
2) **CRR/CRDT-style SQLite extension**
   - treat tables as convergent replicated relations
   - exchange “changesets” rather than raw DB pages

This is the hardest part to do “generically.” For v1, consider scoping SQLite support to a limited set:

- primary-keyed tables
- deterministic conflict strategy per column type
- avoid complex constraints until later

---

## Performance and scaling guidance

### Major scaling dimensions

- Number of peers (N)
- Change rate (ops/sec)
- Dataset size
- History length (CRDT/op log growth)

### Key strategies

- **Delta-based sync**: never resend full state after initial bootstrap.
- **Chunking**: send deltas in bounded batches; support streaming.
- **Compression**: optional on large batches; avoid per-message compression overhead.
- **Compaction/GC**:
  - CRDT engines usually support compaction once all peers have seen history (or by snapshotting).
  - For op logs, prune entries below the “minimum acknowledged watermark” across peers.
- **Backpressure**: if one peer is slow, don’t stall others; queue with limits.
- **Topology**:
  - For small N (≤10), full mesh is fine.
  - For larger N, consider selective forwarding or pub-sub (but that increases complexity).

### Practical v1 target

- 2–5 peers
- <10k objects / moderate SQLite DB
- real-time changes propagate within <200ms on typical LAN

---

## iOS/macOS-first: practical platform guidance

### iOS

- Plan for:
  - local network permission prompts
  - background execution limits (real-time sync may only be reliable in foreground unless using permitted background modes)
  - user approval flows must be native and obvious

Approach:

- keep a small always-on listener when app is active
- gracefully pause discovery/sync when backgrounded, then resync quickly on resume

### macOS

- easier: background daemons possible, long-lived processes acceptable
- can act as an “always-on” peer for faster convergence in your ecosystem

---

## Rust vs other languages

### Rust (recommended)

Pros:

- cross-platform compilation
- memory safety + performance
- good fit for networking + crypto + concurrency
- creates a single source of truth for sync correctness

Cons:

- binding + build pipeline complexity on Apple platforms (solvable)
- careful API design needed for ergonomic Swift/Kotlin usage

### Alternatives

- Go: simpler networking but weaker story for iOS embedding and fine-grained FFI
- Swift/Kotlin native: great platform ergonomics but you’d reimplement core logic multiple times
- C++: portable but significantly higher safety and maintenance risk

Recommendation: **Rust core**, Swift/Kotlin wrappers.

---

## Should you make “a framework per native system”?

You should make:

- **one Rust core library**
- **one SDK wrapper per platform** (thin, idiomatic, minimal)

Do not reimplement sync logic per platform. Treat each platform wrapper as a:

- lifecycle and permissions integration layer
- ergonomic API facade for the Rust core

---

## Tailscale/Headscale support (overlay mode)

### Would it work?

Yes, technically. A Tailscale/Headscale network behaves like a private routed LAN with stable addressing. Your sync traffic remains direct device-to-device.

### The catch: discovery

LAN discovery mechanisms (mDNS/broadcast) typically **do not propagate over Tailscale**. So “zero-config discovery” likely won’t work the same way across an overlay.

Recommended design:

- Keep discovery **pluggable**.
- For overlay mode, offer:
  1) manual peer add (enter tailnet hostname/IP)
  2) optional “peer list provider” integration (advanced; depends on availability of tailnet enumeration APIs)
  3) “introducer peer” concept: one always-on node shares known peers to newly added devices *after trust is established*.

Security note:

- Your own mutual auth still matters; do not rely on Tailscale identity alone as your only trust boundary (though you can use it as a signal).

User experience:

- LAN: “it just finds peers.”
- Tailscale: “add peers by name once; then it behaves similarly.”

---

## Example use cases (structured data, not files)

1) **Health/medical trackers**
   - medication logs, symptom diaries, vitals
   - sync between iPhone and Mac without cloud storage
2) **Offline-first notes**
   - rich text + tags
   - real-time edits on LAN (e.g., iPad + Mac)
3) **Home automation state**
   - local rules, device configurations, sensor history
4) **Small-team collaboration on LAN**
   - workshop/classroom tasks, checklists, kanban boards
5) **Field data collection**
   - multiple devices gathering records offline; merge when back on a local router
6) **Local dev tools**
   - sync app settings/state between dev machines and mobile devices automatically

---

## Project naming ideas

You want something:

- pronounceable
- not overly narrow (not “LAN” only, because Tailscale mode exists)
- evokes trust, convergence, and local-first

Candidate directions:

- **Concord** (agreement/convergence)
- **Harmonic** / **HarmonySync**
- **MeshSync**
- **Lattice**
- **Converge**
- **PeerWeave**
- **Lantern** (local beacon + guidance; could be tasteful)
- **Synapse** (connection + signaling; check trademark conflicts)

If you want a more explicit privacy stance:

- **LibreMeshSync**
- **SovereignSync**

---

## Proposed repository and code structure

### Monorepo layout (recommended)

### Build/bindings approach

- Start with C ABI (stable, lowest common denominator) or UniFFI for faster multiplatform bindings.
- For Apple:
  - build Rust as static libs for device + simulator
  - package as XCFramework
  - wrap with a Swift Package for clean import

---

## Recommended v1 scope (to ship something real)

To avoid boiling the ocean:

### v1 deliverables

- LAN discovery via mDNS
- Secure pairing + allow-list
- One transport (TCP+TLS or QUIC)
- One adapter: JSON CRDT OR simple key-value with deterministic merge
- iOS + macOS SDKs
- CLI debug tool (“list peers”, “pair”, “sync status”, “dump state”)

### v1 non-goals

- full SQLite general replication
- large multi-peer pub-sub / gossip
- background-sync perfection on iOS

---

## Threat model (minimum)

You should explicitly define:

- Attacker on the same Wi-Fi network (eavesdropping, spoofing)
- Malicious peer attempting to join cluster
- Replay attacks
- Device theft
- Metadata leakage (peer IDs, service names, discovery beacons)

Mitigations:

- encrypt everything
- mutual auth
- device approval
- replay protection (nonces + sequence numbers)
- minimize discovery payload contents
- optional “private mode” where discovery doesn’t broadcast until user opts in

---

## Open questions you should decide early

- **Data philosophy**: Do you aim to be “CRDT-first” (best merges) or “log/clock-first” (simpler, more generic)?
- **Adapter API**: What is the minimal contract to support JSON + SQLite cleanly?
- **Peer topology**: Mesh only, or hub option?
- **Compaction semantics**: When and how do you safely GC history?
- **UX for trust**: pairing codes vs QR, and how to handle re-keying.

---

## Final recommendation

- Keep the core **Rust**.
- Build a **pluggable discovery + transport** layer so LAN and overlay modes coexist cleanly.
- Make the sync engine **adapter-driven**:
  - start with JSON/CRDT (developer-friendly, strong merges)
  - add SQLite once the transport/protocol/trust foundation is rock-solid
- Provide a **great CLI** for testing and diagnostics (it will save you months).

If you want, I can produce next:

1) A concise **protocol spec** (message types + state machine),
2) A **threat model document** (structured, actionable),
3) A proposed **Rust crate API** (traits + public functions),
4) A short list of **name finalists** with repo/package naming conventions. 
