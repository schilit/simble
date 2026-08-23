// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// The car domain: a phone and a head unit on one Bluetooth link.
//
// A car head unit is not "an HFP device" — it is one endpoint playing several
// profile roles at once. This page builds the telephony role end to end and
// marks the others as not built, rather than miming them.
//
// The centre column is the reason this domain is worth a page. HFP is the one
// Bluetooth profile whose wire is human-readable, so the dialogue you see is
// the actual bytes SimBLE's HfProtocol and AgProtocol produced, carried in
// real RFCOMM frames — not a scripted animation. Toggle "bytes" to see the
// hex of each line as it was written.
//
// This module is a domain module: mount(root) builds everything into the
// container it is given and starts the single timer; unmount() stops that
// timer, drops the wasm object and removes the markup, so a tab shell can
// swap domains without leaving anything running.

import init, { WebCarKit } from "../pkg/simble.js";
import { createDeviceHeader } from "../common/device-header.js";
import { createAboutBox } from "../common/about-box.js";

/// Which controllers this domain can run on. The shell's controller bar
/// reads this: an option mapped to a string is offered disabled, with that
/// string as the reason, rather than hidden.
export const SUPPORTS = { "in-page": true,
  "websocket": "the phone and head unit hand frames straight to each other — nothing sits between them" };


// One timer for both endpoints. Chrome throttles a hidden tab hard enough
// that a device hosted in one misses protocol deadlines, so a two-page design
// would stall silently; both halves live here and share this interval.
const TICK_MS = 120;

// One `WebCarKit` holds both endpoints and the multiplexers between them, and
// dropping it is the only teardown there is — so neither header offers a stop
// that would take its neighbour down with it.
const SHARED_KIT = "one kit — both endpoints stop together";

// These two endpoints genuinely have no Bluetooth address. The page's own
// prose says why: the two RFCOMM multiplexers hand frames straight to each
// other, so there is no L2CAP channel and no ACL connection underneath, and a
// BD_ADDR is a property of a link that does not exist here. Inventing one for
// the header would be exactly the kind of decoration this component replaced.
const NO_ADDRESS =
  "no BD_ADDR: the two RFCOMM multiplexers are wired directly together, with no ACL underneath";

let wasmReady = null;

