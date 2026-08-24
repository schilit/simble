// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// The Scanner page. All HCI/GAP work — scan bring-up, advertising-report
// parsing, AD-structure decoding — happens in Rust compiled to wasm
// (`transport::wasm_ws::parse_scan_reports`); this file aggregates reports per
// address and draws them. It runs against either controller backend (see the
// selector at the top of the page):
//
//   • in-page   — a wasm `WebLink` in this tab. The scanner and every
//                 advertiser are devices in ONE `SceneEngine`, and that
//                 engine's `link.tick()` routes packets across its own
//                 `Vec<Device>` and nothing else. So the list can only ever
//                 contain peripherals this page added itself. That is not a
//                 filter or a hardcoded list — it is the whole ether.
//   • websocket — a real netsim / rootcanal-ws scene on ws://localhost:7681,
//                 where the ether is shared: other SimBLE tabs, `netsimd
//                 --test-beacons`, and the Android emulator all land here.
//
// The page says which of those it is looking at, in the list header, because
// "why only these devices?" is the first question it provokes.
//
// Two rendering rules, both learned from this page getting them wrong:
//
//   1. ROWS ARE KEYED BY ADDRESS AND SORTED BY ADDRESS. The old sort was
//      `by-name-then-by-RSSI`, and RSSI is redrawn per report — the simulated
//      medium redraws its shadowing term for every advertising report
//      (`controller/sim.rs`: "Shadowing is redrawn per report, which is
//      exactly why an RSSI reading jitters while the devices sit still"). A
//      list sorted on a value that jitters is a list that shuffles four times
//      a second. Address is the one key in an advertising report that cannot
//      change while you are reading it.
//
//   2. ROWS ARE UPDATED, NOT REBUILT. The old render did
//      `devices.innerHTML = ...` on every tick, which destroys and recreates
//      every element — losing focus, text selection, and which row you had
//      expanded. Each row keeps its element; only the sub-spans whose text
//      actually changed are written.
//
//   3. A DEVICE'S NAME IS MERGED, NOT OVERWRITTEN. An advertisement and a
//      scan response can carry different Complete Local Names for the same
//      device, because the advertisement's 31 bytes are shared with everything
//      else it sends and the name is trimmed to fit. Copying whichever arrived
//      last made the name flicker between two spellings four times a second.
//      See `mergeName`.
//
// `checkRows` asserts the first and third of those against the live DOM on
// every render, and publishes the result on `document.body.dataset.
// scannerSelfTest` for a headless driver.

import init, { WebLink, WebPeripheral, WebScanner, catalog_script } from "../pkg/simble.js";
import { createControllerBar } from "../common/controller-bar.js";
import { escapeHtml, nameFor } from "../common/viewer-format.js";

const SCANNER_ADDR = "CC:1E:57:00:00:01";
const wsUrl = (node, addr) =>
  `ws://localhost:7681/v1/websocket/bt?name=${encodeURIComponent(node)}&address=${addr}`;

// --- the device catalog ----------------------------------------------------
//
// The scripts come from the shared catalog via the wasm `catalog_script`
// binding — the same definitions the MCP `example` tool and the Rust tests
// use, so a device cannot mean one thing here and another there. Only the
// menu (order, one-line label, whether it is in the starter set) lives in this
// file, because that is a presentation choice, not a definition.
//
// The names mirror `src/devices/catalog::EXAMPLES`. A name the catalog does
// not know resolves to `undefined` and is dropped from the picker rather than
// throwing, so this list going stale degrades to a missing menu entry.
const CATALOG = [
  { name: "earbud", label: "Earbud L", note: "CSIP set member — advertises a Resolvable Set Identifier", starter: true },
  { name: "earbud_right", label: "Earbud R", note: "the rank-2 member of the same set, a different prand", starter: true },
  { name: "eddystone", label: "Eddystone beacon", note: "FEAA service data, non-connectable", starter: true },
  { name: "fast_pair", label: "Fast Pair beacon", note: "FE2C service data + Google manufacturer data", starter: true },
  { name: "hrm", label: "Heart-rate monitor", note: "180D, notifying its measurement", starter: true },
  { name: "thermometer", label: "Thermometer", note: "1809 Health Thermometer" },
  { name: "battery", label: "Battery", note: "180F — the minimal static peripheral" },
  { name: "env_sensor", label: "Environmental sensor", note: "181A, several characteristics" },
  { name: "hid_keyboard", label: "HID keyboard", note: "1812 over GATT" },
  { name: "hid_mouse", label: "HID mouse", note: "1812, relative motion" },
  { name: "gamepad", label: "Gamepad", note: "1812, two axes and eight buttons" },
  { name: "cycling", label: "Cycling sensor", note: "1816 speed and cadence" },
  { name: "pulse_oximeter", label: "Pulse oximeter", note: "1822, SpO2 as IEEE-11073 SFLOATs" },
  { name: "weight_scale", label: "Smart scale", note: "181D + 181B" },
  { name: "smart_lock", label: "Smart lock", note: "a custom control point" },
  { name: "fitness_tracker", label: "Fitness tracker", note: "several services on one device" },
  { name: "thermostat", label: "Thermostat", note: "a custom 128-bit writable setpoint" },
  { name: "color_bulb", label: "Colour bulb", note: "a custom 128-bit service — 128-bit UUIDs on the air" },
  { name: "media_player", label: "Media player", note: "1848/1849 Media Control" },
  { name: "hearing_aid", label: "Hearing aid", note: "1854 Hearing Access presets" },
  { name: "volume", label: "Volume control", note: "1844" },
  { name: "le-audio-sink", label: "LE Audio sink", note: "PACS / ASCS / VCS" },
  { name: "auracast_source", label: "Auracast source", note: "a broadcast BIG, no connection at all" },
  { name: "auracast_sink", label: "Auracast sink", note: "the BASS Scan Delegator half" },
  { name: "ranging", label: "Ranging responder", note: "185B Channel Sounding" },
  { name: "ranging_tag", label: "Finder tag", note: "ranging + battery, non-connectable until found" },
];

