const statusEl = document.getElementById("status");
const todoListEl = document.getElementById("todo-list");
const todoForm = document.getElementById("todo-form");
const todoInput = document.getElementById("todo-input");
const refreshTodosBtn = document.getElementById("refresh-todos");
const discoverBtn = document.getElementById("discover");
const manualAddressInput = document.getElementById("manual-address");
const manualLinkBtn = document.getElementById("manual-link");
const manualSyncBtn = document.getElementById("manual-sync");
const discoveredListEl = document.getElementById("discovered-list");
const linkedListEl = document.getElementById("linked-list");

function getInvoke() {
  const tauri = window.__TAURI__;
  if (tauri?.core?.invoke) {
    return tauri.core.invoke;
  }
  if (tauri?.tauri?.invoke) {
    return tauri.tauri.invoke;
  }
  throw new Error("Tauri API not available. Enable withGlobalTauri in tauri.conf.json");
}

function getEventApi() {
  const tauri = window.__TAURI__;
  return tauri?.event || tauri?.tauri?.event;
}

const invoke = (cmd, args = {}) => getInvoke()(cmd, args);

function setStatus(message, tone = "info") {
  statusEl.innerHTML = "";
  const line = document.createElement("div");
  line.className = `status-line ${tone}`;
  line.textContent = message;
  statusEl.appendChild(line);
}

function formatAddress(address) {
  return address || "—";
}

function formatLastSeen(secs) {
  if (!secs) return "—";
  const date = new Date(secs * 1000);
  return date.toLocaleString();
}

async function loadStatus() {
  try {
    const status = await invoke("app_status");
    statusEl.innerHTML = `
      <div class="status-line">Device: <strong>${status.device_id}</strong></div>
      <div class="status-line">User: ${status.user_id}</div>
      <div class="status-line">Fingerprint: ${status.fingerprint}</div>
      <div class="status-line">Listener: ${status.listener_addr ?? "—"}</div>
      <div class="status-line">Linked devices: ${status.linked_devices}</div>
    `;
  } catch (error) {
    setStatus(error.toString(), "error");
  }
}

function renderTodos(todos) {
  todoListEl.innerHTML = "";

  if (!todos.length) {
    const empty = document.createElement("li");
    empty.className = "todo-empty";
    empty.textContent = "Nothing here yet. Add your first task.";
    todoListEl.appendChild(empty);
    return;
  }

  todos.forEach((todo) => {
    const item = document.createElement("li");
    item.className = "todo-item";

    const checkbox = document.createElement("input");
    checkbox.type = "checkbox";
    checkbox.checked = todo.completed;

    const title = document.createElement("input");
    title.type = "text";
    title.value = todo.title;
    title.className = todo.completed ? "completed" : "";

    const saveBtn = document.createElement("button");
    saveBtn.textContent = "Save";

    const deleteBtn = document.createElement("button");
    deleteBtn.textContent = "Delete";
    deleteBtn.className = "ghost";

    async function update() {
      const updated = await invoke("update_todo", {
        id: todo.id,
        title: title.value.trim(),
        completed: checkbox.checked,
      });
      renderTodos(updated);
    }

    checkbox.addEventListener("change", async () => {
      await update();
    });

    title.addEventListener("keydown", async (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        await update();
      }
    });

    saveBtn.addEventListener("click", async () => {
      await update();
    });

    deleteBtn.addEventListener("click", async () => {
      const updated = await invoke("delete_todo", { id: todo.id });
      renderTodos(updated);
    });

    item.appendChild(checkbox);
    item.appendChild(title);
    item.appendChild(saveBtn);
    item.appendChild(deleteBtn);
    todoListEl.appendChild(item);
  });
}

async function loadTodos() {
  try {
    const todos = await invoke("get_todos");
    renderTodos(todos);
  } catch (error) {
    setStatus(error.toString(), "error");
  }
}

