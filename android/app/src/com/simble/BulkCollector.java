// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

package com.simble;

import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.bluetooth.BluetoothGatt;
import android.bluetooth.BluetoothGattCallback;
import android.bluetooth.BluetoothSocket;
import android.bluetooth.le.BluetoothLeScanner;
import android.bluetooth.le.ScanCallback;
import android.bluetooth.le.ScanFilter;
import android.bluetooth.le.ScanResult;
import android.bluetooth.le.ScanSettings;
import android.content.Context;
import android.os.Handler;
import android.os.Looper;
import android.os.ParcelUuid;
import android.util.Log;

import java.io.IOException;
import java.io.InputStream;
import java.util.Collections;
import java.util.List;
import java.util.UUID;

/**
 * The collector (subscriber) half of the publish/collect pattern.
 *
 * <p>A {@link BulkPublisher} advertises {@code [generation][size][PSM]} and
 * holds a payload on an L2CAP server. This scans for that advertisement,
 * dedupes on the generation — if the advertised generation is no newer than
 * {@code since}, there is nothing to collect and it says so without connecting —
 * and otherwise connects, opens the L2CAP channel, and reads the payload. The
 * connection is the delivery: a publisher watching its socket drain knows the
 * generation was collected.
 *
 * <p>The 2M PHY is requested on a GATT connection to the publisher (the socket
 * has no PHY control), exactly as the L2CAP-min transfer path does — one MTU
 * exchange to let the fast connection interval settle, then the socket read.
 */
final class BulkCollector {

    private static final String TAG = "SimbleCollector";

    private static final UUID SERVICE =
            UUID.fromString("f0bb0001-1234-5678-90ab-cdef01234567");
    /** Company id the publisher tags its `[gen][size][psm]` advert with. */
    private static final int PSM_COMPANY_ID = 0xFFFF;

    private static final long RUN_TIMEOUT_MS = 40_000L;
    private static final long SCAN_TIMEOUT_MS = 15_000L;
    private static final long PHY_SETTLE_MS = 120L;

    interface Listener {
        void status(String message);
        /** collected = bytes read (0 if nothing new); generation = what was
         *  seen; fresh = a payload was actually collected this run. */
        void finished(long generation, long collected, long ms, boolean fresh);
    }

    private final Context ctx;
    private final BluetoothAdapter adapter;
    private final String targetName;   // null/empty = the first publisher seen
    private final long since;          // the generation already held; pull only newer
    private final boolean fast;
    private final Listener listener;
    private final Handler main = new Handler(Looper.getMainLooper());

    private BluetoothLeScanner scanner;
    private BluetoothGatt gatt;
    private BluetoothSocket socket;
    private BluetoothDevice publisher;

    private int psm;
    private long generation;           // from the advert
    private long size;                 // from the advert
    private long startMs;
    private boolean done;
    private boolean transferStarted;

    BulkCollector(Context ctx, BluetoothAdapter adapter, String targetName,
                  long since, boolean fast, Listener listener) {
        this.ctx = ctx;
        this.adapter = adapter;
        this.targetName = targetName == null ? "" : targetName.trim();
        this.since = since;
        this.fast = fast;
        this.listener = listener;
    }

