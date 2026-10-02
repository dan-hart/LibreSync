const test = require("node:test");
const assert = require("node:assert/strict");
const { reconcileDashboard } = require("../src/dashboard.js");
// A small DOM double exercises the same mutation API as the native webview.
class Node {
  constructor(tag, document, text = null) {
    this.nodeType = text === null ? 1 : 3;
    this.tagName = tag;
    this.nodeValue = text;
    this.ownerDocument = document;
    this.childNodes = [];
    this.attributes = [];
    this.parentNode = null;
  }
  get children() {
    return this.childNodes.filter((n) => n.nodeType === 1);
  }
  getAttribute(name) {
    return this.attributes.find((a) => a.name === name)?.value ?? null;
  }
  setAttribute(name, value) {
    const old = this.attributes.find((a) => a.name === name);
    if (old) old.value = value;
    else this.attributes.push({ name, value });
  }
  removeAttribute(name) {
    this.attributes = this.attributes.filter((a) => a.name !== name);
  }
  contains(node) {
    return this === node || this.childNodes.some((n) => n.contains(node));
  }
  insertBefore(node, before) {
    if (node.parentNode) node.parentNode.removeChild(node);
    const index = before
      ? this.childNodes.indexOf(before)
      : this.childNodes.length;
    this.childNodes.splice(index, 0, node);
    node.parentNode = this;
  }
  removeChild(node) {
    if (node.contains(this.ownerDocument.activeElement))
      this.ownerDocument.activeElement = null;
    this.childNodes.splice(this.childNodes.indexOf(node), 1);
    node.parentNode = null;
  }
  replaceChildren(...nodes) {
    for (const node of [...this.childNodes]) this.removeChild(node);
    for (const node of nodes) this.insertBefore(node, null);
  }
  focus() {
    this.ownerDocument.activeElement = this;
  }
}
function fixture() {
  const doc = { activeElement: null };
  const container = new Node("SECTION", doc);
  function render(view) {
    const card = new Node("ARTICLE", doc);
    card.setAttribute("data-key", view.id);
    const label = new Node("P", doc);
    label.insertBefore(new Node(null, doc, view.status), null);
    const control = new Node("BUTTON", doc);
    control.setAttribute("data-key", view.controlKey || "connect");
    control.action = () => view.revision;
    card.insertBefore(label, null);
    card.insertBefore(control, null);
    return card;
  }
  return { doc, container, render };
}
test("unchanged polling preserves card, AX control identity, and focus despite timestamps", () => {
  const { doc, container, render } = fixture();
  reconcileDashboard(
    container,
    [{ id: "a", status: "Waiting", revision: 1, last_seen: 1 }],
    render,
  );
  const card = container.children[0],
    control = card.children[1];
  control.focus();
  reconcileDashboard(
    container,
    [{ id: "a", status: "Waiting", revision: 1, last_seen: 2 }],
    render,
  );
  assert.equal(container.children[0], card);
  assert.equal(card.children[1], control);
  assert.equal(doc.activeElement, control);
});
test("meaningful status changes patch text while retaining focus and fresh actions", () => {
  const { doc, container, render } = fixture();
  reconcileDashboard(
    container,
    [{ id: "a", status: "Waiting", revision: 1 }],
    render,
  );
  const card = container.children[0],
    control = card.children[1];
  control.focus();
  reconcileDashboard(
    container,
    [{ id: "a", status: "Stored", revision: 2 }],
    render,
  );
  assert.equal(card.children[0].childNodes[0].nodeValue, "Stored");
  assert.equal(doc.activeElement, control);
  assert.equal(control.action(), 2);
});

test("revoked control removal retains focus on its replacement repair action", () => {
  const { doc, container, render } = fixture();
  reconcileDashboard(
    container,
    [{ id: "a", status: "Paused", controlKey: "toggle" }],
    render,
  );
  container.children[0].children[1].focus();
  reconcileDashboard(
    container,
    [{ id: "a", status: "Removed", controlKey: "repair" }],
    render,
  );
  assert.equal(doc.activeElement, container.children[0].children[1]);
  assert.equal(doc.activeElement.getAttribute("data-key"), "repair");
});
test("reordering cards preserves their identities and restores the same focused control", () => {
  const { doc, container, render } = fixture();
  const a = { id: "a", status: "Waiting" },
    b = { id: "b", status: "Stored" };
  reconcileDashboard(container, [a, b], render);
  const card = container.children[1],
    control = card.children[1];
  control.focus();
  reconcileDashboard(container, [b, a], render);
  assert.equal(container.children[0], card);
  assert.equal(doc.activeElement, control);
});

test("polling retains the user’s expanded technical Details", () => {
  const { container, render } = fixture();
  const withDetails = (view) => {
    const card = render(view);
    const details = new Node("DETAILS", container.ownerDocument);
    card.insertBefore(details, null);
    return card;
  };
  reconcileDashboard(container, [{ id: "a", status: "Waiting" }], withDetails);
  const details = container.children[0].children[2];
  details.setAttribute("open", "");
  reconcileDashboard(container, [{ id: "a", status: "Stored" }], withDetails);
  assert.equal(container.children[0].children[2], details);
  assert.equal(details.getAttribute("open"), "");
});
