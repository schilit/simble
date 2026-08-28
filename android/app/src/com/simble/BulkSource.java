// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

package com.simble;

import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.bluetooth.BluetoothGatt;
import android.bluetooth.BluetoothGattCallback;
import android.bluetooth.BluetoothGattCharacteristic;
import android.bluetooth.BluetoothGattDescriptor;
import android.bluetooth.BluetoothSocket;
import android.bluetooth.BluetoothStatusCodes;
import android.bluetooth.le.BluetoothLeScanner;
import android.bluetooth.le.ScanCallback;
import android.bluetooth.le.ScanFilter;
import android.bluetooth.le.ScanResult;
import android.bluetooth.le.ScanSettings;
import android.content.Context;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;
import android.os.ParcelUuid;
import android.util.Log;

import java.io.IOException;
import java.io.OutputStream;
import java.util.Collections;
import java.util.List;
import java.util.UUID;

/**
 * The central/source half of the benchmark, on a phone.
 *
 * <p>{@link SimbleActivity} has always been the receiving end; the sending end
 * was a host driving a USB controller. This drives the same transfer from a
 * second phone's own radio instead, so a phone-to-phone run can be measured
 * with no dongle in the path. The sink still counts and reports the bytes — the
 * authoritative number — over its HTTP endpoint; this side only has to push the
 * payload as fast as Android's GATT client will queue it.
 *
 * <p>Android hides the credit pipeline the native central manages by hand: a
 * Write Without Response returns immediately and {@link
 * BluetoothGattCallback#onCharacteristicWrite} fires when the stack has room
 * for the next. Chaining the next write from that callback is the documented
 * backpressure, and what this class does — one chunk in flight, MTU-sized.
 */
final class BulkSource {

    private static final String TAG = "SimbleSource";

    // Same service, characteristics, and opcodes the sink exposes.
    private static final UUID SERVICE =
            UUID.fromString("f0bb0001-1234-5678-90ab-cdef01234567");
    private static final UUID DATA =
            UUID.fromString("f0bb0002-1234-5678-90ab-cdef01234567");
    private static final UUID CONTROL =
            UUID.fromString("f0bb0003-1234-5678-90ab-cdef01234567");
    private static final UUID CCCD =
            UUID.fromString("00002902-0000-1000-8000-00805f9b34fb");
    private static final UUID PSM =
            UUID.fromString("f0bb0004-1234-5678-90ab-cdef01234567");
    private static final byte BEGIN = 0x01;
    private static final byte FINISH = 0x02;
    private static final byte REPORT = 0x03;

    /** How long a whole run may take before we give up and report short. */
    private static final long RUN_TIMEOUT_MS = 40_000L;
    /** How long to wait for the sink's REPORT after we send FINISH. */
    private static final long REPORT_GRACE_MS = 4_000L;

    interface Listener {
        void status(String message);
        /** acked = bytes the sink counted (or bytes we sent, if no report). */
        void finished(long acked, long chunks, long ms, boolean complete);
    }

    private final Context ctx;
    private final BluetoothAdapter adapter;
    private final String targetName;   // null / empty = the first sink seen
    private final long total;
    // The fast link: 2M PHY + high connection priority. Off runs the 1M
    // baseline. (Android exposes no Data Length control, so DLE — the third
    // fast lever on the dongle path — cannot be toggled here.)
    private final boolean fast;
    // Stream the payload over an L2CAP CoC socket instead of GATT writes. GATT
    // is metered ~one write per connection event; L2CAP rides its own credit-
    // based flow control, so it can pack the event. The GATT link is still set
    // up (discovery, control point, REPORT) — only the payload changes path.
    private final boolean l2cap;
    private final Listener listener;
    private final Handler main = new Handler(Looper.getMainLooper());

    private BluetoothLeScanner scanner;
    private BluetoothGatt gatt;
    private BluetoothDevice peerDevice;   // for opening the L2CAP channel
    private BluetoothSocket l2capSocket;  // held open until the sink confirms
    private BluetoothGattCharacteristic dataCh;
    private BluetoothGattCharacteristic controlCh;

