# The SimBLE scene format

A **scene** is a JSON file describing which Bluetooth devices exist, how each
is placed, and how they are wired to each other. One file, committable and
diffable, that `simble scene.json` turns into a running scene.

```bash
simble examples/scenes/heart-rate-monitor.json
simble --no-run examples/scenes/le-audio-unicast.json      # validate only
simble --controller netsim --seconds 30 my-scene.json      # override placement
```

Exit code 0 if every device came up and none reported an error, 1 otherwise —
the same contract as `simble device.rhai`.

---

## 1. The one rule: topology in JSON, behaviour in Rhai

A scene declares **what exists and how it is wired**. It never says *what
happens*.

There is no `"actions"`, no `"at": "2s"`, no `"then assert"`. Behaviour lives
in the device script, where it already is, and the same script serves as
device *and* test by adding `assert(...)` — a failing assertion fails the
script, the device never instantiates, and the scene run exits 1. Putting
actions in the scene file would invent a second, worse scripting language in
JSON, and the two would immediately start disagreeing about what a device can
do.

If you want a scene to *check* something, put the check in a device's script.

The corollary: **run duration is not in the file either.** How long you care
to watch a topology is a property of the run, not of the scene, so it is
`--seconds` / `--tick-ms` on the command line.

## 2. A complete example

```json
{
  "version": 1,
  "name": "LE Audio unicast",
  "description": "A source streaming LC3 to a sink over a real CIS",
  "controller": "netsim",
  "devices": [
    {
      "id": "sink",
      "role": "peripheral",
      "name": "scene-speaker",
      "address": "CC:1E:57:0F:00:06",
      "device": "volume"
    },
    {
      "id": "source",
      "role": "audio_source",
      "name": "scene-source",
      "address": "CC:1E:57:0F:00:07",
      "target": "sink",
      "config": { "sample_rate_hz": 16000, "octets_per_frame": 40 }
    }
  ]
}
```

The smallest legal scene is much smaller:

```json
{ "version": 1, "devices": [ { "id": "hrm", "device": "hrm" } ] }
```

## 3. Top level

| Field | Type | Required | Meaning |
|---|---|---|---|
| `version` | integer | yes | Format version. Must be `1`. A future version is refused outright rather than half-understood. |
| `name` | string | no | Human-readable scene name, used in listings and log lines. |
| `description` | string | no | Prose. Ignored by the loader; this is where the "why" of a fixture goes, since JSON has no comments. |
| `controller` | string | no | `self` (default), `netsim`, or `usb`. See §7. |
| `devices` | array | yes | At least one device. Brought up in file order. |
| `bonds` | array | no | Pre-existing bonds. See §8. |

**Unknown fields are an error, everywhere.** A misspelt `"adress"` would
otherwise put a device on the air with the wrong identity and fail SMP with no
clue why — a bug this project has already paid for. The format grows by adding
optional fields, and a file that uses a new one needs a `simble` that knows it.

## 4. Devices

| Field | Type | Required | Meaning |
|---|---|---|---|
| `id` | string | yes | Scene-local identifier, unique. Letters, digits, `_`, `-`, `.` — it travels into a netsim URL query string. |
| `role` | string | no | Default `peripheral`. See §6. |
| `name` | string | no | The **node label** the controller registers the device under (what `netsim devices` prints). |
| `address` | string | no | On-air address, `AA:BB:CC:DD:EE:FF`. Omitted means a deterministic scene address (§5). |
| `device` | string | no* | A name in the device catalog (`src/devices/catalog.rs`). |
| `script` | string | no* | Inline Rhai, pinning an exact copy. |
| `target` | string | no* | The peer this device drives, **by `id`**. |
| `config` | object | no | Role-specific placement parameters. See §9. |

\* Exactly one of `device` / `script` for scripted roles; `target` is required
for client roles and forbidden for others. See §6.

### What a device *is* versus where it is *placed*

This separation is load-bearing, not cosmetic:

- **What it is** — `device` or `script`.
- **Where it is placed** — `address`, `name`, `role`, and the scene's
  `controller`.

