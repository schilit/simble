// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// The title bar every simulated device wears on the Devices pages.
//
// A device panel used to open with whatever that page's author felt like: a
// bullet that reported nothing, a page-wide status pill that could only ever
// describe one of the two devices below it, a Run button on one page and none
// on the next. The reader could not tell, from looking, whether a device was on
// the air — which is the first question anyone has about a Bluetooth device.
//
// So the header is fixed:
//
//   [dot] Name · role · what it is doing now  [✎] [▶/■]        address
//   (why this device cannot be stopped on its own)
//
// Two rows, not one wrapping row. Everything except the address and the "why"
// lives in `.dev-main`, and the header is a grid, because a single flex-wrap
// row put the address in a different place on almost every card: it is last in
// source order with `margin-left: auto`, so as soon as the row overflowed --
// which the italic "why" note reliably caused -- the address wrapped down with
// it. A device whose state line was long pushed it to a third row. The address
// is the device's identity and the first thing you look for when comparing two
// cards, so it is pinned to the top row's right edge and the note gets its own
// row underneath.
//
// Every part of it is answerable from the stack:
//
//   * the dot is filled when the device is *actually* doing its job — a
//     peripheral on the air, a central connected. It is not decoration and it
//     is never permanently green: the caller passes what a lit dot means, and
//     that sentence becomes the dot's tooltip so the claim is inspectable.
//   * the pen shows or hides the device's script, and is absent when there is
//     no script — a Rust central has none, and a fake one would be an API
//     nobody can run.
//   * the run/stop toggle is CAPABILITY-AWARE. `WebLink` has add_peripheral and
//     add_central and no way at all to remove one, so several devices on this
//     site genuinely cannot be stopped on their own: freeing the link takes
//     every device with it. Such a device gets the control DISABLED with the
//     reason spelled out beside it, rather than a button that quietly does
//     nothing or takes its neighbour down with it.
//   * the address is the on-air identity, right-aligned. A device that has no
//     Bluetooth address (the car endpoints are two RFCOMM multiplexers wired
//     directly together — no ACL, so no BD_ADDR) says so instead of showing a
//     plausible-looking one.
//
// Everything is returned as element references. Nothing here is reachable by
// `document.getElementById`, on purpose: the domain shell mounts one domain at
// a time into a shared stage, and generic ids like `script` and `conn` have
// already crossed between modules once — one domain's script was written into
// another's textarea.
//
// The styles live in common/simble.css (`.dev-head`), which every page that can
// host a device already links.

/**
 * Builds one device header.
 *
 * @param {object} options
 * @param {string}  options.name      the device's name, as it calls itself.
 * @param {string} [options.kind]     its role, e.g. "peripheral · Rhai script".
 * @param {string} [options.accent]   "good" | "accent" | "warn" — the colour
 *                                    of the name and of the lit dot. Servers
 *                                    are green and clients blue across the
 *                                    site; keep to that.
 * @param {?string} [options.address] the on-air address, or null for a device
 *                                    that genuinely has none.
 * @param {string} [options.addressNote] why there is no address; shown when
 *                                    `address` is null.
 * @param {string}  options.dotMeans  what a filled dot asserts about this
 *                                    device ("advertising on the air"). Shown
 *                                    as the dot's tooltip.
 * @param {?object} [options.script]  { text, editable, note, open, onApply,
 *                                    highlight } — omit entirely when the
 *                                    device has no script, and no pen is
 *                                    rendered. `highlight` is called with the
 *                                    textarea the first time the panel is
 *                                    shown (pass common/highlight.js's
 *                                    attachHighlightedEditor).
 * @param {?object} [options.run]     { running, onRun, onStop } for a device
 *                                    that can start and stop by itself, or
 *                                    { running, disabled: true, reason } for
 *                                    one whose lifetime is shared. Omit for no
 *                                    control at all.
 * @returns {object} the header handle; see the properties assigned to `api`.
 */
