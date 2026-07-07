//! Two-host probe (HITL, issue #46): determines how the Dev Tunnels relay reacts
//! to a **second** in-process host connection on a tunnel id that is already being
//! hosted. The answer (coexist / evict / reject) is the go/no-go gate for the
//! make-before-break re-mint described in #46 — it cannot be derived statically or
//! from the single-host E2E, so this throwaway binary measures it live.
//!
//! Flow:
//!   1. Mint host + manage:ports tokens (subprocess `devtunnel token`).
//!   2. Start a local HTTP server (something to forward) and bring up **host A**:
//!      connect → add_port. Confirm A is serving.
//!   3. While A is still live, mint fresh tokens and bring up **host B** on the
//!      same tunnel id: connect → add_port.
//!   4. Classify the service behavior from authoritative SDK signals:
//!        - B `connect` errors                      → REJECT
//!        - A's relay handle resolves after B joins → EVICT (new evicts old)
//!        - B's relay handle resolves after connect → EVICT (old evicts new) / soft reject
//!        - both handles stay live for the watch window → COEXIST
//!   5. A best-effort HTTP poller curls the public URL throughout and records any
//!      serving gap (informational; requires anonymous access on the port).
//!
//! Usage:
//!   cargo run --features spike --bin two_host_probe -- <tunnel-id.cluster> <port>
//!
//! The port must already exist on the tunnel (add_port treats 409 as OK). For the
//! HTTP gap measurement, enable anonymous access first:
//!   devtunnel access create <id> -p <port> --anonymous

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tunnels::connections::RelayTunnelHost;
use tunnels::contracts::TunnelPort;
use tunnels::management::{
    new_tunnel_management, Authorization, TunnelLocator, TunnelManagementClient,
};

const DEVTUNNEL: &str = "devtunnel";
const MARKER: &str = "DEVTUNNEL_PROBE_OK";

fn devtunnel_bin() -> String {
    std::env::var("DEVTUNNEL_BIN").unwrap_or_else(|_| DEVTUNNEL.to_string())
}

/// Process creation flag that suppresses the console window Windows would
/// otherwise flash for each subprocess.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Builds a `Command` with the console window suppressed on Windows.
fn command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// `devtunnel token <id> --scopes <scope> -j` → token string. One scope per call:
/// repeating `--scopes` corrupts the first value.
fn mint_token(full_id: &str, scope: &str) -> anyhow::Result<String> {
    let out = command(&devtunnel_bin())
        .args(["token", full_id, "--scopes", scope, "-j"])
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "devtunnel token ({scope}) failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    v.get("token")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("'token' field missing from devtunnel token output"))
}

/// Mints both tokens a host needs (relay `host` + `manage:ports` for add_port).
fn mint_pair(full_id: &str) -> anyhow::Result<(String, String)> {
    Ok((
        mint_token(full_id, "host")?,
        mint_token(full_id, "manage:ports")?,
    ))
}

/// Fetches the real Public URL (portUri) for the port via `devtunnel show <id> -j`.
fn fetch_port_uri(full_id: &str, port: u16) -> Option<String> {
    let out = command(&devtunnel_bin())
        .args(["show", full_id, "-j"])
        .output()
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let ports = v.get("tunnel")?.get("ports")?.as_array()?;
    for p in ports {
        if p.get("portNumber").and_then(|n| n.as_u64()) == Some(port as u64) {
            return p
                .get("portUri")
                .and_then(|u| u.as_str())
                .map(|s| s.to_string());
        }
    }
    None
}

/// Builds a host bound to `full_id` and connects it, returning the live host and
/// its relay handle (the handle future resolves when the connection drops). The
/// host MUST stay bound by the caller — dropping it tears the connection down.
async fn bring_up(
    full_id: &str,
    port: u16,
    host_token: &str,
    manage_token: String,
) -> anyhow::Result<(RelayTunnelHost, tunnels::connections::RelayHandle)> {
    let (id, cluster) = full_id
        .rsplit_once('.')
        .map(|(i, c)| (i.to_string(), c.to_string()))
        .ok_or_else(|| anyhow::anyhow!("tunnel id has no cluster: {full_id}"))?;

    let mut builder = new_tunnel_management("devtunnel-gui-probe/0.1");
    builder.authorization(Authorization::Tunnel(manage_token));
    let mgmt: TunnelManagementClient = builder.into();
    let locator = TunnelLocator::ID { cluster, id };

    let mut host = RelayTunnelHost::new(locator, mgmt);
    let handle = host.connect(host_token).await?;
    let tunnel_port = TunnelPort {
        port_number: port,
        protocol: Some("http".to_string()),
        ..Default::default()
    };
    host.add_port(&tunnel_port).await?;
    Ok((host, handle))
}

