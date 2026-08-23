// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// SimBLE Broadcast: Auracast — one source, many listeners, no connection.
//
// Every other device page on this site is a link between two devices: they
// connect, they discover, one of them subscribes. This page has no link in it
// at all. The source never learns that anyone is listening, no receiver ever
// sends it a packet, and nothing here is paired. What a receiver knows, it
// knows because it was *published*:
//
//   * an extended advertisement carrying the Broadcast Audio Announcement
//     (Service Data 0x1852 with the 24-bit Broadcast_ID) and a Broadcast Name,
//   * a periodic advertising train carrying the BASE (Service Data 0x1851):
//     presentation delay, codec configuration, and one Audio Location per BIS,
//   * BIGInfo, which the controller puts in that train's ACAD and reports as
//     an LE BIGInfo Advertising Report.
//
// So the page is built around that round trip: the BASE the source publishes
// sits beside the BASE each receiver parsed back off the air, octet for octet.
// Nothing on this page is drawn from a variable the UI kept on the side — every
// field comes out of `WebBigBroadcaster.status_json()` or one receiver's
// `WebBigReceiver.status_json()`, which are serialized by the same Rust code.
//
// netsim only. rootcanal implements periodic advertising and a BIG; the in-page
// `WebLink` models neither, so there is no honest in-browser path and the
// controller bar says so rather than offering a fake one.
//
// Two receivers by default because one receiver is a point-to-point demo with
// extra steps. Adding a third or a fourth changes nothing at the source, which
// is the whole property this page exists to show.

import init, { WebBigBroadcaster, WebBigReceiver, WebLc3 } from "../pkg/simble.js";
import { createSduPlayer } from "../common/lc3-player.js";
import { createDeviceHeader } from "../common/device-header.js";
import { createAboutBox } from "../common/about-box.js";
import { escapeHtml } from "../common/viewer-format.js";

/// Which controllers this domain can run on. The shell's controller bar
/// reads this: an option mapped to a string is offered disabled, with that
/// string as the reason, rather than hidden.
export const SUPPORTS = {
  "in-page": "broadcast needs periodic advertising and a BIG; the in-page radio models neither",
  websocket: true,
};

// Each device is its own netsim node, which is to say its own controller —
// exactly as a phone and three pairs of earbuds would be.
const SOURCE_ADDR = "CC:1E:57:00:0B:20";
const RECEIVER_ADDRS = [
  "CC:1E:57:00:0B:21",
  "CC:1E:57:00:0B:22",
  "CC:1E:57:00:0B:23",
  "CC:1E:57:00:0B:24",
];
const MAX_RECEIVERS = RECEIVER_ADDRS.length;
const ws = (node, address) =>
  `ws://localhost:7681/v1/websocket/bt?name=${encodeURIComponent(node)}&address=${address}`;

// The broadcast's identity. The Broadcast_ID is 24 bits and is what a receiver
// filters on, so a memorable one keeps this page's devices out of anyone else's
// broadcast on the same netsim.
const DEFAULT_BROADCAST_ID = 0xc0ffee;
const DEFAULT_NAME = "SimBLE Auracast";

// LE Audio's 48_2 broadcast configuration — the one Bumble's `auracast
// transmit` uses, and the one `BroadcastConfig::default()` publishes, so a
// foreign receiver needs no arguments to decode this source. 100 octets per
// frame is also the BIG's Max_SDU.
const PCM_RATE = 48000;
const SDU_INTERVAL_MS = 10;
const SAMPLES_PER_SDU = (PCM_RATE * SDU_INTERVAL_MS) / 1000;
const LC3_FRAME_BYTES = 100;

// One note per BIS, so a listener can tell by ear which stream a receiver
// joined: A4 on the left, C#5 on the right — a major third, which is also what
// the interop run against Bumble puts on the air.
const TONES = [440, 554.37, 659.25, 880];

// netsim does not synthesize a disconnect when a client's WebSocket drops: the
// device entry lingers, and a socket that re-registers under the same name a
// moment later can attach to the stale one. Every teardown hands reconnection
// back to this backoff rather than reopening immediately.
const RECONNECT_MS = 3000;

// A hidden tab's timers run at about 1 Hz against the 20 ms this page asks for,
// so it wakes owing a hundred frames per BIS. Encoding them all in one wake
// costs more than the wake is worth; the cap keeps the page responsive and the
// count of what it skipped is shown rather than hidden.
const MAX_FRAMES_PER_TICK = 25;

// --- state -----------------------------------------------------------------
// All of it belongs to one mount and is cleared by unmount().

let root = null;
let generation = 0;
let timer = 0;

let source = null; // WebBigBroadcaster
let sourceHead = null;
let sourceStatus = null; // the last status_json() parsed
let sourceStopped = false;
let lastSourceAttempt = 0;
let encoders = []; // one WebLc3 per BIS: LC3 carries state between frames
let receivers = []; // see makeReceiver()

// The broadcast's configuration, as the controls hold it. Applying it rebuilds
// the source — a BASE is published once, at LE Set Periodic Advertising Data.
let config = { name: DEFAULT_NAME, broadcastId: DEFAULT_BROADCAST_ID, numBis: 2, code: "" };

let streaming = false; // is the page feeding the source audio?
let startedAt = 0;
let framesSent = 0;
let framesSkipped = 0;
let sampleIndex = 0;
let lastRenderAt = 0;

const $ = (id) => root.querySelector(`#${id}`);
const showError = (m) => ($("error").textContent = m ? String(m) : "");

// --- receivers -------------------------------------------------------------

function makeReceiver(slot) {
  return {
    slot,
    address: RECEIVER_ADDRS[slot],
    label: `Receiver ${slot + 1}`,
    device: null,
    status: null,
    // The BIS this receiver renders. A receiver joins the whole BIG — every
    // BIS in the subgroup — and then chooses what to play, which is exactly
    // what an earbud does with the left channel.
    bis: (slot % 2) + 1,
    code: "",
    player: null,
    lc3: null,
    head: null,
    el: null,
    stopped: false,
    lastAttempt: 0,
    // True between losing a broadcast and finding it again, so the header does
    // not fall back to "offline" while it is looking.
    reacquiring: false,
    meterLevel: 0,
  };
}

function createReceiverDevice(receiver) {
  try {
    receiver.device = new WebBigReceiver(
      ws(`web-auracast-rx${receiver.slot + 1}`, receiver.address),
      config.broadcastId,
      receiver.code || undefined,
    );
    receiver.status = null;
  } catch (e) {
    receiver.device = null;
    showError(e);
  }
}

function dropReceiverDevice(receiver, now) {
  try {
    receiver.device?.free();
  } catch (_) {
    /* already gone */
  }
  receiver.device = null;
  receiver.status = null;
  receiver.lastAttempt = now;
}

