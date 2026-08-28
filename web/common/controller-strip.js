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
  .ctl-strip select { font-size:0.78rem; flex:1 1 auto; min-width:0; max-width:24rem; }
  .ctl-strip .strip-why { flex-basis:100%; font-style:italic; opacity:0.8; }
  /* An empty why still claimed a whole flex row, padding the strip's bottom
     with a blank line. */
  .ctl-strip .strip-why:empty { display:none; }`;
  document.head.append(style);
}

const encode = (v) => v.device ?? "";
// A phone is not a dongle we own, but it *is* an answer to this strip's one
// question — which silicon this device rides. So it belongs in the same
// select rather than in a control beside it.
const decode = (text) => ({
  kind: text === PHONE || text.startsWith(`${PHONE}:`) ? "phone" : "usb",
  device: text,
});
const PHONE = "phone";
// The strip's category label. Constant: a phone and a dongle are both real
// radio, so what is picked never changes it — set once, not per render.
const STRIP_LABEL = "Real radio";

/**
 * @param {object} options
 * @param {{kind:string, device:string}} options.value  initial choice
 * @param {Array<{selector:string, product:string}>} [options.dongles]
 * @param {Array<{value:string, text:string}>} [options.extras] non-dongle
 *        choices (a phone running the sink app) listed after the dongles
 * @param {(value:{kind:string,device:string}) => ?string} options.onChange
 *        return a string to refuse (shown on the strip's why line), or
 *        null/undefined to accept.
 * @param {string} [options.why] a standing note under the choice
 */
export function createControllerStrip({
  value,
  dongles = [],
  extras = [],
  onChange,
  why = "",
}) {
  injectStyles();
  const el = document.createElement("div");
  el.className = "ctl-strip";

  const label = document.createElement("span");
  label.className = "strip-label";
  label.textContent = STRIP_LABEL;

  const pick = document.createElement("select");
  pick.setAttribute("aria-label", "which dongle this device rides");

  const whyEl = document.createElement("span");
  whyEl.className = "strip-why";
  whyEl.textContent = why;

  el.append(label, pick, whyEl);

  const current = { ...value };
  let known = dongles;
  let known_extras = extras;
  // The value the partner strip has taken: one radio cannot be both ends of a
  // link, so its option is shown disabled here rather than selectable-then-
  // refused. Null when there is nothing to block.
  let disabledValue = null;

  function renderOptions() {
    pick.innerHTML = "";
    const add = (valueText, text) => {
      const option = document.createElement("option");
      option.value = valueText;
      option.textContent = text;
      if (valueText && valueText === disabledValue) option.disabled = true;
      pick.append(option);
    };
    for (const d of known) add(d.selector, `${d.selector} — ${d.product}`);
    for (const e of known_extras) add(e.value, e.text);
    // A stored choice whose dongle is not (yet) listed still renders, so a
    // slow /devices answer cannot silently rewrite the choice.
    if (
      current.device
      && !known.some((d) => d.selector === current.device)
      && !known_extras.some((e) => e.value === current.device)
    ) {
      add(current.device, current.device);
    }
    pick.value = encode(current);
  }

  pick.addEventListener("change", () => {
    const next = decode(pick.value);
    // Clear to the standing note *before* the handler runs, so a handler that
    // sets its own note is not immediately overwritten by this one.
    whyEl.textContent = why;
    const refusal = onChange?.(next);
    if (refusal) {
      whyEl.textContent = refusal;
      pick.value = encode(current); // snap back
      return;
    }
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
    /// The non-dongle choices, which arrive asynchronously: the bridge has to
    /// ask adb what phones exist before this strip can offer them.
    setExtras: (list) => {
      known_extras = list;
      renderOptions();
    },
    /// Greys out the option matching `value` — the partner strip's choice — so
    /// one radio cannot be picked as both ends. Null/empty clears it.
    setDisabled: (value) => {
      disabledValue = value || null;
      renderOptions();
    },
    value: () => ({ ...current }),
    setWhy: (text) => {
      whyEl.textContent = text;
    },
  };
}