    private int chunkSize = 20;        // MTU-3, resolved once the MTU is up
    private int psm;                   // the sink's L2CAP PSM, 0 = none (use GATT)
    private int negotiatedMtu = 23;    // the ATT default until onMtuChanged fires
    private int txPhy;                  // 0 until onPhyUpdate; 1=1M 2=2M 3=coded
    private int rxPhy;
    private long sent;                 // bytes handed to the stack
    private long chunks;
    private long startMs;              // the first data chunk went out (transfer start)
    // The current phase, as a tracing-style span. Entering the next phase closes
    // the last; the four closed spans — discover, connect, negotiate, transfer —
    // are the run's breakdown, the same one the dongle path reports. Those four
    // segments matter more than the megabits: setup latency is often the larger
    // half of "how long until it lands".
    private Span phase;
    private long sinkBytes = -1;       // from the REPORT notification
    private long sinkChunks = -1;
    private boolean discovering;       // service discovery has been kicked off
    private boolean finishing;
    private boolean done;

    /** If the MTU exchange stalls this long, discover at the default MTU anyway
     *  rather than hang the whole run on it — some stacks (a beta Android build
     *  has been seen to) never answer requestMtu. */
    private static final long MTU_STALL_MS = 5_000L;

    BulkSource(Context ctx, BluetoothAdapter adapter, String targetName,
               long total, boolean fast, boolean l2cap, Listener listener) {
        this.ctx = ctx;
        this.adapter = adapter;
        this.targetName = targetName == null ? "" : targetName.trim();
        this.total = total;
        this.fast = fast;
        this.l2cap = l2cap;
        this.listener = listener;
    }

