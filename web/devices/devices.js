// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// The domain shell: one page, one timer, one wasm instance, several domains.
//
// Each domain (Media, Car, HID, Ranging) is an ES module exporting
// `mount(root)` and `unmount()`. This shell owns the tab strip and the
// lifecycle; it knows nothing about Bluetooth.
//
// Why a shell rather than a page per domain: a browser's timer budget is
// per-tab, and Chrome *intensively* throttles a hidden tab that produces no
// sound of its own — which describes an audio source exactly. Measured twice
// during development: a 2000-SDU stream delivered 256 before stalling when its
// source sat in a background tab. Domains as tabs on one visible page makes
// that failure structurally impossible instead of something each page has to
// warn about.
//
// Only the active domain runs. Switching unmounts the previous one, which must
// clear its timer, close its sockets and drop its wasm devices — otherwise
// netsim accumulates ghost devices (it does not synthesise a disconnect when a
// WebSocket drops, so a dead device lingers at the same address).

const DOMAINS = [
  { id: "media", label: "Media", module: "../audio/audio.js",
    blurb: "LE Audio: a source streaming LC3 to a sink over a real CIS" },
  { id: "car", label: "Car", module: "../car/car.js",
    blurb: "Hands-free telephony: SDP, RFCOMM and AT over one link" },
  { id: "hid", label: "HID", module: "../hid/hid.js",
    blurb: "HID over GATT: a keyboard and mouse, and a host decoding them" },
  { id: "ranging", label: "Ranging", module: "../ranging/ranging.js",
    blurb: "Distance: channel sounding against RSSI path loss" },
];

const DEFAULT_ID = "media";

let current = null;      // { id, module }
let switching = 0;       // guards against overlapping switches

const $ = (id) => document.getElementById(id);

function domainFor(hash) {
  const id = (hash || "").replace(/^#/, "");
  return DOMAINS.find((d) => d.id === id) || DOMAINS.find((d) => d.id === DEFAULT_ID);
}

function renderTabs(activeId) {
  const strip = $("tabs");
  strip.innerHTML = "";
  for (const domain of DOMAINS) {
    const tab = document.createElement("a");
    tab.href = `#${domain.id}`;
    tab.textContent = domain.label;
    tab.className = "domain-tab";
    if (domain.id === activeId) tab.setAttribute("aria-current", "page");
    strip.append(tab);
  }
  const active = DOMAINS.find((d) => d.id === activeId);
  $("blurb").textContent = active ? active.blurb : "";
}

async function show(domain) {
  const token = ++switching;

  // Tear the previous domain down before loading the next. If its unmount
  // throws we still proceed: a half-cleaned domain is better than a shell
  // stuck on a broken tab, and the error is worth seeing.
  if (current) {
    try {
      current.module.unmount();
    } catch (e) {
      console.error("unmount failed", e);
    }
    current = null;
  }
  $("stage").innerHTML = "";
  renderTabs(domain.id);
  document.title = `SimBLE Devices — ${domain.label}`;

  let module;
  try {
    module = await import(domain.module);
  } catch (e) {
    $("stage").innerHTML =
      `<section class="panel"><h2>${domain.label} unavailable</h2>` +
      `<div class="error">could not load ${domain.module}: ${e}</div></section>`;
    return;
  }
  // A faster tab click may have superseded this load.
  if (token !== switching) return;

  try {
    await module.mount($("stage"));
    current = { id: domain.id, module };
  } catch (e) {
    $("stage").innerHTML =
      `<section class="panel"><h2>${domain.label} failed to start</h2>` +
      `<div class="error">${e}</div></section>`;
  }
}

window.addEventListener("hashchange", () => show(domainFor(location.hash)));

// Leaving the page entirely still deserves a clean teardown, so the devices
// disappear from netsim rather than lingering as ghosts.
window.addEventListener("pagehide", () => {
  if (current) {
    try {
      current.module.unmount();
    } catch (e) {
      /* nothing useful to do while unloading */
    }
    current = null;
  }
});

const initial = domainFor(location.hash);
if (!location.hash) location.replace(`#${initial.id}`);
show(initial);