const STYLE = `
.car h3 { font-size: var(--fs-label); text-transform: uppercase; letter-spacing: 0.05em;
  color: var(--dim); margin: 1rem 0 0.4rem; font-weight: 600; }
.car h3:first-of-type { margin-top: 0.2rem; }

/* Two generic device mock-ups. Deliberately plain shapes: this is a phone and
   a dashboard in the abstract, not anyone's product. */
.mock { display: block; margin: 0 auto 0.6rem; width: 100%; max-width: 15rem; height: auto; }
.screen-line { font: 600 11px ui-monospace, Menlo, monospace; fill: #d8e0e8; }
.screen-dim { font: 10px ui-monospace, Menlo, monospace; fill: #8b98a5; }
.screen-big { font: 700 15px ui-monospace, Menlo, monospace; fill: #e8eef4; }
.screen-ring { fill: #46d17a; }

.ctl { display: flex; gap: 0.45rem; align-items: center; flex-wrap: wrap; margin-top: 0.5rem; }
.ctl label { font-size: var(--fs-meta); color: var(--dim); min-width: 5.2rem; }
.car input[type=range] { flex: 1 1 7rem; min-width: 6rem; }
.car input[type=text] { flex: 1 1 8rem; min-width: 6rem; }
.val { font: 0.78rem ui-monospace, Menlo, monospace; color: var(--text); min-width: 1.6rem;
  text-align: right; }

.answer { background: #1a7f37; border-color: #1a7f37; color: #fff; font-weight: 600; }
.decline { background: #cf222e; border-color: #cf222e; color: #fff; font-weight: 600; }
.answer:disabled, .decline:disabled { opacity: 0.35; }
.car button.on { border-color: var(--accent); color: var(--accent); }

/* The AT dialogue. */
.dialogue { display: flex; flex-direction: column; }
.tape { flex: 1 1 auto; min-height: 24rem; max-height: 34rem; overflow-y: auto; overflow-x: auto;
  background: var(--bg); border: 1px solid var(--border); border-radius: 6px;
  padding: 0.5rem 0.6rem; font: 12px/1.55 ui-monospace, SFMono-Regular, Menlo, monospace; }
.tape .at { display: grid; grid-template-columns: 1fr 1.4rem 1fr; gap: 0 0.4rem;
  white-space: pre; }
.tape .at .arrow { color: var(--dim); text-align: center; }
.tape .hf > span:nth-child(1) { color: var(--accent); }
.tape .ag > span:nth-child(3) { color: var(--good); grid-column: 3; }
.tape .hf > span:nth-child(3), .tape .ag > span:nth-child(1) { color: transparent; }
.tape .bytes { color: var(--dim); font-size: var(--fs-code); padding-left: 0.4rem; white-space: pre-wrap;
  word-break: break-all; }
.tape .unsolicited > span:nth-child(3) { color: #9a6700; }
.tape-empty { color: var(--dim); font-style: italic; }

/* The stack step list, in the shape the Audio Source page uses. */
.stages { list-style: none; padding: 0; margin: 0.2rem 0 0; }
.stages li { padding: 0.4rem 0 0.4rem 1.6rem; position: relative; color: var(--dim);
  font-size: var(--fs-body); border-top: 1px solid var(--border); }
.stages li:first-child { border-top: none; }
.stages li::before { content: "○"; position: absolute; left: 0.3rem; }
.stages li.done { color: var(--text); }
.stages li.done::before { content: "●"; color: var(--good); }
.stages li.active { color: var(--text); font-weight: 600; }
.stages li.active::before { content: "◐"; color: var(--accent); }
.stages li.failed::before { content: "✕"; color: var(--bad); }
.stages .detail { display: block; font-weight: 400; color: var(--dim); font-size: var(--fs-meta);
  margin-top: 0.15rem; font-family: ui-monospace, Menlo, monospace; }

.layers { display: flex; flex-wrap: wrap; gap: 0.3rem; align-items: center; margin: 0.7rem 0 0;
  font: 0.78rem ui-monospace, Menlo, monospace; }
.layers span { padding: 0.2rem 0.55rem; border: 1px solid var(--border); border-radius: 5px;
  background: var(--bg); }
.layers span.real { border-color: var(--good); color: var(--good); }
.layers span.absent { border-style: dashed; color: var(--dim); }
.layers b { color: var(--dim); font-weight: 400; }

/* Indicator mirror. */
.inds { width: 100%; border-collapse: collapse; font-size: var(--fs-label);
  font-family: ui-monospace, Menlo, monospace; margin-top: 0.3rem; }
.inds th { text-align: left; font-weight: 500; color: var(--dim); font-size: var(--fs-meta);
  text-transform: uppercase; letter-spacing: 0.04em; padding-bottom: 0.2rem; }
.inds td { padding: 0.12rem 0.3rem 0.12rem 0; border-top: 1px solid var(--border); }
.inds td.n { color: var(--dim); }
.inds tr.stale td { color: var(--warn); }

/* Roles: what this head unit would also be, and is not yet. */
.roles { display: grid; gap: 0.7rem; grid-template-columns: repeat(auto-fit, minmax(15rem, 1fr));
  margin-top: 0.3rem; }
.role { border: 1px solid var(--border); border-radius: 8px; padding: 0.7rem 0.8rem;
  background: var(--bg); }
.role.gap { border-style: dashed; }
.role .tag { font-size: var(--fs-meta); text-transform: uppercase; letter-spacing: 0.06em;
  border: 1px solid var(--border); border-radius: 1rem; padding: 0.05rem 0.5rem;
  color: var(--dim); float: right; }
.role.built .tag { color: var(--good); border-color: var(--good); }
.role.gap .tag { color: var(--warn); border-color: var(--warn); }
.role h4 { margin: 0 0 0.35rem; font-size: var(--fs-body); }
.role p { margin: 0; font-size: var(--fs-label); color: var(--dim); line-height: 1.45; }
.role code { color: var(--text); }
`;

