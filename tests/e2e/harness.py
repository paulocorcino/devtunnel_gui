"""Harness primitives for the blackbox E2E resilience suite.

Three pieces:
  * `dt`        - thin wrapper over the `devtunnel` CLI (the product's own
                  management surface): create group, add port, anonymous access,
                  resolve public URL, delete.
  * `HostRunner`- drives the production host engine headless by launching the
                  `devtunnel_gui` binary with `DEVTUNNEL_HEADLESS_HOST=<ids>`,
                  parsing its JSON event stream and forwarding stdin commands.
  * `probe`     - client-side load/latency measurement + host-process sampling.

Nothing here is product code; it only *uses* the product from the outside.
"""

from __future__ import annotations

import json
import os
import subprocess
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field

import requests

try:
    import psutil
except ImportError:  # pragma: no cover - guarded at startup
    psutil = None

DEVTUNNEL = os.environ.get("DEVTUNNEL_BIN", "devtunnel")
# Dev Tunnels shows an HTML anti-phishing interstitial for plain browser GETs;
# this header makes the relay forward straight to the backend so we measure the
# real data path, not the warning page.
SKIP_INTERSTITIAL = {"X-Tunnel-Skip-AntiPhishing-Page": "true"}


# --------------------------------------------------------------------------- dt
def _run(args: list[str], timeout: int = 90) -> subprocess.CompletedProcess:
    return subprocess.run(
        [DEVTUNNEL, *args],
        capture_output=True,
        text=True,
        timeout=timeout,
    )


def _run_json(args: list[str], timeout: int = 90):
    cp = _run(args, timeout)
    if cp.returncode != 0:
        raise RuntimeError(f"devtunnel {' '.join(args)} failed: {cp.stderr.strip()}")
    out = cp.stdout
    start = min((i for i in (out.find("{"), out.find("[")) if i != -1), default=-1)
    if start == -1:
        raise RuntimeError(f"no JSON in `devtunnel {' '.join(args)}` output: {out[:200]}")
    return json.loads(out[start:])


def create_group(name: str, expiration: str = "1h") -> str:
    """Creates an anonymous group (tunnel) and returns its Real Tunnel ID (id.cluster)."""
    created = _run_json(["create", name, "-a", "-e", expiration, "-j"])
    full_id = created["tunnel"]["tunnelId"]
    # Mirror the GUI: ensure an anonymous ACE exists so the public URL is reachable
    # without auth (create -a should suffice, but this is idempotent and safe).
    _run(["access", "create", full_id, "--anonymous", "-j"])
    return full_id


def add_port(full_id: str, port: int, protocol: str = "http") -> None:
    cp = _run(["port", "create", full_id, "-p", str(port), "--protocol", protocol, "-j"])
    # 409 (port already exists) is fine for re-runs.
    if cp.returncode != 0 and "already exist" not in cp.stderr.lower():
        raise RuntimeError(f"add_port {full_id}:{port} failed: {cp.stderr.strip()}")


def port_uri(full_id: str, port: int) -> str | None:
    show = _run_json(["show", full_id, "-j"])
    for p in show.get("tunnel", {}).get("ports", []):
        if p.get("portNumber") == port:
            return p.get("portUri")
    return None


def host_connections(full_id: str) -> int:
    """Live host-connection count for the tunnel (0 = nothing hosting it)."""
    try:
        show = _run_json(["show", full_id, "-j"])
    except RuntimeError:
        return -1
    status = show.get("tunnel", {}).get("status", {})
    return status.get("hostConnectionCount", 0) or 0


def delete_group(full_id: str) -> None:
    _run(["delete", full_id, "-f", "-j"])


def list_ids() -> list[str]:
    data = _run_json(["list", "-j"])
    return [t.get("tunnelId") for t in data.get("tunnels", []) if t.get("tunnelId")]


