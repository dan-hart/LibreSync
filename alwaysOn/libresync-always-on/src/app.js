"use strict";
const invoke = (command, args = {}) =>
  window.__TAURI__.core.invoke(command, args);
const $ = (id) => document.getElementById(id);
let catalog = [],
  dialogSpace = null,
  expires = null,
  refreshBusy = false,
  actionBusy = false;
function el(tag, text, className) {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = text;
  if (className) node.className = className;
  return node;
}
function button(text, action, className = "secondary") {
  const node = el("button", text, className);
  node.setAttribute(
    "data-key",
    ["Pause", "Resume"].includes(text) ? "toggle" : text,
  );
  node.action = action;
  node.addEventListener("click", () => perform(node.action, node));
  return node;
}
function message(text, error = false) {
  $("alert").hidden = false;
  $("alert").className = error ? "error" : "";
  $("alert").textContent = text;
  if ($("modal").open) {
    let inline = $("modal-body").querySelector(".flow-message");
    if (!inline) {
      inline = el("p", undefined, "flow-message");
      inline.setAttribute("role", "alert");
      $("modal-body").append(inline);
    }
    inline.textContent = text;
  }
}
async function perform(action, node) {
  if (node && actionBusy) return;
  const originalLabel = node?.textContent;
  let released = false;
  const release = () => {
    if (!node || released) return;
    node.textContent = originalLabel;
    actionBusy = false;
    for (const button of document.querySelectorAll("button"))
      button.disabled = false;
    released = true;
  };
  if (node) {
    node.textContent = originalLabel.includes("Connect")
      ? "Connecting securely…"
      : "Working…";
    actionBusy = true;
    for (const button of document.querySelectorAll("button"))
      button.disabled = true;
  }
  try {
    await action();
    release();
    await refresh();
  } catch (error) {
    message(String(error), true);
  } finally {
    release();
  }
}
function modal(title) {
  $("modal-title").textContent = title;
  $("modal-body").replaceChildren();
  if (!$("modal").open) $("modal").showModal();
  return $("modal-body");
}
async function closeModal() {
  const space = dialogSpace;
  dialogSpace = null;
  expires = null;
  $("modal").close();
  if (space) await invoke("close_invitation", { space });
}
$("close").addEventListener("click", () => perform(closeModal));
$("modal").addEventListener("cancel", (event) => {
  event.preventDefault();
  perform(closeModal);
});
async function refresh() {
  if (refreshBusy || actionBusy) return;
  refreshBusy = true;
  try {
    const data = await invoke("dashboard");
    catalog = data.supported_apps;
    $("development").hidden = !data.development_mode;
    $("empty").hidden = data.spaces.length !== 0;
    $("legacy").hidden = !data.legacy_available;
    reconcileDashboard($("spaces"), data.spaces, spaceCard);
  } finally {
    refreshBusy = false;
  }
}
function spaceCard(view) {
  const { space, session } = view;
  const card = el("article", undefined, "space");
  card.setAttribute("data-key", `space:${space.id}`);
  const top = el("div", undefined, "space-top");
  top.setAttribute("data-key", "header");
  const title = el("div", undefined, "space-title");
  title.append(
    el("h3", space.manifest.display_name),
    el("p", spaceLabel(view)),
  );
  const actions = el("div", undefined, "actions");
  actions.append(
    button("Connect device", () => connectFlow(space.id)),
    button(space.paused ? "Resume" : "Pause", () =>
      invoke("space_action", {
        space: space.id,
        action: space.paused ? "resume" : "pause",
      }),
    ),
    button("Remove", () => removeSpace(space), "quiet"),
  );
  top.append(title, actions);
  card.append(top);
  if (!session.peers.length)
    card.append(
      el(
        "div",
        "Connect this app on your phone or another computer to start exchanging updates.",
        "muted",
      ),
    );
  for (const peer of session.peers) {
    const row = el("div", undefined, "device");
    row.setAttribute("data-key", `peer:${peer.identity.device_id}`);
    const info = el("div");
    info.append(
      el("div", peer.metadata.display_name, "device-name"),
      el("div", peerLabel(peer), "badge"),
    );
    const controls = el("div", undefined, "actions");
    for (const action of peerActions(peer)) {
      if (action === "merge")
        controls.append(
          button("Review merge", () => reviewMerge(space.id, peer)),
        );
      else if (action === "repair")
        controls.append(
          button(
            peer.revoked ? "Reconnect with invitation" : "Repair connection",
            () => consumeInvitation(space.id, peer.identity.device_id),
          ),
        );
      else if (action === "remove")
        controls.append(
          button("Remove", () => removePeer(space.id, peer), "quiet"),
        );
      else
        controls.append(
          button(
            action === "resume" ? "Resume" : "Pause",
            () =>
              invoke("peer_action", {
                space: space.id,
                device: peer.identity.device_id,
                action,
              }),
            "quiet",
          ),
        );
    }
    row.append(info, controls);
    card.append(row);
  }
  if (session.diagnostics.length) {
    const last = session.diagnostics.filter((d) => d.Diagnostic).at(-1);
    if (last) {
      const d = last.Diagnostic;
      const box = el("div", undefined, "guidance");
      box.setAttribute("data-key", "diagnostics");
      const details = el("details");
      details.append(el("summary", "Details"), el("p", d.message));
      box.append(
        el("strong", guidance(d.action)),
        el("p", diagnosticDescription(d.action)),
        details,
      );
      if (["Retry", "Wait"].includes(d.action))
        box.append(
          button(
            "Try again",
            () => invoke("space_action", { space: space.id, action: "wake" }),
            "quiet",
          ),
        );
      card.append(box);
    }
  }
  const backup = el("div", undefined, "backups");
  backup.setAttribute("data-key", "backups");
  backup.append(
    el(
      "span",
      `${view.backups.length} encrypted recovery ${view.backups.length === 1 ? "copy" : "copies"} · keeps latest ${space.retain_backups}. Live updates and deletions stay retained.`,
    ),
  );
  const backupActions = el("div", undefined, "actions");
  backupActions.append(
    button("Back up", () => invoke("backup", { space: space.id }), "quiet"),
    button("Recovery copies", () => backupFlow(view), "quiet"),
  );
  backup.append(backupActions);
  card.append(backup);
  return card;
}
function guidance(action) {
  return (
    {
      CheckPermissions: "Check local network permission",
      GrantPermission: "Allow local network access in system settings",
      CheckSecureStorage: "Unlock or restore your secure key storage",
      RepairPeer: "Get a new invitation from this same device",
      ReviewMerge: "Review incoming data before combining it",
      ReviewCompatibility:
        "Upgrade apps to the same supported schema and pairing version",
      ReviewRecords: "The app sent unsupported data. Update it and retry.",
      ResolveGroupConflict: "Use a new app space for a different trusted group",
      Wait: "Waiting for the other device",
      Retry: "Keep both apps open on the same local network",
    }[action] || "Connection details"
  );
}
function addFlow() {
  const body = modal("Add an app");
  body.append(
    el(
      "p",
      "Each app space keeps its own data, keys and trusted devices. The app must support managed LibreSync connections.",
    ),
  );
  const supported = el("div");
  catalog.forEach((app, choice) =>
    supported.append(
      button(`Add ${app.display_name}`, async () => {
        await invoke("add_app", { choice });
        await closeModal();
      }),
    ),
  );
  body.append(
    supported,
    el("p", "Already have an invitation from your app?"),
    button("Use app invitation", () => consumeInvitation(null)),
  );
}
$("add").addEventListener("click", addFlow);
$("first-add").addEventListener("click", addFlow);
function consumeInvitation(space, repairPeer = null) {
  const body = modal(repairPeer ? "Repair this device" : "Use app invitation");
  body.append(
    el(
      "p",
      repairPeer
        ? "Open Connect on the same device and create a fresh invitation. Repair preserves local data and rechecks its certificate."
        : "Paste the invitation shared by the app’s Connect screen. It expires after five minutes. This grants that device access to this app space.",
    ),
  );
  const label = el("label", "App invitation");
  const text = el("textarea");
  text.placeholder = "Paste the invitation";
  text.autocomplete = "off";
  text.spellcheck = false;
  label.append(text);
  body.append(
    label,
    button("Connect securely", async () => {
      await invoke("connect_invitation", {
        space,
        encoded: text.value.trim(),
        repairPeer,
      });
      text.value = "";
      await closeModal();
      message("Device connected. Incoming data may need your merge approval.");
    }),
  );
}
async function connectFlow(space) {
  const body = modal("Connect a device");
  body.append(
    el(
      "p",
      "Open Connect in the same app on your other device. Scan a QR invitation, use a six-digit code, or paste its invitation. Both devices need to be reachable on your local network.",
    ),
  );
  body.append(
    button("Show QR invitation", () => showInvite(space, false)),
    button("Show six-digit code", () => showInvite(space, true)),
    button("Enter a nearby device’s code", () => nearbyFlow(space)),
    button("Paste invitation", () => consumeInvitation(space)),
  );
}
async function showInvite(space, code) {
  const view = await invoke("invitation", { space, code });
  dialogSpace = space;
  expires = view.expires_at;
  const body = modal(code ? "Connect with this code" : "Scan to connect");
  body.append(
    el(
      "p",
      "This invitation is single use. Only share it with a device you trust.",
    ),
  );
  const image = el("img", undefined, "qr");
  image.alt = "Local pairing invitation";
  image.src =
    "data:image/svg+xml;charset=utf-8," + encodeURIComponent(view.svg);
  body.append(image);
  if (view.code) body.append(el("span", view.code, "code"));
  body.append(el("p", "", "expiry"));
  body.append(
    button(
      "Copy invitation",
      () => navigator.clipboard.writeText(view.encoded),
      "secondary",
    ),
  );
  updateExpiry();
}
function updateExpiry() {
  if (!expires) return;
  const line = $("modal-body").querySelector(".expiry");
  if (!line) return;
  const remaining = Math.max(0, expires - Math.floor(Date.now() / 1000));
  line.textContent = remaining
    ? `Expires in ${Math.floor(remaining / 60)}:${String(remaining % 60).padStart(2, "0")}`
    : "Invitation expired. Close this window and create another.";
}
async function nearbyFlow(space) {
  const body = modal("Nearby devices");
  body.append(
    el(
      "p",
      "Open Connect on your other device and show its six-digit code. Device names are discovery hints; the code authenticates the connection.",
    ),
  );
  const peers = await invoke("discover", { space });
  if (!peers.length)
    body.append(
      el(
        "p",
        "No devices found yet. Keep both apps open on the same local network. Check local network permission if your system requests it.",
      ),
    );
  for (const view of peers) {
    const peer = view.peer;
    const row = el("div", undefined, "list-row");
    const reason = {
      DifferentApp:
        "This device is using a different app. Choose that app’s space.",
      UpgradeSchema:
        "This app schema differs. Upgrade both apps to the same supported version.",
      UpgradePairing: "Upgrade this app before connecting",
      MissingMetadata:
        "No managed app metadata. Upgrade the app before connecting.",
      PairingClosed: "Open Connect on this device",
      Expired: "Invitation expired · open a new one",
    }[view.compatibility];
    const hint =
      view.compatibility === "Ready" ? invitationHint(peer.invitation) : reason;
    row.append(
      el("strong", peer.advertisement?.display_name || "Nearby device"),
      el(
        "p",
        `${peer.advertisement?.app_display_name || "Unknown app"} · ${hint}`,
      ),
    );
    if (hint === "Ready to enter its code") {
      const input = el("input");
      input.inputMode = "numeric";
      input.maxLength = 6;
      input.autocomplete = "off";
      input.placeholder = "Six-digit code";
      input.setAttribute("aria-label", "Six-digit code");
      row.append(
        input,
        button("Connect securely", async () => {
          await invoke("connect_code", {
            space,
            device: peer.identity.device_id,
            code: input.value,
          });
          input.value = "";
          await closeModal();
        }),
      );
    }
    body.append(row);
  }
  body.append(
    button("Refresh nearby devices", () => nearbyFlow(space), "quiet"),
  );
}
async function reviewMerge(space, peer) {
  const preview = await invoke("merge_preview", {
    space,
    device: peer.identity.device_id,
  });
  if (!preview) {
    message("No merge is pending.");
    return;
  }
  const body = modal("Review first merge");
  body.append(
    el(
      "p",
      `Combine ${preview.records} incoming records, including ${preview.deletions} deletions, with the data already held here? The registered app adapter validates and combines records under its declared merge rules. Momentum operations are unioned and newer app snapshots supersede older snapshots; Local notes uses record clocks. A pre-merge recovery copy preserves the previous records.`,
    ),
  );
  body.append(
    el(
      "p",
      "This approves this first exchange only. If local data changes, you will need to review a fresh preview.",
    ),
  );
  const choices = el("div", undefined, "flow-actions");
  choices.append(
    button("Combine data", async () => {
      await invoke("merge_decision", {
        space,
        device: peer.identity.device_id,
        token: preview.token,
        merge: true,
      });
      await closeModal();
    }),
    button(
      "Cancel and pause",
      async () => {
        await invoke("merge_decision", {
          space,
          device: peer.identity.device_id,
          token: preview.token,
          merge: false,
        });
        await closeModal();
      },
      "secondary",
    ),
  );
  body.append(choices);
}
function removeSpace(space) {
  const body = modal(`Remove ${space.manifest.display_name}?`);
  body.append(
    el(
      "p",
      "This stops the space and revokes its local connections. Its encrypted records and keys stay archived on this computer. Copies already stored on other devices cannot be erased. Adding the app again creates a new space.",
    ),
  );
  body.append(
    button(
      "Remove app space",
      async () => {
        await invoke("space_action", { space: space.id, action: "remove" });
        await closeModal();
      },
      "danger",
    ),
  );
}
function removePeer(space, peer) {
  const body = modal(`Remove ${peer.metadata.display_name}?`);
  body.append(
    el(
      "p",
      "This revokes the local connection immediately and keeps your data. Other devices retain their copies. A new authenticated invitation is required to connect again.",
    ),
  );
  body.append(
    button(
      "Remove device",
      async () => {
        await invoke("peer_action", {
          space,
          device: peer.identity.device_id,
          action: "remove",
        });
        await closeModal();
      },
      "danger",
    ),
  );
}
function download(text, name) {
  const blob = new Blob([text], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const link = el("a");
  link.href = url;
  link.download = name;
  document.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
function backupFlow(view) {
  const body = modal("Recovery copies");
  body.append(
    el(
      "p",
      "Encrypted copies hold logical records and exact receipt evidence. Export contains sensitive unencrypted app data: save it somewhere private. Export does not change current data or connections. Momentum needs an app-aware importer; rolling back an immutable operation log is not offered.",
    ),
  );
  if (!view.backups.length)
    body.append(el("p", "No recovery copies yet. Use Back up to create one."));
  for (const name of view.backups) {
    const row = el("div", undefined, "list-row");
    row.append(
      el("strong", new Date(Number(name.split("-")[0])).toLocaleString()),
      button(
        "Export private recovery data",
        async () => {
          const data = await invoke("export_backup", {
            space: view.space.id,
            name,
            confirmed: true,
          });
          download(data, `${view.space.manifest.display_name}-recovery.json`);
        },
        "secondary",
      ),
    );
    body.append(row);
  }
  for (const snapshot of view.recovery || []) {
    const row = el("div", undefined, "list-row");
    row.append(
      el(
        "strong",
        `Before merge · ${snapshot.records} records · revision ${snapshot.revision}`,
      ),
      button(
        "Export private pre-merge data",
        async () => {
          const data = await invoke("export_recovery", {
            space: view.space.id,
            snapshot: snapshot.id,
            confirmed: true,
          });
          download(data, `${view.space.manifest.display_name}-pre-merge.json`);
        },
        "secondary",
      ),
    );
    body.append(row);
  }
  if (view.recovery_copies)
    body.append(
      el(
        "p",
        `${view.recovery_copies} encrypted pre-merge copies remain in the session journal.`,
      ),
    );
}
$("archive").addEventListener("click", () => {
  const body = modal("Preserve previous AlwaysOn data");
  body.append(
    el(
      "p",
      "Make a private recovery copy of your old config, state and data before migrating. Legacy keys and trust remain archived only; they cannot approve new managed connections. Original files remain unchanged. Connect a compatible app to a new app space after its migration.",
    ),
  );
  body.append(
    button("Make recovery copy", async () => {
      await invoke("archive_legacy", { confirmed: true });
      await closeModal();
      message(
        "Legacy recovery copy saved privately. Original data is preserved.",
      );
    }),
  );
});
perform(refresh);
setInterval(() => perform(refresh), 2500);
setInterval(updateExpiry, 1000);
