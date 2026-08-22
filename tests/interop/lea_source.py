"""Stream real liblc3 audio to a simble LE Audio sink over a real CIS."""
import asyncio, math, struct, sys
import lc3
from bumble.transport import open_transport
from bumble.device import Device, Peer, CigParameters

TARGET = sys.argv[1] if len(sys.argv) > 1 else 'CC:1E:57:00:00:06/P'
RATE, DUR_US, FRAME_BYTES = 16000, 10000, 40
ASE_CP = '00002bc6-0000-1000-8000-00805f9b34fb'
SINK_ASE = '00002bc4-0000-1000-8000-00805f9b34fb'

async def main():
    t = await open_transport('tcp-client:127.0.0.1:6402')
    d = Device.with_hci('lea-source', 'F0:F1:F2:F3:F4:D1', t.source, t.sink)
    d.cis_enabled = True
    await d.power_on()
    conn = await d.connect(TARGET)
    print(f'connected to {TARGET}, acl handle {hex(conn.handle)}')

    peer = Peer(conn)
    await peer.discover_services()
    for s in peer.services:
        await s.discover_characteristics()
    cp = peer.get_characteristics_by_uuid(ASE_CP)
    ase = peer.get_characteristics_by_uuid(SINK_ASE)
    if not cp:
        print('!! no ASE control point — the sink has no ASCS'); return
    print('found ASCS: control point + sink ASE')

    # Configure the endpoint the way Android does.
    await cp[0].write_value(bytes([0x01,0x01, 0x01,0x02,0x02, 0x06,0,0,0,0, 0x10,
        0x02,0x01,0x03, 0x02,0x02,0x01, 0x05,0x03,0x01,0,0,0, 0x03,0x04,0x28,0x00]),
        with_response=True); await asyncio.sleep(0.5)
    await cp[0].write_value(bytes([0x02,0x01, 0x01, 0x01,0x01, 0x10,0x27,0x00, 0x00,
        0x02, 0x28,0x00, 0x02, 0x0A,0x00, 0x40,0x9C,0x00]), with_response=True)
    await asyncio.sleep(0.5)
    await cp[0].write_value(bytes([0x03,0x01, 0x01, 0x04, 0x03,0x02,0x04,0x00]),
        with_response=True); await asyncio.sleep(0.5)
    state = await ase[0].read_value()
    print('ASE state after Enable:', hex(state[1]))

    # Real CIS.
    handles = await d.setup_cig(CigParameters(
        cig_id=1,
        cis_parameters=[CigParameters.CisParameters(cis_id=1, max_sdu_c_to_p=FRAME_BYTES, max_sdu_p_to_c=0)],
        sdu_interval_c_to_p=DUR_US, sdu_interval_p_to_c=DUR_US,
        max_transport_latency_c_to_p=10, max_transport_latency_p_to_c=10))
    links = await d.create_cis([(handles[0], conn)])
    link = links[0]
    print('CIS established:', hex(link.handle))
    await link.setup_data_path(direction=0)
    print('ISO data path open — streaming liblc3 audio')

    # A rising arpeggio so it is obviously music, encoded by Google's liblc3.
    enc = lc3.Encoder(DUR_US, RATE)
    n = enc.get_frame_samples()
    notes = [440.0, 554.37, 659.25, 880.0]
    sent, phase = 0, 0.0
    for f in range(500):                      # 5 seconds at 10 ms
        hz = notes[(f // 50) % len(notes)]
        pcm = []
        for i in range(n):
            phase += 2 * math.pi * hz / RATE
            pcm.append(int(math.sin(phase) * 12000))
        frame = enc.encode(struct.pack(f'<{n}h', *pcm), FRAME_BYTES, bit_depth=16)
        link.write(frame)
        sent += 1
        await asyncio.sleep(DUR_US / 1_000_000)
    print(f'=== streamed {sent} liblc3 frames ({sent * DUR_US / 1_000_000:.1f}s of audio)')
    await asyncio.sleep(1)

asyncio.run(asyncio.wait_for(main(), 90))
