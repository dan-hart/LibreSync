# LibreSync AlwaysOn

AlwaysOn keeps durable encrypted copies of app updates on a computer that stays
available. Apps connect directly over the local network. There are no accounts,
cloud storage, relays, or internet services in the connection flow.

## Connect an app

Open the dashboard and choose **Add app**. Choose a supported app or paste the
invitation shared from that app’s Connect screen. Each app space has its own
identity, transport key, storage key, records and trusted devices. Two spaces
for the same app are separate trust groups.

The built-in catalog supports **Momentum**’s declared operation-log schema and
**Local notes sample**, a demonstration logical-record app. Momentum at commit
`766064b` still uses the legacy LibreSync API and requires the opt-in migration
in [the Momentum integration guide](../docs/MOMENTUM-MANAGED-MIGRATION.md).
Adding a Momentum space does not automatically upgrade that existing client.
The Local notes sample is independent of Apple Notes or other notes products.

In an app space, choose **Connect device**. Show a locally rendered QR invitation
or six-digit code, use a nearby device’s code, or paste its invitation. Invitations
expire after five minutes and are single use. Device discovery is an untrusted
hint: compatible app/schema metadata and pairing versions are checked before
code entry, then certificates and complete manifests are authenticated during
pairing. Ordinary flows require no IP address, port, key, or technical app ID.

Empty spaces can import compatible data automatically. A populated first join
requires **Review merge** and explicit **Combine data** consent, or cancel and
pause. The registered adapter defines merge behavior; there is no blind file
replacement. A stale preview requires a new review.

## Understand status and recovery

**Waiting** means the device is unavailable or has pending updates. **Stored**
means the receiving Session saved a durable copy. **Applied** means the receiving
application confirmed its own durable transaction. AlwaysOn is a store-and-forward
companion and never fabricates an application-applied acknowledgment. Historical
receipts do not turn an offline device into an up-to-date device.

Pause/resume retains local data. Device removal revokes the local connection;
app-space removal stops and archives that space. Neither operation remotely erases
copies. Repair requires a fresh authenticated invitation from the same device.
Diagnostics explain network reachability, schema/pairing upgrades, merge consent,
and secure-storage failures without guessing permission denial from no discoveries.

**Back up** retains the latest five encrypted logical recovery exports per space.
The Session also retains at most five encrypted pre-merge recovery copies.
Retention prunes recovery copies, never live records or tombstones. Recovery
export contains sensitive unencrypted data; save it privately. Export changes
neither records nor receipts. Whole-operation-log rollback is not offered:
Momentum requires an app-aware recovery importer.

## Run the desktop app

```sh
cargo build --manifest-path alwaysOn/libresync-always-on/src-tauri/Cargo.toml
```

The tray uses native macOS Keychain or Linux Secret Service. Locked/unavailable
storage is an error, never a reason to generate replacement keys or silently
store plaintext keys. The default friendly name comes from the OS; use
`--device-name "Kitchen computer"` to override it. Closing the dashboard hides
it; **Open dashboard** restores it from the tray. **Quit** stops the managed
listeners/workers gracefully.

The frontend bundles its QR renderer and uses restricted CSP, with untrusted
names rendered as text. Tauri 2 Linux builds require `libwebkit2gtk-4.1-dev`,
`libgtk-3-dev`, `libayatana-appindicator3-dev`, and `librsvg2-dev`. The Windows
tray currently requires a secure OS KeyStore implementation; no production
plaintext fallback is provided.

For an isolated development smoke test (debug builds only):

```sh
./alwaysOn/libresync-always-on/src-tauri/target/debug/libresync-always-on --development-root /tmp/libresync-ui-test \
  --development-file-keys --device-name "AlwaysOn test computer"
# From the repository root, run a compatible real peer in another terminal:
cargo run -p libresync --example companion_app -- /tmp/libresync-notes-test --code
```

The debug flags require an explicit isolated root and show a development banner.
The example explicitly uses private file keys. Production builds reject these
tray flags. Human usability and physical-device support need separate validation.

## Headless daemon

The daemon uses the same companion manager. Default keys use the secure OS
backend. On an unattended installation, `--file-keys` explicitly opts into
owner-only plaintext secret files under the chosen managed root. Protect and
back up that directory; it is not equivalent to a hardware-protected keystore.

```sh
cargo run -p libresync-alwayson-daemon -- --root /private/path/managed status
# Paste an invitation on stdin; EOF completes enrollment. It is not a command-line secret.
cargo run -p libresync-alwayson-daemon -- --root /private/path/managed enroll -
cargo run -p libresync-alwayson-daemon -- --root /private/path/managed serve
```

Commands are `serve` (default), `status`, `add-momentum`, `enroll FILE|-`,
`pause SPACE`, `resume SPACE`, `remove SPACE`, `backup SPACE`, and
`archive-legacy CONFIG`. Run maintenance commands while this root’s service is
stopped: an exclusive manager lease prevents competing processes. Restart
`serve` to resume persisted enrollment and exchange automatically. Completion
of enrollment confirms authenticated trust, not that the other app applied data.
No automatic approval or legacy pairing-secret flags are accepted.

Service templates are in `contrib/` and `alwaysOn/service/`. Set an explicit root
in service arguments. On Linux, the default secure backend requires an unlocked
Secret Service session. Add `--file-keys` only as an intentional operator choice.

## Preserve old installations

The tray has a unique new bundle identity. It checks the historical
`com.tauri.dev/config.json` location only when the config identifies the old
LibreSync tray and declares its state/data paths. It never adopts the parent
folder or other Tauri applications. **Make legacy recovery copy** copies only
that config and its declared existing data/state files to a private archive;
original paths remain untouched. Legacy keys and trust are recovery data only,
not managed enrollment. Follow [the companion migration guide](../docs/COMPANION.md)
and the specific app’s migration before connecting a new space.

Explicit legacy recovery archives retain the latest five complete copies. Failed copies are removed before publication. Retention never prunes original config/data/state paths or managed live records.
