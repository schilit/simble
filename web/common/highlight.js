// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// A tiny, dependency-free Rhai syntax highlighter (no CDN, no Prism/CodeMirror/
// Monaco — consistent with the project's near-zero-dependency ethos). One
// single-pass tokenizer turns Rhai source into HTML <span>s; a helper overlays
// a highlighted layer behind an editable <textarea> using the standard
// transparent-textarea technique, degrading gracefully to a plain textarea if
// anything about the overlay misbehaves (editing always works — the real
// textarea is always on top and functional).

const KEYWORDS = new Set([
  "let", "const", "fn", "if", "else", "for", "while", "loop", "do", "until",
  "in", "return", "break", "continue", "switch", "throw", "try", "catch",
  "true", "false", "import", "export", "as", "private", "global", "this",
]);

const isIdentStart = (c) => c === "_" || (c >= "a" && c <= "z") || (c >= "A" && c <= "Z");
const isIdent = (c) => isIdentStart(c) || (c >= "0" && c <= "9");
const isDigit = (c) => c >= "0" && c <= "9";
const isHex = (c) => isDigit(c) || (c >= "a" && c <= "f") || (c >= "A" && c <= "F");
const isUpperConst = (w) => /^[A-Z][A-Z0-9_]+$/.test(w);

const esc = (s) =>
  s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

// Highlights a Rhai snippet, returning an HTML string of escaped, span-wrapped
// tokens. Covers: line + block comments, strings, numbers (incl. 0x hex and
// floats), keywords, the android::/uuid::/audio:: (any `name::`) path prefixes,
// function-call identifiers, and ALL_CAPS constants.
export function highlightRhai(code) {
  const s = String(code);
  const n = s.length;
  let out = "";
  let i = 0;
  while (i < n) {
    const c = s[i];

    // line comment
    if (c === "/" && s[i + 1] === "/") {
      let j = i;
      while (j < n && s[j] !== "\n") j++;
      out += `<span class="tok-comment">${esc(s.slice(i, j))}</span>`;
      i = j;
      continue;
    }
    // block comment
    if (c === "/" && s[i + 1] === "*") {
      let j = i + 2;
      while (j < n && !(s[j] === "*" && s[j + 1] === "/")) j++;
      j = Math.min(n, j + 2);
      out += `<span class="tok-comment">${esc(s.slice(i, j))}</span>`;
      i = j;
      continue;
    }
    // string
    if (c === '"') {
      let j = i + 1;
      while (j < n && s[j] !== '"') {
        if (s[j] === "\\") j++;
        j++;
      }
      j = Math.min(n, j + 1);
      out += `<span class="tok-string">${esc(s.slice(i, j))}</span>`;
      i = j;
      continue;
    }
    // number (hex or decimal/float)
    if (isDigit(c) || (c === "." && isDigit(s[i + 1] || ""))) {
      let j = i;
      if (c === "0" && (s[i + 1] === "x" || s[i + 1] === "X")) {
        j = i + 2;
        while (j < n && (isHex(s[j]) || s[j] === "_")) j++;
      } else {
        while (j < n && (isDigit(s[j]) || s[j] === "_")) j++;
        if (s[j] === "." && isDigit(s[j + 1] || "")) {
          j++;
          while (j < n && (isDigit(s[j]) || s[j] === "_")) j++;
        }
        if (s[j] === "e" || s[j] === "E") {
          let k = j + 1;
          if (s[k] === "+" || s[k] === "-") k++;
          if (isDigit(s[k] || "")) {
            j = k;
            while (j < n && (isDigit(s[j]) || s[j] === "_")) j++;
          }
        }
      }
      out += `<span class="tok-number">${esc(s.slice(i, j))}</span>`;
      i = j;
      continue;
    }
    // identifier / keyword / path / call / const
    if (isIdentStart(c)) {
      let j = i + 1;
      while (j < n && isIdent(s[j])) j++;
      const word = s.slice(i, j);
      let cls = null;
      if (KEYWORDS.has(word)) {
        cls = "tok-keyword";
      } else if (s[j] === ":" && s[j + 1] === ":") {
        cls = "tok-path"; // a namespace like android:: / uuid:: / audio::
      } else {
        let k = j;
        while (k < n && (s[k] === " " || s[k] === "\t")) k++;
        if (s[k] === "(") cls = "tok-fn";
        else if (isUpperConst(word)) cls = "tok-const";
      }
      out += cls ? `<span class="${cls}">${esc(word)}</span>` : esc(word);
      i = j;
      continue;
    }

    out += esc(c);
    i++;
  }
  return out;
}

