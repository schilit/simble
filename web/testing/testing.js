// SimBLE Testing page: run an assert-based Rhai script and show PASS/FAIL.
// The script IS the device and the test; run_test() evaluates it deterministically
// in a fresh engine (no netsim, no connection) and reports the result.

import init, { run_test } from "../pkg/simble.js";
import { attachHighlightedEditor } from "../common/highlight.js";

const $ = (id) => document.getElementById(id);

const EXAMPLES = {
  structure: `// A device is a script; add assert() and it's a test.
let server = android::BluetoothGattServer("HRM");
let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
let hr = android::BluetoothGattCharacteristic(
    uuid::HEART_RATE_MEASUREMENT,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY,
    android::PERMISSION_READ,
);
hr.set_value([0x00, 72]);
hrs.add_characteristic(hr);
server.add_service(hrs);

// These run now, at build time, against the real GATT stack.
assert(server.name == "HRM", "server keeps its name");
let svc = server.get_service(uuid::HEART_RATE_SERVICE);
assert(svc.characteristics.len() == 1, "one characteristic in the service");
let chr = svc.get_characteristic(uuid::HEART_RATE_MEASUREMENT);
assert((chr.properties & android::PROPERTY_NOTIFY) != 0, "measurement is notify-capable");
assert((chr.properties & android::PROPERTY_READ) != 0, "measurement is readable");
`,
  values: `// A two-service device; assert both are present and correctly shaped.
let server = android::BluetoothGattServer("Wearable");

let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
hrs.add_characteristic(android::BluetoothGattCharacteristic(
    uuid::HEART_RATE_MEASUREMENT, android::PROPERTY_NOTIFY, android::PERMISSION_READ));
server.add_service(hrs);

let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
bas.add_characteristic(android::BluetoothGattCharacteristic(
    uuid::BATTERY_LEVEL, android::PROPERTY_READ, android::PERMISSION_READ));
server.add_service(bas);

assert(server.get_service(uuid::HEART_RATE_SERVICE).characteristics.len() == 1, "HRS has one char");
assert(server.get_service(uuid::BATTERY_SERVICE).characteristics.len() == 1, "BAS has one char");
let batt = server.get_service(uuid::BATTERY_SERVICE).get_characteristic(uuid::BATTERY_LEVEL);
assert((batt.properties & android::PROPERTY_NOTIFY) == 0, "battery level is read-only, not notify");
`,
  failing: `// This test FAILS on purpose — the assertion doesn't hold, and its message
// tells you exactly what went wrong (this is what a caught regression looks like).
let server = android::BluetoothGattServer("HRM");
let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
hrs.add_characteristic(android::BluetoothGattCharacteristic(
    uuid::HEART_RATE_MEASUREMENT, android::PROPERTY_READ, android::PERMISSION_READ));
server.add_service(hrs);

let chr = server.get_service(uuid::HEART_RATE_SERVICE).get_characteristic(uuid::HEART_RATE_MEASUREMENT);
// The characteristic is READ-only, so this assertion is false:
assert((chr.properties & android::PROPERTY_NOTIFY) != 0, "expected the measurement to be notify-capable");
`,
};

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

await init();
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
