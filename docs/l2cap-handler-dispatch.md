# L2CAP handler dispatch is keyed on PSM; channel identity is passed through

**Context.** `ClassicHost::handle_channel_data` resolved a handler with
`self.handlers.iter_mut().find(|h| h.psm() == psm)` — one PSM, one handler. Two
profiles broke that assumption at once:

- **Classic HID** needs *two* PSMs distinguished: Control `0x0011` and Interrupt
  `0x0013`.
- **A2DP/AVDTP** is the mirror image: *one* PSM (`0x0019`) carrying two channels
  with different roles — signalling, then media transport.

**Decision.** Handler lookup stays keyed on PSM. `ProtocolHandler` gained five
**defaulted** methods so channel identity reaches the handler that needs it:
`psms()`, `on_channel_data(HandlerChannel, ..)`, `poll_channel_output(..)`,
`on_channel_open(..)`/`on_channel_lost(..)`, and `poll_channel_requests()`.

**Rejected: keying the host's table on `(psm, cid)`.** It fails at exactly the
moment it would have to work. When a second `0x0019` connection request
arrives, the **host** has no way to know what role that channel plays. Only the
profile knows, and only because an AVDTP `OPEN` just succeeded. Routing
decisions belong where the knowledge is.

**Consequences.**

- All three pre-existing handlers (`SdpHandler`, `RfcommHandler`,
  `SdpQueryHandler`) needed **zero edits** — the defaults preserve them, and 17+
  tests plus the Car page kept working untouched.
- A multi-channel handler must key its own per-channel state on CID, and
  `poll_channel_output` is called once *per channel*, so it must answer for the
  channel it was asked about and no other. That is real bookkeeping pushed into
  the handler (`A2dpSource` keeps a map from CID to queued SDUs).
- `on_channel_closed()` now fires only when a handler's **last** channel goes.
  Previously any channel closing on that PSM ended the session — which would have
  made an AVDTP media channel closing kill the signalling session.
