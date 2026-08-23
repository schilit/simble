// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// SimBLE Audio: both halves of an LE Audio stream, on one page.
//
// The source is a Unicast Client — it connects to a sink, walks its ASE from
// Idle to Enabling over the ASE Control Point, opens a real CIS, and streams
// LC3 frames encoded from a file you pick. The sink is a scripted peripheral
// with PACS, ASCS and the Volume Control Service; the page plays ONLY the SDUs
// that sink actually received, through a gain that follows its Volume State
// characteristic. So both the audio and the volume come out of the stack.
//
// Both devices live here, on ONE timer, and that is not tidiness — it is the
// reason this page exists. Chrome intensively throttles a hidden tab that
// produces no sound of its own, which is precisely what an audio source is.
// Measured with the source in a background tab: 256 of 2000 SDUs delivered
// before the stream stalled. On one visible page: 2000/2000, no underruns.
// Do not split this back into two pages.
//
// The module is mountable rather than a script that runs on load, because the
// domain shell hosts one of these at a time and switches between them:
// `mount(root)` builds the whole UI inside `root` and starts the timer,
// `unmount()` stops the timer and drops every device, socket and audio context
// it created. Anything left running would be a leak here and a ghost device on
// netsim, which does not synthesize a disconnect when a socket goes away.

import init, { WebSource, WebPeripheral, WebLink, WebLc3, WebScanner } from "../pkg/simble.js";
import { createSduPlayer } from "../common/lc3-player.js";
import { LE_AUDIO_SINK_SCRIPT as DEFAULT_SCRIPT } from "../common/le-audio-sink.js";
import { createGattView } from "../common/gatt-view.js";
import { createDeviceHeader } from "../common/device-header.js";
import { attachHighlightedEditor } from "../common/highlight.js";
import { createBackendSelector } from "../common/backend.js";
import { createAboutBox } from "../common/about-box.js";

// Each device gets its own socket, which is to say its own controller, exactly
// as three separate machines would have.
const SINK_ADDR = "CC:1E:57:00:00:08";
const SOURCE_ADDR = "CC:1E:57:00:00:07";
const SCANNER_ADDR = "CC:1E:57:00:00:09";
const ws = (node, address) =>
  `ws://localhost:7681/v1/websocket/bt?name=${encodeURIComponent(node)}&address=${address}`;

// LE Audio's 16_2 configuration. These must match what the sink's PAC record
// advertises and what the ASE is configured with, or the sink decodes noise
// rather than reporting an error. 40 octets is also the CIS's Max_SDU, which
// is why the media plane here is LC3 and not a codec selector: 10 ms of raw
// PCM at 16 kHz is 320 octets and the controller would refuse the SDU.
const PCM_RATE = 16000;
const SDU_INTERVAL_MS = 10;
const SAMPLES_PER_SDU = (PCM_RATE * SDU_INTERVAL_MS) / 1000;
const LC3_FRAME_BYTES = 40;

// Volume Control Service, 16-bit assigned numbers (status_json reports these
// as uppercase hex, which is what the lookups below compare against).
const VOLUME_STATE = "2B7D";
const VOLUME_CONTROL_POINT = "2B7E";
// Volume Control Service 1.0, Table 3.3.
const OP_DOWN = 0x00;
const OP_UP = 0x01;
const OP_UNMUTE_UP = 0x03;
const OP_SET_ABSOLUTE = 0x04;

// What makes an advertiser a sink worth streaming to: ASCS carries the
// endpoint a client configures, PACS says what the device can decode. A
// targeted announcement puts ASCS in service *data* rather than the UUID list,
// so both are worth looking at.
const ASCS_UUID = "184E";
const PACS_UUID = "1850";

// netsim does not synthesize a disconnect when a client's WebSocket drops: the
// device entry lingers, and a socket that re-registers under the same name a
// moment later can be attached to the stale one — the page looks connected and
// the device's transmit count never leaves zero. Every teardown therefore hands
// reconnection back to the loop's backoff instead of reopening immediately.
const RECONNECT_MS = 3000;

// --- state -----------------------------------------------------------------
// All of it belongs to one mount and is cleared by unmount().

let root = null;
let generation = 0; // guards async boot against an unmount that beat it
let timer = 0;

let mode = "in-page"; // "in-page" (a wasm WebLink in this tab) | "websocket" (netsim)
let lc3 = null;
let player = null;

// netsim backend
let source = null;
let sink = null;
let scanner = null;
let sinkOpenedOnce = false;
let lastSourceAttempt = 0;
let lastSinkAttempt = 0;
let lastScanAt = 0;

// in-page backend
let link = null;
let linkSink = -1;
let linkCentral = -1;

let target = SINK_ADDR;
let runStart = 0;
let lastRenderAt = 0;
let lastCounter = -1; // the sink's change counter, echoed back with commands
let prevValues = new Map();
let discovered = new Map(); // address -> { name }

// the file being streamed
let frames = [];
let cursor = 0;
let playing = false;
let startedAt = 0;

let editor = null;
let slider = null;

// The two device headers, and the sink's GATT view. Both devices used to share
// one page-wide status pill, which could only ever describe one of them: the
// pill said "streaming over a real CIS" while the sink beside it might have
// been reconnecting.
let sourceHead = null;
let sinkHead = null;
let gatt = null;

// Stop is real here, so it has to *stay* stopped: the loop below rebuilds a
// missing device on a timer, and without these a stop would silently reconnect
// three seconds later — a control that appears to do nothing.
let sourceStopped = false;
let sinkStopped = false;

const $ = (id) => root.querySelector(`#${id}`);

const showError = (m) => ($("error").textContent = m ? String(m) : "");
const showScriptError = (m) => ($("script-error").textContent = m ? String(m) : "");

