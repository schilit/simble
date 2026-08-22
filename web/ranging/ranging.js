// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// SimBLE Ranging: how far away is that tag?
//
// Two devices on one simulated radio — a tag advertising the Ranging Service
// and a locator that connects to it — measured two ways at once, against a
// truth only the radio knows.
//
// EVERY NUMBER ON THIS PAGE COMES OUT OF THE SIMULATED STACK. The page does
// not compute a distance. It sets a position on the medium and reads back
// status_json(); the RSSI in it arrived in a Command Complete for HCI Read
// RSSI, and the Channel Sounding tones arrived in LE CS Subevent Result
// events and in Ranging Data the tag notified over ATT. The only arithmetic
// here is turning those numbers into pixels.
//
// Mounted by the domain shell, or standalone by index.html:
//   mount(root)  builds the UI and starts the single timer
//   unmount()    clears the timer and drops the wasm scene

import init, { WebRanging } from "../pkg/simble.js";

const TAG_ADDRESS = "CC:1E:57:00:00:0A";
const LOCATOR_ADDRESS = "CC:1E:57:00:00:0B";

// 200 ms per tick: five Channel Sounding procedures a second, which is about
// what a real ranging session runs at, and slow enough that a reader can
// watch a single measurement land.
const TICK_MS = 200;

// The floor plan, in metres. The locator sits at the origin.
const PLAN = { minX: -2, maxX: 32, minY: -9, maxY: 9 };

// How many samples the error history keeps — about 24 seconds.
const HISTORY = 120;

// --- module state ----------------------------------------------------------

let scene = null;
let timer = null;
let styleEl = null;
let container = null;
let ui = {};
let history = [];
let dragging = false;

// --- lifecycle -------------------------------------------------------------

/** Builds the UI into `root` and starts the scene. */
export async function mount(root) {
  container = root;
  styleEl = document.createElement("style");
  styleEl.textContent = STYLE;
  document.head.appendChild(styleEl);
  root.innerHTML = MARKUP;
  cacheElements(root);
  wireControls();

  await init();
  scene = new WebRanging(TAG_ADDRESS, LOCATOR_ADDRESS);
  applyRoom();
  applyAssumptions();
  applyTagPosition();

  timer = setInterval(step, TICK_MS);
}

/** Tears everything down: the timer, the wasm scene, and the injected style. */
export function unmount() {
  if (timer !== null) { clearInterval(timer); timer = null; }
  if (scene) { try { scene.free(); } catch (_) { /* already gone */ } scene = null; }
  if (styleEl) { styleEl.remove(); styleEl = null; }
  if (container) { container.innerHTML = ""; container = null; }
  ui = {};
  history = [];
  dragging = false;
}

/** One timer callback: advance the scene, then render what it reports. */
function step() {
  if (!scene) return;
  scene.tick();
  const status = JSON.parse(scene.status_json());
  record(status);
  render(status);
}

// --- markup ----------------------------------------------------------------

