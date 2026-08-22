// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// SimBLE Speaker: an LE Audio device you can hear, with audio that really is
// streamed. Two devices share an in-page link — a scripted peripheral (the
// speaker, implementing Volume Control Service 1844) and a central that
// streams isochronous SDUs to it. The page plays ONLY what the sink received,
// scaled by the speaker's Volume State characteristic, so both the audio and
// the volume come out of the simulated stack.
//
// What is modeled: the ISO media plane — HCI ISO packets, SDU framing and
// sequence numbers, routing over the simulated radio. What is not: CIS
// establishment (SDUs ride the connection handle) and the LC3 codec (SDUs
// carry 16-bit PCM; the payload is opaque to SimBLE, so a codec can slot in
// later without changing the path).

import init, { WebPeripheral, WebLink, WebLc3 } from "../pkg/simble.js";
import { renderGatt } from "../common/viewer.js";
import { attachHighlightedEditor } from "../common/highlight.js";
import { createBackendSelector } from "../common/backend.js";

const IN_PAGE_ADDR = "CC:1E:57:00:00:06";
const SOURCE_ADDR = "CC:1E:57:00:00:07";

// The media plane: SDUs carry 16-bit PCM at this rate, one frame per interval.
// Real LE Audio would carry LC3 frames over a CIS; SimBLE models the SDU
// transport (framing, sequence numbers, routing) and leaves the payload
// opaque, so a codec can slot in later without changing the path.
const PCM_RATE = 16000;
// LC3 is defined for 7.5 ms and 10 ms frames; 10 ms at 16 kHz with 40 octets
// per frame is exactly what this device's PAC record advertises.
const SDU_INTERVAL_MS = 10;
const SAMPLES_PER_SDU = (PCM_RATE * SDU_INTERVAL_MS) / 1000;
const LC3_FRAME_BYTES = 40;

// Which codec the SDUs carry. "lc3" is what real LE Audio uses and what
// Android would send; "pcm" keeps the media plane codec-free, which is how
// SimBLE models it when no codec is present.
let codecMode = "lc3";
let lc3 = null;
const WS_URL =
  "ws://localhost:7681/v1/websocket/bt?name=web-speaker&address=CC:1E:57:00:00:06";

// Volume Control Service, 16-bit assigned numbers (status_json reports these
// uppercase-hex, which is what the lookups below compare against).
const VOLUME_STATE = "2B7D";
const VOLUME_CONTROL_POINT = "2B7E";

// Volume Control Service 1.0, Table 3.3.
const OP_DOWN = 0x00;
const OP_UP = 0x01;
const OP_UNMUTE_UP = 0x03;
const OP_SET_ABSOLUTE = 0x04;

const DEFAULT_SCRIPT = `// SimBLE Speaker — LE Audio Volume Control Service.
// The control-point idiom: a peer WRITES a command opcode, and the device
// applies it, updates its state and bumps the change counter. Nothing writes
// the volume directly.
let server = android::BluetoothGattServer("web-speaker");
let vcs = android::BluetoothGattService(uuid::VOLUME_CONTROL_SERVICE, android::SERVICE_TYPE_PRIMARY);

let state = android::BluetoothGattCharacteristic(uuid::VOLUME_STATE,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
state.set_value([128, 0, 0]); // [volume 0-255, muted, change counter]
state.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
vcs.add_characteristic(state);

let point = android::BluetoothGattCharacteristic(uuid::VOLUME_CONTROL_POINT,
    android::PROPERTY_WRITE, android::PERMISSION_WRITE);
point.set_value([0xFF]); // 0xFF = no command pending
vcs.add_characteristic(point);

let flags = android::BluetoothGattCharacteristic(uuid::VOLUME_FLAGS,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
flags.set_value([0x01]); // volume setting persisted
vcs.add_characteristic(flags);
server.add_service(vcs);

// Opcodes: 0x00 down, 0x01 up, 0x02/0x03 unmute+down/up, 0x04 set absolute,
// 0x05 unmute, 0x06 mute. A write is [opcode, change_counter] (+ volume for 0x04).
fn tick(server, t) {
    let command = server.value(uuid::VOLUME_CONTROL_POINT);
    if command.len() < 1 || command[0] == 0xFF { return; }
    let state = server.value(uuid::VOLUME_STATE);
    let volume = state[0];
    let muted = state[1];
    let op = command[0];
    if op == 0x00 || op == 0x02 { volume = if volume > 16 { volume - 16 } else { 0 }; }
    if op == 0x01 || op == 0x03 { volume = if volume < 239 { volume + 16 } else { 255 }; }
    if op == 0x02 || op == 0x03 || op == 0x05 { muted = 0; }
    if op == 0x04 && command.len() > 2 { volume = command[2]; }
    if op == 0x06 { muted = 1; }
    server.update_value(uuid::VOLUME_STATE, [volume, muted, (state[2] + 1) % 256]);
    server.update_value(uuid::VOLUME_CONTROL_POINT, [0xFF]); // consumed
}
`;