const ABOUT = `<p>A phone and a car head unit over one link. SDP finds the Hands-Free service, RFCOMM
   opens a channel, and the two ends negotiate a service-level connection before any call can
   ring. Every AT line in the dialogue is bytes the protocol layers actually produced.</p>
   <p>Call <em>audio</em> would ride an SCO link, which SimBLE does not implement — the gap is
   marked where the audio path would be, rather than faked.</p>`;

const MARKUP = `
<div class="car domain two-up">

  <!-- ============ the phone: Audio Gateway ============ -->
  <section class="panel">
    <div id="car-ag-head"></div>
    <svg class="mock" viewBox="0 0 176 250" role="img" aria-label="a phone">
      <rect x="16" y="4" width="144" height="242" rx="18" fill="#2b3138"/>
      <rect x="23" y="16" width="130" height="218" rx="8" fill="#161b21"/>
      <text x="30" y="33" class="screen-dim" id="p-operator">—</text>
      <g id="p-bars" transform="translate(104,25)"></g>
      <rect x="136" y="25" width="14" height="7" rx="2" fill="none" stroke="#8b98a5"/>
      <rect x="137" y="26" width="12" height="5" id="p-batt" fill="#46d17a"/>
      <text x="30" y="122" class="screen-big" id="p-state">idle</text>
      <text x="30" y="143" class="screen-line" id="p-number"></text>
      <text x="30" y="163" class="screen-dim" id="p-roam"></text>
      <circle cx="88" cy="228" r="7" fill="none" stroke="#3d444d" stroke-width="2"/>
    </svg>

    <h3>the network it is on</h3>
    <div class="ctl">
      <label for="car-operator">operator</label>
      <input type="text" id="car-operator" value="Simble Mobile">
    </div>
    <div class="ctl">
      <label for="car-signal">signal</label>
      <input type="range" id="car-signal" min="0" max="5" step="1" value="4">
      <span class="val" id="car-signal-v">4</span>
    </div>
    <div class="ctl">
      <label for="car-battery">battery</label>
      <input type="range" id="car-battery" min="0" max="5" step="1" value="4">
      <span class="val" id="car-battery-v">4</span>
    </div>
    <div class="ctl">
      <button id="car-service">service: registered</button>
      <button id="car-roam">roaming: off</button>
    </div>

    <h3>calls</h3>
    <div class="ctl">
      <label for="car-number">number</label>
      <input type="text" id="car-number" value="+15551234">
    </div>
    <div class="ctl">
      <button id="car-incoming" class="primary">call arrives</button>
      <button id="car-place">phone dials out</button>
      <button id="car-end" class="decline">end call</button>
    </div>
    <p class="hint" style="margin-top:0.7rem">
      These are the indicators the AG owns. Each change is one
      <code>+CIEV: &lt;index&gt;,&lt;value&gt;</code> — and the index is nothing but the
      indicator's position in the <code>+CIND=?</code> list, which is why that order
      is the whole meaning of the notification.
    </p>
  </section>

  <!-- ============ the head unit: Hands-Free ============ -->
  <section class="panel">
    <div id="car-hf-head"></div>
    <svg class="mock" viewBox="0 0 240 150" role="img" aria-label="a car head unit">
      <rect x="2" y="6" width="236" height="138" rx="12" fill="#2b3138"/>
      <rect x="12" y="16" width="216" height="118" rx="6" fill="#161b21"/>
      <circle cx="26" cy="30" r="4" class="screen-ring" id="d-led" opacity="0.25"/>
      <text x="38" y="34" class="screen-dim" id="d-operator">—</text>
      <text x="24" y="72" class="screen-big" id="d-caller">no call</text>
      <text x="24" y="94" class="screen-line" id="d-state">idle</text>
      <text x="24" y="118" class="screen-dim" id="d-gain">spk — · mic —</text>
      <g id="d-bars" transform="translate(196,22)"></g>
    </svg>

    <div class="ctl">
      <button id="car-answer" class="answer" disabled>answer</button>
      <button id="car-hangup" class="decline" disabled>hang up</button>
    </div>

    <h3>audio path gains</h3>
    <div class="ctl">
      <label for="car-speaker">speaker</label>
      <input type="range" id="car-speaker" min="0" max="15" step="1" value="9">
      <span class="val" id="car-speaker-v">9</span>
    </div>
    <div class="ctl">
      <label for="car-mic">microphone</label>
      <input type="range" id="car-mic" min="0" max="15" step="1" value="12">
      <span class="val" id="car-mic-v">12</span>
    </div>
    <div class="ctl">
      <button id="car-mute">mute mic</button>
      <button id="car-voice">voice assistant</button>
      <button id="car-clcc">list calls</button>
    </div>

    <h3>dial from the dashboard</h3>
    <div class="ctl">
      <input type="text" id="car-cardial" value="+15550142">
      <button id="car-cardial-go">dial</button>
    </div>

    <h3>the phone's indicators, as this head unit sees them</h3>
    <table class="inds">
      <thead><tr><th>#</th><th>indicator</th><th>phone</th><th>head unit</th></tr></thead>
      <tbody id="car-inds"></tbody>
    </table>
    <p class="hint" style="margin-top:0.6rem">
      The right-hand column is only ever written by a <code>+CIEV</code> that arrived.
      A row goes amber when the phone has moved on and the notification has not landed
      yet — and it stays amber forever if the head unit never sent <code>AT+CMER</code>,
      because an AG may not report indicators it was not asked to report.
    </p>
  </section>

  <!-- ============ the AT dialogue ============ -->
  <section class="panel dialogue full">
    <h2>The wire — AT over RFCOMM</h2>
    <div class="ctl" style="margin-top:0;margin-bottom:0.5rem">
      <span class="hint" id="car-link">starting…</span>
      <span class="spacer" style="flex:1"></span>
      <button id="car-bytes">bytes</button>
      <button id="car-clear">clear</button>
    </div>
    <div class="tape" id="car-tape"><div class="tape-empty">waiting for the link…</div></div>
    <p class="hint" style="margin-top:0.7rem">
      Left is the head unit, right is the phone. These are the bytes
      <code>HfProtocol</code> and <code>AgProtocol</code> actually wrote into the
      RFCOMM data link connection — commands are <code>\\r</code>-terminated, responses
      are wrapped in <code>\\r\\n</code>. Turn on <b>bytes</b> to see each line's hex.
    </p>
  </section>

  <!-- ============ the stack ============ -->
  <section class="panel full">
    <h2>The stack underneath</h2>
    <ul class="stages" id="car-steps"></ul>
    <div class="layers">
      <span class="real">AT / HFP</span><b>rides</b>
      <span class="real">RFCOMM</span><b>rides</b>
      <span class="absent">L2CAP PSM 3</span><b>rides</b>
      <span class="absent">ACL / baseband</span>
    </div>
    <p class="hint" style="margin-top:0.7rem">
      Solid boxes are exercised on this page; dashed ones are not. The two RFCOMM
      multiplexers hand frames straight to each other here — the L2CAP channel and the
      ACL connection those frames would ride on a real link are
      <code>ClassicHost</code>'s job, not this page's. Everything above that line is
      genuine protocol: SDP PDUs answered by <code>SdpServer</code>, the
      PN/SABM/UA handshake, and credit accounting
      (<span id="car-credits" class="mono">—</span>).
    </p>
  </section>

  <!-- ============ the other roles ============ -->
  <section class="panel full">
    <h2>The rest of the head unit</h2>
    <p class="hint" style="margin-top:0">
      One head unit, one link, several profile roles at the same time. Only the first
      of these is built.
    </p>
    <div class="roles">
      <div class="role built">
        <span class="tag">built</span>
        <h4>Hands-Free — telephony</h4>
        <p>Everything on this page: SDP discovery, an RFCOMM DLC, the Service Level
           Connection, and the call state machine, both directions.</p>
      </div>
      <div class="role gap">
        <span class="tag">not built</span>
        <h4>SCO / eSCO — the call audio</h4>
        <p>This is where the voice would be, and there is nothing here. SimBLE has no
           synchronous connection at all — no <code>Setup Synchronous Connection</code>,
           no SCO handle, no audio path. Codec negotiation on this page
           (<code>AT+BAC</code>, <code>+BCS</code>) settles <em>which</em> codec would be
           used and then stops. SCO is the BR/EDR counterpart of the CIS work recently
           finished for LE Audio; routing this call through that machinery instead would
           be a different transport and a lie about what the stack does.</p>
      </div>
      <div class="role gap">
        <span class="tag">not built</span>
        <h4>A2DP sink — music</h4>
        <p>The same head unit is an audio sink over AVDTP. Blocked on an SBC codec;
           the AVDTP signalling and RTP media path exist.</p>
      </div>
      <div class="role gap">
        <span class="tag">not built</span>
        <h4>AVRCP controller — transport keys</h4>
        <p>The steering-wheel skip and pause buttons, over AVCTP. The layer above A2DP,
           and the next one after it.</p>
      </div>
      <div class="role gap">
        <span class="tag">not built</span>
        <h4>PBAP client — the phonebook</h4>
        <p>How a head unit turns <code>+15551234</code> into a name, over OBEX.
           Not implemented in SimBLE.</p>
      </div>
    </div>
  </section>
</div>
`;

