// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// The Testing page's **Data** category: a 256 KB bulk transfer, measured.
//
// The other category asserts. This one measures, and the difference is worth
// naming: an assertion is true or false and reproducible, while a
// measurement is a number that depends on which controller produced it. So
// every number here carries its provenance — which controller, simulated or
// real RF, confirmed by the receiver or merely sent — and the page refuses to
// show a figure without it.
//
// ## Why a waterfall and not a speedometer
//
// "How many megabits" is the smaller half of the question. The whole
// question is "how long until 256 KB has landed", and on BLE the answer is
// dominated by what happens *before* any payload moves: hearing an
// advertisement, opening the connection, agreeing an MTU, walking the peer's
// attribute table. A single Mbps figure erases exactly that. So each run is
// one horizontal stacked bar on a shared time axis — discover, connect,
// negotiate, transfer — the way a browser's network panel draws a request.
// If real radio wins on transfer and loses on discovery, the bars say so.
//
// ## Why runs accumulate
//
// Discovery latency on BLE is quasi-random: it depends on the phase
// relationship between the advertiser's interval and the scanner's window, so
// its distribution has a long tail. Twenty runs can average 300 ms with one
// run at 1.9 s, and that one run is what a person experiences as "sometimes
// it just hangs". N bars *are* the distribution; the aggregate bar underneath
// them carries the mean with min/max whiskers, and the median is printed
// beside the mean because when the two diverge, the divergence is the
// finding.
//
// A failed run is a measurement too. An empty chart and a chart of three
// failures look identical if failures are dropped, and the second is the more
// useful picture.

import {
  WebBulkBench,
  WebBulkCentral,
  WebBulkSink,
} from "../pkg/simble.js";
import {
  createControllerBar,
  usbBridgeHttp,
  usbBridgeUrl,
} from "../common/controller-bar.js";
import { createControllerStrip } from "../common/controller-strip.js";

/// Which controllers this category can run on. All three: the benchmark's
/// Rust is the same code in every one, which is the only reason comparing
/// the numbers means anything.
export const SUPPORTS = { "in-page": true, "websocket": true, "usb": true };

// --- the four segments ------------------------------------------------------
//
// Ordered, and the order is the run: nothing can transfer before it has
// negotiated. The colours are the site's own palette so the chart does not
// introduce a fifth accent nobody else uses.
const PHASES = [
  // Finding a peer nobody named. Only a scanning run has one, and it happens
  // before the run starts, so it is its own segment rather than part of
  // `discover`, which is bring-up against a peer already known.
  { key: "scan_ms", label: "scan", fill: "#bf3989" },
  { key: "discover_ms", label: "discover", fill: "#8250df" },
  { key: "connect_ms", label: "connect", fill: "#0969da" },
  { key: "negotiate_ms", label: "negotiate", fill: "#9a6700" },
  { key: "transfer_ms", label: "transfer", fill: "#1a7f37" },
];

const STORE_KEY = "simble-data-runs";
/// Enough to see a tail without making the chart unreadable or the stored
/// blob large. Oldest are dropped first.
const MAX_RUNS = 200;

// The two in-page/netsim identities. Fixed rather than settings: nothing
// about the measurement depends on them, and an address field is a knob for
// its own sake.
const SINK_ADDR = "CC:1E:57:00:00:0B";
/// The sink strip's value when the peripheral is a phone running SimBLE Android
/// rather than a dongle of ours.
const PHONE_SINK = "phone";
/// A sink strip value naming one phone: `phone:<adb serial>`. Every phone is
/// its own choice, because "the phone" is ambiguous the moment there are two —
/// and the ambiguity is not cosmetic: the scan would take whichever
/// advertised first while the counters came from whichever port was
/// configured, and those need not be the same device.
const phoneValue = (serial) => `${PHONE_SINK}:${serial}`;
const isPhone = (value) => value === PHONE_SINK || value.startsWith(`${PHONE_SINK}:`);
/// The peripheral strip's note for a sink choice. A phone the bridge sees
/// running needs no instruction; only one that is not running does.
const sinkWhy = (value) => {
  if (!isPhone(value)) return "the peripheral: it counts what lands";
  return phones.get(value)?.running
    ? "the phone counts what lands and reports it back"
    : "the phone counts what lands — start SimBLE Android on it first";
};
/// What the bridge told us about each phone, keyed by strip value. A phone's
/// counters are read back through the bridge's `/sink/<ip>` proxy, the ip taken
/// from here — derived from the choice, never a field to fill in.
const phones = new Map();
const CENTRAL_ADDR = "CC:1E:57:00:00:0C";
const netsimUrl = (node, address) =>
  `ws://localhost:7681/v1/websocket/bt?name=${encodeURIComponent(node)}&address=${address}`;

const $ = (id) => document.getElementById(id);

/// Hands control back to the event loop — so a WebSocket message can be
/// delivered — and takes it straight back.
///
/// **Not `setTimeout(…, 0)`.** After five nested timers browsers clamp
/// `setTimeout` to 4 ms, which on a socket-backed run turns into 4 ms of dead
/// air per tick and reads as a slow controller. A `MessageChannel` port is
/// delivered as a task with no clamp, so the loop runs as fast as the socket
/// does. Measured against netsim: the clamp was costing more time than the
/// transfer itself.
const yieldToBrowser = (() => {
  const channel = new MessageChannel();
  let pending = null;
  channel.port1.onmessage = () => {
    const resolve = pending;
    pending = null;
    resolve?.();
  };
  return () =>
    new Promise((resolve) => {
      pending = resolve;
      channel.port2.postMessage(0);
    });
})();

/// Watches how long the event loop actually takes to come back.
///
/// A hidden or heavily loaded tab can be throttled to one callback per
/// second, and every socket-backed number measured in one is garbage —
/// quantised to whole seconds and an order of magnitude too slow. Rather
/// than guess from `document.hidden` (which is true whenever the window is
/// merely occluded, and would condemn runs that were never actually
/// delayed), the loop *measures* its own yields. A mean gap above
/// [`THROTTLED_YIELD_MS`] means the browser, not the controller, set the
/// pace, and the run is flagged, kept out of the aggregate, and marked on
/// the chart.
const THROTTLED_YIELD_MS = 20;

