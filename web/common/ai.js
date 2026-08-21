// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// The "Generate with AI" prompt and suggestion chips — the generic "make any
// device" authoring aid. The prompt text and chips are the single source of
// truth for LLM device authoring; the scripted-device page (web/hrm/) keeps a
// byte-identical inline copy for its own runtime, and the Playground imports
// this module. Keep the two in sync — do not let the prompt text diverge.
//
// The worked example builds a HEART-RATE MONITOR — on purpose a DIFFERENT
// device from the pages' on-screen defaults — so pasting the AI's result yields
// something visibly different from what's already running.

import { escapeHtml } from "./viewer.js";

export const AI_PROMPT = `You write Rhai scripts that define virtual Bluetooth LE peripherals for Simble (a Rust BLE simulator that runs the script in a web page). Reply with ONLY a Rhai script in a single code block — no explanations.

RHAI IS NOT RUST:
- \`let x = ...;\` declares everything: no types, no \`mut\`, no \`::new()\`.
- Constructors are plain calls of the type name: \`android::BluetoothGattServer("name")\`.
- Byte payloads are arrays of integers: \`[0x00, 72]\`. Strings use "double quotes". Comments use //.
- No imports, no crates. NO infinite loops, NO sleep, NO blocking waits — the script body runs ONCE to build the device.

RUNTIME MODEL (the web page hosts the device):
- The script body must create a server and keep it in a top-level variable:
    let server = android::BluetoothGattServer("my-device");
- Optionally define \`fn tick(server, t)\` — the page calls it ~10 times per second; \`t\` is seconds since the script was run (a float). IMPORTANT: Rhai functions are encapsulated and CANNOT see top-level variables — use only the \`server\` and \`t\` parameters, and keep tick stateless (derive everything from \`t\`: \`sin(t)\`, \`t % 5.0\`, \`(2.0*t).to_int()\`...).
- \`server.update_value(uuid, [bytes])\` (web-runtime extension) writes a characteristic's value into the live GATT database; the page automatically sends a real BLE notification to any subscribed central when the value changes. This is the preferred way to animate values from tick().
- Advertising (device name + 16-bit service UUIDs) is derived from the server you build and issued by the page — do not try to advertise from the script.

API SURFACE (all real, backed by Simble's GATT stack):
- android::BluetoothGattServer(name) -> server
- android::BluetoothGattService(uuid, android::SERVICE_TYPE_PRIMARY) -> svc
- android::BluetoothGattCharacteristic(uuid, properties, permissions) -> chr
- android::BluetoothGattDescriptor(uuid, permissions) -> desc
- chr.set_value([bytes]) / chr.get_value() / chr.value / chr.add_descriptor(desc)
- svc.add_characteristic(chr) / svc.get_characteristic(uuid)
- server.add_service(svc) / server.get_service(uuid) / server.name
- server.notify_characteristic_changed(device, chr, confirm) — needs a connected \`device\` taken from an event; in this web runtime prefer server.update_value.
- server.send_response(device, request_id, status, offset, value)
- take_events() or server.take_events() -> array of event maps {event, server, device, uuid, value, request_id, offset, status, mtu, response_needed}. Event kinds: "connected", "disconnected", "service_added", "characteristic_read", "characteristic_write", "descriptor_read", "descriptor_write", "notification_sent", "mtu_changed". Call inside tick() to react to peer writes.
- wait_for "connected" { /* \`event\` is bound here */ } — consumes queued events, ERRORS if none is pending; use in tests, not in tick().
- assert(condition, "message")

CONSTANTS:
- android::PROPERTY_READ, PROPERTY_WRITE, PROPERTY_WRITE_NO_RESPONSE, PROPERTY_NOTIFY, PROPERTY_INDICATE, PROPERTY_BROADCAST (combine with |)
- android::PERMISSION_READ, PERMISSION_WRITE (plus _ENCRYPTED / _MITM variants)
- android::SERVICE_TYPE_PRIMARY, SERVICE_TYPE_SECONDARY; android::GATT_SUCCESS, GATT_FAILURE
- uuid::HEART_RATE_SERVICE, uuid::HEART_RATE_MEASUREMENT, uuid::BODY_SENSOR_LOCATION, uuid::BATTERY_SERVICE, uuid::BATTERY_LEVEL, uuid::CLIENT_CHARACTERISTIC_CONFIGURATION, uuid::MANUFACTURER_NAME, uuid::MODEL_NUMBER, uuid::SERIAL_NUMBER (Device Information), and more.
- Any other UUID: uuid::of("2A6E") for a 16-bit assigned number, or uuid::of("12345678-1234-5678-1234-56789abcdef0") for a custom 128-bit UUID. Use uuid::of for anything without a named constant (e.g. Environmental Sensing 181A, Temperature 2A6E, Humidity 2A6F, Cycling Speed and Cadence 1816 / CSC Measurement 2A5B).

RULES:
- Every notify-capable characteristic MUST attach a CCCD descriptor, or centrals cannot subscribe and the runtime will not notify:
    let cccd = android::BluetoothGattDescriptor(uuid::CLIENT_CHARACTERISTIC_CONFIGURATION, android::PERMISSION_READ | android::PERMISSION_WRITE);
    chr.add_descriptor(cccd);
- Standard encodings:
    Heart Rate Measurement (2A37) = [flags, bpm], flags 0x00 for an 8-bit bpm.
    Battery Level (2A19) = one byte, 0-100.
    Temperature (2A6E, Environmental Sensing) = signed 16-bit little-endian, hundredths of a degree C: 21.5C -> value 2150 -> [2150 & 0xFF, (2150 >> 8) & 0xFF].
    Humidity (2A6F) = unsigned 16-bit little-endian, hundredths of a percent.

COMPLETE WORKED EXAMPLE (a heart-rate monitor whose bpm breathes over time):
\`\`\`rhai
let server = android::BluetoothGattServer("web-hrm");

let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
let hr = android::BluetoothGattCharacteristic(
    uuid::HEART_RATE_MEASUREMENT,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY,
    android::PERMISSION_READ,
);
hr.set_value([0x00, 72]);
let cccd = android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE,
);
hr.add_descriptor(cccd);
hrs.add_characteristic(hr);
server.add_service(hrs);

fn tick(server, t) {
    let bpm = 76 + (12.0 * sin(t / 4.0)).to_int();
    server.update_value(uuid::HEART_RATE_MEASUREMENT, [0x00, bpm]);
}
\`\`\`

MY DEVICE REQUEST:
`;