# ------------------------------------------------------------------- HostRunner
@dataclass
class HostRunner:
    """Drives the headless production host engine and tracks its event stream."""

    binary: str
    ids: list[str]
    extra_env: dict | None = None
    proc: subprocess.Popen | None = field(default=None, init=False)
    events: list[dict] = field(default_factory=list, init=False)
    _state: dict[str, str] = field(default_factory=dict, init=False)
    _lock: threading.Lock = field(default_factory=threading.Lock, init=False)
    _t0: float = field(default=0.0, init=False)

    def start(self) -> "HostRunner":
        env = dict(os.environ)
        env["DEVTUNNEL_HEADLESS_HOST"] = ",".join(self.ids)
        env.setdefault("RUST_LOG", "devtunnel_gui=info,tunnels=warn")
        if self.extra_env:
            env.update(self.extra_env)
        self._t0 = time.monotonic()
        self.proc = subprocess.Popen(
            [self.binary],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
            env=env,
        )
        threading.Thread(target=self._pump, name="runner-stdout", daemon=True).start()
        return self

    def _pump(self):
        assert self.proc and self.proc.stdout
        for line in self.proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                evt = json.loads(line)
            except json.JSONDecodeError:
                continue
            evt["_recv_ms"] = int((time.monotonic() - self._t0) * 1000)
            with self._lock:
                self.events.append(evt)
                if evt.get("event") == "state":
                    self._state[evt["tunnel_id"]] = evt["state"]

    def send(self, cmd: str):
        if self.proc and self.proc.stdin:
            self.proc.stdin.write(cmd + "\n")
            self.proc.stdin.flush()

    def state(self, full_id: str) -> str | None:
        with self._lock:
            return self._state.get(full_id)

    def wait_state(self, full_id: str, target: str, timeout: float = 90.0) -> float | None:
        """Blocks until `full_id` reaches `target`; returns seconds waited or None on timeout."""
        start = time.monotonic()
        deadline = start + timeout
        while time.monotonic() < deadline:
            if self.state(full_id) == target:
                return time.monotonic() - start
            if self.proc and self.proc.poll() is not None:
                return None
            time.sleep(0.1)
        return None

    def wait_all(self, target: str, timeout: float = 120.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            with self._lock:
                if all(self._state.get(i) == target for i in self.ids):
                    return True
            time.sleep(0.2)
        return False

    @property
    def pid(self) -> int | None:
        return self.proc.pid if self.proc else None

    def quit(self, timeout: float = 8.0):
        try:
            self.send("quit")
        except (BrokenPipeError, OSError):
            pass
        if self.proc:
            try:
                self.proc.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                self.kill()

    def kill(self):
        if self.proc and self.proc.poll() is None:
            self.proc.kill()
            self.proc.wait(timeout=5)


# ------------------------------------------------------------------------ probe
@dataclass
class LoadResult:
    requests: int
    ok: int
    failed: int
    duration_s: float
    latencies_ms: list[float]
    errors: dict[str, int] = field(default_factory=dict)

    @property
    def rps(self) -> float:
        return self.ok / self.duration_s if self.duration_s else 0.0

    @property
    def error_rate(self) -> float:
        return self.failed / self.requests if self.requests else 0.0

    def pct(self, p: float) -> float:
        if not self.latencies_ms:
            return float("nan")
        s = sorted(self.latencies_ms)
        k = min(len(s) - 1, int(round(p / 100 * (len(s) - 1))))
        return s[k]


def hit(url: str, timeout: float = 10.0) -> tuple[bool, float, str]:
    """One GET; returns (ok, latency_ms, err). ok requires 2xx and the marker/echo."""
    t = time.monotonic()
    try:
        r = requests.get(url, headers=SKIP_INTERSTITIAL, timeout=timeout)
        dt = (time.monotonic() - t) * 1000
        return (r.status_code == 200, dt, "" if r.status_code == 200 else f"http{r.status_code}")
    except requests.RequestException as e:
        return (False, (time.monotonic() - t) * 1000, type(e).__name__)


def load(url: str, duration_s: float, concurrency: int = 8, timeout: float = 10.0) -> LoadResult:
    """Drives `url` for `duration_s` with `concurrency` workers; collects latency/errors."""
    lats: list[float] = []
    errors: dict[str, int] = {}
    ok = 0
    total = 0
    lock = threading.Lock()
    stop_at = time.monotonic() + duration_s
    start = time.monotonic()

    def worker():
        nonlocal ok, total
        while time.monotonic() < stop_at:
            good, dt, err = hit(url, timeout)
            with lock:
                total += 1
                lats.append(dt)
                if good:
                    ok += 1
                elif err:
                    errors[err] = errors.get(err, 0) + 1

    with ThreadPoolExecutor(max_workers=concurrency) as ex:
        for _ in range(concurrency):
            ex.submit(worker)
    dur = time.monotonic() - start
    return LoadResult(total, ok, total - ok, dur, lats, errors)


@dataclass
class ProcSamples:
    cpu_percent: list[float] = field(default_factory=list)
    rss_mb: list[float] = field(default_factory=list)

    @property
    def cpu_max(self) -> float:
        return max(self.cpu_percent, default=0.0)

    @property
    def cpu_avg(self) -> float:
        return sum(self.cpu_percent) / len(self.cpu_percent) if self.cpu_percent else 0.0

    @property
    def rss_growth_mb(self) -> float:
        return (self.rss_mb[-1] - self.rss_mb[0]) if len(self.rss_mb) >= 2 else 0.0


def sample_process(pid: int, duration_s: float, interval: float = 0.5) -> ProcSamples:
    """Samples CPU% (normalized across cores) and RSS of `pid` (and its children)."""
    out = ProcSamples()
    if psutil is None:
        return out
    try:
        proc = psutil.Process(pid)
    except psutil.NoSuchProcess:
        return out
    procs = [proc]
    try:
        procs += proc.children(recursive=True)
    except psutil.Error:
        pass
    for p in procs:
        try:
            p.cpu_percent(None)  # prime the per-process counter
        except psutil.Error:
            pass
    ncpu = psutil.cpu_count() or 1
    deadline = time.monotonic() + duration_s
    while time.monotonic() < deadline:
        time.sleep(interval)
        cpu = 0.0
        rss = 0.0
        alive = []
        for p in procs:
            try:
                cpu += p.cpu_percent(None)
                rss += p.memory_info().rss
                alive.append(p)
            except psutil.Error:
                continue
        procs = alive
        out.cpu_percent.append(cpu / ncpu)  # 100% == one full core
        out.rss_mb.append(rss / (1024 * 1024))
    return out
