// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// A per-device controller strip: the slim bar above a device card that says
// which controller THIS device rides — in the browser, netsim, or a named
// USB dongle — echoing the page-level controller bar so "controller level
// sits above device level" reads at a glance.
//
// The page-level bar remains the *default* a device starts on; a strip is
// that device's override. The distinction earns its pixels the moment two
// devices ride different radios — a sink on real silicon answering a phone
// while a source streams into netsim — which the one-global-mode design
// could not even express.

const STYLE_ID = "simble-ctl-strip-style";
let uid = 0;

function injectStyles() {
  if (document.getElementById(STYLE_ID)) return;
  const style = document.createElement("style");
  style.id = STYLE_ID;
  style.textContent = `
  .ctl-strip { display:flex; align-items:center; gap:0.7rem; flex-wrap:wrap;
    padding:0.3rem 0.6rem; margin-bottom:0.55rem; font-size:0.78rem;
    color:var(--muted,#667); background:var(--panel-2,rgba(127,127,127,0.06));
    border:1px solid var(--border,#e3e5e8); border-radius:6px; }
  .ctl-strip .strip-label { font-weight:600; letter-spacing:0.06em;
    font-size:0.68rem; text-transform:uppercase; }
  .ctl-strip label { display:inline-flex; align-items:center; gap:0.25rem;
    cursor:pointer; }
  .ctl-strip select { font-size:0.78rem; max-width:11rem; }
  .ctl-strip .strip-why { flex-basis:100%; font-style:italic; opacity:0.8; }`;
  document.head.append(style);
}

/**
 * @param {object} options
 * @param {{kind:string, device:string}} options.value  initial choice
 * @param {Array<{selector:string, product:string}>} [options.dongles]
 * @param {(value:{kind:string,device:string}) => ?string} options.onChange
 *        return a string to refuse (shown on the strip's why line), or
 *        null/undefined to accept.
 * @param {string} [options.why] a standing note under the choices
 */
export function createControllerStrip({ value, dongles = [], onChange, why = "" }) {
  injectStyles();
  const group = `ctl-strip-${uid++}`;
  const el = document.createElement("div");
  el.className = "ctl-strip";

  const label = document.createElement("span");
  label.className = "strip-label";
  label.textContent = "controller";
  el.append(label);

  const current = { ...value };
  const radios = new Map();
  const whyEl = document.createElement("span");
  whyEl.className = "strip-why";
  whyEl.textContent = why;

  const dongleSelect = document.createElement("select");
  dongleSelect.setAttribute("aria-label", "which dongle");

  function renderDongles(list) {
    dongleSelect.innerHTML = "";
    for (const d of list) {
      const option = document.createElement("option");
      option.value = d.selector;
      option.textContent = `${d.selector} — ${d.product}`;
      dongleSelect.append(option);
    }
    dongleSelect.disabled = list.length === 0;
    if (current.device) dongleSelect.value = current.device;
    dongleSelect.hidden = current.kind !== "usb";
  }

  function commit(next) {
    const refusal = onChange?.(next);
    if (refusal) {
      whyEl.textContent = refusal;
      set(current); // snap the radios back
      return;
    }
    whyEl.textContent = why;
    current.kind = next.kind;
    current.device = next.device;
    dongleSelect.hidden = next.kind !== "usb";
  }

  for (const [kind, text] of [["in-page", "in-page"], ["websocket", "netsim"], ["usb", "usb"]]) {
    const wrap = document.createElement("label");
    const radio = document.createElement("input");
    radio.type = "radio";
    radio.name = group;
    radio.value = kind;
    radio.addEventListener("change", () => {
      if (!radio.checked) return;
      commit({ kind, device: dongleSelect.value || "" });
    });
    radios.set(kind, radio);
    wrap.append(radio, document.createTextNode(text));
    el.append(wrap);
  }
  el.append(dongleSelect, whyEl);
  dongleSelect.addEventListener("change", () => {
    if (current.kind === "usb") commit({ kind: "usb", device: dongleSelect.value });
  });

  /** Sets the choice without firing onChange — for a partner strip's echo. */
  function set(next) {
    current.kind = next.kind;
    current.device = next.device ?? current.device;
    for (const [kind, radio] of radios) radio.checked = kind === next.kind;
    dongleSelect.hidden = next.kind !== "usb";
    if (current.device) dongleSelect.value = current.device;
  }

  renderDongles(dongles);
  set(value);

  return {
    el,
    set,
    setDongles: renderDongles,
    value: () => ({ ...current, device: dongleSelect.value || current.device }),
    setWhy: (text) => { whyEl.textContent = text; },
  };
}
