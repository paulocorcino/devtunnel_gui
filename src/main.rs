// Hide the console window on Windows in release builds (tray app).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod autostart;
mod devtunnel;
mod headless;
mod host;
mod icon_render;
#[cfg(windows)]
mod install;
mod locale;
mod logbuf;
mod metrics_store;
mod model;
#[cfg(feature = "hosting")]
mod probe;
mod state;
mod update;
mod view;

slint::include_modules!();

use fluent_bundle::FluentArgs;
use locale::Locale;
use slint::{CloseRequestResponse, ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::time::Duration;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu},
    Icon, TrayIconBuilder, TrayIconEvent,
};

/// A data-load result pumped from a background thread to the UI thread:
/// `(placeholder id to remove, hidden-delete key to clear, fetched rows)`.
type LoadMsg = (
    Option<u64>,
    Option<(String, Option<i32>)>,
    anyhow::Result<Vec<devtunnel::Row>>,
);

/// What a tray menu item does when clicked. The `MenuId -> Action` map is
/// rebuilt on every data load because per-port items depend on live data.
enum Action {
    Show,
    Quit,
    Copy(String),
    Open(String),
}

use view::Placeholder;

/// A deletion awaiting user confirmation. `port == None` means delete the whole group.
struct PendingDelete {
    tunnel_id: String,
    port: Option<i32>,
}

/// UI-thread state derived from host/probe events. Persists across reloads so a
/// fresh `fetch_rows` keeps the latest health/host status per row.
#[derive(Default)]
struct LiveState {
    /// Latest data load (Real Tunnel ID order preserved), used to rebuild rows
    /// when a host/probe event changes derived status without a refetch.
    rows: Vec<devtunnel::Row>,
    /// Per-port health status id ("ok"/"warn"/"down"), keyed by (tunnel_id, port).
    probe: HashMap<(String, i32), String>,
    /// Per-group host state id ("host"/"hosting"/"" ...), keyed by tunnel_id.
    host: HashMap<String, String>,
    /// Optimistic placeholder rows for in-flight create operations.
    placeholders: Vec<Placeholder>,
    /// Monotonic counter for placeholder ids.
    next_placeholder_id: u64,
    /// Port whose detail panel is expanded, keyed by (tunnel_id, port).
    /// `None` = collapsed; the metrics poll is a no-op while collapsed.
    detail: Option<(String, i32)>,
    /// True while a `devtunnel show -j` metrics fetch is in flight. Guards the
    /// poll timer from spawning overlapping subprocesses (the CLI is a .NET tool
    /// and slow to start, so unguarded ticks piled up under any latency). Dormant
    /// in v0.1.0 — the poll timer that reads it is disabled for stability.
    #[allow(dead_code)]
    metrics_inflight: bool,
    /// Optimistic hidden-delete keys for in-flight delete operations.
    /// `(tunnel_id, None)` hides the whole group; `(tunnel_id, Some(port))` hides one port.
    hidden: HashSet<(String, Option<i32>)>,
}

impl LiveState {
    fn push_placeholder(&mut self, group: String, port: i32, protocol: String) -> u64 {
        let id = self.next_placeholder_id;
        self.next_placeholder_id += 1;
        self.placeholders.push(Placeholder {
            id,
            group,
            port,
            protocol,
        });
        id
    }

    fn remove_placeholder(&mut self, id: u64) {
        self.placeholders.retain(|p| p.id != id);
    }

    fn hide_delete(&mut self, tunnel_id: String, port: Option<i32>) -> (String, Option<i32>) {
        let key = (tunnel_id, port);
        self.hidden.insert(key.clone());
        key
    }

    fn unhide_delete(&mut self, key: &(String, Option<i32>)) {
        self.hidden.remove(key);
    }
}

/// Maps a [`host::HostState`] to the stored host-state id, or `None` when the
/// group is no longer hosted (Stopped / Idle / Error -> clear).
fn map_host_state(hs: &host::HostState) -> Option<&'static str> {
    match hs {
        host::HostState::Connecting | host::HostState::Reconnecting => Some("host"),
        host::HostState::Hosting => Some("hosting"),
        host::HostState::Idle | host::HostState::Stopped | host::HostState::Error(_) => None,
    }
}

/// Maps a [`probe::ProbeState`] to the existing status id used by the UI/theme.
#[cfg(feature = "hosting")]
fn map_probe_state(ps: &probe::ProbeState) -> &'static str {
    match ps {
        probe::ProbeState::Operational => "ok",
        probe::ProbeState::ServiceDown => "warn",
        probe::ProbeState::Down => "down",
    }
}

/// Builds probe targets for every port of a currently-hosting group that has a URL.
#[cfg(feature = "hosting")]
fn hosting_targets(state: &LiveState) -> Vec<probe::ProbeTarget> {
    state
        .rows
        .iter()
        .filter(|r| !r.url.is_empty())
        .filter(|r| {
            matches!(
                state.host.get(&r.tunnel_id).map(String::as_str),
                Some("hosting")
            )
        })
        .map(|r| probe::ProbeTarget {
            tunnel_id: r.tunnel_id.clone(),
            port: r.port,
            url: r.url.clone(),
        })
        .collect()
}