// Overlays a scroll-synced, syntax-highlighted layer behind an editable
// <textarea>. The textarea stays on top with transparent text and a visible
// caret, so editing is unaffected. The textarea's own computed box metrics
// (font, padding, borders, wrapping) are copied to the layer so the two align
// regardless of page-specific styling. Programmatic `textarea.value = …` is
// intercepted to refresh the layer. Returns a manual refresh() function, or
// null (leaving a plain textarea) if the overlay can't be built.
export function attachHighlightedEditor(textarea) {
  try {
    if (!textarea || textarea._hlAttached) return null;
    const parent = textarea.parentNode;
    if (!parent) return null;

    const wrap = document.createElement("div");
    wrap.className = "hl-editor";
    const pre = document.createElement("pre");
    pre.className = "hl-layer";
    pre.setAttribute("aria-hidden", "true");
    const codeEl = document.createElement("code");
    pre.appendChild(codeEl);

    // Insert the wrap in place of the textarea, then move both into it.
    parent.insertBefore(wrap, textarea);
    wrap.appendChild(pre);
    wrap.appendChild(textarea);
    textarea.classList.add("hl-input");

    const cs = getComputedStyle(textarea);
    const copy = [
      "fontFamily", "fontSize", "fontWeight", "fontStyle", "lineHeight",
      "letterSpacing", "tabSize", "textIndent",
      "paddingTop", "paddingRight", "paddingBottom", "paddingLeft",
      "borderTopWidth", "borderRightWidth", "borderBottomWidth", "borderLeftWidth",
      "borderTopStyle", "borderRightStyle", "borderBottomStyle", "borderLeftStyle",
      "boxSizing", "whiteSpace", "wordBreak", "overflowWrap", "wordWrap",
    ];
    for (const p of copy) pre.style[p] = cs[p];
    pre.style.borderColor = "transparent"; // occupy the same space, draw nothing
    // Carry the textarea's outer margins onto the wrap, then neutralize them so
    // the absolutely-positioned layer (inset:0) aligns with the text content.
    wrap.style.marginTop = cs.marginTop;
    wrap.style.marginRight = cs.marginRight;
    wrap.style.marginBottom = cs.marginBottom;
    wrap.style.marginLeft = cs.marginLeft;
    textarea.style.margin = "0";
    textarea.style.background = "transparent";
    textarea.style.caretColor = cs.color;
    textarea.style.color = "transparent";
    pre.style.color = cs.color;

    const refresh = () => {
      codeEl.innerHTML = highlightRhai(textarea.value) + "\n";
    };
    const sync = () => {
      pre.scrollTop = textarea.scrollTop;
      pre.scrollLeft = textarea.scrollLeft;
    };
    textarea.addEventListener("input", () => { refresh(); sync(); });
    textarea.addEventListener("scroll", sync);

    // Intercept programmatic value assignment (Reset / Examples / shared load).
    const desc = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value");
    if (desc && desc.set && desc.get) {
      Object.defineProperty(textarea, "value", {
        configurable: true,
        get() { return desc.get.call(this); },
        set(v) { desc.set.call(this, v); refresh(); sync(); },
      });
    }

    textarea._hlAttached = true;
    refresh();
    sync();
    return refresh;
  } catch (e) {
    console.error("highlight overlay unavailable — using a plain textarea:", e);
    return null;
  }
}
