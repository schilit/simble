// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

package com.simble;

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
import android.bluetooth.BluetoothServerSocket;
import android.bluetooth.BluetoothSocket;
import android.bluetooth.le.AdvertiseCallback;
import android.bluetooth.le.AdvertiseData;
import android.bluetooth.le.AdvertiseSettings;
import android.bluetooth.le.BluetoothLeAdvertiser;
import android.content.Context;
import android.content.pm.PackageManager;
import android.net.wifi.WifiManager;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.ParcelUuid;
import android.os.PowerManager;
import android.util.Log;
import android.view.Gravity;
import android.widget.LinearLayout;
import android.widget.TextView;

import java.io.IOException;
import java.io.InputStream;
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
public class SimbleActivity extends Activity implements StatsServer.Stats {

    private static final String TAG = "SimbleAndroid";

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
    /** The L2CAP CoC PSM to stream payload over, read by a source that wants
     *  the socket path instead of GATT writes. Two little-endian bytes. */
    private static final UUID PSM =
            UUID.fromString("f0bb0004-1234-5678-90ab-cdef01234567");

    // Control-point opcodes, matching `control_op` in Rust.
    private static final byte BEGIN = 0x01;
    private static final byte FINISH = 0x02;
    private static final byte REPORT = 0x03;

    private BluetoothGattServer server;
    private BluetoothLeAdvertiser advertiser;
    private BluetoothGattCharacteristic control;

    /** The L2CAP CoC server: a stream socket that bypasses GATT/ATT entirely,
     *  so payload rides L2CAP's own credit-based flow control rather than one
     *  metered write per connection event. Its PSM is published in the PSM
     *  characteristic for a source to read. Null on a device too old for it. */
    private BluetoothServerSocket l2capServer;
    private int l2capPsm;
    private Thread l2capThread;

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
    /// The currently connected central, if any, so a reset can drop a stale link
    /// — a page reloaded mid-run leaves its central connected, and the next run
    /// would collide with it. Held only between connect and disconnect.
    private BluetoothDevice connectedDevice;
    /// The name this device advertises — `BluetoothAdapter.getName()`, which
    /// is what lands in the scan response. It is the only handle a scanner has
    /// on *which* phone answered: Android advertises from a rotating private
    /// address and will not tell even its own app what that address is, so a
    /// caller with two phones cannot tell them apart by address.
    private String advertisedName = "";

    /** Set from the launch intent: {@code --es role source} drives a transfer
     *  to another phone instead of receiving one. Default is the sink. */
    private boolean sourceMode;

    /** CPU + wifi, held while in use and for a while after, so idle phones sleep. */
    private PowerManager.WakeLock wakeLock;
    private WifiManager.WifiLock wifiLock;
    private Handler awakeHandler;
    /**
     * How long to stay awake after the last thing that talked to us — a BLE
     * connection or an HTTP stats request. Long enough that the web page's phone
     * list (which polls each phone's stats) keeps a phone discoverable through a
     * working session; short enough that a forgotten phone sleeps within the
     * half hour. A 20-second grace was too brief: the phone slept between polls,
     * its stats HTTP died with the CPU, and the page then showed it "not running".
     */
    private static final long AWAKE_TIMEOUT_MS = 30 * 60 * 1000L;
    /** True between a peer's connect and disconnect, so the idle timer never
     *  cuts a run that is still in flight. */
    private volatile boolean peerConnected;