fn main() -> anyhow::Result<()> {
    // Install the capturing logger in every build: it tees records to stderr
    // (what env_logger used to print in the hosting build).
    // Stability (v0.1.0): the tunnels SDK logs per-connection events at info on
    // the relay's own threads; routing that volume through a synchronous logger
    // stalled the host under traffic. Default the SDK to `warn` (only real
    // problems) and our crate to `info`. Override with RUST_LOG when debugging
    // (e.g. `RUST_LOG=devtunnel_gui=debug,tunnels=info`).
    let _ = logbuf::CaptureLogger::from_env("devtunnel_gui=info,tunnels=warn").install();

    // Headless host runner: a diagnostic/test entrypoint (no GUI, no tray) for
    // the blackbox E2E resilience harness in `tests/e2e/`. When
    // `DEVTUNNEL_HEADLESS_HOST=<id>[,<id>…]` is set we drive the production host
    // engine directly and stream every `HostEvent` as JSON on stdout, returning
    // before any UI is built. A real engine only exists with `--features hosting`.
    if let Ok(ids) = std::env::var("DEVTUNNEL_HEADLESS_HOST") {
        return headless::run(&ids);
    }

    // winit registers the window class with a null icon, so the title bar and
    // taskbar would show the generic default. Install the winit backend with a
    // hook that sets our brand icon on every window at creation time. (The
    // embedded executable resource icon only covers the file in Explorer; Slint's
    // own `Window.icon` property is not wired to winit.)
    install_window_icon();

    let app = AppWindow::new()?;

    let loc = Rc::new(Locale::load(&locale::system_locale()));
    apply_strings(&app, &loc);
    // App version for the About panel (not localized). Derived from git tags at
    // build time (see build.rs); falls back to the Cargo version off a checkout.
    app.set_app_version(env!("GIT_VERSION").into());

    // Tray menu action map (rebuilt on every refresh). Lives only on the UI
    // thread — `Rc`/`RefCell` is enough (nothing crosses thread boundaries).
    let actions: Rc<RefCell<HashMap<MenuId, Action>>> = Rc::new(RefCell::new(HashMap::new()));

    // Initial menu (no ports yet) + tray. Must stay alive: drop = tray disappears.
    let menu = build_tray_menu(&[], &mut actions.borrow_mut(), &loc);
    let tray = Rc::new(
        TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(loc.t("app-title"))
            .with_icon(build_icon())
            .build()?,
    );

    // ---- Closing the window hides it to the tray (does not quit) ----
    {
        let weak = app.as_weak();
        app.window().on_close_requested(move || {
            if let Some(a) = weak.upgrade() {
                let _ = a.hide();
            }
            CloseRequestResponse::HideWindow
        });
    }

    // ---- Loading: background thread fetches data and sends it via `Sender`.
    // The UI thread (timer) applies the result to the window and rebuilds the
    // tray menu, keeping non-`Send` objects (tray, app) off the background thread.
    // The `Option<u64>` carries the placeholder id to remove on completion (None for plain loads).
    let (tx, rx) = std::sync::mpsc::channel::<LoadMsg>();

    // Preflight results (startup probe + after sign-in) pumped to the UI thread.
    let (pf_tx, pf_rx) = std::sync::mpsc::channel::<devtunnel::Preflight>();

    // CLI install (winget) outcomes pumped to the UI thread so the banner can
    // clear "Installing…" and surface a clear success / failure / elevation result.
    let (install_tx, install_rx) = std::sync::mpsc::channel::<devtunnel::InstallOutcome>();

    // Selected-port metrics results, tagged with (tunnel_id, port) so the pump
    // can drop results that arrive after the selection changed.
    let (metrics_tx, metrics_rx) =
        std::sync::mpsc::channel::<(String, i32, anyhow::Result<devtunnel::PortMetrics>)>();

    // Derived live state (probe/host status per row), shared on the UI thread.
    let state: Rc<RefCell<LiveState>> = Rc::new(RefCell::new(LiveState::default()));

    // Persistent app state (auto-host set + settings), loaded once on startup and
    // saved best-effort whenever the auto-host set or settings change.
    let app_state: Rc<RefCell<state::AppState>> = Rc::new(RefCell::new(state::AppState::load()));

    // ---- Host + probe engines ----
    // The host engine starts in every build (it is a no-op without `hosting`);
    // the probe engine only exists in the `hosting` build. Both communicate with
    // the UI thread via mpsc channels drained in the Timer pump below.
    let (host_evt_tx, host_evt_rx) = std::sync::mpsc::channel::<host::HostEvent>();
    let tunnel_host = host::spawn(host_evt_tx);

    // ---- Update checker ----
    // A background thread polls GitHub Releases (startup + every 24 h) and pumps
    // an UpdateInfo when a newer version than this build is published; the UI
    // pump then shows the in-app update banner.
    let (update_tx, update_rx) = std::sync::mpsc::channel::<update::UpdateInfo>();
    update::spawn(update_tx);

    #[cfg(feature = "hosting")]
    let (probe_evt_rx, probe_cmd_tx) = {
        let (probe_evt_tx, probe_evt_rx) = std::sync::mpsc::channel::<probe::ProbeEvent>();
        let probe_cmd_tx = probe::spawn(probe_evt_tx);
        // Apply the persisted steady-state cadence (conservative 60 s default;
        // clamped to >= 1 s so a hand-edited 0 cannot busy-loop the probe).
        let secs = app_state.borrow().settings.probe_interval_secs.max(1);
        let _ = probe_cmd_tx.send(probe::ProbeCommand::SetInterval(Duration::from_secs(secs)));
        (probe_evt_rx, probe_cmd_tx)
    };

    // The default (non-hosting) build shows the toggle disabled.
    #[cfg(feature = "hosting")]
    app.set_hosting_enabled(true);

    // ---- Settings: auto-start (start with Windows) ----
    // The registry is the source of truth for the checkbox (the user may have
    // removed the Run entry externally); the persisted setting follows it.
    #[cfg(windows)]
    {
        let enabled = autostart::is_enabled();
        app.set_auto_start_enabled(enabled);
        app_state.borrow_mut().settings.auto_start = enabled;
        refresh_requirements(&app);
    }
    {
        let weak = app.as_weak();
        let app_state = app_state.clone();
        app.on_auto_start_changed(move |enabled| {
            #[cfg(windows)]
            {
                if enabled {
                    // Registers the Run entry at the executable's current
                    // location; it does not relocate the binary or relaunch.
                    enable_auto_start(&app_state);
                } else if let Err(e) = autostart::disable() {
                    log::warn!("autostart: failed to disable: {e}");
                }
                // Re-read the registry so the checkbox reflects the actual state
                // (reverts the optimistic toggle if the write failed).
                let actual = autostart::is_enabled();
                if let Some(a) = weak.upgrade() {
                    a.set_auto_start_enabled(actual);
                    refresh_requirements(&a);
                }
                let mut st = app_state.borrow_mut();
                st.settings.auto_start = actual;
                st.save();
            }
            #[cfg(not(windows))]
            {
                let mut st = app_state.borrow_mut();
                st.settings.auto_start = enabled;
                st.save();
            }
        });
    }
    // ---- Settings: install the Dev Tunnels CLI on demand (winget + fallback) ----
    // The callback runs on the UI thread: flag "installing" so the banner shows
    // "Installing…" + disables the button, then run winget off-thread and pump the
    // classified outcome back to the UI pump (which clears the flag and surfaces it).
    {
        let weak = app.as_weak();
        let install_tx = install_tx.clone();
        app.on_install_cli(move || {
            if let Some(a) = weak.upgrade() {
                a.set_installing(true);
            }
            let install_tx = install_tx.clone();
            std::thread::spawn(move || {
                let _ = install_tx.send(devtunnel::install_cli());
            });
        });
    }
    // ---- Update banner: open the release page in the browser ----
    {
        let weak = app.as_weak();
        app.on_open_update_url(move || {
            if let Some(a) = weak.upgrade() {
                open_browser(&a.get_update_url());
            }
        });
    }
    // ---- Update banner: ignore this version (persist + hide the banner) ----
    {
        let weak = app.as_weak();
        let app_state = app_state.clone();
        app.on_ignore_update(move || {
            if let Some(a) = weak.upgrade() {
                let mut st = app_state.borrow_mut();
                st.settings.skipped_update = a.get_update_version().to_string();
                st.save();
                a.set_update_available(false);
            }
        });
    }
    // ---- Settings: probe interval + default expiration (issue #6) ----
    // Seed the dialog properties from the persisted settings; the handlers
    // persist edits and (hosting build) re-target the live probe immediately.
    {
        let st = app_state.borrow();
        app.set_probe_interval_secs(st.settings.probe_interval_secs as i32);
        app.set_default_expiration_days(expiration_days(&st.settings.default_expiration));
        app.set_log_level(st.settings.log_level.clone().into());
    }

    // ---- Dark mode (persisted; falls back to the Windows app theme) ----
    // The top-bar toggle flips Theme.dark and fires theme-changed; we persist
    // the explicit choice so it survives restarts (issue: gear → sun/moon).
    {
        let dark = app_state
            .borrow()
            .settings
            .dark
            .unwrap_or_else(system_prefers_dark);
        app.global::<Theme>().set_dark(dark);
    }
    {
        let app_state = app_state.clone();
        app.on_theme_changed(move |dark| {
            let mut st = app_state.borrow_mut();
            st.settings.dark = Some(dark);
            st.save();
        });
    }
    {
        let app_state = app_state.clone();
        #[cfg(feature = "hosting")]
        let probe_cmd_tx = probe_cmd_tx.clone();
        app.on_probe_interval_changed(move |secs| {
            let secs = (secs.max(1)) as u64;
            let mut st = app_state.borrow_mut();
            st.settings.probe_interval_secs = secs;
            st.save();
            // Re-target the live probe without a restart.
            #[cfg(feature = "hosting")]
            let _ = probe_cmd_tx.send(probe::ProbeCommand::SetInterval(Duration::from_secs(secs)));
        });
    }
    {
        let app_state = app_state.clone();
        app.on_default_expiration_changed(move |days| {
            let mut st = app_state.borrow_mut();
            // The UI is days-only and pre-clamped; format the canonical CLI
            // string ("Nd") that renewal and new-group creation consume.
            st.settings.default_expiration = expiration_string(days);
            st.save();
        });
    }
    {
        // Persist the chosen Logs-tab severity and re-apply it to the open panel
        // immediately so the filter takes effect without reopening the detail.
        let app_state = app_state.clone();
        let weak = app.as_weak();
        app.on_log_level_changed(move |level| {
            {
                let mut st = app_state.borrow_mut();
                st.settings.log_level = level.to_string();
                st.save();
            }
            if let Some(a) = weak.upgrade() {
                refresh_logs(&a, &app_state);
            }
        });
    }
    {
        // Re-sync the toggle from the registry each time the Settings dialog
        // opens, in case the Run entry changed out of band, then show it.
        let weak = app.as_weak();
        let app_state = app_state.clone();
        app.on_open_settings(move || {
            if let Some(a) = weak.upgrade() {
                #[cfg(windows)]
                {
                    a.set_auto_start_enabled(autostart::is_enabled());
                    // Recompute the requirements checklist each time it opens.
                    refresh_requirements(&a);
                }
                // Re-seed the editable fields from the persisted settings so the
                // dialog always opens showing the current values.
                let st = app_state.borrow();
                a.set_probe_interval_secs(st.settings.probe_interval_secs as i32);
                a.set_default_expiration_days(expiration_days(&st.settings.default_expiration));
                a.set_log_level(st.settings.log_level.clone().into());
                a.set_show_settings(true);
            }
            // Fetch the logged-in account off the UI thread (subprocess call) and
            // fill the "Signed in as …" label when it lands. Best-effort: an empty
            // result leaves the row showing the plain "Signed in" label.
            let weak = weak.clone();
            std::thread::spawn(move || {
                let account = devtunnel::current_username().unwrap_or_default();
                let _ = weak.upgrade_in_event_loop(move |a| {
                    a.set_req_login_account(account.into());
                });
            });
        });
    }
    // ---- Settings: uninstall (remove shortcut + auto-start, then self-delete) ----
    // The UI shows a confirmation dialog before invoking this (uninstall-confirmed).
    #[cfg(windows)]
    app.on_uninstall_confirmed(perform_uninstall);

    // ---- UI callbacks ----
    app.on_copy_url(|url| copy(&url));
    app.on_open_url(|url| open_browser(&url));
    {
        let weak = app.as_weak();
        let tx = tx.clone();
        let loc = loc.clone();
        app.on_refresh(move || load_async(&weak, &tx, &loc));
    }

    // ---- Sign in (re-login banner) ----
    // Runs `devtunnel user login` on a background thread (interactive — opens
    // the browser), then re-runs preflight; the pump applies the new state and
    // reloads when it is Ok. On failure preflight re-detects LoggedOut, so the
    // banner simply stays up.
    {
        let weak = app.as_weak();
        let pf_tx = pf_tx.clone();
        let loc = loc.clone();
        app.on_sign_in(move || {
            if let Some(a) = weak.upgrade() {
                a.set_status(loc.t("status-signing-in").into());
            }
            let pf_tx = pf_tx.clone();
            let lang = locale::system_locale();
            std::thread::spawn(move || {
                let loc = Locale::load(&lang);
                let _ = devtunnel::user_login(&loc);
                let _ = pf_tx.send(devtunnel::preflight());
            });
        });
    }

    // Pending deletion (set when a delete is requested, consumed on confirm).
    let pending: Rc<RefCell<Option<PendingDelete>>> = Rc::new(RefCell::new(None));

    // One-shot flag: re-host the persisted auto-host groups after the next
    // successful load (set at startup and again after a successful re-login).
    let auto_resume_pending: Rc<Cell<bool>> = Rc::new(Cell::new(true));

    // One-shot flag (issue #6): renew the auto-host groups (expiration window +
    // anonymous ACE) after the first successful load; the 12h timer below keeps
    // renewing while the app runs.
    let renew_pending: Rc<Cell<bool>> = Rc::new(Cell::new(true));

    // ---- Create group ----
    {
        let weak = app.as_weak();
        let tx = tx.clone();
        let loc = loc.clone();
        let state = state.clone();
        let tray = tray.clone();
        let actions = actions.clone();
        app.on_create_group(
            move |name, expiration_days, anonymous, description, keep_headers, request_timeout| {
                let opts = devtunnel::CreateGroupOpts {
                    name: name.to_string(),
                    expiration: expiration_string(expiration_days),
                    anonymous,
                    description: description.to_string(),
                    keep_headers,
                    request_timeout: request_timeout.to_string(),
                };
                let placeholder_id =
                    state
                        .borrow_mut()
                        .push_placeholder(opts.name.clone(), 0, String::new());
                if let Some(a) = weak.upgrade() {
                    rebuild_rows(&a, &tray, &actions, &state, &loc);
                }
                run_op_async(
                    &weak,
                    &tx,
                    "status-creating-group",
                    &loc,
                    Some(placeholder_id),
                    None,
                    move |loc| devtunnel::create_group(&opts, loc).map(|_| ()),
                );
            },
        );
    }

    // ---- Add port (creates the group inline when "+ New group…" was chosen) ----
    {
        let weak = app.as_weak();
        let tx = tx.clone();
        let loc = loc.clone();
        let state = state.clone();
        let tray = tray.clone();
        let actions = actions.clone();
        app.on_add_port(
            move |group_id,
                  new_name,
                  port,
                  protocol,
                  description,
                  keep_headers,
                  request_timeout| {
                let group_id = group_id.to_string();
                let new_name = new_name.to_string();
                let port_num: i32 = port.trim().parse().unwrap_or(0);
                let opts = devtunnel::CreatePortOpts {
                    port: port_num,
                    protocol: protocol.to_string(),
                    description: description.to_string(),
                    keep_headers,
                    request_timeout: request_timeout.to_string(),
                };
                // For "+ New group…" the group name comes from new_name; otherwise
                // look it up from state.rows so the placeholder shows a friendly name.
                let placeholder_group = if group_id.is_empty() {
                    new_name.clone()
                } else {
                    state
                        .borrow()
                        .rows
                        .iter()
                        .find(|r| r.tunnel_id == group_id)
                        .map(|r| r.group.clone())
                        .unwrap_or_else(|| group_id.clone())
                };
                let placeholder_id = state.borrow_mut().push_placeholder(
                    placeholder_group,
                    port_num,
                    opts.protocol.clone(),
                );
                if let Some(a) = weak.upgrade() {
                    rebuild_rows(&a, &tray, &actions, &state, &loc);
                }
                run_op_async(
                    &weak,
                    &tx,
                    "status-adding-port",
                    &loc,
                    Some(placeholder_id),
                    None,
                    move |loc| {
                        let tunnel_id = if group_id.is_empty() {
                            devtunnel::create_group(
                                &devtunnel::CreateGroupOpts {
                                    name: new_name,
                                    expiration: "30d".to_string(),
                                    anonymous: true,
                                    description: String::new(),
                                    keep_headers: false,
                                    request_timeout: String::new(),
                                },
                                loc,
                            )?
                        } else {
                            group_id
                        };
                        devtunnel::create_port(&tunnel_id, &opts, loc)
                    },
                );
            },
        );
    }

    // ---- Request delete (opens the confirmation dialog) ----
    {
        let weak = app.as_weak();
        let loc = loc.clone();
        let pending = pending.clone();
        app.on_request_delete_group(move |name, tunnel_id| {
            if let Some(a) = weak.upgrade() {
                let mut args = FluentArgs::new();
                args.set("name", name.to_string());
                a.global::<Strings>()
                    .set_confirm_message(loc.t_args("confirm-delete-group", &args).into());
                a.set_show_confirm(true);
            }
            *pending.borrow_mut() = Some(PendingDelete {
                tunnel_id: tunnel_id.to_string(),
                port: None,
            });
        });
    }
    {
        let weak = app.as_weak();
        let loc = loc.clone();
        let pending = pending.clone();
        app.on_request_delete_port(move |group, tunnel_id, port| {
            if let Some(a) = weak.upgrade() {
                let mut args = FluentArgs::new();
                args.set("group", group.to_string());
                args.set("port", port as i64);
                a.global::<Strings>()
                    .set_confirm_message(loc.t_args("confirm-delete-port", &args).into());
                a.set_show_confirm(true);
            }
            *pending.borrow_mut() = Some(PendingDelete {
                tunnel_id: tunnel_id.to_string(),
                port: Some(port),
            });
        });
    }

    // ---- Confirm accept (runs the pending deletion) ----
    {
        let weak = app.as_weak();
        let tx = tx.clone();
        let loc = loc.clone();
        let pending = pending.clone();
        let state = state.clone();
        let tray = tray.clone();
        let actions = actions.clone();
        app.on_confirm_accept(move || {
            let Some(p) = pending.borrow_mut().take() else {
                return;
            };
            let tunnel_id = p.tunnel_id;
            let port = p.port;
            let hidden_key = state.borrow_mut().hide_delete(tunnel_id.clone(), port);
            if let Some(a) = weak.upgrade() {
                rebuild_rows(&a, &tray, &actions, &state, &loc);
            }
            run_op_async(
                &weak,
                &tx,
                "status-deleting",
                &loc,
                None,
                Some(hidden_key),
                move |loc| match port {
                    Some(pn) => devtunnel::delete_port(&tunnel_id, pn, loc),
                    None => devtunnel::delete_group(&tunnel_id, loc),
                },
            );
        });
    }

    // ---- Host / Stop toggle ----
    // Forward the command to the engine and optimistically reflect the group's
    // host state in the UI; the engine confirms via HostEvent in the pump.
    let tunnel_host = Rc::new(tunnel_host);
    {
        let host = tunnel_host.clone();
        let state = state.clone();
        let weak = app.as_weak();
        let tray = tray.clone();
        let actions = actions.clone();
        let loc = loc.clone();
        let app_state = app_state.clone();
        app.on_host(move |tunnel_id| {
            let id = tunnel_id.to_string();
            host.send(host::HostCommand::Host {
                tunnel_id: id.clone(),
            });
            // Track the group as auto-host so it is re-hosted on next startup.
            {
                let mut st = app_state.borrow_mut();
                st.add_auto_host(&id);
                st.save();
            }
            state.borrow_mut().host.insert(id, "host".to_string());
            if let Some(a) = weak.upgrade() {
                rebuild_rows(&a, &tray, &actions, &state, &loc);
            }
        });
    }
    {
        let host = tunnel_host.clone();
        let state = state.clone();
        let weak = app.as_weak();
        let tray = tray.clone();
        let actions = actions.clone();
        let loc = loc.clone();
        let app_state = app_state.clone();
        app.on_stop(move |tunnel_id| {
            let id = tunnel_id.to_string();
            host.send(host::HostCommand::Stop {
                tunnel_id: id.clone(),
            });
            // An explicit Stop removes the group from the auto-host set.
            {
                let mut st = app_state.borrow_mut();
                st.remove_auto_host(&id);
                st.save();
            }
            let mut st = state.borrow_mut();
            st.host.remove(&id);
            // Drop probe results for this group's ports so badges clear.
            st.probe.retain(|(tid, _), _| tid != &id);
            drop(st);
            if let Some(a) = weak.upgrade() {
                rebuild_rows(&a, &tray, &actions, &state, &loc);
            }
        });
    }

    // ---- Port detail panel: row click toggles selection ----
    {
        let weak = app.as_weak();
        let state = state.clone();
        let loc = loc.clone();
        let metrics_tx = metrics_tx.clone();
        let app_state = app_state.clone();
        app.on_toggle_detail(move |index, tunnel_id, port| {
            let tid = tunnel_id.to_string();
            let mut st = state.borrow_mut();
            let same = st
                .detail
                .as_ref()
                .is_some_and(|(t, p)| *t == tid && *p == port);
            if same {
                // Collapse: clear the selection; the poll timer goes idle.
                st.detail = None;
                st.metrics_inflight = false;
                drop(st);
                if let Some(a) = weak.upgrade() {
                    a.set_selected_index(-1);
                }
            } else {
                st.detail = Some((tid.clone(), port));
                // One fetch starts now; the guard blocks the timer from piling on.
                st.metrics_inflight = true;
                drop(st);
                if let Some(a) = weak.upgrade() {
                    a.set_selected_index(index);
                    // Show "n/a" until the first poll lands; logs straight away.
                    apply_metrics(&a, None, &loc);
                    refresh_logs(&a, &app_state);
                }
                spawn_metrics_fetch(&metrics_tx, tid, port);
            }
        });
    }

    // ---- Port detail polling: DISABLED for stability (v0.1.0) ----
    // The periodic metrics fetch spawned a `devtunnel show -j` subprocess and the
    // log refresh snapshotted the ring on every tick. Both are turned off while we
    // focus on host stability; the detail panel itself is disabled in the UI
    // (PortRow.expandable = false) so nothing here ever runs. Re-enable together
    // with the panel when metrics/logs return (see docs/backlogs/metrics-chart).
    //
    // let detail_timer = slint::Timer::default();
    // {
    //     let weak = app.as_weak();
    //     let state = state.clone();
    //     let metrics_tx = metrics_tx.clone();
    //     let app_state = app_state.clone();
    //     detail_timer.start(
    //         slint::TimerMode::Repeated,
    //         Duration::from_secs(5),
    //         move || {
    //             let selected = state.borrow().detail.clone();
    //             let Some((tid, port)) = selected else { return };
    //             let Some(a) = weak.upgrade() else { return };
    //             // Logs are free to refresh; do it regardless of the active tab.
    //             refresh_logs(&a, &app_state);
    //             // Only poll metrics on the Metrics tab (0), and only if the last
    //             // fetch already returned.
    //             let on_metrics_tab = a.get_detail_active_tab() == 0;
    //             let busy = state.borrow().metrics_inflight;
    //             if on_metrics_tab && !busy {
    //                 state.borrow_mut().metrics_inflight = true;
    //                 spawn_metrics_fetch(&metrics_tx, tid, port);
    //             }
    //         },
    //     );
    // }

    // ---- Pump events (tray + load results) into the Slint loop via Timer ----
    let menu_rx = MenuEvent::receiver();
    let tray_rx = TrayIconEvent::receiver();
    let timer = slint::Timer::default();
    {
        let weak = app.as_weak();
        let tray = tray.clone();
        let actions = actions.clone();
        let loc = loc.clone();
        let state = state.clone();
        let app_state = app_state.clone();
        let tunnel_host = tunnel_host.clone();
        let auto_resume_pending = auto_resume_pending.clone();
        let renew_pending = renew_pending.clone();
        let tx = tx.clone();
        let pf_tx = pf_tx.clone();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(150),
            move || {
                // Tray menu clicks.
                while let Ok(ev) = menu_rx.try_recv() {
                    match actions.borrow().get(&ev.id) {
                        Some(Action::Show) => show_window(&weak),
                        Some(Action::Quit) => {
                            let _ = slint::quit_event_loop();
                        }
                        Some(Action::Copy(url)) => copy(url),
                        Some(Action::Open(url)) => open_browser(url),
                        None => {}
                    }
                }
                // Tray icon click: toggle the window.
                while let Ok(ev) = tray_rx.try_recv() {
                    if let TrayIconEvent::Click { .. } = ev {
                        toggle_window(&weak);
                    }
                }
                // A newer GitHub release was found -> show the update banner,
                // unless the user already chose to ignore exactly this version.
                while let Ok(info) = update_rx.try_recv() {
                    if info.version == app_state.borrow().settings.skipped_update {
                        continue;
                    }
                    if let Some(a) = weak.upgrade() {
                        let mut args = FluentArgs::new();
                        args.set("version", info.version.clone());
                        a.global::<Strings>()
                            .set_update_banner_body(loc.t_args("update-banner-body", &args).into());
                        a.set_update_version(info.version.into());
                        a.set_update_url(info.url.into());
                        a.set_update_available(true);
                    }
                }
                // CLI install outcomes -> clear "Installing…" and surface a
                // clear result instead of swallowing failures.
                while let Ok(outcome) = install_rx.try_recv() {
                    if let Some(a) = weak.upgrade() {
                        a.set_installing(false);
                        match outcome {
                            // Re-run preflight so the banner + requirements rows
                            // advance to the next gate (sign-in / ready).
                            devtunnel::InstallOutcome::Installed => {
                                a.set_status(loc.t("install-status-done").into());
                                let _ = pf_tx.send(devtunnel::preflight());
                            }
                            // winget unavailable — open the manual install page.
                            devtunnel::InstallOutcome::WingetMissing => {
                                a.set_status(loc.t("install-status-winget-missing").into());
                                let _ = open::that(devtunnel::CLI_INSTALL_URL);
                            }
                            // Needs admin: surface it and fall back to the page.
                            devtunnel::InstallOutcome::Elevation => {
                                a.set_status(loc.t("install-status-elevation").into());
                                let _ = open::that(devtunnel::CLI_INSTALL_URL);
                            }
                            // Any other failure: show the trimmed winget stderr.
                            devtunnel::InstallOutcome::Failed(e) => {
                                log::warn!("install_cli: {e}");
                                let mut args = FluentArgs::new();
                                args.set("message", e);
                                a.set_status(loc.t_args("install-status-failed", &args).into());
                            }
                        }
                    }
                }

                // Preflight results -> set app-state, swap the tray icon, and
                // kick the initial load when the environment is ready.
                while let Ok(pf) = pf_rx.try_recv() {
                    let app_state = match pf {
                        devtunnel::Preflight::Ok => "ready",
                        devtunnel::Preflight::CliMissing => "cli-missing",
                        devtunnel::Preflight::LoggedOut => "relogin",
                    };
                    if let Some(a) = weak.upgrade() {
                        a.set_app_state(app_state.into());
                        // Keep the Settings checklist in sync with CLI/login state.
                        #[cfg(windows)]
                        refresh_requirements(&a);
                    }
                    update_tray_icon(&tray, app_state);
                    if pf == devtunnel::Preflight::Ok {
                        // A successful preflight (startup or after re-login) re-arms
                        // the one-shot auto-resume so persisted auto-host groups are
                        // re-hosted once the upcoming load lands.
                        auto_resume_pending.set(true);
                        load_async(&weak, &tx, &loc);
                    }
                }
                // Load result: apply to UI and rebuild tray menu.
                let mut loaded = false;
                // Tracks whether at least one *successful* load landed this tick;
                // a failed first fetch must not consume the one-shot auto-resume.
                let mut load_ok = false;
                while let Ok((placeholder_id, hidden_key, result)) = rx.try_recv() {
                    if apply_rows(
                        &weak,
                        &tray,
                        &actions,
                        &state,
                        placeholder_id,
                        hidden_key,
                        result,
                        &loc,
                    ) {
                        load_ok = true;
                    }
                    loaded = true;
                }

                // Selected-port metrics -> detail panel (skip stale selections).
                while let Ok((tid, port, result)) = metrics_rx.try_recv() {
                    // This fetch is done — let the timer schedule the next one.
                    state.borrow_mut().metrics_inflight = false;
                    let current = state
                        .borrow()
                        .detail
                        .as_ref()
                        .is_some_and(|(t, p)| *t == tid && *p == port);
                    if !current {
                        continue;
                    }
                    // Metrics sample persistence DISABLED for stability (v0.1.0):
                    // no metrics are fetched while the detail panel is off, so
                    // nothing arrives here. Re-enable with the detail polling.
                    // if let Ok(m) = &result {
                    //     if let Ok(now) = std::time::SystemTime::now()
                    //         .duration_since(std::time::UNIX_EPOCH)
                    //     {
                    //         metrics_store::append(&tid, port, m, now.as_secs());
                    //     }
                    // }
                    if let Some(a) = weak.upgrade() {
                        // Errors (port deleted, CLI hiccup) degrade to "n/a".
                        apply_metrics(&a, result.ok().as_ref(), &loc);
                    }
                }

                // Host engine state changes -> update per-group host state.
                let mut host_changed = false;
                while let Ok(ev) = host_evt_rx.try_recv() {
                    match ev {
                        host::HostEvent::State {
                            tunnel_id,
                            state: hs,
                        } => {
                            log::debug!("host event: {tunnel_id} -> {hs:?}");
                            // Surface connection failures in the status bar (otherwise a failed
                            // Host is silent and looks like "nothing happened").
                            if let host::HostState::Error(msg) = &hs {
                                if let Some(a) = weak.upgrade() {
                                    let mut args = FluentArgs::new();
                                    args.set("message", msg.clone());
                                    a.set_status(loc.t_args("status-error", &args).into());
                                    // Login expiry during hosting switches the app
                                    // into the re-login state (banner + warning
                                    // tray icon).
                                    if devtunnel::is_auth_error(msg) {
                                        a.set_app_state("relogin".into());
                                        update_tray_icon(&tray, "relogin");
                                    }
                                }
                                // The tunnel was deleted/expired mid-session: drop
                                // it from the persisted auto-host set so the next
                                // launch does not retry a host that can never
                                // succeed (the loop that left it stuck on
                                // "authorizing…").
                                if devtunnel::is_missing_tunnel_error(msg) {
                                    let mut ps = app_state.borrow_mut();
                                    if ps.contains_auto_host(&tunnel_id) {
                                        ps.remove_auto_host(&tunnel_id);
                                        ps.save();
                                        log::info!(
                                            "host: {tunnel_id} no longer exists; removed from auto-host set"
                                        );
                                    }
                                }
                            }
                            let id = map_host_state(&hs);
                            let mut st = state.borrow_mut();
                            match id {
                                Some(v) => {
                                    st.host.insert(tunnel_id, v.to_string());
                                }
                                None => {
                                    // Stopped / Idle / Error: clear host + probe for the group.
                                    st.host.remove(&tunnel_id);
                                    st.probe.retain(|(tid, _), _| tid != &tunnel_id);
                                }
                            }
                            host_changed = true;
                        }
                        host::HostEvent::Progress { tunnel_id, phase } => {
                            // Show the connect sub-phase in the status bar so a
                            // multi-second connect reads as progress, not a hang
                            // (issue #45). Coarse host state is updated by the
                            // State arm; this only drives the transient label.
                            log::debug!("host progress: {tunnel_id} -> {phase:?}");
                            if let Some(a) = weak.upgrade() {
                                let key = match phase {
                                    host::ConnectPhase::Authorizing => "status-connect-authorizing",
                                    host::ConnectPhase::ConnectingRelay => "status-connect-relay",
                                    host::ConnectPhase::ForwardingPorts => "status-connect-ports",
                                };
                                a.set_status(loc.t(key).into());
                            }
                        }
                        host::HostEvent::ReloginRequired { tunnel_id } => {
                            log::warn!("host: re-login required (reported for {tunnel_id})");
                            // Enter the re-login state: banner + alert tray icon +
                            // a single Windows toast. The banner stays up until a
                            // successful sign-in flips preflight back to Ok.
                            if let Some(a) = weak.upgrade() {
                                if a.get_app_state() != "relogin" {
                                    a.set_app_state("relogin".into());
                                    a.set_status(loc.t("relogin-message").into());
                                    update_tray_icon(&tray, "relogin");
                                    #[cfg(windows)]
                                    show_relogin_toast(&loc);
                                }
                            }
                        }
                    }
                }

                // One-shot auto-resume (hosting build): after a successful load,
                // re-host every persisted auto-host group that resolves to a known
                // Real Tunnel ID with at least one port; log the skipped ones.
                if cfg!(feature = "hosting") && load_ok && auto_resume_pending.get() {
                    auto_resume_pending.set(false);
                    let ids = app_state.borrow().auto_host.clone();
                    if !ids.is_empty() {
                        let mut st = state.borrow_mut();
                        let mut pruned = false;
                        for id in &ids {
                            let exists = st.rows.iter().any(|r| &r.tunnel_id == id);
                            let known =
                                exists && st.rows.iter().any(|r| &r.tunnel_id == id && r.port > 0);
                            if known {
                                log::info!("auto-resume: hosting {id}");
                                tunnel_host.send(host::HostCommand::Host {
                                    tunnel_id: id.clone(),
                                });
                                st.host.insert(id.clone(), "host".to_string());
                                host_changed = true;
                            } else if !exists {
                                // The tunnel no longer exists (deleted/expired
                                // while the app was closed): drop it so we stop
                                // carrying a dead entry across launches.
                                log::info!(
                                    "auto-resume: {id} no longer exists; removing from auto-host set"
                                );
                                app_state.borrow_mut().remove_auto_host(id);
                                pruned = true;
                            } else {
                                log::info!("auto-resume: skipping portless group {id}");
                            }
                        }
                        if pruned {
                            app_state.borrow().save();
                        }
                    }
                }

                // One-shot renewal (issue #6): after the first successful load,
                // re-apply the expiration window + anonymous ACE for every
                // auto-host group. Subprocess-only — never touches the host engine.
                if load_ok && renew_pending.get() {
                    renew_pending.set(false);
                    let st = app_state.borrow();
                    renew_async(st.auto_host.clone(), st.settings.default_expiration.clone());
                }

                // Probe results -> update per-port health status.
                #[cfg(feature = "hosting")]
                let mut probe_changed = false;
                #[cfg(feature = "hosting")]
                while let Ok(ev) = probe_evt_rx.try_recv() {
                    match ev {
                        probe::ProbeEvent::Status {
                            tunnel_id,
                            port,
                            state: ps,
                        } => {
                            state
                                .borrow_mut()
                                .probe
                                .insert((tunnel_id, port), map_probe_state(&ps).to_string());
                            probe_changed = true;
                        }
                        // Zombie-tunnel instrumentation (issue #37): the probe found the
                        // Public URL unreachable while the local port is up. That is a
                        // zombie only if the engine still believes the group is Hosting
                        // (its RelayHandle never resolved); otherwise it is an ordinary
                        // drop the engine is already reconnecting. Log/flag only — no
                        // behaviour change. The recorded occurrences feed the #37
                        // go/no-go and, once that gates open, the #39 reconnect bridge.
                        probe::ProbeEvent::PublicUnreachable { tunnel_id, port } => {
                            let hosting = matches!(
                                state.borrow().host.get(&tunnel_id).map(String::as_str),
                                Some("hosting")
                            );
                            if hosting {
                                log::warn!(
                                    "zombie-tunnel suspect: {tunnel_id} port {port} — Public URL \
                                     unreachable while the local port is listening and the engine \
                                     state is Hosting (RelayHandle not resolved)"
                                );
                            } else {
                                log::debug!(
                                    "probe: {tunnel_id} port {port} Public URL unreachable but the \
                                     engine is not Hosting — ordinary drop, not a zombie"
                                );
                            }
                        }
                    }
                }

                // Re-point the probe at the currently-hosting groups' URLs whenever
                // the load or host state changed.
                #[cfg(feature = "hosting")]
                if loaded || host_changed {
                    let targets = hosting_targets(&state.borrow());
                    let _ = probe_cmd_tx.send(probe::ProbeCommand::SetTargets(targets));
                }

                // Rebuild the visible rows if any derived state changed.
                #[cfg(feature = "hosting")]
                let derived_changed = host_changed || probe_changed;
                #[cfg(not(feature = "hosting"))]
                let derived_changed = host_changed;
                if derived_changed && !loaded {
                    if let Some(a) = weak.upgrade() {
                        rebuild_rows(&a, &tray, &actions, &state, &loc);
                    }
                }
            },
        );
    }

    // ---- Periodic renewal (issue #6): while the app runs, re-apply the
    // expiration window + anonymous ACE for the auto-host groups every 12h.
    // `update --expiration` is idempotent, so the unconditional cadence keeps
    // the window far inside the 30-day limit without parsing timestamps.
    let renew_timer = slint::Timer::default();
    {
        let app_state = app_state.clone();
        renew_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(12 * 60 * 60),
            move || {
                let st = app_state.borrow();
                renew_async(st.auto_host.clone(), st.settings.default_expiration.clone());
            },
        );
    }

    // ---- Initial paint from the row cache (last successful load). Keeps the UI
    // useful instantly; the async refresh below reconciles from the service once
    // preflight reports the environment is ready.
    {
        let cached = state::load_row_cache();
        if !cached.is_empty() {
            state.borrow_mut().rows = cached;
            rebuild_rows(&app, &tray, &actions, &state, &loc);
        }
    }

    // ---- Initial preflight (background) ----
    // Sets app-state from the probe; the pump kicks the first load only when
    // the environment is ready (CLI present + logged in).
    {
        let pf_tx = pf_tx.clone();
        std::thread::spawn(move || {
            let _ = pf_tx.send(devtunnel::preflight());
        });
    }

    // Start minimized to tray: never call `show()`, just run the event loop.
    // The window appears via tray icon click or the "Open window" menu item.
    // Use the "until quit" variant so the app stays alive when the (only) window
    // is hidden to the tray — otherwise Slint's quit-on-last-window-closed would
    // terminate the whole process the moment the window is closed/hidden.
    if std::env::var_os("DEVTUNNEL_SHOW_ON_START").is_some() {
        let _ = app.show();
    }
    slint::run_event_loop_until_quit()?;
    Ok(())
}

