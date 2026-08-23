// This test FAILS on purpose — it is what a caught regression looks like
// when the thing that regressed is temporal.
//
// The trap assert_over exists to close: the first samples pass. A test built
// out of assert alone reads the measurement once, sees a resting rate, and
// reports success while the device climbs past the limit a second later.
let hrm = catalog::device("hrm");

// True, and not enough: one sample says nothing about the next thirty.
let now = hrm.value(uuid::HEART_RATE_MEASUREMENT);
assert(now[1] <= 72, "the reading starts at a resting rate");

// Watched over three seconds, the same kind of claim breaks. The message
// names the device, the characteristic, the sample that violated it, and
// when — which is the part a stack trace could never tell you:
//
//   assert_over failed: hrm 2A37 byte 1 = 72 violated < 72 at t=2.0s (sample 21 of 30)
assert_over(hrm, uuid::HEART_RATE_MEASUREMENT, "<", 72, 3.0);