// A receiver's decoder belongs to one BIS: LC3 keeps state between frames, so
// switching stream means starting a new decoder rather than feeding the old one
// somebody else's audio.
function resetDecoder(receiver) {
  try {
    receiver.lc3?.free();
  } catch (_) {
    /* already gone */
  }
  receiver.lc3 = new WebLc3(PCM_RATE, SDU_INTERVAL_MS * 1000);
  receiver.player?.reset();
}

// --- the source ------------------------------------------------------------

function createSource() {
  try {
    source = new WebBigBroadcaster(
      ws("web-auracast-src", SOURCE_ADDR),
      config.broadcastId,
      config.name,
      config.numBis,
      config.code || undefined,
    );
    sourceStatus = null;
    showError("");
  } catch (e) {
    source = null;
    showError(e);
  }
}

function dropSource(now) {
  try {
    source?.free();
  } catch (_) {
    /* already gone */
  }
  source = null;
  sourceStatus = null;
  lastSourceAttempt = now;
}

function buildEncoders() {
  for (const encoder of encoders) {
    try {
      encoder.free();
    } catch (_) {
      /* already gone */
    }
  }
  encoders = [];
  for (let i = 0; i < config.numBis; i++) encoders.push(new WebLc3(PCM_RATE, SDU_INTERVAL_MS * 1000));
  sampleIndex = 0;
}

/// One 10 ms frame of a tone. The envelope makes it a repeating pluck rather
/// than a continuous sine: a steady tone is indistinguishable from a stuck
/// buffer, and this page's whole claim is that audio is moving.
function tone(hz, out) {
  for (let i = 0; i < out.length; i++) {
    const t = (sampleIndex + i) / PCM_RATE;
    const envelope = 0.4 * (1 - ((t * 2) % 1));
    out[i] = Math.max(-32768, Math.min(32767, Math.round(Math.sin(t * hz * Math.PI * 2) * envelope * 32767)));
  }
  return out;
}

const pcm = new Int16Array(SAMPLES_PER_SDU);

/// Feeds the BIG against a wall clock rather than a fixed count per tick, so
/// scheduling jitter does not accumulate into drift. The clock starts on the
/// first frame that is actually accepted: starting it when the user pressed
/// play would count the whole advertising handshake as playback time.
function pumpAudio(now) {
  if (!streaming || !source || !source.is_streaming()) return;
  if (!startedAt) {
    startedAt = now;
    framesSent = 0;
  }
  const due = Math.floor((now - startedAt) / SDU_INTERVAL_MS) + 1;
  let budget = MAX_FRAMES_PER_TICK;
  while (framesSent + framesSkipped < due && budget-- > 0) {
    for (let bis = 1; bis <= config.numBis; bis++) {
      const payload = encoders[bis - 1].encode(tone(TONES[bis - 1], pcm), LC3_FRAME_BYTES);
      source.send_sdu(bis, payload);
    }
    sampleIndex += SAMPLES_PER_SDU;
    framesSent++;
  }
  // Whatever the clock says is owed and the budget refused is skipped outright
  // rather than played late: every receiver is listening to the same BIG, and
  // audio delivered a second behind is worse for all of them than a gap.
  const owed = due - (framesSent + framesSkipped);
  if (owed > 0) framesSkipped += owed;
  $("hidden-warning").hidden = !document.hidden;
}

// --- rendering -------------------------------------------------------------

const hex4 = (n) => `0x${n.toString(16).toUpperCase().padStart(4, "0")}`;
const hex6 = (n) => `0x${n.toString(16).toUpperCase().padStart(6, "0")}`;
const kv = (label, value) => `<dt>${escapeHtml(label)}</dt><dd>${value}</dd>`;

// The BIS the page is currently able to describe, from whichever BASE we have.
function subgroupOf(status) {
  return status?.base?.subgroups?.[0] ?? null;
}

function renderSource() {
  const status = sourceStatus;
  // A device its own header stopped keeps saying so: without this the next
  // render a tenth of a second later replaced "stopped" with "offline", which
  // is what a device that failed to connect says.
  if (sourceStopped) return;
  if (!status) {
    sourceHead.setState(false, source ? "connecting…" : "offline");
    return;
  }
  const live = Boolean(status.streaming);
  const failed = status.failed !== null && status.failed !== undefined;
  sourceHead.setState(
    live,
    failed
      ? `failed — ${status.failed_name} (0x${status.failed.toString(16).toUpperCase().padStart(2, "0")})`
      : live
        ? `streaming on ${status.bis_handles.length} BIS`
        : status.stage,
    failed ? "bad" : live ? "ok" : "",
  );

  // The setup sequence, which is mostly advertising: a broadcaster publishes
  // everything a receiver needs rather than answering questions about it.
  const order = [
    "advertising set",
    "periodic train",
    "on the air",
    "creating the BIG",
    "opening data paths",
    "streaming",
  ];
  const reached = order.indexOf(status.stage);
  for (const item of root.querySelectorAll("#src-stages li")) {
    const at = order.indexOf(item.dataset.stage);
    item.classList.toggle("done", reached >= 0 && at < reached);
    item.classList.toggle("active", at === reached);
  }

  const c = status.config;
  $("src-facts").innerHTML =
    kv("Broadcast_ID", `<code>${hex6(c.broadcast_id)}</code>`) +
    kv("Broadcast Name", escapeHtml(c.broadcast_name)) +
    kv("advertising SID", String(c.advertising_sid)) +
    kv(
      "BIG",
      status.bis_handles.length
        ? status.bis_handles.map((h, i) => `BIS ${i + 1} → <code>${hex4(h)}</code>`).join(" · ")
        : "not created yet",
    ) +
    kv("SDU", `${c.max_sdu} octets every ${c.sdu_interval_us} µs · RTN ${c.rtn}`) +
    kv("encryption", c.encrypted ? "encrypted — a Broadcast Code is required" : "none");

  $("src-adv").textContent = status.advertising_data;
  $("src-base-hex").textContent = status.base_hex;
  $("src-base").innerHTML = renderBase(status.base);
  $("src-counts").textContent = streaming
    ? `${status.sent} SDUs written per BIS` +
      (status.refused ? ` · ${status.refused} refused before the BIG was up` : "") +
      (framesSkipped ? ` · ${framesSkipped} frames skipped to stay real-time` : "")
    : "not streaming — press play";
}

