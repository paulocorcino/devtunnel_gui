"""Renders the E2E result dict into `report.md`, including a findings section
that flags stability/efficiency problems against fixed thresholds and proposes
concrete product adjustments.
"""

from __future__ import annotations


def _findings(r: dict) -> list[str]:
    """Derives actionable findings from the metrics. Empty list == all green."""
    out: list[str] = []
    sc = r.get("scenarios", {})

    host = sc.get("host", {}).get("time_to_hosting_s", {})
    failed = {k: v for k, v in host.items() if v is None}
    if failed:
        out.append(
            f"**Initial host failed** for {list(failed)} (never reached Hosting). "
            f"Add a connect timeout with a clear Error state instead of an open-ended wait."
        )
    cold = sc.get("s4_auto_resume", {}).get("cold_recover_s", {})
    worst = max([v for v in list(host.values()) + list(cold.values()) if v], default=0)
    if worst > 15:
        out.append(
            f"**Slow connect/resume** (worst {worst:.0f}s to Hosting). The host path "
            f"mints two tokens (`devtunnel token … --scopes host` then `… manage:ports`) "
            f"sequentially, then `list`+`show` per group, then the relay handshake — all "
            f"before serving. Proposed adjustments: mint the two tokens concurrently, "
            f"cache `collect_ports` from the create step instead of a fresh `list`/`show`, "
            f"and emit a `Connecting` sub-progress so a 20–35 s wait doesn't look hung."
        )

    s2 = sc.get("s2_same_port", {})
    if s2 and not s2.get("all_serving"):
        bad = [k for k, g in s2.get("groups", {}).items() if not g.get("serving")]
        out.append(
            f"**Same-port multi-group not fully serving**: {bad} never returned the "
            f"backend marker. Multiple tunnels on one local port should each forward "
            f"independently (issue #18 isolates groups per runtime) — verify no "
            f"forward starvation under concurrent groups."
        )

    s3 = sc.get("s3_load", {})
    if s3:
        if s3.get("idle_cpu_avg", 0) > 10:
            out.append(
                f"**Idle CPU too high** ({s3['idle_cpu_avg']}% avg, peak "
                f"{s3.get('idle_cpu_max')}%) with no traffic — strong signal of the "
                f"relay busy-loop regression (a dropped `ports_tx` makes `run_stream` "
                f"spin). The keep-alive `_host` lifetime invariant must hold; re-check "
                f"`host_group`. A correct host parks near 0%."
            )
        if s3.get("error_rate", 0) > 0.02:
            out.append(
                f"**Elevated error rate under load** ({s3['error_rate']:.1%}). "
                f"Inspect relay backpressure / forward timeouts; consider surfacing a "
                f"degraded state and bounding per-connection concurrency."
            )
        if (s3.get("p99_ms") or 0) > 2000:
            out.append(
                f"**High tail latency** p99={s3['p99_ms']}ms under {r['meta'].get('concurrency')} "
                f"clients. Acceptable for a relay hop, but watch for growth over time."
            )
        if s3.get("rss_growth_mb", 0) > 50:
            out.append(
                f"**Memory growth under load** (+{s3['rss_growth_mb']}MB over the run) — "
                f"possible per-connection leak; sample a longer run to confirm."
            )

    s1 = sc.get("s1_reconnect", {})
    sr = s1.get("stop_rehost", {})
    if sr and not sr.get("serving_again"):
        out.append(
            "**Re-host did not resume serving** after a stop/start cycle. The engine's "
            "`run` map removes the group on Stop and should accept a fresh Host — verify "
            "the teardown fully releases the relay session before reconnect."
        )
    rd = s1.get("relay_drop", {})
    if rd.get("serving_again") is False or (rd.get("recover_to_hosting_s") is None and "skipped" not in rd):
        out.append(
            "**Did not recover from a forced relay drop**: keep-alive reconnect/backoff "
            "did not bring the group back. This is the core product promise — prioritize."
        )

    s4 = sc.get("s4_auto_resume", {})
    if s4 and not s4.get("serving_after"):
        out.append(
            "**Cold restart did not resume serving** all groups. The headless path only "
            "re-hosts what it is told; in the GUI, confirm auto-resume re-hosts the prior "
            "active set on launch."
        )

    return out