function yieldWatch() {
  let count = 0;
  let total = 0;
  return {
    async yield() {
      const before = performance.now();
      await yieldToBrowser();
      total += performance.now() - before;
      count += 1;
    },
    /// `{ throttled, yields, mean_yield_ms }` — the verdict plus its evidence.
    verdict() {
      const mean = count ? total / count : 0;
      return {
        throttled: count > 0 && mean > THROTTLED_YIELD_MS,
        yields: count,
        mean_yield_ms: Number(mean.toFixed(2)),
      };
    },
  };
}

// --- the stored log ---------------------------------------------------------
//
// Every read and write is guarded: a private window throws on the first
// touch of localStorage, and the page has to render anyway. A measurement
// log that survives a reload is the point — comparing controllers is a
// session's worth of experiments, not one click.

function loadRuns() {
  try {
    const raw = localStorage.getItem(STORE_KEY);
    const parsed = raw ? JSON.parse(raw) : [];
    return Array.isArray(parsed) ? parsed : [];
  } catch (e) {
    return [];
  }
}

function saveRuns(runs) {
  try {
    localStorage.setItem(STORE_KEY, JSON.stringify(runs.slice(-MAX_RUNS)));
  } catch (e) {
    /* the runs still show; they just will not outlive the tab */
  }
}

// --- provenance -------------------------------------------------------------

/// What a controller is called on a chart row, and whether its numbers came
/// off a radio.
function provenance(controller, dongles) {
  if (controller === "usb") {
    // A phone driving another phone — no dongle in it at all. Named both ends,
    // source → sink, so it is its own series and reads as what it is.
    if (isPhone(dongles.central) && isPhone(dongles.sink)) {
      const src = phones.get(dongles.central);
      const dst = phones.get(dongles.sink);
      const from = src?.name || src?.model || "phone";
      const to = dst?.name || dst?.model || "phone";
      return {
        id: `${dongles.central}->${dongles.sink}`,
        label: `${from} → ${to}`,
        simulated: false,
      };
    }
    const which = dongles.central || "dongle";
    // The peer is the more interesting half when it is not ours: a run
    // against a phone and a run against a dongle are different measurements
    // and must not stack into one bar.
    if (isPhone(dongles.sink)) {
      // Named, so two phones are two series rather than one blurred average.
      const phone = phones.get(dongles.sink);
      const who = phone?.name || phone?.model || "phone";
      return { id: `${dongles.sink}:${which}`, label: `${who} via ${which}`, simulated: false };
    }
    return { id: `usb:${which}`, label: `usb ${which}`, simulated: false };
  }
  if (controller === "websocket") {
    return { id: "netsim", label: "netsim", simulated: true };
  }
  return { id: "in-page", label: "in browser", simulated: true };
}

// --- the drivers ------------------------------------------------------------
//
// One measurement, three places to run it. The Rust is identical in all
// three (`crate::device::throughput`); only where the packets go differs.

/// Runs the benchmark with both ends in this tab, over the simulated medium.
///
/// `pump` is given a slice of wall clock rather than a step count, so the
/// page stays responsive without the measurement becoming a measurement of
/// the browser's frame rate: the runner's own clock stamps every boundary
/// inside the slice.
async function runInPage(options, onStage) {
  const bench = new WebBulkBench(JSON.stringify(options));
  const giveUpAt = performance.now() + options.timeout_ms + 5000;
  let report = null;
  // An in-page run that finishes inside one `pump` never went round the
  // event loop at all, so it cannot have been throttled — which is why the
  // verdict counts yields rather than reading a visibility flag.
  const watch = yieldWatch();
  for (;;) {
    report = JSON.parse(bench.pump(60));
    if (bench.is_finished()) break;
    if (performance.now() > giveUpAt) {
      report.failure = report.failure || "the page gave up waiting for the run";
      report.phase = "failed";
      break;
    }
    onStage(`${report.phase} — ${report.bytes_sent} bytes out`);
    await watch.yield();
  }
  return { report, ...watch.verdict(), log: Array.from(bench.log()) };
}