/// One BASE, rendered the same way whoever is holding it. Both ends serialize
/// through the same Rust, so this renderer never has to know which end it is
/// looking at — which is what makes the comparison below meaningful.
function renderBase(base) {
  if (!base) return `<div class="hint">no BASE yet</div>`;
  const rows = base.subgroups.map((subgroup, index) => {
    const meta = subgroup.metadata.length
      ? subgroup.metadata
          .map((entry) => `${escapeHtml(entry.name)}: <b>${escapeHtml(entry.value)}</b>`)
          .join(" · ")
      : "none";
    const bis = subgroup.bis
      .map(
        (entry) =>
          `<li><b>BIS ${entry.index}</b> — ${escapeHtml(entry.location_name ?? "no Audio Location")}` +
          (entry.audio_location === null || entry.audio_location === undefined
            ? ""
            : ` <span class="dim">(Audio_Channel_Allocation ${hex4(entry.audio_location)})</span>`) +
          `</li>`,
      )
      .join("");
    return (
      `<div class="subgroup">` +
      `<div class="subgroup-head">subgroup ${index} · codec ${escapeHtml(subgroup.codec_name)} ` +
      `<span class="dim">${escapeHtml(subgroup.codec_id)}</span></div>` +
      `<dl class="kv">` +
      kv("sampling frequency", `${subgroup.sampling_frequency_hz} Hz`) +
      kv("frame duration", `${subgroup.frame_duration_us} µs`) +
      kv("octets per frame", String(subgroup.octets_per_codec_frame)) +
      kv("metadata", meta) +
      `</dl>` +
      `<ul class="bis-list">${bis}</ul>` +
      `</div>`
    );
  });
  return (
    `<dl class="kv"><dt>presentation delay</dt><dd>${base.presentation_delay} µs</dd></dl>` +
    rows.join("")
  );
}

function renderReceiver(receiver) {
  const status = receiver.status;
  const head = receiver.head;
  const q = (id) => receiver.el.querySelector(`.${id}`);
  if (receiver.stopped) return; // see renderSource
  if (!status) {
    if (!receiver.reacquiring) {
      head.setState(false, receiver.device ? "connecting…" : "offline");
    }
    return;
  }
  const failed = status.failed !== null && status.failed !== undefined;
  const receiving = Boolean(status.receiving);
  head.setState(
    receiving,
    failed
      ? `${status.stage} — ${status.failed_name} (0x${status.failed.toString(16).toUpperCase().padStart(2, "0")})`
      : status.stage,
    failed ? "bad" : receiving ? "ok" : "",
  );

  const order = ["scanning", "syncing to the train", "reading the announcement", "joining the BIG", "receiving"];
  const reached = order.indexOf(status.stage);
  for (const item of receiver.el.querySelectorAll(".rx-stages li")) {
    const at = order.indexOf(item.dataset.stage);
    item.classList.toggle("done", reached >= 0 && at < reached);
    item.classList.toggle("active", at === reached);
    item.classList.toggle("stopped", failed && at > reached);
  }

  // The refusal is the interesting failure here, so it is spelled out rather
  // than left as a status byte: the receiver read BIGInfo, saw the streams were
  // encrypted, and stopped *before* joining. Nothing was decoded badly — it
  // never asked to be given anything.
  const note = q("rx-note");
  if (failed && status.failed === 0x1d) {
    note.className = "rx-note bad";
    note.innerHTML =
      `<b>Cannot decrypt.</b> BIGInfo says this BIG is encrypted and this receiver holds no ` +
      `Broadcast Code, so it refused to join — <code>Insufficient Security</code> (0x1D), decided ` +
      `from the announcement, before <code>LE BIG Create Sync</code> was ever sent.`;
  } else if (failed) {
    note.className = "rx-note bad";
    note.textContent = `${status.state} — ${status.failed_name}`;
  } else {
    note.className = "rx-note";
    note.textContent = "";
  }

  const found = status.source;
  q("rx-facts").innerHTML =
    kv(
      "source",
      found
        ? `<code>${escapeHtml(found.address)}</code> <span class="dim">${escapeHtml(found.address_type)} · SID ${found.advertising_sid}</span>`
        : "none found yet",
    ) +
    kv("Broadcast_ID", found ? `<code>${hex6(found.broadcast_id)}</code>` : "—") +
    kv(
      "periodic sync",
      status.sync_handle === null || status.sync_handle === undefined
        ? "not synced"
        : `handle <code>${hex4(status.sync_handle)}</code>`,
    ) +
    kv(
      "BIGInfo",
      status.big_info
        ? `${status.big_info.num_bis} BIS · Max_SDU ${status.big_info.max_sdu} · ` +
          `${status.big_info.sdu_interval_us} µs · NSE ${status.big_info.nse} · BN ${status.big_info.bn} · ` +
          `IRC ${status.big_info.irc} · ${status.big_info.encrypted ? "<b>encrypted</b>" : "unencrypted"}`
        : "not received",
    ) +
    kv(
      "BIS handles",
      status.streams.length
        ? status.streams.map((s) => `${s.index} → <code>${hex4(s.handle)}</code>`).join(" · ")
        : "not joined",
    );

  q("rx-base").innerHTML = renderBase(status.base);

  // Which BIS this receiver plays. The options are the BASE's own BIS list, so
  // the choice is the source's published Audio Locations and not a page-side
  // list of channel names.
  const select = q("rx-bis");
  const subgroup = subgroupOf(status);
  const options = (subgroup?.bis ?? []).map(
    (entry) =>
      `<option value="${entry.index}">BIS ${entry.index} — ${escapeHtml(entry.location_name ?? "no location")}</option>`,
  );
  const signature = options.join("");
  if (select.dataset.signature !== signature) {
    select.dataset.signature = signature;
    select.innerHTML = options.length ? options : `<option value="0">no BASE yet</option>`;
    if (options.length) select.value = String(receiver.bis);
  }
  select.disabled = !options.length;

  // Two counts, because a receiver joins a BIG and renders one BIS of it: the
  // stream it is playing, and everything the controller delivered.
  const stream = status.streams.find((s) => s.index === receiver.bis);
  const others = status.streams.filter((s) => s.index !== receiver.bis);
  const otherSdus = others.reduce((total, s) => total + s.sdus, 0);
  q("counts").textContent =
    (stream ? `${stream.sdus} SDUs on BIS ${receiver.bis}` : "no SDUs yet") +
    (others.length
      ? ` · also joined BIS ${others.map((s) => s.index).join(", ")} (${otherSdus} SDUs, not played)`
      : "") +
    (status.dropped ? ` · ${status.dropped} dropped by this page` : "") +
    ` · audio ${receiver.player ? receiver.player.state : "off"}`;
}

// Meter level, held between frames so the bars fall smoothly, and only while
// the peak is fresh: audio arrives in batches, and a bare peak would leave the
// meter frozen at the last batch's level after a stream stops.
const METER_DECAY = 0.55;
const PEAK_FRESH_MS = 400;

function renderMeter(receiver) {
  let peak = 0;
  const player = receiver.player;
  if (player && player.state === "running") {
    const age = performance.now() - (player.stats.peakAt || 0);
    if (age < PEAK_FRESH_MS) peak = player.stats.peak;
  }
  receiver.meterLevel = peak > receiver.meterLevel ? peak : receiver.meterLevel * METER_DECAY;
  // sqrt curve: a linear peak leaves quiet passages invisible.
  const lit = Math.round(Math.sqrt(receiver.meterLevel) * 12);
  const bars = receiver.el.querySelector(".meter").children;
  for (let i = 0; i < bars.length; i++) bars[i].className = i < lit ? "on" : "";
}