/// Refreshes the Settings "Requirements" checklist properties from the live
/// environment. CLI/login state is read from the already-computed preflight
/// `app-state`; install/shortcut/auto-start are cheap registry + file checks.
#[cfg(windows)]
fn refresh_requirements(app: &AppWindow) {
    let app_state = app.get_app_state();
    app.set_req_cli_ok(app_state != "cli-missing");
    app.set_req_login_ok(app_state == "ready");
    app.set_req_installed_ok(install::is_installed());
    app.set_req_autostart_ok(autostart::is_enabled());
}

/// Enables "Start with Windows" by registering the auto-start Run entry at the
/// executable's *current* location. It does not relocate the binary or relaunch:
/// wherever the app runs from today is the path Windows will start at logon.
#[cfg(windows)]
fn enable_auto_start(_app_state: &Rc<RefCell<state::AppState>>) {
    match std::env::current_exe() {
        Ok(exe) => {
            if let Err(e) = autostart::enable_at(&exe) {
                log::warn!("autostart: failed to set Run entry: {e}");
            }
        }
        Err(e) => log::warn!("autostart: failed to resolve current executable: {e}"),
    }
}

/// Uninstalls the app: removes the Start-menu shortcut, disables start-with-
/// Windows, deletes the persisted app state, schedules deletion of the installed
/// executable (a running exe cannot delete itself — a detached `cmd` does it once
/// we exit), then quits the event loop so the file lock is released. Every step
/// is best-effort: a failure is logged but never blocks the rest of the teardown.
#[cfg(windows)]
fn perform_uninstall() {
    if let Err(e) = autostart::disable() {
        log::warn!("uninstall: failed to remove Run entry: {e}");
    }
    if let Err(e) = install::remove_shortcut() {
        log::warn!("uninstall: failed to remove shortcut: {e}");
    }
    // Remove persisted settings + row cache (%APPDATA%\devtunnel-gui).
    // Guard against state_dir()'s "." fallback when APPDATA is unset: deleting a
    // relative CWD recursively would be catastrophic (e.g. wiping the folder the
    // portable exe was launched from). Only ever remove an absolute, APPDATA-based
    // path; skip otherwise.
    let dir = state::state_dir();
    if dir.is_absolute() {
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            if e.kind() != std::io::ErrorKind::NotFound {
                log::warn!(
                    "uninstall: failed to remove state dir {}: {e}",
                    dir.display()
                );
            }
        }
    } else {
        log::warn!(
            "uninstall: skipping state dir removal; resolved to non-absolute path {}",
            dir.display()
        );
    }
    if let Err(e) = install::spawn_self_delete() {
        log::warn!("uninstall: failed to schedule self-delete: {e}");
    }
    // Exit so the executable's file lock is released and the detached deleter runs.
    let _ = slint::quit_event_loop();
}