const MARKUP = `
<section class="panel rg-span">
  <h2>What this page is doing</h2>
  <p class="rg-prose">
    Two simulated devices share one radio: a <b>tag</b> advertising the
    <b>Ranging Service</b> (<code>0x185B</code>), and a <b>locator</b> that
    connects to it. Drag the tag, or use the slider. The locator estimates how
    far away it is <b>two ways at once</b>, and neither method is told the
    answer.
  </p>
  <p class="rg-prose">
    <b>RSSI path loss</b> inverts a propagation model on the signal strength
    every Bluetooth receiver already reports. It needs no procedure and no
    cooperation from the peer — and it has to <em>guess</em> the transmitter's
    power and how lossy the room is. Get either wrong and the error is
    multiplicative.
  </p>
  <p class="rg-prose">
    <b>Channel Sounding</b> (Bluetooth 6.0) measures the <em>phase</em> of a
    tone on many frequencies. Phase rotates with frequency at a rate set by
    distance, so fitting that slope gives a range — a geometric measurement
    nothing in the room can attenuate. The catch is that each radio adds its
    own oscillator phase, redrawn on every hop, so <b>one end's measurements
    are noise</b>. The two ends' phases have to be added together, which is
    exactly why the tag ships its half back over the Ranging Service.
  </p>
</section>

<section class="panel">
  <h2>The tag — set the truth</h2>
  <svg id="rg-plan" viewBox="0 0 680 340" role="img"
       aria-label="floor plan with a draggable tag"></svg>
  <div class="row">
    <label for="rg-distance">distance</label>
    <input type="range" id="rg-distance" min="0.5" max="30" step="0.1" value="2">
    <span class="rg-truth" id="rg-truth">2.00 m</span>
  </div>
  <p class="hint">
    Drag the tag on the plan, or use the slider. Only the simulated medium
    knows this number; neither host can see it.
  </p>

  <h2 style="margin-top:1.25rem">The room — also unknown to the locator</h2>
  <div class="rg-controls">
    <label for="rg-exponent">path-loss exponent <span class="rg-val" id="rg-exponent-val"></span></label>
    <input type="range" id="rg-exponent" min="1.8" max="4" step="0.1" value="2.7">
    <label for="rg-shadow">shadowing σ <span class="rg-val" id="rg-shadow-val"></span></label>
    <input type="range" id="rg-shadow" min="0" max="8" step="0.5" value="3">
    <label for="rg-txpower">tag TX power <span class="rg-val" id="rg-txpower-val"></span></label>
    <input type="range" id="rg-txpower" min="-20" max="10" step="1" value="0">
  </div>
  <p class="hint">
    Free space is an exponent of 2.0; an ordinary room behaves like 2.7 or
    worse. Shadowing is the reflection-driven fading that makes a stationary
    device's RSSI wander.
  </p>
</section>

<section class="panel">
  <h2>The locator — two estimates</h2>
  <div class="rg-estimates">
    <div class="rg-card rg-rssi">
      <div class="rg-card-head">RSSI path loss</div>
      <div class="rg-big" id="rg-rssi-distance">—</div>
      <div class="rg-err" id="rg-rssi-error">—</div>
      <dl class="kv rg-kv">
        <dt>RSSI now</dt><dd id="rg-rssi-now">—</dd>
        <dt>mean of 16</dt><dd id="rg-rssi-mean">—</dd>
        <dt>jitter (σ)</dt><dd id="rg-rssi-jitter">—</dd>
        <dt>assumes 1 m =</dt><dd id="rg-rssi-ref">—</dd>
        <dt>assumes n =</dt><dd id="rg-rssi-exp">—</dd>
        <dt>single sample says</dt><dd id="rg-rssi-instant">—</dd>
      </dl>
    </div>
    <div class="rg-card rg-cs">
      <div class="rg-card-head">Channel Sounding</div>
      <div class="rg-big" id="rg-cs-distance">—</div>
      <div class="rg-err" id="rg-cs-error">—</div>
      <dl class="kv rg-kv">
        <dt>procedure</dt><dd id="rg-cs-state">—</dd>
        <dt>tones (local + peer)</dt><dd id="rg-cs-tones">—</dd>
        <dt>span</dt><dd id="rg-cs-band">—</dd>
        <dt>fit std. error</dt><dd id="rg-cs-stderr">—</dd>
        <dt>fit residual</dt><dd id="rg-cs-residual">—</dd>
        <dt>unambiguous to</dt><dd id="rg-cs-ambig">—</dd>
      </dl>
    </div>
  </div>
  <div class="rg-controls rg-assume">
    <label for="rg-assumed-exp">locator assumes exponent <span class="rg-val" id="rg-assumed-exp-val"></span></label>
    <input type="range" id="rg-assumed-exp" min="1.8" max="4" step="0.1" value="2">
    <label for="rg-assumed-ref">locator assumes 1 m RSSI <span class="rg-val" id="rg-assumed-ref-val"></span></label>
    <input type="range" id="rg-assumed-ref" min="-70" max="-25" step="1" value="-40">
  </div>
  <p class="hint">
    These two knobs are the locator's <em>guesses</em>, not the room. Match
    them to the truth on the left and the RSSI estimate improves — that is the
    calibration a beacon's TX-power field is trying to provide, and why it is
    never enough. The RSSI figure averages sixteen samples, so it takes a few
    seconds to catch up after the tag moves; the unsmoothed number under it
    reacts at once, and is far worse.
  </p>
</section>

<section class="panel rg-span">
  <h2>Why the tag has to send its half back</h2>
  <div class="rg-charts">
    <div>
      <svg id="rg-phase" viewBox="0 0 660 260" role="img"
           aria-label="measured phase against tone frequency"></svg>
      <p class="hint">
        <b>Top:</b> each tone's phase as one radio measured it — the locator's
        (<span class="rg-key rg-key-local">grey</span>) and the tag's
        (<span class="rg-key rg-key-remote">amber</span>). Scatter, both of
        them: each carries its own oscillator's phase, drawn fresh on every
        hop, so there is no distance to be had from either alone.
        <b>Bottom:</b> the same tones <b>added together</b>
        (<span class="rg-key rg-key-sum">blue</span>). The offsets cancel
        exactly and a straight line appears. Its slope <em>is</em> the
        distance — move the tag and the line tilts. This is the estimator's own
        unwrapped sequence, the thing it fitted.
      </p>
    </div>
    <div>
      <svg id="rg-error" viewBox="0 0 660 260" role="img"
           aria-label="estimation error over time"></svg>
      <p class="hint">
        Error against the truth, over the last few seconds. Channel Sounding
        stays within centimetres; the RSSI estimate wanders with the fading
        and is biased by whatever it assumed about the room.
      </p>
    </div>
  </div>
</section>

<section class="panel rg-span">
  <h2>The protocol underneath</h2>
  <dl class="kv rg-link">
    <dt>locator</dt><dd id="rg-phase-label">—</dd>
    <dt>connection handle</dt><dd id="rg-handle">—</dd>
    <dt>Real-Time Ranging Data</dt><dd id="rg-ras">—</dd>
    <dt>notifications sent by the tag</dt><dd id="rg-notifs">—</dd>
    <dt>Ranging Data accepted / rejected</dt><dd id="rg-accepted">—</dd>
    <dt>procedures combined / mismatched</dt><dd id="rg-procs">—</dd>
    <dt>ranging counter shown</dt><dd id="rg-counter">—</dd>
  </dl>
  <details>
    <summary>the tones this estimate was computed from</summary>
    <table class="rg-table"><thead><tr>
      <th>channel</th><th>MHz</th><th>I</th><th>Q</th>
      <th>locator °</th><th>tag °</th><th>sum °</th>
    </tr></thead><tbody id="rg-tone-rows"></tbody></table>
  </details>
  <p class="hint" style="margin-top:0.75rem">
    The tag is the procedure's <b>reflector</b>: the locator's
    <code>LE CS Create Config</code> carries <code>Create_Context = 1</code>,
    which writes the configuration into the tag's controller too, and that is
    how the tag learns it is in a procedure at all. Each side's controller then
    reports <code>LE CS Subevent Result</code> to its own host, and the tag's
    host packs its steps into RAS Ranging Data and notifies them. Nothing on
    this page has a distance until those two halves meet.
  </p>
</section>
`;