// --- the round trip --------------------------------------------------------
// The one thing this page can show that no other page can: the announcement as
// it was published, and the same announcement as each receiver reconstructed it
// from the air. Both sides are serialized by the same Rust, so a difference is
// a difference in what crossed the radio.

function renderRoundTrip() {
  const published = sourceStatus?.base;
  const live = receivers.filter((r) => r.status?.base);
  const table = $("round-trip");
  if (!published) {
    table.innerHTML = `<div class="hint">The source has not published a BASE yet.</div>`;
    return;
  }
  const fields = [];
  const subgroup = (base) => base.subgroups[0];
  fields.push(["presentation delay", (base) => `${base.presentation_delay} µs`]);
  fields.push(["codec", (base) => `${subgroup(base).codec_name} (${subgroup(base).codec_id})`]);
  fields.push(["sampling frequency", (base) => `${subgroup(base).sampling_frequency_hz} Hz`]);
  fields.push(["frame duration", (base) => `${subgroup(base).frame_duration_us} µs`]);
  fields.push(["octets per codec frame", (base) => String(subgroup(base).octets_per_codec_frame)]);
  fields.push([
    "metadata",
    (base) =>
      subgroup(base)
        .metadata.map((m) => `${m.name}: ${m.value}`)
        .join(" · ") || "none",
  ]);
  for (let index = 0; index < subgroup(published).bis.length; index++) {
    fields.push([
      `BIS ${index + 1} Audio Location`,
      (base) => {
        const entry = subgroup(base).bis[index];
        return entry ? (entry.location_name ?? "no location") : "absent";
      },
    ]);
  }

  const header =
    `<tr><th>BASE field</th><th>published by the source</th>` +
    live.map((r) => `<th>${escapeHtml(r.label)} read back</th>`).join("") +
    `</tr>`;
  const rows = fields
    .map(([name, read]) => {
      const mine = read(published);
      const cells = live.map((r) => {
        let theirs;
        try {
          theirs = read(r.status.base);
        } catch (_) {
          theirs = "absent";
        }
        const same = theirs === mine;
        return `<td class="${same ? "same" : "differs"}">${escapeHtml(theirs)}${same ? "" : " ✗"}</td>`;
      });
      return `<tr><th scope="row">${escapeHtml(name)}</th><td>${escapeHtml(mine)}</td>${cells.join("")}</tr>`;
    })
    .join("");

  // And the octets themselves. Comparing the parsed fields proves the two ends
  // agree about meaning; comparing the bytes proves nothing was lost or
  // re-invented on the way — a receiver that re-serialized its parse would be
  // comparing the parser with itself.
  // A receiver that matches gets a line; only a receiver that DIFFERS gets its
  // own dump. Four identical hex blocks in a column is a lot of screen spent
  // hiding the one thing worth seeing.
  const octets = live.map((r) => {
    const identical = r.status.base_hex === sourceStatus.base_hex;
    return (
      `<div class="octet-line ${identical ? "same" : "differs"}">` +
      `<b>${escapeHtml(r.label)}</b> reassembled ${r.status.base_hex.split(" ").length} octets off the ` +
      `periodic train — ${identical ? "byte-identical to what was published" : "DIFFERENT from what was published"}` +
      `</div>` +
      (identical ? "" : `<pre class="wire">${escapeHtml(r.status.base_hex)}</pre>`)
    );
  });

  table.innerHTML =
    `<table class="round-trip">${header}${rows}</table>` +
    (octets.length
      ? `<div class="octets"><div class="octet-line"><b>Published</b> — ` +
        `${sourceStatus.base_hex.split(" ").length} octets of Service Data 0x1851</div>` +
        `<pre class="wire">${escapeHtml(sourceStatus.base_hex)}</pre>${octets.join("")}</div>`
      : `<div class="hint">No receiver has read the BASE back yet.</div>`);
}

// --- the fan ---------------------------------------------------------------
// One transmission, N listeners, drawn from live state. The trunk is lit when
// the BIG is streaming; a branch is lit when that receiver's own stack says it
// has joined that BIG. There is no arrow pointing back, because there is no
// packet that goes back.

function renderFan() {
  const svg = $("fan");
  const rows = receivers.length;
  // 46 px per receiver plus one row's worth of margin, so the drawing is the
  // height of its content whether there are two listeners or four.
  const height = rows * 46 + 26;
  const width = 640;
  svg.setAttribute("viewBox", `0 0 ${width} ${height}`);
  const trunkX = 250;
  const sourceY = height / 2;
  const firstY = (height - (rows - 1) * 46) / 2;
  const live = Boolean(sourceStatus?.streaming);

  const branches = receivers
    .map((receiver, i) => {
      const y = firstY + i * 46;
      const status = receiver.status;
      const joined = Boolean(status?.receiving);
      const paSynced = Boolean(status?.sync_handle !== null && status?.sync_handle !== undefined);
      const failed = status?.failed !== null && status?.failed !== undefined;
      const cls = failed ? "branch bad" : joined ? "branch on" : paSynced ? "branch part" : "branch";
      const stream = status?.streams?.find((s) => s.index === receiver.bis);
      const label = joined
        ? `BIS ${receiver.bis} · ${stream ? stream.sdus.toLocaleString() : 0} SDUs`
        : failed
          ? status.failed_name
          : (status?.stage ?? (receiver.reacquiring ? "looking again" : "offline"));
      return (
        `<path class="${cls}" d="M${trunkX} ${sourceY} C ${trunkX + 60} ${sourceY}, ${trunkX + 60} ${y}, ${trunkX + 130} ${y}"/>` +
        `<rect class="node ${joined ? "on" : failed ? "bad" : ""}" x="${trunkX + 130}" y="${y - 15}" width="240" height="30" rx="7"/>` +
        `<text class="node-label" x="${trunkX + 142}" y="${y + 4}">${escapeHtml(receiver.label)}</text>` +
        `<text class="node-meta" x="${trunkX + 360}" y="${y + 4}" text-anchor="end">${escapeHtml(label)}</text>`
      );
    })
    .join("");

  const c = sourceStatus?.config;
  svg.innerHTML =
    `<rect class="node source ${live ? "on" : ""}" x="8" y="${sourceY - 22}" width="170" height="44" rx="8"/>` +
    `<text class="node-label" x="20" y="${sourceY - 4}">Broadcast source</text>` +
    `<text class="node-meta" x="20" y="${sourceY + 12}" text-anchor="start">${escapeHtml(
      live && c ? `${c.num_bis} BIS · ${hex6(c.broadcast_id)}` : (sourceStatus?.stage ?? "offline"),
    )}</text>` +
    `<path class="trunk ${live ? "on" : ""}" d="M178 ${sourceY} H${trunkX}"/>` +
    `<text class="trunk-label" x="214" y="${sourceY - 10}" text-anchor="middle">one BIG</text>` +
    branches;
}

// --- the loop --------------------------------------------------------------