// --- loading a file --------------------------------------------------------
// decodeAudioData handles whatever the browser can play; rendering through an
// OfflineAudioContext at PCM_RATE does the downmix to mono and the resample in
// one pass, which is both correct and far simpler than resampling by hand.
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

  const offline = new OfflineAudioContext(1, Math.ceil(decoded.duration * PCM_RATE), PCM_RATE);
  const node = offline.createBufferSource();
  node.buffer = decoded;
  node.connect(offline.destination);
  node.start();
  const rendered = await offline.startRendering();
  const samples = rendered.getChannelData(0);

  // Encode up front. Encoding inside the send loop is what starved the sink in
  // an earlier version of this demo: each iteration overran the 10 ms SDU
  // interval and the stream drifted steadily behind the clock.
  const encoded = [];
  const pcm = new Int16Array(SAMPLES_PER_SDU);
  for (let at = 0; at + SAMPLES_PER_SDU <= samples.length; at += SAMPLES_PER_SDU) {
    for (let i = 0; i < SAMPLES_PER_SDU; i++) {
      const value = Math.round(samples[at + i] * 32767);
      pcm[i] = Math.max(-32768, Math.min(32767, value));
    }
    encoded.push(lc3.encode(pcm, LC3_FRAME_BYTES));
  }

  frames = encoded;
  cursor = 0;
  const seconds = (frames.length * SDU_INTERVAL_MS) / 1000;
  $("track").textContent =
    `${file.name} — ${seconds.toFixed(1)}s, ${frames.length} LC3 frames ` +
    `(${PCM_RATE / 1000} kHz, ${LC3_FRAME_BYTES} octets/frame)`;
  $("play").disabled = false;
}

// --- the stream ------------------------------------------------------------

function start() {
  if (!frames.length) return;
  cursor = 0;
  startedAt = 0;
  playing = true;
  player.reset(); // the counters below describe this stream, not the page's history
  $("play").disabled = true;
  $("stop").disabled = false;
}

function stop() {
  playing = false;
  $("play").disabled = !frames.length;
  $("stop").disabled = true;
  $("hidden-warning").hidden = true;
}

// Hands one SDU to whichever media plane is live. Returns false when there is
// nowhere for it to go yet — over netsim the CIS may still be opening, and the
// in-page central may still be connecting.
function sendSdu(sdu) {
  if (mode === "websocket") {
    if (!source || !source.is_streaming()) return false;
    source.send_audio(sdu);
    return true;
  }
  if (!link || linkCentral < 0) return false;
  return link.central_send_audio(linkCentral, sdu);
}

// Feeds SDUs against a wall clock rather than a fixed count per tick, so
// scheduling jitter in the page does not accumulate into drift. The clock
// starts on the first SDU that actually goes out: starting it when the user
// clicked would count the handshake as playback time, and the stream would
// begin already behind.
function pumpAudio(now) {
  if (!playing || !frames.length) return;
  if (!startedAt) {
    if (!sendSdu(frames[0])) return;
    startedAt = now;
    cursor = 1;
  }
  const due = Math.floor((now - startedAt) / SDU_INTERVAL_MS) + 1;
  while (cursor < due && cursor < frames.length) {
    if (!sendSdu(frames[cursor])) return; // stream dropped — hold position
    cursor++;
  }
  $("progress").style.width = `${(cursor / frames.length) * 100}%`;
  // Chrome throttles timers in a hidden tab to roughly one a second. This page
  // paces against a wall clock, so a throttled tab wakes late and hands over a
  // burst instead of a steady 100 SDUs a second: nothing is lost, but the
  // stream stops being real-time — and that failure looks like a Bluetooth
  // problem rather than a browser one, so say so.
  $("hidden-warning").hidden = !document.hidden;
  if (cursor >= frames.length) stop();
}

// --- the sink --------------------------------------------------------------

function applySpeaker(volume, muted, changeCounter) {
  const bars = $("meter").children;
  const lit = muted ? 0 : Math.round((volume / 255) * 16);
  [...bars].forEach((bar, i) => {
    bar.className = i < lit ? (muted ? "muted" : "on") : "";
  });
  ["w1", "w2", "w3", "w4"].forEach((id, i) => {
    const threshold = (i + 1) * 60; // each wave lights as the volume climbs
    $(id).setAttribute("opacity", !muted && volume >= threshold ? "0.9" : "0.12");
  });
  $("muteMark").setAttribute("opacity", muted ? "1" : "0");
  $("cone").setAttribute("fill", muted ? "#d1495b" : "#33cc77");
  $("readout").innerHTML =
    `Volume State — volume <b>${volume}</b> · muted <b>${muted ? "yes" : "no"}</b> ` +
    `· change counter <b>${changeCounter}</b>`;
  if (document.activeElement !== slider) slider.value = String(volume);
  player.setVolume(volume, muted);
}

// The sink's GATT, as the page renders it. Everything visible about the device
// — including the volume the audio is played at — is read back out of the live
// characteristic values, never from a variable the UI kept on the side.
function renderSink(status) {
  // The name is the one the sink's own GATT server advertises.
  sinkHead.setName(status.name);
  if (status.address) sinkHead.setAddress(status.address);
  $("dev-conn").textContent = status.connected
    ? `connected to ${status.peer}`
    : "advertising, no central connected";
  const services = status.services || [];
  const anySub = services.some((s) => s.characteristics.some((c) => c.subscribed));
  $("dev-sub").textContent = anySub
    ? "central subscribed — notifications flowing"
    : "no subscriber yet";

  gatt.update(status);

  const state = services.flatMap((s) => s.characteristics).find((c) => c.uuid === VOLUME_STATE);
  if (state && state.value && state.value.length >= 6) {
    const volume = parseInt(state.value.slice(0, 2), 16);
    const muted = parseInt(state.value.slice(2, 4), 16);
    const changeCounter = parseInt(state.value.slice(4, 6), 16);
    lastCounter = changeCounter;
    applySpeaker(volume, muted !== 0, changeCounter);
  }
  if (status.last_error) showScriptError(`tick error: ${status.last_error}`);
}