// --- styles ----------------------------------------------------------------

const STYLE = `
.rg-span { grid-column: 1 / -1; }
.rg-prose { font-size: 0.88rem; margin: 0 0 0.7rem; max-width: 62rem; }
.rg-prose:last-child { margin-bottom: 0; }
#rg-plan { width: 100%; height: auto; background: var(--bg);
  border: 1px solid var(--border); border-radius: 8px; touch-action: none; }
.rg-truth { font-family: ui-monospace, Menlo, monospace; font-size: 1rem;
  font-weight: 600; min-width: 5rem; }
.rg-controls { display: grid; grid-template-columns: auto 1fr; gap: 0.35rem 0.8rem;
  align-items: center; font-size: 0.82rem; margin-top: 0.5rem; }
.rg-controls label { color: var(--dim); }
.rg-val { color: var(--text); font-family: ui-monospace, Menlo, monospace; }
.rg-assume { margin-top: 0.9rem; padding-top: 0.9rem; border-top: 1px solid var(--border); }
.rg-estimates { display: grid; grid-template-columns: 1fr 1fr; gap: 0.75rem; }
@media (max-width: 44rem) { .rg-estimates { grid-template-columns: 1fr; } }
.rg-card { border: 1px solid var(--border); border-radius: 8px; padding: 0.7rem;
  background: var(--bg); }
.rg-card-head { font-size: 0.72rem; text-transform: uppercase; letter-spacing: 0.06em;
  color: var(--dim); margin-bottom: 0.3rem; }
.rg-rssi { border-left: 3px solid var(--warn); }
.rg-cs { border-left: 3px solid var(--accent); }
.rg-big { font: 600 1.7rem/1.1 ui-monospace, Menlo, monospace; }
.rg-err { font-size: 0.8rem; font-family: ui-monospace, Menlo, monospace;
  color: var(--dim); margin-bottom: 0.5rem; }
.rg-err.good { color: var(--good); } .rg-err.bad { color: var(--bad); }
.rg-kv { font-size: 0.78rem; margin: 0; }
.rg-kv dd { font-family: ui-monospace, Menlo, monospace; }
.rg-charts { display: grid; grid-template-columns: 1fr 1fr; gap: 1rem; }
@media (max-width: 60rem) { .rg-charts { grid-template-columns: 1fr; } }
#rg-phase, #rg-error { width: 100%; height: auto; background: var(--bg);
  border: 1px solid var(--border); border-radius: 8px; }
.rg-key { font-weight: 600; }
.rg-key-local { color: #8c959f; } .rg-key-remote { color: var(--warn); }
.rg-key-sum { color: var(--accent); }
.rg-link { font-size: 0.82rem; }
.rg-link dd { font-family: ui-monospace, Menlo, monospace; }
.rg-table { width: 100%; border-collapse: collapse; font-size: 0.74rem;
  font-family: ui-monospace, Menlo, monospace; margin-top: 0.5rem; }
.rg-table th { text-align: right; color: var(--dim); font-weight: 500;
  padding: 0.15rem 0.5rem; border-bottom: 1px solid var(--border); }
.rg-table td { text-align: right; padding: 0.12rem 0.5rem; }
.rg-table tr:nth-child(even) { background: var(--panel2); }
`;

