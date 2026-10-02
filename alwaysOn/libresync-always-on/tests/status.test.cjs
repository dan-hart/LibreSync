const test = require("node:test");
const assert = require("node:assert/strict");
const { peerLabel, invitationHint } = require("../src/model.js");
test("Stored is never mislabeled Applied or up to date", () => {
  const p = {
    state: "Stored",
    pending: 0,
    stored: { epoch: "a", sequence: 4 },
    applied: { epoch: "a", sequence: 0 },
  };
  assert.equal(peerLabel(p), "Stored · waiting for app");
  p.applied.sequence = 4;
  assert.equal(peerLabel(p), "Applied by app");
  p.pending = 1;
  assert.equal(peerLabel(p), "Waiting · 1 pending");
});
test("Pairing guides expiry and legacy protocol before connect", () => {
  assert.equal(
    invitationHint({ version: 1, expires_at: 300 }, 100),
    "Upgrade this app before connecting",
  );
  assert.equal(
    invitationHint({ version: 2, expires_at: 50 }, 100),
    "Invitation expired · open a new one",
  );
  assert.equal(invitationHint(null, 100), "Open Connect on this device");
  assert.equal(
    invitationHint({ version: 2, expires_at: 300 }, 100),
    "Ready to enter its code",
  );
});
test("Waiting availability takes precedence over older Applied receipts", () => {
  const p = {
    state: "Waiting",
    pending: 0,
    stored: { epoch: "a", sequence: 4 },
    applied: { epoch: "a", sequence: 4 },
  };
  assert.equal(peerLabel(p), "Waiting · last update applied");
  p.state = "Paused";
  assert.equal(peerLabel(p), "Paused");
  p.state = "NeedsRepair";
  assert.equal(peerLabel(p), "Reconnect this device");
});
test("Space phase reflects failed storage and stopped runtime", () => {
  const { spaceLabel } = require("../src/model.js");
  assert.equal(
    spaceLabel({
      space: { paused: false },
      session: { phase: "Failed", peers: [] },
    }),
    "Storage needs attention · data preserved",
  );
  assert.equal(
    spaceLabel({
      space: { paused: false },
      session: { phase: "Stopped", peers: [] },
    }),
    "Not running · data preserved",
  );
});

test("Removed peers offer fresh-invitation repair instead of resume", () => {
  const { peerActions } = require("../src/model.js");
  const peer = { revoked: true, state: "Paused" };
  assert.equal(peerLabel(peer), "Removed · local data preserved");
  assert.deepEqual(peerActions(peer), ["repair"]);
  assert.deepEqual(peerActions({ revoked: false, state: "Paused" }), [
    "resume",
    "remove",
  ]);
});

test("Removed peers do not count as trusted devices", () => {
  const { spaceLabel } = require("../src/model.js");
  assert.equal(
    spaceLabel({
      space: { paused: false },
      session: {
        phase: "Running",
        peers: [{ revoked: true }, { revoked: false }],
      },
    }),
    "1 trusted device · independent app space",
  );
});

test("Typed diagnostic actions guide recovery without raw socket details or inferred denial", () => {
  const { diagnosticDescription } = require("../src/model.js");
  assert.equal(
    diagnosticDescription("Retry"),
    "The other device is unavailable right now. Keep both apps open, then try again.",
  );
  assert.ok(diagnosticDescription("CheckPermissions").startsWith("Check"));
  assert.ok(!diagnosticDescription("Unknown").includes("denied"));
});
