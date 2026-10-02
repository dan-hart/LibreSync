"use strict";
function peerLabel(peer) {
  if (peer.revoked) return "Removed · local data preserved";
  if (peer.state === "NeedsMerge") return "Review first merge";
  if (peer.state === "NeedsRepair") return "Reconnect this device";
  if (peer.state === "Paused") return "Paused";
  if (peer.state === "Exchanging") return "Exchanging locally";
  if (peer.pending > 0) return `Waiting · ${peer.pending} pending`;
  if (peer.state === "Waiting") {
    if (
      peer.stored.sequence > 0 &&
      peer.applied.epoch === peer.stored.epoch &&
      peer.applied.sequence >= peer.stored.sequence
    )
      return "Waiting · last update applied";
    if (peer.stored.sequence > 0) return "Waiting · last update stored";
    return "Waiting for first exchange";
  }
  if (peer.stored.sequence > 0) {
    return peer.applied.epoch === peer.stored.epoch &&
      peer.applied.sequence >= peer.stored.sequence
      ? "Applied by app"
      : "Stored · waiting for app";
  }
  return "Waiting for first exchange";
}
function peerActions(peer) {
  if (peer.revoked) return ["repair"];
  const actions = [];
  if (peer.state === "NeedsMerge") actions.push("merge");
  if (peer.state === "NeedsRepair") actions.push("repair");
  actions.push(peer.state === "Paused" ? "resume" : "pause", "remove");
  return actions;
}
function diagnosticDescription(action) {
  return (
    {
      Retry:
        "The other device is unavailable right now. Keep both apps open, then try again.",
      Wait: "Waiting for the other device to become available. Updates remain saved here.",
      CheckPermissions:
        "Check this device’s local network settings and permissions, then try again.",
      GrantPermission:
        "Allow local network access in system settings, then try again.",
      CheckSecureStorage:
        "Secure storage needs attention. Unlock or restore it before continuing.",
      RepairPeer:
        "Open a fresh invitation on this same device and use Repair connection.",
      ReviewMerge: "Review the incoming data and choose whether to combine it.",
      ReviewCompatibility:
        "Update both apps to compatible versions and open a new invitation.",
      ReviewRecords:
        "The app sent data this version cannot accept. Update it and try again.",
      ResolveGroupConflict:
        "This device belongs to a different trusted group. Add a new app space to connect it.",
    }[action] ||
    "The connection needs attention. Open Details for information to share with support."
  );
}
function invitationHint(invitation, now = Math.floor(Date.now() / 1000)) {
  if (!invitation) return "Open Connect on this device";
  if (invitation.version !== 2) return "Upgrade this app before connecting";
  if (invitation.expires_at <= now)
    return "Invitation expired · open a new one";
  return "Ready to enter its code";
}
function spaceLabel(view) {
  if (view.session.phase === "Failed")
    return "Storage needs attention · data preserved";
  if (view.session.phase === "Stopped") return "Not running · data preserved";
  if (view.space.paused || view.session.phase === "Paused")
    return "Paused · your data stays here";
  const count = view.session.peers.filter((peer) => !peer.revoked).length;
  return `${count} trusted device${count === 1 ? "" : "s"} · independent app space`;
}
if (typeof module !== "undefined")
  module.exports = {
    peerLabel,
    peerActions,
    invitationHint,
    spaceLabel,
    diagnosticDescription,
  };