function tickSource(now) {
  if (sourceStopped) return;
  if (!source) {
    if (now - lastSourceAttempt >= RECONNECT_MS) {
      lastSourceAttempt = now;
      createSource();
    }
    return;
  }
  if (source.ready_state() === 3) {
    dropSource(now);
    sourceHead.setState(false, "connection lost — reconnecting…", "bad");
    $("setup").classList.add("visible");
    return;
  }
  $("setup").classList.remove("visible");
  sourceStatus = JSON.parse(source.tick());
}

function tickReceiver(receiver, now) {
  if (receiver.stopped) return;
  if (!receiver.device) {
    if (now - receiver.lastAttempt >= RECONNECT_MS) {
      receiver.lastAttempt = now;
      createReceiverDevice(receiver);
    }
    return;
  }
  if (receiver.device.ready_state() === 3) {
    dropReceiverDevice(receiver, now);
    receiver.head.setState(false, "connection lost — reconnecting…", "bad");
    return;
  }
  receiver.status = JSON.parse(receiver.device.tick());

  // A broadcast that goes away is not an error: the source terminated its BIG
  // and the controller said so. A listener's answer to that is to look for it
  // again, which is what a pair of earbuds does when the transmitter is
  // switched off and on. Only `lost` is retried — a `failed` receiver made a
  // decision (no Broadcast Code, say) that retrying cannot change, and a page
  // that churned through the same refusal every three seconds would be showing
  // a flicker instead of an answer.
  if (receiver.status.stage === "lost") {
    receiver.head.setState(false, `the broadcast stopped — looking for it again`, "warn");
    receiver.reacquiring = true;
    dropReceiverDevice(receiver, now);
    return;
  }
  receiver.reacquiring = false;
  // Every BIS is drained, not only the one being played: a receiver joined the
  // whole BIG and the controller is delivering all of it. Leaving the other
  // streams to pile up would have the receiver reporting audio it dropped when
  // in fact it simply is not rendering that channel.
  const decoder = receiver.lc3;
  for (const stream of receiver.status.streams) {
    const sdus = receiver.device.take_audio(stream.index);
    if (stream.index === receiver.bis && receiver.player && decoder) {
      receiver.player.play(sdus, (bytes) => decoder.decode(bytes));
    }
  }
}

function loop() {
  const now = performance.now();
  try {
    tickSource(now);
    pumpAudio(now);
    for (const receiver of receivers) tickReceiver(receiver, now);
  } catch (e) {
    showError(e);
  }
  for (const receiver of receivers) renderMeter(receiver);
  // Rendering is a hundredth of the work of pumping, but it is still work no
  // stream benefits from doing 50 times a second.
  if (now - lastRenderAt > 100) {
    lastRenderAt = now;
    renderSource();
    for (const receiver of receivers) renderReceiver(receiver);
    renderRoundTrip();
    renderFan();
  }
}

// --- controls --------------------------------------------------------------

function applyConfig() {
  const id = parseInt($("cfg-id").value.replace(/^0x/i, ""), 16);
  if (Number.isNaN(id) || id < 0 || id > 0xffffff) {
    showError("a Broadcast_ID is 24 bits — six hex digits, 000000 to FFFFFF");
    return;
  }
  const code = $("cfg-code").value;
  if (code.length > 16) {
    showError("a Broadcast Code is at most 16 octets");
    return;
  }
  showError("");
  const codeChanged = code !== config.code;
  config = {
    name: $("cfg-name").value.trim() || DEFAULT_NAME,
    broadcastId: id,
    numBis: Number($("cfg-bis").value),
    code,
  };
  if (codeChanged) shareCode();
  restartAll();
}

/// A BASE is published once, at LE Set Periodic Advertising Data, and a BIG is
/// created once from the same numbers — so changing any of them is a new
/// broadcast, not an update to this one. Every receiver goes with it: they are
/// synced to a train that is about to stop.
function restartAll() {
  const now = performance.now();
  streaming = false;
  startedAt = 0;
  framesSent = 0;
  framesSkipped = 0;
  $("play").textContent = "▶ broadcast";
  dropSource(now - RECONNECT_MS);
  buildEncoders();
  for (const receiver of receivers) {
    dropReceiverDevice(receiver, now - RECONNECT_MS);
    receiver.bis = Math.min(receiver.bis, config.numBis);
    resetDecoder(receiver);
    receiver.player?.reset();
  }
  syncCodeFields();
}

/// A receiver's own Broadcast Code field appears only when there is one to
/// hold, and is filled with the source's — the common case is a listener that
/// was told the code. Clearing one is how the page shows a listener that was
/// not: it is refused at the announcement, before it joins anything.
function syncCodeFields() {
  for (const receiver of receivers) {
    const field = receiver.el.querySelector(".rx-code");
    const label = receiver.el.querySelector(".rx-code-label");
    field.hidden = !config.code;
    label.hidden = !config.code;
    if (field.value !== receiver.code) field.value = receiver.code;
  }
}

/// Hands the source's code to every receiver. Called when the code changes, so
/// that applying one does not silently lock every listener out.
function shareCode() {
  for (const receiver of receivers) receiver.code = config.code;
}

function toggleStream() {
  streaming = !streaming;
  startedAt = 0;
  framesSent = 0;
  framesSkipped = 0;
  $("play").textContent = streaming ? "■ stop" : "▶ broadcast";
  $("hidden-warning").hidden = true;
}

/// The lowest address not currently on the air. Receivers can be removed from
/// the middle, and two devices at one address on netsim is a ghost, not a pair.
function nextFreeSlot() {
  const taken = new Set(receivers.map((r) => r.slot));
  for (let slot = 0; slot < MAX_RECEIVERS; slot++) if (!taken.has(slot)) return slot;
  return -1;
}

function addReceiver() {
  const slot = nextFreeSlot();
  if (slot < 0) return;
  const receiver = makeReceiver(slot);
  // A listener that joins an encrypted broadcast was told the code, unless the
  // page is asked to show one that was not.
  receiver.code = config.code;
  receivers.push(receiver);
  buildReceiverCard(receiver);
  syncCodeFields();
  $("add-receiver").disabled = receivers.length >= MAX_RECEIVERS;
  $("many-note").textContent = manyNote();
}

function removeReceiver(receiver) {
  dropReceiverDevice(receiver, performance.now());
  try {
    receiver.player?.close();
  } catch (_) {
    /* already closed */
  }
  try {
    receiver.lc3?.free();
  } catch (_) {
    /* already gone */
  }
  receiver.head?.destroy();
  receiver.el.remove();
  receivers = receivers.filter((r) => r !== receiver);
  $("add-receiver").disabled = receivers.length >= MAX_RECEIVERS;
  $("many-note").textContent = manyNote();
}

const manyNote = () =>
  `${receivers.length} receiver${receivers.length === 1 ? "" : "s"} on one BIG. ` +
  `Adding or removing one changes nothing at the source: it has no idea they are there.`;

// --- markup ----------------------------------------------------------------

const STYLE_ID = "simble-broadcast-style";