/// Minimal local HTTP server tagging responses so the poller can tell which side
/// is actually serving.
async fn run_local_server(port: u16) -> anyhow::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    log::info!("local test server listening on 127.0.0.1:{port}");
    loop {
        let (mut sock, _) = listener.accept().await?;
        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            let _ = sock.read(&mut buf).await;
            let body = format!("{MARKER}\n");
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.flush().await;
        });
    }
}

/// Best-effort serving check: curl the public URL (skipping the anti-phishing
/// interstitial) and report whether our marker came back. Returns `None` when
/// curl itself could not run.
fn curl_serves(uri: &str) -> Option<bool> {
    let out = command("curl")
        .args([
            "-s",
            "-m",
            "3",
            "-H",
            "X-Tunnel-Skip-AntiPhishing-Page: true",
            uri,
        ])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).contains(MARKER))
}

/// Polls the public URL until `stop` is set, printing only on serving-state
/// transitions (with elapsed-since-start timestamps) so any gap is visible.
async fn poll_serving(uri: String, start: Instant, stop: Arc<AtomicBool>) {
    let mut last: Option<bool> = None;
    let mut probed_at_all = false;
    while !stop.load(Ordering::Relaxed) {
        let serving = curl_serves(&uri);
        match serving {
            Some(s) => {
                probed_at_all = true;
                if last != Some(s) {
                    let t = start.elapsed().as_millis();
                    println!(
                        "  [poll +{t:>6}ms] serving = {}",
                        if s { "YES" } else { "no" }
                    );
                    last = Some(s);
                }
            }
            None => {
                if !probed_at_all {
                    println!("  [poll] curl unavailable — skipping HTTP gap measurement");
                    return;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let mut args = std::env::args().skip(1);
    let full_id = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: two_host_probe <tunnel-id.cluster> <port>"))?;
    let port: u16 = args.next().and_then(|s| s.parse().ok()).unwrap_or(3000);

    println!("== two-host relay probe (issue #46) ==");
    println!("tunnel id : {full_id}");
    println!("port      : {port}\n");

    // Local server to forward.
    tokio::spawn(async move {
        if let Err(e) = run_local_server(port).await {
            log::error!("local server failed: {e}");
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;

    let start = Instant::now();

    // ---- Host A -------------------------------------------------------------
    println!("[A] minting tokens + connecting…");
    let (a_host_tok, a_manage_tok) = mint_pair(&full_id)?;
    let (_host_a, handle_a) = bring_up(&full_id, port, &a_host_tok, a_manage_tok).await?;
    println!("[A] connected + port forwarded ✓");

    // Start the HTTP gap poller (informational).
    let stop = Arc::new(AtomicBool::new(false));
    let poller = {
        let uri = fetch_port_uri(&full_id, port);
        match uri {
            Some(u) => {
                println!("[A] public URL: {u}");
                let stop = stop.clone();
                Some(tokio::spawn(poll_serving(u, start, stop)))
            }
            None => {
                println!("[A] could not resolve public URL — HTTP gap measurement skipped");
                None
            }
        }
    };

    // Let A settle and confirm it is serving before the second host joins.
    println!("[A] holding 4s to confirm steady-state serving…");
    tokio::time::sleep(Duration::from_secs(4)).await;

    tokio::pin!(handle_a);
    // Sanity: A must still be connected at this point.
    if let Some(r) = poll_handle(&mut handle_a) {
        println!("\n‼ A dropped before B even started: {r:?}");
        finish(stop, poller).await;
        println!("\nVERDICT: INCONCLUSIVE (host A unstable on its own)");
        return Ok(());
    }

    // ---- Host B (the experiment) -------------------------------------------
    println!("\n[B] minting fresh tokens + connecting a SECOND host to the same id…");
    let t_b_start = Instant::now();
    let (b_host_tok, b_manage_tok) = mint_pair(&full_id)?;
    let b = bring_up(&full_id, port, &b_host_tok, b_manage_tok).await;

    let (_host_b, handle_b) = match b {
        Err(e) => {
            let msg = e.to_string().to_lowercase();
            // A transient transport failure (Windows WSANO_DATA / DNS, websocket
            // IO, EOF) is NOT a service decision — the same flaky lookup makes
            // host A retry on connect too. Only a service-level refusal (a status
            // code, "forbidden", "conflict", "already hosted") is a real reject.
            let transient = msg.contains("11001")
                || msg.contains("host não é conhecido")
                || msg.contains("host not known")
                || msg.contains("io error")
                || msg.contains("eof")
                || msg.contains("timed out");
            let rejected = msg.contains("403")
                || msg.contains("409")
                || msg.contains("forbidden")
                || msg.contains("conflict")
                || msg.contains("already");
            println!("[B] connect FAILED after {:?}: {e}", t_b_start.elapsed());
            let a_after = watch_for(&mut handle_a, Duration::from_secs(5)).await;
            finish(stop, poller).await;
            if rejected && !transient {
                println!("\nVERDICT: REJECT — the relay refuses a second host on one tunnel id.");
                println!(
                    "  → make-before-break (connect-new-then-drop-old) is impossible as framed."
                );
                println!("  → fall back to minimizing the break window on re-mint.");
            } else {
                println!(
                    "\nVERDICT: INCONCLUSIVE — B failed on a transient transport error, not a"
                );
                println!(
                    "  service rejection. Re-run; this is the same flaky DNS that retries on A."
                );
            }
            match a_after {
                Some(r) => println!("  note: host A also dropped during B's attempt: {r:?}"),
                None => println!("  note: host A kept serving through B's attempt."),
            }
            return Ok(());
        }
        Ok(pair) => {
            println!(
                "[B] connected + port forwarded ✓ ({:?})",
                t_b_start.elapsed()
            );
            pair
        }
    };

    // Both connect calls succeeded. Watch both handles for the verdict window.
    // Measure any eviction delay from the moment B finished connecting (the
    // handover gap that #46 cares about), not from when B started minting.
    println!("\n[probe] both hosts connected — watching 15s for eviction…");
    tokio::pin!(handle_b);
    let window = Duration::from_secs(15);
    let watch_start = Instant::now();

    let verdict = loop {
        if watch_start.elapsed() >= window {
            break Verdict::Coexist;
        }
        tokio::select! {
            r = &mut handle_a => break Verdict::EvictOld(format!("{r:?}"), watch_start.elapsed()),
            r = &mut handle_b => break Verdict::EvictNew(format!("{r:?}"), watch_start.elapsed()),
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
    };

    finish(stop, poller).await;

    println!("\n──────────────────────────────────────────────");
    match verdict {
        Verdict::Coexist => {
            println!("VERDICT: COEXIST — two hosts served the same tunnel id for 15s.");
            println!("  → make-before-break is CLEAN: connect new, verify serving, drop old.");
            println!("  → GO on the #46 overlap rewrite of the re-mint path.");
        }
        Verdict::EvictOld(r, dt) => {
            println!("VERDICT: EVICT (new evicts old) — host A dropped {dt:?} after B finished connecting.");
            println!("  detail: A handle resolved with {r}");
            println!("  → make-before-break still works, but with a handover window.");
            println!("  → GO, but measure the gap from the poll trace above before committing.");
        }
        Verdict::EvictNew(r, dt) => {
            println!(
                "VERDICT: EVICT (old evicts new) — host B dropped {dt:?} after finishing connect."
            );
            println!("  detail: B handle resolved with {r}");
            println!("  → the service keeps the incumbent; a second host cannot take over live.");
            println!(
                "  → NO-GO on make-before-break as framed; minimize the break window instead."
            );
        }
    }
    println!("──────────────────────────────────────────────");
    Ok(())
}

enum Verdict {
    Coexist,
    EvictOld(String, Duration),
    EvictNew(String, Duration),
}

/// Non-blocking peek at a pinned relay handle: `Some(debug)` if it has resolved
/// (connection dropped), `None` if still live.
fn poll_handle(
    handle: &mut std::pin::Pin<&mut tunnels::connections::RelayHandle>,
) -> Option<String> {
    use std::future::Future;
    use std::task::{Context, Poll};
    let waker = futures_noop_waker();
    let mut cx = Context::from_waker(&waker);
    match handle.as_mut().poll(&mut cx) {
        Poll::Ready(r) => Some(format!("{r:?}")),
        Poll::Pending => None,
    }
}

/// Awaits a handle for up to `dur`; returns `Some(debug)` if it resolved within
/// the window, `None` if it stayed live.
async fn watch_for(
    handle: &mut std::pin::Pin<&mut tunnels::connections::RelayHandle>,
    dur: Duration,
) -> Option<String> {
    tokio::select! {
        r = handle.as_mut() => Some(format!("{r:?}")),
        _ = tokio::time::sleep(dur) => None,
    }
}

/// Stops the poller and awaits its task so the final trace lines flush before the
/// verdict prints.
async fn finish(stop: Arc<AtomicBool>, poller: Option<tokio::task::JoinHandle<()>>) {
    stop.store(true, Ordering::Relaxed);
    if let Some(p) = poller {
        let _ = p.await;
    }
}

/// A no-op waker so we can poll a future once without a runtime scheduling it.
fn futures_noop_waker() -> std::task::Waker {
    use std::task::{RawWaker, RawWakerVTable, Waker};
    fn clone(_: *const ()) -> RawWaker {
        RawWaker::new(std::ptr::null(), &VTABLE)
    }
    fn noop(_: *const ()) {}
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
}