def render(r: dict) -> str:
    m = r.get("meta", {})
    sc = r.get("scenarios", {})
    L: list[str] = []
    L.append("# DevTunnel GUI — Blackbox E2E Resilience Report\n")
    L.append(f"- Started: `{m.get('started')}`")
    L.append(f"- Result: **{m.get('result')}**")
    L.append(f"- Groups: {m.get('groups')} on port {m.get('port')} · "
             f"load {m.get('load_secs')}s @ {m.get('concurrency')} clients · "
             f"elevated: {m.get('admin')}")
    L.append(f"- Binary: `{m.get('binary')}`\n")

    findings = _findings(r)
    L.append("## Findings & proposed adjustments\n")
    if not findings:
        L.append("No stability/efficiency problems crossed the thresholds. "
                 "Host parked at near-idle CPU, all groups served on the shared port, "
                 "reconnect/auto-resume recovered.\n")
    else:
        for i, f in enumerate(findings, 1):
            L.append(f"{i}. {f}")
        L.append("")

    L.append("## Host startup\n")
    L.append("| group | time to Hosting (s) |")
    L.append("|---|---|")
    for k, v in sc.get("host", {}).get("time_to_hosting_s", {}).items():
        L.append(f"| `{k}` | {v} |")
    L.append("")

    s2 = sc.get("s2_same_port", {})
    if s2:
        L.append("## S2 — multiple groups, same port\n")
        L.append(f"All serving: **{s2.get('all_serving')}**\n")
        L.append("| group | serving | p50 ms | rps | err |")
        L.append("|---|---|---|---|---|")
        for k, g in s2.get("groups", {}).items():
            L.append(f"| `{k}` | {g['serving']} | {g['p50_ms']} | {g['rps']} | {g['error_rate']} |")
        L.append("")

    s3 = sc.get("s3_load", {})
    if s3:
        L.append("## S3 — sustained load + host efficiency\n")
        L.append(f"- Requests: {s3['ok']}/{s3['requests']} ok · rps {s3['rps']} · "
                 f"error rate {s3['error_rate']}")
        L.append(f"- Latency: p50 {s3['p50_ms']} · p95 {s3['p95_ms']} · p99 {s3['p99_ms']} ms")
        L.append(f"- Host CPU: idle avg {s3['idle_cpu_avg']}% (max {s3['idle_cpu_max']}%) · "
                 f"loaded avg {s3['loaded_cpu_avg']}% (max {s3['loaded_cpu_max']}%)")
        L.append(f"- RSS growth under load: {s3['rss_growth_mb']} MB  "
                 f"_(100% CPU = one full core)_\n")

    s1 = sc.get("s1_reconnect", {})
    if s1:
        L.append("## S1 — reconnect after drop\n")
        sr = s1.get("stop_rehost", {})
        L.append(f"- stop→rehost: stopped in {sr.get('stopped_s')}s, "
                 f"re-Hosting in {sr.get('rehost_to_hosting_s')}s, "
                 f"serving again: **{sr.get('serving_again')}**")
        rd = s1.get("relay_drop", {})
        if "skipped" in rd:
            L.append(f"- forced relay drop: _skipped_ ({rd['skipped']})\n")
        else:
            L.append(f"- forced relay drop: Reconnecting observed {rd.get('reconnecting_observed')}, "
                     f"recovered in {rd.get('recover_to_hosting_s')}s, "
                     f"serving again: **{rd.get('serving_again')}**\n")

    s4 = sc.get("s4_auto_resume", {})
    if s4:
        L.append("## S4 — auto-resume after process kill\n")
        L.append(f"- Killed pid {s4.get('killed_pid')}; serving after relaunch: "
                 f"**{s4.get('serving_after')}**")
        L.append(f"- Cold recover: {s4.get('cold_recover_s')}\n")

    return "\n".join(L)