// --- DOM -------------------------------------------------------------------
const $ = (id) => document.getElementById(id);
const editor = $("script");
const connPill = $("conn");
const setupPanel = $("setup");
const slider = $("vol");

let mode = "in-page"; // "in-page" (a wasm WebLink in this tab) | "websocket" (netsim)
let peripheral = null;
let link = null;
let linkIndex = -1;
let sourceIndex = -1; // the streaming central, in-page backend only
let streaming = false;
let melodyPhase = 0;
let melodyStep = 0;
let runStart = performance.now();
let lastConnectAttempt = 0;
let openedOnce = false;
const prevValues = new Map();
let lastCounter = -1; // change counter to send with the next command

function setPill(text, cls) {
  connPill.textContent = text;
  connPill.className = "pill" + (cls ? " " + cls : "");
}
function showScriptError(m) { $("script-error").textContent = m ? String(m) : ""; }

function createPeripheral(script) {
  if (peripheral) { try { peripheral.free(); } catch (_) { /* gone */ } }
  peripheral = new WebPeripheral(WS_URL, script);
  runStart = performance.now();
}

// In-page backend: host the speaker on a wasm WebLink in this tab — no netsim.
function buildInPage(script) {
  const next = new WebLink();
  let idx;
  try { idx = next.add_peripheral(IN_PAGE_ADDR, script); }
  catch (e) { try { next.free(); } catch (_) { /* gone */ } throw e; }
  // A central on the same link is the audio source: it connects to the
  // speaker and streams isochronous SDUs to it.
  let src = -1;
  try { src = next.add_central(SOURCE_ADDR, IN_PAGE_ADDR); }
  catch (e) { /* streaming stays unavailable; the speaker still works */ }
  if (link) { try { link.free(); } catch (_) { /* gone */ } }
  link = next;
  linkIndex = idx;
  sourceIndex = src;
  runStart = performance.now();
}

function teardownDevices() {
  if (peripheral) { try { peripheral.free(); } catch (_) { /* gone */ } peripheral = null; }
  if (link) { try { link.free(); } catch (_) { /* gone */ } link = null; linkIndex = -1; }
}

function run() {
  showScriptError(null);
  try {
    if (mode === "in-page") buildInPage(editor.value);
    else if (peripheral) { peripheral.run_script(editor.value); runStart = performance.now(); }
    else createPeripheral(editor.value);
    prevValues.clear();
    $("run-state").textContent = "device rebuilt from script";
    setTimeout(() => ($("run-state").textContent = ""), 2500);
  } catch (e) { showScriptError(e); }
}

// --- the command path ------------------------------------------------------
// Every control writes the Volume Control Point; the device's script decides
// what that means. This is the host-write path (what a phone's ATT write would
// deliver), so the script sees it on its next tick.
function writeControlPoint(bytes) {
  const value = new Uint8Array(bytes);
  try {
    if (mode === "in-page") {
      if (link && linkIndex >= 0) link.peripheral_set_value(linkIndex, VOLUME_CONTROL_POINT, value);
    } else if (peripheral) {
      peripheral.set_value(VOLUME_CONTROL_POINT, value);
    }
  } catch (e) {
    showScriptError(e);
  }
}

