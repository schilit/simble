// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// SimBLE Audio Source: the central half of LE Audio, hosted in the page.
//
// The Speaker page is the sink. This is the side that drives it — connect,
// walk the peer's ASE from Idle to Enabling over its control point, open a
// real CIS, and stream LC3 frames. Nothing here fakes the media plane: the
// stream is established with LE Set CIG Parameters / LE Create CIS / LE Setup
// ISO Data Path, and the SDUs are HCI ISO packets on the CIS handle.
//
// The file never leaves the browser. It is decoded, resampled and encoded
// here, then streamed over the simulated radio to a sink that is usually
// another tab.

import init, { WebSource, WebPeripheral, WebLc3 } from "../pkg/simble.js";
import { createSduPlayer } from "../common/lc3-player.js";
import { LE_AUDIO_SINK_SCRIPT } from "../common/le-audio-sink.js";

// Both devices live on this page, each with its own netsim socket — which is
// to say its own controller, exactly as two separate machines would have.
// Hosting them together is not only tidier: Chrome intensively throttles a
// hidden tab that produces no audio of its own, so a source in a background
// tab stops feeding the stream within seconds. One visible page, one timer,
// no throttling.
//
// The built-in sink deliberately does NOT use the Speaker page's address, so
// both pages can be open at once without two devices claiming CC:...:06.
const SOURCE_WS =
  "ws://localhost:7681/v1/websocket/bt?name=web-source&address=CC:1E:57:00:00:07";
const SINK_WS =
  "ws://localhost:7681/v1/websocket/bt?name=web-sink&address=CC:1E:57:00:00:08";

// LE Audio's 16_2 configuration: 16 kHz, 10 ms frames, 40 octets per frame.
// These must match what the sink's PAC record advertises and what the ASE is
// configured with, or the sink decodes noise rather than reporting an error.
const PCM_RATE = 16000;
const SDU_INTERVAL_MS = 10;
const SAMPLES_PER_SDU = (PCM_RATE * SDU_INTERVAL_MS) / 1000;
const LC3_FRAME_BYTES = 40;

const $ = (id) => document.getElementById(id);

let source = null;
let sink = null;
let player = null;
let lc3 = null;
let sinkStarted = 0;
let sdus = [];
let cursor = 0;
let playing = false;
let startedAt = 0;

function setPill(text, kind) {
  const pill = $("conn");
  pill.textContent = text;
  pill.className = `pill${kind ? " " + kind : ""}`;
}

function showError(message) {
  $("error").textContent = message ? String(message) : "";
}

// --- loading a file -------------------------------------------------------
// decodeAudioData handles whatever the browser can play; rendering through an
// OfflineAudioContext at PCM_RATE does the downmix to mono and the resample
// in one pass, which is both correct and far simpler than resampling by hand.
async function loadFile(file) {
  $("track").textContent = `decoding ${file.name}…`;
  showError("");

  const bytes = await file.arrayBuffer();
  const context = new AudioContext();
  let decoded;
  try {
    decoded = await context.decodeAudioData(bytes);
  } finally {
    context.close();
  }

  const offline = new OfflineAudioContext(
    1,
    Math.ceil(decoded.duration * PCM_RATE),
    PCM_RATE,
  );
  const node = offline.createBufferSource();
  node.buffer = decoded;
  node.connect(offline.destination);
  node.start();
  const rendered = await offline.startRendering();
  const samples = rendered.getChannelData(0);

  // Encode up front. Encoding inside the send loop is what starved the sink
  // in an earlier version of this demo: each iteration overran the 10 ms SDU
  // interval and the stream drifted steadily behind the clock.
  const frames = [];
  const pcm = new Int16Array(SAMPLES_PER_SDU);
  for (let at = 0; at + SAMPLES_PER_SDU <= samples.length; at += SAMPLES_PER_SDU) {
    for (let i = 0; i < SAMPLES_PER_SDU; i++) {
      const value = Math.round(samples[at + i] * 32767);
      pcm[i] = Math.max(-32768, Math.min(32767, value));
    }
    frames.push(lc3.encode(pcm, LC3_FRAME_BYTES));
  }

  sdus = frames;
  cursor = 0;
  const seconds = (frames.length * SDU_INTERVAL_MS) / 1000;
  $("track").textContent =
    `${file.name} — ${seconds.toFixed(1)}s, ${frames.length} LC3 frames ` +
    `(${PCM_RATE / 1000} kHz, ${LC3_FRAME_BYTES} octets/frame)`;
  $("play").disabled = false;
}

// --- the stream -----------------------------------------------------------

function start() {
  if (!sdus.length) return;
  cursor = 0;
  startedAt = 0;
  playing = true;
  $("play").disabled = true;
  $("stop").disabled = false;
}

function stop() {
  playing = false;
  $("play").disabled = !sdus.length;
  $("stop").disabled = true;
}