// Deterministic per-entry address: the same catalog device always lands at the
// same place, so adding it twice is a no-op and a mode switch does not move
// it. Index-derived rather than hashed, so two entries can never collide.
const catalogAddress = (index) =>
  `CC:1E:57:01:00:${(index + 1).toString(16).toUpperCase().padStart(2, "0")}`;

// --- assigned-number tables (small, deliberately not exhaustive) ------------

// GAP *service* names, for UUIDs that appear in an advertisement. Kept here
// rather than grown into `common/viewer-format.js`'s UUID_NAMES: that table is
// the shared floor for a GATT viewer (services and characteristics a viewer
// must decode), and advertised-service names are this page's business.
// `nameFor` is still consulted as a fallback so the two never disagree.
const SERVICE_NAMES = {
  "1800": "Generic Access", "1801": "Generic Attribute", "1802": "Immediate Alert",
  "1809": "Health Thermometer", "180A": "Device Information", "180D": "Heart Rate",
  "180F": "Battery", "1812": "Human Interface Device", "1816": "Cycling Speed and Cadence",
  "181A": "Environmental Sensing", "181B": "Body Composition", "181D": "Weight Scale",
  "1822": "Pulse Oximeter", "1826": "Fitness Machine", "1843": "Audio Input Control",
  "1844": "Volume Control", "1845": "Volume Offset Control",
  "1846": "Coordinated Set Identification", "1848": "Media Control",
  "1849": "Generic Media Control", "184E": "Audio Stream Control",
  "1850": "Published Audio Capabilities", "1851": "Basic Audio Announcement",
  "1852": "Broadcast Audio Announcement", "1853": "Common Audio",
  "1854": "Hearing Access", "1855": "Telephony and Media Audio",
  "1856": "Public Broadcast Announcement", "185B": "Ranging",
  "FD6F": "Exposure Notification", "FE2C": "Google Fast Pair", "FEAA": "Eddystone",
};
const serviceName = (uuid) => SERVICE_NAMES[uuid] || nameFor(uuid);

// Bluetooth SIG Company Identifiers. Five entries, on purpose: the point is to
// show that the two bytes in front of manufacturer data are a company and not
// payload, which one recognisable name per row does. A full table is 3000
// rows and belongs in a generated file, not in a page.
const COMPANY_NAMES = {
  "004C": "Apple", "0059": "Nordic Semiconductor", "0075": "Samsung",
  "00E0": "Google", "0006": "Microsoft",
};

// AD types, for labelling the raw length-type-value dump. Splitting the
// payload into LTV triplets is the AD *framing* (Core Spec Vol 3 Part C
// Section 11), not decoding — every semantic value on this page comes from the
// Rust decoder. Types SimBLE's decoder understands are listed in DECODED_AD so
// the dump can mark the ones it walks past.
const AD_TYPE_NAMES = {
  0x01: "Flags",
  0x02: "Incomplete 16-bit Service UUIDs", 0x03: "Complete 16-bit Service UUIDs",
  0x04: "Incomplete 32-bit Service UUIDs", 0x05: "Complete 32-bit Service UUIDs",
  0x06: "Incomplete 128-bit Service UUIDs", 0x07: "Complete 128-bit Service UUIDs",
  0x08: "Shortened Local Name", 0x09: "Complete Local Name", 0x0a: "TX Power Level",
  0x0d: "Class of Device", 0x12: "Peripheral Connection Interval Range",
  0x14: "16-bit Service Solicitation UUIDs", 0x15: "128-bit Service Solicitation UUIDs",
  0x16: "Service Data — 16-bit UUID", 0x17: "Public Target Address",
  0x18: "Random Target Address", 0x19: "Appearance", 0x1a: "Advertising Interval",
  0x1b: "LE Bluetooth Device Address", 0x1c: "LE Role",
  0x20: "Service Data — 32-bit UUID", 0x21: "Service Data — 128-bit UUID",
  0x24: "URI", 0x26: "Transport Discovery Data", 0x27: "LE Supported Features",
  0x28: "Channel Map Update Indication", 0x2d: "Broadcast Code",
  0x2e: "Resolvable Set Identifier", 0x2f: "Advertising Interval — long",
  0x30: "Broadcast Name", 0x3d: "3D Information Data",
  0xff: "Manufacturer Specific Data",
};
const DECODED_AD = new Set([0x01, 0x02, 0x03, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x16, 0x2e, 0xff]);

