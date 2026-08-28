// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

package com.simble;

import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.bluetooth.BluetoothGatt;
import android.bluetooth.BluetoothGattCharacteristic;
import android.bluetooth.BluetoothGattServer;
import android.bluetooth.BluetoothGattServerCallback;
import android.bluetooth.BluetoothGattService;
import android.bluetooth.BluetoothManager;
import android.bluetooth.BluetoothServerSocket;
import android.bluetooth.BluetoothSocket;
import android.bluetooth.le.AdvertiseCallback;
import android.bluetooth.le.AdvertiseData;
import android.bluetooth.le.AdvertiseSettings;
import android.bluetooth.le.BluetoothLeAdvertiser;
import android.content.Context;
import android.os.ParcelUuid;
import android.util.Log;

import java.io.IOException;
import java.io.OutputStream;
import java.util.UUID;

/**
 * The publisher half of the publish/collect pattern.
 *
 * <p>Holds the latest payload and a monotonic generation, and raises its hand:
 * it advertises {@code [generation(4)][size(4)][PSM(2)]} as manufacturer data
 * so a {@link BulkCollector} learns — without connecting — whether there is
 * anything new and how big it is. A collector past its own generation connects,
 * and the publisher writes the payload over the L2CAP channel; the collector's
 * ack is the delivery receipt.
 *
 * <p>Latest-only: {@link #setGeneration} replaces the payload outright and
 * re-advertises, so only the current generation is ever served. Advertising is
 * offloaded to the controller and continues while the phone dozes, so the
 * hand stays raised for free until a collector comes.
 */
final class BulkPublisher {

    private static final String TAG = "SimblePublisher";

    private static final UUID SERVICE =
            UUID.fromString("f0bb0001-1234-5678-90ab-cdef01234567");
    /** A placeholder characteristic so the collector's GATT connect (which it
     *  needs only to request 2M on the shared ACL) has a server to reach. */
    private static final UUID PRESENCE =
            UUID.fromString("f0bb0005-1234-5678-90ab-cdef01234567");
    private static final int PSM_COMPANY_ID = 0xFFFF;

    interface Listener {
        void status(String message);
    }

    private final Context ctx;
    private final BluetoothAdapter adapter;
    private final Listener listener;

    private BluetoothGattServer gattServer;
    private BluetoothServerSocket l2capServer;
    private BluetoothLeAdvertiser advertiser;
    private Thread acceptThread;

    private int psm;
    private volatile long generation;
    private volatile int size;         // bytes in the current payload

    BulkPublisher(Context ctx, BluetoothAdapter adapter, long generation, int size,
                  Listener listener) {
        this.ctx = ctx;
        this.adapter = adapter;
        this.generation = generation;
        this.size = size;
        this.listener = listener;
    }

    void start() {
        BluetoothManager manager = (BluetoothManager) ctx.getSystemService(Context.BLUETOOTH_SERVICE);
        // A minimal GATT server: the collector connects to it only to hold a
        // 2M PHY request on the ACL; no characteristic is ever read.
        try {
            gattServer = manager.openGattServer(ctx, gattCallback);
            BluetoothGattService service =
                    new BluetoothGattService(SERVICE, BluetoothGattService.SERVICE_TYPE_PRIMARY);
            service.addCharacteristic(new BluetoothGattCharacteristic(
                    PRESENCE,
                    BluetoothGattCharacteristic.PROPERTY_READ,
                    BluetoothGattCharacteristic.PERMISSION_READ));
            gattServer.addService(service);
        } catch (Exception e) {
            listener.status("could not open a GATT server: " + e);
            return;
        }
        // The L2CAP server that hands out the payload.
        try {
            l2capServer = adapter.listenUsingInsecureL2capChannel();
            psm = l2capServer.getPsm();
            Log.i(TAG, "publisher L2CAP on PSM " + psm);
            acceptThread = new Thread(this::acceptLoop, "simble-publish");
            acceptThread.setDaemon(true);
            acceptThread.start();
        } catch (Exception e) {
            listener.status("could not open an L2CAP server: " + e);
            return;
        }
        startAdvertising();
        listener.status("publishing generation " + generation + " (" + size + " bytes), PSM " + psm);
    }

