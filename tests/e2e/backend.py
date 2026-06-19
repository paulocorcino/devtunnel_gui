"""Local HTTP test page served through the dev tunnel under test.

This is the "produto em uso" target: a small, fast, threaded HTTP server the
harness exposes via one or more tunnels (groups) and then hammers from the
public side to measure stability, latency and throughput.

Endpoints:
  GET /            -> 200 text marker + a monotonic request counter
  GET /health      -> 200 "ok" (cheap liveness probe)
  GET /echo?bytes=N-> 200 with N bytes of payload (throughput test; capped)
  GET /stats       -> 200 JSON with per-path counters (server-side ground truth)

Run standalone:  python backend.py [port]   (default 3000)
"""

from __future__ import annotations

import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs

MARKER = "DEVTUNNEL_E2E_OK"
MAX_ECHO_BYTES = 4 * 1024 * 1024  # cap so a bad query can't OOM the box

_counts_lock = threading.Lock()
_counts: dict[str, int] = {}


def _bump(path: str) -> int:
    with _counts_lock:
        total = _counts.get("__total__", 0) + 1
        _counts["__total__"] = total
        _counts[path] = _counts.get(path, 0) + 1
        return total


class Handler(BaseHTTPRequestHandler):
    # Keep the access log quiet: the harness measures from the client side and
    # the per-request stderr spam would only obscure the host logs.
    def log_message(self, *_args):  # noqa: D401
        pass

    def _send(self, code: int, body: bytes, ctype: str = "text/plain"):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def do_GET(self):
        parsed = urlparse(self.path)
        path = parsed.path
        total = _bump(path)

        if path == "/health":
            self._send(200, b"ok")
            return
        if path == "/stats":
            with _counts_lock:
                snap = dict(_counts)
            self._send(200, json.dumps(snap).encode(), "application/json")
            return
        if path == "/echo":
            qs = parse_qs(parsed.query)
            n = int(qs.get("bytes", ["1024"])[0])
            n = max(0, min(n, MAX_ECHO_BYTES))
            self._send(200, b"x" * n)
            return

        # Default page: a stable marker + counter the harness asserts on.
        self._send(200, f"{MARKER} n={total}\n".encode())


def serve(port: int) -> ThreadingHTTPServer:
    """Starts the threaded server on 127.0.0.1:port and returns it (not blocking)."""
    httpd = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    threading.Thread(target=httpd.serve_forever, name=f"backend-{port}", daemon=True).start()
    return httpd


if __name__ == "__main__":
    p = int(sys.argv[1]) if len(sys.argv) > 1 else 3000
    server = serve(p)
    print(f"backend listening on http://127.0.0.1:{p} (Ctrl-C to stop)")
    try:
        threading.Event().wait()
    except KeyboardInterrupt:
        server.shutdown()