/// True when Windows is set to dark app mode (`AppsUseLightTheme == 0`).
/// Defaults to light when the value is missing or unreadable.
#[cfg(windows)]
fn system_prefers_dark() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|k| k.get_value::<u32, _>("AppsUseLightTheme"))
        .map(|v| v == 0)
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn system_prefers_dark() -> bool {
    false
}

/// Maximum tunnel lifetime the Dev Tunnels service accepts.
const MAX_EXPIRATION_DAYS: i32 = 30;

/// Parses a stored expiration string (e.g. `"30d"`) into whole days for the
/// days-only UI, clamped to `[1, MAX_EXPIRATION_DAYS]`. Non-day strings (a
/// legacy `"12h"`, or empty) fall back to the maximum.
fn expiration_days(s: &str) -> i32 {
    s.trim()
        .trim_end_matches(['d', 'D'])
        .trim()
        .parse::<i32>()
        .unwrap_or(MAX_EXPIRATION_DAYS)
        .clamp(1, MAX_EXPIRATION_DAYS)
}

/// Formats a day count as the canonical CLI expiration string (`"Nd"`),
/// clamping to the service limit so an out-of-range value can never be sent.
fn expiration_string(days: i32) -> String {
    format!("{}d", days.clamp(1, MAX_EXPIRATION_DAYS))
}

