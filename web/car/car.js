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
// the actual bytes SimBLE's HfProtocol and AgProtocol produced — and those
// bytes go into an RFCOMM data link, an L2CAP channel on PSM 3, an ACL
// connection, and across the simulated BR/EDR controller before the far end's
// ClassicHost hands them up. Toggle "bytes" to see the hex of each line as it
// was written.
//
// The two endpoints are two BR/EDR devices with real BD_ADDRs in one
// SceneEngine. The head unit is told the phone's address and nothing else:
// the Class of Device, the name and the RFCOMM channel it opens are all
// learned over the air, by inquiry, Remote Name Request and SDP. Turn the
// phone's inquiry scan off and the head unit never finds it — which is the
// difference between this page and the one it replaced.
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
///
/// The link is real now, but it is real *in this page*: WebCarKit builds its
/// own SceneEngine around the simulated BR/EDR controller. Nothing in SimBLE
/// puts a ClassicHost on a WebSocket, so the netsim backend still cannot run
/// this domain — the reason has changed, the answer has not.
export const SUPPORTS = { "in-page": true,
  "websocket": "both hosts run on this page's own simulated BR/EDR controller — no transport carries a ClassicHost to netsim" };


// One timer for both endpoints. Chrome throttles a hidden tab hard enough
// that a device hosted in one misses protocol deadlines, so a two-page design
// would stall silently; both halves live here and share this interval.
const TICK_MS = 120;

// One `WebCarKit` holds the whole scene — both hosts and the controller
// between them — and dropping it is the only teardown there is, so neither
// header offers a stop that would take its neighbour down with it.
const SHARED_KIT = "one scene — both devices stop together";

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