// The Flags octet (Core Spec Vol 3 Part C Section 11, Table 11.1).
const FLAG_BITS = [
  "LE Limited Discoverable",
  "LE General Discoverable",
  "BR/EDR Not Supported",
  "Simultaneous LE and BR/EDR (Controller)",
  "Simultaneous LE and BR/EDR (Host)",
];

const $ = (id) => document.getElementById(id);

// address -> aggregated device state. Insertion order is irrelevant: the view
// sorts by address.
const devices = new Map();
// address -> { el, nameEl, addrEl, signalEl, detailEl, lastName, ... } — the
// live DOM for a device, kept so a row is written to rather than recreated.
const rows = new Map();
// Addresses whose detail pane is open. Lives outside the DOM so it survives a
// backend rebuild: switch controller, come back, your row is still expanded.
const expanded = new Set();
// Catalog names the user has asked for, by `CATALOG` index. Also outside the
// backend, for the same reason.
const chosen = new Set(CATALOG.flatMap((e, i) => (e.starter ? [i] : [])));

let backend = null;
let mode = "in-page";

// --- the liveness meter ----------------------------------------------------
//
// Each row's detail already carries that device's own adv/rsp counts. What the
// page had no sign of was whether it was receiving anything *at all* — a
// silent list and a working-but-idle list look identical. So: one number that
// ticks up, and the rate it is ticking at, above the list.
//
// The rate is measured the same way the per-device interval is, from report
// arrivals, over a short trailing window so it reacts rather than averaging
// across the whole session.
const RATE_WINDOW_MS = 3000;
let reportTotal = 0;
let meterStart = 0;
// One entry per tick that delivered reports — `{ t, n }`, not one per report.
// A tick drains everything the controller has buffered, so the reports in a
// batch all arrive at the same instant: counting them individually would put
// thousands of identical timestamps in here, and dividing by the span between
// the first and the last of them divides by roughly zero. A hidden tab, whose
// timers Chrome drops to once a minute, makes that batch enormous — which is
// exactly how this read 39,741/s before it counted batches instead.
const drains = [];

function countReports(now, n) {
  if (!n) return;
  reportTotal += n;
  drains.push({ t: now, n });
}

function reportRate(now) {
  const cutoff = now - RATE_WINDOW_MS;
  while (drains.length && drains[0].t < cutoff) drains.shift();
  let n = 0;
  for (const d of drains) n += d.n;
  if (!n) return 0;
  // The denominator is the window, not the spread of the arrivals inside it,
  // so a single fat batch reads as a burst rather than as infinity. Before the
  // window has filled, the time since the scene started stands in for it.
  const span = Math.min(RATE_WINDOW_MS, now - meterStart);
  return span > 0 ? (n / span) * 1000 : 0;
}

// --- the invariant, asserted in the page -----------------------------------
//
// This list has now shown a row the wrong thing twice: once by rebuilding
// rows from a list sorted on a jittering key, once by a name that alternated
// between an advertisement's trimmed spelling and a scan response's full one.
// Both were invisible in the source and plain in the DOM, so the check reads
// the DOM, runs every render, and is cheap enough to leave on.
//
// It is published to `document.body.dataset.scannerSelfTest` so a headless
// driver can assert on it without the page growing a test-only hook.
const selfTest = { renders: 0, renames: 0, violations: [] };

function checkRows() {
  selfTest.renders++;
  for (const el of $("devices").children) {
    const addr = el.dataset.addr;
    const d = devices.get(addr);
    const fail = (why) => {
      if (selfTest.violations.length < 20) selfTest.violations.push(`${addr}: ${why}`);
      console.error("scanner invariant:", addr, why);
    };
    if (!d) { fail("row has no device"); continue; }
    if (rows.get(addr)?.el !== el) { fail("element is not the one keyed for this address"); continue; }
    // The rendered name must be this address's own name — the whole bug class.
    const shown = el.querySelector(".nameline .nm-real, .nameline .nm-none")?.textContent;
    const want = d.name ?? d.address;
    if (shown !== want) fail(`renders name "${shown}", device holds "${want}"`);
  }
}

function setPill(text, cls) {
  const pill = $("conn");
  pill.textContent = text;
  pill.className = "pill" + (cls ? " " + cls : "");
}

// The catalog script for entry `index`, or null if the catalog does not have
// it (a stale name in CATALOG above).
function scriptFor(index) {
  const entry = CATALOG[index];
  if (!entry) return null;
  try {
    return catalog_script(entry.name) ?? null;
  } catch (e) {
    console.error("catalog_script:", entry.name, e);
    return null;
  }
}

