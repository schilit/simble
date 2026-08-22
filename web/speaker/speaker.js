// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// SimBLE Speaker: an LE Audio device you can hear, with audio that really is
// streamed. Two devices share an in-page link — a scripted peripheral (the
// speaker, implementing Volume Control Service 1844) and a central that
// streams isochronous SDUs to it. The page plays ONLY what the sink received,
// scaled by the speaker's Volume State characteristic, so both the audio and
// the volume come out of the simulated stack.
//
// What is modeled: the whole media plane. On the netsim controller the SDUs
// arrive over a real CIS, carrying real LC3 frames — the Audio Source page
// (or any LE Audio source) is the other end. The in-page link keeps the
// simpler path, where a source in this same page streams over the simulated
// radio without a CIS.

import init, { WebPeripheral, WebLink, WebLc3 } from "../pkg/simble.js";
import { renderGatt } from "../common/viewer.js";
import { attachHighlightedEditor } from "../common/highlight.js";
import { createBackendSelector } from "../common/backend.js";
import { LE_AUDIO_SINK_SCRIPT as DEFAULT_SCRIPT } from "../common/le-audio-sink.js";

const IN_PAGE_ADDR = "CC:1E:57:00:00:06";
const SOURCE_ADDR = "CC:1E:57:00:00:07";

// The media plane: one frame per interval at this rate. The payload is
// opaque to SimBLE itself — the codec lives in the page — which is why the
// same path carries LC3 or raw PCM depending on the selector below.
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
  // Run the graph at the stream's own sample rate. At the hardware rate
  // (44.1 kHz here) every 160-sample buffer was resampled on its own, by a
  // ratio of 2.75625, so each 10 ms chunk picked up a filter transient at
  // both edges -- 100 discontinuities a second, which is audible as a
  // scratchy stream even when every SDU arrives on time.
  const Ctor = window.AudioContext || window.webkitAudioContext;
  try {
    audio = new Ctor({ sampleRate: PCM_RATE });
  } catch (e) {
    audio = new Ctor();   // some browsers refuse a non-hardware rate
  }
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
// A jitter buffer. SDUs are drained in ~100 ms batches, so scheduling them
// only 20 ms ahead meant any late tick landed past the cursor and playback
// snapped forward — an audible gap on nearly every batch. Holding a lead
// larger than the drain interval absorbs that jitter.
const JITTER_LEAD_S = 0.25;
let playCursor = 0;
function playReceivedSdus() {
  if (!audio || !masterGain) return;
  // Either backend can receive audio: an in-page source streams over the
  // wasm link, and over netsim a real peer streams over a real CIS.
  let sdus;
  try {
    if (mode === "in-page") {
      if (!link || linkIndex < 0) return;
      sdus = link.peripheral_take_audio(linkIndex);
    } else {
      if (!peripheral) return;
      sdus = peripheral.take_audio();
    }
  } catch (e) { return; }
  if (!sdus || sdus.length === 0) return;
  // Decode the whole batch into ONE buffer. Scheduling each SDU as its own
  // AudioBufferSourceNode put a seam every 10 ms; a batch has one seam.
  const decoded = [];
  let total = 0;
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
    decoded.push(pcm);
    total += pcm.length;
  }
  if (total > 0) {
    const buffer = audio.createBuffer(1, total, PCM_RATE);
    const channel = buffer.getChannelData(0);
    let at = 0;
    for (const pcm of decoded) {
      for (let i = 0; i < pcm.length; i++) channel[at + i] = pcm[i] / 32768;
      at += pcm.length;
    }
    const node = audio.createBufferSource();
    node.buffer = buffer;
    node.connect(masterGain);
    // Schedule back-to-back so consecutive SDUs play gaplessly. Falling
    // behind the clock means the source is not keeping up: re-prime the
    // buffer and count it, because that is what scratchy playback sounds
    // like and it should be visible rather than merely audible.
    let startAt = playCursor;
    if (startAt < audio.currentTime) {
      if (streamStats.played > 0) streamStats.underruns++;
      startAt = audio.currentTime + JITTER_LEAD_S;
    }
    node.start(startAt);
    playCursor = startAt + buffer.duration;
    streamStats.played += decoded.length;
  }
  streamStats.received += sdus.length;
  updateStreamStats();
}

const streamStats = { received: 0, played: 0, underruns: 0 };
function updateStreamStats() {
  const el = document.getElementById("stream-stats");
  if (el) {
    el.textContent = streaming
      ? `streaming ${codecMode.toUpperCase()} — ${streamStats.received} SDUs received ` +
        `(${SAMPLES_PER_SDU} samples/frame, ${PCM_RATE / 1000} kHz` +
        (codecMode === "lc3" ? `, ${LC3_FRAME_BYTES} octets/frame)` : ")") +
        (streamStats.underruns ? ` · ${streamStats.underruns} underruns` : " · no underruns")
      : `${streamStats.received} SDUs received` +
        (streamStats.received ? ` · ${streamStats.underruns} underruns` : "");
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
    playReceivedSdus();
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
