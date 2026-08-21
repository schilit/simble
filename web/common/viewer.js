// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// The generic GATT viewer, shared by the playground, explorer, and lightbulb
// pages. It renders whatever GATT structure a running script/session builds —
// every service and characteristic, with property chips, decoded + raw values,
// and subscription state — from the wasm stack's `status_json`. The decoding
// tables and helpers are lifted verbatim from the scripted-device page so all
// pages decode the same known characteristic types identically.

export const escapeHtml = (s) =>
  String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

// Assigned-number -> friendly-name table for the viewer. Keys are the uppercase
// 16-bit hex forms Simble emits (uuid.to_string()).
export const UUID_NAMES = {
  "180D": "Heart Rate", "2A37": "Heart Rate Measurement", "2A38": "Body Sensor Location",
  "180F": "Battery", "2A19": "Battery Level",
  "181A": "Environmental Sensing", "2A6E": "Temperature", "2A6F": "Humidity",
  "1809": "Health Thermometer", "2A1C": "Temperature Measurement",
  "1816": "Cycling Speed and Cadence", "2A5B": "CSC Measurement", "2A5C": "CSC Feature",
  "180A": "Device Information", "2A29": "Manufacturer Name", "2A24": "Model Number",
  "2A25": "Serial Number", "2A26": "Firmware Revision",
  "1800": "Generic Access", "2A00": "Device Name", "1801": "Generic Attribute",
  "2902": "Client Characteristic Configuration",
};
export const nameFor = (uuid) => UUID_NAMES[uuid] || null;

export function bytesFromHex(hex) {
  const out = [];
  for (let i = 0; i + 1 < hex.length; i += 2) out.push(parseInt(hex.slice(i, i + 2), 16));
  return out;
}

export function bpmFromHex(hex) {
  // Heart Rate Measurement (0x2A37): flags byte, then u8 or LE u16 bpm.
  if (!hex || hex.length < 4) return null;
  const flags = parseInt(hex.slice(0, 2), 16);
  if (flags & 0x01) {
    if (hex.length < 6) return null;
    return parseInt(hex.slice(2, 4), 16) | (parseInt(hex.slice(4, 6), 16) << 8);
  }
  return parseInt(hex.slice(2, 4), 16);
}

function bodySensorLocation(n) {
  return ["Other", "Chest", "Wrist", "Finger", "Hand", "Ear Lobe", "Foot"][n] ?? `location ${n}`;
}

// If every byte is printable ASCII, show it as text (manufacturer/model names).
function autoText(bytes) {
  if (!bytes.length) return null;
  if (bytes.every((c) => c >= 0x20 && c <= 0x7e)) {
    return `"${String.fromCharCode(...bytes)}"`;
  }
  return null;
}

// Returns a human string for a known characteristic type, or null to fall back
// to hex / auto text.
export function decodeValue(uuid, hex) {
  if (!hex) return null;
  const b = bytesFromHex(hex);
  switch (uuid) {
    case "2A37": { const bpm = bpmFromHex(hex); return bpm == null ? null : `${bpm} bpm`; }
    case "2A19": return b.length ? `${b[0]}%` : null;
    case "2A38": return b.length ? bodySensorLocation(b[0]) : null;
    case "2A6E": { // Temperature, sint16 LE, 0.01 C
      if (b.length < 2) return null;
      let v = b[0] | (b[1] << 8); if (v & 0x8000) v -= 0x10000;
      return `${(v / 100).toFixed(2)} °C`;
    }
    case "2A6F": { // Humidity, uint16 LE, 0.01 %
      if (b.length < 2) return null;
      return `${((b[0] | (b[1] << 8)) / 100).toFixed(1)} %`;
    }
    default: return autoText(b);
  }
}

export function propChips(props, subscribed) {
  const chips = [];
  if (props & 0x02) chips.push("R");
  if (props & (0x08 | 0x04)) chips.push("W");
  if (props & 0x10) chips.push("N");
  if (props & 0x20) chips.push("I");
  if (props & 0x01) chips.push("B");
  return chips
    .map((c) => `<span class="prop${subscribed && (c === "N" || c === "I") ? " sub" : ""}">${c}</span>`)
    .join(" ");
}

