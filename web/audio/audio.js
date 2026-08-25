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
import { currentController, usbBridgeHttp, usbBridgeUrl } from "../common/controller-bar.js";
import { createAboutBox } from "../common/about-box.js";

/// Which controllers this domain can run on. The shell's controller bar
/// reads this: an option mapped to a string is offered disabled, with that
/// string as the reason, rather than hidden.
export const SUPPORTS = { "in-page": true, "websocket": true, "usb": true };


// Each device gets its own socket, which is to say its own controller, exactly
// as three separate machines would have.
//
// Only the sink's address is a page control. The source is a central: it never
// advertises, so it has no advertising details to surface, and its address is
// visible in its header where a reader would look for it. The scanner is
// plumbing — it exists so the picker below has something to list — and an
// address field for a device that is never on anyone's screen is a knob for
// its own sake.
const DEFAULT_SINK_ADDR = "CC:1E:57:00:00:08";
const SOURCE_ADDR = "CC:1E:57:00:00:07";
const SCANNER_ADDR = "CC:1E:57:00:00:09";
const ws = (node, address) =>
  `ws://localhost:7681/v1/websocket/bt?name=${encodeURIComponent(node)}&address=${address}`;

// --- what the sink calls itself, and what the air can carry -----------------
//
// The name is the sink's *script* talking: `android::BluetoothGattServer(name)`
// is where a scripted device's name comes from, so the field on the page edits
// that literal and re-runs the script rather than keeping a second name beside
// it. One source of truth, and the change is visible in the editor.
//
// The budget is the part that has to be said out loud. A legacy advertisement
// is 31 octets, and `gap::advertising::fit_within_legacy_limit` fits a name
// into whatever is left by TRIMMING it — while still labelling the result a
// *Complete* Local Name, and while the scan response carries the full name
// untrimmed. A scanner therefore shows the name flickering between two forms
// and neither the device nor the page says a thing about it.
//
// This sink's advertisement, read off the air:
//
//   02 01 06                                     flags            3
//   0B 09 "web-speake"                           complete name    2 + 10
//   05 03 5018 4E18                              PACS + ASCS      6
//   09 16 4E18 010600000000                      ASCS announcement 10
//                                                                 -- = 31
//
// which is to say the shipped name `web-speaker` is ALREADY one octet over and
// goes out as `web-speake`. So the page shows the trimmed form everywhere it
// shows a name, and says when it trimmed. The overhead below mirrors the
// `advertise_service_uuid` / `advertise_service_data` lines of
// `catalog/devices/le-audio-sink.rhai`; edit those and this needs editing too,
// which is why the readout states the budget rather than hiding it.
const MAX_ADV_LEN = 31;
const SINK_ADV_OVERHEAD = 19; // flags 3 + the two 16-bit UUIDs 6 + service data 10
const NAME_BUDGET = MAX_ADV_LEN - SINK_ADV_OVERHEAD - 2; // less the name AD's own length+type

