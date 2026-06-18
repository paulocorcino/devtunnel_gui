"""Blackbox E2E resilience suite for DevTunnel GUI.

Uses the product the way a user would: creates groups (tunnels) on the same
local port, hosts them through the *production* keep-alive engine (headless),
serves a real Python backend, hammers the public URLs and runs resilience
scenarios, sampling the host process the whole time. Emits `report.md`.

Scenarios (chosen with the user):
  S2  multiple groups, same port  - N tunnels -> one backend, all serving
  S3  sustained load + latency    - throughput / p50-p95-p99 / error rate,
                                     plus idle + loaded CPU/RSS of the host
                                     (catches the relay busy-loop regression)
  S1  reconnect after drop        - stop->rehost proxy always; real relay drop
                                     via firewall block only when run elevated
  S4  auto-resume                 - kill the host process, relaunch, recover

Run:  python tests/e2e/run_e2e.py [--groups N] [--port P] [--load-secs S]
Prereqs: `devtunnel` signed in; binary built with `--features hosting`.
"""

from __future__ import annotations

import argparse
import ctypes
import os
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

import backend
import harness as H

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
BINARY = REPO / "target" / "debug" / "devtunnel_gui.exe"
PREFIX = "e2e"


def is_admin() -> bool:
    try:
        return bool(ctypes.windll.shell32.IsUserAnAdmin())
    except Exception:
        return False


def banner(msg: str):
    print(f"\n=== {msg} ===", flush=True)


def wait_url_serving(url: str, attempts: int = 30, delay: float = 1.0) -> bool:
    """Polls a public URL until it returns the backend marker (route propagation)."""
    for _ in range(attempts):
        ok, _dt, _err = H.hit(url, timeout=8)
        if ok:
            return True
        time.sleep(delay)
    return False


def fw_block(program: str) -> bool:
    r = subprocess.run(
        ["netsh", "advfirewall", "firewall", "add", "rule", "name=e2e-relay-drop",
         "dir=out", "action=block", f"program={program}", "enable=yes"],
        capture_output=True, text=True)
    return r.returncode == 0