const counter = () => (lastCounter < 0 ? 0 : lastCounter);
const sendOp = (op) => writeControlPoint([op, counter()]);
const setAbsolute = (volume) => writeControlPoint([OP_SET_ABSOLUTE, counter(), volume & 0xff]);

// --- audio -----------------------------------------------------------------
// A soft three-oscillator pad whose gain tracks the Volume State. Browsers
// require a user gesture before an AudioContext can make sound, hence the
// enable button.
let audio = null;
let masterGain = null;

function startAudio() {
  if (audio) return;
  audio = new (window.AudioContext || window.webkitAudioContext)();
  masterGain = audio.createGain();
  masterGain.gain.value = 0;

  masterGain.connect(audio.destination);
  playCursor = audio.currentTime;
  try {
    lc3 = new WebLc3(PCM_RATE, SDU_INTERVAL_MS * 1000);
  } catch (e) {
    showScriptError(`LC3 unavailable, falling back to PCM: ${e}`);
    codecMode = "pcm";
  }

  // Nothing is synthesized into the graph here: the only sound comes from
  // SDUs the sink receives (see playReceivedSdus), so what you hear is the
  // streamed audio, scaled by the Volume State characteristic.
  $("sound").textContent = "🔊 sound on";
  $("sound").disabled = true;
}

// Perceptual-ish curve, and deliberately quiet at the top.
function applyGain(volume, muted) {
  if (!audio || !masterGain) return;
  const level = muted ? 0 : 0.16 * Math.pow(volume / 255, 2);
  masterGain.gain.setTargetAtTime(level, audio.currentTime, 0.05);
}

// --- the audio source ------------------------------------------------------
// Renders one SDU's worth of PCM: a simple arpeggio so the stream is
// recognizably music rather than a flat tone.
const MELODY = [440.0, 554.37, 659.25, 554.37]; // A4, C#5, E5, C#5

function nextSdu() {
  const frame = new Int16Array(SAMPLES_PER_SDU);
  const hz = MELODY[melodyStep % MELODY.length];
  for (let i = 0; i < SAMPLES_PER_SDU; i++) {
    melodyPhase += (2 * Math.PI * hz) / PCM_RATE;
    if (melodyPhase > 2 * Math.PI) melodyPhase -= 2 * Math.PI;
    // A soft envelope over the note keeps the loop from clicking.
    const t = i / SAMPLES_PER_SDU;
    const envelope = Math.min(1, t * 8) * Math.min(1, (1 - t) * 8);
    frame[i] = Math.round(Math.sin(melodyPhase) * 0.5 * envelope * 32767);
  }
  melodyStep++;
  if (codecMode === "lc3" && lc3) {
    try {
      return lc3.encode(frame, LC3_FRAME_BYTES);
    } catch (e) {
      showScriptError(`LC3 encode failed: ${e}`);
      codecMode = "pcm";
    }
  }
  return new Uint8Array(frame.buffer);
}

let lastSduAt = 0;
function pumpSource(now) {
  if (!streaming || mode !== "in-page" || !link || sourceIndex < 0) return;
  if (now - lastSduAt < SDU_INTERVAL_MS) return;
  lastSduAt = now;
  try { link.central_send_audio(sourceIndex, nextSdu()); }
  catch (e) { showScriptError(e); }
}