function renderSinkStats() {
  const stats = player.stats;
  $("sink-stats").textContent =
    `${stats.received} SDUs received` +
    (stats.received ? ` · ${stats.underruns} underruns` : "") +
    ` · audio ${player.state}`;
}

// Every control writes the Volume Control Point; the sink's script decides what
// that means. This is the host-write path — what a phone's ATT write would
// deliver — so the script sees it on its next tick.
function writeControlPoint(bytes) {
  const value = new Uint8Array(bytes);
  try {
    if (mode === "websocket") sink?.set_value(VOLUME_CONTROL_POINT, value);
    else if (link && linkSink >= 0) link.peripheral_set_value(linkSink, VOLUME_CONTROL_POINT, value);
  } catch (e) {
    showScriptError(e);
  }
}
const counter = () => (lastCounter < 0 ? 0 : lastCounter);
const sendOp = (op) => writeControlPoint([op, counter()]);
const setAbsolute = (volume) => writeControlPoint([OP_SET_ABSOLUTE, counter(), volume & 0xff]);

// --- picking a sink --------------------------------------------------------
// The built-in sink is always offered; anything else comes off the air. A
// scanner on netsim is the honest way to fill this list — the same advertising
// reports a phone would use to find a pair of earbuds — and a typed address is
// the fallback for a device that is not advertising while you look.

function isAudioSink(report) {
  return (
    report.service_uuids.includes(ASCS_UUID) ||
    report.service_uuids.includes(PACS_UUID) ||
    (report.service_data || []).some((entry) => entry.tag === ASCS_UUID)
  );
}

function renderSinkOptions() {
  const select = $("sink-pick");
  const keep = select.value;
  const options = [
    `<option value="${SINK_ADDR}">Built-in sink on this page — ${SINK_ADDR}</option>`,
  ];
  if (discovered.size) {
    const entries = [...discovered.entries()].sort((a, b) => a[0].localeCompare(b[0]));
    options.push(
      `<optgroup label="Advertising an LE Audio sink">` +
        entries
          .map(([address, info]) => {
            const label = info.name ? `${info.name} — ${address}` : address;
            return `<option value="${address}">${label}</option>`;
          })
          .join("") +
        `</optgroup>`,
    );
  }
  options.push(`<option value="__other">Another address…</option>`);
  select.innerHTML = options.join("");
  // Rebuilding the list must not silently retarget a stream in flight.
  select.value = [...select.options].some((o) => o.value === keep) ? keep : SINK_ADDR;
}

function setTarget(address) {
  const clean = address.trim().toUpperCase();
  if (!/^([0-9A-F]{2}:){5}[0-9A-F]{2}$/.test(clean)) {
    showError(`"${address}" is not a Bluetooth address (AA:BB:CC:DD:EE:FF)`);
    return;
  }
  showError("");
  if (clean === target) return;
  target = clean;
  stop();
  cursor = 0;
  $("progress").style.width = "0";
  // A source is aimed at one peer for its lifetime — the connection, the ASE
  // and the CIS all belong to that peer — so retargeting means a new one.
  if (mode === "websocket") dropSource(performance.now());
}

// --- backends --------------------------------------------------------------

function freeNetsim() {
  for (const device of [source, sink, scanner]) {
    try {
      device?.free();
    } catch (_) {
      /* already gone */
    }
  }
  source = null;
  sink = null;
  scanner = null;
}

function dropSource(now) {
  try {
    source?.free();
  } catch (_) {
    /* already gone */
  }
  source = null;
  lastSourceAttempt = now;
}

function createSource() {
  try {
    source = new WebSource(ws("web-audio-source", SOURCE_ADDR), target);
    showError("");
  } catch (e) {
    source = null;
    showError(e);
  }
}

function createSink() {
  try {
    sink = new WebPeripheral(ws("web-audio-sink", SINK_ADDR), editor.value);
    runStart = performance.now();
  } catch (e) {
    sink = null;
    showScriptError(e);
  }
}

// --- stopping one device without the other -------------------------------
// Over netsim each of these devices owns its own WebSocket, which is to say its
// own controller, so one can genuinely leave the air while the other stays on
// it. In the in-page controller they share a single WebLink, and `WebLink` has
// add_peripheral and add_central and no way at all to remove either — so there
// the headers' toggles are disabled and say why rather than pretending.

function stopSource() {
  sourceStopped = true;
  stop(); // a stream with no source is not a stream
  dropSource(performance.now());
  sourceHead.setRunning(false);
  sourceHead.setState(false, "stopped");
  $("status").textContent = "stopped";
  for (const item of root.querySelectorAll(".stages li")) {
    item.classList.remove("done", "active");
  }
}

function startSource() {
  sourceStopped = false;
  lastSourceAttempt = performance.now() - RECONNECT_MS; // connect on the next tick
  sourceHead.setRunning(true);
  sourceHead.setState(false, "connecting…");
}

function stopSink() {
  sinkStopped = true;
  try {
    sink?.free();
  } catch (_) {
    /* already gone */
  }
  sink = null;
  sinkOpenedOnce = false;
  gatt.update({ services: [] });
  $("dev-conn").textContent = "stopped";
  $("dev-sub").textContent = "—";
  sinkHead.setRunning(false);
  sinkHead.setState(false, "stopped");
}