/// Runs the benchmark against a phone: one dongle as the central, and a
/// peripheral we do not own and cannot address in advance.
///
/// Two things differ from every other run here, and both come from the peer
/// being somebody else's device. There is no `WebBulkSink` to tick, so the
/// end of the transfer is whatever the phone reports over the control point
/// rather than a count on our own clock — `peer-reported`, never
/// `server-stamped`. And the run has to *find* its peer: Android advertises
/// from a rotating resolvable private address that it will not disclose even
/// to its own app, so the address is discovered by service and never
/// configured.
async function runAgainstPhone(options, centralUrl, legacyMasks, onStage, statsBase, name) {
  // Zero the phone's counters before a byte moves. This is the out-of-band
  // twin of a BEGIN on the control point, and the reason the link can carry
  // payload and nothing else.
  onStage("resetting the phone's counters");
  try {
    await fetch(`${statsBase}/reset?total=${options.total_bytes}`, { cache: "no-store" });
  } catch (e) {
    return {
      report: {
        phase: "failed",
        failure: `SimBLE Android is not answering at ${statsBase} — `
          + `is the app running, and can the bridge reach the phone over WiFi?`,
      },
      log: [],
    };
  }
  let central = null;
  try {
    central = WebBulkCentral.discovering(
      centralUrl,
      JSON.stringify(options),
      legacyMasks,
      name || "",
    );
  } catch (e) {
    return {
      report: {
        phase: "failed",
        failure: `could not reach the controller: ${e?.message ?? e}`,
      },
      log: [],
    };
  }
  // The scan gets the run's own patience on top of the run's own timeout,
  // because finding the peer and transferring to it are separate waits.
  const giveUpAt = performance.now() + options.timeout_ms * 2 + 8000;
  let report = null;
  const watch = yieldWatch();
  try {
    for (;;) {
      report = JSON.parse(central.tick());
      if (central.is_finished()) break;
      if (performance.now() > giveUpAt) {
        report.failure = report.failure
          || (central.ready_state() === 1
            ? "the page gave up waiting for the run"
            : "the controller's socket never opened");
        report.phase = "failed";
        break;
      }
      onStage(
        report.phase === "discovering"
          ? "scanning for the phone"
          : `${report.phase} — ${report.bytes_sent ?? 0} sent`,
      );
      await watch.yield();
    }
    const log = Array.from(central.log());
    // -1 means this run was aimed at an address and never scanned.
    const scanned = central.scan_ms();
    if (scanned >= 0) report.scan_ms = scanned;
    // The measurement proper, on a path the run never touched.
    try {
      const sink = await (await fetch(`${statsBase}/stats`, { cache: "no-store" })).json();
      report.bytes_received = sink.bytes;
      report.chunks_received = sink.chunks;
      // Not `peer-reported`: nothing came back over the link. The count is the
      // phone's own, fetched over wifi, and the duration is measured end to
      // end on the phone's clock — a duration needs no agreement about epochs.
      report.confirmation = "http-reported";
      report.sink_duration_ms = sink.duration_ms;
      log.push(`the phone received ${sink.bytes} of ${options.total_bytes} bytes `
        + `in ${sink.chunks} chunks over ${sink.duration_ms} ms`);
      if (sink.bytes !== options.total_bytes) {
        log.push(`${options.total_bytes - sink.bytes} bytes lost`);
      }
    } catch (e) {
      log.push(`could not read the phone's counters: ${e?.message ?? e}`);
    }
    return { report, ...watch.verdict(), log };
  } finally {
    central?.free?.();
  }
}

/// A phone-to-phone run: one phone's own radio drives the transfer into
/// another, no dongle in the path. Neither end is ours to drive from the page —
/// the source phone's central role is an Android intent over adb, which only the
/// bridge can fire — so the page hands the bridge both serials and the byte
/// count, and the bridge runs it (the same sequence `bench-pair.sh` does) and
/// returns the four-segment breakdown its source shim traced. That breakdown is
/// the same shape a dongle run reports, so the chart draws it identically.
async function runPairToPhone(centralValue, sinkValue, options, onStage) {
  const source = centralValue.slice(PHONE_SINK.length + 1);
  const sink = sinkValue.slice(PHONE_SINK.length + 1);
  const http = usbBridgeUrl().trim().replace(/^ws/, "http").replace(/\/+$/, "");
  onStage("phone-to-phone over adb — discovery can take ~30 s");
  let res;
  try {
    const url = `${http}/pair-run?source=${encodeURIComponent(source)}`
      + `&sink=${encodeURIComponent(sink)}&bytes=${options.total_bytes}`
      + `&fast=${options.fast_link === false ? 0 : 1}`;
    res = await (await fetch(url, { cache: "no-store" })).json();
  } catch (e) {
    return {
      report: { phase: "failed", failure: `the bridge could not run the pair: ${e?.message ?? e}` },
      log: [],
    };
  }
  const segs = {
    discover_ms: res.discover_ms ?? 0,
    connect_ms: res.connect_ms ?? 0,
    negotiate_ms: res.negotiate_ms ?? 0,
    transfer_ms: res.transfer_ms ?? 0,
  };
  if (!res.ok) {
    return {
      report: { phase: "failed", failure: res.error || "the phone-to-phone run did not complete", ...segs },
      log: res.error ? [res.error] : [],
    };
  }
  const report = {
    phase: "complete",
    ...segs,
    total_ms: segs.discover_ms + segs.connect_ms + segs.negotiate_ms + segs.transfer_ms,
    throughput_kb_s: res.throughput_kb_s,
    bytes_sent: res.expected,
    bytes_received: res.bytes,
    // The MTU the source negotiated and the PHY it settled on — so a run that
    // silently stayed on 1M reads as such rather than "not reported".
    mtu: res.mtu || null,
    tx_phy: res.tx_phy ?? null,
    rx_phy: res.rx_phy ?? null,
    // The sink counted the bytes and reported them back over the control point;
    // the duration is the source's own transfer clock. Not stamped on our clock,
    // so: peer-reported.
    confirmation: "peer-reported",
  };
  return {
    report,
    log: [`${res.name}: ${res.bytes} of ${res.expected} bytes — `
      + `discover ${segs.discover_ms} ms, connect ${segs.connect_ms} ms, `
      + `negotiate ${segs.negotiate_ms} ms, transfer ${segs.transfer_ms} ms `
      + `(${res.throughput_kb_s} kB/s)`],
  };
}

/// Runs the benchmark across two sockets: a sink on one controller, a
/// central on another. netsim gives each a device on the shared scene; the
/// `simble --usb` bridge gives each a physical dongle.
///
/// The sink is ticked first and its counters handed to the central before
/// the central produces its report, so the transfer segment ends at the
/// moment bytes *arrived* rather than at the central's last queued write.
async function runOverSockets(options, urls, legacyMasks, onStage) {
  let sink = null;
  let central = null;
  try {
    sink = new WebBulkSink(urls.sink, SINK_ADDR, legacyMasks);
    central = new WebBulkCentral(
      urls.central,
      SINK_ADDR,
      JSON.stringify(options),
      legacyMasks,
    );
  } catch (e) {
    sink?.free?.();
    central?.free?.();
    return {
      report: {
        phase: "failed",
        failure: `could not reach the controller: ${e?.message ?? e}`,
      },
      log: [],
    };
  }
  const giveUpAt = performance.now() + options.timeout_ms + 8000;
  let report = null;
  const watch = yieldWatch();
  try {
    for (;;) {
      sink.tick();
      central.note_server(sink.bytes(), sink.chunks(), sink.last_byte_ms());
      report = JSON.parse(central.tick());
      if (central.is_finished()) break;
      if (performance.now() > giveUpAt) {
        report.failure = report.failure
          || (central.ready_state() === 1
            ? "the page gave up waiting for the run"
            : "the controller's socket never opened");
        report.phase = "failed";
        break;
      }
      onStage(`${report.phase} — ${report.bytes_sent ?? 0} sent, ${sink.bytes()} in`);
      await watch.yield();
    }
    return { report, ...watch.verdict(), log: Array.from(central.log()) };
  } finally {
    sink.free?.();
    central.free?.();
  }
}