    /** Replaces the payload with a new generation and re-advertises it. */
    void setGeneration(long gen, int newSize) {
        generation = gen;
        size = newSize;
        startAdvertising(); // re-emit the [gen][size][psm] with the new values
        listener.status("now publishing generation " + gen + " (" + newSize + " bytes)");
    }

    long generation() {
        return generation;
    }

    private void startAdvertising() {
        if (advertiser == null) {
            advertiser = adapter.getBluetoothLeAdvertiser();
            if (advertiser == null) {
                listener.status("this device cannot advertise");
                return;
            }
        }
        advertiser.stopAdvertising(advertiseCallback); // restart with fresh metadata
        AdvertiseSettings settings = new AdvertiseSettings.Builder()
                .setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_LOW_LATENCY)
                .setTxPowerLevel(AdvertiseSettings.ADVERTISE_TX_POWER_HIGH)
                .setConnectable(true)
                .build();
        AdvertiseData advertisement = new AdvertiseData.Builder()
                .setIncludeDeviceName(false)
                .addServiceUuid(new ParcelUuid(SERVICE))
                .build();
        // [generation(4 LE)][size(4 LE)][psm(2 LE)] — the collector reads this
        // off the scan and decides whether to bother connecting.
        byte[] meta = new byte[10];
        putU32(meta, 0, generation);
        putU32(meta, 4, size);
        meta[8] = (byte) (psm & 0xFF);
        meta[9] = (byte) ((psm >> 8) & 0xFF);
        AdvertiseData scanResponse = new AdvertiseData.Builder()
                .setIncludeDeviceName(true)
                .addManufacturerData(PSM_COMPANY_ID, meta)
                .build();
        advertiser.startAdvertising(settings, advertisement, scanResponse, advertiseCallback);
    }

    /** Accepts one collector at a time and writes the current payload:
     *  {@code [len(4)][payload]}, then waits for the collector's one-byte ack
     *  (the delivery receipt) before looping for the next. */
    private void acceptLoop() {
        byte[] chunk = new byte[4096];
        for (int i = 0; i < chunk.length; i++) {
            chunk[i] = (byte) (i & 0xFF); // a ramp; the collector counts length
        }
        while (l2capServer != null) {
            try (BluetoothSocket s = l2capServer.accept()) {
                int n = size; // snapshot the generation being served
                status("collector connected — sending generation " + generation);
                OutputStream out = s.getOutputStream();
                byte[] header = new byte[4];
                putU32(header, 0, n);
                out.write(header);
                int left = n;
                while (left > 0) {
                    int w = Math.min(chunk.length, left);
                    out.write(chunk, 0, w);
                    left -= w;
                }
                out.flush();
                s.getInputStream().read(); // block on the collector's ack
                status("generation " + generation + " delivered");
            } catch (IOException e) {
                if (l2capServer != null) {
                    Log.w(TAG, "publisher accept ended: " + e);
                }
                return;
            }
        }
    }

    void stop() {
        BluetoothServerSocket l2 = l2capServer;
        l2capServer = null;
        if (l2 != null) {
            try {
                l2.close();
            } catch (IOException ignored) {
            }
        }
        if (advertiser != null) {
            try {
                advertiser.stopAdvertising(advertiseCallback);
            } catch (Exception ignored) {
            }
        }
        if (gattServer != null) {
            gattServer.close();
        }
    }

    private final BluetoothGattServerCallback gattCallback = new BluetoothGattServerCallback() {
        @Override
        public void onConnectionStateChange(BluetoothDevice device, int status, int state) {
            Log.i(TAG, "collector GATT state " + state + " status " + status);
        }
    };

    private final AdvertiseCallback advertiseCallback = new AdvertiseCallback() {
        @Override
        public void onStartFailure(int error) {
            Log.w(TAG, "advertising failed: " + error);
        }
    };

    private void status(String message) {
        listener.status(message);
    }

    private static void putU32(byte[] b, int at, long v) {
        b[at] = (byte) (v & 0xFF);
        b[at + 1] = (byte) ((v >> 8) & 0xFF);
        b[at + 2] = (byte) ((v >> 16) & 0xFF);
        b[at + 3] = (byte) ((v >> 24) & 0xFF);
    }
}