function startSink() {
  sinkStopped = false;
  lastSinkAttempt = performance.now() - RECONNECT_MS;
  sinkHead.setRunning(true);
  sinkHead.setState(false, "connecting…");
}

/// The in-page controller hosts both devices on one link, so neither can be
/// stopped alone there. The capability follows the backend rather than being
/// decided once at mount.
function applyStopCapability() {
  const shared = mode === "in-page"
    ? { disabled: true, reason: "one in-page link — the devices stop together" }
    : { disabled: false };
  sourceHead.setStopCapability(shared);
  sinkHead.setStopCapability(shared);
}

// In-page backend: the sink and a central that streams to it share one wasm
// Link in this tab, so the whole page works with no netsim at all. What it
// cannot do is a CIS — that is a controller feature, and the in-page radio
// carries SDUs on the connection handle instead.
function buildInPage() {
  const next = new WebLink();
  let sinkIndex;
  try {
    sinkIndex = next.add_peripheral(SINK_ADDR, editor.value);
  } catch (e) {
    try {
      next.free();
    } catch (_) {
      /* already gone */
    }
    throw e;
  }
  let central = -1;
  try {
    central = next.add_central(SOURCE_ADDR, SINK_ADDR);
  } catch (_) {
    /* streaming stays unavailable; the speaker still works */
  }
  try {
    link?.free();
  } catch (_) {
    /* already gone */
  }
  link = next;
  linkSink = sinkIndex;
  linkCentral = central;
  runStart = performance.now();
}

function teardown() {
  freeNetsim();
  try {
    link?.free();
  } catch (_) {
    /* already gone */
  }
  link = null;
  linkSink = -1;
  linkCentral = -1;
}

// What the page says about itself in each mode. The in-page controller is not
// a lesser netsim: it is a different thing, and the difference that matters
// here is the CIS.
function applyMode() {
  const inPage = mode === "in-page";
  $("sink-pick").disabled = inPage;
  $("rescan").disabled = inPage;
  $("sink-addr").hidden = true;
  $("stages").classList.toggle("off", inPage);
  $("mode-hint").textContent = inPage
    ? "In-browser controller — the sink, the source and the radio between them all run in this tab."
    : "";
  $("scan-note").textContent = inPage
    ? "The in-page controller hosts exactly one sink, so there is nothing to choose and nothing to scan."
    : "scanning netsim for LE Audio sinks…";
  $("status").textContent = inPage
    ? "not applicable — the in-page controller has no CIS; SDUs ride the connection handle"
    : "offline";
  if (inPage) {
    for (const item of root.querySelectorAll(".stages li")) {
      item.classList.remove("done", "active");
    }
  }
}

function switchBackend() {
  const now = performance.now();
  const hadSockets = Boolean(source || sink || scanner);
  teardown();
  stop();
  cursor = 0;
  startedAt = 0;
  $("progress").style.width = "0";
  player.reset();
  prevValues.clear();
  discovered.clear();
  sinkOpenedOnce = false;
  sourceStopped = false;
  sinkStopped = false;
  sourceHead.setRunning(true);
  sinkHead.setRunning(true);
  applyStopCapability();
  $("setup").classList.remove("visible");
  target = SINK_ADDR;
  renderSinkOptions();
  applyMode();
  if (mode === "in-page") {
    sourceHead.setState(false, "in browser · connecting…");
    sinkHead.setState(false, "in browser · starting…");
    try {
      buildInPage();
    } catch (e) {
      showScriptError(e);
    }
    return;
  }
  sourceHead.setState(false, "starting…");
  sinkHead.setState(false, "starting…");
  // Nothing was open on first load, so connect at once; coming back from the
  // in-page controller means netsim is still holding the sockets we just
  // closed, and reconnecting into those is what the backoff above avoids.
  const at = hadSockets ? now : now - RECONNECT_MS;
  lastSourceAttempt = at;
  lastSinkAttempt = at;
  lastScanAt = at + RECONNECT_MS;
}

// --- the one timer ---------------------------------------------------------
// Everything below runs on a single interval: the source's pump, the sink's
// tick, and the audio. Two timers would be two throttling victims.

function tickSource(now) {
  if (sourceStopped) return; // stopped by its own header, and staying stopped
  if (!source) {
    if (now - lastSourceAttempt >= RECONNECT_MS) {
      lastSourceAttempt = now;
      createSource();
    }
    sourceHead.setState(false, "connecting to localhost:7681…");
    return;
  }
  if (source.ready_state() === 3) {
    dropSource(now);
    sourceHead.setState(false, "connection lost — reconnecting…", "bad");
    return;
  }
  const status = JSON.parse(source.tick());
  pumpAudio(now);
  renderHandshake(status);
  // The dot means the thing this device exists to do: a stream is open. Every
  // earlier stage is progress towards it, not the thing itself.
  const streaming = Boolean(source.is_streaming());
  sourceHead.setState(
    streaming,
    streaming ? "streaming over a real CIS" : status.stage || "connecting…",
    status.error ? "bad" : streaming ? "ok" : "",
  );
}

function renderHandshake(status) {
  const order = [
    "connecting",
    "discovered",
    "configuring the endpoint",
    "opening the stream",
    "streaming",
  ];
  const reached = order.indexOf(status.stage);
  for (const item of root.querySelectorAll(".stages li")) {
    const at = order.indexOf(item.dataset.stage);
    item.classList.toggle("done", reached >= 0 && at < reached);
    item.classList.toggle("active", at === reached);
  }
  const cis =
    status.cis_handle !== null
      ? ` · CIS 0x${status.cis_handle.toString(16).toUpperCase().padStart(4, "0")}`
      : "";
  $("status").textContent = status.error
    ? status.error
    : `${status.stage}${cis}` +
      `${status.queued ? ` · ${status.queued} SDUs queued` : ""}` +
      `${status.dropped ? ` · ${status.dropped} dropped before the stream opened` : ""}`;
  if (status.error) showError(status.error);
}