// --- WebSocket backend (netsim / rootcanal-ws) -----------------------------
function makeWsBackend() {
  let scanner = null;
  const hosted = new Map(); // catalog index -> WebPeripheral
  let openedOnce = false;
  let lastAttempt = 0;
  const t0 = performance.now();

  const startScanner = () => {
    try { scanner = new WebScanner(wsUrl("web-scanner", SCANNER_ADDR)); }
    catch (e) { scanner = null; console.error("WebScanner:", e); }
  };
  const dropHosted = () => {
    for (const dev of hosted.values()) { try { dev.free(); } catch (_) { /* gone */ } }
    hosted.clear();
  };
  // netsim does not synthesise a disconnect when a WebSocket drops, so a
  // device whose socket died lingers on the scene at the same address. Freeing
  // ours on teardown is the only thing that keeps the scene honest.
  const syncHosted = () => {
    for (const index of chosen) {
      if (hosted.has(index)) continue;
      const script = scriptFor(index);
      if (!script) continue;
      const entry = CATALOG[index];
      try {
        hosted.set(index, new WebPeripheral(
          wsUrl(`web-scan-${entry.name}`, catalogAddress(index)), script));
      } catch (e) { console.error("WebPeripheral:", entry.name, e); }
    }
    for (const [index, dev] of [...hosted]) {
      if (chosen.has(index)) continue;
      try { dev.free(); } catch (_) { /* gone */ }
      hosted.delete(index);
      devices.delete(catalogAddress(index));
    }
  };

  startScanner();
  return {
    scope: "websocket",
    hostedCount: () => hosted.size,
    tick() {
      if (!scanner) {
        const now = performance.now();
        if (now - lastAttempt > 3000) { lastAttempt = now; startScanner(); }
        return [];
      }
      const state = scanner.ready_state(); // 0 connecting 1 open 2 closing 3 closed
      if (state === 3) {
        setPill(openedOnce ? "connection lost — reconnecting…" : "netsim not reachable", "bad");
        if (!openedOnce) $("setup").classList.add("visible");
        try { scanner.free(); } catch (_) { /* gone */ }
        scanner = null;
        dropHosted();
        return [];
      }
      if (state === 0) {
        setPill(openedOnce ? "reconnecting…" : "connecting to localhost:7681…", "");
        return [];
      }
      openedOnce = true;
      $("setup").classList.remove("visible");
      syncHosted();
      const t = (performance.now() - t0) / 1000;
      for (const dev of hosted.values()) {
        try { if (dev.ready_state() !== 3) dev.tick(t); } catch (_) { /* skip */ }
      }
      setPill(`scanning · ${devices.size} device${devices.size === 1 ? "" : "s"}`, "ok");
      try { return JSON.parse(scanner.tick()); } catch (e) { console.error("tick:", e); return []; }
    },
    teardown() { dropHosted(); try { scanner?.free(); } catch (_) { /* gone */ } scanner = null; },
  };
}

// --- In-page backend (one wasm SceneEngine in this tab) --------------------
function makeInPageBackend() {
  const link = new WebLink();
  const scannerIndex = link.add_scanner(SCANNER_ADDR);
  const added = new Set(); // catalog indices already in the scene
  const t0 = performance.now();
  $("setup").classList.remove("visible");

  // `SceneEngine` has no remove: a device is in the scene for the engine's
  // lifetime. Adding is therefore incremental, and un-choosing a device is
  // handled by `rebuildBackend` throwing the whole engine away — which is also
  // what "Clear" does.
  const syncAdded = () => {
    for (const index of chosen) {
      if (added.has(index)) continue;
      const script = scriptFor(index);
      if (!script) { added.add(index); continue; }
      try {
        link.add_peripheral(catalogAddress(index), script);
        added.add(index);
      } catch (e) {
        console.error("in-page peripheral:", CATALOG[index].name, e);
        added.add(index); // do not retry a script that does not compile
      }
    }
  };

  return {
    scope: "in-page",
    hostedCount: () => added.size,
    tick() {
      syncAdded();
      link.tick((performance.now() - t0) / 1000);
      const n = link.device_count() - 1; // minus the scanner itself
      setPill(`scanning · ${n} advertiser${n === 1 ? "" : "s"} in this tab`, "ok");
      try { return JSON.parse(link.scanner_reports_json(scannerIndex)); }
      catch (e) { console.error("tick:", e); return []; }
    },
    teardown() { try { link.free(); } catch (_) { /* gone */ } },
  };
}

function rebuildBackend() {
  backend?.teardown();
  devices.clear();
  rows.clear();
  // The meter counts the scene in front of you, so a new scene starts at zero
  // rather than carrying a total that no longer refers to anything.
  reportTotal = 0;
  drains.length = 0;
  meterStart = performance.now();
  selfTest.violations.length = 0;
  $("devices").replaceChildren();
  backend = mode === "websocket" ? makeWsBackend() : makeInPageBackend();
  renderScope();
  renderPicker();
  render();
}

// --- report aggregation ----------------------------------------------------

// Handled explicitly below, so the generic pass must not touch them.
const MERGE_SKIP = new Set([
  "address", "address_type", "rssi", "connectable", "scan_response", "raw", "name",
]);
// Fields this page draws a row for. Anything else the Rust decoder starts
// sending shows up under "other fields" instead of vanishing.
const RENDERED = new Set([
  "name", "flags", "tx_power", "service_uuids", "service_data",
  "manufacturer_data", "resolvable_set_identifier",
]);

