// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// The GATT database widget, in both of the views a device page ever needs.
//
// There are genuinely two, and conflating them is what made this code fork
// four ways across the site:
//
//   "server"  — the database a peripheral *holds*. Read-only, values live. What
//               the device knows about itself, straight out of its own
//               attribute table.
//   "client"  — the tree a central *discovered*, in the shape nRF Connect shows
//               it: attribute handles are visible, because a client addresses
//               things by handle and a server does not, and each characteristic
//               offers the operations the client can actually perform on it.
//
// The client view's buttons are capability-gated twice over: a characteristic
// without the Read property gets no read button, and a page that supplied no
// `onRead` gets none either. A button that issues nothing is worse than no
// button, because it teaches that the operation happened.
//
// Both views rebuild their DOM only when the GATT *structure* changes — a
// signature over services, characteristics, properties and subscription state.
// Values are patched in place on every other update. Two of the forked
// renderers rewrote their whole subtree ten times a second, which flickers, and
// in the client view also meant the buttons were destroyed and rebuilt under
// the reader's pointer.
//
// Decoding is PLUGGABLE. The built-in table knows the SIG-assigned
// characteristics any Bluetooth explorer knows — battery level, temperature,
// heart rate — and that much belongs in a viewer. What does not belong here is
// the next device's decoder, and the next one after that: each page that
// invents a characteristic passes its own `decode` and keeps its profile
// knowledge in the page that has the profile.
//
// Styles live in common/simble.css (`.gatt-view`), which every page links.

import { escapeHtml, nameFor, decodeValue, propChips } from "./viewer-format.js";

export { escapeHtml, nameFor, decodeValue, propChips };

/** GATT characteristic property bits (Core, Vol 3 Part G, 3.3.1.1). */
const PROP_READ = 0x02;
const PROP_WRITE_ANY = 0x0c; // write with response | write without response
const PROP_NOTIFY_ANY = 0x30; // notify | indicate

/**
 * Builds a GATT view.
 *
 * @param {object} [options]
 * @param {"server"|"client"} [options.mode]  which of the two views. Default
 *        "server".
 * @param {function} [options.decode]  (characteristic) => string | null |
 *        undefined. Return a string to render it as the decoded value, null to
 *        show only raw hex, or undefined to fall through to the built-in
 *        SIG table. This is the seam that keeps per-profile decoders in the
 *        pages that own the profile.
 * @param {string} [options.empty]  what to say when the device has no services.
 * @param {function} [options.onRead]  (characteristic) => void. Client view
 *        only; omit and no read button is offered.
 * @param {function} [options.onWrite]  (characteristic) => void.
 * @param {function} [options.onSubscribe]  (characteristic) => void.
 * @returns {object} { el, update, destroy }
 */