A device script carries a placeholder address. The *on-air* address is stamped
over it at bring-up (`set_identity`), and SMP mixes that on-air address into
the pairing crypto. A recent pairing failure turned exactly on this: the
script's placeholder address was used for the crypto while the air carried the
netsim URL's address, and SMP failed silently until the two agreed. A format
that let a script own its address would reproduce that bug by design.

`name` is placement too. It is the label netsim lists the device under, so two
devices built from the same catalog script are still tellable apart. It does
**not** change the device's advertised GATT name, which still comes from the
script. If you need a different advertised name, you need a different script.

### `device` (catalog) versus `script` (inline)

| | Use it when |
|---|---|
| `"device": "hrm"` | Normally. The scene stays a topology instead of a wall of embedded script, and the device improves underneath it as the catalog improves. |
| `"script": "…"` | The scene is a CI fixture whose device **must not drift**, or the device does not exist in the catalog. A pinned copy is the point: it fails when *this* script breaks, not when someone improves the shared one. |

The catalog is the same registry the MCP `example` tool serves, so a name that
works in one works in the other. Today it holds:

`hrm`, `thermometer`, `battery`, `env_sensor`, `volume`, `hid_keyboard`,
`hid_mouse`, `gamepad`, `cycling`, `pulse_oximeter`, `weight_scale`,
`smart_lock`, `fitness_tracker`, `eddystone`, `ranging`, `ranging_tag`,
`fast_pair`, `thermostat`.

An unknown name is an error that lists the whole catalog.

### `target`: peers are referenced by `id`, never by address

```json
{ "id": "phone", "role": "central", "target": "sink" }
```

so an address can change — or be left to the allocator entirely — without
breaking the link. Targeting yourself, targeting a device that does not exist,
or giving a `target` to a role that connects to nothing are all errors that
name the offender.

## 5. Addresses

An omitted `address` gets a deterministic scene address: `F0:DE:C0:00:00:01`,
`…:02`, and so on in file order, the same allocator the MCP server uses. It is
deterministic on purpose — a committed fixture must produce the same devices
every run.

- Explicit addresses are claimed **before** auto-assignment, so file order
  cannot silently produce a collision.
- Two devices at one address is an error either way. On the air that failure
  looks intermittent, which is the worst kind.
- `F0:DE:C0:…` has both top bits set, so it is a well-formed random-static
  address rather than something a real stack may reject.

Give explicit addresses when something outside the scene refers to them: a
phone that has already bonded, a test script, a `netsim devices` listing you
want to read at a glance.

## 6. Roles

| Role | Meaning | `device`/`script` | `target` | Instantiated today |
|---|---|---|---|---|
| `peripheral` | GATT server; advertises and answers a central. The default. | exactly one | forbidden | **yes** — `self` and `netsim` |
| `central` | GATT client; connects to `target` and discovers it. | forbidden | required | **yes** — `self` only |
| `scanner` | Passive observer collecting advertising reports. | forbidden | forbidden | **yes** — `self` only |
| `audio_source` | LE Audio Unicast Client: configures `target`'s ASE and opens a CIS to it. | optional | required | **no** |
| `hid_host` | HID host consuming `target`'s input reports. | optional | required | **no** |
| `car_kit` | Hands-free car kit (Classic HFP/A2DP) paired with `target`. | optional | required | **no** |

**Expressible is not the same as instantiated.** The format accepts the whole
vocabulary so a scene can be written against a role that is still being built;
the loader then refuses it *by name*, saying which module will provide it,
rather than failing obscurely or — worse — silently skipping the device:

```
device "source": role audio_source cannot run on controller netsim —
the LE Audio source role is being built (device::cis_central +
profiles::ascs_client); the scene format accepts it, the loader does not host
it yet
```

`--no-run` validates such a scene without trying to host it, which is what CI
should do with a fixture whose role has not landed.

New roles are additive: add a variant to `scene::Role`, add its string, and
every scene written before it keeps parsing.

## 7. Controllers

| Value | Where the devices run |
|---|---|
| `self` | In this process, on an in-process radio. Deterministic, no setup. The **only** controller that hosts centrals and scanners. |
| `netsim` | The Android emulator's netsim, one WebSocket per device. **Peripheral-only**: the far side (a phone, Bumble, another netsim client) plays the central. |
| `usb` | A real dongle. Not wired yet. |