// The name is *merged*, not overwritten — the one field where "copy whatever
// this report carries" is wrong.
//
// A device may put a different Complete Local Name in its advertisement than
// in its scan response, and both are AD type 0x09. The advertisement shares a
// 31-byte budget with everything else it sends, so the name is trimmed to fit
// whatever is left; the scan response is a second, nearly empty payload, so
// the full name goes there. The catalog's hearing aid is exactly this:
//
//   ADV      0B 09 "Hearing Ai"     (31 bytes: flags + 3 UUIDs + RSI + name)
//   SCAN_RSP 0E 09 "Hearing Aid L"  (15 bytes: nothing but the name)
//
// Last-writer-wins made `d.name` alternate between the two spellings on every
// report — a name changing several times a second on a row whose address never
// moved. That is the "names jump between rows" bug: nothing moved between
// rows at all, the name of one row was flickering between two values.
//
// A shorter name that is a prefix of the one already held is that same name
// truncated, so the fuller one wins. Anything else is a device actually
// renaming itself and replaces what is held. The one case this gets wrong —
// a device renaming itself to a strict prefix of its old name — keeps the old
// name, which is a far better failure than a name that will not sit still.
function mergeName(d, name) {
  if (d.name === undefined) { d.name = name; return; }
  if (d.name === name || d.name.startsWith(name)) return;
  if (!name.startsWith(d.name)) selfTest.renames++;
  d.name = name;
}

// Folds one advertising report into the device's aggregated state.
//
// The merge is driven by what the report actually carries, not by a
// hand-written list of field names. The old version assigned a fixed set of
// fields and dropped everything else, which is how `resolvable_set_identifier`
// could land in the Rust decoder and never reach the page. The rule now: copy
// any field that is *present* (not null, not an empty array or string), and
// remember anything not in RENDERED so it is still shown.
//
// "Present" is the right test rather than "in a non-scan-response report",
// because a scan response is a second, different payload — merging its empty
// service-UUID list over the advertisement's would erase what the device said.
// `name` is the exception, and `mergeName` above says why.
function merge(report, now) {
  const addr = report.address;
  let d = devices.get(addr);
  if (!d) {
    d = { address: addr, firstSeen: now, arrivals: [], advCount: 0, rspCount: 0, extra: {} };
    devices.set(addr, d);
  }
  d.lastSeen = now;
  d.rssi = report.rssi;
  d.address_type = report.address_type;
  if (report.scan_response) {
    d.rspCount++;
    if (report.raw) d.rawScanRsp = report.raw;
  } else {
    d.connectable = report.connectable;
    if (report.raw) d.raw = report.raw;
    d.advCount++;
    // One timestamp per report, not per poll: several reports can be drained
    // by a single tick, and dividing the window by the gap count recovers the
    // right rate either way.
    d.arrivals.push(now);
    if (d.arrivals.length > 32) d.arrivals.shift();
  }
  if (report.name) mergeName(d, report.name);
  for (const [key, value] of Object.entries(report)) {
    if (MERGE_SKIP.has(key)) continue;
    if (value === null || value === undefined) continue;
    if (Array.isArray(value) && value.length === 0) continue;
    if (value === "") continue;
    d[key] = value;
    if (!RENDERED.has(key)) d.extra[key] = value;
  }
}

// Mean advertising interval, measured from report arrivals — the same thing
// nRF Connect shows, and the only way to get it: the interval is a property of
// the advertiser's timing, and nothing on the air states it (AD types 0x1A and
// 0x2F would, and no device here sends them).
function advInterval(d) {
  if (d.arrivals.length < 3) return null;
  const span = d.arrivals[d.arrivals.length - 1] - d.arrivals[0];
  if (span <= 0) return null;
  return span / (d.arrivals.length - 1);
}

// --- formatting ------------------------------------------------------------

const hexPairs = (hex) => (hex.match(/../g) ?? []).join(" ");
const bar = (rssi) => (rssi >= -60 ? 4 : rssi >= -72 ? 3 : rssi >= -84 ? 2 : 1);

function flagsText(flags) {
  const set = FLAG_BITS.filter((_, i) => flags & (1 << i));
  return `0x${flags.toString(16).toUpperCase().padStart(2, "0")}` +
    (set.length ? ` — ${set.join(", ")}` : " — none set");
}

function uuidText(uuid) {
  const known = serviceName(uuid);
  return known ? `${uuid} <span class="nm">${escapeHtml(known)}</span>` : escapeHtml(uuid);
}

function adRow(type, label, valueHtml) {
  const t = `0x${type.toString(16).toUpperCase().padStart(2, "0")}`;
  return `<tr><td class="t">${t}</td><td class="k">${escapeHtml(label)}</td>` +
    `<td class="v">${valueHtml}</td></tr>`;
}