function tickSink(now) {
  if (sinkStopped) return; // stopped by its own header, and staying stopped
  if (!sink) {
    if (now - lastSinkAttempt >= RECONNECT_MS) {
      lastSinkAttempt = now;
      createSink();
    }
    return;
  }
  const state = sink.ready_state(); // 0 connecting, 1 open, 2 closing, 3 closed
  if (state === 3) {
    if (sinkOpenedOnce) sinkHead.setState(false, "connection lost — reconnecting…", "bad");
    else {
      sinkHead.setState(false, "netsim not reachable", "bad");
      $("setup").classList.add("visible");
    }
    try {
      sink.free();
    } catch (_) {
      /* already gone */
    }
    sink = null;
    lastSinkAttempt = now;
    return;
  }
  if (state === 0) {
    sinkHead.setState(false, sinkOpenedOnce ? "reconnecting…" : "connecting to localhost:7681…");
    return;
  }
  sinkOpenedOnce = true;
  $("setup").classList.remove("visible");
  const status = JSON.parse(sink.tick((now - runStart) / 1000));
  player.play(sink.take_audio(), decodeSdu);
  if (now - lastRenderAt > 100) renderSink(status);
  sinkHead.setState(
    true,
    status.connected ? "on air · a client is connected" : "on air · advertising",
    "ok",
  );
}

// Scanning is paused while a stream is running: the scanner shares this page's
// single timer, and the send loop has a 10 ms deadline to keep.
function tickScanner(now) {
  if (playing || now - lastScanAt < 500) return;
  lastScanAt = now;
  if (!scanner) {
    try {
      scanner = new WebScanner(ws("web-audio-scan", SCANNER_ADDR));
    } catch (_) {
      return; // netsim is down; the sink's own reconnect reports that
    }
  }
  if (scanner.ready_state() === 3) {
    try {
      scanner.free();
    } catch (_) {
      /* already gone */
    }
    scanner = null;
    return;
  }
  if (scanner.ready_state() !== 1) return;
  let found = false;
  for (const report of JSON.parse(scanner.tick())) {
    if (!isAudioSink(report)) continue;
    const address = report.address.toUpperCase();
    if (address === SINK_ADDR) continue; // already the first option
    const known = discovered.get(address);
    if (known && (!report.name || known.name === report.name)) continue;
    discovered.set(address, { name: report.name || known?.name || null });
    found = true;
  }
  if (found) renderSinkOptions();
  $("scan-note").textContent = discovered.size
    ? `${discovered.size} other sink${discovered.size === 1 ? "" : "s"} on the air`
    : "scanning netsim for LE Audio sinks…";
}

const decodeSdu = (bytes) => lc3.decode(bytes);

function tickInPage(now) {
  if (!link || linkSink < 0) {
    try {
      buildInPage();
    } catch (e) {
      showScriptError(e);
    }
    return;
  }
  pumpAudio(now);
  link.tick((now - runStart) / 1000);
  player.play(link.peripheral_take_audio(linkSink), decodeSdu);
  const json = link.peripheral_status_json(linkSink);
  if (json && now - lastRenderAt > 100) renderSink(JSON.parse(json));
  sinkHead.setState(true, "in browser · advertising", "ok");
  // No CIS in the in-page controller: SDUs ride the connection handle, so the
  // source's dot means "there is a central attached and SDUs can go out".
  sourceHead.setState(linkCentral >= 0, linkCentral >= 0
    ? "in browser · SDUs on the connection handle"
    : "no central — streaming unavailable", linkCentral >= 0 ? "ok" : "warn");
}

function loop() {
  const now = performance.now();
  try {
    if (mode === "websocket") {
      tickSource(now);
      tickSink(now);
      tickScanner(now);
    } else {
      tickInPage(now);
    }
  } catch (e) {
    showError(e);
  }
  renderSinkStats();
  if (now - lastRenderAt > 100) lastRenderAt = now;
}

// Keyboard: arrows step the volume the way a device's own buttons would. This
// one listener is on `document`, so unmount() has to take it off again.
function onKeyDown(event) {
  if (event.target === slider || event.target === editor) return;
  if (event.target instanceof HTMLInputElement || event.target instanceof HTMLTextAreaElement) return;
  if (event.key === "ArrowUp") {
    sendOp(OP_UP);
    event.preventDefault();
  }
  if (event.key === "ArrowDown") {
    sendOp(OP_DOWN);
    event.preventDefault();
  }
  if (event.key === "m") sendOp(OP_UNMUTE_UP);
}

// --- markup ----------------------------------------------------------------

const STYLE_ID = "simble-audio-style";