// --- wiring ----------------------------------------------------------------

/** Looks up every element the render path touches, once. */
function cacheElements(root) {
  const ids = [
    "rg-plan", "rg-distance", "rg-truth", "rg-exponent", "rg-exponent-val",
    "rg-shadow", "rg-shadow-val", "rg-txpower", "rg-txpower-val",
    "rg-rssi-distance", "rg-rssi-error", "rg-rssi-now", "rg-rssi-mean",
    "rg-rssi-jitter", "rg-rssi-ref", "rg-rssi-exp", "rg-rssi-instant",
    "rg-cs-distance", "rg-cs-error", "rg-cs-state", "rg-cs-tones", "rg-cs-band",
    "rg-cs-stderr", "rg-cs-residual", "rg-cs-ambig",
    "rg-assumed-exp", "rg-assumed-exp-val", "rg-assumed-ref", "rg-assumed-ref-val",
    "rg-phase", "rg-error", "rg-phase-label", "rg-handle", "rg-ras", "rg-notifs",
    "rg-accepted", "rg-procs", "rg-counter", "rg-tone-rows",
  ];
  ui = {};
  for (const id of ids) ui[id] = root.querySelector("#" + id);
}

/** Where the tag stands, in metres, kept here so the slider and the drag
 *  handle agree on a single source of truth for the bearing. */
let tag = { x: 2, y: 0 };

function wireControls() {
  ui["rg-distance"].addEventListener("input", () => {
    // Keep the bearing, change the range: dragging then sliding does what a
    // reader expects rather than snapping the tag back onto the axis.
    const target = parseFloat(ui["rg-distance"].value);
    const current = Math.hypot(tag.x, tag.y);
    const angle = current < 1e-6 ? 0 : Math.atan2(tag.y, tag.x);
    tag = { x: target * Math.cos(angle), y: target * Math.sin(angle) };
    applyTagPosition();
  });

  for (const id of ["rg-exponent", "rg-shadow", "rg-txpower"]) {
    ui[id].addEventListener("input", applyRoom);
  }
  for (const id of ["rg-assumed-exp", "rg-assumed-ref"]) {
    ui[id].addEventListener("input", applyAssumptions);
  }

  const plan = ui["rg-plan"];
  const move = (event) => {
    if (!dragging) return;
    const point = planToMetres(plan, event);
    const range = Math.min(30, Math.max(0.5, Math.hypot(point.x, point.y)));
    const angle = Math.atan2(point.y, point.x);
    tag = { x: range * Math.cos(angle), y: range * Math.sin(angle) };
    ui["rg-distance"].value = range.toFixed(1);
    applyTagPosition();
  };
  plan.addEventListener("pointerdown", (event) => {
    dragging = true;
    // Capture keeps the drag alive when the pointer leaves the plan, but it
    // throws on a synthetic event with no live pointer id — and an exception
    // here would abort the handler before the tag ever moved.
    try { plan.setPointerCapture(event.pointerId); } catch (_) { /* uncaptured */ }
    move(event);
  });
  plan.addEventListener("pointermove", move);
  plan.addEventListener("pointerup", () => { dragging = false; });
  plan.addEventListener("pointercancel", () => { dragging = false; });
}