// The decoded view: one row per AD structure SimBLE understood, labelled with
// the AD type it came from. Every value here is what Rust decoded — this
// function chooses wording and order, nothing more.
function decodedTable(d) {
  const out = [];
  if (d.flags !== undefined) out.push(adRow(0x01, "Flags", escapeHtml(flagsText(d.flags))));
  if (d.name) out.push(adRow(0x09, "Complete Local Name", escapeHtml(d.name)));
  const short = (d.service_uuids ?? []).filter((u) => u.length <= 8);
  const long = (d.service_uuids ?? []).filter((u) => u.length > 8);
  if (short.length) out.push(adRow(0x03, "Service UUIDs (16-bit)", short.map(uuidText).join("<br>")));
  if (long.length) out.push(adRow(0x07, "Service UUIDs (128-bit)", long.map(uuidText).join("<br>")));
  if (d.tx_power !== undefined) out.push(adRow(0x0a, "TX Power Level", `${d.tx_power} dBm`));
  for (const sd of d.service_data ?? []) {
    out.push(adRow(0x16, "Service Data", `${uuidText(sd.tag)} · ` +
      `<span class="hex">${escapeHtml(hexPairs(sd.data)) || "(empty)"}</span>`));
  }
  // The CSIP Resolvable Set Identifier: six octets, hash first then the prand
  // the hash was taken over (CSIS Section 4.9). Split rather than shown as one
  // blob, because the split IS the mechanism — a coordinator recomputes
  // sih(SIRK, prand) from the second half and compares it to the first.
  // A scanner cannot do that resolution: it has no SIRK. That is the point of
  // the profile, so the row says so instead of pretending.
  if (d.resolvable_set_identifier) {
    const rsi = d.resolvable_set_identifier;
    out.push(adRow(0x2e, "Resolvable Set Identifier",
      `<span class="hex">hash ${escapeHtml(hexPairs(rsi.slice(0, 6)))}</span> · ` +
      `<span class="hex">prand ${escapeHtml(hexPairs(rsi.slice(6)))}</span>` +
      `<div class="sub">member of a coordinated set — resolves only against the ` +
      `set's SIRK, which a scanner does not hold</div>`));
  }
  if (d.manufacturer_data) {
    const id = d.manufacturer_data.tag;
    const co = COMPANY_NAMES[id];
    out.push(adRow(0xff, "Manufacturer Specific Data",
      `0x${escapeHtml(id)}${co ? ` <span class="nm">${escapeHtml(co)}</span>` : ""} · ` +
      `<span class="hex">${escapeHtml(hexPairs(d.manufacturer_data.data)) || "(empty)"}</span>`));
  }
  // Anything the Rust decoder gained that this page has no row for yet. The
  // old merge dropped unknown fields silently; this makes a new field visible
  // the day it appears, without a page change.
  for (const [key, value] of Object.entries(d.extra)) {
    out.push(`<tr><td class="t">—</td><td class="k">${escapeHtml(key)}</td>` +
      `<td class="v"><span class="hex">${escapeHtml(JSON.stringify(value))}</span></td></tr>`);
  }
  return out.length ? `<table class="adtab">${out.join("")}</table>`
    : `<p class="empty">No AD structures — an advertisement with an empty payload.</p>`;
}

// The raw payload, split into its length-type-value triplets. This is framing,
// not decoding: the value column stays hex. Its job is to show that the
// decoded table above is a reading of these exact bytes, and to make an AD
// type SimBLE walks past visible rather than invisible.
function rawTable(hex) {
  const bytes = (hex.match(/../g) ?? []).map((b) => parseInt(b, 16));
  const out = [];
  let i = 0;
  while (i < bytes.length) {
    const len = bytes[i];
    // A structure is one length octet plus `len` octets of type+data. Zero
    // length is the end-of-data padding a controller writes into the fixed
    // 31-byte block; an overlong length is a truncated capture.
    if (len === 0 || i + 1 + len > bytes.length) break;
    const type = bytes[i + 1];
    const payload = bytes.slice(i + 2, i + 1 + len);
    const label = AD_TYPE_NAMES[type] ?? "unknown AD type";
    const skipped = DECODED_AD.has(type) ? "" : ` <span class="skip">not decoded</span>`;
    out.push(`<tr><td class="t">${len.toString(16).toUpperCase().padStart(2, "0")} ` +
      `${type.toString(16).toUpperCase().padStart(2, "0")}</td>` +
      `<td class="k">${escapeHtml(label)}${skipped}</td>` +
      `<td class="v"><span class="hex">` +
      `${payload.map((b) => b.toString(16).toUpperCase().padStart(2, "0")).join(" ") || "—"}` +
      `</span></td></tr>`);
    i += 1 + len;
  }
  if (i < bytes.length) {
    out.push(`<tr><td class="t">—</td><td class="k">trailing bytes</td>` +
      `<td class="v"><span class="hex">${escapeHtml(hexPairs(hex.slice(i * 2)))}</span></td></tr>`);
  }
  return out.length ? `<table class="adtab raw">${out.join("")}</table>` : "";
}

function payloadBlock(title, hex) {
  if (!hex) return "";
  return `<div class="pl">
    <div class="pl-head"><span class="pl-title">${escapeHtml(title)}</span>
      <span class="pl-len">${hex.length / 2} bytes</span>
      <button class="copy" data-copy="${escapeHtml(hex)}">copy hex</button></div>
    ${rawTable(hex)}
    <div class="hexdump">${escapeHtml(hexPairs(hex))}</div>
  </div>`;
}

function detailHtml(d) {
  return `<div class="dsec"><h4>Decoded advertising data</h4>${decodedTable(d)}</div>
    <div class="dsec"><h4>Raw</h4>
      ${payloadBlock("Advertisement (AD)", d.raw)}
      ${payloadBlock("Scan response (SCAN_RSP)", d.rawScanRsp)}
      ${!d.raw && !d.rawScanRsp ? `<p class="empty">No payload captured yet.</p>` : ""}
    </div>`;
}

