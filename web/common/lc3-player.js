// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// Playing isochronous SDUs through the Web Audio graph.
//
// Lives in common/ rather than in the Audio page, because getting this right
// took several attempts and the next page that plays a stream should inherit
// the answers rather than rediscover them:
//
//   - The graph runs at the *stream's* sample rate, not the hardware rate.
//     At 44.1 kHz every 160-sample buffer was resampled independently, by a
//     ratio of 2.75625, so each 10 ms chunk picked up a filter transient at
//     both edges — 100 discontinuities a second, audible as a scratchy stream
//     even when every SDU arrives on time.
//   - A whole batch of SDUs becomes ONE AudioBuffer. Scheduling each SDU as
//     its own source node put a seam every 10 ms; a batch has one seam.
//   - The jitter lead is larger than the drain interval. Scheduling 20 ms
//     ahead while draining every 100 ms meant any late tick landed past the
//     cursor and playback snapped forward, gapping on nearly every batch.

const JITTER_LEAD_S = 0.25;

// A silence longer than this means the stream stopped and started again, not
// that it fell behind. Without the distinction every stream begins with one
// phantom underrun — the cursor is stale from whenever audio was last enabled —
// and a counter that is never zero is a counter nobody reads.
const RESTART_GAP_S = 0.5;

/// Creates a player for one stream configuration. `decode` turns one SDU into
/// an Int16Array of PCM; pass null to treat SDUs as raw 16-bit PCM.
export function createSduPlayer({ sampleRate }) {
  let audio = null;
  let gain = null;
  let cursor = 0;
  let lastPlayedAt = -Infinity; // context time of the previous batch
  const stats = { received: 0, played: 0, underruns: 0 };

  return {
    stats,

    /// Zeroes the counters so they describe one stream rather than the page's
    /// whole history — call it when a new stream starts.
    reset() {
      stats.received = 0;
      stats.played = 0;
      stats.underruns = 0;
      lastPlayedAt = -Infinity;
      if (audio) cursor = audio.currentTime;
    },

    /// Browsers require a user gesture before an AudioContext makes sound, so
    /// this must be called from a real click handler. A context created
    /// without one starts suspended: SDUs are still counted and scheduled,
    /// and nothing is heard — which looks exactly like a broken audio path.
    /// `state` below is what tells the two apart.
    enable() {
      if (audio) {
        audio.resume();
        return;
      }
      const Ctor = window.AudioContext || window.webkitAudioContext;
      try {
        audio = new Ctor({ sampleRate });
      } catch (e) {
        audio = new Ctor(); // some browsers refuse a non-hardware rate
      }
      gain = audio.createGain();
      gain.gain.value = 0;
      gain.connect(audio.destination);
      cursor = audio.currentTime;
      // Harmless if it is already running, and the difference between
      // "suspended" and "running" is the difference between silence and audio.
      audio.resume();
    },

    /// Releases the AudioContext. A page that is unmounted and remounted would
    /// otherwise leak one context per visit, and a browser caps how many a
    /// document may hold — after which `enable()` silently gets nothing.
    close() {
      if (!audio) return;
      const closing = audio;
      audio = null;
      gain = null;
      cursor = 0;
      closing.close();
    },

    /// The AudioContext's state, or "off" before one exists.
    get state() {
      return audio ? audio.state : "off";
    },

    get enabled() {
      return audio !== null;
    },

    /// Sets the output level, which is how a device's Volume State reaches
    /// the speakers. Deliberately quiet at the top.
    setVolume(volume, muted) {
      if (!audio) return;
      const level = muted ? 0 : 0.16 * Math.pow(volume / 255, 2);
      gain.gain.setTargetAtTime(level, audio.currentTime, 0.05);
    },

    /// Plays one batch of SDUs. `decode` is applied per SDU when given.
    play(sdus, decode) {
      stats.received += sdus.length;
      if (!audio || !sdus.length) return;

      const decoded = [];
      let total = 0;
      for (const bytes of sdus) {
        let pcm;
        if (decode) {
          try {
            pcm = decode(bytes);
          } catch (e) {
            continue;
          }
        } else {
          pcm = new Int16Array(bytes.buffer, bytes.byteOffset, bytes.byteLength / 2);
        }
        decoded.push(pcm);
        total += pcm.length;
      }
      if (!total) return;

      const buffer = audio.createBuffer(1, total, sampleRate);
      const channel = buffer.getChannelData(0);
      let at = 0;
      for (const pcm of decoded) {
        for (let i = 0; i < pcm.length; i++) channel[at + i] = pcm[i] / 32768;
        at += pcm.length;
      }

      const node = audio.createBufferSource();
      node.buffer = buffer;
      node.connect(gain);

      // Falling behind the clock means the source is not keeping up:
      // re-prime and count it, because that is what scratchy playback
      // sounds like and it should be visible rather than merely audible.
      let startAt = cursor;
      if (startAt < audio.currentTime) {
        if (stats.played > 0 && audio.currentTime - lastPlayedAt < RESTART_GAP_S) {
          stats.underruns++;
        }
        startAt = audio.currentTime + JITTER_LEAD_S;
      }
      node.start(startAt);
      cursor = startAt + buffer.duration;
      lastPlayedAt = audio.currentTime;
      stats.played += decoded.length;
    },
  };
}