/** Converts a pointer event on the plan into floor-plan metres. */
function planToMetres(svg, event) {
  const box = svg.getBoundingClientRect();
  const fx = (event.clientX - box.left) / box.width;
  const fy = (event.clientY - box.top) / box.height;
  return {
    x: PLAN.minX + fx * (PLAN.maxX - PLAN.minX),
    y: PLAN.maxY - fy * (PLAN.maxY - PLAN.minY),
  };
}

function applyTagPosition() {
  ui["rg-distance"].value = Math.hypot(tag.x, tag.y).toFixed(1);
  if (scene) scene.set_tag_position(tag.x, tag.y);
}

function applyRoom() {
  const exponent = parseFloat(ui["rg-exponent"].value);
  const shadow = parseFloat(ui["rg-shadow"].value);
  const power = parseFloat(ui["rg-txpower"].value);
  ui["rg-exponent-val"].textContent = exponent.toFixed(1);
  ui["rg-shadow-val"].textContent = shadow.toFixed(1) + " dB";
  ui["rg-txpower-val"].textContent = power.toFixed(0) + " dBm";
  if (scene) scene.set_room(power, exponent, shadow);
}

function applyAssumptions() {
  const exponent = parseFloat(ui["rg-assumed-exp"].value);
  const reference = parseFloat(ui["rg-assumed-ref"].value);
  ui["rg-assumed-exp-val"].textContent = exponent.toFixed(1);
  ui["rg-assumed-ref-val"].textContent = reference.toFixed(0) + " dBm";
  if (scene) scene.set_rssi_assumptions(reference, exponent);
}

// --- rendering -------------------------------------------------------------

/** Appends this tick's errors to the rolling history. */
function record(status) {
  history.push({ rssi: status.rssi.error_m, cs: status.cs.error_m });
  if (history.length > HISTORY) history.shift();
}

function render(status) {
  const truth = status.truth.distance_m;
  ui["rg-truth"].textContent = truth.toFixed(2) + " m";

  renderRssi(status.rssi, truth);
  renderCs(status.cs);
  renderLink(status.link, status.cs);
  drawPlan(status);
  drawPhase(status.cs);
  drawError();
  renderTones(status.cs.tones);
}

function renderRssi(rssi, truth) {
  ui["rg-rssi-distance"].textContent = metres(rssi.distance_m);
  setError(ui["rg-rssi-error"], rssi.error_m, truth);
  ui["rg-rssi-now"].textContent = rssi.latest_dbm === null ? "—" : rssi.latest_dbm + " dBm";
  ui["rg-rssi-mean"].textContent =
    rssi.mean_dbm === null ? "—" : rssi.mean_dbm.toFixed(1) + " dBm";
  ui["rg-rssi-jitter"].textContent =
    rssi.std_dev_db === null ? "—" : "±" + rssi.std_dev_db.toFixed(1) + " dB";
  ui["rg-rssi-ref"].textContent = rssi.assumed_reference_dbm.toFixed(0) + " dBm";
  ui["rg-rssi-exp"].textContent = rssi.assumed_exponent.toFixed(1);
  ui["rg-rssi-instant"].textContent = metres(rssi.instantaneous_distance_m);
}

function renderCs(cs) {
  ui["rg-cs-distance"].textContent = metres(cs.distance_m);
  setError(ui["rg-cs-error"], cs.error_m);
  ui["rg-cs-state"].textContent = cs.state;
  ui["rg-cs-tones"].textContent = cs.local_tones + " + " + cs.remote_tones;
  ui["rg-cs-band"].textContent =
    cs.bandwidth_hz === null ? "—" : (cs.bandwidth_hz / 1e6).toFixed(0) + " MHz";
  ui["rg-cs-stderr"].textContent =
    cs.std_error_m === null ? "—" : "±" + (cs.std_error_m * 100).toFixed(1) + " cm";
  ui["rg-cs-residual"].textContent =
    cs.residual_rad === null ? "—" : cs.residual_rad.toFixed(3) + " rad";
  ui["rg-cs-ambig"].textContent =
    cs.unambiguous_range_m === null ? "—" : cs.unambiguous_range_m.toFixed(1) + " m";
}