// --- helpers ---------------------------------------------------------------

const CALL_TEXT = {
  idle: "idle",
  incoming: "incoming call",
  dialing: "dialing",
  alerting: "ringing at the far end",
  active: "in call",
};

// The phone's own screen is narrow, so it gets the short labels.
const PHONE_CALL_TEXT = {
  idle: "idle",
  incoming: "incoming",
  dialing: "dialing",
  alerting: "alerting",
  active: "in call",
};

const LINK_TEXT = {
  down: "link down",
  discovering: "SDP: searching the phone",
  "starting-multiplexer": "RFCOMM: starting the multiplexer",
  "opening-dlc": "RFCOMM: opening the data link connection",
  "establishing-slc": "HFP: service level connection",
  configuring: "HFP: head-unit setup",
  ready: "linked",
  failed: "failed",
};

function bars(count, max, colour) {
  let out = "";
  for (let i = 0; i < max; i += 1) {
    const height = 4 + i * 2;
    const on = i < count;
    out +=
      `<rect x="${i * 5}" y="${12 - height}" width="3" height="${height}" ` +
      `fill="${on ? colour : "#3d444d"}"/>`;
  }
  return out;
}

// --- the module ------------------------------------------------------------

let timer = null;
let kit = null;
let styleEl = null;
let container = null;
let nextSeq = 0;
let showBytes = false;
let heads = {};