// Feeds SDUs against a wall-clock deadline rather than a fixed count per
// tick, so scheduling jitter in the page does not accumulate into drift.
function pumpAudio() {
  if (!playing || !source.is_streaming()) return;
  if (!startedAt) startedAt = performance.now();
  const due = Math.floor((performance.now() - startedAt) / SDU_INTERVAL_MS);
  while (cursor < due && cursor < sdus.length) {
    source.send_audio(sdus[cursor++]);
  }
  if (cursor >= sdus.length) stop();
  $("progress").style.width = `${(cursor / sdus.length) * 100}%`;
  checkVisibility();
}

function render(status) {
  const stage = status.stage;
  const order = [
    "connecting",
    "discovered",
    "configuring the endpoint",
    "opening the stream",
    "streaming",
  ];
  const reached = order.indexOf(stage);
  for (const item of document.querySelectorAll(".stages li")) {
    const at = order.indexOf(item.dataset.stage);
    item.classList.toggle("done", reached >= 0 && at < reached);
    item.classList.toggle("active", at === reached);
  }

  $("status").textContent = status.error
    ? status.error
    : `${stage}${status.cis_handle !== null ? ` · CIS 0x${status.cis_handle.toString(16).toUpperCase().padStart(4, "0")}` : ""}` +
      `${status.queued ? ` · ${status.queued} SDUs queued` : ""}` +
      `${status.dropped ? ` · ${status.dropped} dropped before the stream opened` : ""}`;

  if (status.error) {
    setPill("stopped", "warn");
    showError(status.error);
  } else if (status.streaming) {
    setPill("streaming over a real CIS", "ok");
  } else {
    setPill(`on air · ${stage}`, "ok");
  }
}

// Chrome throttles timers in a hidden tab to roughly one a second. This page
// paces against a wall clock, so a throttled tab wakes late and hands over a
// large burst instead of a steady 100 SDUs a second. Nothing is lost now that
// the send path is unbuffered, but the stream stops being real-time — and the
// failure looks like a Bluetooth problem rather than a browser one, so say so.
function checkVisibility() {
  const warn = document.hidden && playing;
  $("hidden-warning").hidden = !warn;
}
document.addEventListener("visibilitychange", checkVisibility);

function loop() {
  if (!source) return;
  try {
    const status = JSON.parse(source.tick());
    pumpAudio();
    render(status);
  } catch (e) {
    showError(e);
  }
  // The sink runs on the same timer. Its audio arrives over the CIS the
  // source opened, so what you hear has crossed the simulated radio.
  if (sink) {
    try {
      if (!sinkStarted) sinkStarted = performance.now();
      const status = JSON.parse(sink.tick((performance.now() - sinkStarted) / 1000));
      player.play(sink.take_audio(), (bytes) => lc3.decode(bytes));
      renderSink(status);
    } catch (e) {
      // A sink fault must not stop the source; show it and keep streaming.
      $("sink-status").textContent = String(e);
    }
  }
}

function renderSink(status) {
  const state = (status.characteristics || []).find((c) => c.uuid === "2B7D");
  let volume = 128;
  let muted = 0;
  if (state && state.value && state.value.length >= 6) {
    volume = parseInt(state.value.slice(0, 2), 16);
    muted = parseInt(state.value.slice(2, 4), 16);
  }
  player.setVolume(volume, muted);
  $("sink-volume").textContent =
    `volume ${volume} · ${muted ? "muted" : "unmuted"}`;
  const stats = player.stats;
  $("sink-status").textContent =
    `${stats.received} SDUs received` +
    (stats.received ? ` · ${stats.underruns} underruns` : "") +
    ` · audio ${player.state}`;
  $("meter").style.width =
    `${Math.min(100, (stats.received % 100))}%`;
}

// --- wiring ---------------------------------------------------------------

await init();
lc3 = new WebLc3(PCM_RATE, SDU_INTERVAL_MS * 1000);

function connect() {
  const target = $("target").value.trim();
  try {
    source = new WebSource(SOURCE_WS, target);
    showError("");
  } catch (e) {
    showError(e);
    setPill("cannot start", "warn");
  }
}
connect();
$("target").addEventListener("change", connect);

$("file").addEventListener("change", (event) => {
  const file = event.target.files[0];
  if (file) loadFile(file).catch((e) => showError(`could not decode: ${e}`));
});

const drop = $("drop");
for (const name of ["dragenter", "dragover"]) {
  drop.addEventListener(name, (e) => {
    e.preventDefault();
    drop.classList.add("over");
  });
}
for (const name of ["dragleave", "drop"]) {
  drop.addEventListener(name, (e) => {
    e.preventDefault();
    drop.classList.remove("over");
  });
}
drop.addEventListener("drop", (event) => {
  const file = event.dataTransfer.files[0];
  if (file) loadFile(file).catch((e) => showError(`could not decode: ${e}`));
});

// Bring the built-in sink up alongside the source.
player = createSduPlayer({ sampleRate: PCM_RATE });
try {
  sink = new WebPeripheral(SINK_WS, LE_AUDIO_SINK_SCRIPT);
} catch (e) {
  $("sink-status").textContent = `sink unavailable: ${e}`;
}

$("sound").addEventListener("click", () => {
  player.enable();
  $("sound").textContent = "🔊 sound on";
  $("sound").disabled = true;
});

$("play").addEventListener("click", start);
$("stop").addEventListener("click", stop);

setInterval(loop, 20);
