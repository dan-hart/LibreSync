# Native Nearby Sync polish

Improve existing Apple and Android Nearby Sync views and Apple notes demos without changing protocols, public API signatures, data storage, or trust semantics. Use the creator's requested platform-native direction; SDK tint remains inherited.

## Acceptance

- Apple SDK: clear identity/status, grouped device rows, a useful empty state, prominent Add Device, secondary diagnostics. Pairing uses clear show-code and connect sections, readable code/expiry, native dismissal and confirmations. Avoid nested scrolling lists and duplicate headers in host navigation.
- Apple demos: iOS grouped settings-style Nearby screen and familiar toolbar navigation; macOS balanced sidebar/content and toolbar actions; polish notes rows/editor and rename sheets while preserving drafts and lifecycle.
- Android: Material 3 cards, typography, controls, readable device status/actions and pairing groups; preserve permissions, invitation confirmation, expiry, repair and bootstrap consent.
- Accessibility: semantic colors, labels, multiline names, large text, dark/light appearance, keyboard and narrow layouts. Minimum SDK macOS 12/iOS 15 remains supported.
- Patch version 0.8.1 with changelog; all maintained version declarations consistent. No new cloud service or dependency merely for decoration.

## Verification

Capture representative before/after screens on dedicated demo profiles. Build Swift SDK, macOS and iOS demos, and Android Compose/sample. Run existing relevant tests and version/readiness checks. Review spec compliance first, then code quality, fixing actionable issues. Do not claim physical-device testing.