fn show_window(weak: &slint::Weak<AppWindow>) {
    if let Some(a) = weak.upgrade() {
        let _ = a.show();
        a.window().set_minimized(false);
    }
}

fn toggle_window(weak: &slint::Weak<AppWindow>) {
    if let Some(a) = weak.upgrade() {
        if a.window().is_visible() {
            let _ = a.hide();
        } else {
            let _ = a.show();
            a.window().set_minimized(false);
        }
    }
}

fn copy(url: &str) {
    if url.is_empty() {
        return;
    }
    if let Ok(mut cb) = arboard::Clipboard::new() {
        let _ = cb.set_text(url.to_string());
    }
}

fn open_browser(url: &str) {
    if !url.is_empty() {
        let _ = open::that(url);
    }
}

/// Fires the fetch on a background thread; the result comes back via `Sender`.
fn load_async(weak: &slint::Weak<AppWindow>, tx: &Sender<LoadMsg>, loc: &Rc<Locale>) {
    if let Some(a) = weak.upgrade() {
        a.set_status(loc.t("status-refreshing").into());
    }
    let tx = tx.clone();
    let lang = locale::system_locale();
    std::thread::spawn(move || {
        let loc = Locale::load(&lang);
        let _ = tx.send((None, None, devtunnel::fetch_rows(&loc)));
    });
}

