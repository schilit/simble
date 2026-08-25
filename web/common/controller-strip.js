// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// A per-device dongle strip: the slim line above a device card, shown only
// when the page runs on USB, that says which dongle THIS device rides:
//
//   USB DONGLE [ 02.3.1 — CSR8510 ▾ ]
//
// Nothing else belongs in it. Which *simulator* a page uses is the page
// bar's choice (nobody splits two devices across simulators — they would
// have nobody to talk to), and a simulated entry here only re-asked a
// question the bar had answered. The one per-device question USB raises is
// which silicon.

const STYLE_ID = "simble-ctl-strip-style";

function injectStyles() {
  if (document.getElementById(STYLE_ID)) return;
  const style = document.createElement("style");
  style.id = STYLE_ID;
  style.textContent = `
  .ctl-strip { display:flex; align-items:center; gap:0.6rem; flex-wrap:wrap;
    padding:0.3rem 0.6rem; margin-bottom:0.3rem; font-size:0.78rem;
    color:var(--muted,#667); background:var(--panel-2,rgba(127,127,127,0.06));
    border:1px solid var(--border,#e3e5e8); border-radius:6px; }
  .ctl-strip .strip-label { font-weight:600; letter-spacing:0.06em;
    font-size:0.68rem; text-transform:uppercase; }
  .ctl-strip select { font-size:0.78rem; max-width:16rem; }
  .ctl-strip .strip-why { flex-basis:100%; font-style:italic; opacity:0.8; }`;
  document.head.append(style);
}

const encode = (v) => v.device ?? "";
const decode = (text) => ({ kind: "usb", device: text });

/**
 * @param {object} options
 * @param {{kind:string, device:string}} options.value  initial choice
 * @param {Array<{selector:string, product:string}>} [options.dongles]
 * @param {(value:{kind:string,device:string}) => ?string} options.onChange
 *        return a string to refuse (shown on the strip's why line), or
 *        null/undefined to accept.
 * @param {string} [options.why] a standing note under the choice
 */
export function createControllerStrip({ value, dongles = [], onChange, why = "" }) {
  injectStyles();
  const el = document.createElement("div");
  el.className = "ctl-strip";

  const label = document.createElement("span");
  label.className = "strip-label";
  label.textContent = "usb dongle";

  const pick = document.createElement("select");
  pick.setAttribute("aria-label", "which dongle this device rides");

  const whyEl = document.createElement("span");
  whyEl.className = "strip-why";
  whyEl.textContent = why;

  el.append(label, pick, whyEl);

  const current = { ...value };
  let known = dongles;

  function renderOptions() {
    pick.innerHTML = "";
    const add = (valueText, text) => {
      const option = document.createElement("option");
      option.value = valueText;
      option.textContent = text;
      pick.append(option);
    };
    for (const d of known) add(d.selector, `${d.selector} — ${d.product}`);
    // A stored choice whose dongle is not (yet) listed still renders, so a
    // slow /devices answer cannot silently rewrite the choice.
    if (current.device && !known.some((d) => d.selector === current.device)) {
      add(current.device, current.device);
    }
    pick.value = encode(current);
  }

  pick.addEventListener("change", () => {
    const next = decode(pick.value);
    const refusal = onChange?.(next);
    if (refusal) {
      whyEl.textContent = refusal;
      pick.value = encode(current); // snap back
      return;
    }
    whyEl.textContent = why;
    Object.assign(current, next);
  });

  /** Sets the choice without firing onChange — for a partner strip's echo. */
  function set(next) {
    current.kind = next.kind;
    current.device = next.device ?? "";
    renderOptions();
  }

  renderOptions();

  return {
    el,
    set,
    setDongles: (list) => {
      known = list;
      renderOptions();
    },
    value: () => ({ ...current }),
    setWhy: (text) => {
      whyEl.textContent = text;
    },
  };
}
