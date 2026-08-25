// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// The "Generate a device with AI" panel — the generic "make any device"
// authoring aid the Playground mounts beside its editor. The AI_PROMPT text
// and the suggestion chips are the single source of truth for LLM device
// authoring. (web/hrm once kept a byte-identical inline copy for its own
// runtime; that copy is gone — the Playground is the only consumer now.)
//
// There is no API call in here, and the panel does not pretend otherwise: the
// page holds no LLM credentials, so generation happens in the assistant's own
// tab and the person carries the reply back. The panel's job is to make that
// round trip short and honest — three numbered steps, each claiming only what
// the page actually knows: the request text, whether (and where) the prompt
// was handed off, and whether a script came back. No spinner, ever: the page
// cannot see the assistant working, so it never says that it can.
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
  { label: "🔋 battery",
    request: "a battery monitor: a Battery Service with a Battery Level characteristic (uuid::BATTERY_LEVEL, notify + a CCCD) whose percentage slowly drains from 100 toward 5 and then jumps back to full." },
  { label: "🚴 cycling",
    request: "a cycling speed and cadence sensor: service uuid::of(\"1816\") with a CSC Measurement characteristic uuid::of(\"2A5B\") (notify + a CCCD) whose cumulative wheel revolutions increase steadily over time." },
  { label: "💡 light",
    request: "an RGB smart light: a custom 128-bit service via uuid::of(\"f0000001-1234-5678-1234-56789abcdef0\") with a writable+notify color characteristic holding [R, G, B] bytes that cycle through the rainbow over time." },
  { label: "❤️ heart rate",
    request: "a heart-rate monitor whose bpm rises and falls like exercise intervals (uuid::HEART_RATE_MEASUREMENT, notify + a CCCD, payload [0x00, bpm])." },
  { label: "🌫 humidity",
    request: "a humidity sensor: Environmental Sensing service uuid::of(\"181A\") with a Humidity characteristic uuid::of(\"2A6F\") (notify + a CCCD), an unsigned 16-bit little-endian value in hundredths of a percent drifting around 45%." },
  { label: "🎲 surprise",
    request: "a surprising, fun made-up BLE device of your choice — pick something delightful and make its values animate over time." },
];

// Where the prompt can go. `prefill` targets take the whole prompt in the URL
// (the handoff is one click, no clipboard involved); Gemini publishes no
// prefill URL, so its handoff is copy-then-open; `copy` serves every other
// LLM. The go-control's label always says exactly what pressing it will do.
const TARGETS = {
  claude: {
    label: "Claude", kind: "prefill", go: "Open in Claude ↗",
    url: (q) => `https://claude.ai/new?q=${q}`,
    title: "opens claude.ai in a new tab with the full prompt prefilled",
  },
  chatgpt: {
    label: "ChatGPT", kind: "prefill", go: "Open in ChatGPT ↗",
    url: (q) => `https://chatgpt.com/?q=${q}`,
    title: "opens chatgpt.com in a new tab with the full prompt prefilled",
  },
  gemini: {
    // The go-label matches its siblings; that this click also copies the
    // prompt is said where it matters — the tooltip before, the status
    // line after — not stretched across the button.
    label: "Gemini", kind: "copy-open", go: "Open Gemini ↗",
    url: () => "https://gemini.google.com/app",
    title: "Gemini has no prefill URL — the prompt is copied, paste it there",
  },
  clipboard: {
    label: "any LLM (copy)", kind: "copy", go: "Copy the prompt",
    title: "copies the full prompt to paste into any LLM",
  },
};