    @Override
    protected void onCreate(Bundle saved) {
        super.onCreate(saved);
        // Locks are taken on demand and released after an idle timeout — not
        // held around the clock (which drained four phones), and not dropped the
        // instant a peer leaves (which made an idle phone undiscoverable: its
        // stats HTTP dies with the CPU, so the web page's poll can't confirm it).
        // Anything that talks to us — a BLE connection or an HTTP stats poll —
        // calls touch(), holding the wake + wifi locks and re-arming the idle
        // release. A phone in active use stays awake and listable; a forgotten
        // one sleeps. (BLE advertising is offloaded to the controller and keeps
        // going even while the phone dozes, so a central can still connect.)
        awakeHandler = new Handler(Looper.getMainLooper());
        initLocks();
        touch(); // discoverable for the first idle window after launch
        buildUi();

        // Started before the permission check: the counters are worth serving
        // even from a run that never got a radio.
        statsServer = new StatsServer(this);
        statsThread = new Thread(statsServer, "simble-stats");
        statsThread.setDaemon(true);
        statsThread.start();
        runOnUiThread(() -> endpoint.setText("http://" + StatsServer.address() + "/stats"));

        // Granted out of band in practice:
        //   adb shell pm grant com.simble android.permission.BLUETOOTH_ADVERTISE
        //   adb shell pm grant com.simble android.permission.BLUETOOTH_CONNECT
        // Asking here too keeps the app usable when launched by hand.
        sourceMode = "source".equals(getIntent().getStringExtra("role"));
        String[] needed = sourceMode
                ? new String[] {
                    Manifest.permission.BLUETOOTH_SCAN,
                    Manifest.permission.BLUETOOTH_CONNECT,
                }
                : new String[] {
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
        begin();
    }

    /** Sink or source, whichever the launch intent asked for. */
    private void begin() {
        if (sourceMode) {
            startSource();
        } else {
            start();
        }
    }

    /** Drive a bulk transfer to another phone's sink over this radio. The sink
     *  reports the authoritative byte count over HTTP, exactly as in a run from
     *  a USB controller; this side just pushes the payload. */
    private void startSource() {
        BluetoothManager manager = getSystemService(BluetoothManager.class);
        BluetoothAdapter adapter = manager != null ? manager.getAdapter() : null;
        if (adapter == null || !adapter.isEnabled()) {
            say("Bluetooth is off — enable it and relaunch");
            return;
        }
        String target = getIntent().getStringExtra("target");
        long total = getIntent().getLongExtra("bytes", getIntent().getIntExtra("bytes", 65536));
        boolean fast = getIntent().getIntExtra("fast", 1) != 0;
        boolean l2cap = "l2cap".equals(getIntent().getStringExtra("link"));
        touch();
        showCounters();
        say("source mode" + (target != null ? " → " + target : "") + " — " + total + " bytes"
                + (l2cap ? " (L2CAP)" : ""));
        BulkSource src = new BulkSource(this, adapter, target, total, fast, l2cap, new BulkSource.Listener() {
            @Override
            public void status(String message) {
                say(message);
            }

            @Override
            public void finished(long acked, long chunkCount, long ms, boolean complete) {
                bytes = acked;
                chunks = chunkCount;
                showCounters();
                double kbs = ms > 0 ? acked / 1024.0 / (ms / 1000.0) : 0;
                say(complete
                        ? String.format("done — %d bytes in %d ms (%.1f KB/s)", acked, ms, kbs)
                        : "stopped — " + acked + " of " + total + " bytes");
                touch();
            }
        });
        src.start();
    }

    @Override
    public void onRequestPermissionsResult(int code, String[] perms, int[] results) {
        for (int r : results) {
            if (r != PackageManager.PERMISSION_GRANTED) {
                say("Bluetooth permission refused");
                return;
            }
        }
        begin();
    }

    private void start() {
        BluetoothManager manager = getSystemService(BluetoothManager.class);
        BluetoothAdapter adapter = manager != null ? manager.getAdapter() : null;
        if (adapter == null || !adapter.isEnabled()) {
            say("Bluetooth is off — enable it and relaunch");
            return;
        }

        advertisedName = adapter.getName() == null ? "" : adapter.getName();
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

        // The L2CAP CoC server, and a characteristic that publishes its PSM.
        // A source reads the PSM and streams payload over the socket, bypassing
        // GATT. Best-effort: a device or run that cannot open one simply has no
        // PSM to offer and the source stays on the GATT path.
        openL2capServer(adapter);
        BluetoothGattCharacteristic psm = new BluetoothGattCharacteristic(
                PSM,
                BluetoothGattCharacteristic.PROPERTY_READ,
                BluetoothGattCharacteristic.PERMISSION_READ);

        service.addCharacteristic(data);
        service.addCharacteristic(control);
        service.addCharacteristic(psm);
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

    /** Opens the L2CAP CoC server and starts accepting on it, storing the PSM
     *  the source will read. Insecure: no pairing, matching the run's model. */
    private void openL2capServer(BluetoothAdapter adapter) {
        try {
            l2capServer = adapter.listenUsingInsecureL2capChannel();
            l2capPsm = l2capServer.getPsm();
            Log.i(TAG, "L2CAP server on PSM " + l2capPsm);
            l2capThread = new Thread(this::acceptL2cap, "simble-l2cap");
            l2capThread.setDaemon(true);
            l2capThread.start();
        } catch (Exception e) {
            l2capServer = null;
            l2capPsm = 0;
            Log.w(TAG, "no L2CAP server (source will use GATT): " + e);
        }
    }

    /** Accepts one L2CAP connection at a time and drains its stream into the
     *  same counters the GATT path feeds, so /stats and the control-point REPORT
     *  describe an L2CAP run exactly as they do a GATT one. Loops for the next
     *  run after each peer leaves. */
    private void acceptL2cap() {
        byte[] buf = new byte[65536];
        while (l2capServer != null) {
            try (BluetoothSocket socket = l2capServer.accept()) {
                holdAndSay("L2CAP peer connected");
                InputStream in = socket.getInputStream();
                int n;
                while ((n = in.read(buf)) > 0) {
                    count(n);
                    showCounters();
                }
                say("L2CAP transfer ended");
            } catch (IOException e) {
                // accept() throws when the server socket is closed on teardown —
                // the loop's own exit, not an error to shout about.
                if (l2capServer != null) {
                    Log.w(TAG, "L2CAP accept ended: " + e);
                }
                return;
            }
        }
    }

    /** touch() + a status line, from a worker thread. */
    private void holdAndSay(String message) {
        runOnUiThread(this::touch);
        say(message);
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
                peerConnected = true;
                connectedDevice = device;
                touch();
                peer = device.getAddress();
                say("connected to " + device.getAddress());
            } else {
                peerConnected = false;
                connectedDevice = null;
                touch();
                say("disconnected — advertising again");
            }
            Log.i(TAG, "connection state " + state + " status " + status);
        }

        @Override
        public void onMtuChanged(BluetoothDevice device, int mtu) {
            SimbleActivity.this.mtu = mtu;
            Log.i(TAG, "MTU " + mtu);
            say("connected, MTU " + mtu);
        }

        @Override
        public void onCharacteristicReadRequest(
                BluetoothDevice device,
                int requestId,
                int offset,
                BluetoothGattCharacteristic characteristic) {
            if (PSM.equals(characteristic.getUuid())) {
                // The PSM as two little-endian bytes, or 0 (no L2CAP here) so the
                // source falls back to GATT rather than waiting on a channel that
                // will never open.
                byte[] value = {(byte) (l2capPsm & 0xFF), (byte) ((l2capPsm >> 8) & 0xFF)};
                server.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, 0, value);
            } else {
                server.sendResponse(device, requestId, BluetoothGatt.GATT_FAILURE, 0, null);
            }
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
                // FINISH arrives over GATT, which can beat the last of an L2CAP
                // stream still draining on the accept thread (a socket write
                // returns once buffered, not once transmitted). Wait for the
                // count to reach `expected` before reporting, so an L2CAP run is
                // not scored short by a control-point race. Keyed on *progress*,
                // not a fixed deadline: keep waiting while bytes climb, and give
                // up only after ~1.5 s of no arrivals. A GATT run is already
                // complete here, so the loop falls straight through.
                long lastSeen = -1;
                int idle = 0;
                while (bytes < expected && idle < 150) {
                    if (bytes != lastSeen) {
                        lastSeen = bytes;
                        idle = 0;
                    } else {
                        idle++;
                    }
                    try {
                        Thread.sleep(10);
                    } catch (InterruptedException ignored) {
                        break;
                    }
                }
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
        touch(); // an HTTP poll counts as activity — keep the phone discoverable
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
        out.append("\"name\":\"").append(advertisedName.replace("\"", "")).append("\",");
        out.append("\"model\":\"").append(android.os.Build.MODEL.replace("\"", "")).append("\",");
        out.append("\"advertising\":").append(advertising);
        out.append('}');
        return out.toString();
    }

