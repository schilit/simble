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
  // Id and label only. Each option used to carry a `note` rendered beside its
  // button; the reasons moved into the one sentence under the row and the
  // note stopped being read, but the field and the comment describing it
  // stayed behind for long enough to look load-bearing. Whatever
  // distinguishes the two controllers is in `why` below, said once.
  { id: "in-page", label: "In browser" },
  { id: "websocket", label: "netsim" },
  // Real radio, whatever holds it: a `simble --usb` bridge with a physical
  // dongle, and — where the page supports it — a phone running SimBLE Android
  // reached through that same bridge. The id stays `usb` because that is the
  // bridge it goes through; the label is the *category*, since "USB dongle"
  // named one member of it as though it were the whole. The bars already say
  // "RF", so the selector says the spelled form of the same word.
  { id: "usb", label: "Real radio" },
];

/// What a domain that says nothing about a controller means: it has not been
/// wired for it. Spelled here once so eight SUPPORTS maps do not each carry
/// the sentence.
const NOT_WIRED = "this page has not been wired for it yet";

/// Where the `simble --usb` bridge answers. The URL is controller
/// configuration, so it lives here in the bar with the rest of the
/// controller choice — a domain asks [`usbBridgeUrl`] rather than growing
/// its own field. Picking a *dongle* stays each domain's business.
const BRIDGE_KEY = "simble-usb-bridge";
const BRIDGE_DEFAULT = "ws://127.0.0.1:32323/";

/// The bridge WebSocket URL as configured in the bar (persisted per origin).
export function usbBridgeUrl() {
  try {
    return localStorage.getItem(BRIDGE_KEY) || BRIDGE_DEFAULT;
  } catch (e) {
    return BRIDGE_DEFAULT;
  }
}

/// The bridge's HTTP side, where /devices answers.
export function usbBridgeHttp() {
  return usbBridgeUrl().trim().replace(/^ws/, "http").replace(/\/+$/, "");
}

/// Decorates the Real radio option's label with how many real endpoints the
/// bridge sees — dongles plus any phones running SimBLE Android — e.g.
/// "Real radio (3)". When no bridge answers, says how to start one, here where
/// the choice is offered rather than on some page's card. Instructions for a
/// thing already running are noise, so the sentence appears only when needed.
///
/// Counting phones matters: a page whose sink is a phone has zero dongles and
/// one endpoint, and "(0)" beside a working choice would read as unavailable.
async function decorateUsbCount(nameEl, whyEl) {
  try {
    const http = usbBridgeHttp();
    const { devices } = await (await fetch(`${http}/devices`)).json();
    let count = Array.isArray(devices) ? devices.length : 0;
    // Phones are the newer half of "real radio" and answer a separate route.
    // A bridge too old to serve it just leaves the count at the dongles.
    try {
      const { phones } = await (await fetch(`${http}/phones`)).json();
      if (Array.isArray(phones)) count += phones.filter((p) => p.running).length;
    } catch (_) {
      /* no /phones on this bridge; dongles alone */
    }
    nameEl.textContent = `Real radio (${count})`;
  } catch (_) {
    const hint = " Real radio needs its bridge: run simble --usb --ws 32323.";
    if (!whyEl.textContent.includes("needs its bridge")) whyEl.textContent += hint;
  }
}

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

  // Row one is the same on every domain -- label, both buttons, help -- so
  // switching tabs does not reshape it. Whatever differs goes in the sentence
  // underneath.
  const row = document.createElement("div");
  row.className = "controller-row";
  const why = document.createElement("p");
  why.className = "controller-why";

  const label = document.createElement("span");
  label.className = "controller-label";
  label.textContent = "Controller";
  row.append(label);

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
    //
    // The correction is for *this domain only*. What is stored is the
    // reader's standing preference across the whole shell, and a domain that
    // cannot honour it has no business rewriting it. Persisting the fallback
    // is what made netsim unreachable on Generic even though Generic supports
    // it: clicking any in-page-only tab -- Car and Ranging still are, and HID
    // was until it was wired up alongside this fix -- rewrote the stored
    // choice to "in-page". They sit in the same tab strip as Generic, one
    // click away, so coming back found in-page waiting with nothing on screen
    // saying the choice had been discarded. Measured before the fix: pick
    // netsim on Generic, touch Car, return, and the bar reads "In browser".
    //
    // So the effective choice is re-derived from the preference on every
    // render rather than accumulated in this closure; only a click (below) is
    // a choice, and only a choice is written down.
    const preferred = currentController();
    selected = map[preferred] === true
      ? preferred
      : (CONTROLLERS.find((c) => map[c.id] === true)?.id ?? preferred);
    for (const input of inputs) input.wrap.remove();
    inputs.length = 0;

    const blocked = CONTROLLERS.filter((c) => map[c.id] !== true);
    why.textContent = blocked.length
      ? blocked
          .map((c) => `${c.label} is not available here — ${map[c.id] ?? NOT_WIRED}.`)
          .join(" ")
      // "the same radio" was wrong: each device has its own radio, and what
      // joining netsim shares is the medium they all transmit into. "Network"
      // would be worse -- nothing here is an IP network. netsim's own word for
      // it, and the one the rest of this site already uses, is a scene.
      : "In browser needs nothing installed; netsim needs netsimd running, "
        + "and puts the devices in the same scene as the Android emulator; "
        + "Real radio is a dongle — or a phone running SimBLE Android — "
        + "through the simble --usb bridge.";
    for (const c of CONTROLLERS) {
      const reason = map[c.id];
      const usable = reason === true;

      const wrap = document.createElement("label");
      wrap.className = "controller-choice" + (usable ? "" : " unusable");

      const radio = document.createElement("input");
      radio.type = "radio";
      radio.name = "simble-controller";
      radio.value = c.id;
      radio.disabled = !usable;
      radio.checked = c.id === selected;

      // The radio and its name are one unit that never breaks; only the
      // explanation is allowed to wrap, so the buttons always sit on the
      // first line however long a reason gets.
      const pick = document.createElement("span");
      pick.className = "controller-pick";
      const name = document.createElement("b");
      name.textContent = c.label;
      if (c.id === "usb") {
        decorateUsbCount(name, why);
        // The bridge URL sits beside the option it configures. Editing it
        // re-probes the dongle count, so the badge always describes the
        // bridge the field names.
        const bridge = document.createElement("input");
        bridge.className = "controller-bridge";
        bridge.value = usbBridgeUrl();
        bridge.spellcheck = false;
        bridge.setAttribute("aria-label", "the simble --usb bridge URL");
        bridge.style.cssText =
          "margin-left:0.5rem;font-family:monospace;font-size:0.8em;width:13.5rem;" +
          "padding:0.1rem 0.3rem";
        bridge.addEventListener("change", () => {
          try {
            localStorage.setItem(BRIDGE_KEY, bridge.value.trim());
          } catch (e) {
            /* applies this session only */
          }
          decorateUsbCount(name, why);
        });
        // Appended after the radio and label land in `pick` (below), so the
        // field reads as belonging to the option, not to its neighbour.
        queueMicrotask(() => pick.append(bridge));
      }

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

      pick.append(radio, name);
      wrap.append(pick);
      row.insertBefore(wrap, help);
      inputs.push({ wrap });
    }
  }

  // The help link is appended first: render() inserts each choice before it,
  // so it has to already be a child.
  row.append(help);
  el.append(row, why);
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
