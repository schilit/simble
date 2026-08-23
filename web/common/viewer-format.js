// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// The formatting a GATT view needs before it can draw anything: names for the
// assigned numbers, property chips, and a default decoding for the
// characteristics every Bluetooth explorer already understands.
//
// This is deliberately the *floor*, not a growing pile. A viewer that cannot
// say "72 bpm" for a Heart Rate Measurement is not a viewer, so those live
// here. A viewer that grows a decoder for every device someone invents becomes
// the seam the widget forks along, so it does not: `createGattView` takes a
// `decode` callback, and a page with its own characteristic keeps the knowledge
// of it in the page.

// Escapes quotes as well as angle brackets, because callers interpolate into
// ATTRIBUTE positions (`value="${escapeHtml(x)}"`), not just text nodes. The
// three-replace version this replaced left `"` alone, so a device name
// carrying one broke out of the attribute it was written into. Four copies of
// this function existed and only one of them got that right.
export const escapeHtml = (s) =>
  String(s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

// Assigned-number -> friendly-name table. Keys are the uppercase 16-bit hex
// forms Simble emits (uuid.to_string()).
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

/// Heart Rate Measurement (0x2A37): a flags byte, then the rate as u8 or LE
/// u16 depending on flag bit 0. Exported as a number rather than a string
/// because the Health page animates a heart at that rate — a page that needs
/// the value should not have to parse the label back out of the viewer.
export function bpmFromHex(hex) {
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

/// A human string for a known characteristic type, or null to show raw hex
/// only. Pages with their own characteristics pass `decode` to the view
/// instead of adding cases here.
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
