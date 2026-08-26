// SimBLE Testing page: two categories of test, one page.
//
// **Asserts** — the original runner. A SimBLE device is a Rhai script; add
// `assert(...)` and the same script is a test. `run_test()` evaluates it
// deterministically in a fresh engine — no netsim, no connection, no clock —
// and reports PASS or FAIL.
//
// **Data** — a 256 KB bulk transfer, measured on whichever controller you
// pick, with the setup phases timed separately from the payload. Lives in
// ./data.js; it is a much larger thing than the assert runner and shares
// nothing with it but the tab strip.
//
// The two are categories rather than two pages because the question they
// answer is one question — "is this device right, and how does it behave?" —
// and because a person comparing controllers wants the assert runner one
// click away, not one navigation away.

import init, { run_test } from "../pkg/simble.js";
import { attachHighlightedEditor } from "../common/highlight.js";
import { mountData } from "./data.js";

const $ = (id) => document.getElementById(id);

// The examples are real .rhai files in the repository-root catalog/ — the
// same files CI runs, shared with the other surfaces rather than living
// under this page. The dev server maps /catalog/; the Pages workflow
// stages a copy.
// through the `simble` CLI, so the page and CI can never drift. The map
// is select-option value -> file (the *.pass/*.fail suffix tells CI what to
// expect; see .github/workflows/ci.yml).
const EXAMPLE_FILES = {
  structure: "/catalog/tests/structure.pass.rhai",
  values: "/catalog/tests/two-service.pass.rhai",
  failing: "/catalog/tests/notify-required.fail.rhai",
};
const EXAMPLES = {};

async function loadExamples() {
  await Promise.all(
    Object.entries(EXAMPLE_FILES).map(async ([key, file]) => {
      const res = await fetch(file);
      EXAMPLES[key] = res.ok ? await res.text() : `// could not load ${file}\n`;
    }),
  );
}

function showResult(ok, message, tookMs) {
  const r = $("result");
  r.className = "result show " + (ok ? "pass" : "fail");
  $("result-icon").textContent = ok ? "✓" : "✗";
  $("result-title").textContent = ok ? "PASSED — all assertions held" : "FAILED";
  $("result-took").textContent = `${tookMs.toFixed(1)} ms`;
  const msg = $("result-msg");
  msg.textContent = ok ? "" : message;
  msg.style.display = ok ? "none" : "block";
}

let editor;

function runTest() {
  const t0 = performance.now();
  let res;
  try { res = JSON.parse(run_test(editor.value)); }
  catch (e) { res = { ok: false, error: String(e) }; }
  showResult(res.ok, res.error, performance.now() - t0);
}

// --- the category strip -----------------------------------------------------
//
// Both panels exist in the DOM from the start and one is hidden. The Data
// category holds a running measurement and a chart laid out against its
// container's pixel width; rebuilding it on every tab switch would throw
// away the first and mis-size the second.

let dataMounted = false;

function selectCategory(name) {
  for (const tab of document.querySelectorAll(".cat-tab")) {
    const active = tab.dataset.cat === name;
    tab.classList.toggle("active", active);
    tab.setAttribute("aria-selected", String(active));
  }
  $("cat-asserts").hidden = name !== "asserts";
  $("cat-data").hidden = name !== "data";
  if (name === "data" && !dataMounted) {
    dataMounted = true;
    mountData($("cat-data"));
  }
  try {
    localStorage.setItem("simble-testing-category", name);
  } catch (e) {
    /* the choice applies to this page, it just is not remembered */
  }
}

await Promise.all([init(), loadExamples()]);
editor = $("script");
editor.value = EXAMPLES.structure;
attachHighlightedEditor(editor);

$("run").addEventListener("click", runTest);
$("examples").addEventListener("change", (e) => {
  const ex = EXAMPLES[e.target.value];
  if (ex) {
    editor.value = ex;
    e.target.value = "";
    $("result").className = "result";
  }
});

for (const tab of document.querySelectorAll(".cat-tab")) {
  tab.addEventListener("click", () => selectCategory(tab.dataset.cat));
}

let remembered = "asserts";
try {
  remembered = localStorage.getItem("simble-testing-category") || "asserts";
} catch (e) {
  /* private window */
}
selectCategory(remembered === "data" ? "data" : "asserts");