// Plays the SDUs the sink received. Each is 16-bit PCM; it is queued into the
// Web Audio graph THROUGH the volume gain, so what you hear is the streamed
// audio scaled by the device's Volume State characteristic.
let playCursor = 0;
function playReceivedSdus() {
  if (!audio || !masterGain || mode !== "in-page" || !link || linkIndex < 0) return;
  let sdus;
  try { sdus = link.peripheral_take_audio(linkIndex); }
  catch (e) { return; }
  if (!sdus || sdus.length === 0) return;
  for (const bytes of sdus) {
    let pcm;
    if (codecMode === "lc3" && lc3) {
      try {
        pcm = lc3.decode(bytes);
      } catch (e) {
        showScriptError(`LC3 decode failed: ${e}`);
        continue;
      }
    } else {
      pcm = new Int16Array(bytes.buffer, bytes.byteOffset, bytes.byteLength / 2);
    }
    const buffer = audio.createBuffer(1, pcm.length, PCM_RATE);
    const channel = buffer.getChannelData(0);
    for (let i = 0; i < pcm.length; i++) channel[i] = pcm[i] / 32768;
    const node = audio.createBufferSource();
    node.buffer = buffer;
    node.connect(masterGain);
    // Schedule back-to-back so consecutive SDUs play gaplessly.
    const startAt = Math.max(audio.currentTime + 0.02, playCursor);
    node.start(startAt);
    playCursor = startAt + buffer.duration;
    streamStats.played++;
  }
  streamStats.received += sdus.length;
  updateStreamStats();
}

const streamStats = { received: 0, played: 0 };
function updateStreamStats() {
  const el = document.getElementById("stream-stats");
  if (el) {
    el.textContent = streaming
      ? `streaming ${codecMode.toUpperCase()} — ${streamStats.received} SDUs received ` +
        `(${SAMPLES_PER_SDU} samples/frame, ${PCM_RATE / 1000} kHz` +
        (codecMode === "lc3" ? `, ${LC3_FRAME_BYTES} octets/frame)` : ")")
      : `${streamStats.received} SDUs received`;
  }
}

// --- rendering -------------------------------------------------------------
const meter = $("meter");
for (let i = 0; i < 16; i++) {
  const bar = document.createElement("i");
  bar.style.height = `${20 + i * 5}%`;
  meter.appendChild(bar);
}

function applySpeaker(volume, muted, changeCounter) {
  const lit = muted ? 0 : Math.round((volume / 255) * 16);
  [...meter.children].forEach((bar, i) => {
    bar.className = i < lit ? (muted ? "muted" : "on") : "";
  });

  const waves = ["w1", "w2", "w3", "w4"];
  waves.forEach((id, i) => {
    const threshold = (i + 1) * 60; // each wave lights as the volume climbs
    $(id).setAttribute("opacity", !muted && volume >= threshold ? "0.9" : "0.12");
  });
  $("muteMark").setAttribute("opacity", muted ? "1" : "0");
  $("cone").setAttribute("fill", muted ? "#d1495b" : "#33cc77");

  $("readout").innerHTML =
    `Volume State — volume <b>${volume}</b> · muted <b>${muted ? "yes" : "no"}</b> ` +
    `· change counter <b>${changeCounter}</b>`;

  if (document.activeElement !== slider) slider.value = String(volume);
  applyGain(volume, muted);
}

function render(status) {
  $("dev-name").textContent = status.name ? `${status.name} (${status.address})` : "—";
  $("dev-conn").textContent = status.connected
    ? `connected to ${status.peer}` : "advertising, no central connected";
  const anySub = (status.services || []).some((s) => s.characteristics.some((c) => c.subscribed));
  $("dev-sub").textContent = anySub ? "central subscribed — notifications flowing" : "no subscriber yet";

  renderGatt($("gatt"), status, prevValues);

  const state = (status.services || [])
    .flatMap((s) => s.characteristics)
    .find((c) => c.uuid === VOLUME_STATE);
  if (state && state.value && state.value.length >= 6) {
    const volume = parseInt(state.value.slice(0, 2), 16);
    const muted = parseInt(state.value.slice(2, 4), 16);
    const changeCounter = parseInt(state.value.slice(4, 6), 16);
    lastCounter = changeCounter;
    applySpeaker(volume, muted !== 0, changeCounter);
  }
  if (status.last_error) showScriptError(`tick error: ${status.last_error}`);
}