function injectStyles() {
  if (document.getElementById(STYLE_ID)) return;
  const style = document.createElement("style");
  style.id = STYLE_ID;
  style.textContent = `
  /* The fan: one transmission, several listeners. Sized by its viewBox so the
     row count sets the height rather than a scroll bar. */
  .bcast-page #fan { width: 100%; height: auto; display: block; }
  .bcast-page #fan .node { fill: var(--panel2); stroke: var(--border); stroke-width: 1; }
  .bcast-page #fan .node.on { stroke: var(--good); }
  .bcast-page #fan .node.source.on { stroke: var(--accent); }
  .bcast-page #fan .node.bad { stroke: var(--bad); }
  .bcast-page #fan .node-label { fill: var(--text); font-size: 13px; font-weight: 600; }
  .bcast-page #fan .node-meta { fill: var(--dim); font-size: 11px;
    font-family: ui-monospace, Menlo, monospace; }
  .bcast-page #fan .trunk-label { fill: var(--dim); font-size: 11px; }
  /* Unlit is a hairline, lit is the same line with weight and colour: the
     branch is always there -- the source transmits whether anyone listens or
     not -- and what changes is whether that receiver is taking it. */
  .bcast-page #fan .trunk, .bcast-page #fan .branch { fill: none; stroke: var(--border);
    stroke-width: 1.5; }
  .bcast-page #fan .trunk.on { stroke: var(--accent); stroke-width: 4; }
  .bcast-page #fan .branch.part { stroke: var(--warn); stroke-dasharray: 4 3; }
  .bcast-page #fan .branch.on { stroke: var(--good); stroke-width: 3; }
  .bcast-page #fan .branch.bad { stroke: var(--bad); stroke-dasharray: 2 4; }

  /* Stage lists: the same idiom the Media domain uses for its handshake. */
  .bcast-page .stages { list-style: none; padding: 0; margin: 0.2rem 0 0; }
  .bcast-page .stages li { padding: 0.3rem 0 0.3rem 1.5rem; position: relative;
    color: var(--dim); font-size: var(--fs-body); }
  .bcast-page .stages li::before { content: "○"; position: absolute; left: 0.25rem; }
  .bcast-page .stages li.done { color: var(--text); }
  .bcast-page .stages li.done::before { content: "●"; color: var(--good); }
  .bcast-page .stages li.active { color: var(--text); font-weight: 600; }
  .bcast-page .stages li.active::before { content: "◐"; color: var(--good); }
  .bcast-page .stages li.stopped::before { content: "·"; color: var(--bad); }

  /* Wire bytes. A BASE is 47 octets and every one of them is the point, so it
     wraps rather than scrolling out of sight. */
  .bcast-page .wire { font-family: ui-monospace, Menlo, monospace; font-size: var(--fs-code);
    background: var(--bg); border: 1px solid var(--border); border-radius: 6px;
    padding: 0.5rem 0.6rem; margin: 0.3rem 0 0.7rem; white-space: pre-wrap;
    word-break: break-all; color: var(--text); }
  .bcast-page .label { font-size: var(--fs-label); text-transform: uppercase;
    letter-spacing: 0.06em; color: var(--dim); margin-top: 0.8rem; }
  .bcast-page .dim { color: var(--dim); }

  .bcast-page .subgroup { border: 1px solid var(--border); border-radius: 8px;
    padding: 0.5rem 0.7rem; margin-top: 0.5rem; background: var(--bg); }
  .bcast-page .subgroup-head { font-size: var(--fs-label); color: var(--text);
    font-weight: 600; margin-bottom: 0.3rem; }
  .bcast-page .bis-list { list-style: none; padding: 0; margin: 0.4rem 0 0;
    font-size: var(--fs-body); }
  .bcast-page .bis-list li { padding: 0.15rem 0; }

  /* The round trip. Two columns are a comparison; N+1 columns are a comparison
     against every listener at once, which is the point of a broadcast. */
  .bcast-page table.round-trip { border-collapse: collapse; width: 100%;
    font-size: var(--fs-body); }
  .bcast-page table.round-trip th, .bcast-page table.round-trip td {
    border: 1px solid var(--border); padding: 0.35rem 0.55rem; text-align: left;
    vertical-align: top; }
  .bcast-page table.round-trip thead th, .bcast-page table.round-trip tr:first-child th {
    background: var(--panel2); font-size: var(--fs-label); }
  .bcast-page table.round-trip th[scope=row] { color: var(--dim); font-weight: 500;
    background: var(--panel); }
  .bcast-page table.round-trip td.same { color: var(--good); }
  .bcast-page table.round-trip td.differs { color: var(--bad); font-weight: 600; }
  .bcast-page .octets { margin-top: 1rem; }
  .bcast-page .octet-line { font-size: var(--fs-body); color: var(--dim); }
  .bcast-page .octet-line.same b { color: var(--good); }
  .bcast-page .octet-line.differs { color: var(--bad); }

  /* Receivers sit in their own grid so three of them do not make one very tall
     column beside an empty one. */
  .bcast-page .receivers { display: grid; gap: 1.25rem;
    grid-template-columns: repeat(auto-fit, minmax(22rem, 1fr)); }
  /* A gate that has been opened is not a disabled control: it reports a state,
     so it keeps its weight and takes the colour that means "live" everywhere
     else on the site, rather than fading to the greyed-out look of a button
     that cannot be used. */
  .bcast-page .rx-sound.on { color: var(--good); border-color: var(--good); opacity: 1; }
  .bcast-page .rx-note { font-size: var(--fs-body); margin: 0.5rem 0 0; }
  .bcast-page .rx-note.bad { color: var(--bad); }
  .bcast-page .counts { font-family: ui-monospace, Menlo, monospace;
    font-size: var(--fs-meta); color: var(--dim); margin-top: 0.5rem; }

  /* The meter is the one thing here that reports the AUDIO rather than the
     protocol: it is driven from the decoded signal's peak, never from a level
     the page set. */
  .bcast-page .meter { display: flex; gap: 2px; height: 1.2rem; align-items: flex-end;
    flex: 1 1 5rem; max-width: 12rem; padding: 3px; background: var(--panel2);
    border: 1px solid var(--border); border-radius: 5px; }
  .bcast-page .meter i { flex: 1 1 0; min-width: 0; background: var(--border);
    border-radius: 1.5px; display: block; transition: background 0.1s; }
  .bcast-page .meter i.on { background: var(--good); }

  .bcast-page .field { display: block; margin-top: 0.7rem; font-size: var(--fs-body);
    color: var(--dim); }
  /* .field's display beats the UA's [hidden] rule, so a hidden label stayed on
     screen: the Broadcast Code field showed on every receiver of an
     unencrypted broadcast. */
  .bcast-page [hidden] { display: none; }
  .bcast-page input[type=text], .bcast-page select { font-family: ui-monospace, Menlo, monospace;
    font-size: var(--fs-body); padding: 0.3rem 0.45rem; border: 1px solid var(--border);
    border-radius: 6px; background: var(--bg); color: var(--text); }
  .bcast-page .cfg { display: flex; gap: 0.6rem; flex-wrap: wrap; align-items: flex-end; }
  .bcast-page .cfg label { display: flex; flex-direction: column; gap: 0.2rem;
    font-size: var(--fs-meta); text-transform: uppercase; letter-spacing: 0.05em;
    color: var(--dim); }
  .bcast-page .cfg input[type=text] { width: 9rem; }
  .bcast-page .warn-line { color: var(--warn); font-size: var(--fs-label); margin-top: 0.5rem; }
  .bcast-page .two-col { display: grid; gap: 1rem;
    grid-template-columns: repeat(auto-fit, minmax(18rem, 1fr)); }
  `;
  document.head.appendChild(style);
}