function renderLink(link, cs) {
  ui["rg-phase-label"].textContent = link.phase;
  ui["rg-handle"].textContent = link.connected
    ? "0x" + link.connection_handle.toString(16).toUpperCase().padStart(4, "0")
    : "not connected";
  ui["rg-ras"].textContent = link.ranging_data_handle === null
    ? "not discovered"
    : "handle 0x" + link.ranging_data_handle.toString(16).toUpperCase().padStart(4, "0") +
      (link.ras_subscribed ? " — subscribed" : " — not subscribed");
  ui["rg-notifs"].textContent = String(link.notifications_sent);
  ui["rg-accepted"].textContent =
    link.ranging_data_accepted + " / " + link.ranging_data_rejected;
  ui["rg-procs"].textContent =
    cs.procedures_combined + " / " + cs.procedures_mismatched;
  ui["rg-counter"].textContent =
    cs.ranging_counter === null ? "—" : String(cs.ranging_counter);
}

/** Formats an optional distance. */
function metres(value) {
  return value === null || value === undefined ? "—" : value.toFixed(2) + " m";
}

/** Shows an error in metres, coloured by how large it is relative to truth. */
function setError(el, error) {
  if (error === null || error === undefined) {
    el.textContent = "waiting for a measurement";
    el.className = "rg-err";
    return;
  }
  const sign = error >= 0 ? "+" : "−";
  el.textContent = "error " + sign + Math.abs(error).toFixed(2) + " m";
  el.className = "rg-err " + (Math.abs(error) < 0.5 ? "good" : "bad");
}

// --- drawing ---------------------------------------------------------------

/** SVG element helper. */
function svg(tag, attrs, text) {
  const el = document.createElementNS("http://www.w3.org/2000/svg", tag);
  for (const [key, value] of Object.entries(attrs)) el.setAttribute(key, value);
  if (text !== undefined) el.textContent = text;
  return el;
}

const PLAN_W = 680, PLAN_H = 340;

/** Metres to plan pixels. */
function planX(x) { return ((x - PLAN.minX) / (PLAN.maxX - PLAN.minX)) * PLAN_W; }
function planY(y) { return ((PLAN.maxY - y) / (PLAN.maxY - PLAN.minY)) * PLAN_H; }

function drawPlan(status) {
  const plan = ui["rg-plan"];
  plan.replaceChildren();

  // Range rings every 5 m, so the plan is readable as a distance and not
  // only as a picture.
  for (let r = 5; r <= 30; r += 5) {
    plan.appendChild(svg("ellipse", {
      cx: planX(0), cy: planY(0),
      rx: planX(r) - planX(0), ry: planY(0) - planY(r),
      fill: "none", stroke: "var(--border)", "stroke-dasharray": "3 4",
    }));
    plan.appendChild(svg("text", {
      x: planX(r) + 3, y: planY(0) - 5, fill: "var(--dim)", "font-size": "10",
    }, r + " m"));
  }

  const tagPoint = status.truth.tag;
  const locatorPoint = status.truth.locator;

  // The true separation, and each method's estimate drawn along the same
  // bearing — the picture of the error.
  plan.appendChild(svg("line", {
    x1: planX(locatorPoint.x), y1: planY(locatorPoint.y),
    x2: planX(tagPoint.x), y2: planY(tagPoint.y),
    stroke: "var(--text)", "stroke-width": "1.5",
  }));
  const bearing = Math.atan2(tagPoint.y - locatorPoint.y, tagPoint.x - locatorPoint.x);
  const arc = (distance, colour) => {
    if (distance === null || distance === undefined) return;
    const capped = Math.min(distance, 40);
    plan.appendChild(svg("circle", {
      cx: planX(locatorPoint.x + capped * Math.cos(bearing)),
      cy: planY(locatorPoint.y + capped * Math.sin(bearing)),
      r: 7, fill: "none", stroke: colour, "stroke-width": "2",
    }));
  };
  arc(status.rssi.distance_m, "var(--warn)");
  arc(status.cs.distance_m, "var(--accent)");

  plan.appendChild(svg("circle", {
    cx: planX(locatorPoint.x), cy: planY(locatorPoint.y), r: 9,
    fill: "var(--accent)",
  }));
  plan.appendChild(svg("text", {
    x: planX(locatorPoint.x) + 13, y: planY(locatorPoint.y) + 4,
    fill: "var(--text)", "font-size": "12",
  }, "locator"));

  plan.appendChild(svg("circle", {
    cx: planX(tagPoint.x), cy: planY(tagPoint.y), r: 10,
    fill: "var(--good)", stroke: "var(--bg)", "stroke-width": "2",
  }));
  plan.appendChild(svg("text", {
    x: planX(tagPoint.x) + 14, y: planY(tagPoint.y) + 4,
    fill: "var(--text)", "font-size": "12",
  }, "tag"));
}