    void start() {
        scanner = adapter.getBluetoothLeScanner();
        if (scanner == null) {
            fail("this device cannot scan");
            return;
        }
        List<ScanFilter> filters = Collections.singletonList(
                new ScanFilter.Builder().setServiceUuid(new ParcelUuid(SERVICE)).build());
        ScanSettings settings = new ScanSettings.Builder()
                .setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY)
                .build();
        say(targetName.isEmpty()
                ? "scanning for a publisher…"
                : "scanning for \"" + targetName + "\"…");
        try {
            scanner.startScan(filters, settings, scanCallback);
        } catch (SecurityException e) {
            fail("scan permission missing (BLUETOOTH_SCAN)");
            return;
        }
        main.postDelayed(this::onScanTimeout, SCAN_TIMEOUT_MS);
        main.postDelayed(this::onRunTimeout, RUN_TIMEOUT_MS);
    }

    private final ScanCallback scanCallback = new ScanCallback() {
        @Override
        public void onScanResult(int callbackType, ScanResult result) {
            if (done || publisher != null || result.getScanRecord() == null) {
                return;
            }
            String name = result.getScanRecord().getDeviceName();
            if (!targetName.isEmpty() && (name == null || !name.equalsIgnoreCase(targetName))) {
                return;
            }
            byte[] mfg = result.getScanRecord().getManufacturerSpecificData(PSM_COMPANY_ID);
            if (mfg == null || mfg.length < 10) {
                return; // not a publisher advert (no gen/size/psm)
            }
            long gen = u32(mfg, 0);
            long sz = u32(mfg, 4);
            int p = (mfg[8] & 0xFF) | ((mfg[9] & 0xFF) << 8);
            // Dedupe: nothing to do if the publisher is not past what we hold.
            if (gen <= since) {
                stopScan();
                done = true;
                main.removeCallbacksAndMessages(null);
                say("up to date — publisher at generation " + gen + ", have " + since);
                listener.finished(gen, 0, 0, false);
                return;
            }
            generation = gen;
            size = sz;
            psm = p;
            publisher = result.getDevice();
            stopScan();
            say("generation " + gen + " (" + sz + " bytes) available — collecting");
            connect(publisher);
        }

        @Override
        public void onScanFailed(int errorCode) {
            fail("scan failed (" + errorCode + ")");
        }
    };

    private void stopScan() {
        try {
            scanner.stopScan(scanCallback);
        } catch (SecurityException ignored) {
        }
    }

    private void connect(BluetoothDevice device) {
        startMs = System.currentTimeMillis();
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
                // Request 2M on the shared ACL (the socket has no PHY control),
                // then one MTU exchange so the fast interval settles, then read.
                int phyMask = fast ? BluetoothDevice.PHY_LE_2M_MASK : BluetoothDevice.PHY_LE_1M_MASK;
                safe(() -> g.setPreferredPhy(phyMask, phyMask, BluetoothDevice.PHY_OPTION_NO_PREFERRED));
                safe(() -> g.requestConnectionPriority(fast
                        ? BluetoothGatt.CONNECTION_PRIORITY_HIGH
                        : BluetoothGatt.CONNECTION_PRIORITY_BALANCED));
                safe(() -> g.requestMtu(517));
            } else if (state == BluetoothGatt.STATE_DISCONNECTED && !done) {
                fail("link dropped (status " + status + ")");
            }
        }

        @Override
        public void onMtuChanged(BluetoothGatt g, int mtu, int status) {
            // The one ATT round trip is done — pull the payload off the socket.
            if (!transferStarted) {
                transferStarted = true;
                main.postDelayed(BulkCollector.this::readL2cap, PHY_SETTLE_MS);
            }
        }

        @Override
        public void onPhyUpdate(BluetoothGatt g, int txPhy, int rxPhy, int status) {
            Log.i(TAG, "PHY tx=" + txPhy + " rx=" + rxPhy + " status=" + status);
        }
    };

    /** Opens the L2CAP channel and reads the whole payload the publisher writes,
     *  then acks so the publisher knows the generation was delivered. */
    private void readL2cap() {
        Thread worker = new Thread(() -> {
            long got = 0;
            try {
                socket = publisher.createInsecureL2capChannel(psm);
                socket.connect();
                InputStream in = socket.getInputStream();
                long want = readLength(in); // the publisher's [len] header
                byte[] buf = new byte[65536];
                int n;
                while (got < want && (n = in.read(buf)) > 0) {
                    got += n;
                    if ((got & 0x3FFF) == 0) {
                        long g = got;
                        main.post(() -> listener.status("collecting… " + g + " / " + want + " bytes"));
                    }
                }
                socket.getOutputStream().write(1); // ack: got it
                socket.getOutputStream().flush();
            } catch (IOException | SecurityException e) {
                Log.w(TAG, "L2CAP read failed: " + e);
                main.post(() -> fail("L2CAP read failed: " + e.getMessage()));
                return;
            }
            long total = got;
            main.post(() -> report(total));
        }, "simble-collect-rx");
        worker.setDaemon(true);
        worker.start();
    }

    private void report(long collected) {
        if (done) {
            return;
        }
        done = true;
        main.removeCallbacksAndMessages(null);
        long ms = startMs > 0 ? System.currentTimeMillis() - startMs : 0;
        boolean fresh = collected > 0;
        Log.i(TAG, "span{collect} closed busy_ms=" + ms
                + " generation=" + generation + " bytes=" + collected
                + " fresh=" + (fresh ? 1 : 0));
        closeSocket();
        try {
            if (gatt != null) {
                gatt.disconnect();
                gatt.close();
            }
        } catch (SecurityException ignored) {
        }
        listener.finished(generation, collected, ms, fresh);
    }

    private void onScanTimeout() {
        if (!done && publisher == null) {
            done = true;
            main.removeCallbacksAndMessages(null);
            stopScan();
            say("no publisher found");
            listener.finished(since, 0, 0, false);
        }
    }

    private void onRunTimeout() {
        if (!done) {
            Log.w(TAG, "collect timed out after " + RUN_TIMEOUT_MS + " ms");
            report(0);
        }
    }

    private void closeSocket() {
        if (socket != null) {
            try {
                socket.close();
            } catch (IOException ignored) {
            }
            socket = null;
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
        report(0);
    }

    private static long u32(byte[] b, int at) {
        return (b[at] & 0xFFL)
                | ((b[at + 1] & 0xFFL) << 8)
                | ((b[at + 2] & 0xFFL) << 16)
                | ((b[at + 3] & 0xFFL) << 24);
    }

    private static long readLength(InputStream in) throws IOException {
        byte[] head = new byte[4];
        int got = 0;
        while (got < 4) {
            int n = in.read(head, got, 4 - got);
            if (n < 0) {
                throw new IOException("stream closed before length header");
            }
            got += n;
        }
        return u32(head, 0);
    }
}
