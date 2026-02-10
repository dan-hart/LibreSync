# Tailscale/Headscale overlay discovery

LibreSync now auto-discovers peers on private overlays in addition to LAN mDNS.

## What works by default
- If the `tailscale` client is installed and connected, `discover`, `link`, `refresh`, `watch`, and `auto_refresh` will include reachable Tailscale peers.
- This also covers Headscale deployments because clients use the same `tailscale status --json` interface.

## Optional: other private overlays
- Set `LIBRESYNC_OVERLAY_PEERS` to provide explicit peer addresses.
- Format: comma-separated `ip[:port]` values.
- If port is omitted, LibreSync uses `52345`.

Example:
```
export LIBRESYNC_OVERLAY_PEERS="10.10.0.2:52345,10.10.0.3,fd7a:115c:a1e0::12"
libresync discover
```

## Trust model reminder
- Discovery only finds candidates.
- Linking and fingerprint verification still gate trust.