const ABOUT = `
  <p><strong>Auracast</strong> — LE Audio's connectionless media plane. One source, any number of
     listeners, <strong>no connection and no pairing</strong>: the source never learns that a
     receiver exists, and a receiver never sends it a packet. Everything a listener needs is
     <em>published</em> — a Broadcast Audio Announcement (<code>0x1852</code>) on an extended
     advertisement, the <strong>BASE</strong> (<code>0x1851</code>) on a periodic advertising
     train, and BIGInfo in that train's ACAD.</p>
  <p>So the page is that round trip: the BASE the source publishes, beside the BASE each receiver
     reconstructed from the air, octet for octet. Both sides are serialized by the same Rust —
     <code>BigBroadcaster</code> and <code>BigReceiver</code>, the pair that also passes interop
     against Bumble's <code>auracast</code> app — so a difference in the table below would be a
     difference that crossed the radio.</p>
  <p>Each receiver plays one BIS. The two carry different notes on purpose, so which stream a
     receiver joined is audible and not just tabulated.</p>`;

const TEMPLATE = `
<div class="bcast-page domain one-up">

  <section class="panel full">
    <h2>One transmission, every listener</h2>
    <svg id="fan" role="img" aria-label="the broadcast, drawn as one trunk fanning out to each receiver"></svg>
    <div class="row">
      <button id="add-receiver">+ add a receiver</button>
      <span class="hint" id="many-note"></span>
    </div>
  </section>

  <section class="panel full">
    <div id="source-head"></div>
    <div class="two-col">
      <div>
        <div class="cfg">
          <label>Broadcast Name<input type="text" id="cfg-name" value="${DEFAULT_NAME}"></label>
          <label>Broadcast_ID<input type="text" id="cfg-id" value="C0FFEE" size="6"></label>
          <label>BIS<select id="cfg-bis">
            <option value="1">1</option><option value="2" selected>2</option>
            <option value="3">3</option><option value="4">4</option>
          </select></label>
          <label>Broadcast Code<input type="text" id="cfg-code" placeholder="none" size="10"></label>
          <button id="apply">apply</button>
        </div>
        <p class="hint" style="margin-top:0.5rem">
          Applying restarts the broadcast: a BASE is published once, at
          <code>LE Set Periodic Advertising Data</code>, and the BIG is created from the same
          numbers — so changing one is a new broadcast, not an edit to this one. A Broadcast Code
          encrypts the BISes; a receiver without it refuses to join at all.
        </p>
        <p class="hint" style="margin-top:0.5rem">
          What that refusal is, exactly: the receiver reads the encryption flag out of
          <strong>BIGInfo</strong> and stops — <code>Insufficient Security</code> (0x1D) — before it
          sends <code>LE BIG Create Sync</code>. It is a host decision taken from the announcement,
          not a failed decode. Note that netsim's rootcanal carries the flag but does not actually
          encrypt the payload, so a receiver holding the <em>wrong</em> code still decodes audio
          here; only the "no code at all" case is the real thing on this controller.
        </p>
        <div class="row">
          <button id="play" class="primary">▶ broadcast</button>
        </div>
        <div class="counts" id="src-counts">not streaming — press play</div>
        <div class="warn-line" id="hidden-warning" hidden>
          ⚠ This tab is in the background. Chrome throttles hidden tabs to about one timer a
          second against the 20 ms this page asks for, so frames are being skipped to stay
          real-time — that is the browser, not the stack.
        </div>
        <div id="error" class="error"></div>

        <ul class="stages" id="src-stages">
          <li data-stage="advertising set">Extended advertising set — the announcement</li>
          <li data-stage="periodic train">Periodic advertising data — the BASE</li>
          <li data-stage="on the air">Enable both trains</li>
          <li data-stage="creating the BIG">LE Create BIG</li>
          <li data-stage="opening data paths">LE Setup ISO Data Path per BIS</li>
          <li data-stage="streaming">Stream</li>
        </ul>
      </div>

      <div>
        <dl class="kv" id="src-facts"></dl>
        <div class="label">Advertising data — what a scanner sees</div>
        <pre class="wire" id="src-adv">—</pre>
        <div class="label">BASE — Service Data 0x1851 on the periodic train</div>
        <pre class="wire" id="src-base-hex">—</pre>
        <div id="src-base"></div>
      </div>
    </div>

    <p class="hint" style="margin-top:0.9rem">
      A broadcaster's setup is almost all advertising, because it has nobody to ask: the
      announcement carries the <code>Broadcast_ID</code> a receiver filters on, the BASE carries
      the codec configuration and one Audio Location per BIS, and only then can
      <code>LE Create BIG</code> be issued against that advertising handle — the controller
      answers with the BIS connection handles, which is the only way to learn them.
    </p>
  </section>

  <div class="receivers full" id="receivers"></div>

  <section class="panel full">
    <h2>The BASE round trip</h2>
    <div id="round-trip"></div>
    <p class="hint" style="margin-top:0.9rem">
      The left column is <code>BroadcastConfig::base()</code> as the source published it; each
      other column is what that receiver parsed back out of periodic advertising reports it
      reassembled itself. This is the only page on the site where the same structure can be shown
      from both ends of a radio, because it is the only one where a device has to reconstruct its
      peer's configuration rather than being told it over a connection.
    </p>
  </section>

  <section id="setup" class="panel setup full">
    <h2>netsim is not reachable</h2>
    <p>Could not reach netsim at <code>localhost:7681</code> — is <code>netsimd</code> running with
       its WebSocket frontend enabled? Start it with:</p>
    <pre><code>netsimd --logtostderr --no-shutdown --ws-port 7681</code></pre>
    <p class="hint">There is no in-browser fallback for this domain: the in-page radio has no
       periodic advertising and no BIG, and a broadcast without either would be a picture of one.</p>
  </section>
</div>
`;

