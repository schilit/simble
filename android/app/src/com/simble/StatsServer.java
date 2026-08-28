// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

package com.simble;

import android.util.Log;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.net.Inet4Address;
import java.net.InetAddress;
import java.net.NetworkInterface;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.util.Collections;
import java.util.Enumeration;

/**
 * A tiny HTTP server carrying the sink's counters off this device by a path
 * that is <em>not</em> the link under test.
 *
 * <p>Reporting over the Bluetooth connection was the original design and it
 * was wrong three ways: the report costs air time on the link being measured,
 * its arrival is what ended the measured transfer so every number included a
 * round trip of the thing under test, and a run whose whole point is a broken
 * link can never deliver it. Wi-Fi has none of those properties relative to a
 * Bluetooth measurement.
 *
 * <p>Hand-rolled on {@link ServerSocket} because the app has no dependencies
 * and this speaks exactly two routes. It is a measuring instrument on a
 * developer's own network, not a web server: no TLS, no auth, and it should
 * not be run anywhere that matters.
 */
final class StatsServer implements Runnable {

    private static final String TAG = "SimbleAndroid";
    static final int PORT = 8099;

    /** What the activity knows; read on the server thread. */
    interface Stats {
        String json();

        void reset(long expected);

        /** Advance a running publisher to generation `gen` with a `size`-byte
         *  payload, in place — no relaunch, so the L2CAP PSM is kept. Returns a
         *  small JSON acknowledgement. A no-op with an error body off the
         *  publish role. */
        default String publish(long gen, long size) {
            return "{\"error\":\"not a publisher\"}";
        }
    }

    private final Stats stats;
    private volatile boolean running = true;
    private ServerSocket socket;

    StatsServer(Stats stats) {
        this.stats = stats;
    }

    @Override
    public void run() {
        try {
            socket = new ServerSocket(PORT);
            Log.i(TAG, "stats server on " + address());
            while (running) {
                try (Socket client = socket.accept()) {
                    serve(client);
                } catch (IOException e) {
                    if (running) {
                        Log.w(TAG, "stats request failed: " + e);
                    }
                }
            }
        } catch (IOException e) {
            Log.e(TAG, "stats server could not start: " + e);
        }
    }

    private void serve(Socket client) throws IOException {
        BufferedReader in = new BufferedReader(
                new InputStreamReader(client.getInputStream(), StandardCharsets.UTF_8));
        String request = in.readLine();
        if (request == null) {
            return;
        }
        // "GET /stats HTTP/1.1" — the path is all this needs.
        String[] parts = request.split(" ");
        String path = parts.length > 1 ? parts[1] : "/";
        String query = "";
        int mark = path.indexOf('?');
        if (mark >= 0) {
            query = path.substring(mark + 1);
            path = path.substring(0, mark);
        }

        String body;
        if ("/reset".equals(path)) {
            // `?total=N` tells the sink how many bytes to expect, which is the
            // out-of-band twin of `BEGIN`'s length. Without it the sink still
            // counts, it just cannot say a run fell short by itself.
            stats.reset(longParam(query, "total"));
            body = stats.json();
        } else if ("/publish".equals(path)) {
            // `?gen=N&size=M` advances a running publisher in place.
            body = stats.publish(longParam(query, "gen"), longParam(query, "size"));
        } else if ("/stats".equals(path) || "/".equals(path)) {
            body = stats.json();
        } else {
            respond(client, "404 Not Found", "{\"error\":\"no such route\"}");
            return;
        }
        respond(client, "200 OK", body);
    }

    private void respond(Socket client, String status, String body) throws IOException {
        byte[] bytes = body.getBytes(StandardCharsets.UTF_8);
        StringBuilder head = new StringBuilder();
        head.append("HTTP/1.1 ").append(status).append("\r\n");
        head.append("Content-Type: application/json\r\n");
        head.append("Content-Length: ").append(bytes.length).append("\r\n");
        // The page fetching this is served from localhost, so every request is
        // cross-origin. Without this the browser drops the answer.
        head.append("Access-Control-Allow-Origin: *\r\n");
        head.append("Connection: close\r\n\r\n");
        OutputStream out = client.getOutputStream();
        out.write(head.toString().getBytes(StandardCharsets.UTF_8));
        out.write(bytes);
        out.flush();
    }

    void stop() {
        running = false;
        try {
            if (socket != null) {
                socket.close();
            }
        } catch (IOException ignored) {
            // Closing to interrupt accept(); the failure is not interesting.
        }
    }

    /** One `name=value` from a query string, or zero. */
    private static long longParam(String query, String name) {
        for (String pair : query.split("&")) {
            int eq = pair.indexOf('=');
            if (eq > 0 && pair.substring(0, eq).equals(name)) {
                try {
                    return Long.parseLong(pair.substring(eq + 1));
                } catch (NumberFormatException e) {
                    return 0;
                }
            }
        }
        return 0;
    }

    /** This device's address on the local network, as `host:port`. */
    static String address() {
        return hostAddress() + ":" + PORT;
    }

    private static String hostAddress() {
        try {
            for (NetworkInterface nic : Collections.list(NetworkInterface.getNetworkInterfaces())) {
                if (nic.isLoopback() || !nic.isUp()) {
                    continue;
                }
                for (InetAddress address : Collections.list(nic.getInetAddresses())) {
                    // isSiteLocalAddress() is the test that matters. Taking
                    // the first non-loopback IPv4 picked 192.0.0.4 — the
                    // 464XLAT clat interface, which is an address this device
                    // has and nothing else on the network can reach.
                    if (address instanceof Inet4Address
                            && !address.isLoopbackAddress()
                            && address.isSiteLocalAddress()) {
                        return address.getHostAddress();
                    }
                }
            }
        } catch (Exception e) {
            Log.w(TAG, "could not read a local address: " + e);
        }
        return "0.0.0.0";
    }
}
