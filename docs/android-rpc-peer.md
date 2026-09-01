# A scriptable phone: SimBLE scripts on real Android

> Superseded by [`phone-as-backend.md`](phone-as-backend.md), which reverses the
> recommendation (the script runs on the device, not per-call RPC). Only the
> Android-API boundary analysis below is kept.

## The boundary, stated before anyone starts

**SimBLE is a host stack and needs a controller. Android does not give an app
one.** An app gets the framework's Bluetooth API; HCI is not reachable without
root. So the app cannot *run* SimBLE — it must **interpret the script and call
the Android API**. Everything below GATT is out of reach, and no amount of
effort changes that:

| Reachable from an Android app | Not reachable |
|---|---|
| GATT server: services, characteristics, descriptors, values, notifications | Raw HCI of any kind |
| GATT client: connect, discover, read, write, subscribe | PHY selection (1M / 2M / Coded) |
| Advertising via `AdvertiseData` (service UUIDs, service data, manufacturer data) | Connection interval, latency, supervision timeout |
| Connection as a peer, MTU request | Data Length Extension control |
| | Arbitrary AD structures — `AdvertiseData` is a builder, not a byte array |
| | LE Audio / BIS — restricted or absent for normal apps |
| | The `tick()` model: no equivalent; the app must drive device physics on its own timer |

**What the phone therefore is:** a scriptable GATT peer with a real Bluetooth
5.3 radio — 2M PHY and Data Length Extension in practice, negotiated by the
framework rather than chosen by us. Excellent for device-level interop and for
the Data benchmark. Useless for controller-level work, which stays with
dongles.