function injectStyles() {
  if (document.getElementById(STYLE_ID)) return;
  const style = document.createElement("style");
  style.id = STYLE_ID;
  style.textContent = `
  .audio-page { display: grid; grid-template-columns: minmax(20rem, 1fr) minmax(20rem, 1fr);
    gap: 1.25rem; padding: 1.25rem 1.5rem; max-width: 76rem; margin: 0 auto;
    align-items: start; }
  @media (max-width: 60rem) { .audio-page { grid-template-columns: 1fr; } }
  /* Each column is its own flex stack so panels of unequal height do not drag
     their neighbour's top edge down. */
  .audio-page .col { display: flex; flex-direction: column; gap: 1.25rem; min-width: 0; }
  .audio-page .full { grid-column: 1 / -1; }

  .audio-page .drop { border: 2px dashed var(--border); border-radius: 10px;
    padding: 1.4rem 1rem; text-align: center; color: var(--dim);
    transition: border-color .15s, background .15s; }
  .audio-page .drop.over { border-color: var(--good); background: rgba(26,127,55,0.06); }
  .audio-page .drop input { display: block; margin: 0.7rem auto 0; }
  .audio-page .readout { font-family: ui-monospace, Menlo, monospace; color: var(--dim);
    font-size: 0.85rem; margin-top: 0.6rem; }
  .audio-page .readout b { color: var(--text); }
  .audio-page .bar { height: 8px; background: var(--panel2); border-radius: 4px;
    overflow: hidden; margin-top: 0.7rem; }
  .audio-page .bar > div { height: 100%; width: 0; background: var(--good);
    transition: width .1s linear; }
  .audio-page .field { display: block; margin-top: 0.9rem; font-size: 0.85rem; color: var(--dim); }
  .audio-page input[type=text] { font-family: ui-monospace, Menlo, monospace;
    padding: 0.35rem 0.5rem; border: 1px solid var(--border); border-radius: 6px; width: 14rem; }
  .audio-page #sink-pick { max-width: 100%; }

  .audio-page .stages { list-style: none; padding: 0; margin: 0.2rem 0 0; }
  .audio-page .stages li { padding: 0.32rem 0 0.32rem 1.5rem; position: relative;
    color: var(--dim); font-size: 0.9rem; }
  .audio-page .stages li::before { content: "○"; position: absolute; left: 0.25rem; }
  .audio-page .stages li.done { color: var(--text); }
  .audio-page .stages li.done::before { content: "●"; color: var(--good); }
  .audio-page .stages li.active { color: var(--text); font-weight: 600; }
  .audio-page .stages li.active::before { content: "◐"; color: var(--good); }
  .audio-page .stages.off { opacity: 0.45; }

  .audio-page .speaker-stage { display: flex; flex-direction: column; align-items: center;
    padding: 0.5rem 0 0.25rem; }
  .audio-page #speakerSvg { width: 160px; height: auto; }
  .audio-page #cone { transition: fill 0.15s; }
  .audio-page .waves path { transition: opacity 0.15s; }
  .audio-page .meter { display: flex; gap: 3px; margin-top: 0.9rem; height: 2.2rem;
    align-items: flex-end; }
  .audio-page .meter i { width: 7px; background: var(--border); border-radius: 2px; display: block; }
  .audio-page .meter i.on { background: var(--good); }
  .audio-page .meter i.muted { background: var(--bad); }
  .audio-page .controls { display: flex; align-items: center; gap: 0.6rem;
    justify-content: center; margin-top: 0.9rem; flex-wrap: wrap; }
  .audio-page input[type=range] { width: 13rem; }
  .audio-page .ops { display: flex; gap: 0.4rem; flex-wrap: wrap; justify-content: center;
    margin-top: 0.8rem; }
  .audio-page .ops button { font-family: ui-monospace, Menlo, monospace; font-size: 0.8rem; }
  .audio-page .warn-line { color: var(--warn); font-size: 0.8rem; margin-top: 0.5rem; }
  `;
  document.head.appendChild(style);
}

const ABOUT = `<p>Both halves of an LE Audio stream, on one page. Pick an audio file: it is decoded,
   resampled to 16&nbsp;kHz, encoded to LC3 and streamed to the sink over a <strong>real
   CIS</strong> — <code>LE Set CIG Parameters</code> → <code>LE Create CIS</code> →
   <code>LE Setup ISO Data Path</code> — and the speaker plays it at whatever volume its
   Volume Control Service currently holds.</p>`;