/// Renews every auto-host group on a background thread (issue #6): re-applies
/// the expiration window and the anonymous ACE via one-shot subprocess calls.
/// Independent of in-process SDK hosting by construction — it never sends a
/// `HostCommand`, so an active host connection is not disturbed. Failures are
/// logged, never surfaced (the next cycle retries).
fn renew_async(ids: Vec<String>, expiration: String) {
    if ids.is_empty() {
        return;
    }
    let lang = locale::system_locale();
    std::thread::spawn(move || {
        let loc = Locale::load(&lang);
        for id in ids {
            match devtunnel::renew_tunnel(&id, &expiration, &loc) {
                Ok(()) => log::info!("renew: refreshed expiration + anonymous ACE for {id}"),
                Err(e) => log::warn!("renew: failed for {id}: {e}"),
            }
        }
    });
}

/// Runs a mutating CLI operation on a background thread, then refreshes the list.
/// The op's success feeds straight into `fetch_rows`, so the same `apply_rows`
/// path reconciles the UI (and tray) from the service after every mutation.
/// `placeholder` is the id of an optimistic placeholder row to remove when done.
/// `hidden_key` is the hidden-delete key to clear when the op settles.
fn run_op_async<F>(
    weak: &slint::Weak<AppWindow>,
    tx: &Sender<LoadMsg>,
    status_key: &str,
    loc: &Rc<Locale>,
    placeholder: Option<u64>,
    hidden_key: Option<(String, Option<i32>)>,
    op: F,
) where
    F: FnOnce(&Locale) -> anyhow::Result<()> + Send + 'static,
{
    if let Some(a) = weak.upgrade() {
        a.set_status(loc.t(status_key).into());
    }
    let tx = tx.clone();
    let lang = locale::system_locale();
    std::thread::spawn(move || {
        let loc = Locale::load(&lang);
        let result = op(&loc).and_then(|()| devtunnel::fetch_rows(&loc));
        let _ = tx.send((placeholder, hidden_key, result));
    });
}

/// Applies a load result: fills the list and rebuilds the tray menu.
/// Always runs on the UI thread (called by the timer). Returns `true` when the
/// load succeeded (used to trigger the one-shot auto-resume).
// Each argument is a distinct UI-thread handle (window, tray, action map, live
// state, optimistic-update keys, the result, locale); bundling them into a
// struct would only move the same fields behind one indirection.
#[allow(clippy::too_many_arguments)]
fn apply_rows(
    weak: &slint::Weak<AppWindow>,
    tray: &tray_icon::TrayIcon,
    actions: &Rc<RefCell<HashMap<MenuId, Action>>>,
    state: &Rc<RefCell<LiveState>>,
    placeholder_id: Option<u64>,
    hidden_key: Option<(String, Option<i32>)>,
    result: anyhow::Result<Vec<devtunnel::Row>>,
    loc: &Rc<Locale>,
) -> bool {
    let Some(app) = weak.upgrade() else {
        return false;
    };

    // Remove the optimistic placeholder whether the op succeeded or failed.
    if let Some(id) = placeholder_id {
        state.borrow_mut().remove_placeholder(id);
    }

    // Clear the hidden-delete key on both success and error so the row is restored on failure.
    if let Some(ref key) = hidden_key {
        state.borrow_mut().unhide_delete(key);
    }

    match result {
        Ok(rows) => {
            // Build the group picker (deduped by Real Tunnel ID, order preserved);
            // append the "+ New group…" entry last with an empty id.
            let mut group_names: Vec<SharedString> = Vec::new();
            let mut group_ids: Vec<SharedString> = Vec::new();
            let mut seen = HashSet::new();
            for r in &rows {
                if seen.insert(r.tunnel_id.clone()) {
                    group_names.push(r.group.clone().into());
                    group_ids.push(r.tunnel_id.clone().into());
                }
            }
            group_names.push(loc.t("new-group-option").into());
            group_ids.push(SharedString::new());
            app.set_group_names(ModelRc::new(VecModel::from(group_names)));
            app.set_group_ids(ModelRc::new(VecModel::from(group_ids)));

            // Cache the load and rebuild the row model (status/host-state derived
            // from the latest probe/host events), then refresh probe targets.
            // Also persist the rows so the next startup paints immediately.
            state::save_row_cache(&rows);
            state.borrow_mut().rows = rows;
            // The header chip is set inside rebuild_rows from the ports actually
            // rendered into the cards (not raw `rows`): an optimistically-hidden or
            // stale port must not inflate the chip while its card shows portless.
            rebuild_rows(&app, tray, actions, state, loc);
            true
        }
        Err(e) => {
            // Rebuild so that the removed placeholder / restored hidden row is reflected.
            rebuild_rows(&app, tray, actions, state, loc);
            let msg = e.to_string();
            // Login expiry during management switches the app into the
            // re-login state (banner + warning tray icon).
            if devtunnel::is_auth_error(&msg) {
                app.set_app_state("relogin".into());
                update_tray_icon(tray, "relogin");
            }
            let mut args = FluentArgs::new();
            args.set("message", msg);
            app.set_status(loc.t_args("status-error", &args).into());
            false
        }
    }
}

/// Rebuilds the Slint group model and tray menu from the cached load, folding the
/// flat CLI rows into per-group `GroupView`s with nested `PortView`s. Each port's
/// `status` and each group's `hosting` flag derive from the latest probe/host
/// events. Runs on the UI thread (after a load, or when a host/probe event
/// updates derived state).
///
/// Returns the number of real service ports actually rendered into the cards
/// (excludes portless groups, optimistically-hidden ports, and provisioning
/// placeholders). The header chip is set from this so it can never disagree with
/// what the cards show — counting raw `rows` instead let a hidden/stale port
/// inflate the chip while the card showed the group as portless.
fn rebuild_rows(
    app: &AppWindow,
    tray: &tray_icon::TrayIcon,
    actions: &Rc<RefCell<HashMap<MenuId, Action>>>,
    state: &Rc<RefCell<LiveState>>,
    loc: &Rc<Locale>,
) -> usize {
    let st = state.borrow();

    // All folding (visible-row index space, optimistic delete/placeholder
    // handling, derived status/host-state, detail-panel reconciliation) lives in
    // the pure `view::fold`. main.rs only feeds the inputs and maps the plain
    // result onto Slint structs + the tray menu.
    let out = view::fold(&view::FoldInput {
        rows: &st.rows,
        probe: &st.probe,
        host: &st.host,
        hidden: &st.hidden,
        placeholders: &st.placeholders,
        detail: st.detail.as_ref(),
    });

    // Map the plain group/port data onto the Slint models.
    let groups: Vec<GroupView> = out
        .groups
        .iter()
        .map(|g| GroupView {
            group: g.group.clone().into(),
            tunnel_id: g.tunnel_id.clone().into(),
            expiration: g.expiration.clone().into(),
            hosting: g.hosting,
            host_state: g.host_state.clone().into(),
            provisioning: g.provisioning,
            has_port: g.has_port,
            ports: ModelRc::new(VecModel::from(
                g.ports
                    .iter()
                    .map(|p| PortView {
                        port: p.port,
                        protocol: p.protocol.clone().into(),
                        url: p.url.clone().into(),
                        status: p.status.clone().into(),
                        row_index: p.row_index,
                    })
                    .collect::<Vec<_>>(),
            )),
        })
        .collect();

    // Rebuild the tray menu with per-port actions from the same load (placeholders
    // have no URL, so they are skipped by build_tray_menu).
    let menu = build_tray_menu(&st.rows, &mut actions.borrow_mut(), loc);
    tray.set_menu(Some(Box::new(menu)));

    app.set_selected_index(out.selected_index);
    app.set_groups(ModelRc::new(VecModel::from(groups)));

    // The selected port no longer exists (deleted elsewhere): collapse so the
    // poll timer stops issuing CLI calls for it.
    drop(st);
    if out.stale_detail {
        state.borrow_mut().detail = None;
    }

    // Keep the header chip in lockstep with the cards: it is set here, at the one
    // place that knows how many real ports were actually rendered, so it can never
    // disagree with what the list shows. Callers that need a transient message
    // (creating…, deleting…, an error) set it *after* this returns and win.
    let mut args = FluentArgs::new();
    args.set("count", out.rendered_ports as i64);
    app.set_status(loc.t_args("status-port-count", &args).into());

    out.rendered_ports
}

/// Fires a `fetch_port_status` for the selected port on a background thread;
/// the tagged result comes back via the metrics channel drained in the pump.
fn spawn_metrics_fetch(
    tx: &Sender<(String, i32, anyhow::Result<devtunnel::PortMetrics>)>,
    tunnel_id: String,
    port: i32,
) {
    let tx = tx.clone();
    let lang = locale::system_locale();
    std::thread::spawn(move || {
        let loc = Locale::load(&lang);
        let result = devtunnel::fetch_port_status(&tunnel_id, port, &loc);
        let _ = tx.send((tunnel_id, port, result));
    });
}

/// Formats a byte count as a short human-readable value (e.g. "1.5 MB").
/// Unit symbols are technical notation, intentionally not localized.
fn human_bytes(v: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = v.max(0.0);
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", v as u64, UNITS[unit])
    } else {
        format!("{:.1} {}", v, UNITS[unit])
    }
}

