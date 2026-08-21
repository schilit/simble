// SimBLE Testing page: run an assert-based Rhai script and show PASS/FAIL.
// The script IS the device and the test; run_test() evaluates it deterministically
// in a fresh engine (no netsim, no connection) and reports the result.

import init, { run_test } from "../pkg/simble.js";
import { attachHighlightedEditor } from "../common/highlight.js";

const $ = (id) => document.getElementById(id);

// The examples are real .rhai files under examples/ — the same files CI runs
// through the `simble` CLI, so the page and CI can never drift. The map
// is select-option value -> file (the *.pass/*.fail suffix tells CI what to
// expect; see .github/workflows/ci.yml).
const EXAMPLE_FILES = {
  structure: "examples/structure.pass.rhai",
  values: "examples/two-service.pass.rhai",
  failing: "examples/notify-required.fail.rhai",
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