const RECEIVER_TEMPLATE = `
  <section class="panel">
    <div class="rx-head"></div>
    <ul class="stages rx-stages">
      <li data-stage="scanning">Scan for a Broadcast Audio Announcement</li>
      <li data-stage="syncing to the train">LE Periodic Advertising Create Sync</li>
      <li data-stage="reading the announcement">Read the BASE and BIGInfo off the train</li>
      <li data-stage="joining the BIG">LE BIG Create Sync</li>
      <li data-stage="receiving">Receive</li>
    </ul>
    <p class="rx-note"></p>
    <dl class="kv rx-facts"></dl>
    <div class="row">
      <button class="rx-sound primary" title="Enable sound — a browser only starts an AudioContext from a real click">♪ listen</button>
      <select class="rx-bis" aria-label="which BIS to play"></select>
      <div class="meter"></div>
    </div>
    <label class="field rx-code-label" hidden>Broadcast Code
      <span class="hint">— clear it to see what a listener who was not told it does</span></label>
    <input type="text" class="rx-code" hidden placeholder="none" aria-label="this receiver's Broadcast Code">
    <div class="counts">—</div>
    <div class="label">BASE as this receiver parsed it</div>
    <div class="rx-base"></div>
  </section>
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

  config = { name: DEFAULT_NAME, broadcastId: DEFAULT_BROADCAST_ID, numBis: 2, code: "" };
  streaming = false;
  startedAt = 0;
  framesSent = 0;
  framesSkipped = 0;
  lastRenderAt = 0;

  sourceHead = createDeviceHeader({
    name: "Broadcast Source",
    kind: "broadcaster · no connection, no pairing",
    accent: "accent",
    address: SOURCE_ADDR,
    dotMeans: "the BIG exists and SDUs written to it go out over the air",
    run: {
      running: true,
      onRun: () => {
        sourceStopped = false;
        lastSourceAttempt = performance.now() - RECONNECT_MS;
        sourceHead.setRunning(true);
        sourceHead.setState(false, "starting…");
      },
      onStop: () => {
        sourceStopped = true;
        streaming = false;
        $("play").textContent = "▶ broadcast";
        try {
          source?.terminate();
        } catch (_) {
          /* the socket may already be gone */
        }
        dropSource(performance.now());
        sourceHead.setRunning(false);
        sourceHead.setState(false, "stopped");
        for (const item of root.querySelectorAll("#src-stages li")) {
          item.classList.remove("done", "active");
        }
      },
    },
  });
  $("source-head").append(sourceHead.el);

  $("apply").addEventListener("click", applyConfig);
  $("play").addEventListener("click", toggleStream);
  $("add-receiver").addEventListener("click", addReceiver);

  (async () => {
    await init();
    if (gen !== generation) return;
    buildEncoders();
    receivers = [makeReceiver(0), makeReceiver(1)];
    for (const receiver of receivers) buildReceiverCard(receiver);
    $("many-note").textContent = manyNote();
    syncCodeFields();
    lastSourceAttempt = performance.now() - RECONNECT_MS;
    renderFan();
    timer = setInterval(loop, 20);
  })();
}

/// Builds one receiver's card: its own header, its own stage list, its own
/// decoder and its own audio output. Nothing is shared between receivers except
/// the air.
function buildReceiverCard(receiver) {
  const holder = document.createElement("div");
  holder.innerHTML = RECEIVER_TEMPLATE;
  receiver.el = holder.firstElementChild;
  $("receivers").append(receiver.el);

  receiver.head = createDeviceHeader({
    name: receiver.label,
    kind: "receiver · joins the BIG, tells nobody",
    accent: "good",
    address: receiver.address,
    dotMeans: "this receiver has joined the BIG and SDUs are arriving",
    run: {
      running: true,
      onRun: () => {
        receiver.stopped = false;
        receiver.lastAttempt = performance.now() - RECONNECT_MS;
        receiver.head.setRunning(true);
        receiver.head.setState(false, "starting…");
      },
      onStop: () => {
        receiver.stopped = true;
        try {
          receiver.device?.terminate();
        } catch (_) {
          /* the socket may already be gone */
        }
        dropReceiverDevice(receiver, performance.now());
        receiver.head.setRunning(false);
        receiver.head.setState(false, "stopped");
        for (const item of receiver.el.querySelectorAll(".rx-stages li")) {
          item.classList.remove("done", "active", "stopped");
        }
      },
    },
  });
  receiver.el.querySelector(".rx-head").append(receiver.head.el);

  const meter = receiver.el.querySelector(".meter");
  for (let i = 0; i < 12; i++) {
    const bar = document.createElement("i");
    bar.style.height = `${55 + i * 4}%`;
    meter.append(bar);
  }

  receiver.player = createSduPlayer({ sampleRate: PCM_RATE });
  receiver.lc3 = new WebLc3(PCM_RATE, SDU_INTERVAL_MS * 1000);

  // A browser only lets a user gesture create a running AudioContext; one made
  // from script starts suspended, and then SDUs are counted and scheduled in
  // perfect silence — which looks exactly like a broken audio path.
  const sound = receiver.el.querySelector(".rx-sound");
  sound.addEventListener("click", () => {
    receiver.player.enable();
    // There is no volume characteristic anywhere in a broadcast — no
    // connection, so no Volume Control Service — so this is the page's own
    // output level and is set once, not something read back off a device.
    receiver.player.setVolume(200, false);
    sound.textContent = "♪ on";
    sound.classList.remove("primary");
    sound.classList.add("on");
    sound.disabled = true;
  });

  receiver.el.querySelector(".rx-bis").addEventListener("change", (event) => {
    receiver.bis = Number(event.target.value) || 1;
    resetDecoder(receiver);
  });

  const code = receiver.el.querySelector(".rx-code");
  code.value = receiver.code;
  code.addEventListener("change", () => {
    receiver.code = code.value;
    dropReceiverDevice(receiver, performance.now() - RECONNECT_MS);
  });

  // The last receiver can be taken away again; the first two are the page.
  if (receiver.slot >= 2) {
    const remove = document.createElement("button");
    remove.textContent = "− remove";
    remove.className = "rx-remove";
    remove.addEventListener("click", () => removeReceiver(receiver));
    receiver.el.querySelector(".row").append(remove);
  }
}

/// Stops the timer and drops everything this module created: every netsim
/// socket, every codec and every AudioContext. A tab switch must not leave a
/// device on the air — netsim does not synthesize a disconnect when a socket
/// goes away, so anything left running lingers as a ghost.
export function unmount() {
  generation++;
  if (timer) {
    clearInterval(timer);
    timer = 0;
  }
  for (const receiver of receivers) {
    try {
      receiver.device?.free();
    } catch (_) {
      /* already gone */
    }
    try {
      receiver.player?.close();
    } catch (_) {
      /* already closed */
    }
    try {
      receiver.lc3?.free();
    } catch (_) {
      /* already gone */
    }
    receiver.head?.destroy();
  }
  receivers = [];
  try {
    source?.free();
  } catch (_) {
    /* already gone */
  }
  source = null;
  sourceStatus = null;
  sourceStopped = false;
  for (const encoder of encoders) {
    try {
      encoder.free();
    } catch (_) {
      /* already gone */
    }
  }
  encoders = [];
  sourceHead?.destroy();
  sourceHead = null;
  streaming = false;
  if (root) {
    root.innerHTML = "";
    root = null;
  }
}