def fw_unblock():
    subprocess.run(["netsh", "advfirewall", "firewall", "delete", "rule", "name=e2e-relay-drop"],
                   capture_output=True, text=True)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--groups", type=int, default=2, help="number of tunnels on the same port")
    ap.add_argument("--port", type=int, default=3000, help="local backend port")
    ap.add_argument("--load-secs", type=float, default=45.0, help="sustained-load duration")
    ap.add_argument("--concurrency", type=int, default=8)
    args = ap.parse_args()

    if not BINARY.exists():
        print(f"ERROR: host binary not found at {BINARY}\n"
              f"Build it first:  cargo build --features hosting", file=sys.stderr)
        return 2
    if H.psutil is None:
        print("WARNING: psutil missing — CPU/RSS sampling disabled (pip install psutil)",
              file=sys.stderr)

    admin = is_admin()
    report: dict = {"meta": {}, "scenarios": {}}
    report["meta"] = {
        "started": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "groups": args.groups, "port": args.port, "binary": str(BINARY),
        "admin": admin, "load_secs": args.load_secs, "concurrency": args.concurrency,
    }

    created: list[str] = []
    urls: dict[str, str] = {}
    runner: H.HostRunner | None = None
    httpd = None

    try:
        # ---- Setup --------------------------------------------------------
        banner("Setup: backend + groups")
        httpd = backend.serve(args.port)
        print(f"backend on 127.0.0.1:{args.port}")

        for i in range(args.groups):
            name = f"{PREFIX}-{int(time.time())}-{i}"
            fid = H.create_group(name)
            created.append(fid)
            H.add_port(fid, args.port, "http")
            print(f"  group {i}: {fid}")

        # ---- Host via production engine (headless) ------------------------
        banner("Host: launch headless production engine")
        runner = H.HostRunner(str(BINARY), created).start()
        t_host = {}
        for fid in created:
            secs = runner.wait_state(fid, "Hosting", timeout=120)
            t_host[fid] = secs
            print(f"  {fid}: Hosting after {secs:.1f}s" if secs is not None
                  else f"  {fid}: did NOT reach Hosting (state={runner.state(fid)})")

        # The public `portUri` only materializes once a host connection exists,
        # so resolve URLs now (post-Hosting). The URL is stable for the tunnel's
        # life, so cache it and reuse it across the later scenarios.
        for fid in created:
            uri = None
            for _ in range(20):
                uri = H.port_uri(fid, args.port)
                if uri:
                    break
                time.sleep(1.0)
            ident, cluster = fid.rsplit(".", 1)
            urls[fid] = uri or f"https://{ident}-{args.port}.{cluster}.devtunnels.ms/"
            print(f"  url {fid} -> {urls[fid]}")
        report["scenarios"]["host"] = {
            "time_to_hosting_s": {k: round(v, 2) if v is not None else None
                                  for k, v in t_host.items()},
        }

        # ---- S2: multiple groups, same port -------------------------------
        banner("S2: multiple groups share one port")
        s2 = {"groups": {}}
        for fid, url in urls.items():
            serving = wait_url_serving(url) if url else False
            res = H.load(url, duration_s=5, concurrency=4) if serving else None
            s2["groups"][fid] = {
                "url": url, "serving": serving,
                "p50_ms": round(res.pct(50), 1) if res else None,
                "rps": round(res.rps, 1) if res else None,
                "error_rate": round(res.error_rate, 3) if res else None,
            }
            print(f"  {fid}: serving={serving}"
                  + (f"  p50={res.pct(50):.0f}ms rps={res.rps:.1f}" if res else ""))
        s2["all_serving"] = all(g["serving"] for g in s2["groups"].values())
        report["scenarios"]["s2_same_port"] = s2

        # ---- S3: sustained load + latency + busy-loop watch ---------------
        banner("S3: sustained load + host CPU/RSS")
        target = next((u for u in urls.values() if u), None)
        # Idle baseline first: no traffic, ~8s. A correct host parks at ~0% CPU;
        # the relay busy-loop regression (issue: dropped ports_tx) pegs cores.
        idle = H.sample_process(runner.pid, duration_s=8) if runner.pid else H.ProcSamples()
        print(f"  idle CPU avg={idle.cpu_avg:.1f}% max={idle.cpu_max:.1f}% "
              f"rss={idle.rss_mb[-1] if idle.rss_mb else 0:.0f}MB")

        import threading
        load_res = {}

        def _run_load():
            load_res["r"] = H.load(target, args.load_secs, args.concurrency)

        lt = threading.Thread(target=_run_load)
        lt.start()
        loaded = H.sample_process(runner.pid, duration_s=args.load_secs) if runner.pid \
            else H.ProcSamples()
        lt.join()
        r = load_res.get("r")
        if r:
            print(f"  load: {r.ok}/{r.requests} ok  rps={r.rps:.1f}  "
                  f"p50={r.pct(50):.0f} p95={r.pct(95):.0f} p99={r.pct(99):.0f}ms  "
                  f"err={r.error_rate:.3f}")
        print(f"  loaded CPU avg={loaded.cpu_avg:.1f}% max={loaded.cpu_max:.1f}%  "
              f"RSS growth={loaded.rss_growth_mb:+.1f}MB")
        report["scenarios"]["s3_load"] = {
            "url": target,
            "requests": r.requests if r else 0, "ok": r.ok if r else 0,
            "rps": round(r.rps, 1) if r else 0, "error_rate": round(r.error_rate, 3) if r else 1,
            "p50_ms": round(r.pct(50), 1) if r else None,
            "p95_ms": round(r.pct(95), 1) if r else None,
            "p99_ms": round(r.pct(99), 1) if r else None,
            "idle_cpu_avg": round(idle.cpu_avg, 1), "idle_cpu_max": round(idle.cpu_max, 1),
            "loaded_cpu_avg": round(loaded.cpu_avg, 1), "loaded_cpu_max": round(loaded.cpu_max, 1),
            "rss_growth_mb": round(loaded.rss_growth_mb, 1),
        }

        # ---- S1: reconnect after drop -------------------------------------
        banner("S1: reconnect after drop")
        s1 = {}
        fid = created[0]
        url = urls[fid]
        # (a) Always: stop -> rehost proxy (clean teardown -> reconnect path).
        runner.send(f"stop {fid}")
        stopped = runner.wait_state(fid, "Stopped", timeout=20)
        runner.send(f"host {fid}")
        t0 = time.monotonic()
        rehosted = runner.wait_state(fid, "Hosting", timeout=90)
        reserve = wait_url_serving(url)
        s1["stop_rehost"] = {
            "stopped_s": round(stopped, 2) if stopped is not None else None,
            "rehost_to_hosting_s": round(rehosted, 2) if rehosted is not None else None,
            "serving_again": reserve,
        }
        print(f"  stop->rehost: stopped={stopped}  rehost={rehosted}  serving_again={reserve}")

        # (b) Real relay drop via firewall — only when elevated.
        if admin:
            print("  forcing real relay drop via firewall block…")
            if fw_block(str(BINARY)):
                drop_seen = False
                deadline = time.monotonic() + 30
                while time.monotonic() < deadline:
                    if runner.state(fid) == "Reconnecting":
                        drop_seen = True
                        break
                    time.sleep(0.5)
                time.sleep(5)
                fw_unblock()
                t0 = time.monotonic()
                back = runner.wait_state(fid, "Hosting", timeout=120)
                serving = wait_url_serving(url)
                s1["relay_drop"] = {
                    "reconnecting_observed": drop_seen,
                    "recover_to_hosting_s": round(back, 2) if back is not None else None,
                    "serving_again": serving,
                }
                print(f"  relay drop: reconnecting={drop_seen} recover={back} serving={serving}")
            else:
                s1["relay_drop"] = {"skipped": "firewall rule add failed"}
        else:
            s1["relay_drop"] = {"skipped": "not elevated — re-run as admin to force a real relay drop"}
            print("  real relay drop SKIPPED (needs admin). stop/rehost proxy used instead.")
        report["scenarios"]["s1_reconnect"] = s1

        # ---- S4: auto-resume (process kill + relaunch) --------------------
        banner("S4: auto-resume after host process kill")
        old_pid = runner.pid
        runner.kill()
        time.sleep(2)
        runner = H.HostRunner(str(BINARY), created).start()
        cold = {}
        for fid in created:
            secs = runner.wait_state(fid, "Hosting", timeout=120)
            cold[fid] = secs
        serving_after = all(wait_url_serving(u) for u in urls.values() if u)
        report["scenarios"]["s4_auto_resume"] = {
            "killed_pid": old_pid,
            "cold_recover_s": {k: round(v, 2) if v is not None else None for k, v in cold.items()},
            "serving_after": serving_after,
        }
        print(f"  killed pid {old_pid}; cold recover={ {k: round(v,1) if v else None for k,v in cold.items()} }  serving={serving_after}")

        report["meta"]["result"] = "completed"

    except Exception as e:
        report["meta"]["result"] = f"error: {e!r}"
        print(f"\nERROR: {e!r}", file=sys.stderr)
    finally:
        banner("Teardown")
        if admin:
            fw_unblock()
        if runner:
            runner.quit()
        for fid in created:
            try:
                H.delete_group(fid)
                print(f"  deleted {fid}")
            except Exception as e:
                print(f"  WARN delete {fid}: {e}")
        if httpd:
            httpd.shutdown()

    write_report(report)
    return 0


def write_report(report: dict):
    import json as _json

    from report_md import render
    (HERE / "report.json").write_text(_json.dumps(report, indent=2), encoding="utf-8")
    out = HERE / "report.md"
    out.write_text(render(report), encoding="utf-8")
    print(f"\nReport written to {out}")


if __name__ == "__main__":
    sys.exit(main())