// Clickable suggestions that seed the "MY DEVICE REQUEST:" line. Each is
// buildable with the bindings above (named uuid::* consts, or uuid::of for the
// rest). One is picked at random on load so a first-time visitor always has an
// interesting, non-default device to generate in a single click.
export const SUGGESTIONS = [
  { label: "🔋 battery monitor",
    request: "a battery monitor: a Battery Service with a Battery Level characteristic (uuid::BATTERY_LEVEL, notify + a CCCD) whose percentage slowly drains from 100 toward 5 and then jumps back to full." },
  { label: "🚴 cycling speed sensor",
    request: "a cycling speed and cadence sensor: service uuid::of(\"1816\") with a CSC Measurement characteristic uuid::of(\"2A5B\") (notify + a CCCD) whose cumulative wheel revolutions increase steadily over time." },
  { label: "💡 RGB smart light",
    request: "an RGB smart light: a custom 128-bit service via uuid::of(\"f0000001-1234-5678-1234-56789abcdef0\") with a writable+notify color characteristic holding [R, G, B] bytes that cycle through the rainbow over time." },
  { label: "❤️ heart-rate monitor",
    request: "a heart-rate monitor whose bpm rises and falls like exercise intervals (uuid::HEART_RATE_MEASUREMENT, notify + a CCCD, payload [0x00, bpm])." },
  { label: "🌫 humidity sensor",
    request: "a humidity sensor: Environmental Sensing service uuid::of(\"181A\") with a Humidity characteristic uuid::of(\"2A6F\") (notify + a CCCD), an unsigned 16-bit little-endian value in hundredths of a percent drifting around 45%." },
  { label: "🎲 surprise me",
    request: "a surprising, fun made-up BLE device of your choice — pick something delightful and make its values animate over time." },
];

// Wires the AI affordance (Claude/ChatGPT prefill links, Gemini copy+open,
// copy-prompt button, suggestion chips) against a set of element IDs matching
// the scripted-device page's markup: ai-claude, ai-chatgpt, ai-gemini, ai-copy,
// ai-prompt-view, ai-hint, suggest, req-echo. Identical behavior to hrm.js.
export function wireAi() {
  const $ = (id) => document.getElementById(id);
  let currentRequest = "";

  const effectivePrompt = () =>
    AI_PROMPT + (currentRequest ? currentRequest + "\n" : "");

  function refreshAi() {
    const encoded = encodeURIComponent(effectivePrompt());
    $("ai-claude").href = `https://claude.ai/new?q=${encoded}`;
    $("ai-chatgpt").href = `https://chatgpt.com/?q=${encoded}`;
    $("ai-prompt-view").textContent = effectivePrompt();
    $("req-echo").innerHTML = currentRequest
      ? `Request: <b>${escapeHtml(currentRequest)}</b>`
      : "Pick a suggestion above, or type your own after “MY DEVICE REQUEST:”.";
  }

  function setRequest(request, chipEl) {
    currentRequest = request;
    for (const el of document.querySelectorAll("#suggest .chip")) el.classList.remove("active");
    if (chipEl) chipEl.classList.add("active");
    refreshAi();
  }

  const suggest = $("suggest");
  suggest.innerHTML = SUGGESTIONS
    .map((s, i) => `<span class="chip" data-i="${i}">${escapeHtml(s.label)}</span>`)
    .join("");
  for (const chip of suggest.querySelectorAll(".chip")) {
    chip.addEventListener("click", () =>
      setRequest(SUGGESTIONS[+chip.dataset.i].request, chip));
  }
  // Rotating seed: a random suggestion is pre-filled so the prompt is
  // immediately useful, and it's a non-default device type.
  const seed = Math.floor(Math.random() * SUGGESTIONS.length);
  setRequest(SUGGESTIONS[seed].request, suggest.querySelector(`.chip[data-i="${seed}"]`));

  const hint = (t) => { $("ai-hint").textContent = t; setTimeout(() => ($("ai-hint").textContent = ""), 4000); };
  $("ai-gemini").addEventListener("click", async () => {
    await navigator.clipboard.writeText(effectivePrompt());
    window.open("https://gemini.google.com/app", "_blank", "noopener");
    hint("prompt copied — paste it into Gemini and send");
  });
  $("ai-copy").addEventListener("click", async () => {
    await navigator.clipboard.writeText(effectivePrompt());
    hint("prompt copied — paste into any LLM, then paste the returned Rhai here and press Run");
  });
}
