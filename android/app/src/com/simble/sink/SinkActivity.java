// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

package com.simble.sink;

import android.Manifest;
import android.app.Activity;
import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.bluetooth.BluetoothGatt;
import android.bluetooth.BluetoothGattCharacteristic;
import android.bluetooth.BluetoothGattDescriptor;
import android.bluetooth.BluetoothGattServer;
import android.bluetooth.BluetoothGattServerCallback;
import android.bluetooth.BluetoothGattService;
import android.bluetooth.BluetoothManager;
import android.bluetooth.le.AdvertiseCallback;
import android.bluetooth.le.AdvertiseData;
import android.bluetooth.le.AdvertiseSettings;
import android.bluetooth.le.BluetoothLeAdvertiser;
import android.content.pm.PackageManager;
import android.os.Build;
import android.os.Bundle;
import android.os.ParcelUuid;
import android.util.Log;
import android.view.Gravity;
import android.view.WindowManager;
import android.widget.LinearLayout;
import android.widget.TextView;

import java.util.UUID;

/**
 * The peripheral half of SimBLE's bulk-transfer benchmark, on a real phone.
 *
 * <p>This is a measuring instrument, not a demo. It counts what arrives and
 * reports that count back over the control point, because the central cannot
 * know it: the benchmark writes without response, so "the central finished
 * writing" happens well before "the phone received the last byte", and a
 * client-only stopwatch is wrong in the flattering direction. The number this
 * app sends back is the one worth quoting.
 *
 * <p>Deliberately an Activity rather than the headless service {@code
 * android/README.md} describes. The full design runs Rhai on the device and
 * needs JNI, the NDK and a Gradle build; none of that is required to put a
 * real Android host stack and a real controller on the receiving end of a
 * transfer, which is the whole point of a first phone measurement. A visible
 * counter is also the fastest way to see a run stall.
 */
public class SinkActivity extends Activity implements StatsServer.Stats {

    private static final String TAG = "SimbleSink";

    /** Bulk Transfer Service — matches {@code bulk_uuid::SERVICE} in Rust. */
    private static final UUID SERVICE =
            UUID.fromString("f0bb0001-1234-5678-90ab-cdef01234567");
    /** Where the payload lands. Write and Write Without Response. */
    private static final UUID DATA =
            UUID.fromString("f0bb0002-1234-5678-90ab-cdef01234567");
    /** Begin/finish in, our own count out. Write and Notify. */
    private static final UUID CONTROL =
            UUID.fromString("f0bb0003-1234-5678-90ab-cdef01234567");
    /** Client Characteristic Configuration, the standard 0x2902. */
    private static final UUID CCCD =
            UUID.fromString("00002902-0000-1000-8000-00805f9b34fb");

    // Control-point opcodes, matching `control_op` in Rust.
    private static final byte BEGIN = 0x01;
    private static final byte FINISH = 0x02;
    private static final byte REPORT = 0x03;

    private BluetoothGattServer server;
    private BluetoothLeAdvertiser advertiser;
    private BluetoothGattCharacteristic control;

    private long bytes;
    private long chunks;
    private long expected;
    private long firstByteMs;
    private long lastByteMs;
    private boolean advertising;

    private TextView status;
    private TextView counters;
    private TextView endpoint;

    private StatsServer statsServer;
    private Thread statsThread;
    private int mtu;
    private String peer = "";

    @Override
    protected void onCreate(Bundle saved) {
        super.onCreate(saved);
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        buildUi();

        // Started before the permission check: the counters are worth serving
        // even from a run that never got a radio.
        statsServer = new StatsServer(this);
        statsThread = new Thread(statsServer, "simble-stats");
        statsThread.setDaemon(true);
        statsThread.start();
        runOnUiThread(() -> endpoint.setText("http://" + StatsServer.address() + "/stats"));

        // Granted out of band in practice:
        //   adb shell pm grant com.simble.sink android.permission.BLUETOOTH_ADVERTISE
        //   adb shell pm grant com.simble.sink android.permission.BLUETOOTH_CONNECT
        // Asking here too keeps the app usable when launched by hand.
        String[] needed = {
            Manifest.permission.BLUETOOTH_ADVERTISE,
            Manifest.permission.BLUETOOTH_CONNECT,
        };
        for (String p : needed) {
            if (checkSelfPermission(p) != PackageManager.PERMISSION_GRANTED) {
                requestPermissions(needed, 1);
                say("waiting for Bluetooth permission");
                return;
            }
        }
        start();
    }

    @Override
    public void onRequestPermissionsResult(int code, String[] perms, int[] results) {
        for (int r : results) {
            if (r != PackageManager.PERMISSION_GRANTED) {
                say("Bluetooth permission refused — cannot advertise");
                return;
            }
        }
        start();
    }