// The inner HTML of a characteristic's value cell (decoded + raw hex).
function valInnerHtml(c) {
  const decoded = decodeValue(c.uuid, c.value);
  return c.value
    ? `${decoded ? `<span class="decoded">${escapeHtml(decoded)}</span>` : ""}<span class="raw">${c.value}</span>`
    : `<span class="raw">—</span>`;
}

// A signature of the GATT *structure* — services, characteristics, properties,
// and subscription state, but not the fast-changing values. The DOM is rebuilt
// only when this changes; values are patched in place otherwise.
function structureSig(services) {
  return JSON.stringify(services.map((s) => [
    s.uuid,
    s.characteristics.map((c) => [c.uuid, c.properties, c.subscribed]),
  ]));
}

function buildCards(gattEl, services) {
  const cards = [];
  for (const service of services) {
    const sName = nameFor(service.uuid);
    const head = sName
      ? `${escapeHtml(sName)}<span class="u">0x${service.uuid}</span>`
      : `<span class="u">Service 0x${service.uuid}</span>`;
    const rows = [];
    for (const c of service.characteristics) {
      const key = `${service.uuid}/${c.uuid}`;
      const cName = nameFor(c.uuid);
      const nameHtml = cName
        ? `<span class="chr-name">${escapeHtml(cName)}</span><span class="chr-uuid">0x${c.uuid}</span>`
        : `<span class="chr-name chr-uuid">0x${c.uuid}</span>`;
      const subNote = c.subscribed ? `<span class="sub-note">⚡ subscribed</span>` : "";
      rows.push(
        `<div class="chr" data-key="${key}">
          <div class="chr-top">${nameHtml} ${propChips(c.properties, c.subscribed)}${subNote}</div>
          <div class="chr-val">${valInnerHtml(c)}</div>
        </div>`
      );
    }
    cards.push(`<div class="svc"><div class="svc-head">${head}</div>${rows.join("")}</div>`);
  }
  gattEl.innerHTML = cards.join("");
  const valNodes = new Map();
  for (const el of gattEl.querySelectorAll(".chr")) {
    valNodes.set(el.dataset.key, el.querySelector(".chr-val"));
  }
  return valNodes;
}

// Renders the GATT structure of `status` into `gattEl`. `prevValues` is a
// caller-owned Map ("service/char" -> last hex) used to detect and pulse the
// characteristic that most recently changed. Returns the key that changed this
// render (or null), so callers can drive their own animation.
//
// To avoid flicker, the DOM subtree is rebuilt only when the GATT *structure*
// changes; on every other call only the changed value cells are patched. The
// per-element view state (structure signature + cached value nodes) is stashed
// on `gattEl` itself so the function stays stateless from the caller's side.
export function renderGatt(gattEl, status, prevValues) {
  const services = status.services || [];
  if (!services.length) {
    if (gattEl._simbleSig !== "empty") {
      gattEl.innerHTML = `<p class="viewer-empty">No services yet.</p>`;
      gattEl._simbleSig = "empty";
      gattEl._simbleVal = null;
    }
    return null;
  }

  const sig = structureSig(services);
  const rebuilt = sig !== gattEl._simbleSig;
  if (rebuilt) {
    gattEl._simbleVal = buildCards(gattEl, services);
    gattEl._simbleSig = sig;
  }
  const valNodes = gattEl._simbleVal;

  let changedKey = null;
  for (const service of services) {
    for (const c of service.characteristics) {
      const key = `${service.uuid}/${c.uuid}`;
      if (prevValues && prevValues.has(key) && prevValues.get(key) !== c.value) {
        changedKey = key;
        const cell = valNodes.get(key);
        if (cell) cell.innerHTML = valInnerHtml(c);
      }
      if (prevValues) prevValues.set(key, c.value);
    }
  }

  // Don't pulse on the tick the row was just built, or it flashes on load.
  if (changedKey && !rebuilt) {
    const el = gattEl.querySelector(`.chr[data-key="${CSS.escape(changedKey)}"]`);
    if (el) { el.classList.remove("pulse"); void el.offsetWidth; el.classList.add("pulse"); }
  }
  return changedKey;
}
