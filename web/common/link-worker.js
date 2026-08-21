// SimBLE cross-tab SharedWorker.
//
// This is a *module* SharedWorker (loaded as
//   new SharedWorker("../common/link-worker.js", { type: "module" })).
// It owns exactly ONE wasm `WebLink` shared by every same-origin tab of the
// site. Because a SharedWorker has a single instance per origin, all tabs that
// connect to it add their devices to the *same* in-page BLE scene — so a
// scanner opened in tab A sees an advertiser added in tab B, a central in one
// tab connects to a peripheral in another, and so on. No netsim, no server: the
// whole radio scene lives in this worker's wasm heap.
//
// ---------------------------------------------------------------------------
// PROTOCOL (JSON messages over each tab's MessagePort)
//
// tab -> worker:
//   { cmd: "addPeripheral", address, script }   add a scripted peripheral
//   { cmd: "addScanner",    address }           add a passive scanner
//   { cmd: "addCentral",    address, target }   add a central that connects to `target`
//   { cmd: "ping" }                             liveness heartbeat (see below)
//   { cmd: "bye" }                              tab is going away (best-effort)
//
// worker -> tab:
//   { type: "hello", ready }                    sent immediately on connect
//   { type: "ready" }                           broadcast once wasm+Link are up
//   { type: "fatal", message }                  wasm/Link failed to initialise
//   { type: "added", role, index, address, target }   an add() succeeded
//   { type: "error", cmd, message }             an add() threw (e.g. bad script)
//   { type: "status", t, peripherals, scanners, centrals }
//        Periodic snapshot of the WHOLE shared scene, so every tab can render
//        every device. Each of peripherals/scanners/centrals is an object keyed
//        by the device's Link index; each value is that device's parsed status
//        with an extra `mine: true|false` flag telling the receiving tab whether
//        it owns that device. Peripheral/central values are the wasm
//        *_status_json objects; a scanner value is { address, mine, reports:[…] }.
//
// LIVENESS. A MessagePort has no reliable "the other side closed" event, so
// tabs send a { cmd:"ping" } heartbeat (~1/s). The worker stamps each port's
// lastSeen and a sweep drops ports silent for PING_TIMEOUT_MS. WebLink has no
// remove-device call, so a dropped tab's devices cannot be torn out of the wasm
// scene; we DO the next best thing: we stop reporting them, and we filter their
// addresses out of every scanner's reports so a gone advertiser visibly
// disappears from other tabs' scan lists. (Its packets still exist inside the
// Link and still cost a tick; only the *reporting* is suppressed. This is a
// documented limitation of the no-remove WebLink API.)
// ---------------------------------------------------------------------------

import init, { WebLink } from "../pkg/simble.js";

const TICK_MS = 100; // advance the shared Link's clock at ~10 Hz
const STATUS_MS = 250; // broadcast a scene snapshot 4×/second
const SWEEP_MS = 1000; // check for dead ports every second
const PING_TIMEOUT_MS = 4000; // a port silent this long is considered gone

const now = () => (typeof performance !== "undefined" ? performance.now() : Date.now());

let link = null;
let ready = false;
const startedAt = now();

// Connected tabs. portId -> { port, lastSeen, devices: [{ index, role, address }] }.
const ports = new Map();
let nextPortId = 1;

// Commands that arrived before wasm finished loading, replayed once ready.
const pending = [];

// Uppercased addresses of devices whose owning tab has gone away. Their
// advertising reports are filtered out of every scanner so they disappear.
const goneAddresses = new Set();

const elapsed = () => (now() - startedAt) / 1000;

// --- connection handling ----------------------------------------------------
// Registered synchronously (before any await) so connect events that queued
// while the module was evaluating are captured and, if needed, buffered.
self.onconnect = (event) => {
  const port = event.ports[0];
  const id = nextPortId++;
  ports.set(id, { port, lastSeen: now(), devices: [] });
  port.onmessage = (ev) => handleMessage(id, ev.data);
  port.onmessageerror = () => dropPort(id);
  // onmessage implicitly starts the port, but be explicit for clarity.
  port.start();
  post(id, { type: "hello", ready });
};

function post(id, message) {
  const entry = ports.get(id);
  if (!entry) return;
  try {
    entry.port.postMessage(message);
  } catch (_) {
    // The far side is gone; treat the port as dead.
    dropPort(id);
  }
}