const CHART_W = 660, CHART_H = 260, PAD_L = 46, PAD_R = 12, PAD_T = 14, PAD_B = 30;

/** Draws the axes and returns the plot rectangle. */
function chartFrame(el, yLabel, yMin, yMax, ticks) {
  el.replaceChildren();
  const plot = {
    x0: PAD_L, x1: CHART_W - PAD_R, y0: PAD_T, y1: CHART_H - PAD_B,
    yMin, yMax,
  };
  plot.toY = (v) =>
    plot.y1 - ((v - yMin) / (yMax - yMin)) * (plot.y1 - plot.y0);
  for (const value of ticks) {
    const y = plot.toY(value);
    el.appendChild(svg("line", {
      x1: plot.x0, y1: y, x2: plot.x1, y2: y,
      stroke: "var(--border)", "stroke-dasharray": value === 0 ? "" : "2 4",
    }));
    el.appendChild(svg("text", {
      x: plot.x0 - 6, y: y + 3, fill: "var(--dim)", "font-size": "10",
      "text-anchor": "end",
    }, String(value)));
  }
  el.appendChild(svg("text", {
    x: 12, y: PAD_T + 4, fill: "var(--dim)", "font-size": "10",
  }, yLabel));
  return plot;
}

/** Phase against tone frequency: the picture of why RAS exists.
 *
 *  Two stacked panels, because the wrapped phases and the unwrapped ramp
 *  cannot share a y axis. On top, each end's raw measurement — scatter. Below,
 *  the estimator's own unwrapped sums — a straight line whose slope is the
 *  distance. The unwrapping is done in Rust by the same code the estimate uses;
 *  the page only plots it. */
function drawPhase(cs) {
  const el = ui["rg-phase"];
  el.replaceChildren();
  const tones = cs.tones || [];
  if (tones.length === 0) {
    el.appendChild(svg("text", {
      x: CHART_W / 2, y: CHART_H / 2, fill: "var(--dim)", "font-size": "12",
      "text-anchor": "middle",
    }, "waiting for the first Channel Sounding procedure…"));
    return;
  }

  const channels = tones.map((t) => t.channel);
  const lo = Math.min(...channels), hi = Math.max(...channels);
  const x0 = 52, x1 = CHART_W - 10;
  const toX = (channel) => x0 + ((channel - lo) / Math.max(1, hi - lo)) * (x1 - x0);

  const panel = (yTop, yBottom, label, min, max, ticks) => {
    const p = { toY: (v) => yBottom - ((v - min) / (max - min)) * (yBottom - yTop) };
    for (const value of ticks) {
      const y = p.toY(value);
      el.appendChild(svg("line", {
        x1: x0, y1: y, x2: x1, y2: y,
        stroke: "var(--border)", "stroke-dasharray": "2 4",
      }));
      el.appendChild(svg("text", {
        x: x0 - 5, y: y + 3, fill: "var(--dim)", "font-size": "9",
        "text-anchor": "end",
      }, String(Math.round(value))));
    }
    // Right-aligned: the left margin belongs to the tick labels, and a
    // caption there collides with the topmost one.
    el.appendChild(svg("text", {
      x: x1, y: yTop - 4, fill: "var(--dim)", "font-size": "9",
      "text-anchor": "end",
    }, label));
    return p;
  };

  // Top: what each radio measured, wrapped into the half turn it can observe.
  const raw = panel(26, 116, "each end alone, degrees", -180, 180, [-180, 0, 180]);
  for (const [key, colour] of [
    ["local_phase_deg", "#8c959f"],
    ["remote_phase_deg", "var(--warn)"],
  ]) {
    for (const tone of tones) {
      const value = tone[key];
      if (value === null || value === undefined) continue;
      el.appendChild(svg("circle", {
        cx: toX(tone.channel), cy: raw.toY(value), r: 3, fill: colour,
      }));
    }
  }

  // Bottom: their sums, unwrapped — the line the distance was fitted to.
  const values = tones
    .map((t) => t.unwrapped_phase_deg)
    .filter((v) => v !== null && v !== undefined);
  if (values.length === 0) return;
  const min = Math.min(0, ...values), max = Math.max(1, ...values);
  const sum = panel(154, 236, "added together, degrees (unwrapped)", min, max,
    [min, (min + max) / 2, max]);
  let path = "";
  for (const tone of tones) {
    const value = tone.unwrapped_phase_deg;
    if (value === null || value === undefined) continue;
    const cx = toX(tone.channel), cy = sum.toY(value);
    path += (path ? "L" : "M") + cx.toFixed(1) + "," + cy.toFixed(1) + " ";
    el.appendChild(svg("circle", { cx, cy, r: 3.5, fill: "var(--accent)" }));
  }
  el.appendChild(svg("path", {
    d: path.trim(), fill: "none", stroke: "var(--accent)", "stroke-width": "1",
    opacity: "0.5",
  }));

  el.appendChild(svg("text", {
    x: x0, y: CHART_H - 4, fill: "var(--dim)", "font-size": "9",
  }, (2402 + lo) + " MHz"));
  el.appendChild(svg("text", {
    x: x1, y: CHART_H - 4, fill: "var(--dim)", "font-size": "9",
    "text-anchor": "end",
  }, (2402 + hi) + " MHz"));
}