// --- the chart --------------------------------------------------------------

const SVG_NS = "http://www.w3.org/2000/svg";
const svgEl = (name, attrs = {}) => {
  const node = document.createElementNS(SVG_NS, name);
  for (const [key, value] of Object.entries(attrs)) node.setAttribute(key, value);
  return node;
};

/// Every segment before the transfer: scan, discover, connect, negotiate.
const setupMs = (run) =>
  PHASES.filter((phase) => phase.key !== "transfer_ms").reduce(
    (total, phase) => total + (run[phase.key] || 0),
    0,
  );

const fmtSeconds = (ms) => (ms >= 1000 ? `${(ms / 1000).toFixed(2)} s` : `${ms.toFixed(0)} ms`);
/// A simulated link moves tens of megabytes a second, and five digits of
/// kB/s is a number nobody reads. Above a megabyte, say megabytes.
const fmtRate = (kbs) => {
  if (kbs == null) return "—";
  return kbs >= 1024 ? `${(kbs / 1024).toFixed(1)} MB/s` : `${kbs.toFixed(0)} kB/s`;
};

/// Mean, median, min and max of a list, ignoring anything missing.
function summarise(values) {
  const clean = values.filter((v) => typeof v === "number" && isFinite(v));
  if (!clean.length) return null;
  const sorted = [...clean].sort((a, b) => a - b);
  const middle = sorted.length >> 1;
  return {
    mean: clean.reduce((a, b) => a + b, 0) / clean.length,
    median: sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2,
    min: sorted[0],
    max: sorted[sorted.length - 1],
    n: clean.length,
  };
}

/// The rows the chart draws: every run, grouped by controller, each group
/// followed by its aggregate. One aggregate per controller rather than one
/// for everything, because averaging a dongle together with a simulation
/// would produce a number describing nothing.
function chartRows(runs) {
  // Grouped by controller *and* write mode. Averaging an acknowledged run
  // together with an unacknowledged one produces a number describing
  // neither, and the two differ by more than any controller does.
  const groups = new Map();
  for (const run of runs) {
    const key = `${run.controller}|${run.with_response ? "req" : "cmd"}`;
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(run);
  }
  const rows = [];
  for (const [key, list] of groups) {
    for (const run of list) rows.push({ kind: "run", run });
    const finished = list.filter((r) => r.complete && !r.throttled);
    if (finished.length < 2) continue;
    const totals = summarise(finished.map((r) => r.total_ms));
    rows.push({
      kind: "aggregate",
      controller: key,
      label: `${list[0].controllerLabel} · write ${list[0].with_response ? "req" : "cmd"}`,
      mode: list[0].with_response ? "req" : "cmd",
      simulated: list[0].simulated,
      n: finished.length,
      failures: list.length - finished.length,
      totals,
      rate: summarise(finished.map((r) => r.throughput_kb_s)),
      phases: Object.fromEntries(
        PHASES.map((phase) => [phase.key, summarise(finished.map((r) => r[phase.key]))]),
      ),
    });
  }
  return rows;
}

const ROW_H = 22;
const LABEL_W = 132;
const NUM_W = 168;
const TOP = 26;

/// Draws the waterfall. One shared time axis across every row — per-bar
/// autoscaling would make a slow controller look identical to a fast one,
/// which is the one thing this chart exists not to do.
function renderChart(host, runs) {
  host.innerHTML = "";
  if (!runs.length) {
    host.append(
      Object.assign(document.createElement("p"), {
        className: "empty",
        textContent: "No runs yet. Pick a controller, press Run, and each transfer lands here as a bar.",
      }),
    );
    return;
  }
  const rows = chartRows(runs);
  const width = host.clientWidth || 820;
  const plotW = Math.max(220, width - LABEL_W - NUM_W - 12);
  const scaleMax = Math.max(
    1,
    ...runs.map((r) => r.total_ms || sumPhases(r) || 0),
  );
  const height = TOP + rows.length * ROW_H + 12;
  const svg = svgEl("svg", {
    viewBox: `0 0 ${width} ${height}`,
    width: "100%",
    height,
    role: "img",
    "aria-label": "one stacked bar per run: discover, connect, negotiate, transfer",
  });

  // A hatch for anything the receiver did not confirm. Solid means measured;
  // striped means claimed.
  const defs = svgEl("defs");
  const hatch = svgEl("pattern", {
    id: "unconfirmed-hatch",
    width: "6",
    height: "6",
    patternUnits: "userSpaceOnUse",
    patternTransform: "rotate(45)",
  });
  hatch.append(svgEl("rect", { width: "6", height: "6", fill: "#ffffff" }));
  hatch.append(svgEl("rect", { width: "3", height: "6", fill: "#1a7f37", opacity: "0.75" }));
  defs.append(hatch);
  svg.append(defs);

  // Axis: ticks in seconds along the top, so bars are read against one ruler.
  const x = (ms) => LABEL_W + (ms / scaleMax) * plotW;
  const step = niceStep(scaleMax);
  for (let t = 0; t <= scaleMax + 1e-6; t += step) {
    svg.append(
      svgEl("line", {
        x1: x(t), x2: x(t), y1: TOP - 8, y2: height - 10,
        stroke: "#d0d7de", "stroke-width": t === 0 ? 1 : 0.5,
      }),
    );
    const label = svgEl("text", {
      x: x(t), y: TOP - 12, "text-anchor": "middle",
      "font-size": "10", fill: "#656d76",
    });
    // An axis that reads "0.0s 0.0s 0.0s" is not an axis. The unit follows
    // the tick spacing, not a fixed choice: a simulated run is milliseconds
    // end to end and a real one is seconds.
    label.textContent = t === 0
      ? "0"
      : step >= 1000
        ? `${(t / 1000).toFixed(0)}s`
        : step >= 100
          ? `${(t / 1000).toFixed(1)}s`
          : `${t.toFixed(0)}ms`;
    svg.append(label);
  }

  rows.forEach((row, index) => {
    const y = TOP + index * ROW_H;
    svg.append(row.kind === "run" ? runBar(row.run, y, x) : aggregateBar(row, y, x));
  });
  host.append(svg);
}