/// Pulls the device script out of an assistant's reply. The prompt demands a
/// single fenced code block, but replies arrive with prose around the fence,
/// with the fence's closing \`\`\` cut off by a truncated copy, or as bare code
/// with no fence at all — all are accepted. What is NOT accepted is text with
/// no `android::BluetoothGattServer(...)` in it: without that call nothing is
/// built, and loading it into the editor would only move the confusion there.
///
/// Returns `{ script, fenced }` on success, `{ why }` with a human-readable
/// refusal otherwise.
export function extractRhai(reply) {
  const text = String(reply ?? "").replace(/\r\n?/g, "\n");
  const buildsDevice = (s) => /BluetoothGattServer\s*\(/.test(s);
  // `(?:```|$)`: a fence whose closing marker was lost to a partial copy still
  // yields its content — Run will say precisely what is missing.
  const fences = [...text.matchAll(/```[^\n]*\n([\s\S]*?)(?:```|$)/g)]
    .map((m) => m[1].trim())
    .filter(Boolean);
  const fenced = fences.find(buildsDevice);
  if (fenced) return { script: fenced, fenced: true };
  if (fences.length) {
    return { why: "that code block doesn't build a device — no android::BluetoothGattServer(…) in it. Ask the assistant for a Simble Rhai script (step 2 sends the full instructions)." };
  }
  const trimmed = text.trim();
  if (buildsDevice(trimmed)) return { script: trimmed, fenced: false };
  return { why: "no Rhai script found — paste the assistant's whole reply, or just its code block." };
}

const STYLE_ID = "simble-ai-style";

function injectStyles() {
  if (document.getElementById(STYLE_ID)) return;
  const style = document.createElement("style");
  style.id = STYLE_ID;
  // (No backticks in this block — one ends the template literal and takes the
  // whole module down.)
  style.textContent = `
  /* Three numbered steps, one per leg of the round trip. The numbers are the
     progress display: a step's disc fills green when the page has seen that
     leg actually happen (the prompt handed off, a script arrive) — the same
     done-state idiom as the Audio page's handshake stages. Step 1 never
     "completes": a request is always present, so there is nothing to claim. */
  .ai-panel .ai-steps { list-style: none; margin: 0; padding: 0; }
  .ai-panel .ai-step { display: grid; grid-template-columns: auto minmax(0, 1fr);
    column-gap: 0.6rem; padding: 0.65rem 0; }
  .ai-panel .ai-step:first-child { padding-top: 0; }
  .ai-panel .ai-step:last-child { padding-bottom: 0; }
  .ai-panel .ai-step + .ai-step { border-top: 1px solid var(--border); }
  .ai-panel .ai-num { width: 1.35rem; height: 1.35rem; border-radius: 50%;
    border: 1.5px solid var(--dim); color: var(--dim); font-size: var(--fs-meta);
    font-weight: 600; display: inline-flex; align-items: center;
    justify-content: center; margin-top: 0.05rem; }
  .ai-panel .ai-step.done .ai-num { border-color: var(--good);
    background: var(--good); color: #fff; }
  .ai-panel .ai-title { font-size: var(--fs-body); font-weight: 600;
    margin-bottom: 0.35rem; }
  /* The request is prose a person writes, so it reads in the page's own face,
     not the code font — the code appears in the editor, where code lives. */
  .ai-panel .ai-request { width: 100%; resize: vertical; min-height: 4.8rem;
    background: var(--bg); color: var(--text); border: 1px solid var(--border);
    border-radius: 6px; padding: 0.45rem 0.55rem; font: inherit;
    font-size: var(--fs-body); }
  .ai-panel .ai-row { display: flex; gap: 0.5rem; align-items: center;
    flex-wrap: wrap; }
  .ai-panel .ai-row select { flex: 1 1 8rem; min-width: 0; }
  /* simble.css gives button/.btn an explicit display, which out-cascades the
     UA's [hidden] rule — so the go-control's hidden twin needs saying again. */
  .ai-panel [hidden] { display: none; }
  .ai-panel .ai-status { margin-top: 0.4rem; }
  .ai-panel .ai-status.warn { color: var(--warn); }
  .ai-panel details { margin-top: 0.45rem; }
  .ai-panel details summary { font-size: var(--fs-label); }
  /* The paste zone borrows the Audio drop zone's grammar: accent dashed =
     "give me the reply", green solid = "got it". Colour is the state. */
  .ai-panel .ai-paste { width: 100%; resize: vertical; min-height: 3.2rem;
    border: 1.5px dashed var(--accent); border-radius: 8px;
    background: rgba(9,105,218,0.045); color: var(--text);
    padding: 0.45rem 0.55rem; font: inherit; font-size: var(--fs-body);
    transition: border-color .15s, background .15s; }
  .ai-panel .ai-paste:focus { outline: 2px solid var(--accent); outline-offset: 1px; }
  .ai-panel .ai-paste.loaded { border-style: solid; border-color: var(--good);
    background: rgba(26,127,55,0.05); }
  .ai-panel .ai-paste.refused { border-color: var(--warn);
    background: rgba(154,103,0,0.05); }
  .ai-panel .ai-note { margin-top: 0.35rem; }
  .ai-panel .ai-note.ok { color: var(--good); }
  .ai-panel .ai-note.warn { color: var(--warn); }
  `;
  document.head.append(style);
}

