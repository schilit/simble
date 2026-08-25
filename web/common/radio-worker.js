// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// The radio lives here, not on the page.
//
// A page timer is the wrong heartbeat for a radio protocol: Chrome throttles
// a hidden tab's timers to ~1/s, and after five quiet minutes to ~1/min.
// Measured on the Audio page: 3 ticks in 2 seconds where 100 were asked for,
// the moment the tab lost focus — which is exactly when a person looks down
// at the phone they are pairing. SSP answers arrived seconds late, past the
// LMP timeouts, and every failure looked like a radio bug. Worker timers are
// exempt from visibility throttling, and postMessage delivery to a hidden
// page is not throttled either — so the protocol runs here at full rate and
// the page only renders what it is told.
//
// The page speaks one protocol to this worker regardless of controller:
//   { op: "sink-start", url, name }   url names the controller (a
//                                     `simble --usb` bridge socket, with
//                                     ?device=<selector> picking the dongle)
//   { op: "sink-stop" }
// and hears:
//   { op: "status", stage, frames, undecodable, rate, channels, failure, log }
//   { op: "pcm", pcm: Int16Array, rate, channels }   (buffer transferred)
//   { op: "error", message }

import init, { WebA2dpSink, WebA2dpSource } from "../pkg/simble.js";

let sink = null;
let source = null;
let timer = 0;
let logLen = 0;
let srcLogLen = 0;
let srcLastPostAt = 0;
let srcLastKey = "";
let lastNeedAskAt = 0;
let lastPostAt = 0;
let lastKey = "";
// Decoded PCM is accumulated and posted in ~100 ms batches, not per pump.
// One batch is one AudioBuffer on the page, and one buffer has one seam;
// posting every 20 ms pump gave the graph fifty seams a second, which is
// what "scratchy" is (see lc3-player.js's header — this lesson was already
// written down once).
let pcmChunks = [];
let pcmSamples = 0;

const ready = init();

/// One interval serves both devices; it runs while either exists.
function ensureTimer() {
  if (!timer) timer = setInterval(pump, 20);
}

function maybeStopTimer() {
  if (!sink && !source && timer) {
    clearInterval(timer);
    timer = 0;
  }
}

function stopSource() {
  try {
    source?.free();
  } catch (_) {
    /* already gone */
  }
  source = null;
  srcLogLen = 0;
  srcLastKey = "";
  maybeStopTimer();
}

function stopSink() {
  flushPcm(); // the tail of the stream still deserves to play
  try {
    sink?.free(); // Drop closes the socket, releasing the bridge session
  } catch (_) {
    /* already gone */
  }
  sink = null;
  logLen = 0;
  lastKey = "";
  maybeStopTimer();
}

function flushPcm() {
  if (!pcmSamples) return;
  const rate = sink?.sample_rate() || 44100;
  const channels = sink?.channels() || 2;
  const batch = new Int16Array(pcmSamples);
  let at = 0;
  for (const chunk of pcmChunks) {
    batch.set(chunk, at);
    at += chunk.length;
  }
  pcmChunks = [];
  pcmSamples = 0;
  postMessage({ op: "pcm", pcm: batch, rate, channels }, [batch.buffer]);
}

function pump() {
  pumpSource();
  pumpSink();
}

function pumpSource() {
  if (!source) return;
  try {
    source.tick(performance.now());
  } catch (e) {
    postMessage({ op: "source-error", message: String(e) });
    stopSource();
    return;
  }
  const st = JSON.parse(source.status_json(srcLogLen));
  srcLogLen = st.log_len;
  const key = `${st.stage}|${st.packets_sent}|${st.failure ?? ""}`;
  const now = Date.now();
  if (st.log.length || key !== srcLastKey || now - srcLastPostAt > 250) {
    srcLastKey = key;
    srcLastPostAt = now;
    postMessage({ op: "source-status", ...st });
  }
  // Ask the page for more PCM before the runner runs dry — one ask per
  // second, so a page with nothing left is not nagged fifty times a tick.
  if (source.pending_samples() < 88200 * 2 && now - lastNeedAskAt > 1000) {
    lastNeedAskAt = now;
    postMessage({ op: "source-need-pcm" });
  }
}

function pumpSink() {
  if (!sink) return;
  try {
    sink.tick();
  } catch (e) {
    postMessage({ op: "error", message: String(e) });
    stopSink();
    return;
  }
  const st = JSON.parse(sink.status_json(logLen));
  logLen = st.log_len;
  // Post on any news, else at most 5/s: the page renders state, and state
  // that has not changed is not worth a message every 20 ms.
  const key = `${st.stage}|${st.frames}|${st.failure ?? ""}`;
  const now = Date.now();
  if (st.log.length || key !== lastKey || now - lastPostAt > 200) {
    lastKey = key;
    lastPostAt = now;
    // The whole status object rides along: the sink's per-layer counters
    // (acl_in, media_sdus, host_errors, …) are the tracing surface now that
    // every controller path crosses this worker, and filtering them here is
    // how a lossy run stays unexplained.
    postMessage({
      op: "status",
      ...st,
      frames: st.frames,
      undecodable: st.undecodable_bytes,
      rate: st.sample_rate,
      channels: st.channels,
    });
  }
  const pcm = sink.take_pcm();
  if (pcm.length) {
    pcmChunks.push(pcm);
    pcmSamples += pcm.length;
  }
  // ~100 ms at the negotiated rate; mono streams just batch a little longer.
  if (pcmSamples >= ((sink.sample_rate() || 44100) / 10) * 2) flushPcm();
}

self.onmessage = async (e) => {
  const m = e.data;
  await ready;
  if (m.op === "source-start") {
    stopSource();
    setTimeout(() => {
      try {
        source = new WebA2dpSource(m.url, m.target || "");
        ensureTimer();
      } catch (err) {
        postMessage({ op: "source-error", message: String(err) });
      }
    }, 300);
  } else if (m.op === "source-stop") {
    stopSource();
  } else if (m.op === "source-pcm") {
    source?.queue_pcm(m.pcm);
  } else if (m.op === "source-finish") {
    source?.finish();
  } else if (m.op === "sink-start") {
    stopSink();
    // The drop above closes the previous socket, but the bridge releases
    // the dongle only on its next pump (and then resets it); claiming again
    // in the same breath fails against our own abandoned session.
    setTimeout(() => {
      try {
        sink = new WebA2dpSink(m.url, m.name, m.keys || "");
        ensureTimer();
      } catch (err) {
        postMessage({ op: "error", message: String(err) });
      }
    }, 300);
  } else if (m.op === "sink-stop") {
    stopSink();
  }
};
