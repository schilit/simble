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

import init, { WebA2dpSink } from "../pkg/simble.js";

let sink = null;
let timer = 0;
let logLen = 0;
let lastPostAt = 0;
let lastKey = "";

const ready = init();

function stopSink() {
  if (timer) clearInterval(timer);
  timer = 0;
  try {
    sink?.free(); // Drop closes the socket, releasing the bridge session
  } catch (_) {
    /* already gone */
  }
  sink = null;
  logLen = 0;
  lastKey = "";
}

function pump() {
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
    postMessage(
      { op: "pcm", pcm, rate: sink.sample_rate() || 44100, channels: sink.channels() || 2 },
      [pcm.buffer],
    );
  }
}

self.onmessage = async (e) => {
  const m = e.data;
  await ready;
  if (m.op === "sink-start") {
    stopSink();
    // The drop above closes the previous socket, but the bridge releases
    // the dongle only on its next pump (and then resets it); claiming again
    // in the same breath fails against our own abandoned session.
    setTimeout(() => {
      try {
        sink = new WebA2dpSink(m.url, m.name);
        timer = setInterval(pump, 20);
      } catch (err) {
        postMessage({ op: "error", message: String(err) });
      }
    }, 300);
  } else if (m.op === "sink-stop") {
    stopSink();
  }
};