/// A round-ish tick spacing that yields five to ten gridlines.
function niceStep(maxMs) {
  const raw = maxMs / 6;
  const magnitude = 10 ** Math.floor(Math.log10(raw));
  for (const multiple of [1, 2, 2.5, 5, 10]) {
    if (raw <= magnitude * multiple) return magnitude * multiple;
  }
  return magnitude * 10;
}

const sumPhases = (run) =>
  PHASES.reduce((total, phase) => total + (run[phase.key] || 0), 0);

/// One run: its segments end to end, its provenance chip, its two headline
/// numbers.
function runBar(run, y, x) {
  const group = svgEl("g");
  const chip = run.simulated ? "sim" : "RF";
  const label = svgEl("text", {
    x: 0, y: y + 15, "font-size": "11", fill: "#1f2328",
  });
  label.textContent = `${chip}  ${run.controllerLabel}${run.throttled ? " ⏱" : ""}`;
  label.setAttribute("fill", run.simulated ? "#656d76" : "#9a6700");
  group.append(label);

  let cursor = 0;
  for (const phase of PHASES) {
    const value = run[phase.key];
    if (!(value > 0)) continue;
    const unconfirmedTail =
      phase.key === "transfer_ms" && run.confirmation === "unconfirmed";
    const rect = svgEl("rect", {
      x: x(cursor), y: y + 4, width: Math.max(1, x(cursor + value) - x(cursor)),
      height: 14, fill: unconfirmedTail ? "url(#unconfirmed-hatch)" : phase.fill,
      stroke: unconfirmedTail ? phase.fill : "none", "stroke-width": "1",
    });
    const title = svgEl("title");
    title.textContent = `${phase.label}: ${fmtSeconds(value)}`;
    rect.append(title);
    group.append(rect);
    cursor += value;
  }
  if (!run.complete) {
    // A failure is drawn where it stopped, with a cross: an empty chart and
    // a chart of failures must not look the same.
    const mark = svgEl("text", {
      x: x(cursor) + 6, y: y + 15, "font-size": "12", fill: "#cf222e",
    });
    mark.textContent = "✗";
    const title = svgEl("title");
    title.textContent = run.failure || "the run did not finish";
    mark.append(title);
    group.append(mark);
  }

  group.append(numbersFor(run, y));
  return group;
}

/// The two headline numbers to the right of a bar: how long until it landed,
/// and the pipe width over the transfer segment only.
function numbersFor(run, y) {
  const group = svgEl("g");
  const total = svgEl("text", {
    x: "100%", y: y + 15, "text-anchor": "end", "font-size": "11",
    fill: run.complete ? "#1f2328" : "#cf222e",
  });
  total.setAttribute("transform", "translate(-6,0)");
  const rate = run.confirmation === "unconfirmed" ? `${fmtRate(run.throughput_kb_s)}*` : fmtRate(run.throughput_kb_s);
  // "stopped in failed" says nothing. The useful fact is which segment it
  // died in, which is the last one that got a duration at all.
  const reached = PHASES.filter((phase) => run[phase.key] != null).pop();
  total.textContent = run.complete
    ? `${fmtSeconds(run.total_ms)}   ${rate}`
    : `stopped in ${reached ? reached.label : "setup"}`;
  group.append(total);
  return group;
}

/// The aggregate: mean segments, min/max whiskers on the total, and the
/// median printed beside the mean so a long tail is visible rather than
/// averaged away.
function aggregateBar(row, y, x) {
  const group = svgEl("g");
  const label = svgEl("text", {
    x: 0, y: y + 15, "font-size": "11", "font-weight": "600", fill: "#1f2328",
  });
  // The mode belongs in the narrow left column: the numbers column is only
  // as wide as `NUM_W`, and a label spliced in there ran under the whiskers.
  label.textContent = `mean of ${row.n} · ${row.mode}`;
  const which = svgEl("title");
  which.textContent = row.label;
  label.append(which);
  group.append(label);

  let cursor = 0;
  for (const phase of PHASES) {
    const stat = row.phases[phase.key];
    if (!stat || !(stat.mean > 0)) continue;
    const rect = svgEl("rect", {
      x: x(cursor), y: y + 3, width: Math.max(1, x(cursor + stat.mean) - x(cursor)),
      height: 16, fill: phase.fill, opacity: "0.85",
    });
    const title = svgEl("title");
    title.textContent =
      `${phase.label}: mean ${fmtSeconds(stat.mean)}, median ${fmtSeconds(stat.median)}, ` +
      `min ${fmtSeconds(stat.min)}, max ${fmtSeconds(stat.max)}`;
    rect.append(title);
    group.append(rect);
    cursor += stat.mean;
  }
  if (row.totals) {
    // Whiskers on the total: the fastest and slowest of these runs. The gap
    // between them is the tail the mean hides.
    const mid = y + 11;
    group.append(
      svgEl("line", {
        x1: x(row.totals.min), x2: x(row.totals.max), y1: mid, y2: mid,
        stroke: "#1f2328", "stroke-width": "1",
      }),
    );
    for (const edge of [row.totals.min, row.totals.max]) {
      group.append(
        svgEl("line", {
          x1: x(edge), x2: x(edge), y1: mid - 5, y2: mid + 5,
          stroke: "#1f2328", "stroke-width": "1",
        }),
      );
    }
  }
  const numbers = svgEl("text", {
    x: "100%", y: y + 15, "text-anchor": "end", "font-size": "11",
    "font-weight": "600", fill: "#1f2328", transform: "translate(-6,0)",
  });
  numbers.textContent = row.totals
    ? `${fmtSeconds(row.totals.mean)} mean · ${fmtSeconds(row.totals.median)} median`
    : "";
  group.append(numbers);
  if (row.failures) {
    const note = svgEl("text", {
      x: LABEL_W - 8, y: y + 15, "text-anchor": "end", "font-size": "10", fill: "#cf222e",
    });
    note.textContent = `+${row.failures}✗`;
    group.append(note);
  }
  return group;
}