function loop() {
  if (mode === "in-page") {
    if (!link || linkIndex < 0) {
      try { buildInPage(editor.value); } catch (e) { showScriptError(e); }
      return;
    }
    pumpSource(performance.now());
    try {
      link.tick((performance.now() - runStart) / 1000);
      playReceivedSdus();
      const json = link.peripheral_status_json(linkIndex);
      if (json) {
        setPill("in browser · advertising", "ok");
        render(JSON.parse(json));
      }
    } catch (e) { showScriptError(e); }
    return;
  }
  if (!peripheral) {
    const now = performance.now();
    if (now - lastConnectAttempt > 3000) {
      lastConnectAttempt = now;
      try { createPeripheral(editor.value); } catch (e) { showScriptError(e); }
    }
    return;
  }
  const state = peripheral.ready_state();
  if (state === 3) {
    if (openedOnce) setPill("connection lost — reconnecting…", "bad");
    else { setPill("netsim not reachable", "bad"); setupPanel.classList.add("visible"); }
    try { peripheral.free(); } catch (_) { /* gone */ }
    peripheral = null;
    return;
  }
  if (state === 0) {
    setPill(openedOnce ? "reconnecting…" : "connecting to localhost:7681…", "");
    return;
  }
  openedOnce = true;
  setupPanel.classList.remove("visible");
  try {
    const status = JSON.parse(peripheral.tick((performance.now() - runStart) / 1000));
    setPill(status.connected ? "on air · central connected" : "on air · advertising", "ok");
    render(status);
  } catch (e) { showScriptError(e); }
}

// --- boot ------------------------------------------------------------------
await init();
editor.value = DEFAULT_SCRIPT;
attachHighlightedEditor(editor);

$("sound").addEventListener("click", startAudio);
$("codec").addEventListener("change", (event) => {
  codecMode = event.target.value;
  streamStats.received = 0;
  streamStats.played = 0;
  updateStreamStats();
});
$("stream").addEventListener("click", () => {
  streaming = !streaming;
  if (streaming) startAudio(); // no point streaming into a silent graph
  $("stream").textContent = streaming ? "⏸ stop stream" : "▶ start stream";
  $("stream").classList.toggle("primary", !streaming);
  updateStreamStats();
});
$("run").addEventListener("click", run);

// The slider sends Set Absolute Volume, the same command a phone's volume UI
// sends — it does not poke the state characteristic.
slider.addEventListener("input", () => setAbsolute(Number(slider.value)));
for (const button of document.querySelectorAll(".ops button")) {
  button.addEventListener("click", () => sendOp(Number(button.dataset.op)));
}
// Keyboard: arrows step the volume the way a device's own buttons would.
document.addEventListener("keydown", (event) => {
  if (event.target === slider || event.target === editor) return;
  if (event.key === "ArrowUp") { sendOp(OP_UP); event.preventDefault(); }
  if (event.key === "ArrowDown") { sendOp(OP_DOWN); event.preventDefault(); }
  if (event.key === "m") sendOp(OP_UNMUTE_UP);
});

function setModeHint() {
  $("mode-hint").textContent = mode === "in-page"
    ? "In-browser controller — no netsim; the speaker runs entirely in this tab."
    : "";
}
function switchBackend() {
  teardownDevices();
  openedOnce = false;
  setupPanel.classList.remove("visible");
  setModeHint();
  if (mode === "in-page") {
    try { buildInPage(editor.value); } catch (e) { showScriptError(e); }
  } else {
    setPill("starting…", "");
    try { createPeripheral(editor.value); } catch (e) { showScriptError(e); }
  }
}
mode = createBackendSelector($("backend"), {
  onChange: (m) => { mode = m; switchBackend(); },
});
setModeHint();

if (mode === "in-page") {
  try { buildInPage(editor.value); } catch (e) { showScriptError(e); }
} else {
  try { createPeripheral(editor.value); } catch (e) { showScriptError(e); }
}
setInterval(loop, 100);