/// Pushes a metrics result into the detail panel. `None` (no data yet, fetch
/// error, or CLI without the status block) renders every field as "n/a".
fn apply_metrics(app: &AppWindow, metrics: Option<&devtunnel::PortMetrics>, loc: &Locale) {
    let na = loc.t("metric-na");
    let total = |v: Option<f64>| -> SharedString {
        v.map(human_bytes).unwrap_or_else(|| na.clone()).into()
    };
    let rate = |v: Option<f64>| -> SharedString {
        v.map(|b| {
            let mut args = FluentArgs::new();
            args.set("value", human_bytes(b));
            loc.t_args("metric-rate-per-second", &args)
        })
        .unwrap_or_else(|| na.clone())
        .into()
    };
    let count = |v: Option<f64>| -> SharedString {
        v.map(|c| (c as i64).to_string())
            .unwrap_or_else(|| na.clone())
            .into()
    };
    app.set_detail_upload_total(total(metrics.and_then(|m| m.upload_total)));
    app.set_detail_upload_rate(rate(metrics.and_then(|m| m.upload_rate)));
    app.set_detail_download_total(total(metrics.and_then(|m| m.download_total)));
    app.set_detail_download_rate(rate(metrics.and_then(|m| m.download_rate)));
    app.set_detail_connections(count(metrics.and_then(|m| m.connection_count)));
}

/// Refreshes the Logs tab model from the capture ring buffer (oldest first),
/// filtered to the user-chosen minimum severity from Settings.
fn refresh_logs(app: &AppWindow, app_state: &Rc<RefCell<state::AppState>>) {
    let level = state::parse_log_level(&app_state.borrow().settings.log_level);
    let lines: Vec<SharedString> = logbuf::snapshot(level)
        .into_iter()
        .map(SharedString::from)
        .collect();
    app.set_detail_logs(ModelRc::new(VecModel::from(lines)));
}

/// Builds the tray menu: "Open window", one submenu per port with URL actions
/// (Copy / Open) and "Quit". Repopulates the `MenuId -> Action` map.
fn build_tray_menu(
    rows: &[devtunnel::Row],
    actions: &mut HashMap<MenuId, Action>,
    loc: &Locale,
) -> Menu {
    actions.clear();
    let menu = Menu::new();

    let show = MenuItem::new(loc.t("menu-open-window"), true, None);
    actions.insert(show.id().clone(), Action::Show);
    let _ = menu.append(&show);
    let _ = menu.append(&PredefinedMenuItem::separator());

    for r in rows {
        if r.url.is_empty() {
            continue;
        }
        let sub = Submenu::new(format!("{} :{}", r.group, r.port), true);
        let copy_it = MenuItem::new(loc.t("menu-copy-url"), true, None);
        let open_it = MenuItem::new(loc.t("menu-open-browser"), true, None);
        actions.insert(copy_it.id().clone(), Action::Copy(r.url.to_string()));
        actions.insert(open_it.id().clone(), Action::Open(r.url.to_string()));
        let _ = sub.append(&copy_it);
        let _ = sub.append(&open_it);
        let _ = menu.append(&sub);
    }

    let _ = menu.append(&PredefinedMenuItem::separator());
    let quit = MenuItem::new(loc.t("menu-quit"), true, None);
    actions.insert(quit.id().clone(), Action::Quit);
    let _ = menu.append(&quit);

    menu
}

/// Populates the Slint `Strings` global with translated values for the current locale.
/// Call once after constructing `AppWindow`, before showing the UI.
fn apply_strings(app: &AppWindow, loc: &Locale) {
    let s = app.global::<Strings>();
    // Store (MSIX) builds hide the self-install / uninstall / auto-start controls;
    // the package manages those. Compile-time constant so it is stripped in each build.
    s.set_store_build(cfg!(feature = "store"));
    s.set_status_loading(loc.t("status-loading").into());
    s.set_status_refreshing(loc.t("status-refreshing").into());
    s.set_btn_refresh(loc.t("btn-refresh").into());
    s.set_btn_new_group(loc.t("btn-new-group").into());
    s.set_btn_add_port(loc.t("btn-add-port").into());
    s.set_btn_settings(loc.t("btn-settings").into());

    s.set_no_url(loc.t("no-url").into());
    s.set_expires_label(loc.t("expires-label").into());
    s.set_btn_copy(loc.t("btn-copy").into());
    s.set_btn_open(loc.t("btn-open").into());

    // Hosting + health badges
    s.set_btn_host(loc.t("btn-host").into());
    s.set_btn_stop(loc.t("btn-stop").into());
    s.set_status_hosting(loc.t("status-hosting").into());
    s.set_status_stopped(loc.t("status-stopped").into());
    s.set_badge_operational(loc.t("badge-operational").into());
    s.set_badge_service_down(loc.t("badge-service-down").into());
    s.set_badge_down(loc.t("badge-down").into());
    s.set_badge_provisioning(loc.t("badge-provisioning").into());
    s.set_badge_hosted_external(loc.t("badge-hosted-external").into());
    s.set_btn_del_port(loc.t("btn-del-port").into());
    s.set_btn_del_group(loc.t("btn-del-group").into());

    // Redesign: status-dot tooltips, top bar, row tooltips, toast, empty state
    s.set_badge_stopped(loc.t("badge-stopped").into());
    s.set_badge_hosting(loc.t("badge-hosting").into());
    s.set_app_title(loc.t("app-title").into());
    s.set_pill_connected(loc.t("pill-connected").into());
    s.set_tooltip_settings(loc.t("tooltip-settings").into());
    s.set_tooltip_copy(loc.t("tooltip-copy").into());
    s.set_tooltip_open(loc.t("tooltip-open").into());
    s.set_toast_copied(loc.t("toast-copied").into());
    s.set_empty_title(loc.t("empty-title").into());
    s.set_btn_create_group(loc.t("btn-create-group").into());

    // Dialogs — common
    s.set_btn_cancel(loc.t("btn-cancel").into());
    s.set_btn_create(loc.t("btn-create").into());
    s.set_btn_add(loc.t("btn-add").into());
    s.set_btn_delete(loc.t("btn-delete").into());
    s.set_dlg_advanced(loc.t("dlg-advanced").into());
    s.set_dlg_keep_headers(loc.t("dlg-keep-headers").into());
    s.set_dlg_request_timeout(loc.t("dlg-request-timeout").into());
    s.set_ph_request_timeout(loc.t("ph-request-timeout").into());
    s.set_unit_days(loc.t("unit-days").into());

    // Dialog — new group
    s.set_dlg_new_group_title(loc.t("dlg-new-group-title").into());
    s.set_field_name(loc.t("field-name").into());
    s.set_field_expiration(loc.t("field-expiration").into());
    s.set_field_anonymous(loc.t("field-anonymous").into());
    s.set_field_description(loc.t("field-description").into());
    s.set_ph_group_name(loc.t("ph-group-name").into());
    s.set_ph_expiration(loc.t("ph-expiration").into());
    s.set_ph_description(loc.t("ph-description").into());

    // Port detail panel
    s.set_tab_metrics(loc.t("tab-metrics").into());
    s.set_tab_logs(loc.t("tab-logs").into());
    s.set_metric_upload(loc.t("metric-upload").into());
    s.set_metric_download(loc.t("metric-download").into());
    s.set_metric_total(loc.t("metric-total").into());
    s.set_metric_rate(loc.t("metric-rate").into());
    s.set_metric_connections(loc.t("metric-connections").into());
    s.set_metric_active(loc.t("metric-active").into());
    s.set_metric_na(loc.t("metric-na").into());
    s.set_logs_empty(loc.t("logs-empty").into());

    // Preflight banner / re-login
    s.set_banner_cli_missing_title(loc.t("banner-cli-missing-title").into());
    s.set_banner_cli_missing_body(loc.t("banner-cli-missing-body").into());
    s.set_banner_cli_missing_install(loc.t("banner-cli-missing-install").into());
    s.set_banner_relogin_title(loc.t("banner-relogin-title").into());
    s.set_banner_relogin_body(loc.t("banner-relogin-body").into());
    s.set_btn_sign_in(loc.t("btn-sign-in").into());
    s.set_banner_action_open_settings(loc.t("banner-action-open-settings").into());

    // Update available banner (update-banner-body is filled from Rust with the
    // release version when a newer release is found).
    s.set_update_banner_title(loc.t("update-banner-title").into());
    s.set_btn_update_download(loc.t("btn-update-download").into());
    s.set_btn_update_ignore(loc.t("btn-update-ignore").into());

    s.set_install_status_running(loc.t("install-status-running").into());
    s.set_install_status_done(loc.t("install-status-done").into());
    s.set_install_status_elevation(loc.t("install-status-elevation").into());
    s.set_install_status_winget_missing(loc.t("install-status-winget-missing").into());

    // Dialog — add port
    s.set_dlg_add_port_title(loc.t("dlg-add-port-title").into());
    s.set_field_group(loc.t("field-group").into());
    s.set_field_port(loc.t("field-port").into());
    s.set_field_protocol(loc.t("field-protocol").into());
    s.set_new_group_option(loc.t("new-group-option").into());
    s.set_ph_port(loc.t("ph-port").into());

    // Dialog — inline validation messages
    s.set_err_field_name_required(loc.t("err-field-name-required").into());
    s.set_err_field_port_required(loc.t("err-field-port-required").into());
    s.set_err_field_port_range(loc.t("err-field-port-range").into());

    // Settings — requirements checklist
    s.set_req_title(loc.t("req-title").into());
    s.set_req_cli(loc.t("req-cli").into());
    s.set_req_login(loc.t("req-login").into());
    s.set_req_login_as(loc.t("req-login-as").into());
    s.set_req_autostart(loc.t("req-autostart").into());
    s.set_btn_install_cli(loc.t("btn-install-cli").into());

    // Settings
    s.set_settings_title(loc.t("settings-title").into());
    s.set_settings_section_general(loc.t("settings-section-general").into());
    s.set_settings_section_status(loc.t("settings-section-status").into());
    s.set_settings_section_about(loc.t("settings-section-about").into());
    s.set_field_auto_start(loc.t("field-auto-start").into());
    s.set_field_probe_interval(loc.t("field-probe-interval").into());
    s.set_field_default_expiration(loc.t("field-default-expiration").into());
    s.set_field_log_level(loc.t("field-log-level").into());
    s.set_log_level_error(loc.t("log-level-error").into());
    s.set_log_level_warn(loc.t("log-level-warn").into());
    s.set_log_level_info(loc.t("log-level-info").into());
    s.set_log_level_debug(loc.t("log-level-debug").into());
    s.set_btn_close(loc.t("btn-close").into());
    s.set_btn_uninstall(loc.t("btn-uninstall").into());
    s.set_confirm_uninstall(loc.t("confirm-uninstall").into());

    // About
    s.set_about_title(loc.t("about-title").into());
    s.set_about_app_name(loc.t("about-app-name").into());
    s.set_about_version_label(loc.t("about-version-label").into());
    s.set_about_tagline(loc.t("about-tagline").into());
    s.set_about_built_on(loc.t("about-built-on").into());
    s.set_about_created_by(loc.t("about-created-by").into());
    s.set_about_link_docs(loc.t("about-link-docs").into());
    s.set_about_link_repo(loc.t("about-link-repo").into());
    s.set_about_link_license(loc.t("about-link-license").into());

    // Re-login
    s.set_relogin_message(loc.t("relogin-message").into());
}