const TEMPLATE = `
<div class="audio-page">
  <div id="backend" class="full"></div>

  <div class="col">
    <section class="panel">
      <div id="source-head"></div>

      <div id="drop" class="drop">
        <strong>Drop an audio file here</strong><br>
        <span class="readout">mp3, m4a, wav, flac — whatever your browser can decode</span>
        <input type="file" id="file" accept="audio/*">
      </div>

      <label class="field" for="sink-pick">Stream to</label>
      <div class="row">
        <select id="sink-pick"></select>
        <button id="rescan" title="scan netsim for LE Audio sinks">⟳ rescan</button>
      </div>
      <input type="text" id="sink-addr" hidden placeholder="CC:1E:57:00:00:06"
             aria-label="sink address">
      <div class="readout" id="scan-note">—</div>

      <div class="row">
        <button id="play" class="primary" disabled>▶ stream</button>
        <button id="stop" disabled>■ stop</button>
      </div>

      <div class="bar"><div id="progress"></div></div>
      <div class="readout" id="track">no file loaded</div>
      <div class="warn-line" id="hidden-warning" hidden>
        ⚠ This tab is in the background. Chrome throttles hidden tabs, so the stream is no
        longer real-time — keep this window visible while streaming.
      </div>
      <div id="error" class="error"></div>

      <p class="hint" style="margin-top:1rem">
        This half of the page is a <strong>Unicast Client</strong>. It connects to the sink,
        writes <code>Config Codec</code>, <code>Config QoS</code> and <code>Enable</code> to its
        ASE Control Point, opens a <strong>real CIS</strong>, and streams LC3 frames it encodes
        from your file — decoded, downmixed and resampled to 16 kHz in the browser, so the file
        never leaves it.
      </p>
      <p class="hint" style="margin-top:0.5rem">
        The codec is not a choice here, and that is the protocol talking: the stream is LE Audio's
        <strong>16_2</strong> configuration — 16 kHz, 10 ms frames, <strong>40 octets</strong> per
        SDU, 100 SDUs a second — and the CIS is set up with <code>Max_SDU = 40</code>. Raw PCM at
        the same rate would be 320 octets a frame and the controller would refuse it.
      </p>
    </section>

    <section class="panel">
      <h2>Handshake</h2>
      <ul class="stages" id="stages">
        <li data-stage="connecting">Connect to the sink</li>
        <li data-stage="discovered">Discover its GATT</li>
        <li data-stage="configuring the endpoint">Configure the ASE</li>
        <li data-stage="opening the stream">Open the CIS</li>
        <li data-stage="streaming">Stream</li>
      </ul>
      <div class="readout" id="status">offline</div>

      <p class="hint" style="margin-top:1rem">
        Every step is real protocol, not a picture of one: the ASE operations are the bytes ASCS
        defines, and the stream is established with <code>LE Set CIG Parameters</code> →
        <code>LE Create CIS</code> → <code>LE Setup ISO Data Path</code>. If the chosen sink is not
        an LE Audio device the handshake stops at "Configure the ASE" — there is no control point
        to write to, and the status line says so.
      </p>
    </section>
  </div>

  <div class="col">
    <section class="panel">
      <div id="sink-head"></div>
      <div id="sink-script"></div>
      <div class="speaker-stage">
        <svg id="speakerSvg" viewBox="0 0 150 120" role="img" aria-label="speaker">
          <rect x="8" y="20" width="58" height="80" rx="8" fill="#57606a"/>
          <circle cx="37" cy="48" r="10" fill="#2d333b"/>
          <circle id="cone" cx="37" cy="76" r="16" fill="#33cc77"/>
          <circle cx="37" cy="76" r="6" fill="#2d333b"/>
          <g class="waves" stroke="#33cc77" stroke-width="4" fill="none" stroke-linecap="round">
            <path id="w1" d="M78 60 Q86 60 86 60" opacity="0"/>
            <path id="w2" d="M80 46 Q96 60 80 74" opacity="0"/>
            <path id="w3" d="M94 36 Q116 60 94 84" opacity="0"/>
            <path id="w4" d="M108 26 Q136 60 108 94" opacity="0"/>
          </g>
          <g id="muteMark" opacity="0" stroke="#d1495b" stroke-width="6" stroke-linecap="round">
            <path d="M92 44 L124 76"/><path d="M124 44 L92 76"/>
          </g>
        </svg>
        <div class="meter" id="meter"></div>
        <div class="readout" id="sink-stats">0 SDUs received · audio off</div>
        <div class="readout" id="readout">Volume State — volume <b>128</b> · muted <b>no</b> · change counter <b>0</b></div>
        <div class="controls">
          <button id="sound" class="primary">🔊 Enable sound</button>
          <label for="vol">Volume</label>
          <input type="range" id="vol" min="0" max="255" value="128">
        </div>
        <div class="ops">
          <button data-op="1">0x01 up</button>
          <button data-op="0">0x00 down</button>
          <button data-op="6">0x06 mute</button>
          <button data-op="5">0x05 unmute</button>
          <button data-op="3">0x03 unmute+up</button>
        </div>
      </div>

      <p class="hint" style="margin-top:1rem">
        <strong>Enable sound must be a real click.</strong> A browser only lets a user gesture
        create a running <code>AudioContext</code>; one made from script starts
        <em>suspended</em>, and then SDUs are still counted and still scheduled while nothing is
        heard — which looks exactly like a broken audio path. The counter above reports the
        context's own state so the two can be told apart.
      </p>
      <p class="hint" style="margin-top:0.5rem">
        The buttons are the <strong>control-point idiom</strong>, the pattern behind most settable
        BLE devices. Nothing here sets the volume directly: every control writes an opcode to the
        write-only <code>Volume Control Point</code> (<code>2B7E</code>), the sink's own script
        applies it, updates <code>Volume State</code> (<code>2B7D</code>) and bumps the change
        counter — exactly what a phone does over LE Audio's Volume Control Service. The output
        gain follows the characteristic, so <em>what you hear is the GATT value</em>, and a
        connected central writing the same opcodes moves it too.
      </p>
      <p class="hint" id="mode-hint" style="margin-top:0.5rem"></p>
      <div id="script-error" class="error"></div>
      <dl class="kv">
        <dt>connection</dt><dd id="dev-conn">—</dd>
        <dt>subscription</dt><dd id="dev-sub">—</dd>
      </dl>
      <div id="gatt"></div>
    </section>

  </div>

  <section id="setup" class="panel setup full">
    <h2>netsim is not reachable</h2>
    <p>Could not reach netsim at <code>localhost:7681</code> — is <code>netsimd</code> running with its
       WebSocket frontend enabled? This page may be served from the cloud, but the Bluetooth scene runs
       <strong>on your machine</strong>: the wasm build of SimBLE in this tab connects to a local
       <code>netsimd</code> over <code>ws://localhost:7681</code>. Start it with:</p>
    <pre><code>netsimd --logtostderr --no-shutdown --ws-port 7681</code></pre>
    <p class="hint">Needs the canary-channel emulator. Switch the controller above to
       <strong>In browser</strong> to run the whole thing offline — the speaker and its volume
       controls work there, and audio still crosses the simulated radio, just without a CIS.</p>
  </section>
</div>`;

// --- mount / unmount -------------------------------------------------------