The names match the MCP `run_on` targets deliberately, so a scene file and a
`run_on` call cannot come to disagree about what "netsim" means.

`--controller` overrides the file, so a fixture committed as `netsim` can still
be exercised on `self` by a CI runner with no emulator.

netsim needs `netsimd` running with its WebSocket frontend:

```bash
netsimd --logtostderr --no-shutdown --ws-port 7681
```

**Clocks differ.** On `self`, `--seconds` is *simulated* time advanced in fixed
steps, and the run is deterministic and as fast as the CPU allows. On `netsim`
the far side has its own clock, so the loop is paced against the wall and the
scene is pumped between ticks. A netsim run of `--seconds 30` takes 30 seconds.

## 8. Bonds

A `bonds` entry lets a scene start where pairing has **already succeeded**, so
what gets exercised is everything after it: encryption startup from a stored
LTK, RPA resolution, and CCCD restore on reconnect. That is worth a lot here —
this project spent a long session finding eight distinct bugs in one pairing
chain, and none of them were about what happens once you are bonded.

```json
"bonds": [
  {
    "between": ["lock", "phone"],
    "security": {
      "keys": { "ltk": { "value": "8f3b1c05d94e27a6b0113fd8c27ae415" } },
      "secure_connections": true,
      "authenticated": true,
      "key_size": 16
    },
    "cccds": [ { "handle": 12, "value": 1 } ],
    "known_by": ["lock"],
    "sides": {
      "phone": { "security": { "keys": { "irk": { "value": "…" } } } }
    }
  }
]
```

| Field | Type | Required | Meaning |
|---|---|---|---|
| `between` | `[id, id]` | yes | The two devices, by `id`. |
| `security` | object | yes in practice | Key material and metadata both sides hold. Must carry a long-term key. |
| `cccds` | array | no | Subscriptions to restore: `{ "handle": u16, "value": 1\|2 }`. |
| `known_by` | array of id | no | Which sides remember it. Default: both. |
| `sides` | object | no | Per-side overrides keyed by `id`. |

### A bond is a relationship, so it is declared once

Symmetric by default: one entry materializes into **both** devices' stores,
each keyed by the *other* device's address, which is how the runtime
`BondStore` is keyed. The common case stays a one-liner.

### …but asymmetry is a real failure mode

`"known_by": ["lock"]` says the phone forgot the bond — it will offer an LTK
the lock no longer has, or vice versa. That is a genuine field failure and a
valuable test, so the format states it explicitly rather than requiring you to
write two entries and hope they stay consistent.

`sides` is the other axis: `known_by` controls **existence**, `sides` controls
**content**. Each field in a `sides` entry replaces the shared one for that
side. This is where genuinely per-device material goes — an **IRK belongs to a
device**, so the record A holds about B carries *B's* IRK, not A's. A single
shared `security` block cannot express that; two `sides` entries can.

### Privacy couples to this

If a device uses a resolvable private address, it is the **peer's IRK** in the
peer's bond record that makes resolution work: A resolves B's RPA using the
IRK A stored about B. Privacy configuration itself (whether a device uses an
RPA, and how often it rotates) is **not in the format today**. When it lands it
belongs on the device as placement — alongside `address` — and it will only
work against a bond whose `sides` entries carry the right IRKs.

### `security`

Embeds `device::BondSecurity` **verbatim**, which in turn embeds `smp::
PairingKeys` — the same record shape `smp::KeyStore` persists in
Bumble-compatible JSON. That is deliberate: key material lifted out of a Bumble
keystore pastes straight into a scene, and there is no scene-private key schema
that can drift from the runtime one.

| Field | Meaning |
|---|---|
| `keys.ltk` | Secure Connections LTK (a single `f5`-derived key). |
| `keys.ltk_central`, `keys.ltk_peripheral` | Legacy pairing distributes separate keys. |
| `keys.irk` | Identity Resolving Key — resolves that device's private addresses. |
| `keys.csrk` | Signing key for signed writes. |
| `keys.link_key` | BR/EDR link key from cross-transport key derivation. |
| `keys.address_type` | The identity address type, if an IRK was distributed. |
| `secure_connections`, `authenticated` | How the bond was formed. Default `false`. |
| `key_size` | 7..=16. Omitted means 16, and is written back explicitly on save. |