export function createDeviceHeader(options) {
  const {
    name,
    kind = "",
    accent = "",
    address = null,
    addressNote = "",
    dotMeans = "",
    script = null,
    run = null,
  } = options;

  const el = document.createElement("header");
  el.className = "dev-head" + (accent ? ` accent-${accent}` : "");

  const dot = document.createElement("i");
  dot.className = "dev-dot";
  // The tooltip is the claim the dot is making. Without it a coloured circle
  // is just a coloured circle, which is what this component exists to end.
  dot.title = dotMeans ? `filled when: ${dotMeans}` : "no state reported";

  const nameEl = document.createElement("span");
  nameEl.className = "dev-name";
  nameEl.textContent = name;

  const kindEl = document.createElement("span");
  kindEl.className = "dev-kind";
  kindEl.textContent = kind;

  // The live half of the label — what the device is doing right now. This is
  // what the page-wide status pill used to try to say for every device at once.
  const stateEl = document.createElement("span");
  stateEl.className = "dev-state";

  // Everything that belongs on the top row goes in here; the address and the
  // "why" are placed by the grid, so neither can be pushed around by how long
  // a device's name or state line happens to be.
  const main = document.createElement("div");
  main.className = "dev-main";
  main.append(dot, nameEl, kindEl, stateEl);
  el.append(main);

  // --- the script, and the pen that reveals it -----------------------------
  let panel = null;
  let textarea = null;
  let pen = null;
  let penTitle = null;
  let applyState = null;

  if (script) {
    panel = document.createElement("div");
    panel.className = "dev-script";
    panel.hidden = !script.open;

    textarea = document.createElement("textarea");
    textarea.className = "code";
    textarea.spellcheck = false;
    textarea.value = script.text || "";
    textarea.readOnly = !script.editable;
    panel.append(textarea);

    if (script.note) {
      const note = document.createElement("p");
      note.className = "hint";
      // The note explains what kind of text this is — a device definition, a
      // transcript of real binding calls, a read-only copy — so a reader is
      // never guessing whether they are looking at something runnable. It is
      // page-authored markup (links, <code>), never anything a device reported.
      note.innerHTML = script.note;
      panel.append(note);
    }

    if (script.editable && script.onApply) {
      const row = document.createElement("div");
      row.className = "row";
      const apply = document.createElement("button");
      apply.textContent = "apply";
      apply.title = "Rebuild the device from this script";
      apply.addEventListener("click", () => script.onApply(textarea.value));
      const state = document.createElement("span");
      state.className = "hint";
      row.append(apply, state);
      panel.append(row);
      applyState = state;
    }

    pen = document.createElement("button");
    pen.className = "dev-icon";
    pen.textContent = "✎";
    // The tooltip says which way the click goes, and follows the state. The
    // kind line used to spell out "Rhai script" beside the name, which the
    // pen already tells you -- so the words moved here, where they answer a
    // question rather than repeating one.
    penTitle = (shown) =>
      `${shown ? "Hide" : "Show"} Rhai script${script.editable ? "" : " (read-only)"}`;
    pen.title = penTitle(Boolean(script.open));
    pen.setAttribute("aria-pressed", String(Boolean(script.open)));
    pen.addEventListener("click", () => showScript(panel.hidden));
    main.append(pen);
  }

  // --- run / stop ----------------------------------------------------------
  let runBtn = null;
  let whyEl = null;
  let running = run ? Boolean(run.running) : false;

  if (run) {
    runBtn = document.createElement("button");
    runBtn.className = "dev-icon";
    runBtn.addEventListener("click", () => {
      if (runBtn.disabled) return;
      if (running) run.onStop?.();
      else run.onRun?.();
    });
    whyEl = document.createElement("span");
    whyEl.className = "dev-why";
    // The toggle sits on the top row; its explanation gets its own row below,
    // where a long reason cannot shove the address anywhere.
    main.append(runBtn);
    el.append(whyEl);
    setStopCapability({ disabled: Boolean(run.disabled), reason: run.reason || "" });
    setRunning(running);
  }

  // --- the address ---------------------------------------------------------
  const addrEl = document.createElement("span");
  addrEl.className = "dev-addr";
  el.append(addrEl);
  setAddress(address, addressNote);

  // --- behaviour -----------------------------------------------------------

  /**
   * Renames the device. A scripted peripheral's real name is the one its GATT
   * server advertises, which the page only learns on the first tick — so the
   * header takes it from `status.name` rather than repeating a constant that
   * can drift away from the script.
   */
  function setName(next) {
    if (next && next !== nameEl.textContent) nameEl.textContent = next;
  }

  /** The dot and the live label. `on` is the device's real state, not a mood. */
  function setState(on, label = "", tone = "") {
    dot.classList.toggle("on", Boolean(on));
    stateEl.textContent = label;
    stateEl.className = "dev-state" + (tone ? ` ${tone}` : "");
  }

  /** Flips the toggle's glyph. The caller owns whether the device is running;
   *  this only reports it. */
  function setRunning(on) {
    running = Boolean(on);
    if (!runBtn) return;
    runBtn.textContent = running ? "■" : "▶";
    if (!runBtn.disabled) {
      runBtn.title = running ? `Stop ${name}` : `Run ${name}`;
    }
  }

  /**
   * Says whether this device can be stopped on its own right now — which can
   * change under a running page: the same sink is independently stoppable over
   * netsim (its own socket) and not stoppable in the in-page controller (one
   * WebLink, no remove).
   */
  function setStopCapability({ disabled, reason = "" }) {
    if (!runBtn) return;
    runBtn.disabled = Boolean(disabled);
    whyEl.textContent = disabled ? reason : "";
    whyEl.hidden = !disabled || !reason;
    if (disabled) {
      runBtn.title = reason
        ? `Cannot stop this device on its own — ${reason}`
        : "Cannot stop this device on its own";
    } else {
      setRunning(running);
    }
  }

  function setAddress(next, note = "") {
    if (next) {
      addrEl.textContent = next;
      addrEl.classList.remove("none");
      addrEl.title = "the address this device is on the air as";
    } else {
      addrEl.textContent = "no address";
      addrEl.classList.add("none");
      addrEl.title = note || "this device has no Bluetooth address";
    }
  }

  function showScript(show) {
    if (!panel) return;
    panel.hidden = !show;
    pen.setAttribute("aria-pressed", String(show));
    if (penTitle) pen.title = penTitle(show);
    // Syntax highlighting is attached the first time the panel is actually
    // revealed: the overlay copies the textarea's computed box metrics, and a
    // hidden element has none to copy. The caller supplies the attacher so the
    // shared header carries no dependency on the highlighter.
    if (show && script?.highlight && !panel._highlighted) {
      panel._highlighted = true;
      try { script.highlight(textarea); } catch (_) { /* plain textarea is fine */ }
    }
  }

  /** Replaces the script text, for a panel whose content is generated. */
  function setScript(text) {
    if (textarea) textarea.value = text;
  }

  /** Appends one line — used by panels that are a live transcript of the real
   *  binding calls a page issued, rather than a script. */
  function appendScript(line) {
    if (!textarea) return;
    textarea.value += line + "\n";
    textarea.scrollTop = textarea.scrollHeight;
  }

  /** Momentary feedback next to the script's apply button. */
  function setApplyState(text) {
    if (applyState) applyState.textContent = text || "";
  }

  function destroy() {
    el.remove();
    panel?.remove();
  }

  return {
    el,
    panel,
    textarea,
    setName,
    setState,
    setRunning,
    setStopCapability,
    setAddress,
    showScript,
    setScript,
    appendScript,
    setApplyState,
    destroy,
    get running() {
      return running;
    },
  };
}