    void start() {
        enter("discover");
        scanner = adapter.getBluetoothLeScanner();
        if (scanner == null) {
            fail("this device cannot scan");
            return;
        }
        // Filter on the service UUID so we wake only for SimBLE sinks, not the
        // whole neighbourhood. The name (if given) is checked on the result,
        // because Android advertises the name in the scan response.
        List<ScanFilter> filters = Collections.singletonList(
                new ScanFilter.Builder()
                        .setServiceUuid(new ParcelUuid(SERVICE))
                        .build());
        ScanSettings settings = new ScanSettings.Builder()
                .setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY)
                .build();
        say(targetName.isEmpty()
                ? "scanning for a SimBLE sink…"
                : "scanning for \"" + targetName + "\"…");
        try {
            scanner.startScan(filters, settings, scanCallback);
        } catch (SecurityException e) {
            fail("scan permission missing (BLUETOOTH_SCAN)");
            return;
        }
        main.postDelayed(this::onRunTimeout, RUN_TIMEOUT_MS);
    }

    private final ScanCallback scanCallback = new ScanCallback() {
        @Override
        public void onScanResult(int callbackType, ScanResult result) {
            if (done || gatt != null) {
                return;
            }
            String name = null;
            if (result.getScanRecord() != null) {
                name = result.getScanRecord().getDeviceName();
            }
            if (!targetName.isEmpty()
                    && (name == null || !name.equalsIgnoreCase(targetName))) {
                return; // a sink, but not the one asked for
            }
            BluetoothDevice device = result.getDevice();
            String shown = name != null ? name : device.getAddress();
            try {
                scanner.stopScan(this);
            } catch (SecurityException ignored) {
            }
            enter("connect");
            say("found " + shown + " — connecting");
            connect(device);
        }

        @Override
        public void onScanFailed(int errorCode) {
            fail("scan failed (" + errorCode + ")");
        }
    };

    private void connect(BluetoothDevice device) {
        peerDevice = device;
        try {
            gatt = device.connectGatt(ctx, false, gattCallback, BluetoothDevice.TRANSPORT_LE);
        } catch (SecurityException e) {
            fail("connect permission missing (BLUETOOTH_CONNECT)");
        }
    }

    private final BluetoothGattCallback gattCallback = new BluetoothGattCallback() {
        @Override
        public void onConnectionStateChange(BluetoothGatt g, int status, int state) {
            if (state == BluetoothGatt.STATE_CONNECTED) {
                enter("negotiate");
                say("connected — negotiating MTU");
                safe(() -> g.requestMtu(517));
                // Don't hang forever on a peer that never answers requestMtu:
                // discover at the default MTU if it stalls.
                main.postDelayed(() -> {
                    if (!discovering && !done) {
                        Log.w(TAG, "MTU exchange stalled — discovering at default MTU");
                        beginDiscovery(g);
                    }
                }, MTU_STALL_MS);
            } else if (state == BluetoothGatt.STATE_DISCONNECTED && !done) {
                // 0x3E-class failures land here mid-run; report what we pushed.
                fail("link dropped (status " + status + ")");
            }
        }

        @Override
        public void onMtuChanged(BluetoothGatt g, int mtu, int status) {
            // Cap at 512: the max BLE attribute value length. MTU-3 can be 514,
            // and a 514-byte write is invalid and silently dropped by the peer.
            chunkSize = Math.min(Math.max(20, mtu - 3), 512);
            negotiatedMtu = mtu;
            say("MTU " + mtu);
            beginDiscovery(g);
        }

        @Override
        public void onPhyUpdate(BluetoothGatt g, int txPhy, int rxPhy, int status) {
            Log.i(TAG, "PHY tx=" + txPhy + " rx=" + rxPhy + " status=" + status);
            if (status == BluetoothGatt.GATT_SUCCESS) {
                BulkSource.this.txPhy = txPhy;
                BulkSource.this.rxPhy = rxPhy;
            }
        }

        @Override
        public void onServicesDiscovered(BluetoothGatt g, int status) {
            if (g.getService(SERVICE) == null) {
                fail("sink has no bulk service");
                return;
            }
            dataCh = g.getService(SERVICE).getCharacteristic(DATA);
            controlCh = g.getService(SERVICE).getCharacteristic(CONTROL);
            if (dataCh == null || controlCh == null) {
                fail("sink is missing the data or control characteristic");
                return;
            }
            // Subscribe to the control point so we hear the sink's REPORT.
            g.setCharacteristicNotification(controlCh, true);
            BluetoothGattDescriptor cccd = controlCh.getDescriptor(CCCD);
            if (cccd == null) {
                fail("control point has no CCCD");
                return;
            }
            writeDescriptor(g, cccd, BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE);
        }

        @Override
        public void onDescriptorWrite(BluetoothGatt g, BluetoothGattDescriptor d, int status) {
            if (CCCD.equals(d.getUuid())) {
                if (l2cap) {
                    // Read the sink's L2CAP PSM before the transfer; BEGIN goes
                    // out once we have it (or once we know to fall back to GATT).
                    say("subscribed — reading L2CAP PSM");
                    BluetoothGattCharacteristic psmCh = g.getService(SERVICE).getCharacteristic(PSM);
                    if (psmCh != null && safeReadPsm(g, psmCh)) {
                        return;
                    }
                    Log.w(TAG, "no PSM characteristic — falling back to GATT");
                }
                sendBegin(g);
            }
        }

        @Override
        public void onCharacteristicRead(BluetoothGatt g,
                                         BluetoothGattCharacteristic ch, int status) {
            onPsmRead(ch, status == BluetoothGatt.GATT_SUCCESS ? ch.getValue() : null);
        }

        // API 33+ delivers the read value as an argument.
        @Override
        public void onCharacteristicRead(BluetoothGatt g,
                                         BluetoothGattCharacteristic ch, byte[] value, int status) {
            onPsmRead(ch, status == BluetoothGatt.GATT_SUCCESS ? value : null);
        }

        @Override
        public void onCharacteristicWrite(BluetoothGatt g,
                                          BluetoothGattCharacteristic ch, int status) {
            if (CONTROL.equals(ch.getUuid())) {
                if (finishing) {
                    // FINISH is acknowledged; wait a beat for the REPORT notify,
                    // then close out on whatever we have.
                    main.postDelayed(() -> report(false), REPORT_GRACE_MS);
                } else {
                    // BEGIN is acknowledged — open the taps. Post rather than
                    // call in-line: a GATT op issued from inside its own callback
                    // is unreliable on Android's stack.
                    startMs = System.currentTimeMillis();
                    enter("transfer");
                    if (psm > 0) {
                        streamL2cap();          // payload over the socket
                    } else {
                        main.post(() -> pump(g)); // payload over GATT writes
                    }
                }
            } else if (DATA.equals(ch.getUuid())) {
                main.post(() -> pump(g)); // confirmed write done — send the next
            }
        }

        // API 33+ delivers the notification bytes as an argument.
        @Override
        public void onCharacteristicChanged(BluetoothGatt g,
                                            BluetoothGattCharacteristic ch, byte[] value) {
            onReport(ch, value);
        }

        // API 31/32 delivers them through the characteristic instead.
        @Override
        @SuppressWarnings("deprecation")
        public void onCharacteristicChanged(BluetoothGatt g,
                                            BluetoothGattCharacteristic ch) {
            onReport(ch, ch.getValue());
        }
    };

    /** Sends BEGIN so the sink zeroes its counters and learns the length. */
    private void sendBegin(BluetoothGatt g) {
        say("subscribed — starting the transfer");
        byte[] begin = new byte[5];
        begin[0] = BEGIN;
        putU32(begin, 1, total);
        writeControl(g, begin);
    }

    /** Reads the PSM characteristic; false if the read could not be issued. */
    private boolean safeReadPsm(BluetoothGatt g, BluetoothGattCharacteristic ch) {
        try {
            return g.readCharacteristic(ch);
        } catch (SecurityException e) {
            return false;
        }
    }

    /** The PSM read came back: capture it (0 = sink has no L2CAP, stay on GATT),
     *  then BEGIN. */
    private void onPsmRead(BluetoothGattCharacteristic ch, byte[] value) {
        if (PSM.equals(ch.getUuid())) {
            if (value != null && value.length >= 2) {
                psm = (value[0] & 0xFF) | ((value[1] & 0xFF) << 8);
            }
            if (psm > 0) {
                say("sink L2CAP PSM " + psm);
            } else {
                Log.w(TAG, "sink reported no L2CAP PSM — using GATT");
            }
            main.post(() -> {
                if (gatt != null) {
                    sendBegin(gatt);
                }
            });
        }
    }

    /**
     * Streams the whole payload over an L2CAP CoC socket on a worker thread,
     * then closes it and sends FINISH so the sink reports its count. The socket
     * bypasses GATT/ATT: L2CAP's credit-based flow control packs the connection
     * event, where a GATT write is metered roughly one per event.
     */
    private void streamL2cap() {
        Thread worker = new Thread(() -> {
            try {
                l2capSocket = peerDevice.createInsecureL2capChannel(psm);
                l2capSocket.connect();
                OutputStream out = l2capSocket.getOutputStream();
                byte[] chunk = new byte[Math.min(4096, (int) Math.min(total, Integer.MAX_VALUE))];
                for (int i = 0; i < chunk.length; i++) {
                    chunk[i] = (byte) (i & 0xFF); // a ramp; the sink counts length
                }
                long left = total;
                while (left > 0 && !done) {
                    int n = (int) Math.min(chunk.length, left);
                    out.write(chunk, 0, n);
                    left -= n;
                    sent += n;
                    if ((sent & 0x3FFF) == 0) {
                        long s = sent;
                        main.post(() -> listener.status("streaming… " + s + " / " + total + " bytes"));
                    }
                }
                out.flush();
            } catch (IOException | SecurityException e) {
                Log.w(TAG, "L2CAP stream failed: " + e);
                main.post(() -> fail("L2CAP stream failed: " + e.getMessage()));
                return;
            }
            // The bytes are written, but a socket write returns once buffered,
            // not once transmitted — closing now would discard the tail. Leave
            // the socket open (report() closes it) and send FINISH; the sink
            // waits for its byte count to reach `expected` before REPORTing, so
            // the credit-controlled drain finishes first.
            main.post(() -> {
                if (done || gatt == null) {
                    return;
                }
                finishing = true;
                byte[] fin = {FINISH};
                writeControl(gatt, fin);
            });
        }, "simble-l2cap-tx");
        worker.setDaemon(true);
        worker.start();
    }

    /** Applies the fast-link PHY/priority preferences and starts service
     *  discovery — once, whether reached by a normal MTU change or the stall
     *  fallback. Fast: 2M PHY + high priority; baseline: 1M + balanced. */
    private void beginDiscovery(BluetoothGatt g) {
        if (discovering || done) {
            return;
        }
        discovering = true;
        int phyMask = fast ? BluetoothDevice.PHY_LE_2M_MASK : BluetoothDevice.PHY_LE_1M_MASK;
        safe(() -> g.setPreferredPhy(phyMask, phyMask, BluetoothDevice.PHY_OPTION_NO_PREFERRED));
        safe(() -> g.requestConnectionPriority(fast
                ? BluetoothGatt.CONNECTION_PRIORITY_HIGH
                : BluetoothGatt.CONNECTION_PRIORITY_BALANCED));
        say("discovering services");
        safe(g::discoverServices);
    }

    private void onReport(BluetoothGattCharacteristic ch, byte[] value) {
        if (CONTROL.equals(ch.getUuid()) && value != null && value.length >= 9
                && value[0] == REPORT) {
            sinkBytes = readU32(value, 1);
            sinkChunks = readU32(value, 5);
            report(true);
        }
    }

    /**
     * Fills the stack's Write-Without-Response queue until it reports busy, so
     * several chunks ride each connection event instead of one. Each queued
     * write's {@link BluetoothGattCallback#onCharacteristicWrite} calls back here
     * to top the queue up as it drains.
     *
     * <p>Chaining one write deep (write, await the callback, write the next) left
     * roughly one 512-byte chunk per connection interval on the table — about
     * 50 KB/s at an ~9 ms interval. Packing the connection event is what the 2M
     * PHY is for.
     *
     * <p>Chunks are capped at 512 bytes — the maximum BLE attribute value. MTU-3
     * works out to 514, and a 514-byte write is silently dropped.
     */
    private void pump(BluetoothGatt g) {
        if (done || finishing) {
            return;
        }
        while (sent < total) {
            int len = (int) Math.min(chunkSize, total - sent);
            byte[] chunk = new byte[len];
            for (int i = 0; i < len; i++) {
                chunk[i] = (byte) ((sent + i) & 0xFF); // a ramp; the sink counts length only
            }
            int rc = writeData(g, chunk);
            if (rc != BluetoothStatusCodes.SUCCESS && rc != BluetoothGatt.GATT_SUCCESS) {
                // Queue full — a completion callback will call pump() again as it
                // drains. (RUN_TIMEOUT covers a stack that never calls back.)
                return;
            }
            sent += len;
            chunks++;
            if ((chunks & 0x3F) == 0) {
                long s = sent;
                main.post(() -> listener.status("sending… " + s + " / " + total + " bytes"));
            }
        }
        // Everything is queued, but a no-response write returns before it reaches
        // the air — let the tail drain before FINISH, or the sink counts short.
        finishing = true;
        main.postDelayed(() -> {
            byte[] fin = {FINISH};
            writeControl(g, fin);
        }, 250);
    }

    private void report(boolean complete) {
        if (done) {
            return;
        }
        done = true;
        main.removeCallbacksAndMessages(null);
        long ms = startMs > 0 ? System.currentTimeMillis() - startMs : 0;
        long acked = sinkBytes >= 0 ? sinkBytes : sent;
        long ch = sinkChunks >= 0 ? sinkChunks : chunks;
        boolean ok = complete && acked >= total;

        // Close the open span with the run's totals. On a clean run that is the
        // transfer span; a run that failed early closes on whatever phase it had
        // reached, so its cost is still recorded and the bridge still gets a
        // final (ok=…) span to key on.
        if (phase != null) {
            phase.close("bytes=" + acked + " ok=" + (ok ? 1 : 0)
                    + " mtu=" + negotiatedMtu + " txphy=" + txPhy + " rxphy=" + rxPhy
                    + " link=" + (psm > 0 ? "l2cap" : "gatt"));
            phase = null;
        }
        if (l2capSocket != null) {
            try {
                l2capSocket.close(); // now safe: the sink has REPORTed its count
            } catch (IOException ignored) {
            }
            l2capSocket = null;
        }
        try {
            if (gatt != null) {
                gatt.disconnect();
                gatt.close();
            }
        } catch (SecurityException ignored) {
        }
        listener.finished(acked, ch, ms, ok);
    }

    private void onRunTimeout() {
        if (!done) {
            Log.w(TAG, "run timed out after " + RUN_TIMEOUT_MS + " ms");
            report(false);
        }
    }

    // -- version-straddling GATT writes -------------------------------------

    private void writeControl(BluetoothGatt g, byte[] value) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            safe(() -> g.writeCharacteristic(controlCh, value,
                    BluetoothGattCharacteristic.WRITE_TYPE_DEFAULT));
        } else {
            controlCh.setWriteType(BluetoothGattCharacteristic.WRITE_TYPE_DEFAULT);
            controlCh.setValue(value);
            safe(() -> g.writeCharacteristic(controlCh));
        }
    }

    private int writeData(BluetoothGatt g, byte[] value) {
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                return g.writeCharacteristic(dataCh, value,
                        BluetoothGattCharacteristic.WRITE_TYPE_NO_RESPONSE);
            }
            dataCh.setWriteType(BluetoothGattCharacteristic.WRITE_TYPE_NO_RESPONSE);
            dataCh.setValue(value);
            return g.writeCharacteristic(dataCh)
                    ? BluetoothGatt.GATT_SUCCESS : BluetoothGatt.GATT_FAILURE;
        } catch (SecurityException e) {
            return BluetoothGatt.GATT_FAILURE;
        }
    }

    private void writeDescriptor(BluetoothGatt g, BluetoothGattDescriptor d, byte[] value) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            safe(() -> g.writeDescriptor(d, value));
        } else {
            d.setValue(value);
            safe(() -> g.writeDescriptor(d));
        }
    }

    // -- helpers ------------------------------------------------------------

    private interface Action {
        void run() throws SecurityException;
    }

    private void safe(Action a) {
        try {
            a.run();
        } catch (SecurityException e) {
            fail("a Bluetooth permission is missing");
        }
    }

    private void say(String message) {
        main.post(() -> listener.status(message));
    }

    private void fail(String why) {
        say(why);
        report(false);
    }

    /// Enters the next phase span, closing the previous one. The first call
    /// opens the first span; {@link #report} closes the last.
    private void enter(String name) {
        if (phase != null) {
            phase.close("");
        }
        phase = new Span(name);
    }

    /// A minimal tracing-style span: a named phase, timed from when it opened.
    /// Closing it logs one structured line — {@code span{name} closed
    /// busy_ms=…} — the shape tracing's fmt layer prints, so a phase's cost is a
    /// span rather than a bespoke field, and a new phase is a new span, not a new
    /// column for every reader to learn.
    private final class Span {
        private final String name;
        private final long openedMs = System.currentTimeMillis();

        Span(String name) {
            this.name = name;
        }

        void close(String fields) {
            long busy = System.currentTimeMillis() - openedMs;
            Log.i(TAG, "span{" + name + "} closed busy_ms=" + busy
                    + (fields.isEmpty() ? "" : " " + fields));
        }
    }

    private static void putU32(byte[] b, int at, long v) {
        b[at] = (byte) (v & 0xFF);
        b[at + 1] = (byte) ((v >> 8) & 0xFF);
        b[at + 2] = (byte) ((v >> 16) & 0xFF);
        b[at + 3] = (byte) ((v >> 24) & 0xFF);
    }

    private static long readU32(byte[] b, int at) {
        return (b[at] & 0xFFL)
                | ((b[at + 1] & 0xFFL) << 8)
                | ((b[at + 2] & 0xFFL) << 16)
                | ((b[at + 3] & 0xFFL) << 24);
    }
}