function nameLineHtml(d) {
  const name = d.name
    ? `<span class="nm-real">${escapeHtml(d.name)}</span>`
    : `<span class="nm-none">${escapeHtml(d.address)}</span>`;
  const chips = [];
  chips.push(d.connectable
    ? `<span class="chip yes">connectable</span>`
    : `<span class="chip no">non-connectable</span>`);
  for (const uuid of d.service_uuids ?? []) {
    const known = serviceName(uuid);
    chips.push(`<span class="chip">${escapeHtml(known ?? (uuid.length > 8 ? uuid.slice(0, 8) + "…" : uuid))}</span>`);
  }
  if (d.resolvable_set_identifier) chips.push(`<span class="chip set">set member</span>`);
  if (d.manufacturer_data) {
    const co = COMPANY_NAMES[d.manufacturer_data.tag];
    chips.push(`<span class="chip">${escapeHtml(co ?? `mfg 0x${d.manufacturer_data.tag}`)}</span>`);
  }
  if (d.tx_power !== undefined) chips.push(`<span class="chip">tx ${d.tx_power} dBm</span>`);
  return `${name}${chips.join("")}`;
}

function addrLineHtml(d) {
  const parts = [`<span class="mono">${escapeHtml(d.address)}</span>`,
                 escapeHtml(d.address_type ?? "?")];
  const interval = advInterval(d);
  if (interval !== null) parts.push(`${Math.round(interval)} ms`);
  parts.push(`${d.advCount} adv${d.rspCount ? ` · ${d.rspCount} rsp` : ""}`);
  return parts.join(" · ");
}

function signalHtml(d) {
  const level = bar(d.rssi);
  const bars = [1, 2, 3, 4].map((n) =>
    `<i class="${n <= level ? "on" : ""}"></i>`).join("");
  return `<span class="db">${d.rssi} dBm</span><span class="bars">${bars}</span>`;
}

// --- keyed rendering -------------------------------------------------------

// Writes only when the markup actually differs. A row whose RSSI moved must
// not have its address line — which the user may be selecting to copy —
// replaced along with it.
function setHtml(el, html, row, key) {
  if (row[key] === html) return;
  row[key] = html;
  el.innerHTML = html;
}

function makeRow(address) {
  const el = document.createElement("div");
  el.className = "device";
  el.dataset.addr = address;
  el.innerHTML = `<div class="top" tabindex="0" role="button" aria-expanded="false">
      <span class="caret" aria-hidden="true">▸</span>
      <span class="ident"><span class="nameline"></span><span class="addrline"></span></span>
      <span class="signal"></span>
    </div><div class="detail" hidden></div>`;
  return {
    el,
    top: el.querySelector(".top"),
    caret: el.querySelector(".caret"),
    nameEl: el.querySelector(".nameline"),
    addrEl: el.querySelector(".addrline"),
    signalEl: el.querySelector(".signal"),
    detailEl: el.querySelector(".detail"),
  };
}

function render() {
  const list = $("devices");
  // Sorted by address: the one key in an advertising report that does not
  // change while you are reading the list. See the header comment.
  const sorted = [...devices.values()].sort((a, b) => (a.address < b.address ? -1 : 1));
  const now = performance.now();

  for (let i = 0; i < sorted.length; i++) {
    const d = sorted[i];
    let row = rows.get(d.address);
    if (!row) {
      row = makeRow(d.address);
      rows.set(d.address, row);
      // Address order never changes, so a row is placed once and then stays:
      // insert before the first row that already sorts after it.
      const before = [...list.children].find((c) => c.dataset.addr > d.address);
      list.insertBefore(row.el, before ?? null);
    }
    setHtml(row.nameEl, nameLineHtml(d), row, "hName");
    setHtml(row.addrEl, addrLineHtml(d), row, "hAddr");
    setHtml(row.signalEl, signalHtml(d), row, "hSignal");

    const open = expanded.has(d.address);
    if (row.open !== open) {
      row.open = open;
      row.detailEl.hidden = !open;
      row.top.setAttribute("aria-expanded", String(open));
      row.caret.textContent = open ? "▾" : "▸";
    }
    // The detail pane is only written while it is visible — a closed row's
    // hex table is not worth rebuilding four times a second, and an open one's
    // only changes when the payload does.
    if (open) setHtml(row.detailEl, detailHtml(d), row, "hDetail");

    const stale = now - d.lastSeen > 5000;
    if (row.stale !== stale) { row.stale = stale; row.el.classList.toggle("stale", stale); }
  }

  $("empty").hidden = sorted.length > 0;
  renderScope();
  renderMeter(now);
  checkRows();
  document.body.dataset.scannerSelfTest = JSON.stringify({
    renders: selfTest.renders, reports: reportTotal,
    renames: selfTest.renames, violations: selfTest.violations,
  });
}

const setText = (el, text) => { if (el.textContent !== text) el.textContent = text; };

function renderMeter(now) {
  setText($("adv-total"), reportTotal.toLocaleString());
  setText($("adv-rate"), `${reportRate(now).toFixed(1)}/s`);
}

// --- "why only these devices?" ---------------------------------------------
//
// Answered where the question is asked — above the list — rather than in an
// About box. In-page mode is a closed ether; websocket mode is a shared one.
function renderScope() {
  const n = devices.size;
  const hosted = backend?.hostedCount() ?? 0;
  const el = $("scope");
  const heard = n === 0
    ? `<b>Nothing on the air yet.</b>`
    : `<b>${n} device${n === 1 ? "" : "s"}.</b>`;
  const html = mode === "websocket"
    ? `${heard} This is the netsim scene at <code>localhost:7681</code>, and its
       ether is shared — so the list is everything on it: the ${hosted}
       device${hosted === 1 ? "" : "s"} this page is hosting, plus any other SimBLE
       tab pointed at netsim, <code>netsimd --test-beacons</code>, and the Android
       emulator if one is attached.`
    : `${heard} There can be no others. The in-page controller runs the scanner and
       every advertiser as devices in <em>one wasm scene inside this tab</em>, and
       that scene routes packets across its own device list and nothing else — so
       the only things on the air are the ${hosted} this page put there. Nothing is
       filtered out; there is nothing else to hear.
       ${hosted === 0 ? "Add some below" : "Add more below"}, or switch to
       <b>netsim</b> above to scan a scene other tabs and the emulator share.`;
  if (el.dataset.html !== html) { el.dataset.html = html; el.innerHTML = html; }
}

