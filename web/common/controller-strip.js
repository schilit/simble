// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// A per-device controller strip: the slim line above a device card that says
// which controller THIS device rides. One dropdown, two kinds of entry:
// the page's simulator (whichever the top bar picked — in-page or netsim),
// or a named USB dongle:
//
//   CONTROLLER [ usb: 02.3.1 — CSR8510 ▾ ]
//
// The simulator is deliberately ONE entry, not two. In-page vs netsim is a
// page-level choice: in-page is a single link hosting both devices, and a
// netsim/in-page split leaves the two with nobody to talk to — nobody would
// split devices across simulators, and the strip should not offer it. What
// devices genuinely split across is simulated vs real: a sink on silicon
// answering a phone while the source streams into netsim.

const STYLE_ID = "simble-ctl-strip-style";

function injectStyles() {
  if (document.getElementById(STYLE_ID)) return;
  const style = document.createElement("style");
  style.id = STYLE_ID;
  style.textContent = `
  .ctl-strip { display:flex; align-items:center; gap:0.6rem; flex-wrap:wrap;
    padding:0.3rem 0.6rem; margin-bottom:0.55rem; font-size:0.78rem;
    color:var(--muted,#667); background:var(--panel-2,rgba(127,127,127,0.06));
    border:1px solid var(--border,#e3e5e8); border-radius:6px; }
  .ctl-strip .strip-label { font-weight:600; letter-spacing:0.06em;
    font-size:0.68rem; text-transform:uppercase; }
  .ctl-strip select { font-size:0.78rem; max-width:16rem; }
  .ctl-strip .strip-why { flex-basis:100%; font-style:italic; opacity:0.8; }`;
  document.head.append(style);
}

/// `value` ↔ option encoding: the simulator is "sim"; a usb choice is
/// "usb:<selector>".
const encode = (v) => (v.kind === "usb" ? `usb:${v.device ?? ""}` : "sim");
const decode = (text) =>
  text.startsWith("usb:")
    ? { kind: "usb", device: text.slice(4) }
    : { kind: "sim", device: "" };

/**
 * @param {object} options
 * @param {{kind:string, device:string}} options.value  initial choice
 * @param {string} [options.simLabel] what the one simulator entry is called
 *        (e.g. "simulated — netsim"); follows the page-level bar.
 * @param {Array<{selector:string, product:string}>} [options.dongles]
 * @param {(value:{kind:string,device:string}) => ?string} options.onChange
 *        return a string to refuse (shown on the strip's why line), or
 *        null/undefined to accept.
 * @param {string} [options.why] a standing note under the choice
 */
export function createControllerStrip({
  value,
  simLabel = "simulated",
  dongles = [],
  onChange,
  why = "",
}) {
  injectStyles();
  const el = document.createElement("div");
  el.className = "ctl-strip";

  const label = document.createElement("span");
  label.className = "strip-label";
  label.textContent = "controller";

  const pick = document.createElement("select");
  pick.setAttribute("aria-label", "which controller this device rides");

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
    add("sim", simLabel);
    for (const d of known) add(`usb:${d.selector}`, `usb: ${d.selector} — ${d.product}`);
    // A stored usb choice whose dongle is not (yet) listed still renders,
    // so a slow /devices answer cannot silently rewrite the choice.
    if (current.kind === "usb" && !known.some((d) => d.selector === current.device)) {
      add(`usb:${current.device}`, `usb: ${current.device || "(no dongle)"}`);
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
    setSimLabel: (text) => {
      simLabel = text;
      renderOptions();
    },
    value: () => ({ ...current }),
    setWhy: (text) => {
      whyEl.textContent = text;
    },
  };
}