async function refreshDevices() {
  try {
    const discovered = await invoke("discover_devices", { timeout_secs: 3 });
    const linked = await invoke("list_devices");
    renderDeviceList(discoveredListEl, discovered, "discover");
    renderDeviceList(linkedListEl, linked, "linked");
  } catch (error) {
    setStatus(error.toString(), "error");
  }
}

async function setupSyncEvents() {
  const eventApi = getEventApi();
  if (!eventApi?.listen) return;
  await eventApi.listen("sync_event", async (event) => {
    const payload = event?.payload;
    if (!payload) return;
    if (payload.kind === "finished") {
      await loadTodos();
      await loadStatus();
    }
    if (payload.kind === "error" && payload.message) {
      setStatus(payload.message, "error");
    }
  });
}

function renderDeviceList(container, devices, mode) {
  container.innerHTML = "";

  if (!devices.length) {
    const empty = document.createElement("div");
    empty.className = "device-empty";
    empty.textContent = "No devices yet.";
    container.appendChild(empty);
    return;
  }

  devices.forEach((device) => {
    const card = document.createElement("div");
    card.className = "device-card";

    const header = document.createElement("div");
    header.className = "device-header";
    header.innerHTML = `
      <div>
        <strong>${device.device_id}</strong>
        <div class="device-sub">${device.user_id}</div>
      </div>
      <span class="chip ${device.linked ? "paired" : "new"}">
        ${device.linked ? "linked" : "available"}
      </span>
    `;

    const meta = document.createElement("div");
    meta.className = "device-meta";
    meta.innerHTML = `
      <div>Address: ${formatAddress(device.address)}</div>
      <div>Last seen: ${formatLastSeen(device.last_seen_unix_secs)}</div>
      <div class="fingerprint">Fingerprint: ${device.fingerprint || "—"}</div>
    `;

    const actions = document.createElement("div");
    actions.className = "device-actions-row";

    if (mode === "discover" && device.address) {
      const linkBtn = document.createElement("button");
      linkBtn.textContent = "Link";
      linkBtn.addEventListener("click", async () => {
        await invoke("link_device", { address: device.address });
        await refreshDevices();
        await loadStatus();
      });

      const syncBtn = document.createElement("button");
      syncBtn.textContent = "Sync";
      syncBtn.className = "ghost";
      syncBtn.addEventListener("click", async () => {
        const result = await invoke("sync_device", { address: device.address });
        renderTodos(result.todos);
        await loadStatus();
      });

      actions.appendChild(linkBtn);
      actions.appendChild(syncBtn);
    }

    if (mode === "linked") {
      const revokeBtn = document.createElement("button");
      revokeBtn.textContent = "Revoke";
      revokeBtn.className = "ghost";
      revokeBtn.addEventListener("click", async () => {
        await invoke("revoke_device", { device_id: device.device_id });
        await refreshDevices();
        await loadStatus();
      });
      actions.appendChild(revokeBtn);
    }

    card.appendChild(header);
    card.appendChild(meta);
    if (mode !== "none") {
      card.appendChild(actions);
    }
    container.appendChild(card);
  });
}

todoForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const title = todoInput.value.trim();
  if (!title) return;
  const todos = await invoke("add_todo", { title });
  todoInput.value = "";
  renderTodos(todos);
});

refreshTodosBtn.addEventListener("click", loadTodos);

discoverBtn.addEventListener("click", refreshDevices);

manualLinkBtn.addEventListener("click", async () => {
  const address = manualAddressInput.value.trim();
  if (!address) return;
  await invoke("link_device", { address });
  await refreshDevices();
  await loadStatus();
});

manualSyncBtn.addEventListener("click", async () => {
  const address = manualAddressInput.value.trim();
  if (!address) return;
  const result = await invoke("sync_device", { address });
  renderTodos(result.todos);
  await loadStatus();
});

(async () => {
  await loadStatus();
  await loadTodos();
  await refreshDevices();
  await setupSyncEvents();
})();
