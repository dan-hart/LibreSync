# Privacy Policy

LibreSync is a **local-first** synchronization framework. The core goal is to keep user data off the internet and out of centralized services.

## Data handling
- **No cloud sync**: data is exchanged only between trusted devices.
- **No telemetry by default**: the library should not collect or transmit analytics.
- **Local storage**: each device keeps its own local copy of data.

## Network modes
- **LAN mode**: peers auto-discover on the local network.
- **Overlay mode (optional)**: peers connect over a private overlay (e.g., Tailscale/Headscale), still device-to-device with no third-party relay.

## Metadata minimization
Discovery and pairing flows should share the minimum metadata needed to establish trust. Avoid embedding personal information in device identifiers or discovery beacons.

## Policy updates
As the implementation evolves, this document will be updated to reflect any privacy-impacting changes.