// --- the run table ----------------------------------------------------------

/// Sent versus received, per run. If the two differ that is the single most
/// interesting fact the benchmark can produce, so it gets its own column
/// rather than a tooltip.
function renderTable(host, runs) {
  host.innerHTML = "";
  if (!runs.length) return;
  const table = document.createElement("table");
  table.className = "runs";
  table.innerHTML =
    "<thead><tr><th>when</th><th>controller</th><th>mode</th><th>MTU</th><th>PHY</th>" +
    "<th>sent</th><th>received</th><th>setup</th><th>total</th><th>transfer</th>" +
    "<th>confirmation</th>" +
    "<th>event loop</th></tr></thead>";
  const body = document.createElement("tbody");
  for (const run of [...runs].reverse()) {
    const row = document.createElement("tr");
    if (!run.complete) row.className = "failed";
    const received = run.bytes_received == null ? "—" : String(run.bytes_received);
    const short = (n) => (n == null ? "—" : `${(n / 1024).toFixed(0)} KB`);
    const cells = [
      new Date(run.at).toLocaleTimeString(),
      `${run.simulated ? "sim" : "RF"} · ${run.controllerLabel}`,
      run.with_response ? "write req" : "write cmd",
      run.mtu ?? "—",
      run.tx_phy ?? "not reported",
      short(run.bytes_sent),
      run.bytes_received != null && run.bytes_received !== run.bytes_sent
        ? `${short(run.bytes_received)} ⚠`
        : short(run.bytes_received),
      // Everything before a byte moves. On a small transfer this *is* the
      // run — 2.5 s of it against a phone, which no per-phase tooltip makes
      // obvious at a glance.
      setupMs(run) ? fmtSeconds(setupMs(run)) : "—",
      run.complete ? fmtSeconds(run.total_ms) : (run.failure || "failed"),
      run.transfer_ms ? fmtSeconds(run.transfer_ms) : "—",
      run.confirmation ?? "—",
      run.throttled
        ? `throttled — ${run.mean_yield_ms} ms per yield`
        : run.yields
          ? `${run.mean_yield_ms} ms per yield`
          : "one slice",
    ];
    for (const value of cells) {
      const cell = document.createElement("td");
      cell.textContent = String(value);
      row.append(cell);
    }
    if (run.bytes_received != null && run.bytes_received !== run.bytes_sent) {
      row.classList.add("lossy");
    }
    if (run.throttled) row.classList.add("throttled");
    body.append(row);
  }
  table.append(body);
  host.append(table);
}

// --- CSV ---------------------------------------------------------------------

const CSV_COLUMNS = [
  "at", "controller", "controllerLabel", "simulated", "complete", "phase", "failure",
  "throttled", "yields", "mean_yield_ms",
  "requested_bytes", "bytes_sent", "bytes_received", "chunks_sent", "chunks_received",
  "chunk_bytes", "mtu", "tx_phy", "rx_phy", "with_response", "window_chunks", "acl_credits",
  "discover_ms", "connect_ms", "negotiate_ms", "transfer_ms", "total_ms",
  "throughput_kb_s", "confirmation",
];

