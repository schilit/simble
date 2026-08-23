// A device that ships its own acceptance test.
//
// Every other entry in this catalog builds a device out of services and
// characteristics. This one does not build anything: it LOADS one by name and
// then makes a claim about it. catalog::device("thermostat") is the same
// resolution a scene file's "device": "thermostat" performs and the same one
// the MCP `example` tool serves — now available in the language the entries
// are written in, so a test never has to re-type the device it is testing.
let room = catalog::device("thermostat");

// wait_for reads the event stream the observer recorded while the loaded
// entry was coming up. A loaded device runs in this script's own engine, so
// its events are in this script's queue: building a service raised one, and
// this is where the script learns the stack accepted it instead of assuming.
wait_for "service_added" {
    assert(event.status == 0, "the thermostat's first service was accepted");
}

const TEMPERATURE = uuid::from_u16(0x2A6E);

// The claim that needs a window rather than a sample. The room converges on
// its setpoint one degree per tick, and the property worth having is that it
// never overshoots — which is invisible to a single read, because a single
// read lands either before or after the moment that would have shown it.
// assert_over runs the loaded device's OWN fn tick for three seconds,
// sampling every 100 ms, and requires the claim on all thirty samples.
//
//   assert_over(device, uuid, op, value, seconds, byte)
//
// `seconds` defaults to 2 and `byte` to 1 — index 0 is the flags octet that
// opens nearly every SIG measurement value.
assert_over(room, TEMPERATURE, "<=", 21, 3.0);   // never past the setpoint
assert_over(room, TEMPERATURE, ">=", 18, 3.0);   // and never below where it began

// Loading a device by name keeps its own `fn tick` alongside it, so forward
// the scene clock into it and the hosted device goes on converging for real.
fn tick(server, t) {
    server.advance(t);
}