// --- the catalog picker ----------------------------------------------------
function renderPicker() {
  const host = $("picker");
  const html = CATALOG.map((entry, i) => {
    if (!scriptFor(i)) return ""; // not in the catalog — drop it from the menu
    const on = chosen.has(i);
    return `<label class="pick${on ? " on" : ""}">
      <input type="checkbox" data-index="${i}"${on ? " checked" : ""}>
      <span class="pick-name">${escapeHtml(entry.label)}</span>
      <span class="pick-note">${escapeHtml(entry.note)}</span>
    </label>`;
  }).join("");
  if (host.dataset.html !== html) { host.dataset.html = html; host.innerHTML = html; }
}

// --- events ----------------------------------------------------------------

// `navigator.clipboard` is not always available or allowed — it needs a secure
// context and a trusted user gesture, and rejects without either. The
// selection fallback works anywhere, and leaving the bytes selected means even
// a total failure ends with the payload ready for the platform's own copy.
async function copyHex(button) {
  const hex = button.dataset.copy;
  const done = (label) => {
    button.textContent = label;
    setTimeout(() => { button.textContent = "copy hex"; }, 1400);
  };
  try {
    await navigator.clipboard.writeText(hex);
    done("copied");
    return;
  } catch (_) { /* fall through to the selection path */ }
  const dump = button.closest(".pl")?.querySelector(".hexdump");
  if (!dump) { done("copy failed"); return; }
  const range = document.createRange();
  range.selectNodeContents(dump);
  const selection = window.getSelection();
  selection.removeAllRanges();
  selection.addRange(range);
  done(document.execCommand?.("copy") ? "copied" : "press ⌘C");
}

$("devices").addEventListener("click", (e) => {
  const copy = e.target.closest(".copy");
  if (copy) { copyHex(copy); return; }
  const top = e.target.closest(".top");
  if (!top) return;
  // Do not swallow a click that ended a text selection — the whole head is the
  // toggle, and the address in it is something you select to copy. Only a
  // selection *inside this head* suppresses the toggle: a leftover selection
  // somewhere else on the page (the hex fallback above leaves one) must not
  // make the next row refuse to open.
  const selection = window.getSelection();
  if (selection && !selection.isCollapsed && top.contains(selection.anchorNode)) return;
  toggle(top.closest(".device").dataset.addr);
});

$("devices").addEventListener("keydown", (e) => {
  if (e.key !== "Enter" && e.key !== " ") return;
  const top = e.target.closest(".top");
  if (!top) return;
  e.preventDefault();
  toggle(top.closest(".device").dataset.addr);
});

function toggle(address) {
  if (expanded.has(address)) expanded.delete(address); else expanded.add(address);
  render();
}

$("picker").addEventListener("change", (e) => {
  const box = e.target.closest("input[type=checkbox]");
  if (!box) return;
  const index = Number(box.dataset.index);
  if (box.checked) {
    chosen.add(index);
    // Adding is incremental in both backends — no rebuild, so the rest of the
    // list does not blink. The picker is likewise only restyled, never
    // rewritten: replacing its innerHTML here detaches every other checkbox
    // (and drops the focus ring off the one just ticked), which is the same
    // mistake the device list used to make four times a second.
    box.closest(".pick")?.classList.add("on");
  } else {
    chosen.delete(index);
    // Removing is not: `SceneEngine` has no remove, so in-page mode has to
    // start a fresh scene. WebSocket mode could drop just the one socket, but
    // taking the same path keeps one behaviour to reason about.
    rebuildBackend();
  }
});

$("pick-all").addEventListener("click", () => {
  CATALOG.forEach((_, i) => { if (scriptFor(i)) chosen.add(i); });
  renderPicker();
});
$("pick-none").addEventListener("click", () => {
  chosen.clear();
  rebuildBackend();
});

function loop() {
  if (!backend) return;
  const now = performance.now();
  const reports = backend.tick();
  countReports(now, reports.length);
  for (const report of reports) merge(report, now);
  render();
}

await init();

const controllerBar = createControllerBar({
  supports: { "in-page": true, "websocket": true },
  onChange: (m) => { mode = m; rebuildBackend(); },
});
controllerBar.el.classList.add("standalone");
$("backend").append(controllerBar.el);
mode = controllerBar.selected;
rebuildBackend();
setInterval(loop, 250);

// Chrome throttles a hidden tab's timers to about 1 Hz, which makes the
// measured advertising interval meaningless while you are on another tab. The
// arrival window is cleared on return so the number re-converges from live
// samples instead of averaging across the gap.
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState !== "visible") return;
  for (const d of devices.values()) d.arrivals.length = 0;
});