/** Both methods' error over time. */
function drawError() {
  const el = ui["rg-error"];
  // A fixed scale: an auto-scaling axis would hide how much bigger one error
  // is than the other, which is the entire comparison.
  const plot = chartFrame(el, "error, m", -12, 12, [-10, -5, 0, 5, 10]);
  if (history.length < 2) return;
  const toX = (index) =>
    plot.x0 + (index / (HISTORY - 1)) * (plot.x1 - plot.x0);

  const line = (key, colour, width) => {
    // A gap in the data (before the first estimate arrives) has to lift the
    // pen rather than draw a straight line across it — otherwise the chart
    // invents measurements that were never made.
    let path = "";
    let penDown = false;
    history.forEach((point, index) => {
      const value = point[key];
      if (value === null || value === undefined) { penDown = false; return; }
      const y = plot.toY(Math.max(-12, Math.min(12, value)));
      path += (penDown ? "L" : "M") + toX(index).toFixed(1) + "," + y.toFixed(1) + " ";
      penDown = true;
    });
    if (path) {
      el.appendChild(svg("path", {
        d: path.trim(), fill: "none", stroke: colour, "stroke-width": width,
        "stroke-linejoin": "round",
      }));
    }
  };
  line("rssi", "var(--warn)", "1.5");
  line("cs", "var(--accent)", "2");

  el.appendChild(svg("text", {
    x: plot.x1 - 4, y: PAD_T + 12, fill: "var(--warn)", "font-size": "10",
    "text-anchor": "end",
  }, "RSSI"));
  el.appendChild(svg("text", {
    x: plot.x1 - 4, y: PAD_T + 26, fill: "var(--accent)", "font-size": "10",
    "text-anchor": "end",
  }, "Channel Sounding"));
}

/** The per-tone table, so the estimate's raw inputs are inspectable. */
function renderTones(tones) {
  const body = ui["rg-tone-rows"];
  if (!body || !body.closest("details").open) return;
  body.replaceChildren();
  for (const tone of tones || []) {
    const row = document.createElement("tr");
    const cells = [
      tone.channel,
      2402 + tone.channel,
      tone.i,
      tone.q,
      tone.local_phase_deg.toFixed(1),
      tone.remote_phase_deg === null ? "—" : tone.remote_phase_deg.toFixed(1),
      tone.combined_phase_deg === null ? "—" : tone.combined_phase_deg.toFixed(1),
    ];
    for (const value of cells) {
      const cell = document.createElement("td");
      cell.textContent = String(value);
      row.appendChild(cell);
    }
    body.appendChild(row);
  }
}
