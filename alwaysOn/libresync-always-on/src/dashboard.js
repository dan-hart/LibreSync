"use strict";
// Reconcile only rendered content, so timestamp/receipt metadata that has no
// visible effect cannot replace controls or invalidate accessibility identities.
function domKey(node) {
  return node.nodeType === 1 ? node.getAttribute("data-key") : null;
}
function sameDomKind(a, b) {
  return (
    a.nodeType === b.nodeType &&
    a.tagName === b.tagName &&
    domKey(a) === domKey(b)
  );
}
function patchDashboardNode(current, next) {
  if (current.nodeType === 3) {
    if (current.nodeValue !== next.nodeValue)
      current.nodeValue = next.nodeValue;
    return;
  }
  for (const attribute of Array.from(current.attributes)) {
    if (current.tagName === "DETAILS" && attribute.name === "open") continue;
    if (next.getAttribute(attribute.name) === null)
      current.removeAttribute(attribute.name);
  }
  for (const attribute of Array.from(next.attributes)) {
    if (current.getAttribute(attribute.name) !== attribute.value)
      current.setAttribute(attribute.name, attribute.value);
  }
  // The stable listener reads this current action, never a stale view closure.
  if (next.action) current.action = next.action;
  reconcileDashboardChildren(current, Array.from(next.childNodes));
}
function reconcileDashboardChildren(parent, nextChildren) {
  const previous = Array.from(parent.childNodes);
  const used = new Set();
  nextChildren.forEach((next, index) => {
    const key = domKey(next);
    const current =
      key !== null
        ? previous.find((node) => !used.has(node) && sameDomKind(node, next))
        : previous.find(
            (node) =>
              !used.has(node) &&
              domKey(node) === null &&
              sameDomKind(node, next),
          );
    const node = current || next;
    if (current) patchDashboardNode(current, next);
    used.add(node);
    if (parent.childNodes[index] !== node)
      parent.insertBefore(node, parent.childNodes[index] || null);
  });
  for (const node of previous) if (!used.has(node)) parent.removeChild(node);
}
function findDashboardNode(root, predicate) {
  if (predicate(root)) return root;
  for (const child of Array.from(root.children)) {
    const found = findDashboardNode(child, predicate);
    if (found) return found;
  }
  return null;
}
function reconcileDashboard(container, views, render) {
  const active = container.ownerDocument.activeElement;
  const focused = active && container.contains(active) ? active : null;
  const ancestors = [];
  for (
    let node = focused?.parentNode;
    node && node !== container;
    node = node.parentNode
  ) {
    const key = domKey(node);
    if (key !== null) ancestors.push(key);
  }
  reconcileDashboardChildren(container, views.map(render));
  if (!focused || container.ownerDocument.activeElement === focused) return;
  if (container.contains(focused)) {
    focused.focus({ preventScroll: true });
    return;
  }
  // A revoked peer's Pause button disappears. Keep navigation in that peer's
  // row on its fresh-invitation repair control rather than dropping focus.
  const groups = [];
  let scope = container;
  for (const key of ancestors.reverse()) {
    const group = findDashboardNode(scope, (node) => domKey(node) === key);
    if (!group) break;
    groups.push(group);
    scope = group;
  }
  for (const group of groups.reverse()) {
    const replacement = findDashboardNode(
      group,
      (node) => node.tagName === "BUTTON",
    );
    if (replacement) {
      replacement.focus({ preventScroll: true });
      return;
    }
  }
}
if (typeof module !== "undefined") module.exports = { reconcileDashboard };
