# Privacy Policy

LibreSync is a **local-first** synchronization framework. The core goal is to keep user data off the internet and out of centralized services.

## Data Handling
- **No cloud sync**: data is exchanged only between trusted devices.
- **No telemetry by default**: the library should not collect or transmit analytics.
- **Local storage**: each device keeps its own local copy of data.

## Network Modes
- **LAN mode**: devices auto-discover on the local network.
- **Overlay mode (optional)**: devices connect over a private overlay (e.g., Tailscale/Headscale), still device-to-device with no third-party relay.

## Repository Privacy Rules
- Do not commit PII (real names, emails, phone numbers, addresses, or identifiers).
- Use placeholders such as `user@example.com` or `[REDACTED]`.
- Avoid including private communications or personal notes in docs.

## Privacy Checks
- ASP preflight warns on PII-like content in added lines.
- Review diffs for accidental disclosures before every commit.

## Policy Updates
As implementation evolves, this document will be updated to reflect privacy-impacting changes.