    /// Zeroes the counters for the next run, and drops any central still
    /// attached from an abandoned one.
    ///
    /// The out-of-band twin of a `BEGIN` on the control point, so a run can be
    /// set up and read back without the link carrying anything but payload. It
    /// also frees a stale link: a page reloaded mid-run leaves its central
    /// connected, and the next run would collide with it — so a reset severs
    /// that connection first, and the fresh run reconnects cleanly.
    @Override
    public synchronized void reset(long expected) {
        touch(); // a reset is an HTTP request too — refresh the idle timer
        BluetoothDevice stale = connectedDevice;
        if (stale != null && server != null) {
            try {
                server.cancelConnection(stale);
                Log.i(TAG, "reset dropped a stale connection to " + stale.getAddress());
            } catch (Exception e) {
                Log.w(TAG, "could not drop stale connection: " + e);
            }
        }
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
        title.setText("SimBLE Android");
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

    /** Builds the locks once, unheld. A partial wake lock keeps the CPU (so the
     *  stats-server thread runs); a high-performance wifi lock keeps the radio
     *  out of power-save so counters stay reachable — both with the screen dark. */
    private void initLocks() {
        try {
            PowerManager pm = (PowerManager) getSystemService(Context.POWER_SERVICE);
            wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, TAG + ":sink");
            wakeLock.setReferenceCounted(false);
            WifiManager wm =
                    (WifiManager) getApplicationContext().getSystemService(Context.WIFI_SERVICE);
            wifiLock = wm.createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, TAG + ":stats");
            wifiLock.setReferenceCounted(false);
        } catch (Exception e) {
            Log.w(TAG, "could not create wake/wifi locks: " + e);
        }
    }

    /** Mark activity: hold the wake + wifi locks and re-arm the idle release, so
     *  the phone stays awake (and its stats reachable) for {@link
     *  #AWAKE_TIMEOUT_MS} after anything last talked to it. Called on launch, on
     *  every BLE connect/disconnect, and on every HTTP stats request. Safe to
     *  call off the main thread — the stats-server thread does, and Handler and
     *  the locks are both thread-safe. */
    void touch() {
        try {
            if (wakeLock != null && !wakeLock.isHeld()) {
                wakeLock.acquire();
            }
            if (wifiLock != null && !wifiLock.isHeld()) {
                wifiLock.acquire();
            }
        } catch (Exception e) {
            Log.w(TAG, "could not acquire wake/wifi locks: " + e);
        }
        if (awakeHandler != null) {
            awakeHandler.removeCallbacks(idleRelease);
            awakeHandler.postDelayed(idleRelease, AWAKE_TIMEOUT_MS);
        }
    }

    /** Fires {@link #AWAKE_TIMEOUT_MS} after the last {@link #touch()}. A peer
     *  still connected means a run is in flight — re-arm rather than cut it. */
    private final Runnable idleRelease = new Runnable() {
        @Override
        public void run() {
            if (peerConnected) {
                if (awakeHandler != null) {
                    awakeHandler.postDelayed(this, AWAKE_TIMEOUT_MS);
                }
            } else {
                releaseLocks();
            }
        }
    };

    private void releaseLocks() {
        try {
            if (wifiLock != null && wifiLock.isHeld()) {
                wifiLock.release();
            }
            if (wakeLock != null && wakeLock.isHeld()) {
                wakeLock.release();
            }
            Log.i(TAG, "released locks; sleeping until the next connection");
        } catch (Exception e) {
            Log.w(TAG, "could not release wake/wifi locks: " + e);
        }
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
        BluetoothServerSocket l2 = l2capServer;
        l2capServer = null; // signals acceptL2cap() the close is intentional
        if (l2 != null) {
            try {
                l2.close();
            } catch (IOException ignored) {
            }
        }
        if (statsServer != null) {
            statsServer.stop();
        }
        if (awakeHandler != null) {
            awakeHandler.removeCallbacksAndMessages(null);
        }
        releaseLocks();
    }
}