export function createGattView(options = {}) {
  const {
    mode = "server",
    decode = null,
    empty = "No services yet.",
    onRead = null,
    onWrite = null,
    onSubscribe = null,
  } = options;

  const el = document.createElement("div");
  el.className = `gatt-view ${mode}`;

  // Structure signature and the value cells cached from the last rebuild. Kept
  // on the closure rather than on the element, so two views over the same
  // container cannot read each other's cache.
  let sig = null;
  let cells = new Map(); // "service/characteristic" -> the .chr-val element
  let chars = new Map(); // value handle -> the characteristic as last seen

  // The view owns its own value history, so it can patch only what changed.
  // Callers used to pass this Map in, and several passed the *same* Map for two
  // devices — a value changing on the server then suppressed the client's
  // update for the same characteristic.
  const lastValues = new Map();

  // One delegated listener for every action button, so a rebuild cannot leak
  // listeners and destroy() has exactly one thing to remove.
  const onClick = (event) => {
    const button = event.target.closest("button[data-op]");
    if (!button || !el.contains(button)) return;
    const characteristic = chars.get(Number(button.dataset.h));
    if (!characteristic) return;
    if (button.dataset.op === "read") onRead?.(characteristic);
    else if (button.dataset.op === "write") onWrite?.(characteristic);
    else if (button.dataset.op === "sub") onSubscribe?.(characteristic);
  };
  if (mode === "client") el.addEventListener("click", onClick);

  function valueHtml(c) {
    const custom = decode ? decode(c) : undefined;
    const decoded = custom === undefined ? decodeValue(c.uuid, c.value) : custom;
    if (!c.value) return `<span class="raw">${mode === "client" ? "— not read —" : "—"}</span>`;
    return (
      (decoded ? `<span class="decoded">${escapeHtml(decoded)}</span>` : "") +
      `<span class="raw">${escapeHtml(c.value)}</span>`
    );
  }

  function actionsHtml(c) {
    if (mode !== "client") return "";
    const p = c.properties;
    const buttons = [];
    if (onRead && p & PROP_READ) {
      buttons.push(`<button data-op="read" data-h="${c.value_handle}">read ↓</button>`);
    }
    if (onWrite && p & PROP_WRITE_ANY) {
      buttons.push(`<button data-op="write" data-h="${c.value_handle}">write ↑</button>`);
    }
    if (onSubscribe && p & PROP_NOTIFY_ANY) {
      buttons.push(
        `<button data-op="sub" data-h="${c.value_handle}" class="${c.subscribed ? "on" : ""}">` +
          `${c.subscribed ? "🔔 subscribed" : "subscribe 🔔"}</button>`,
      );
    }
    return buttons.length ? `<div class="chr-actions">${buttons.join(" ")}</div>` : "";
  }

  // Services, characteristics, properties and subscription state — everything
  // except the values, which change constantly and are patched rather than
  // rebuilt.
  function structureSig(services) {
    return JSON.stringify(
      services.map((s) => [
        s.uuid,
        (s.characteristics || []).map((c) => [c.uuid, c.properties, c.subscribed, c.value_handle]),
      ]),
    );
  }

  function build(services) {
    el.innerHTML = services
      .map((s) => {
        const serviceName = nameFor(s.uuid);
        const head = serviceName
          ? `${escapeHtml(serviceName)}<span class="u">0x${escapeHtml(s.uuid)}</span>`
          : `<span class="u">Service 0x${escapeHtml(s.uuid)}</span>`;
        const rows = (s.characteristics || [])
          .map((c) => {
            const key = `${s.uuid}/${c.uuid}`;
            const charName = nameFor(c.uuid);
            const nameHtml = charName
              ? `<span class="chr-name">${escapeHtml(charName)}</span>` +
                `<span class="chr-uuid">0x${escapeHtml(c.uuid)}</span>`
              : `<span class="chr-name chr-uuid">0x${escapeHtml(c.uuid)}</span>`;
            // The handle is the client's whole vocabulary — it reads and writes
            // by handle, never by UUID — so the discovered tree shows it and
            // the server's own view does not.
            const handle =
              mode === "client" && c.value_handle !== undefined
                ? `<span class="chr-h">handle 0x${c.value_handle
                    .toString(16)
                    .padStart(4, "0")}</span>`
                : "";
            const subNote =
              mode === "server" && c.subscribed
                ? `<span class="sub-note">⚡ subscribed</span>`
                : "";
            return (
              `<div class="chr" data-key="${escapeHtml(key)}">` +
              `<div class="chr-top">${nameHtml} ${propChips(c.properties, c.subscribed)}` +
              `${subNote}${handle}</div>` +
              `<div class="chr-val">${valueHtml(c)}</div>${actionsHtml(c)}</div>`
            );
          })
          .join("");
        return `<div class="svc"><div class="svc-head">${head}</div>${rows}</div>`;
      })
      .join("");

    cells = new Map();
    for (const node of el.querySelectorAll(".chr")) {
      cells.set(node.dataset.key, node.querySelector(".chr-val"));
    }
  }

  function showMessage(text, marker) {
    if (sig === marker) return;
    el.innerHTML = `<p class="viewer-empty">${escapeHtml(text)}</p>`;
    sig = marker;
    cells = new Map();
    chars = new Map();
  }

  /**
   * Renders `status` — a parsed `peripheral_status_json` / `central_status_json`
   * — into the view.
   *
   * @param {object} status the device's own status object.
   * @returns {?string} the "service/characteristic" key whose value changed on
   *        this update, so a caller can drive its own animation, or null.
   */
  function update(status) {
    // A central that has not finished connecting has nothing to show, and
    // saying "no services" would blame the peer for the client's own progress.
    if (mode === "client") {
      if (!status || !status.connected) {
        showMessage(status?.phase ? `Connecting — ${status.phase}…` : "Connecting…", "@connecting");
        return null;
      }
      if (!status.services || !status.services.length) {
        showMessage(`Connected — ${status.phase || "discovering"}…`, "@discovering");
        return null;
      }
    }

    const services = (status && status.services) || [];
    if (!services.length) {
      showMessage(empty, "@empty");
      return null;
    }

    const next = structureSig(services);
    if (next !== sig) {
      build(services);
      sig = next;
      chars = new Map();
      for (const s of services) {
        for (const c of s.characteristics || []) chars.set(c.value_handle, c);
      }
    }

    let changedKey = null;
    for (const s of services) {
      for (const c of s.characteristics || []) {
        chars.set(c.value_handle, c);
        const key = `${s.uuid}/${c.uuid}`;
        const previous = lastValues.get(key);
        if (previous !== undefined && previous !== c.value) {
          changedKey = key;
          const cell = cells.get(key);
          if (cell) cell.innerHTML = valueHtml(c);
        }
        lastValues.set(key, c.value);
      }
    }
    return changedKey;
  }

  function destroy() {
    if (mode === "client") el.removeEventListener("click", onClick);
    el.remove();
    cells = new Map();
    chars = new Map();
    lastValues.clear();
    sig = null;
  }

  return { el, update, destroy };
}

/**
 * The get-or-create form, for callers that own a container and render into it
 * on a timer rather than holding a view of their own. The view is stashed on
 * the element, and rebuilt if the caller has cleared its container in the
 * meantime — otherwise the next update would quietly patch a detached node.
 *
 * @param {HTMLElement} el the caller's container.
 * @param {object} [options] passed to createGattView on first use only.
 * @returns {object} the view.
 */
export function gattViewFor(el, options) {
  let view = el._simbleView;
  if (view && view.el.parentNode !== el) view = null;
  if (!view) {
    view = createGattView(options);
    el.innerHTML = "";
    el.append(view.el);
    el._simbleView = view;
  }
  return view;
}

/**
 * Asks the reader for bytes to write, in the hex form the rest of the site
 * prints them in. Returns a Uint8Array, or null if they cancelled.
 *
 * Lives beside the widget rather than inside it: the widget reports that a
 * write was asked for, and the page decides how the bytes are obtained — a
 * colour picker and a volume slider are also "write" buttons.
 */
export function promptForBytes(label = "Bytes to write (hex, space-separated, e.g. 00 5A):") {
  const input = prompt(label, "00");
  if (input == null) return null;
  const bytes = input
    .trim()
    .split(/\s+/)
    .map((x) => parseInt(x, 16))
    .filter((n) => !Number.isNaN(n));
  return bytes.length ? new Uint8Array(bytes) : null;
}
