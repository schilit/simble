// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// A small, shared "controller backend" selector for the demo pages: choose
// whether a page runs against the in-page controller (the wasm Link — no
// external process) or a WebSocket backend (netsim / rootcanal-ws). The choice
// is remembered in localStorage and applied across pages. Self-contained: it
// injects its own styles once, using the shared theme tokens with fallbacks.

const STYLE_ID = "simble-backend-style";

function injectStyles() {
  if (document.getElementById(STYLE_ID)) return;
  const style = document.createElement("style");
  style.id = STYLE_ID;
  style.textContent = `
    .backend {
      display: flex; align-items: center; gap: 0.5rem 0.9rem; flex-wrap: wrap;
      padding: 0.5rem 0.9rem; margin: 0 0 0.9rem;
      border: 1px solid var(--border, #30363d); border-radius: 8px;
      background: var(--panel, #161b22);
      font-size: 0.85rem; color: var(--text, #e6edf3);
    }
    .backend .backend-label { color: var(--dim, #8b949e); font-weight: 600; }
    .backend label { display: inline-flex; align-items: center; gap: 0.35rem; cursor: pointer; }
    .backend label .hint { color: var(--dim, #8b949e); font-size: 0.8em; }
    .backend a.backend-help { margin-left: auto; color: var(--accent, #58a6ff); text-decoration: none; font-size: 0.82rem; }
    .backend a.backend-help:hover { text-decoration: underline; }
  `;
  document.head.appendChild(style);
}

/**
 * Render a backend selector into `container`.
 * @param {HTMLElement} container where to render.
 * @param {object} opts
 * @param {string} [opts.key] localStorage key (default "simble-backend").
 * @param {string} [opts.helpHref] href for the "what's this?" link (default "../controllers/").
 * @param {(mode: string) => void} opts.onChange called with "in-page" | "websocket" on change.
 * @returns {string} the current mode ("in-page" | "websocket").
 */
export function createBackendSelector(container, { key = "simble-backend", helpHref = "../controllers/", onChange } = {}) {
  injectStyles();
  const current = localStorage.getItem(key) === "websocket" ? "websocket" : "in-page";
  container.innerHTML = `
    <div class="backend">
      <span class="backend-label">Controller</span>
      <label><input type="radio" name="${key}" value="in-page"> In-page <span class="hint">— no netsim</span></label>
      <label><input type="radio" name="${key}" value="websocket"> WebSocket <span class="hint">— netsim / rootcanal</span></label>
      <a class="backend-help" href="${helpHref}">which controller? ↗</a>
    </div>`;
  for (const radio of container.querySelectorAll(`input[name="${key}"]`)) {
    radio.checked = radio.value === current;
    radio.addEventListener("change", () => {
      if (!radio.checked) return;
      localStorage.setItem(key, radio.value);
      onChange?.(radio.value);
    });
  }
  return current;
}