/// Builds the whole page into `root` and starts it. Safe to call again: an
/// existing mount is torn down first.
export function mount(container) {
  unmount();
  injectStyles();
  root = container;
  root.innerHTML = TEMPLATE;
  root.prepend(createAboutBox(ABOUT));
  const gen = ++generation;

  slider = $("vol");
  buildHeaders();
  editor = sinkHead.textarea;
  prevValues = new Map();
  discovered = new Map();
  frames = [];
  cursor = 0;
  playing = false;
  startedAt = 0;
  lastCounter = -1;
  sinkOpenedOnce = false;
  lastRenderAt = 0;

  const meter = $("meter");
  for (let i = 0; i < 16; i++) {
    const bar = document.createElement("i");
    bar.style.height = `${20 + i * 5}%`;
    meter.appendChild(bar);
  }

  wireControls();
  document.addEventListener("keydown", onKeyDown);

  // The wasm module and the codec are async; a tab switch can beat them here,
  // so every step after the await checks it is still the current mount.
  (async () => {
    await init();
    if (gen !== generation) return;
    lc3 = new WebLc3(PCM_RATE, SDU_INTERVAL_MS * 1000);
    player = createSduPlayer({ sampleRate: PCM_RATE });
    renderSinkOptions();
    mode = createBackendSelector($("backend"), {
      onChange: (next) => {
        mode = next;
        switchBackend();
      },
    });
    switchBackend();
    timer = setInterval(loop, 20);
  })();
}

/// Stops the timer and drops everything this module created: both netsim
/// sockets and the scanner, the in-page link, the codec, the AudioContext, and
/// the one listener that lives on `document`. A tab switch must not leave a
/// device on the air.
export function unmount() {
  generation++;
  if (timer) {
    clearInterval(timer);
    timer = 0;
  }
  document.removeEventListener("keydown", onKeyDown);
  teardown();
  try {
    player?.close();
  } catch (_) {
    /* already closed */
  }
  try {
    lc3?.free();
  } catch (_) {
    /* already gone */
  }
  player = null;
  lc3 = null;
  frames = [];
  playing = false;
  gatt?.destroy();
  sourceHead?.destroy();
  sinkHead?.destroy();
  gatt = null;
  sourceHead = null;
  sinkHead = null;
  if (root) {
    root.innerHTML = "";
    root = null;
  }
  editor = null;
  slider = null;
}

/// The two device headers and the sink's GATT view. The source is a Rust
/// `WebSource` and has no script of its own, so it gets no pen -- a fabricated
/// one would be an API nobody can run.
function buildHeaders() {
  sourceHead = createDeviceHeader({
    name: "Audio Source",
    kind: "central · Unicast Client (Rust)",
    accent: "accent",
    address: SOURCE_ADDR,
    dotMeans: "a stream to the sink is open and SDUs can go out",
    run: { running: true, onRun: startSource, onStop: stopSource },
  });
  $("source-head").append(sourceHead.el);

  sinkHead = createDeviceHeader({
    name: "Audio Sink",
    kind: "peripheral",
    accent: "good",
    address: SINK_ADDR,
    dotMeans: "the sink is on the air",
    script: {
      text: DEFAULT_SCRIPT,
      editable: true,
      highlight: attachHighlightedEditor,
      note: "The sink is this script: PACS says what it can decode, ASCS carries the endpoint the " +
        "source configures, and the Volume Control Service is what the speaker's buttons write to. " +
        "Applying rebuilds the device on the same socket or link.",
      onApply: applySinkScript,
    },
    run: { running: true, onRun: startSink, onStop: stopSink },
  });
  $("sink-head").append(sinkHead.el);
  $("sink-script").append(sinkHead.panel);

  // Volume State is three bytes the Volume Control Service defines; the
  // decoder lives with the page that knows the profile rather than in the
  // shared viewer.
  gatt = createGattView({
    mode: "server",
    decode: (c) => {
      if (c.uuid !== VOLUME_STATE || !c.value || c.value.length < 6) return undefined;
      const [volume, muted, changes] = [0, 2, 4].map((i) => parseInt(c.value.slice(i, i + 2), 16));
      return `volume ${volume}${muted ? " · muted" : ""} · change counter ${changes}`;
    },
  });
  $("gatt").append(gatt.el);
}

/// Rebuilding the sink from its script is the same operation in both backends:
/// tear the device down and stand a new one up on the same socket or link.
function applySinkScript() {
  showScriptError(null);
  try {
    if (mode === "in-page") buildInPage();
    else if (sink) {
      sink.run_script(editor.value);
      runStart = performance.now();
    } else createSink();
    prevValues.clear();
    sinkHead.setApplyState("device rebuilt from script");
    setTimeout(() => {
      if (root) sinkHead.setApplyState("");
    }, 2500);
  } catch (e) {
    showScriptError(e);
  }
}

function wireControls() {
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

  $("play").addEventListener("click", start);
  $("stop").addEventListener("click", stop);

  $("sink-pick").addEventListener("change", (event) => {
    const value = event.target.value;
    if (value === "__other") {
      $("sink-addr").hidden = false;
      $("sink-addr").focus();
      return;
    }
    $("sink-addr").hidden = true;
    setTarget(value);
  });
  $("sink-addr").addEventListener("change", (event) => setTarget(event.target.value));
  $("rescan").addEventListener("click", () => {
    discovered.clear();
    renderSinkOptions();
    lastScanAt = 0;
  });

  // This handler must stay a real click: see the note on the page. Creating the
  // context from script yields a suspended one, which counts and schedules SDUs
  // in perfect silence.
  $("sound").addEventListener("click", (event) => {
    if (!player) return;
    player.enable();
    event.currentTarget.textContent = "🔊 sound on";
    event.currentTarget.classList.remove("primary");
    event.currentTarget.disabled = true;
  });

  // The slider sends Set Absolute Volume, the same command a phone's volume UI
  // sends — it does not poke the state characteristic.
  slider.addEventListener("input", () => setAbsolute(Number(slider.value)));
  for (const button of root.querySelectorAll(".ops button")) {
    button.addEventListener("click", () => sendOp(Number(button.dataset.op)));
  }
}