Each key is `{ "value": "<32 hex chars>", "authenticated": bool?, "ediv": u16?,
"rand": "<16 hex chars>"? }`.

A bond with no long-term key at all is rejected: it cannot start encryption, so
it is not a bond.

### Scene files may contain key material — and that is fine

These are **simulation keys for simulated devices**. A committed fixture's LTK
is as sensitive as its fake heart rate, and inventing a secrets mechanism for
it would be theatre.

**Never put a real device's or a real dongle's keys in a committed fixture.**
If you have paired a phone or a dongle for real and want to replay that bond,
keep the file out of the repo. The format cannot tell the difference; you can.

## 9. `config`

Role-specific **placement** parameters — an audio source's stream parameters,
for instance. It is deliberately *not* a place for device behaviour: that lives
in the script, per §1. The line to hold is that `config` configures the Rust
machinery a role brings with it (`CisConfig`, `AseConfig`), never what the
device does over time.

Today the loader carries `config` through and validates nothing inside it,
because no instantiated role reads it yet. It is documented here so that a
scene written for `audio_source` now stays valid when the role lands.

## 10. What the loader actually does today

| The format expresses | The loader does |
|---|---|
| `peripheral` on `self` / `netsim` | hosts it |
| `central`, `scanner` on `self` | hosts it |
| `central`, `scanner` on `netsim` | refuses, saying the far side plays that part |
| `audio_source`, `hid_host`, `car_kit` | refuses by name, saying which module will provide it |
| `controller: "usb"` | refuses, pointing at `simble --usb` |
| `config` | carries it through; no role reads it yet |
| `bonds` | **validates and materializes** them into a `MemoryBondStore` per device — and stops there |

The bond gap is worth stating precisely, because a bond that looks applied but
is not would be exactly the kind of silent failure this format is meant to
prevent. `Scene::resolve` builds each device a real `MemoryBondStore` holding
the right `BondSecurity` and CCCD records, keyed by peer address (this is
tested). What it cannot yet do is hand that store to a running device:
`VirtualDevice::bond_store` is reachable through
`ScriptGattServer::with_server`, but `ScriptedPeripheral` — the type that owns
the servers a script built — exposes no accessor for them. One additive method
on `ScriptedPeripheral` closes it. Until then, running a scene with bonds
prints:

```
note: 2 declared bond record(s) were validated but not installed —
the loader cannot yet reach a scripted device's bond store
```

**No bonded reconnection has been demonstrated.** Do not read the `bonds`
section as working pre-pairing yet.

## 11. Round trip

`Scene::from_json` → `Scene::to_json` → `Scene::from_json` is the identity,
and every committed example is tested for it. Absent optional fields stay
absent on save, so a diff of a saved scene shows what changed rather than a
wall of nulls. Two normalizations happen at parse and are written back
explicitly: an omitted bond `key_size` becomes 16.

## 12. Designed-for, not built

Deliberately out of scope, but the format is shaped for them:

- **MCP `load_scene` / `save_scene`.** `Scene::from_json` / `to_json` /
  `resolve` are the whole surface such tools need. `controller` uses the
  `run_on` vocabulary so the two cannot disagree.
- **Web import/export.** The Scene page's spec array
  (`{ id, kind, address, target, script }`) maps onto `devices` one-to-one;
  its `kind` is this format's `role`, and its `in-page` / `websocket` backends
  are `self` / `netsim`.

## 13. Example scenes

In `examples/scenes/`:

| File | What it shows |
|---|---|
| `heart-rate-monitor.json` | The smallest complete scene. |
| `central-and-two-peripherals.json` | Wiring by `id`; centrals and scanners on `self`. |
| `netsim-sensors.json` | Peripheral-only on netsim, with explicit addresses and node names. |
| `bonded-reconnect.json` | A `bonds` section with a per-side override and a restored CCCD. |
| `le-audio-unicast.json` | A role the format expresses and the loader does not host yet. |