    private void start() {
        BluetoothManager manager = getSystemService(BluetoothManager.class);
        BluetoothAdapter adapter = manager != null ? manager.getAdapter() : null;
        if (adapter == null || !adapter.isEnabled()) {
            say("Bluetooth is off — enable it and relaunch");
            return;
        }

        server = manager.openGattServer(this, gattCallback);
        if (server == null) {
            say("could not open a GATT server");
            return;
        }

        BluetoothGattService service =
                new BluetoothGattService(SERVICE, BluetoothGattService.SERVICE_TYPE_PRIMARY);

        BluetoothGattCharacteristic data = new BluetoothGattCharacteristic(
                DATA,
                BluetoothGattCharacteristic.PROPERTY_WRITE
                        | BluetoothGattCharacteristic.PROPERTY_WRITE_NO_RESPONSE,
                BluetoothGattCharacteristic.PERMISSION_WRITE);

        control = new BluetoothGattCharacteristic(
                CONTROL,
                BluetoothGattCharacteristic.PROPERTY_WRITE
                        | BluetoothGattCharacteristic.PROPERTY_NOTIFY,
                BluetoothGattCharacteristic.PERMISSION_WRITE);
        // Without a CCCD the central's subscribe fails and the run falls back
        // to "unconfirmed" — bytes sent, not bytes delivered.
        BluetoothGattDescriptor cccd = new BluetoothGattDescriptor(
                CCCD,
                BluetoothGattDescriptor.PERMISSION_READ
                        | BluetoothGattDescriptor.PERMISSION_WRITE);
        control.addDescriptor(cccd);

        service.addCharacteristic(data);
        service.addCharacteristic(control);
        server.addService(service);

        advertiser = adapter.getBluetoothLeAdvertiser();
        if (advertiser == null) {
            say("this device cannot advertise");
            return;
        }
        AdvertiseSettings settings = new AdvertiseSettings.Builder()
                .setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_LOW_LATENCY)
                .setTxPowerLevel(AdvertiseSettings.ADVERTISE_TX_POWER_HIGH)
                .setConnectable(true)
                .build();
        // A 128-bit UUID is 16 of the 31 octets, so the name goes in the scan
        // response rather than the advertisement.
        AdvertiseData advertisement = new AdvertiseData.Builder()
                .setIncludeDeviceName(false)
                .addServiceUuid(new ParcelUuid(SERVICE))
                .build();
        AdvertiseData scanResponse = new AdvertiseData.Builder()
                .setIncludeDeviceName(true)
                .build();
        advertiser.startAdvertising(settings, advertisement, scanResponse, advertiseCallback);
    }

    private final AdvertiseCallback advertiseCallback = new AdvertiseCallback() {
        @Override
        public void onStartSuccess(AdvertiseSettings settings) {
            // Android advertises from a rotating resolvable private address and
            // does not tell the app what it is, so the central has to find us by
            // service UUID rather than by a address written down in advance.
            advertising = true;
            say("advertising f0bb0001 — waiting for a central");
            Log.i(TAG, "advertising started");
        }

        @Override
        public void onStartFailure(int error) {
            say("advertising failed (" + error + ")");
            Log.e(TAG, "advertising failed: " + error);
        }
    };

    private final BluetoothGattServerCallback gattCallback = new BluetoothGattServerCallback() {
        @Override
        public void onConnectionStateChange(BluetoothDevice device, int status, int state) {
            if (state == BluetoothGatt.STATE_CONNECTED) {
                peer = device.getAddress();
                say("connected to " + device.getAddress());
            } else {
                say("disconnected — advertising again");
            }
            Log.i(TAG, "connection state " + state + " status " + status);
        }

        @Override
        public void onMtuChanged(BluetoothDevice device, int mtu) {
            SinkActivity.this.mtu = mtu;
            Log.i(TAG, "MTU " + mtu);
            say("connected, MTU " + mtu);
        }

        @Override
        public void onCharacteristicWriteRequest(
                BluetoothDevice device,
                int requestId,
                BluetoothGattCharacteristic characteristic,
                boolean preparedWrite,
                boolean responseNeeded,
                int offset,
                byte[] value) {

            if (DATA.equals(characteristic.getUuid())) {
                if (value != null && value.length > 0) {
                    count(value.length);
                    showCounters();
                }
            } else if (CONTROL.equals(characteristic.getUuid())) {
                handleControl(device, value);
            }

            // Write Without Response arrives with responseNeeded false, and
            // answering one is a protocol error rather than a courtesy.
            if (responseNeeded) {
                server.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, offset, null);
            }
        }

        @Override
        public void onDescriptorWriteRequest(
                BluetoothDevice device,
                int requestId,
                BluetoothGattDescriptor descriptor,
                boolean preparedWrite,
                boolean responseNeeded,
                int offset,
                byte[] value) {
            if (responseNeeded) {
                server.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, offset, null);
            }
            if (CCCD.equals(descriptor.getUuid())) {
                say("central subscribed to the control point");
            }
        }
    };

    private void handleControl(BluetoothDevice device, byte[] value) {
        if (value == null || value.length == 0) {
            return;
        }
        switch (value[0]) {
            case BEGIN:
                bytes = 0;
                chunks = 0;
                firstByteMs = 0;
                lastByteMs = 0;
                expected = value.length >= 5 ? readU32(value, 1) : 0;
                showCounters();
                say("run started — expecting " + expected + " bytes");
                Log.i(TAG, "BEGIN expecting " + expected);
                break;
            case FINISH:
                // Report whatever we have, so a short count is visible rather
                // than a hang. A run that lost bytes is still a measurement.
                byte[] report = new byte[9];
                report[0] = REPORT;
                writeU32(report, 1, bytes);
                writeU32(report, 5, chunks);
                notifyControl(device, report);
                long ms = lastByteMs - firstByteMs;
                Log.i(TAG, "FINISH " + bytes + " bytes in " + chunks + " chunks over " + ms + " ms");
                say(bytes == expected
                        ? "run complete — all " + bytes + " bytes"
                        : "run complete — " + bytes + " of " + expected + " bytes");
                break;
            default:
                break;
        }
    }

    /// One arrival. Synchronized because writes land on a binder thread and
    /// the stats server reads from its own.
    private synchronized void count(int length) {
        long now = System.currentTimeMillis();
        if (bytes == 0) {
            firstByteMs = now;
        }
        lastByteMs = now;
        bytes += length;
        chunks++;
    }

    private void notifyControl(BluetoothDevice device, byte[] payload) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            server.notifyCharacteristicChanged(device, control, false, payload);
        } else {
            control.setValue(payload);
            server.notifyCharacteristicChanged(device, control, false);
        }
    }

    private static long readU32(byte[] b, int at) {
        return (b[at] & 0xFFL)
                | ((b[at + 1] & 0xFFL) << 8)
                | ((b[at + 2] & 0xFFL) << 16)
                | ((b[at + 3] & 0xFFL) << 24);
    }

    private static void writeU32(byte[] b, int at, long v) {
        b[at] = (byte) (v & 0xFF);
        b[at + 1] = (byte) ((v >> 8) & 0xFF);
        b[at + 2] = (byte) ((v >> 16) & 0xFF);
        b[at + 3] = (byte) ((v >> 24) & 0xFF);
    }

    // -- what the stats server serves ---------------------------------------

    /// The counters, as JSON.
    ///
    /// `duration_ms` is the span between the first and last byte *on this
    /// device's clock*. A duration needs no agreement about epochs, which is
    /// what makes it quotable by a caller whose clock is unrelated — and it
    /// covers only the data path, with none of the round trip that ending a
    /// run over the link itself would have included.
    @Override
    public synchronized String json() {
        long duration = (bytes > 0 && lastByteMs >= firstByteMs) ? lastByteMs - firstByteMs : 0;
        StringBuilder out = new StringBuilder();
        out.append('{');
        out.append("\"bytes\":").append(bytes).append(',');
        out.append("\"chunks\":").append(chunks).append(',');
        out.append("\"expected\":").append(expected).append(',');
        out.append("\"duration_ms\":").append(duration).append(',');
        out.append("\"first_byte_ms\":").append(firstByteMs).append(',');
        out.append("\"last_byte_ms\":").append(lastByteMs).append(',');
        out.append("\"mtu\":").append(mtu).append(',');
        out.append("\"peer\":\"").append(peer).append("\",");
        out.append("\"advertising\":").append(advertising);
        out.append('}');
        return out.toString();
    }

    /// Zeroes the counters for the next run.
    ///
    /// The out-of-band twin of a `BEGIN` on the control point, so a run can be
    /// set up and read back without the link carrying anything but payload.
    @Override
    public synchronized void reset(long expected) {
        bytes = 0;
        chunks = 0;
        firstByteMs = 0;
        lastByteMs = 0;
        this.expected = expected;
        showCounters();
        say(expected > 0
                ? "reset over HTTP — expecting " + expected + " bytes"
                : "counters reset over HTTP");
    }

    // -- the visible part ---------------------------------------------------

    private void buildUi() {
        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setGravity(Gravity.CENTER);
        root.setPadding(48, 48, 48, 48);

        TextView title = new TextView(this);
        title.setText("SimBLE Sink");
        title.setTextSize(28);
        title.setGravity(Gravity.CENTER);

        status = new TextView(this);
        status.setTextSize(16);
        status.setGravity(Gravity.CENTER);
        status.setPadding(0, 32, 0, 32);

        counters = new TextView(this);
        counters.setTextSize(20);
        counters.setGravity(Gravity.CENTER);

        endpoint = new TextView(this);
        endpoint.setTextSize(13);
        endpoint.setGravity(Gravity.CENTER);
        endpoint.setPadding(0, 32, 0, 0);

        root.addView(title);
        root.addView(status);
        root.addView(counters);
        root.addView(endpoint);
        setContentView(root);
        showCounters();
    }

    private void say(String message) {
        runOnUiThread(() -> status.setText(message));
        Log.i(TAG, message);
    }

    private void showCounters() {
        runOnUiThread(() -> counters.setText(bytes + " bytes · " + chunks + " chunks"));
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        if (advertiser != null) {
            advertiser.stopAdvertising(advertiseCallback);
        }
        if (server != null) {
            server.close();
        }
        if (statsServer != null) {
            statsServer.stop();
        }
    }
}
