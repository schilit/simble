// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// The one controller selector, in the Devices shell's chrome.
//
// It used to be six selectors on six pages, all writing one localStorage key,
// with a page called "Controllers" that explained the choice but could not
// make it — and four domains that showed no control at all because they only
// run in-page. Switching tabs made the control appear and disappear, which
// reads as a bug rather than as a capability difference. The commonest
// question about these pages was where the controller actually lives.
//
// So: one bar, always present, showing which controller you are on. Each
// domain declares what it supports; an option it cannot honour is disabled
// with the reason visible, rather than hidden. A control that says why it is
// unavailable is information; one that vanishes is a mystery.

const STORE_KEY = "simble-backend";

/// The controllers a domain may declare. Ids match the values the existing
/// selector already writes, so a stored choice carries over.
export const CONTROLLERS = [
  { id: "in-page", label: "In browser", note: "built-in wasm controller" },
  { id: "websocket", label: "netsim", note: "netsim / rootcanal over a WebSocket" },
];

/// Reads the current choice. A stored value the caller cannot honour is not
/// this module's problem to fix — the caller decides what to fall back to.
export function currentController() {
  try {
    return localStorage.getItem(STORE_KEY) || "in-page";
  } catch (e) {
    return "in-page"; // private window, or site data blocked
  }
}

/// Builds the bar. `supports` maps a controller id to `true`, or to a string
/// explaining why this domain cannot use it.
///
/// Returns `{ el, setSupports, selected }`.
export function createControllerBar({ supports, onChange }) {
  const el = document.createElement("div");
  el.className = "controller-bar";

  const label = document.createElement("span");
  label.className = "controller-label";
  label.textContent = "Controller";
  el.append(label);

  const help = document.createElement("a");
  help.className = "controller-help";
  help.href = "../controllers/";
  help.textContent = "how controllers work ↗";

  let selected = currentController();
  const inputs = [];

  function render(map) {
    // A selection this domain cannot honour is corrected here rather than
    // left showing: a disabled option that is also the checked one tells the
    // reader they are on a controller the page is not using.
    if (map[selected] !== true) {
      const fallback = CONTROLLERS.find((c) => map[c.id] === true);
      if (fallback) {
        selected = fallback.id;
        try {
          localStorage.setItem(STORE_KEY, selected);
        } catch (e) {
          /* not remembered, but correct for this page */
        }
      }
    }
    for (const input of inputs) input.wrap.remove();
    inputs.length = 0;
    for (const c of CONTROLLERS) {
      const why = map[c.id];
      const usable = why === true;

      const wrap = document.createElement("label");
      wrap.className = "controller-choice" + (usable ? "" : " unusable");

      const radio = document.createElement("input");
      radio.type = "radio";
      radio.name = "simble-controller";
      radio.value = c.id;
      radio.disabled = !usable;
      radio.checked = c.id === selected;

      const text = document.createElement("span");
      text.innerHTML = `<b>${c.label}</b> <span class="controller-note">— ${
        usable ? c.note : String(why)
      }</span>`;

      radio.addEventListener("change", () => {
        if (!radio.checked) return;
        selected = c.id;
        try {
          localStorage.setItem(STORE_KEY, c.id);
        } catch (e) {
          /* the choice still applies to this page, it just is not remembered */
        }
        onChange?.(c.id);
      });

      wrap.append(radio, text);
      el.insertBefore(wrap, help);
      inputs.push({ wrap });
    }
  }

  // The help link is appended first: render() inserts each choice before it,
  // so it has to already be a child.
  el.append(help);
  render(supports);

  return {
    el,
    get selected() {
      return selected;
    },
    /// Re-declares support when the mounted domain changes.
    setSupports(map) {
      render(map);
    },
  };
}