.answer { background: var(--good); border-color: var(--good); color: #fff; font-weight: 600; }
.decline { background: var(--bad); border-color: var(--bad); color: #fff; font-weight: 600; }
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
.tape .unsolicited > span:nth-child(3) { color: var(--warn); }
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

/* What the head unit learned over the air, in the order it learned it. The
   middle column is the answer, so it is the one that reads as data. */
.inds.bredr td:first-child { width: 9rem; white-space: nowrap; }
.inds.bredr td:nth-child(2) { color: var(--accent); white-space: nowrap; padding-right: 1rem; }
.inds.bredr td:last-child { font-family: inherit; font-size: var(--fs-meta); width: 55%; }

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

const ABOUT = `<p>A phone and a car head unit, as two Bluetooth Classic devices on one simulated
   controller. The head unit is given the phone's BD_ADDR and nothing else: it inquires for it,
   asks its name, pages it to open an ACL connection, searches its SDP server for the Hands-Free
   Audio Gateway record, opens the RFCOMM channel that record names, and only then starts the
   service-level connection that lets a call ring.</p>
   <p>Every AT line in the dialogue is bytes <code>HfProtocol</code> and <code>AgProtocol</code>
   actually produced, and every one of them crosses RFCOMM, L2CAP and an ACL connection to reach
   the other end. Nothing is wired straight through.</p>
   <p>Call <em>audio</em> rides a real SCO/eSCO link. When a call arrives, the phone runs the
   codec connection procedure over AT and then opens a synchronous connection over HCI — a
   handle of its own, separate from the ACL — and payload crosses it in both directions until
   the call ends, which takes the audio and leaves the service-level connection standing. What
   is <em>not</em> here is a codec: the two ends agree on mSBC, ask the controller for the
   transparent air mode it needs, and then carry a counter pattern rather than speech.</p>
   <p>Pairing is still absent: this link is unauthenticated and unencrypted, where a real head
   unit would bond once and remember.</p>`;

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
      Each one leaves as a UIH frame and arrives on the other side of an ACL connection;
      the profile itself never learns that, which is the property that lets the same
      <code>AT</code> layer sit on a serial cable.
    </p>
  </section>

  <!-- ============ the stack ============ -->
  <section class="panel full">
    <h2>The stack underneath</h2>
    <ul class="stages" id="car-steps"></ul>
    <div class="layers">
      <span class="real">AT / HFP</span><b>rides</b>
      <span class="real">RFCOMM</span><b>rides</b>
      <span class="real">L2CAP PSM 3</span><b>rides</b>
      <span class="real">ACL / BR/EDR controller</span><b>·</b>
      <span class="real" id="car-sco-box">SCO / eSCO</span>
    </div>
    <p class="hint" style="margin-top:0.7rem">
      Every box is exercised on this page. Each AT line above becomes a UIH frame with a
      credit octet, an L2CAP SDU on PSM 3, and an ACL packet handed to the simulated
      controller in <code>controller::sim</code>, which routes it to the other device's
      <code>ClassicHost</code>. Credit accounting is live
      (<span id="car-credits" class="mono">—</span>).
      The call audio takes the other branch: not an L2CAP payload at all, but H4 packet type
      <code>0x03</code> on a synchronous handle the controller allocates separately
      (<span id="car-sco" class="mono">—</span>).
    </p>
  </section>

  <!-- ============ how the two found each other ============ -->
  <section class="panel full">
    <h2>How they found each other</h2>
    <p class="hint" style="margin-top:0">
      The head unit is given one thing — the phone's address. Everything in the right-hand
      column it learned over the air, in the order below.
    </p>
    <table class="inds bredr">
      <tbody>
        <tr><td class="n">head unit</td><td id="car-hu-addr">—</td>
            <td class="n">its own BD_ADDR, and the only device it pages</td></tr>
        <tr><td class="n">inquiry</td><td id="car-found">—</td>
            <td class="n">who answered the General Inquiry Access Code</td></tr>
        <tr><td class="n">remote name</td><td id="car-found-name">—</td>
            <td class="n">an inquiry result carries no name; this took its own request</td></tr>
        <tr><td class="n">class of device</td><td id="car-found-cod">—</td>
            <td class="n">the number a pairing list turns into a phone icon</td></tr>
        <tr><td class="n">ACL</td><td id="car-acl">—</td>
            <td class="n">the connection handle every layer above rides inside</td></tr>
        <tr><td class="n">BR/EDR phase</td><td id="car-classic">—</td>
            <td class="n">where <code>ClassicDevice</code>'s plan has got to</td></tr>
        <tr><td class="n">audio</td><td id="car-audio">—</td>
            <td class="n">the SCO/eSCO handle, which is not the ACL's — audio addressed to
            the ACL handle reaches nobody</td></tr>
        <tr><td class="n">frames carried</td><td id="car-audio-frames">—</td>
            <td class="n">payload taken <em>off</em> the link at each end, which is what
            proves the routing rather than the writing</td></tr>
      </tbody>
    </table>
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
        <p>Everything on this page: inquiry and paging, SDP discovery, an RFCOMM DLC over
           L2CAP, the Service Level Connection, and the call state machine, both
           directions.</p>
      </div>
      <div class="role built">
        <span class="tag">built</span>
        <h4>SCO / eSCO — the call audio</h4>
        <p>The synchronous link is real: <code>Setup Synchronous Connection</code> answered
           with a Command Status, a <code>Connection Request</code> at the head unit whose
           link type is eSCO rather than ACL, <code>Accept Synchronous Connection
           Request</code> back, and a <code>Synchronous Connection Complete</code> at both
           ends carrying one handle. Payload then rides H4 type <code>0x03</code> on that
           handle. Codec negotiation (<code>AT+BAC</code>, <code>+BCS</code>) now decides
           something: mSBC turns into a transparent Voice Setting and an EV3 packet-type
           mask, which is what makes the controller build an eSCO link instead of a plain
           SCO one.</p>
        <p><strong>No codec, and no radio.</strong> Nothing encodes: the bytes on the link
           are a counter pattern, carried untouched, and simble's SBC and LC3 encoders are
           not wired in. Nothing is scheduled either — no reserved slots, no 3.75 ms eSCO
           interval, no retransmission window, no loss. Sequencing and state are modelled;
           the air interface is what rootcanal and netsim are for.</p>
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
  inquiring: "BR/EDR: inquiry, then the remote name",
  paging: "BR/EDR: paging — opening the ACL connection",
  discovering: "SDP: searching the phone",
  "opening-dlc": "RFCOMM: L2CAP PSM 3, then the data link connection",
  "establishing-slc": "HFP: service level connection",
  configuring: "HFP: head-unit setup",
  ready: "linked",
  failed: "failed",
};

// The audio connection's states, in the words that say what is happening
// rather than what the enum is called. HFP's codec connection procedure runs
// *before* any HCI, so "negotiating" is an AT state and "connecting" is the
// one where a Setup Synchronous Connection is in flight.
const AUDIO_TEXT = {
  disconnected: "no audio connection",
  negotiating: "codec connection: +BCS out, waiting for AT+BCS",
  connecting: "codec settled — Setup Synchronous Connection in flight",
  connected: "carrying audio",
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
  // script and neither gets a pen. They do have BD_ADDRs, though — read them
  // out of the kit rather than writing them twice, so the header can never
  // disagree with the device it names.
  const identity = JSON.parse(kit.status_json(0));

  heads.ag = createDeviceHeader({
    name: "Phone",
    kind: "Audio Gateway · AgProtocol",
    accent: "accent",
    address: identity.phone_address,
    dotMeans: "the head unit has an established Service Level Connection to it",
    run: { running: true, disabled: true, reason: SHARED_KIT },
  });
  $("car-ag-head").append(heads.ag.el);

  heads.hf = createDeviceHeader({
    name: "Head Unit",
    kind: "Hands-Free · HfProtocol",
    accent: "good",
    address: identity.head_unit_address,
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
        : ready
          ? `${CALL_TEXT[status.call] || status.call} · codec ${status.codec}` +
            (status.sco_handle != null ? ` · ${status.sco_link_type} audio` : "")
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

    // -- the audio connection --
    // Solid only while a link really exists. The AT handshake settling a
    // codec is not the same event as a synchronous handle being allocated,
    // and drawing the box solid on the strength of the first would be
    // exactly the fiction this page replaced.
    const audioUp = status.sco_handle != null;
    $("car-sco-box").className = audioUp ? "real" : "absent";
    $("car-sco").textContent = audioUp
      ? `${status.sco_link_type} handle 0x${status.sco_handle.toString(16).padStart(4, "0")},` +
        ` ${status.sco_air_mode} air mode, codec ${status.codec}`
      : AUDIO_TEXT[status.audio] || "no audio connection";
    $("car-audio").textContent = audioUp
      ? `${status.sco_link_type} handle 0x${status.sco_handle.toString(16).padStart(4, "0")}` +
        ` · air mode ${status.sco_air_mode}`
      : AUDIO_TEXT[status.audio] || "—";
    $("car-audio-frames").textContent =
      status.audio_frames_to_car || status.audio_frames_to_phone
        ? `${status.audio_frames_to_car} arrived at the head unit,` +
          ` ${status.audio_frames_to_phone} at the phone`
        : "—";

    // -- how the two found each other --
    // Everything here except the head unit's own address arrived over the
    // air, so an em-dash is the honest answer until it does.
    const found = status.discovered.find((d) => d.address === status.phone_address);
    $("car-hu-addr").textContent = status.head_unit_address;
    $("car-found").textContent = found ? found.address : "nothing yet";
    $("car-found-name").textContent = found && found.name ? found.name : "—";
    $("car-found-cod").textContent = found ? found.class_of_device : "—";
    $("car-acl").textContent =
      status.acl_handle == null
        ? "—"
        : `handle 0x${status.acl_handle.toString(16).padStart(4, "0")}` +
          (status.phone_linked ? " · the phone sees it too" : " · the phone has not seen it");
    $("car-classic").textContent = status.classic;
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