function toCsv(runs) {
  const escape = (value) => {
    const text = value == null ? "" : String(value);
    return /[",\n]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
  };
  return [
    CSV_COLUMNS.join(","),
    ...runs.map((run) => CSV_COLUMNS.map((key) => escape(run[key])).join(",")),
  ].join("\n");
}

// --- the category ------------------------------------------------------------

/// Builds the Data category inside `root` and returns nothing: the page hosts
/// exactly one of these and never tears it down.
export function mountData(root) {
  let runs = loadRuns();
  let running = false;
  const dongles = { central: "", sink: "" };

  // Appended rather than assigned: the panel already carries the prose that
  // introduces the category, and this is the instrument below it.
  const panel = document.createElement("div");
  root.append(panel);
  panel.innerHTML = `
    <div id="data-controller"></div>
    <div id="data-strips"></div>
    <div class="settings" id="data-settings">
      <label>Transfer size
        <select id="bench-size">
          <option value="16384">16 KB</option>
          <option value="32768">32 KB</option>
          <option value="65536">64 KB</option>
          <option value="262144" selected>256 KB</option>
          <option value="1048576">1 MB</option>
        </select>
      </label>
      <label>Runs
        <select id="bench-runs">
          <option value="1">1</option>
          <option value="5">5</option>
          <option value="10" selected>10</option>
          <option value="20">20</option>
          <option value="30">30</option>
        </select>
      </label>
      <label>Write
        <select id="bench-mode">
          <option value="cmd" selected>without response</option>
          <option value="req">with response</option>
        </select>
      </label>
      <label>Fast link
        <select id="bench-fast">
          <option value="on" selected>on</option>
          <option value="off">off</option>
        </select>
      </label>
      <p class="settings-note">
        Fast link requests 2M&nbsp;PHY, Data Length Extension, and a tight connection interval on
        connect; off leaves the 1M, 27-octet baseline — the setup cost the fast path buys down, side by
        side with it. PHY, interval and MTU are not yet settable one at a time, and advertising interval
        belongs to the peripheral, not the central. What the run <em>negotiated</em> is reported per run
        in the table.
      </p>
    </div>
    <div class="toolbar">
      <button id="bench-run" class="primary">▶ Run</button>
      <span id="bench-pill" class="pill">idle</span>
      <span id="bench-stage" class="stage"></span>
      <span class="spacer"></span>
      <button id="bench-copy-json">Copy JSON</button>
      <button id="bench-copy-csv">Copy CSV</button>
      <button id="bench-clear">Clear log</button>
    </div>
    <div class="legend" id="bench-legend"></div>
    <div class="chart" id="bench-chart"></div>
    <p class="honesty" id="bench-honesty"></p>
    <div class="table-wrap" id="bench-table"></div>
    <details class="raw"><summary>Run log</summary><pre id="bench-log"></pre></details>
  `;

  // The controller choice is the shell's, not this page's: the same bar the
  // Devices pages use, writing the same stored preference.
  const bar = createControllerBar({
    supports: SUPPORTS,
    onChange: () => refreshStrips(),
  });
  $("data-controller").append(bar.el);
  bar.el.classList.add("standalone");

  // Which silicon each end rides is the one question USB raises that the bar
  // does not answer, and it is per-device — so it is a strip, not a second bar.
  const centralStrip = createControllerStrip({
    value: { kind: "usb", device: "" },
    onChange: (value) => {
      dongles.central = value.device;
      // One radio cannot be both ends of a link, so grey this choice out in the
      // sink strip rather than let it be picked there and then refused.
      sinkStrip.setDisabled(value.device);
      return null;
    },
    why: "the central: it does the writing",
  });
  const sinkStrip = createControllerStrip({
    value: { kind: "usb", device: "" },
    extras: [],
    onChange: (value) => {
      dongles.sink = value.device;
      centralStrip.setDisabled(value.device);
      sinkStrip.setWhy(sinkWhy(value.device));
      return null;
    },
    why: "the peripheral: it counts what lands",
  });

  $("data-strips").append(centralStrip.el, sinkStrip.el);

  async function refreshStrips() {
    const usb = bar.selected === "usb";
    $("data-strips").hidden = !usb;
    if (!usb) return;
    try {
      const { devices } = await (await fetch(`${usbBridgeHttp()}/devices`)).json();
      const list = Array.isArray(devices) ? devices : [];
      centralStrip.setDongles(list);
      sinkStrip.setDongles(list);

      // Phones come from the same bridge for the same reason dongles do: a
      // page cannot run adb, and an https page cannot reach a phone's LAN ip
      // itself, but it can read a list from the bridge and fetch each phone's
      // counters back through the bridge's `/sink/<ip>` proxy.
      phones.clear();
      let found = [];
      try {
        const answer = await (await fetch(`${usbBridgeHttp()}/phones`)).json();
        found = Array.isArray(answer.phones) ? answer.phones : [];
      } catch (e) {
        found = [];
      }
      const extras = found.map((phone) => {
        const value = phoneValue(phone.serial);
        phones.set(value, phone);
        return {
          value,
          // Named by what it advertises, because that is what the scan
          // matches on; the model is the human's handle on which desk it is.
          text: phone.running
            ? `${phone.name || phone.model} — SimBLE on ${phone.host}:8099`
            : `${phone.model} — SimBLE not running`,
        };
      });
      sinkStrip.setExtras(extras);
      // A phone can be the central too, now that the app has a source role: the
      // bridge drives it over adb (a phone → phone run). So the central strip
      // offers the same phones as the sink strip.
      centralStrip.setExtras(extras);
      if (!dongles.central && list[0]) {
        dongles.central = list[0].selector;
        centralStrip.set({ kind: "usb", device: dongles.central });
      }
      // Two ends, two radios: default the sink to a *different* dongle, since
      // one controller cannot be both halves of a link.
      if (dongles.sink !== PHONE_SINK && !dongles.sink && list.length) {
        dongles.sink = list[Math.min(1, list.length - 1)].selector;
        sinkStrip.set({ kind: "usb", device: dongles.sink });
      }
      // One radio cannot be both ends: disable each end's current pick in the
      // other strip's list.
      centralStrip.setDisabled(dongles.sink);
      sinkStrip.setDisabled(dongles.central);
      if (isPhone(dongles.sink)) {
        sinkStrip.setWhy(sinkWhy(dongles.sink));
      } else if (list.length < 2) {
        sinkStrip.setWhy("the bridge sees fewer than two dongles — a real-RF run needs one for each end");
      }
    } catch (e) {
      centralStrip.setWhy("the bridge is not answering — see the controller bar");
    }
  }

  // The legend names the four segments, and says out loud what the fills and
  // the chips mean. A chart that distinguishes real radio from simulation
  // only in a tooltip is a chart that will be screenshotted misleadingly.
  const legend = $("bench-legend");
  for (const phase of PHASES) {
    const item = document.createElement("span");
    item.className = "legend-item";
    const swatch = document.createElement("i");
    swatch.style.background = phase.fill;
    item.append(swatch, document.createTextNode(phase.label));
    legend.append(item);
  }
  const marks = document.createElement("span");
  marks.className = "legend-marks";
  marks.innerHTML =
    '<b class="sim">sim</b> simulated link · <b class="rf">RF</b> real radio · ' +
    "striped transfer + <b>*</b> = sent, not confirmed delivered · ✗ = the run stopped there · " +
    "⏱ = the browser throttled the event loop during this run, so its timings measure the tab, not the controller";
  legend.append(marks);

  function redraw() {
    renderChart($("bench-chart"), runs);
    renderTable($("bench-table"), runs);
    const simulated = runs.filter((r) => r.simulated).length;
    $("bench-honesty").textContent = runs.length
      ? `${runs.length} run(s) stored: ${simulated} on a simulated link (in browser or netsim — ` +
        "these time simble's own stack and a simulated medium, not radio), " +
        `${runs.length - simulated} on real RF — a dongle, or a phone through one.`
      : "";
  }

  function setPill(text, kind) {
    const pill = $("bench-pill");
    pill.className = `pill${kind ? ` ${kind}` : ""}`;
    pill.textContent = text;
  }

  function settings() {
    return {
      total_bytes: Number($("bench-size").value),
      with_response: $("bench-mode").value === "req",
      fast_link: $("bench-fast").value === "on",
      timeout_ms: 15000,
    };
  }

  /// One run on the chosen controller, wherever that is.
  async function runOnce(options, onStage) {
    const controller = bar.selected;
    if (controller === "in-page") return runInPage(options, onStage);
    if (controller === "websocket") {
      return runOverSockets(
        options,
        {
          sink: netsimUrl("bulk-sink", SINK_ADDR),
          central: netsimUrl("bulk-central", CENTRAL_ADDR),
        },
        false,
        onStage,
      );
    }
    const base = usbBridgeUrl().trim().replace(/\/+$/, "");
    if (!dongles.central || !dongles.sink) {
      return {
        report: { phase: "failed", failure: "pick a dongle for each end above" },
        log: [],
      };
    }
    // A phone as the central is a phone-to-phone run: the source phone's own
    // radio drives the transfer, no dongle in the path. The page cannot start
    // an Android intent over adb, so the bridge does — see runPairToPhone.
    if (isPhone(dongles.central)) {
      if (!isPhone(dongles.sink)) {
        return {
          report: {
            phase: "failed",
            failure: "a phone as the central needs a phone as the sink — pick a phone for the peripheral too",
          },
          log: [],
        };
      }
      if (dongles.central === dongles.sink) {
        return {
          report: {
            phase: "failed",
            failure: "source and destination are the same phone — one radio cannot drive itself",
          },
          log: [],
        };
      }
      return runPairToPhone(dongles.central, dongles.sink, options, onStage);
    }
    if (isPhone(dongles.sink)) {
      const phone = phones.get(dongles.sink);
      if (phone && !phone.running) {
        return {
          report: {
            phase: "failed",
            failure: `SimBLE Android is not running on ${phone.model} (${phone.serial})`,
          },
          log: [],
        };
      }
      return runAgainstPhone(
        // The link carries payload only: a FINISH/REPORT exchange costs air
        // time on the link under test and ends the measured transfer a round
        // trip late, and a broken-link run could never deliver it at all.
        { ...options, use_control_point: false },
        `${base}/?device=${encodeURIComponent(dongles.central)}`,
        true,
        onStage,
        // Counters read back through the bridge's /sink/<ip> proxy — derived
        // from the chosen phone, not a field to fill in.
        `${usbBridgeHttp()}/sink/${phone?.host ?? ""}`,
        phone?.name ?? "",
      );
    }
    if (dongles.central === dongles.sink) {
      return {
        report: {
          phase: "failed",
          failure: "both ends are the same dongle — one radio cannot be both halves of a link",
        },
        log: [],
      };
    }
    // A dongle of unknown vintage refuses the wide LE event mask outright and
    // then reports nothing at all, so the real-RF path asks for the 4.0 one.
    return runOverSockets(
      options,
      {
        sink: `${base}/?device=${encodeURIComponent(dongles.sink)}`,
        central: `${base}/?device=${encodeURIComponent(dongles.central)}`,
      },
      true,
      onStage,
    );
  }

  async function runSeries() {
    if (running) return;
    running = true;
    $("bench-run").disabled = true;
    const options = settings();
    const iterations = Number($("bench-runs").value);
    const where = provenance(bar.selected, dongles);
    const lines = [];
    for (let i = 0; i < iterations; i += 1) {
      setPill(`run ${i + 1} of ${iterations}`, null);
      const { report, throttled, yields, mean_yield_ms, log } = await runOnce(options, (stage) => {
        $("bench-stage").textContent = stage;
      });
      lines.push(`--- run ${i + 1} on ${where.label} ---`, ...log);
      runs.push({
        at: new Date().toISOString(),
        controller: where.id,
        controllerLabel: where.label,
        simulated: where.simulated,
        complete: report.phase === "complete",
        throttled: Boolean(throttled),
        yields: yields ?? 0,
        mean_yield_ms: mean_yield_ms ?? 0,
        ...report,
      });
      if (runs.length > MAX_RUNS) runs = runs.slice(-MAX_RUNS);
      saveRuns(runs);
      redraw();
      await yieldToBrowser();
    }
    $("bench-log").textContent = lines.join("\n");
    const failures = runs.slice(-iterations).filter((r) => !r.complete).length;
    setPill(failures ? `${failures} of ${iterations} failed` : "done", failures ? "bad" : "ok");
    $("bench-stage").textContent = "";
    $("bench-run").disabled = false;
    running = false;
  }

  $("bench-run").addEventListener("click", runSeries);
  $("bench-clear").addEventListener("click", () => {
    runs = [];
    saveRuns(runs);
    $("bench-log").textContent = "";
    setPill("idle", null);
    redraw();
  });
  const copy = (text, button) => {
    navigator.clipboard?.writeText(text).then(
      () => {
        const was = button.textContent;
        button.textContent = "copied";
        setTimeout(() => {
          button.textContent = was;
        }, 1200);
      },
      () => {
        /* clipboard refused: the raw log below is still selectable */
      },
    );
  };
  $("bench-copy-json").addEventListener("click", (e) =>
    copy(JSON.stringify(runs, null, 2), e.currentTarget));
  $("bench-copy-csv").addEventListener("click", (e) => copy(toCsv(runs), e.currentTarget));

  // The chart is laid out against the container's pixel width, so it has to
  // be redrawn when that changes.
  let resizeTimer = 0;
  window.addEventListener("resize", () => {
    clearTimeout(resizeTimer);
    resizeTimer = setTimeout(redraw, 150);
  });

  refreshStrips();
  redraw();
}