const utf8 = new TextEncoder();
const NAME_IN_SCRIPT = /(BluetoothGattServer\s*\(\s*")([^"]*)(")/;

/// The name a script's primary server is built with, or "" if it names none.
const scriptName = (text) => (NAME_IN_SCRIPT.exec(text) || [, , ""])[2];

/// The same script with that name replaced. Returns null when there is no
/// `BluetoothGattServer("…")` to edit — a script the field cannot honour, which
/// is worth saying rather than silently doing nothing.
///
/// The replacement is a FUNCTION, not a `$1…$3` template: a name is arbitrary
/// text a person typed, and `String.replace` reads `$&`, `$1` and `$'` inside a
/// template string as references to the match. Someone naming their speaker
/// `$1` would have got the whole `BluetoothGattServer("` prefix spliced into
/// its own name, and the corruption would have shown up as a Rhai parse error
/// pointing at a line they never edited.
const withScriptName = (text, name) =>
  NAME_IN_SCRIPT.test(text)
    ? text.replace(NAME_IN_SCRIPT, (_, open, __, close) => open + name + close)
    : null;

/// A name a Rhai string literal can hold as typed. A quote would close the
/// literal and a backslash would start an escape, so both are refused up front
/// rather than escaped: this field's job is to name a speaker, and a name that
/// needs escaping is one the script editor below can be used for directly.
const nameFitsScript = (name) => !/["\\]/.test(name);

/// What a name actually goes out as: trimmed to the budget a whole `char` at a
/// time, exactly as `fit_within_legacy_limit` does, so a multi-byte name is
/// never cut mid-codepoint.
function onAir(name) {
  let trimmed = String(name ?? "");
  while (utf8.encode(trimmed).length > NAME_BUDGET) trimmed = [...trimmed].slice(0, -1).join("");
  return trimmed;
}

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
const OP_UNMUTE = 0x05;
const OP_MUTE = 0x06;

// The speaker artwork's own two colours. The page's tokens are picked for text
// on white -- --good is #1a7f37, which is nearly black once it is a disc on a
// dark cabinet -- so the mock's lit parts use the brighter pair the Car page's
// phone already uses for its battery. Everything that is UI rather than
// artwork stays on the tokens; this is the one place the two diverge, and the
// reason is the ground the colour sits on.
const ART_LIVE = "#46d17a";
const ART_MUTED = "#e5534b";

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

let mode = "in-page"; // "in-page" (a wasm WebLink) | "websocket" (netsim) | "usb" (bridge)

// --- USB mode state ---------------------------------------------------------
// The radio itself lives in a Worker (see common/radio-worker.js for why a
// page timer must not drive a radio protocol). The page holds only the
// worker handle, its own player — A2DP is 44.1/48 kHz stereo where the LE
// player is 16 kHz mono, and a player's rate is fixed at creation — and the
// render targets. The phone is the source, so the LE source, scanner and
// script editor all stand down in this mode.
let radioWorker = null;
let usbPlayer = null;
let usbRate = 0;
let soundOn = false; // the gate was clicked; a later-created player may enable itself
// There is no Volume Control Service on the Classic path — the phone runs
// AVRCP absolute volume at *its* end, and this end's volume is simply the
// page's gain. The same buttons, slider and keys drive it, through here.
let usbVolume = 128;
let usbMuted = false;
// The source half: decoded 44.1 kHz stereo PCM waiting for the worker's
// need-pcm credit asks, sent one ~5 s slice at a time so a whole song is
// not one 30 MB postMessage.
let usbSrcPcm = null;
let usbSrcCursor = 0;
let usbSrcRunning = false;
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

// The sink's identity, both halves of it editable from the page. The address
// lives in the socket URL (netsim reads it off the query string), so changing
// it means a new socket; the name lives in the script, so changing it means a
// re-run. Neither is a constant any more, and `sinkName` is kept beside the
// script only so the page can tell an applied name from a typed one.
let sinkAddr = DEFAULT_SINK_ADDR;
let sinkName = "";

let target = sinkAddr;
let runStart = 0;
let lastRenderAt = 0;
let lastCounter = -1; // the sink's change counter, echoed back with commands
let lastMuted = false; // the sink's mute flag, so the meter can read empty
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
  // The zone is the loudest thing in the column until it has been answered.
  // A call to action that never acknowledges being obeyed keeps asking.
  $("drop").classList.add("loaded");
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
  applyIdentityLock();
}

function stop() {
  playing = false;
  $("play").disabled = !frames.length;
  $("stop").disabled = true;
  $("hidden-warning").hidden = true;
  applyIdentityLock();
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
  // The meter is deliberately NOT touched here. It used to be
  // `lit = (volume / 255) * 16` -- the volume again, in a widget shaped like a
  // signal meter, so it danced when you dragged the slider and sat perfectly
  // still while music played. Volume was on screen five times over (arcs,
  // meter, slider, the number beside it, and Volume State) and the one display
  // that looked like it reported the audio was the one that never did. It is
  // driven from the decoded signal now, in renderMeter().
  ["w1", "w2", "w3", "w4"].forEach((id, i) => {
    const threshold = (i + 1) * 60; // each wave lights as the volume climbs
    // Fully off, not almost off: an unlit arc is drawn underneath in
    // --border, so hiding this one reveals the empty slot rather than a hole.
    $(id).setAttribute("opacity", !muted && volume >= threshold ? "1" : "0");
  });
  lastMuted = muted;
  syncMuteToggle(muted);
  $("muteMark").setAttribute("opacity", muted ? "1" : "0");
  $("cone").setAttribute("fill", muted ? ART_MUTED : ART_LIVE);
  // Terser than it was, and still every field of the characteristic: the
  // number now also sits beside the slider, where it is actually being read.
  $("readout").innerHTML =
    `Volume State <b>${volume}</b> · <b>${muted ? "muted" : "unmuted"}</b> ` +
    `· ctr <b>${changeCounter}</b>`;
  $("vol-num").textContent = String(volume);
  if (document.activeElement !== slider) slider.value = String(volume);
  player.setVolume(volume, muted);
}

// The sink's GATT, as the page renders it. Everything visible about the device
// — including the volume the audio is played at — is read back out of the live
// characteristic values, never from a variable the UI kept on the side.
function renderSink(status) {
  // The name is the one the sink's own GATT server advertises -- and
  // *advertises* is meant literally: `status.name` is what the script asked to
  // be called, which is not necessarily what fits in the advertisement. The
  // header showed the asked-for name and the air carried a shorter one, so the
  // page and a scanner beside it disagreed with no way to tell which was lying.
  sinkHead.setName(onAir(status.name));
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

// Meter level, held between frames so the bars fall smoothly. Audio arrives in
// batches and renders at ~10 Hz; without the decay the meter would snap to
// zero in every gap between batches.
let meterLevel = 0;
const METER_DECAY = 0.55;
// How long a peak still counts as "now". This must be comfortably LONGER than
// the render interval or the meter goes dark between frames: at 120 ms it was
// tighter than the loop's own period whenever the tab was throttled, so the
// display depended on the render rate rather than on the audio. The loop asks
// for 20 ms, so 400 ms holds through ordinary jitter and lets the decay -- not
// the freshness cutoff -- be what takes the bars down when a track ends.
const PEAK_FRESH_MS = 400;

function renderMeter() {
  // Muted means the speaker is emitting nothing, so the meter reads empty
  // however loud the arriving signal is -- that is device state, not volume.
  // The peak counts only while it is FRESH. Left as a standing value it meant
  // the meter froze at the last batch's level when a file ended, still lit
  // with nothing playing; consuming it on read instead made the reading depend
  // on whether this loop ran before or after play(), which is not something a
  // display should care about. A timestamp settles it either way.
  let peak = 0;
  const active = mode === "usb" ? usbPlayer : player;
  if (active && !lastMuted && active.state === "running") {
    const age = performance.now() - (active.stats.peakAt || 0);
    if (age < PEAK_FRESH_MS) peak = active.stats.peak;
  }
  meterLevel = peak > meterLevel ? peak : meterLevel * METER_DECAY;
  // sqrt curve: a linear peak leaves quiet passages invisible, because most
  // music sits well below full scale.
  const lit = Math.round(Math.sqrt(meterLevel) * 16);
  const bars = $("meter").children;
  for (let i = 0; i < bars.length; i++) bars[i].className = i < lit ? "on" : "";
}

function renderSinkStats() {
  const active = mode === "usb" ? usbPlayer : player;
  if (!active) {
    $("sink-stats").textContent = "no media yet · audio off";
    return;
  }
  const stats = active.stats;
  const batches = mode === "usb" ? "batches" : "SDUs";
  $("sink-stats").textContent =
    `${stats.received} ${batches} received` +
    (stats.received ? ` · ${stats.underruns} underruns` : "") +
    ` · audio ${active.state}`;
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

// --- the source's chooser: which sink to stream to --------------------------
// The built-in sink is always offered; anything else comes off the air. A
// scanner on netsim is the honest way to fill this list — the same advertising
// reports a phone would use to find a pair of earbuds — and a typed address is
// the fallback for a device that is not advertising while you look.
//
// Devices are listed BY NAME, because that is how a person identifies one: you
// name the sink in the panel opposite and then pick it here. The address is
// still shown, subordinate, and is still what the option's value is keyed on —
// two sinks can advertise the same name and only the address tells them apart,
// which is exactly what the Earbud L / Earbud R pair elsewhere in this project
// does. A name is a label; an address is an identity.

function isAudioSink(report) {
  return (
    report.service_uuids.includes(ASCS_UUID) ||
    report.service_uuids.includes(PACS_UUID) ||
    (report.service_data || []).some((entry) => entry.tag === ASCS_UUID)
  );
}

const escapeHtml = (text) =>
  String(text).replace(
    /[&<>"]/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c],
  );

function renderSinkOptions() {
  const select = $("sink-pick");
  const keep = select.value;
  // The built-in sink is named by what it puts on the air, not by what its
  // script asked to be called -- see onAir(). An unnamed device is said to be
  // unnamed rather than being labelled with its address twice.
  const label = (name, address, note) =>
    `${escapeHtml(name || "(no name advertised)")} — ${address}${note ? ` · ${note}` : ""}`;
  const options = [
    `<option value="${sinkAddr}">${label(onAir(sinkName), sinkAddr, "on this page")}</option>`,
  ];
  if (discovered.size) {
    // Sorted by name so the list reads the way it is now labelled, with the
    // address breaking ties between sinks that share one.
    const entries = [...discovered.entries()].sort(
      (a, b) =>
        (a[1].name || "￿").localeCompare(b[1].name || "￿") ||
        a[0].localeCompare(b[0]),
    );
    options.push(
      `<optgroup label="Advertising an LE Audio sink">` +
        entries
          .map(([address, info]) => `<option value="${address}">${label(info.name, address)}</option>`)
          .join("") +
        `</optgroup>`,
    );
  }
  options.push(`<option value="__other">Another address…</option>`);
  select.innerHTML = options.join("");
  // Rebuilding the list must not silently retarget a stream in flight.
  select.value = [...select.options].some((o) => o.value === keep) ? keep : sinkAddr;
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
    sink = new WebPeripheral(ws("web-audio-sink", sinkAddr), editor.value);
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
    sinkIndex = next.add_peripheral(sinkAddr, editor.value);
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
    central = next.add_central(SOURCE_ADDR, sinkAddr);
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

function dropUsb() {
  radioWorker?.postMessage({ op: "sink-stop" });
  try {
    usbPlayer?.close();
  } catch (_) {
    /* already closed */
  }
  usbPlayer = null;
  usbRate = 0;
}

function teardown() {
  dropUsb();
  radioWorker?.terminate();
  radioWorker = null;
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
  const isUsb = mode === "usb";
  // The USB card replaces the LE source outright, and strips the sink card
  // down to the parts that still mean something: the speaker face, the meter,
  // the gate and the volume. The script editor, identity fields and VCS
  // controls all describe the LE peripheral, which is not on the air here.
  $("usb-card").hidden = !isUsb;
  $("usb-source-note").hidden = !isUsb;
  $("le-source").hidden = isUsb;
  // The Name field stays: one speaker identity, whichever radio hosts it.
  // The address does not — on a dongle the silicon owns the address, and the
  // dongle picker is how one is chosen.
  for (const el of [$("sink-script"), $("air-note")]) {
    if (el) el.hidden = isUsb;
  }
  // One verb: connect applies the name, so a second Apply button is a
  // second way to do the same thing standing somewhere else. (`hidden`
  // loses to the ident grid's explicit display, so say display directly.)
  $("ident-apply").style.display = isUsb ? "none" : "";
  if (isUsb) {
    $("mode-hint").textContent =
      "USB dongle — a Classic A2DP sink on real silicon; the phone is the source.";
    $("status").textContent = "waiting for the bridge";
    loadDongleList();
    setUsbVolume(usbVolume, usbMuted);
    return;
  }
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
  target = sinkAddr;
  renderSinkOptions();
  applyMode();
  if (mode === "usb") {
    sourceHead.setRunning(false);
    sourceHead.setState(false, "stopped — press play, or use any source in the room");
    sinkHead.setRunning(false);
    sinkHead.setState(false, "USB bridge — not connected");
    return; // the headers' run controls build the devices
  }
  if (mode === "in-page") {
    sourceHead.setState(false, "connecting…");
    sinkHead.setState(false, "starting…");
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

// --- USB backend ------------------------------------------------------------
// The page renders; the worker radios. postMessage delivery is not
// visibility-throttled, so this keeps working with the tab in the background
// — which is exactly where the tab is while a person pairs their phone.

function usbLog(line) {
  const log = $("usb-log");
  log.textContent += (log.textContent ? "\n" : "") + line;
  log.scrollTop = log.scrollHeight;
}

function ensureWorker() {
  if (radioWorker) return radioWorker;
  radioWorker = new Worker(new URL("../common/radio-worker.js", import.meta.url), {
    type: "module",
  });
  radioWorker.onmessage = onRadioMessage;
  radioWorker.onerror = (e) => {
    $("usb-stage").textContent = `worker failed: ${e.message ?? "see console"}`;
  };
  return radioWorker;
}

function onRadioMessage({ data: m }) {
  if (m.op === "status") window.__usbStatus = m; // the tracing surface, inspectable
  if (m.op === "source-status") window.__usbSourceStatus = m;
  if (mode !== "usb") return; // a late message after switching controllers
  if (m.op === "source-need-pcm") {
    feedSourcePcm();
    return;
  }
  if (m.op === "source-error") {
    $("usb-src-stage").textContent = m.message;
    sourceHead.setRunning(false);
    usbSrcRunning = false;
    return;
  }
  if (m.op === "source-status") {
    for (const line of m.log) usbSrcLog(line);
    $("usb-src-stage").textContent =
      m.failure ?? `${m.stage}${m.packets_sent ? ` · ${m.packets_sent} RTP packets` : ""}`;
    sourceHead.setState(m.stage.startsWith("5."), m.failure ? "failed" : m.stage);
    return;
  }
  if (m.op === "error") {
    $("usb-stage").textContent = m.message;
    sinkHead.setState(false, "error");
    sinkHead.setRunning(false);
    return;
  }
  if (m.op === "status") {
    if (m.bd_addr) sinkHead.setAddress(m.bd_addr);
    for (const line of m.log) usbLog(line);
    const detail = m.frames
      ? ` · ${m.frames} SBC frames` +
        (m.rate ? ` · ${m.rate} Hz × ${m.channels}` : "") +
        (m.undecodable ? ` · ${m.undecodable} undecodable bytes` : "")
      : "";
    $("usb-stage").textContent = m.stage + detail;
    sinkHead.setState(m.stage === "streaming", m.stage);
    $("status").textContent = m.stage;
    return;
  }
  if (m.op === "pcm") {
    ensureUsbPlayer(m.rate);
    const mono = m.channels === 2 ? downmixToMono(m.pcm) : m.pcm;
    usbPlayer.play([mono], null);
  }
}

/// The bridge's own list of what is plugged in, for the device picker.
async function loadDongleList() {
  const base = usbBridgeHttp();
  const pick = $("usb-device");
  try {
    const { devices } = await (await fetch(`${base}/devices`)).json();
    for (const target of [pick, $("usb-src-device")]) {
      target.innerHTML = "";
      for (const d of devices) {
        const option = document.createElement("option");
        option.value = d.selector;
        option.textContent = `${d.selector} — ${d.product}`;
        target.append(option);
      }
      target.disabled = devices.length === 0;
    }
    // Different halves want different dongles: default the source to the
    // second one, since the sink took the first.
    if (devices.length > 1) $("usb-src-device").selectedIndex = 1;
    if (!devices.length) $("usb-stage").textContent = "the bridge reports no dongles";
  } catch (e) {
    pick.innerHTML = "<option value=''>bridge not reachable</option>";
    pick.disabled = true;
  }
}

function buildUsb() {
  $("usb-log").textContent = "";
  $("usb-stage").textContent = "connecting to the bridge…";
  sinkHead.setState(false, "connecting…");
  const base = usbBridgeUrl().trim().replace(/\/+$/, "");
  const device = $("usb-device").value;
  const url = device ? `${base}/?device=${encodeURIComponent(device)}` : `${base}/`;
  // One speaker identity: the same Name field that names the LE sink names
  // this one. Two radios advertising two different names is how a phone
  // ends up seeing speakers nobody asked for.
  const name = $("ident-name").value.trim() || "webspeaker";
  ensureWorker().postMessage({ op: "sink-start", url, name });
  sinkHead.setRunning(true);
}

function disconnectUsb() {
  dropUsb(); // sink-stop: the worker frees the sink, closing the socket —
  // and the bridge resets the dongle, so nothing keeps advertising.
  $("usb-stage").textContent = "not connected";
  sinkHead.setRunning(false);
  sinkHead.setState(false, "USB bridge — not connected");
}

/// A player at the stream's own rate — see lc3-player.js for why the graph
/// must run at the stream rate rather than resampling per batch. A2DP rates
/// are unknown until Set_Configuration, so the player is (re)built on first
/// PCM; the gate's sticky user activation carries over to the new context.
function ensureUsbPlayer(rate) {
  if (usbPlayer && usbRate === rate) return;
  try {
    usbPlayer?.close();
  } catch (_) {
    /* already closed */
  }
  usbPlayer = createSduPlayer({ sampleRate: rate });
  usbRate = rate;
  if (soundOn) usbPlayer.enable();
  usbPlayer.setVolume(usbVolume, usbMuted);
}

/// Sets the USB speaker's volume and mute — the page's gain, drawn with the
/// same cone, arcs and readout the LE sink uses, because to a person it is
/// the same speaker.
function setUsbVolume(volume, muted) {
  usbVolume = Math.max(0, Math.min(255, Math.round(volume)));
  usbMuted = muted;
  slider.value = usbVolume;
  $("vol-num").textContent = usbVolume;
  usbPlayer?.setVolume(usbVolume, usbMuted);
  applySpeaker(usbVolume, usbMuted, "—");
}

// --- the USB source ---------------------------------------------------------

const SRC_RATE = 44100;

/// The example's melody, in JS: a major arpeggio up and down. A deliberate
/// tune says "frames arrived in order and decoded" the way a steady test
/// tone cannot — a stuck buffer also hums.
function renderMelody() {
  const notes = [[440, 260], [554.37, 260], [659.25, 260], [880, 420],
                 [659.25, 260], [554.37, 260], [440, 520], [0, 360]];
  const total = notes.reduce((n, [, ms]) => n + Math.round(SRC_RATE * ms / 1000), 0);
  const pcm = new Int16Array(total * 2);
  let at = 0;
  for (const [f, ms] of notes) {
    const samples = Math.round(SRC_RATE * ms / 1000);
    const fade = Math.max(1, Math.min(SRC_RATE / 200, samples / 4));
    for (let n = 0; n < samples; n++) {
      let a = 0;
      if (f > 0) {
        const env = n < fade ? n / fade : n + fade >= samples ? (samples - n) / fade : 1;
        a = Math.sin(2 * Math.PI * f * n / SRC_RATE) * env * 0.35;
      }
      const sample = Math.max(-32768, Math.min(32767, Math.round(a * 32767)));
      pcm[at++] = sample;
      pcm[at++] = sample;
    }
  }
  return pcm;
}

/// Decodes a picked file to interleaved 44.1 kHz stereo i16 — the format the
/// stream is negotiated at. (The LE path resamples to 16 kHz mono for LC3;
/// this is deliberately a separate pipeline.)
async function decodeForA2dp(file) {
  const bytes = await file.arrayBuffer();
  const probe = new AudioContext();
  const decoded = await probe.decodeAudioData(bytes);
  probe.close();
  const offline = new OfflineAudioContext(2, Math.ceil(decoded.duration * SRC_RATE), SRC_RATE);
  const node = offline.createBufferSource();
  node.buffer = decoded;
  node.connect(offline.destination);
  node.start();
  const rendered = await offline.startRendering();
  const left = rendered.getChannelData(0);
  const right = rendered.getChannelData(rendered.numberOfChannels > 1 ? 1 : 0);
  const pcm = new Int16Array(left.length * 2);
  for (let i = 0; i < left.length; i++) {
    pcm[2 * i] = Math.max(-32768, Math.min(32767, Math.round(left[i] * 32767)));
    pcm[2 * i + 1] = Math.max(-32768, Math.min(32767, Math.round(right[i] * 32767)));
  }
  return pcm;
}

function usbSrcLog(line) {
  const log = $("usb-src-log");
  log.textContent += (log.textContent ? "\n" : "") + line;
  log.scrollTop = log.scrollHeight;
}

/// Feeds the worker's credit ask: one ~5 s slice per ask, looping the
/// melody but playing a file once through.
function feedSourcePcm() {
  if (!usbSrcRunning || !usbSrcPcm) return;
  const slice = SRC_RATE * 2 * 5;
  if (usbSrcCursor >= usbSrcPcm.length) {
    if (usbSrcPcm.isMelody) usbSrcCursor = 0; // the melody loops
    else return; // the file played through; the runner drains what is left
  }
  const chunk = usbSrcPcm.slice(usbSrcCursor, usbSrcCursor + slice);
  usbSrcCursor += chunk.length;
  radioWorker?.postMessage({ op: "source-pcm", pcm: chunk }, [chunk.buffer]);
}

async function startUsbSource() {
  const base = usbBridgeUrl().trim().replace(/\/+$/, "");
  const device = $("usb-src-device").value;
  if (!device) {
    $("usb-src-stage").textContent = "no dongle picked";
    return;
  }
  $("usb-src-log").textContent = "";
  $("usb-src-stage").textContent = "connecting to the bridge…";
  const file = $("usb-src-file").files[0];
  try {
    usbSrcPcm = file ? await decodeForA2dp(file) : renderMelody();
    if (!file) usbSrcPcm.isMelody = true;
  } catch (e) {
    $("usb-src-stage").textContent = `could not decode: ${e}`;
    return;
  }
  usbSrcCursor = 0;
  usbSrcRunning = true;
  const url = `${base}/?device=${encodeURIComponent(device)}`;
  ensureWorker().postMessage({
    op: "source-start",
    url,
    target: $("usb-src-target").value.trim(),
  });
  sourceHead.setRunning(true);
}

function stopUsbSource() {
  usbSrcRunning = false;
  radioWorker?.postMessage({ op: "source-stop" });
  $("usb-src-stage").textContent = "stopped";
  sourceHead.setRunning(false);
  sourceHead.setState(false, "stopped");
}

/// Interleaved stereo to mono. The shared player schedules one mono channel;
/// averaging is the downmix that cannot clip.
function downmixToMono(pcm) {
  const mono = new Int16Array(pcm.length >> 1);
  for (let i = 0; i < mono.length; i++) {
    mono[i] = (pcm[2 * i] + pcm[2 * i + 1]) >> 1;
  }
  return mono;
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
    // Advertisements only. A device's two on-air names need not agree -- the
    // advertisement carries a name trimmed to the 31-octet budget, the scan
    // response carries the full one -- and taking whichever arrived last is
    // what makes a name flicker between its two forms in a list. The
    // advertisement is the one a scanner sees without asking, so it wins.
    // (isAudioSink already rejects most scan responses, which carry no service
    // UUIDs; that is a side effect of the filter, not a decision, so say it.)
    if (report.scan_response) continue;
    if (!isAudioSink(report)) continue;
    const address = report.address.toUpperCase();
    if (address === sinkAddr) continue; // already the first option
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
  sinkHead.setState(true, "advertising", "ok");
  // No CIS in the in-page controller: SDUs ride the connection handle, so the
  // source's dot means "there is a central attached and SDUs can go out".
  sourceHead.setState(linkCentral >= 0, linkCentral >= 0
    ? "SDUs on the connection handle"
    : "no central — streaming unavailable", linkCentral >= 0 ? "ok" : "warn");
}

function loop() {
  const now = performance.now();
  try {
    if (mode === "usb") {
      // nothing: the worker radios and pushes; the page renders on message
    } else if (mode === "websocket") {
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
  renderMeter();
  if (now - lastRenderAt > 100) lastRenderAt = now;
}

// Keyboard: arrows step the volume the way a device's own buttons would. This
// one listener is on `document`, so unmount() has to take it off again.
function onKeyDown(event) {
  if (event.target === slider || event.target === editor) return;
  if (event.target instanceof HTMLInputElement || event.target instanceof HTMLTextAreaElement) return;
  const send = mode === "usb" ? usbOp : sendOp;
  if (event.key === "ArrowUp") {
    send(OP_UP);
    event.preventDefault();
  }
  if (event.key === "ArrowDown") {
    send(OP_DOWN);
    event.preventDefault();
  }
  if (event.key === "m") send(OP_UNMUTE_UP);
}

/// The same opcodes the VCS buttons write, applied to the USB speaker's
/// gain. VOLUME_STEP matches the LE sink's script so the two speakers feel
/// the same under the same buttons.
const USB_VOLUME_STEP = 16;
function usbOp(op) {
  switch (op) {
    case OP_DOWN:
      return setUsbVolume(usbVolume - USB_VOLUME_STEP, usbMuted);
    case OP_UP:
      return setUsbVolume(usbVolume + USB_VOLUME_STEP, usbMuted);
    case OP_UNMUTE_UP:
      return setUsbVolume(usbVolume + USB_VOLUME_STEP, false);
    case OP_UNMUTE:
      return setUsbVolume(usbVolume, false);
    case OP_MUTE:
      return setUsbVolume(usbVolume, true);
    default:
      return setUsbVolume(usbVolume, usbMuted);
  }
}

// --- markup ----------------------------------------------------------------

// Icons are inline SVG rather than emoji: emoji are a different size and
// weight in every platform's font, they cannot be dimmed when a button is
// disabled, and 🔊 on a technical page reads as decoration. These draw in
// `currentColor`, so they inherit the button's state for free.
// The cone is deliberately small and hard-left, leaving over half the box for
// the mark that distinguishes one opcode from the next. A first attempt gave
// each icon a large cone and a small mark, and at this size all five were the
// same grey blob -- which is worse than the words they replaced.
const CONE = `<path d="M1.6 6.2h2L5.9 3.6v8.8L3.6 9.8h-2z" fill="currentColor" stroke="currentColor" stroke-width="1.1" stroke-linejoin="round"/>`;
const icon = (body) =>
  `<svg viewBox="0 0 20 16" width="20" height="16" aria-hidden="true" fill="none"` +
  ` stroke="currentColor" stroke-width="1.7" stroke-linecap="round">${CONE}${body}</svg>`;

// The arcs are struck from the cone's own mouth (5.9, 8) rather than drawn as
// three unrelated curves at three unrelated origins, which is what made the
// old pair read as parentheses instead of as sound leaving a speaker. Sweep
// narrows as the radius grows so the fan stays inside the 16-unit box.
const WAVE_1 = `<path d="M8.73 4.63A4.4 4.4 0 0 1 8.73 11.37"/>`;
const WAVE_2 = `<path d="M11.27 2.63A7.6 7.6 0 0 1 11.27 13.37"/>`;
// (A third arc existed while the two unmute icons were told apart by arc
// count. That is what the plus does now, so it has no user.)

const ICON = {
  // Volume steps: a full-height arithmetic sign, nothing speaker-ish beside
  // the cone, so they never blur into the three mute glyphs.
  // The bars are short and stand well clear of the cone: a long one butted up
  // against it turned "speaker minus" into a left arrow.
  // Both signs sit in one 10.0-15.4 box so the pair share a stem line, and
  // that box ends where the widest arc ends -- the two families are drawn
  // differently on purpose, but they are not allowed to sit at different
  // optical centres in the same strip.
  up: icon(`<path d="M12.7 5.3v5.4M10 8h5.4"/>`),
  down: icon(`<path d="M10 8h5.4"/>`),
  // Mute is the X *instead of* the arcs, which is the universal glyph, so it
  // occupies the band the arcs would have. Square, not the old squat 7x5.6.
  mute: icon(`<path d="M9.7 5.1l5.7 5.8M15.4 5.1l-5.7 5.8"/>`),
  // These two differ by a PLUS, not by how many arcs they have.
  //
  // Arc count was tried twice -- one-vs-two, then two-vs-three -- and both
  // fail the same way: more arcs is the universal loudness scale, so the pair
  // reads as "softer / louder" and looks like a duplicate of the - and +
  // buttons beside them. It is not a loudness scale. Arcs here mean the mute
  // flag is clear; the volume number is a separate thing that only - and +
  // move. A reader asked outright what the difference was, which is the whole
  // answer to whether the icons worked.
  //
  // 0x03 is defined as 0x05 plus 0x01 -- unmute AND step up -- so it is drawn
  // that way: the unmute glyph carrying the same plus that 0x01 uses. The
  // fan is trimmed to one arc to make room, which is fine because the plus,
  // not the arc count, is now what tells them apart.
  unmute: icon(`${WAVE_1}${WAVE_2}`),
  unmuteUp: icon(`${WAVE_1}<path d="M15 5.3v5.4M12.3 8h5.4"/>`),
  // The sound gate is not a volume control at all, so it gets a power symbol —
  // no cone, nothing that could be mistaken for an opcode button.
  power:
    `<svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true" fill="none"` +
    ` stroke="currentColor" stroke-width="1.7" stroke-linecap="round">` +
    `<path d="M8 2.2v5.2"/><path d="M4.5 4.6a4.8 4.8 0 1 0 7 0"/></svg>`,
  check:
    `<svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true" fill="none"` +
    ` stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">` +
    `<path d="M3.2 8.4l3.2 3.2 6.4-7.2"/></svg>`,
};

// The drop zone's glyph. Bars rather than a document-with-a-note, because the
// same bars are the sink's level meter at the other end of the page: a file
// goes in as a waveform and comes out on a meter, and drawing both from one
// vocabulary is the cheapest way to say that the two panels are one path.
const WAVEFORM =
  `<svg class="drop-art" viewBox="0 0 56 26" aria-hidden="true" fill="none"` +
  ` stroke="currentColor" stroke-width="3" stroke-linecap="round">` +
  `<path d="M3 9v8M11 5v16M19 2v22M27 7v12M35 3v20M43 8v10M51 10v6"/></svg>`;

// The five control-point opcodes the page exposes, in the order a person
// reaches for them: the two steps, then the three mute states. The hex stays
// in the tooltip because the prose below the panel is about exactly that — an
// icon says what the button does, the title says what goes on the wire.
const OPS = [
  { op: OP_DOWN, name: "Relative Volume Down", icon: "down" },
  { op: OP_UP, name: "Relative Volume Up", icon: "up" },
  // Mute and Unmute are two opcodes but one *state*, and a device never
  // accepts both at once — so they share one toggle button whose opcode
  // follows the state (see syncMuteToggle). Two adjacent buttons where one
  // is always a no-op read as a broken pair, and were asked about exactly
  // that way.
  { op: OP_MUTE, name: "Mute", icon: "mute", group: true, id: "mute-toggle" },
  { op: OP_UNMUTE_UP, name: "Unmute / Relative Volume Up", icon: "unmuteUp" },
];

const opButton = (o) =>
  `<button class="icon-btn${o.group ? " grouped" : ""}"${o.id ? ` id="${o.id}"` : ""}` +
  ` data-op="${o.op}"` +
  ` title="0x${o.op.toString(16).padStart(2, "0").toUpperCase()} · ${o.name}"` +
  ` aria-label="${o.name}">${ICON[o.icon]}</button>`;

/// Points the mute toggle at the opcode that currently applies: Mute on a
/// live speaker, Unmute on a muted one. The hover title keeps teaching the
/// control-point idiom — it names the opcode this press will write.
function syncMuteToggle(muted) {
  const toggle = $("mute-toggle");
  if (!toggle) return;
  const op = muted ? OP_UNMUTE : OP_MUTE;
  const name = muted ? "Unmute" : "Mute";
  toggle.dataset.op = op;
  toggle.title = `0x${op.toString(16).padStart(2, "0").toUpperCase()} · ${name}`;
  toggle.setAttribute("aria-label", name);
  toggle.innerHTML = ICON[muted ? "unmute" : "mute"];
}

const STYLE_ID = "simble-audio-style";

function injectStyles() {
  if (document.getElementById(STYLE_ID)) return;
  const style = document.createElement("style");
  style.id = STYLE_ID;
  style.textContent = `
  /* Each column is its own flex stack so panels of unequal height do not drag
     their neighbour's top edge down. */

  /* The drop zone is the page's one real call to action -- nothing else here
     does anything until a file is loaded -- and it used to be the quietest
     thing in the column: a grey dashed box round the browser's own file
     button, which is the only control on the site not drawn by the site. So
     the whole box is the target now (it is a <label>, the input is only
     visually hidden so it stays focusable), and it carries the accent that
     everywhere else on the site means "the thing to click first".
     Colour is the state: accent = give me a file, green = got one. */
  /* Two columns -- artwork beside the words, not stacked above them. Centred
     and stacked, this was 189px tall: taller than the speaker it feeds, for a
     control that is one line of instruction and a list of extensions. The
     waveform spans the three text rows, so the block is as tall as its text
     and no taller. */
  .audio-page .drop { display: grid; grid-template-columns: auto minmax(0, 1fr);
    align-items: center; column-gap: 0.9rem; row-gap: 0.1rem;
    border: 1.5px dashed var(--accent); border-radius: 10px;
    background: rgba(9,105,218,0.045); /* --accent, washed out; the same trick .chr.pulse uses */
    padding: 0.85rem 1.1rem; text-align: left; cursor: pointer;
    color: var(--accent); /* the waveform draws in currentColor, so state flips it */
    transition: border-color .15s, background .15s, color .15s; }
  .audio-page .drop.over { border-color: var(--good); background: rgba(26,127,55,0.07);
    color: var(--good); }
  /* Once a file is in, the call to action has been answered: it stops
     shouting and turns into a receipt. #track below spells out which file. */
  .audio-page .drop.loaded { border-style: solid; border-color: var(--good);
    background: rgba(26,127,55,0.05); color: var(--good); }
  .audio-page .drop:focus-within { outline: 2px solid var(--accent); outline-offset: 2px; }
  /* Kept in the layout so it is still tab-reachable and still the label's
     control; display:none would take both away. */
  .audio-page .drop input[type=file] { position: absolute; width: 1px; height: 1px;
    opacity: 0; pointer-events: none; }
  /* Three tiers, one per question: what to do, how else to do it, what will be
     accepted. One paragraph carrying all three wrapped mid-list, and a list of
     file extensions broken across two lines is the kind of detail that makes a
     panel look unconsidered even when nobody can say why. */
  .audio-page .drop-art { display: block; width: 3rem; height: auto;
    grid-column: 1; grid-row: 1 / span 3; align-self: center; }
  .audio-page .drop-title { grid-column: 2; font-size: var(--fs-title); font-weight: 600;
    color: var(--text); line-height: 1.2; }
  .audio-page .drop-sub { grid-column: 2; font-size: var(--fs-label); color: var(--dim); }
  .audio-page .drop-sub b { color: var(--text); font-weight: 600; }
  .audio-page .drop-formats { grid-column: 2; font-size: var(--fs-meta); color: var(--dim); }
  .audio-page .readout { font-family: ui-monospace, Menlo, monospace; color: var(--dim);
    font-size: var(--fs-body); margin-top: 0.6rem; }
  .audio-page .readout b { color: var(--text); }
  .audio-page .bar { height: 8px; background: var(--panel2); border-radius: 4px;
    overflow: hidden; margin-top: 0.7rem; }
  .audio-page .bar > div { height: 100%; width: 0; background: var(--good);
    transition: width .1s linear; }
  .audio-page .field { display: block; margin-top: 0.9rem; font-size: var(--fs-body); color: var(--dim); }
  .audio-page input[type=text] { font-family: ui-monospace, Menlo, monospace;
    padding: 0.35rem 0.5rem; border: 1px solid var(--border); border-radius: 6px; width: 14rem; }
  .audio-page #sink-pick { max-width: 100%; }

  /* The sink's identity, on one wrapping row: two labelled fields and the one
     button that commits both. One button rather than two because both edits
     are the same operation underneath -- stand the device up again -- and two
     Apply buttons a centimetre apart invite the reader to wonder which does
     which. The name is elastic and the address is not: an address is a fixed
     17 characters, so it gets exactly that and the name takes the slack. */
  .audio-page .ident { display: flex; flex-wrap: wrap; align-items: center;
    gap: 0.4rem 0.55rem; margin: 0.65rem 0 0.45rem; }
  .audio-page .ident-label { font-size: var(--fs-label); color: var(--dim); }
  .audio-page .ident input[type=text] { width: auto; }
  .audio-page .ident #ident-name { flex: 1 1 7rem; min-width: 6rem; }
  /* An address is exactly 17 characters in a monospace face, so the field is
     sized to hold all of them -- a box that clips its own contents is the one
     thing a field showing an identity must not do. */
  /* Disabled is the honest state while a stream is up, so it has to LOOK
     disabled -- the browser greys the control, and the line underneath says
     why rather than leaving the reader to guess it is broken. */
  .audio-page .ident input:disabled, .audio-page .ident button:disabled { opacity: 0.5; }
  /* The air readout is a claim about the wire, so it is mono like every other
     readout on the page; it turns warn only when the two names differ. */
  .audio-page #air-note { margin-top: 0; font-size: var(--fs-meta); }
  .audio-page #air-note.trimmed { color: var(--warn); }
  .audio-page #air-note b { color: var(--text); }

  .audio-page .stages { list-style: none; padding: 0; margin: 0.2rem 0 0; }
  .audio-page .stages li { padding: 0.32rem 0 0.32rem 1.5rem; position: relative;
    color: var(--dim); font-size: var(--fs-body); }
  .audio-page .stages li::before { content: "○"; position: absolute; left: 0.25rem; }
  .audio-page .stages li.done { color: var(--text); }
  .audio-page .stages li.done::before { content: "●"; color: var(--good); }
  .audio-page .stages li.active { color: var(--text); font-weight: 600; }
  .audio-page .stages li.active::before { content: "◐"; color: var(--good); }
  .audio-page .stages.off { opacity: 0.45; }

  /* The sink used to be a single centred column six rows tall: art, meter,
     two readouts, a control row and five word-and-hex buttons, each row
     centred so nothing shared an edge with anything. Two columns instead --
     the device on the left, everything you can do to it on the right -- which
     is also how the thing reads: an object, and its controls. */
  /* ...and the two columns sit in a well of their own -- the same inset the
     site already uses for a textarea or a <pre> inside a panel -- because the
     device and its controls are one object, and without a boundary they were
     just four loose rows floating between a script editor and a paragraph.
     Stretch plus space-between below gives the two columns a shared top and
     bottom line, which centring never did. */
  .audio-page .sink-face { display: flex; align-items: stretch; gap: 1rem;
    flex-wrap: wrap; margin: 0.2rem 0 0;
    background: var(--bg); border: 1px solid var(--border); border-radius: 8px;
    padding: 0.85rem 0.95rem; }
  .audio-page .sink-visual { display: flex; flex-direction: column; align-items: center;
    justify-content: space-between; gap: 0.55rem; flex: 0 0 auto; width: 7rem; }
  /* The viewBox is cropped to the ink -- the drawing is laid out so the
     cabinet's left margin and the outermost arc's right margin match -- so
     this width is the width of the speaker, and the meter below it can share
     an edge with something instead of floating under empty box. */
  .audio-page #speakerSvg { width: 7rem; height: auto; display: block; }
  .audio-page #cone { transition: fill 0.15s; }
  .audio-page .waves path { transition: opacity 0.15s; }
  /* The unlit fan, drawn once and never touched: without it the whole right
     half of the artwork was empty at low volume, and there was no way to see
     that the lit arcs were four steps of a scale rather than decoration. It is
     the same idea as the meter's dark bars, in the same token. */
  .audio-page .waves-off path { stroke: var(--border); }
  .audio-page .sink-controls { flex: 1 1 15rem; min-width: 0;
    display: flex; flex-direction: column; justify-content: space-between; gap: 0.5rem; }

  /* 16 bars stretched to the artwork's width, so the meter reads as the
     speaker's own level rather than as a stray widget beside it -- and set
     into a recessed track, so the unlit end of the scale is visibly part of
     the instrument rather than absent. */
  .audio-page .meter { display: flex; gap: 2px; height: 1.55rem; align-items: flex-end;
    width: 100%; padding: 3px; background: var(--panel2);
    border: 1px solid var(--border); border-radius: 5px; }
  .audio-page .meter i { flex: 1 1 0; min-width: 0; background: var(--border);
    border-radius: 1.5px; display: block; transition: background 0.1s; }
  .audio-page .meter i.on { background: var(--good); }
  /* No muted-bar colour: a muted speaker emits nothing, so the meter reads
     empty rather than red-at-some-level. (No backticks in this block -- one
     ends the template literal and takes the whole module down.) */

  .audio-page .vol-row { display: flex; align-items: center; gap: 0.55rem; }
  /* Chrome paints a range track in the same accent blue as .primary, so the
     slider and the power button read as one control group. They are unrelated
     -- one is a GATT write, the other a browser permission -- so blue stays
     reserved for "the thing to click first". Green rather than grey, though:
     the slider and the meter beside it are the same quantity, and a dead grey
     track made the one live control in the panel look disabled. */
  .audio-page input[type=range] { flex: 1 1 auto; min-width: 6rem; margin: 0;
    accent-color: var(--good); }
  /* Tabular figures so the slider does not twitch as the number changes width. */
  .audio-page .vol-num { font-family: ui-monospace, Menlo, monospace;
    font-size: var(--fs-label); color: var(--dim); font-variant-numeric: tabular-nums;
    min-width: 2.2ch; text-align: right; }

  /* Icon buttons: square, so five of them make an even strip rather than five
     different widths. */
  .audio-page .icon-btn { display: inline-flex; align-items: center; justify-content: center;
    width: 2rem; height: 2rem; padding: 0; flex: 0 0 auto; color: var(--text); }
  .audio-page .icon-btn svg { display: block; }
  .audio-page .icon-btn:hover:not(:disabled) { border-color: var(--accent); color: var(--accent); }
  .audio-page .icon-btn.gate { border-radius: 50%; }
  .audio-page .icon-btn.gate.primary { color: #fff; }
  .audio-page .icon-btn.gate.on { color: var(--good); border-color: var(--good); opacity: 1; }
  /* One segmented strip, not five loose tiles. These five buttons are one
     family -- every one of them is a byte written to the same control point --
     and five separate boxes with five separate gaps said the opposite. Sharing
     borders also removes five stray vertical hairlines from a small panel.
     One wider gap still splits the two volume steps from the three mute
     states; that gap is now the only division in the strip, so it carries. */
  .audio-page .ops { display: flex; }
  .audio-page .ops .icon-btn { border-radius: 0; margin-left: -1px; }
  .audio-page .ops .icon-btn:first-child { border-radius: 6px 0 0 6px; margin-left: 0; }
  /* The one button that starts the second segment: rounded on its left, and
     the only gap in the strip. Both live in one rule because a separate,
     less specific .grouped rule lost its margin to the reset above. */
  .audio-page .ops .icon-btn.grouped { border-radius: 6px 0 0 6px; margin-left: 0.5rem; }
  .audio-page .ops .icon-btn:last-child,
  .audio-page .ops .icon-btn:has(+ .grouped) { border-radius: 0 6px 6px 0; }
  /* A hovered button's coloured border has to sit above its neighbours' or it
     is clipped along the shared seam. */
  .audio-page .ops .icon-btn:hover:not(:disabled) { position: relative; z-index: 1; }
  /* Scoped to the sink: the source column's readouts keep their own spacing. */
  .audio-page .sink-controls .readout { margin-top: 0; }
  /* Two mono lines that looked identical said the wrong thing: the first is
     the characteristic read back off the device, the second is this page's own
     plumbing. A rule sets the state line off from the controls that change it;
     the diagnostics drop a size. */
  .audio-page #readout { padding-top: 0.5rem; border-top: 1px solid var(--border); }
  .audio-page #sink-stats { font-size: var(--fs-meta); }
  .audio-page .warn-line { color: var(--warn); font-size: var(--fs-label); margin-top: 0.5rem; }
  `;
  document.head.appendChild(style);
}

const ABOUT = `<p>Both halves of an LE Audio stream, on one page. Pick an audio file: it is decoded,
   resampled to 16&nbsp;kHz, encoded to LC3 and streamed to the sink over a <strong>real
   CIS</strong> — <code>LE Set CIG Parameters</code> → <code>LE Create CIS</code> →
   <code>LE Setup ISO Data Path</code> — and the speaker plays it at whatever volume its
   Volume Control Service currently holds.</p>`;

const TEMPLATE = `
<div class="audio-page domain two-up">

  <section class="panel">
      <div id="source-head"></div>

      <div id="usb-source-note" hidden>
        <p class="hint">
          <strong>Anything can be the source.</strong> This card puts one on a second
          dongle: it inquires, pairs, reads the speaker's SDP, negotiates SBC and streams a
          file over real radio — to this page's own speaker or any real one in pairing
          mode. Or skip it and stream from a phone, a laptop, anything that speaks A2DP,
          at the speaker named on the right.
        </p>
        <label class="field" for="usb-src-device">Source dongle</label>
        <div class="row">
          <select id="usb-src-device" aria-label="which dongle hosts the source"></select>
        </div>
        <label class="field" for="usb-src-target">Speaker address</label>
        <input type="text" id="usb-src-target" spellcheck="false"
               placeholder="empty = first speaker the inquiry finds"
               aria-label="the target speaker's address, or empty to inquire">
        <label id="usb-src-drop" class="field" for="usb-src-file"
               style="margin-top:0.5rem">Audio file (else a test melody)</label>
        <input type="file" id="usb-src-file" accept="audio/*">
        <div class="readout" id="usb-src-stage">not started</div>
        <pre id="usb-src-log" class="readout"
             style="max-height:12rem;overflow-y:auto;white-space:pre-wrap"></pre>
      </div>

      <div id="le-source">
      <label id="drop" class="drop" for="file">
        ${WAVEFORM}
        <span class="drop-title">Drop an audio file here</span>
        <span class="drop-sub">or <b>click to choose one</b></span>
        <span class="drop-formats">mp3 · m4a · wav · flac — whatever your browser can decode</span>
        <input type="file" id="file" accept="audio/*">
      </label>

      <label class="field" for="sink-pick">Stream to</label>
      <div class="row">
        <select id="sink-pick"></select>
        <button id="rescan" title="scan netsim for LE Audio sinks">⟳ rescan</button>
      </div>
      <input type="text" id="sink-addr" hidden placeholder="CC:1E:57:00:00:06"
             aria-label="another sink address to stream to">
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
      </div>
    </section>

  <section class="panel">
      <div id="sink-head"></div>

      <!-- What the sink calls itself and where it sits, above the script that
           defines it, because the name field IS a shortcut into that script's
           first line and the reader should see the two next to each other. -->
      <div class="ident">
        <label class="ident-label" for="ident-name">Name</label>
        <input type="text" id="ident-name" maxlength="60" spellcheck="false"
               aria-label="the name the sink advertises">
        <button id="ident-apply">apply</button>
      </div>
      <div class="readout" id="air-note"></div>
      <div class="warn-line" id="ident-locked" hidden>
        ⚠ The sink's identity is fixed while a stream is running: changing either
        field rebuilds the device, which would drop the CIS underneath it.
      </div>

      <div id="usb-card" hidden>
        <p class="hint">
          This speaker is running on a <strong>physical dongle</strong> through the
          <code>simble --usb</code> bridge named in the controller bar: a Classic
          <strong>A2DP sink</strong> on real radio. Whatever pairs with it and streams —
          your phone — is the source, and the decoded PCM plays here.
        </p>
        <label class="field" for="usb-device">Dongle</label>
        <div class="row">
          <select id="usb-device" aria-label="which dongle hosts the speaker"></select>
        </div>
        <div class="readout" id="usb-stage">not connected</div>
        <pre id="usb-log" class="readout" style="max-height:14rem;overflow-y:auto;white-space:pre-wrap"></pre>
        <p class="hint">
          The sink appears to the phone under the <strong>Name</strong> above, Class of
          Device Loudspeaker. Its link keys live in this tab: reconnecting after a reload
          means <em>Forget</em> the device on the phone first, then pair again.
        </p>
      </div>

      <div id="sink-script"></div>
      <div class="sink-face">
        <div class="sink-visual">
          <!-- A cabinet, not a rounded rectangle with two holes in it. The
               previous drawing was one flat mid-grey slab, a black disc that
               read as a hole rather than a tweeter, and four arcs struck from
               four different origins with four different curvatures -- the
               first of which was a zero-length path that the round line cap
               turned into a stray blob. Redrawn against the Car page's phone
               and head unit so the three mocks are recognisably the same
               workshop: two-tone dark body, an inset baffle, flat fills, no
               gradients, and exactly one part of the object that is alive.
               The four arcs are now genuinely concentric on the cabinet's
               right edge at its vertical centre, radii 12/22/32/42 at a
               constant 52 degrees of half-sweep, with the stroke thinning
               outward the way the energy does. -->
          <svg id="speakerSvg" viewBox="0 0 108 100" role="img"
               aria-label="the sink, drawn as a speaker">
            <rect x="4" y="4" width="58" height="92" rx="9" fill="#2b3138"/>
            <rect x="8" y="8" width="50" height="84" rx="5" fill="#161b21"/>
            <!-- Both drivers are built the same way -- a basket ring with
                 something seated in it -- because that repetition is most of
                 what makes a shape read as a loudspeaker rather than as two
                 circles. The tweeter was a single near-black disc before, and
                 against a near-black baffle it read as a hole. -->
            <circle cx="33" cy="28" r="9" fill="#3d444d"/>
            <circle cx="33" cy="28" r="6.5" fill="#2b3138"/>
            <circle cx="33" cy="65" r="19" fill="#3d444d"/>
            <circle id="cone" cx="33" cy="65" r="15" fill="${ART_LIVE}"/>
            <circle cx="33" cy="65" r="6" fill="#161b21"/>
            <g class="waves-off" fill="none" stroke-linecap="round">
              <path d="M69.39 40.54A12 12 0 0 1 69.39 59.46" stroke-width="3.2"/>
              <path d="M75.55 32.66A22 22 0 0 1 75.55 67.34" stroke-width="3"/>
              <path d="M81.70 24.78A32 32 0 0 1 81.70 75.22" stroke-width="2.8"/>
              <path d="M87.86 16.90A42 42 0 0 1 87.86 83.10" stroke-width="2.6"/>
            </g>
            <g class="waves" stroke="${ART_LIVE}" fill="none" stroke-linecap="round">
              <path id="w1" d="M69.39 40.54A12 12 0 0 1 69.39 59.46" stroke-width="3.2" opacity="0"/>
              <path id="w2" d="M75.55 32.66A22 22 0 0 1 75.55 67.34" stroke-width="3" opacity="0"/>
              <path id="w3" d="M81.70 24.78A32 32 0 0 1 81.70 75.22" stroke-width="2.8" opacity="0"/>
              <path id="w4" d="M87.86 16.90A42 42 0 0 1 87.86 83.10" stroke-width="2.6" opacity="0"/>
            </g>
            <!-- Centred on the fan it cancels, so mute reads as the whole
                 right half being struck out rather than as a mark beside it. -->
            <g id="muteMark" opacity="0" stroke="${ART_MUTED}" stroke-width="6"
               stroke-linecap="round">
              <path d="M69 35L99 65"/><path d="M99 35L69 65"/>
            </g>
          </svg>
          <div class="meter" id="meter"></div>
        </div>

        <div class="sink-controls">
          <div class="vol-row">
            <button id="sound" class="icon-btn gate primary"
              title="Enable sound — a browser only starts an AudioContext from a real click"
              aria-label="Enable sound">${ICON.power}</button>
            <input type="range" id="vol" min="0" max="255" value="128" aria-label="Volume">
            <span class="vol-num" id="vol-num">128</span>
          </div>
          <div class="ops">${OPS.map(opButton).join("")}</div>
          <div class="readout" id="readout">Volume State <b>128</b> · <b>unmuted</b> · ctr <b>0</b></div>
          <div class="readout" id="sink-stats">0 SDUs received · audio off</div>
        </div>
      </div>

      <p class="hint" style="margin-top:0.9rem">
        The round <strong>power button must be a real click</strong>. A browser only lets a user
        gesture create a running <code>AudioContext</code>; one made from script starts
        <em>suspended</em>, and then SDUs are still counted and still scheduled while nothing is
        heard — which looks exactly like a broken audio path. The line above reports the
        context's own state so the two can be told apart.
      </p>
      <p class="hint" style="margin-top:0.5rem">
        The five square buttons are the <strong>control-point idiom</strong>, the pattern behind
        most settable BLE devices — hover one to see the opcode it writes. Nothing here sets the
        volume directly: every control writes an opcode to the write-only
        <code>Volume Control Point</code> (<code>2B7E</code>), the sink's own script applies it,
        updates <code>Volume State</code> (<code>2B7D</code>) and bumps the change counter —
        exactly what a phone does over LE Audio's Volume Control Service. The output gain follows
        the characteristic, so <em>what you hear is the GATT value</em>, and a connected central
        writing the same opcodes moves it too.
      </p>
      <p class="hint" id="mode-hint" style="margin-top:0.5rem"></p>
      <div id="script-error" class="error"></div>
      <dl class="kv">
        <dt>connection</dt><dd id="dev-conn">—</dd>
        <dt>subscription</dt><dd id="dev-sub">—</dd>
      </dl>
      <div id="gatt"></div>
    </section>

  <section class="panel full">
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
</div>
`;

// --- mount / unmount -------------------------------------------------------

/// Builds the whole page into `root` and starts it. Safe to call again: an
/// existing mount is torn down first.
export function mount(container) {
  unmount();
  injectStyles();
  root = container;
  root.innerHTML = TEMPLATE;
  (root.querySelector(".domain") ?? root).prepend(createAboutBox(ABOUT));
  const gen = ++generation;

  slider = $("vol");
  sinkAddr = DEFAULT_SINK_ADDR;
  target = sinkAddr;
  buildHeaders();
  editor = sinkHead.textarea;
  // The identity fields describe the device that is about to be built, so they
  // are filled from the same two places it is built from: the script for the
  // name, the socket URL for the address.
  sinkName = scriptName(editor.value);
  $("ident-name").value = sinkName;
  renderAirNote();
  applyIdentityLock();
  prevValues = new Map();
  discovered = new Map();
  frames = [];
  cursor = 0;
  playing = false;
  startedAt = 0;
  lastCounter = -1;
  sinkOpenedOnce = false;
  lastRenderAt = 0;
  soundOn = false;

  // A gentle ramp, 55% to 100%, not the old 20% to 95%. The steep version put
  // two variables on one quantity -- the bars got taller *and* they lit up --
  // and at low volume the lit end of the scale was a row of four-pixel stubs,
  // so the bottom third of the range was unreadable. The slope now only says
  // "left is quieter"; how far it is lit says the rest.
  const meter = $("meter");
  for (let i = 0; i < 16; i++) {
    const bar = document.createElement("i");
    bar.style.height = `${55 + i * 3}%`;
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
    mode = currentController();
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
    // The header's run control is the device's one start/stop, whichever
    // radio it is on: the LE central over netsim, or the A2DP source on a
    // dongle. A second connect button beside it was the same verb twice.
    run: {
      running: true,
      onRun: () => (mode === "usb" ? startUsbSource() : startSource()),
      onStop: () => (mode === "usb" ? stopUsbSource() : stopSource()),
    },
  });
  $("source-head").append(sourceHead.el);

  sinkHead = createDeviceHeader({
    name: "Audio Sink",
    kind: "peripheral",
    accent: "good",
    address: sinkAddr,
    onAddressEdit: onSinkAddressEdit,
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
    run: {
      running: true,
      onRun: () => (mode === "usb" ? buildUsb() : startSink()),
      onStop: () => (mode === "usb" ? disconnectUsb() : stopSink()),
    },
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
    // The script is the name's home, so editing it through the pen must move
    // the field and the picker with it rather than leaving them describing the
    // device that used to be here.
    syncSinkName();
    sinkHead.setApplyState("device rebuilt from script");
    setTimeout(() => {
      if (root) sinkHead.setApplyState("");
    }, 2500);
  } catch (e) {
    showScriptError(e);
  }
}

// --- the sink's identity ---------------------------------------------------
// Two fields, one Apply, and a line that says what actually leaves the device.
//
// What is deliberately NOT here: the codec. The header comment above warns
// that what the sink advertises and what its ASE is configured with have to
// agree or the sink decodes noise, and that agreement is the 16_2 numbers —
// PCM_RATE, SDU_INTERVAL_MS, LC3_FRAME_BYTES, the PAC record and the CIS's
// Max_SDU, five values that are one decision. A control for any one of them
// would be a control for breaking the other four silently, which is the exact
// failure the warning describes. The name and the address are not in that
// knot: nothing decodes them.

/// Re-reads the name out of the live script and puts every display of it back
/// in step — the field, the on-air line, and the source's chooser.
function syncSinkName() {
  sinkName = scriptName(editor.value);
  if (document.activeElement !== $("ident-name")) $("ident-name").value = sinkName;
  renderAirNote();
  renderSinkOptions();
}

/// What the name in the field will actually go out as. The budget is stated in
/// both cases, not only when it is exceeded: a reader watching the count climb
/// knows why the next name gets cut, which a message that only ever appears
/// after the damage never tells them.
function renderAirNote() {
  const typed = $("ident-name").value;
  const air = onAir(typed);
  const note = $("air-note");
  note.classList.toggle("trimmed", air !== typed);
  note.innerHTML =
    air === typed
      ? `advertised as <b>${escapeHtml(air || "(no name)")}</b> · ` +
        `${utf8.encode(air).length}/${NAME_BUDGET} bytes`
      : `advertised as <b>${escapeHtml(air)}</b> — trimmed to the ${NAME_BUDGET} bytes ` +
        `this advertisement leaves for a name. The scan response still carries it whole.`;
}

/// Moves the sink to a new address: a new socket over netsim, a new link
/// in-page. The source is aimed at one peer for its lifetime, so a source that
/// was pointed at the old address is re-aimed rather than left calling an
/// address nothing answers to.
function applySinkAddress(next) {
  const wasTargeted = target === sinkAddr;
  sinkAddr = next;
  sinkHead.setAddress(sinkAddr);
  if (mode === "in-page") {
    // One link hosts both devices, so rebuilding moves them together.
    try {
      buildInPage();
    } catch (e) {
      showScriptError(e);
    }
  } else {
    try {
      sink?.free();
    } catch (_) {
      /* already gone */
    }
    sink = null;
    sinkOpenedOnce = false;
    lastSinkAttempt = performance.now() - RECONNECT_MS; // stand it back up next tick
  }
  discovered.delete(sinkAddr); // it is the built-in sink now, not a stranger
  if (wasTargeted) setTarget(sinkAddr);
  renderSinkOptions();
}

/// Commits whichever of the two fields changed. Nothing is applied while a
/// stream is running — both edits rebuild the device and would drop the CIS —
/// and the controls are disabled to say so, with this as the backstop.
function applyIdentity() {
  if (playing) return;
  showScriptError(null);
  showError("");
  if (mode === "usb") {
    // The name is the only identity a page can give a dongle-hosted
    // speaker; applying it rebuilds the sink under the new name.
    buildUsb();
    return;
  }

  const name = $("ident-name").value;
  if (name !== sinkName) {
    if (!nameFitsScript(name)) {
      showScriptError('a name cannot contain " or \\ — it goes into a string literal in the script');
      return;
    }
    const next = withScriptName(editor.value, name);
    if (next === null) {
      showScriptError("this script builds no BluetoothGattServer(\"…\"), so it has no name to set");
      return;
    }
    editor.value = next;
    editor.dispatchEvent(new Event("input", { bubbles: true })); // repaint the highlighter
    applySinkScript(); // syncs sinkName and re-renders the chooser
  }
}

/// The header's address edit lands here: one address, one place. Returns a
/// refusal string (the header reverts and shows it) or null to accept.
function onSinkAddressEdit(address) {
  if (mode === "usb") return "a dongle's address belongs to its silicon";
  if (playing) return "the identity is fixed while a stream is running";
  if (!/^([0-9A-F]{2}:){5}[0-9A-F]{2}$/.test(address)) {
    return `"${address}" is not a Bluetooth address (AA:BB:CC:DD:EE:FF)`;
  }
  if (address === target && address !== sinkAddr) {
    // The source is already aimed there, so handing it to the sink would
    // point the source at itself by a different route.
    return `${address} is what the source is streaming to — pick another`;
  }
  if (address !== sinkAddr) applySinkAddress(address);
  return null;
}

/// The identity is fixed for as long as a stream is up. A field that silently
/// did nothing, or one that took the stream down without warning, are both
/// worse than a disabled control with the reason beside it.
function applyIdentityLock() {
  for (const id of ["ident-name", "ident-apply"]) $(id).disabled = playing;
  $("ident-locked").hidden = !playing;
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

  // The on-air line follows the field as it is typed, so the budget is visible
  // before the name is committed rather than as a surprise afterwards.
  $("ident-name").addEventListener("input", renderAirNote);
  $("ident-apply").addEventListener("click", applyIdentity);
  $("ident-name").addEventListener("keydown", (event) => {
    if (event.key === "Enter") applyIdentity();
  });

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
    soundOn = true;
    usbPlayer?.enable();
    const gate = event.currentTarget;
    gate.innerHTML = ICON.check;
    gate.classList.replace("primary", "on");
    gate.title = "Sound is on — the AudioContext is running";
    gate.setAttribute("aria-label", "Sound is on");
    gate.disabled = true;
  });

  // The slider sends Set Absolute Volume, the same command a phone's volume UI
  // sends — it does not poke the state characteristic.
  slider.addEventListener("input", () => {
    if (mode === "usb") {
      setUsbVolume(Number(slider.value), usbMuted);
      return;
    }
    setAbsolute(Number(slider.value));
  });
  for (const button of root.querySelectorAll(".ops button")) {
    button.addEventListener("click", () => {
      const op = Number(button.dataset.op);
      if (mode === "usb") return usbOp(op);
      sendOp(op);
    });
  }
}