/// Selects the winit backend with a window-attributes hook that sets the brand
/// icon on every window at creation time (so the title bar and taskbar show it,
/// not winit's generic default). Best-effort: logs and continues on failure.
fn install_window_icon() {
    const SIZE: u32 = 256;
    let rgba = icon_render::rgba(SIZE, icon_render::IconVariant::Normal);
    let icon = match slint::winit_030::winit::window::Icon::from_rgba(rgba, SIZE, SIZE) {
        Ok(icon) => icon,
        Err(e) => {
            log::warn!("window icon: failed to build ({e})");
            return;
        }
    };
    if let Err(e) = slint::BackendSelector::new()
        .with_winit_window_attributes_hook(move |attrs| attrs.with_window_icon(Some(icon.clone())))
        .select()
    {
        log::warn!("window icon: winit backend selection failed ({e})");
    }
}

/// The brand tunnel-portal tray icon (procedurally rendered — see `icon_render`).
fn build_icon() -> Icon {
    const SIZE: u32 = 32;
    let rgba = icon_render::rgba(SIZE, icon_render::IconVariant::Normal);
    Icon::from_rgba(rgba, SIZE, SIZE).expect("invalid tray icon rgba data")
}

/// Amber warning variant shown on the tray while the app is not "ready"
/// (CLI missing / re-login required).
fn build_warning_icon() -> Icon {
    const SIZE: u32 = 32;
    let rgba = icon_render::rgba(SIZE, icon_render::IconVariant::Warning);
    Icon::from_rgba(rgba, SIZE, SIZE).expect("invalid tray icon rgba data")
}

/// Swaps the tray icon to match the app-level preflight state: the normal blue
/// icon when "ready", the warning variant otherwise.
fn update_tray_icon(tray: &tray_icon::TrayIcon, app_state: &str) {
    let icon = if app_state == "ready" {
        build_icon()
    } else {
        build_warning_icon()
    };
    let _ = tray.set_icon(Some(icon));
}

/// Fires the one-shot Windows toast for the re-login case (no toasts for any
/// other status change). Best-effort: a toast failure is not worth surfacing.
#[cfg(windows)]
fn show_relogin_toast(loc: &Locale) {
    use tauri_winrt_notification::Toast;
    let _ = Toast::new(Toast::POWERSHELL_APP_ID)
        .title(&loc.t("toast-relogin-title"))
        .text1(&loc.t("toast-relogin-body"))
        .show();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state() -> LiveState {
        LiveState::default()
    }

    #[test]
    fn placeholder_push_remove() {
        let mut st = make_state();

        let id1 = st.push_placeholder("my-group".into(), 3000, "http".into());
        let id2 = st.push_placeholder("other-group".into(), 8080, "tcp".into());
        assert_eq!(st.placeholders.len(), 2);
        assert_ne!(id1, id2);

        st.remove_placeholder(id1);
        assert_eq!(st.placeholders.len(), 1);
        assert_eq!(st.placeholders[0].id, id2);

        st.remove_placeholder(id2);
        assert!(st.placeholders.is_empty());
    }

    #[test]
    fn expiration_days_parses_and_clamps() {
        assert_eq!(expiration_days("30d"), 30);
        assert_eq!(expiration_days("7d"), 7);
        assert_eq!(expiration_days("  14d "), 14);
        // Over the service limit clamps down; zero/negative clamp up to 1.
        assert_eq!(expiration_days("99d"), 30);
        assert_eq!(expiration_days("0d"), 1);
        // Legacy hour strings and empty/garbage fall back to the maximum.
        assert_eq!(expiration_days("12h"), MAX_EXPIRATION_DAYS);
        assert_eq!(expiration_days(""), MAX_EXPIRATION_DAYS);
    }

    #[test]
    fn expiration_string_formats_and_clamps() {
        assert_eq!(expiration_string(30), "30d");
        assert_eq!(expiration_string(1), "1d");
        assert_eq!(expiration_string(99), "30d");
        assert_eq!(expiration_string(0), "1d");
    }

    #[test]
    fn human_bytes_scales_units() {
        assert_eq!(human_bytes(0.0), "0 B");
        assert_eq!(human_bytes(512.0), "512 B");
        assert_eq!(human_bytes(1024.0), "1.0 KB");
        assert_eq!(human_bytes(1536.0), "1.5 KB");
        assert_eq!(human_bytes(5.0 * 1024.0 * 1024.0), "5.0 MB");
        // Negative values clamp to zero instead of underflowing.
        assert_eq!(human_bytes(-3.0), "0 B");
    }

    #[test]
    fn placeholder_row_is_provisioning() {
        let mut st = make_state();
        // One real row
        st.rows.push(devtunnel::Row {
            group: "g1".into(),
            tunnel_id: "tid1".into(),
            port: 9000,
            protocol: "http".into(),
            url: "https://example.com".into(),
            expiration: "30d".into(),
            host_connections: 0,
        });

        // No placeholder yet — only one row, status derives to "idle". The
        // derivation itself is exhaustively tested in `view`; here we just sanity
        // check the LiveState maps feed it correctly.
        let real_row_status = view::derive_status(&st.probe, &st.host, "tid1", 9000, 0);
        assert_eq!(real_row_status, "idle");

        // Push a placeholder; its fields are what `view::fold` turns into a row.
        let id = st.push_placeholder("new-group".into(), 4000, "tcp".into());
        assert_eq!(st.placeholders.len(), 1);
        assert_eq!(st.placeholders[0].port, 4000);
        assert_eq!(st.placeholders[0].group, "new-group");
        assert_eq!(st.placeholders[0].protocol, "tcp");

        // `view::fold` assigns this id to every placeholder row, which the
        // theme/UI render as the "Provisioning…" badge.
        assert_eq!(view::PROVISIONING_STATUS, "provisioning");

        // After removal the placeholder list is empty again.
        st.remove_placeholder(id);
        assert!(st.placeholders.is_empty());
    }

    fn make_row(tunnel_id: &str, port: i32) -> devtunnel::Row {
        devtunnel::Row {
            group: tunnel_id.to_string(),
            tunnel_id: tunnel_id.to_string(),
            port,
            protocol: "http".into(),
            url: "https://example.com".into(),
            expiration: "30d".into(),
            host_connections: 0,
        }
    }

    #[test]
    fn hidden_delete_insert_remove() {
        let mut st = make_state();

        let key1 = st.hide_delete("tid1".into(), Some(3000));
        let key2 = st.hide_delete("tid2".into(), None);
        assert_eq!(st.hidden.len(), 2);
        assert!(st.hidden.contains(&("tid1".to_string(), Some(3000))));
        assert!(st.hidden.contains(&("tid2".to_string(), None)));

        st.unhide_delete(&key1);
        assert_eq!(st.hidden.len(), 1);
        assert!(!st.hidden.contains(&("tid1".to_string(), Some(3000))));

        st.unhide_delete(&key2);
        assert!(st.hidden.is_empty());
    }

    #[test]
    fn hidden_port_excludes_matching_row() {
        let mut st = make_state();
        st.rows.push(make_row("tid1", 3000));
        st.rows.push(make_row("tid1", 8080));

        // Hide the port-3000 row; port-8080 should still be visible.
        st.hide_delete("tid1".into(), Some(3000));

        let visible: Vec<_> = st
            .rows
            .iter()
            .filter(|r| {
                !st.hidden.contains(&(r.tunnel_id.clone(), Some(r.port)))
                    && !st.hidden.contains(&(r.tunnel_id.clone(), None))
            })
            .collect();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].port, 8080);
    }

    #[test]
    fn hidden_group_excludes_all_ports() {
        let mut st = make_state();
        st.rows.push(make_row("tid1", 3000));
        st.rows.push(make_row("tid1", 8080));
        st.rows.push(make_row("tid2", 9000));

        // Hide the entire group tid1; tid2's row should remain.
        st.hide_delete("tid1".into(), None);

        let visible: Vec<_> = st
            .rows
            .iter()
            .filter(|r| {
                !st.hidden.contains(&(r.tunnel_id.clone(), Some(r.port)))
                    && !st.hidden.contains(&(r.tunnel_id.clone(), None))
            })
            .collect();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].tunnel_id, "tid2");
    }
}
