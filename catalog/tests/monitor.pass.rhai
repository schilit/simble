// Three primitives that were each one step out of reach, in one test.
//
//   assert(...)      — this happened
//   wait_for "..."   — this happened, and here is the event that says so
//   assert_over(...) — this STAYED true, on every sample of a window
//
// The device under test is not typed out here: catalog::device loads a
// shipped entry by name — the same names a scene file's "device": "hrm"
// resolves against, and the same names the MCP `example` tool serves.

let hrm = catalog::device("hrm");

// A loaded device runs in this script's own engine, so the events it raised
// on the way up are in this script's queue. wait_for drains until it finds
// the one it was asked for and binds it as `event`.
wait_for "service_added" {
    assert(event.uuid == uuid::HEART_RATE_SERVICE, "Heart Rate is the first service the HRM publishes");
    assert(event.status == 0, "the stack accepted the service");
}

// "This happened": one sample, right now.
let first = hrm.value(uuid::HEART_RATE_MEASUREMENT);
assert(first.len() == 2, "a measurement is [flags, bpm]");

// "This stayed true": three seconds of the device's OWN fn tick, sampled
// every 100 ms, and the condition has to hold on every one of the 30 samples.
// A single read could not have caught a spike between them — that is the
// whole difference between assert and assert_over.
//
//   assert_over(device, uuid, op, value, seconds, byte)
//
// `seconds` defaults to 2 and `byte` to 1, past the flags octet that opens
// nearly every SIG measurement value.
assert_over(hrm, uuid::HEART_RATE_MEASUREMENT, "<", 100, 3.0);
assert_over(hrm, uuid::HEART_RATE_MEASUREMENT, ">", 60, 3.0);

// A passing monitor's value is the sample that came closest to breaking it,
// so a test can say how much headroom it actually had.
let peak = assert_over(hrm, uuid::HEART_RATE_MEASUREMENT, "<", 100, 3.0);
assert(peak < 90, `the busiest sample was ${peak} bpm, well inside the band`);
