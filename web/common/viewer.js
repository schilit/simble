// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// The original GATT viewer entry point, now a thin adapter over the shared
// widget in gatt-view.js.
//
// It stays because seven pages import `renderGatt(el, status, prevValues)` and
// four of them are tooling rather than device pages. Rather than edit all of
// them, `renderGatt` now creates a `createGattView({ mode: "server" })` inside
// the element it is handed and keeps it there, so every page on the site draws
// a server-side database with the same code — which was the point of unifying
// it. New code should call `createGattView` directly: it also does the
// discovered-tree view, with the per-characteristic operations the server view
// has no business offering.

import { createGattView } from "./gatt-view.js";

export {
  escapeHtml,
  UUID_NAMES,
  nameFor,
  bytesFromHex,
  bpmFromHex,
  decodeValue,
  propChips,
} from "./viewer-format.js";

export { createGattView, promptForBytes } from "./gatt-view.js";

/**
 * Renders a peripheral's own GATT database into `gattEl`.
 *
 * @param {HTMLElement} gattEl the container the caller owns.
 * @param {object} status a parsed `peripheral_status_json`.
 * @param {Map} [prevValues] historical: the view now keeps its own value
 *        history, so this is only kept up to date for callers that still read
 *        it. Passing it is no longer necessary.
 * @returns {?string} the "service/characteristic" key whose value just changed.
 */
export function renderGatt(gattEl, status, prevValues) {
  let view = gattEl._simbleView;
  // Some callers clear their own container between renders; a view whose
  // element is no longer in it would quietly update a detached node.
  if (view && view.el.parentNode !== gattEl) view = null;
  if (!view) {
    view = createGattView({ mode: "server" });
    gattEl.innerHTML = "";
    gattEl.append(view.el);
    gattEl._simbleView = view;
  }
  const changed = view.update(status);
  if (prevValues) {
    for (const service of status?.services || []) {
      for (const c of service.characteristics || []) {
        prevValues.set(`${service.uuid}/${c.uuid}`, c.value);
      }
    }
  }
  return changed;
}