export async function mount(root) {
  if (kit) unmount();
  container = root;

  styleEl = document.createElement("style");
  styleEl.dataset.domain = "car";
  styleEl.textContent = STYLE;
  document.head.appendChild(styleEl);
  root.innerHTML = MARKUP;
  (root.querySelector(".domain") ?? root).prepend(createAboutBox(ABOUT));

  wasmReady = wasmReady || init();
  await wasmReady;
  kit = new WebCarKit();
  nextSeq = 0;

  const $ = (id) => root.querySelector(`#${id}`);
  const tape = $("car-tape");
  let tapeEmpty = true;

  // Both endpoints are Rust (`AgProtocol` and `HfProtocol`), so neither has a
  // script and neither gets a pen.
  heads.ag = createDeviceHeader({
    name: "Phone",
    kind: "Audio Gateway · AgProtocol",
    accent: "accent",
    address: null,
    addressNote: NO_ADDRESS,
    dotMeans: "the head unit has an established Service Level Connection to it",
    run: { running: true, disabled: true, reason: SHARED_KIT },
  });
  $("car-ag-head").append(heads.ag.el);

  heads.hf = createDeviceHeader({
    name: "Head Unit",
    kind: "Hands-Free · HfProtocol",
    accent: "good",
    address: null,
    addressNote: NO_ADDRESS,
    dotMeans: "it has an established Service Level Connection to the phone",
    run: { running: true, disabled: true, reason: SHARED_KIT },
  });
  $("car-hf-head").append(heads.hf.el);

  // Every control is one call into the wasm object; the object refuses
  // anything the link cannot do yet, which is what drives the disabled
  // states below.
  const send = (name, argument = "") => kit && kit.command(name, String(argument));

  const bind = (id, event, handler) => $(id).addEventListener(event, handler);

  bind("car-incoming", "click", () => send("incoming", $("car-number").value.trim()));
  bind("car-place", "click", () => send("phone-dial", $("car-number").value.trim()));
  bind("car-end", "click", () => send("phone-end"));
  bind("car-answer", "click", () => send("answer"));
  bind("car-hangup", "click", () => send("hangup"));
  bind("car-cardial-go", "click", () => send("car-dial", $("car-cardial").value.trim()));
  bind("car-clcc", "click", () => send("calls"));
  bind("car-operator", "change", () => send("operator", $("car-operator").value.trim()));

  const slider = (id, command) =>
    bind(id, "input", (e) => {
      $(`${id}-v`).textContent = e.target.value;
      send(command, e.target.value);
    });
  slider("car-signal", "signal");
  slider("car-battery", "battery");
  slider("car-speaker", "speaker");
  slider("car-mic", "microphone");

  let service = true;
  bind("car-service", "click", () => {
    service = !service;
    $("car-service").textContent = `service: ${service ? "registered" : "no service"}`;
    $("car-service").classList.toggle("on", !service);
    send("service", service ? 1 : 0);
  });
  let roaming = false;
  bind("car-roam", "click", () => {
    roaming = !roaming;
    $("car-roam").textContent = `roaming: ${roaming ? "on" : "off"}`;
    $("car-roam").classList.toggle("on", roaming);
    send("roam", roaming ? 1 : 0);
  });
  let muted = false;
  bind("car-mute", "click", () => {
    muted = !muted;
    send("mute", muted ? 1 : 0);
  });
  let voice = false;
  bind("car-voice", "click", () => {
    voice = !voice;
    send("voice", voice ? 1 : 0);
  });

  bind("car-bytes", "click", () => {
    showBytes = !showBytes;
    $("car-bytes").classList.toggle("on", showBytes);
    tape.querySelectorAll(".bytes").forEach((el) => {
      el.hidden = !showBytes;
    });
  });
  bind("car-clear", "click", () => {
    tape.innerHTML = "";
    tapeEmpty = false;
  });

  function appendLines(lines) {
    if (!lines.length) return;
    if (tapeEmpty) {
      tape.innerHTML = "";
      tapeEmpty = false;
    }
    const atBottom = tape.scrollHeight - tape.scrollTop - tape.clientHeight < 40;
    for (const line of lines) {
      const row = document.createElement("div");
      // An AG line that is not a status code and not a "+" answer to something
      // the head unit asked for is an unsolicited result code — RING, +CIEV,
      // +CLIP. Those are the interesting ones, so they get their own colour.
      const unsolicited =
        !line.from_hf && /^(RING|\+CIEV|\+CLIP|\+VGS|\+VGM|\+BVRA|\+BSIR|\+BCS)/.test(line.text);
      row.className = `at ${line.from_hf ? "hf" : "ag"}${unsolicited ? " unsolicited" : ""}`;
      const left = document.createElement("span");
      const arrow = document.createElement("span");
      const right = document.createElement("span");
      arrow.className = "arrow";
      arrow.textContent = line.from_hf ? "→" : "←";
      left.textContent = line.from_hf ? line.text : "";
      right.textContent = line.from_hf ? "" : line.text;
      row.append(left, arrow, right);

      const hex = document.createElement("div");
      hex.className = "bytes";
      hex.textContent = line.hex;
      hex.hidden = !showBytes;

      tape.append(row, hex);
    }
    while (tape.children.length > 800) tape.removeChild(tape.firstChild);
    if (atBottom) tape.scrollTop = tape.scrollHeight;
  }

  function render() {
    kit.tick(performance.now());
    const status = JSON.parse(kit.status_json(nextSeq));
    nextSeq = status.next_seq;
    appendLines(status.at);

    const ready = status.link === "ready";
    $("car-link").textContent = status.error
      ? `failed: ${status.error}`
      : LINK_TEXT[status.link] || status.link;

    // The dot is the Service Level Connection: until it is up, an AG may not
    // report indicators it was not asked for and the head unit's screen is
    // showing nothing that arrived over the link. Each side's label is what
    // *it* knows, which is the whole point of the page.
    const tone = status.error ? "bad" : ready ? "ok" : "";
    heads.ag.setState(
      ready,
      status.error ? `failed: ${status.error}`
        : ready ? `${CALL_TEXT[status.call] || status.call} · ${status.operator}`
        : LINK_TEXT[status.link] || status.link,
      tone,
    );
    heads.hf.setState(
      ready,
      status.error ? `failed: ${status.error}`
        : ready ? `${CALL_TEXT[status.call] || status.call} · codec ${status.codec}`
        : LINK_TEXT[status.link] || status.link,
      tone,
    );

    // -- the phone --
    const byName = Object.fromEntries(status.indicators.map((i) => [i.name, i]));
    const signal = byName.signal ? byName.signal.value : 0;
    const battery = byName.battchg ? byName.battchg.value : 0;
    $("p-operator").textContent =
      byName.service && byName.service.value ? status.operator.slice(0, 13) : "no service";
    $("p-bars").innerHTML = bars(signal, 5, "#8b98a5");
    $("p-batt").setAttribute("width", String(Math.max(1, (battery / 5) * 12)));
    $("p-batt").setAttribute("fill", battery <= 1 ? "#e5534b" : "#46d17a");
    $("p-state").textContent = PHONE_CALL_TEXT[status.call] || status.call;
    $("p-number").textContent = status.caller || "";
    $("p-roam").textContent = byName.roam && byName.roam.value ? "roaming" : "";

    // -- the head unit: only ever shows what reached it over the link --
    $("d-operator").textContent = status.car_operator || "—";
    $("d-caller").textContent = status.caller && status.call !== "idle" ? status.caller : "no call";
    $("d-state").textContent = CALL_TEXT[status.call] || status.call;
    $("d-gain").textContent =
      `spk ${status.speaker_gain} · mic ${status.microphone_muted ? "muted" : status.microphone_gain}`;
    $("d-led").setAttribute("opacity", status.call === "idle" ? "0.25" : "1");
    const mirrored = status.indicators.find((i) => i.name === "signal");
    $("d-bars").innerHTML = bars(mirrored && mirrored.mirrored != null ? mirrored.mirrored : 0, 5, "#46d17a");

    $("car-answer").disabled = status.call !== "incoming";
    $("car-hangup").disabled = status.call === "idle";
    $("car-incoming").disabled = !ready || status.call !== "idle";
    $("car-place").disabled = !ready || status.call !== "idle";
    $("car-cardial-go").disabled = !ready || status.call !== "idle";
    $("car-end").disabled = status.call === "idle";
    $("car-mute").classList.toggle("on", status.microphone_muted);
    $("car-voice").classList.toggle("on", status.voice_recognition);
    if (document.activeElement !== $("car-speaker")) $("car-speaker").value = status.speaker_gain;
    $("car-speaker-v").textContent = status.speaker_gain;

    // -- indicator mirror --
    $("car-inds").innerHTML = status.indicators
      .map((i) => {
        const seen = i.mirrored == null ? "—" : i.mirrored;
        const stale = i.mirrored != null && i.mirrored !== i.value;
        return (
          `<tr class="${stale ? "stale" : ""}"><td class="n">${i.index}</td>` +
          `<td>${i.name}</td><td>${i.value}/${i.max}</td><td>${seen}</td></tr>`
        );
      })
      .join("");

    // -- the stack --
    $("car-steps").innerHTML = status.steps
      .map(
        (s) =>
          `<li class="${s.state}">${s.label}` +
          (s.detail ? `<span class="detail">${s.detail}</span>` : "") +
          `</li>`,
      )
      .join("");
    $("car-credits").textContent = status.credits_out
      ? `${status.credits_out} credits to send, ${status.credits_in} granted to the phone`
      : "no data link connection yet";
  }

  render();
  timer = setInterval(render, TICK_MS);
}

export function unmount() {
  if (timer !== null) {
    clearInterval(timer);
    timer = null;
  }
  // The wasm object holds both endpoints; dropping it is what stops them.
  if (kit) {
    kit.free();
    kit = null;
  }
  for (const head of Object.values(heads)) head.destroy();
  heads = {};
  if (styleEl) {
    styleEl.remove();
    styleEl = null;
  }
  if (container) {
    container.innerHTML = "";
    container = null;
  }
  nextSeq = 0;
}
