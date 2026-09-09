# Packaging on Linux

LibreSync talks TCP on the listener port (default `52345`) and multicast UDP
`5353` for mDNS. Everything below follows from those two facts.

## Flatpak (GTK 4 apps)

### Required `finish-args`
- `--share=network` — without it the sandbox has no network at all: no
  listener, no outgoing sync, no mDNS. There is no finer-grained network
  permission in Flatpak.

### Optional `finish-args`
- `--filesystem=/var/run/tailscale:ro` — lets the engine read Tailscale peers
  through the daemon's local API socket (`/var/run/tailscale/tailscaled.sock`)
  when the crate is built with the `tailscale-local-api` feature. Override the
  path with `LIBRESYNC_TAILSCALE_SOCKET`.
- `--talk-name=org.freedesktop.secrets` — needed only if the app stores keys
  with the Secret Service directly. Apps that use the Secret portal
  (`org.freedesktop.portal.Secret`, available in every sandbox) need nothing.

A complete fragment is in `contrib/flatpak/com.example.LibreSyncApp.yml`.

### Tailscale inside the sandbox
The `tailscale` CLI is not in the sandbox, so `tailscale status --json`
cannot run. The engine handles this explicitly:
- The CLI is probed once per process. On failure (`ENOENT`, permission error,
  non-zero exit) discovery logs one `info` line through the `log` facade and
  never spawns the CLI again (`libresync::tailscale_cli_state()` reports
  `Unavailable`; `reset_tailscale_probe()` retries, e.g. after the user
  installs Tailscale).
- With the `tailscale-local-api` feature the engine then reads
  `GET /localapi/v0/status` over the Unix socket, which works in the sandbox
  when `--filesystem=/var/run/tailscale` is granted. The socket is owned by
  root with mode 0666 on stock installs; if your distribution restricts it,
  add the user to the `tailscale` group or skip this feature.
- LAN mDNS and `LIBRESYNC_OVERLAY_PEERS` keep working either way.

Build the crate with `--features tailscale-local-api` in the manifest.

### Keys in the sandbox
Use the Secret portal through libsecret (`secret_password_store` with the
default collection works under the portal automatically in GNOME 46+ runtimes)
and hand the material to the engine through your `DeviceHandler`, or use
`libresync::SecretToolKeyStore` when `secret-tool` is available (it is not in
the GNOME runtime; prefer libsecret from the app). `FileKeyStore` is only for
development. See `docs/PACKAGING-MACOS.md` for the Keychain equivalent and
`docs/API.md` for the `KeyStore` trait.

## mDNS and firewalls

mDNS needs inbound multicast UDP on port 5353 (source port 5353, destination
`224.0.0.251` / `ff02::fb`) and the sync port needs inbound TCP.

### Fedora (firewalld)
Fedora Workstation's default `FedoraWorkstation` zone allows the `mdns`
service and all ports `1025-65535/tcp,udp`, so LibreSync works out of the box.
Fedora Server (`FedoraServer` / `public` zone) allows neither. Make it explicit
on any zone:

```bash
sudo firewall-cmd --permanent --add-service=mdns
sudo firewall-cmd --permanent --add-port=52345/tcp
sudo firewall-cmd --reload
sudo firewall-cmd --list-all   # verify: services include mdns, ports include 52345/tcp
```

If you changed the listener port, replace `52345`. Zones are per interface;
run the commands with `--zone=<zone>` if your LAN interface is not in the
default zone (`firewall-cmd --get-active-zones`).

### Ubuntu (ufw)
ufw is installed but inactive by default; once enabled its default policy is
deny-incoming, which blocks both mDNS replies and the listener:

```bash
sudo ufw allow 5353/udp comment 'mDNS (LibreSync discovery)'
sudo ufw allow 52345/tcp comment 'LibreSync sync listener'
sudo ufw status verbose   # verify
```

Ubuntu Desktop also runs `avahi-daemon`, which already binds 5353. `mdns-sd`
sets `SO_REUSEPORT`/`SO_REUSEADDR` on its socket and coexists with Avahi;
both answer queries for their own services.

### Verification
The core crate ships an ignored test that advertises and browses on the real
network:

```bash
cargo test -p libresync --lib -- --ignored mdns_advertise_and_browse_round_trip
```

It passes on a Fedora 44 workstation (firewalld running, `FedoraWorkstation`
zone) and is expected to pass on Ubuntu with the two ufw rules above; it fails
with a firewall hint when UDP 5353 is blocked. Note that a single machine can
see its own advertisement through the loopback multicast path even when
inbound 5353 is filtered, so finish with a two-device `libresync discover`. Two-device checks: `libresync discover` on both machines (see
`TESTING.md`).

## systemd user service
`contrib/systemd/libresync-alwayson-daemon.service` plus the steps in
`contrib/README.md`.