function handleMessage(id, msg) {
  const entry = ports.get(id);
  if (!entry) return;
  entry.lastSeen = now(); // any message counts as a heartbeat
  if (!msg || typeof msg !== "object") return;

  switch (msg.cmd) {
    case "ping":
      return;
    case "bye":
      dropPort(id);
      return;
    case "addPeripheral":
    case "addScanner":
    case "addCentral":
      if (!ready) {
        pending.push({ id, msg }); // replayed in flushPending()
        return;
      }
      doAdd(id, msg);
      return;
    default:
      post(id, { type: "error", cmd: String(msg.cmd), message: "unknown command" });
  }
}

function doAdd(id, msg) {
  const entry = ports.get(id);
  if (!entry) return;
  try {
    let index;
    let role;
    const address = msg.address;
    if (msg.cmd === "addPeripheral") {
      index = link.add_peripheral(address, msg.script); // throws on script errors
      role = "peripheral";
    } else if (msg.cmd === "addScanner") {
      index = link.add_scanner(address);
      role = "scanner";
    } else {
      index = link.add_central(address, msg.target);
      role = "central";
    }
    entry.devices.push({ index, role, address });
    // If an address is reused after a previous owner left, it's live again.
    if (address) goneAddresses.delete(String(address).toUpperCase());
    post(id, { type: "added", role, index, address, target: msg.target });
  } catch (e) {
    post(id, { type: "error", cmd: msg.cmd, message: String((e && e.message) || e) });
  }
}

function dropPort(id) {
  const entry = ports.get(id);
  if (!entry) return;
  // Remember this tab's device addresses so scanners stop reporting them.
  for (const d of entry.devices) {
    if (d.address) goneAddresses.add(String(d.address).toUpperCase());
  }
  ports.delete(id);
}

function sweep() {
  const cutoff = now() - PING_TIMEOUT_MS;
  for (const [id, entry] of ports) {
    if (entry.lastSeen < cutoff) dropPort(id);
  }
}

// --- scene rendering --------------------------------------------------------
// Pull each live device's status out of the Link exactly once per broadcast,
// then fan the same snapshot out to every port (each annotated with `mine`).
function renderDevice(d, ownerId) {
  let value;
  try {
    if (d.role === "peripheral") {
      value = JSON.parse(link.peripheral_status_json(d.index));
    } else if (d.role === "central") {
      value = JSON.parse(link.central_status_json(d.index));
    } else {
      let reports = JSON.parse(link.scanner_reports_json(d.index));
      if (goneAddresses.size) {
        reports = reports.filter(
          (r) => !goneAddresses.has(String(r.address || "").toUpperCase())
        );
      }
      value = { address: d.address, reports };
    }
  } catch (e) {
    value = { error: String((e && e.message) || e) };
  }
  return { role: d.role, index: d.index, ownerId, value };
}

function broadcast() {
  if (!ready || ports.size === 0) return;

  // One pass over the whole scene: drains each scanner's report queue once.
  const rendered = [];
  for (const [ownerId, entry] of ports) {
    for (const d of entry.devices) rendered.push(renderDevice(d, ownerId));
  }

  for (const [id, entry] of ports) {
    const peripherals = {};
    const scanners = {};
    const centrals = {};
    for (const r of rendered) {
      const value = { ...r.value, mine: r.ownerId === id, index: r.index };
      if (r.role === "peripheral") peripherals[r.index] = value;
      else if (r.role === "scanner") scanners[r.index] = value;
      else centrals[r.index] = value;
    }
    try {
      entry.port.postMessage({ type: "status", t: elapsed(), peripherals, scanners, centrals });
    } catch (_) {
      dropPort(id);
    }
  }
}

function tick() {
  if (!ready) return;
  try {
    link.tick(elapsed());
  } catch (e) {
    // A single bad tick shouldn't kill the shared scene.
    // eslint-disable-next-line no-console
    console.error("link.tick:", e);
  }
}

function flushPending() {
  while (pending.length) {
    const { id, msg } = pending.shift();
    if (ports.has(id)) doAdd(id, msg);
  }
}

function broadcastAll(message) {
  for (const id of ports.keys()) post(id, message);
}

// --- boot -------------------------------------------------------------------
// Not a top-level await: the module body finishes synchronously (registering
// onconnect) and initialisation continues in the background, so no connect
// event is missed while wasm loads.
(async () => {
  try {
    await init(); // fetches ../pkg/simble_bg.wasm relative to this module
    link = new WebLink();
    ready = true;
    flushPending();
    broadcastAll({ type: "ready" });
    setInterval(tick, TICK_MS);
    setInterval(broadcast, STATUS_MS);
    setInterval(sweep, SWEEP_MS);
  } catch (e) {
    broadcastAll({ type: "fatal", message: String((e && e.message) || e) });
    // eslint-disable-next-line no-console
    console.error("link-worker init failed:", e);
  }
})();
