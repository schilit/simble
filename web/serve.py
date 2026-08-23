#!/usr/bin/env python3
# Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
#
# Static server for web/, with caching turned off.
#
# `python3 -m http.server` sends no Cache-Control, so Chrome falls back to
# heuristic freshness: with only a Last-Modified to go on it may reuse a
# response for a tenth of the file's age without asking. For ES modules that
# is invisible and expensive -- an edited module keeps running its old code,
# and a `?v=` on the entry point does not reach the modules it imports. During
# one development session that cost nine wrong diagnoses: bugs declared fixed
# that were not, and fixes declared broken that were fine.
#
# So: no-store on everything. This serves a handful of files to one browser on
# localhost; there is nothing here worth caching.
#
#     python3 web/serve.py [port]

import functools
import http.server
import os
import sys


class NoCacheHandler(http.server.SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header("Cache-Control", "no-store, must-revalidate")
        self.send_header("Pragma", "no-cache")
        self.send_header("Expires", "0")
        super().end_headers()

    # .mjs is not in the stdlib map on every platform, and a module served as
    # text/plain is refused by the browser outright.
    extensions_map = {
        **http.server.SimpleHTTPRequestHandler.extensions_map,
        ".mjs": "text/javascript",
        ".js": "text/javascript",
        ".wasm": "application/wasm",
    }

    def log_message(self, fmt, *args):
        # One line per request, without the date noise.
        sys.stderr.write("  %s\n" % (fmt % args))


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8777
    root = os.path.dirname(os.path.abspath(__file__))
    handler = functools.partial(NoCacheHandler, directory=root)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", port), handler)
    print(f"serving {root} at http://127.0.0.1:{port}/  (no-store)")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nstopped")


if __name__ == "__main__":
    main()