/**
 * Builds the panel. Everything is scoped element references — nothing here is
 * reachable by document.getElementById, for the same reason device-header.js
 * gives: generic ids have crossed between modules before.
 *
 * @param {object} options
 * @param {(script: string) => void} options.onScript  called with the extracted
 *        Rhai when a reply is pasted; the page owns what "loaded" means
 *        (typically: put it in the editor, stop the device, invite Run).
 * @returns {{ el: HTMLElement }}
 */
export function createAiPanel({ onScript }) {
  injectStyles();

  const el = document.createElement("section");
  el.className = "panel ai-panel";
  el.innerHTML = `
    <h2>Generate a device with AI</h2>
    <ol class="ai-steps">
      <li class="ai-step">
        <span class="ai-num">1</span>
        <div>
          <div class="ai-title">Describe the device</div>
          <div class="suggest"></div>
          <textarea class="ai-request" rows="3" spellcheck="true"
            aria-label="describe the device to generate"
            placeholder="…or describe it in your own words"></textarea>
        </div>
      </li>
      <li class="ai-step" data-step="generate">
        <span class="ai-num">2</span>
        <div>
          <div class="ai-title">Generate the script</div>
          <div class="ai-row">
            <select class="ai-target" aria-label="which assistant writes the script"></select>
            <a class="ai-go btn primary" target="_blank" rel="noopener"></a>
            <button class="ai-go-copy primary" hidden></button>
          </div>
          <div class="ai-status hint"></div>
          <details>
            <summary>the exact prompt it will get</summary>
            <pre class="ai-prompt-view"></pre>
          </details>
        </div>
      </li>
      <li class="ai-step" data-step="return">
        <span class="ai-num">3</span>
        <div>
          <div class="ai-title">Bring the reply back</div>
          <textarea class="ai-paste" rows="3" spellcheck="false"
            aria-label="paste the assistant's reply here"
            placeholder="paste the assistant's reply here — the Rhai is pulled out automatically"></textarea>
          <div class="ai-note hint"></div>
        </div>
      </li>
    </ol>`;

  const $ = (sel) => el.querySelector(sel);
  const suggest = $(".suggest");
  const request = $(".ai-request");
  const target = $(".ai-target");
  const goLink = $(".ai-go");
  const goCopy = $(".ai-go-copy");
  const status = $(".ai-status");
  const promptView = $(".ai-prompt-view");
  const stepGenerate = $('[data-step="generate"]');
  const stepReturn = $('[data-step="return"]');
  const paste = $(".ai-paste");
  const note = $(".ai-note");

  const effectivePrompt = () =>
    AI_PROMPT + (request.value.trim() ? request.value.trim() + "\n" : "");

  const setStatus = (text, warn = false) => {
    status.textContent = text;
    status.className = "ai-status hint" + (warn ? " warn" : "");
  };
  const setNote = (text, tone = "") => {
    note.textContent = text;
    note.className = "ai-note hint" + (tone ? ` ${tone}` : "");
  };

  // --- step 1: the request --------------------------------------------------
  suggest.innerHTML = SUGGESTIONS
    .map((s, i) => `<span class="chip" data-i="${i}" title="fill the request with this device">${escapeHtml(s.label)}</span>`)
    .join("");

  function markChips() {
    const text = request.value.trim();
    for (const chip of suggest.querySelectorAll(".chip")) {
      chip.classList.toggle("active", SUGGESTIONS[+chip.dataset.i].request === text);
    }
  }

  for (const chip of suggest.querySelectorAll(".chip")) {
    chip.addEventListener("click", () => {
      request.value = SUGGESTIONS[+chip.dataset.i].request;
      onRequestChanged();
    });
  }

  // A changed request makes any earlier handoff stale: the tab that was opened
  // holds the OLD prompt. The done-marks come off and the status says why,
  // instead of the panel quietly claiming a generation that never used this
  // request.
  function onRequestChanged() {
    markChips();
    refreshHandoff();
    if (stepGenerate.classList.contains("done") || stepReturn.classList.contains("done")) {
      stepGenerate.classList.remove("done");
      stepReturn.classList.remove("done");
      paste.classList.remove("loaded", "refused");
      setStatus("the request changed — generate again to use it");
      setNote("");
    }
  }
  request.addEventListener("input", onRequestChanged);

  // --- step 2: the handoff --------------------------------------------------
  target.innerHTML = Object.entries(TARGETS)
    .map(([key, t]) => `<option value="${key}">${escapeHtml(t.label)}</option>`)
    .join("");

  // One control, two elements: prefill/open targets are a real link (new tab,
  // middle-click, no popup heuristics), the clipboard-only target is a button
  // (a link that navigates nowhere is a lie). Exactly one is visible, in the
  // same spot, with the verb as its label.
  function refreshHandoff() {
    const t = TARGETS[target.value];
    const isLink = t.kind !== "copy";
    goLink.hidden = !isLink;
    goCopy.hidden = isLink;
    const go = isLink ? goLink : goCopy;
    go.textContent = t.go;
    go.title = t.title;
    if (isLink) {
      goLink.href = t.kind === "prefill"
        ? t.url(encodeURIComponent(effectivePrompt()))
        : t.url();
    }
    promptView.textContent = effectivePrompt();
  }
  target.addEventListener("change", () => {
    refreshHandoff();
  });

  function markHandedOff(text) {
    stepGenerate.classList.add("done");
    setStatus(text);
  }

  async function copyPrompt() {
    await navigator.clipboard.writeText(effectivePrompt());
  }

  goLink.addEventListener("click", () => {
    refreshHandoff(); // the href must carry the request as it reads right now
    const t = TARGETS[target.value];
    if (t.kind === "prefill") {
      markHandedOff(`opened ${t.label} with the prompt — send it, then paste its reply below`);
      return; // the link itself navigates
    }
    // copy-open (Gemini): the copy rides the same click gesture as the
    // navigation, so the clipboard write is permitted.
    copyPrompt().then(
      () => markHandedOff(`prompt copied — paste it into ${t.label} and send, then paste its reply below`),
      () => setStatus("could not write the clipboard — open “the exact prompt” below and copy it by hand", true),
    );
  });

  goCopy.addEventListener("click", () => {
    copyPrompt().then(
      () => markHandedOff("prompt copied — paste it into any LLM, then paste its reply below"),
      () => setStatus("could not write the clipboard — open “the exact prompt” below and copy it by hand", true),
    );
  });

  // --- step 3: the reply ----------------------------------------------------
  // The zone evaluates whatever lands in it (debounced past keystroke rate):
  // on success the script moves to the editor — the editor IS the result view,
  // with highlighting, editing and Run already there — and the zone becomes a
  // green receipt; anything unusable turns it warn with the reason.
  let pasteTimer = 0;
  paste.addEventListener("input", () => {
    clearTimeout(pasteTimer);
    pasteTimer = setTimeout(evaluatePaste, 150);
  });

  function evaluatePaste() {
    const raw = paste.value;
    if (!raw.trim()) {
      paste.classList.remove("refused");
      if (!stepReturn.classList.contains("done")) setNote("");
      return;
    }
    const got = extractRhai(raw);
    if (!got.script) {
      paste.classList.remove("loaded");
      paste.classList.add("refused");
      stepReturn.classList.remove("done");
      setNote(got.why, "warn");
      return;
    }
    onScript(got.script);
    paste.value = ""; // the script's home is the editor; the receipt says so
    paste.classList.remove("refused");
    paste.classList.add("loaded");
    stepReturn.classList.add("done");
    const lines = got.script.split("\n").length;
    setNote(
      `✓ ${lines}-line script${got.fenced ? " (from the reply's code block)" : ""}` +
        " → the editor — press ▶ Run",
      "ok",
    );
  }

  // Rotating seed: a random suggestion is pre-filled so the prompt is
  // immediately useful, and it's a non-default device type.
  request.value = SUGGESTIONS[Math.floor(Math.random() * SUGGESTIONS.length)].request;
  markChips();
  refreshHandoff();

  return { el };
}
