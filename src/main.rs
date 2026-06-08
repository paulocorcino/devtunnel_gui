// Hide the console window on Windows in release builds (tray app).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod autostart;
mod devtunnel;
mod host;
mod icon_render;
#[cfg(windows)]
mod install;
mod locale;
mod logbuf;
mod model;
#[cfg(feature = "hosting")]
mod probe;
mod state;

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

/// Status id assigned to optimistic placeholder rows (see [`rebuild_rows`]).
/// Drives the "Provisioning…" badge and disables the row's action buttons.
const PROVISIONING_STATUS: &str = "provisioning";

/// A deletion awaiting user confirmation. `port == None` means delete the whole group.
struct PendingDelete {
    tunnel_id: String,
    port: Option<i32>,
}

/// An optimistic placeholder inserted immediately when a create-group / add-port
/// operation is dispatched. Replaced by the real row when the op's refresh lands.
struct Placeholder {
    id: u64,
    group: String,
    port: i32,
    protocol: String,
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

/// Derives a row's `status` id from the latest probe + host state.
/// Probe result wins (it is the most specific); otherwise fall back to the
/// group's host state ("host" = hosting but not yet probed), then to the
/// service-reported `host_connections` count, then "idle".
fn derive_status(state: &LiveState, tunnel_id: &str, port: i32, host_connections: i64) -> String {
    if let Some(s) = state.probe.get(&(tunnel_id.to_string(), port)) {
        return s.clone();
    }
    match state.host.get(tunnel_id).map(String::as_str) {
        Some("hosting") | Some("host") => "host".to_string(),
        _ if host_connections > 0 => "host".to_string(),
        _ => "idle".to_string(),
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

/// Derives the group toggle state:
/// - `"hosting"` when this session is actively hosting the group,
/// - `"external"` when the service reports active connections but this session is not hosting,
/// - `""` otherwise.
fn derive_host_state(state: &LiveState, tunnel_id: &str, host_connections: i64) -> String {
    match state.host.get(tunnel_id).map(String::as_str) {
        Some("hosting") | Some("host") => "hosting".to_string(),
        _ if host_connections > 0 => "external".to_string(),
        _ => String::new(),
    }
}

fn main() -> anyhow::Result<()> {
    // Install the capturing logger in every build: it tees records to stderr
    // (what env_logger used to print in the hosting build) and into the ring
    // buffer behind the Logs tab. Default to debug for our crate + info for the
    // tunnels SDK; override with RUST_LOG (e.g. `devtunnel_gui=trace`).
    let _ = logbuf::CaptureLogger::from_env("devtunnel_gui=debug,tunnels=info").install();

    // Relocation handshake: when launched by a just-installed copy with
    // `--relocated-from <path>`, delete the portable original we were moved from.
    // Runs on a detached thread because the delete retries for up to ~3 s while
    // the previous process exits and releases the file lock — never block startup.
    #[cfg(windows)]
    {
        let mut args = std::env::args().skip(1);
        while let Some(a) = args.next() {
            if a == install::RELOCATED_FROM_FLAG {
                match args.next() {
                    Some(old) => {
                        std::thread::spawn(move || {
                            install::cleanup_relocated(std::path::Path::new(&old));
                        });
                    }
                    None => log::warn!(
                        "install: {} given with no path",
                        install::RELOCATED_FROM_FLAG
                    ),
                }
            }
        }
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
            .with_tooltip("DevTunnel GUI")
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
                    enable_auto_start(&app_state);
                    // `enable_auto_start` exits the process when it relocates a
                    // portable install, so reaching here means we either were
                    // already installed or the relocation could not hand off.
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
    // ---- Settings: probe interval + default expiration (issue #6) ----
    // Seed the dialog properties from the persisted settings; the handlers
    // persist edits and (hosting build) re-target the live probe immediately.
    {
        let st = app_state.borrow();
        app.set_probe_interval_secs(st.settings.probe_interval_secs as i32);
        app.set_default_expiration_days(expiration_days(&st.settings.default_expiration));
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
                a.set_show_settings(true);
            }
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
                drop(st);
                if let Some(a) = weak.upgrade() {
                    a.set_selected_index(-1);
                }
            } else {
                st.detail = Some((tid.clone(), port));
                drop(st);
                if let Some(a) = weak.upgrade() {
                    a.set_selected_index(index);
                    // Show "n/a" until the first poll lands; logs straight away.
                    apply_metrics(&a, None, &loc);
                    refresh_logs(&a);
                }
                spawn_metrics_fetch(&metrics_tx, tid, port);
            }
        });
    }

    // ---- Port detail polling: refresh metrics + logs while a panel is open ----
    // A no-op while collapsed (detail == None), so no CLI calls are wasted.
    let detail_timer = slint::Timer::default();
    {
        let weak = app.as_weak();
        let state = state.clone();
        let metrics_tx = metrics_tx.clone();
        detail_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(3),
            move || {
                let selected = state.borrow().detail.clone();
                let Some((tid, port)) = selected else { return };
                spawn_metrics_fetch(&metrics_tx, tid, port);
                if let Some(a) = weak.upgrade() {
                    refresh_logs(&a);
                }
            },
        );
    }

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
                    let current = state
                        .borrow()
                        .detail
                        .as_ref()
                        .is_some_and(|(t, p)| *t == tid && *p == port);
                    if !current {
                        continue;
                    }
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
                        for id in &ids {
                            let known = st.rows.iter().any(|r| &r.tunnel_id == id && r.port > 0);
                            if known {
                                log::info!("auto-resume: hosting {id}");
                                tunnel_host.send(host::HostCommand::Host {
                                    tunnel_id: id.clone(),
                                });
                                st.host.insert(id.clone(), "host".to_string());
                                host_changed = true;
                            } else {
                                log::info!("auto-resume: skipping unknown or portless group {id}");
                            }
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
                while let Ok(probe::ProbeEvent::Status {
                    tunnel_id,
                    port,
                    state: ps,
                }) = probe_evt_rx.try_recv()
                {
                    state
                        .borrow_mut()
                        .probe
                        .insert((tunnel_id, port), map_probe_state(&ps).to_string());
                    probe_changed = true;
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
    app.set_req_shortcut_ok(install::shortcut_exists());
    app.set_req_autostart_ok(autostart::is_enabled());
}

/// Enables "Start with Windows", performing the full per-user install when the
/// app is running as a portable executable: relocate into `%LOCALAPPDATA%\
/// Programs`, create the Start-menu shortcut, register auto-start at the new
/// path, then relaunch from there and exit (the fresh instance deletes the
/// portable original). When already installed, just (re)writes the Run entry and
/// ensures the shortcut exists.
#[cfg(windows)]
fn enable_auto_start(app_state: &Rc<RefCell<state::AppState>>) {
    if install::is_installed() {
        if let Ok(exe) = std::env::current_exe() {
            if let Err(e) = autostart::enable_at(&exe) {
                log::warn!("autostart: failed to set Run entry: {e}");
            }
            if let Err(e) = install::create_start_menu_shortcut(&exe) {
                log::warn!("install: failed to create shortcut: {e}");
            }
        }
        return;
    }

    // Portable: move into the programs folder and hand off to the new copy.
    let new_exe = match install::install_self() {
        Ok(p) => p,
        Err(e) => {
            log::warn!("install: relocation failed: {e}");
            return;
        }
    };
    if let Err(e) = install::create_start_menu_shortcut(&new_exe) {
        log::warn!("install: failed to create shortcut: {e}");
    }
    if let Err(e) = autostart::enable_at(&new_exe) {
        log::warn!("autostart: failed to set Run entry: {e}");
    }
    // Persist the enabled state before relaunching so the fresh instance reflects it.
    {
        let mut st = app_state.borrow_mut();
        st.settings.auto_start = true;
        st.save();
    }
    if let Ok(old) = std::env::current_exe() {
        if install::relaunch_from(&new_exe, &old).is_ok() {
            std::process::exit(0);
        }
        log::warn!("install: relaunch from new location failed; staying in place");
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
            // Count real ports only: a portless group is carried as a `port == 0`
            // row, which must not inflate the header's port tally.
            let count = rows.iter().filter(|r| r.port != 0).count();

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
            rebuild_rows(&app, tray, actions, state, loc);

            let mut args = FluentArgs::new();
            args.set("count", count as i64);
            app.set_status(loc.t_args("status-port-count", &args).into());
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
fn rebuild_rows(
    app: &AppWindow,
    tray: &tray_icon::TrayIcon,
    actions: &Rc<RefCell<HashMap<MenuId, Action>>>,
    state: &Rc<RefCell<LiveState>>,
    loc: &Rc<Locale>,
) {
    let st = state.borrow();
    // Build a flat index space first: every visible (non-hidden) real port gets a
    // stable `row-index` used to key the expandable detail panel (issue #17). The
    // same index drives `selected-index` so the open panel survives reloads.
    // Optimistic delete (#13) hides ports/groups awaiting their confirming refresh.
    // Only a group-level delete (`(id, None)`) drops the whole card here; a
    // port-level delete (`(id, Some(port))`) keeps the row in the index space and
    // is skipped further down when attaching ports. This way deleting a group's
    // last port leaves the card standing (as portless) instead of flickering the
    // whole card out and back when the confirming refresh lands.
    let visible_rows: Vec<&devtunnel::Row> = st
        .rows
        .iter()
        .filter(|r| !st.hidden.contains(&(r.tunnel_id.clone(), None)))
        .collect();

    // Fold the flat rows into groups (Real Tunnel ID order preserved). Ports are
    // collected separately and attached as models at the end.
    let mut groups: Vec<GroupView> = Vec::new();
    let mut ports: Vec<Vec<PortView>> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (flat_idx, r) in visible_rows.iter().enumerate() {
        let gi = match index.get(&r.tunnel_id) {
            Some(&i) => i,
            None => {
                index.insert(r.tunnel_id.clone(), groups.len());
                groups.push(GroupView {
                    group: r.group.clone().into(),
                    tunnel_id: r.tunnel_id.clone().into(),
                    expiration: r.expiration.clone().into(),
                    hosting: derive_host_state(&st, &r.tunnel_id, r.host_connections) == "hosting",
                    // "Hosted elsewhere" pill: service reports connections but this
                    // session is not hosting the group (issue #15).
                    host_state: derive_host_state(&st, &r.tunnel_id, r.host_connections).into(),
                    provisioning: false,
                    has_port: false,
                    ports: ModelRc::default(),
                });
                ports.push(Vec::new());
                groups.len() - 1
            }
        };
        // A port==0 row is a portless group: keep the card, skip the port row.
        // A port hidden by an optimistic delete (#13) likewise keeps its card but
        // drops the port row until the reflush refresh confirms the deletion.
        if r.port != 0 && !st.hidden.contains(&(r.tunnel_id.clone(), Some(r.port))) {
            groups[gi].has_port = true;
            ports[gi].push(PortView {
                port: r.port,
                protocol: r.protocol.clone().into(),
                url: r.url.clone().into(),
                status: derive_status(&st, &r.tunnel_id, r.port, r.host_connections).into(),
                row_index: flat_idx as i32,
            });
        }
    }

    // Optimistic placeholders for in-flight creates: attach the provisioning
    // port to its existing group (matched by friendly name) when possible,
    // otherwise add a whole provisioning card. Placeholders are inert, so they
    // carry row-index -1 (not expandable).
    for p in &st.placeholders {
        match groups.iter().position(|g| g.group == p.group.as_str()) {
            Some(gi) if p.port != 0 => ports[gi].push(PortView {
                port: p.port,
                protocol: p.protocol.clone().into(),
                url: SharedString::new(),
                status: PROVISIONING_STATUS.into(),
                row_index: -1,
            }),
            _ => {
                groups.push(GroupView {
                    group: p.group.clone().into(),
                    tunnel_id: SharedString::new(),
                    expiration: SharedString::new(),
                    hosting: false,
                    host_state: SharedString::new(),
                    provisioning: true,
                    has_port: p.port != 0,
                    ports: ModelRc::default(),
                });
                ports.push(if p.port != 0 {
                    vec![PortView {
                        port: p.port,
                        protocol: p.protocol.clone().into(),
                        url: SharedString::new(),
                        status: PROVISIONING_STATUS.into(),
                        row_index: -1,
                    }]
                } else {
                    Vec::new()
                });
            }
        }
    }
    for (g, pv) in groups.iter_mut().zip(ports) {
        g.ports = ModelRc::new(VecModel::from(pv));
    }

    // Recompute the expanded port's flat index: rows can reorder or disappear
    // across reloads, so the selection is keyed by (tunnel_id, port), not index.
    let mut selected = -1;
    let mut stale_detail = false;
    if let Some((tid, port)) = st.detail.as_ref() {
        // A port hidden by an optimistic delete is still in `visible_rows` (to keep
        // its group card alive), so check the hidden set too: deleting the expanded
        // port must collapse the panel rather than leave it pointing at a gone row.
        let deleting = st.hidden.contains(&(tid.clone(), Some(*port)))
            || st.hidden.contains(&(tid.clone(), None));
        match visible_rows
            .iter()
            .position(|r| r.tunnel_id == tid.as_str() && r.port == *port)
        {
            Some(i) if !deleting => selected = i as i32,
            _ => stale_detail = true,
        }
    }

    // Rebuild the tray menu with per-port actions from the same load (placeholders
    // have no URL, so they are skipped by build_tray_menu).
    let menu = build_tray_menu(&st.rows, &mut actions.borrow_mut(), loc);
    tray.set_menu(Some(Box::new(menu)));

    app.set_selected_index(selected);
    app.set_groups(ModelRc::new(VecModel::from(groups)));

    // The selected port no longer exists (deleted elsewhere): collapse so the
    // poll timer stops issuing CLI calls for it.
    drop(st);
    if stale_detail {
        state.borrow_mut().detail = None;
    }
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

/// Refreshes the Logs tab model from the capture ring buffer (oldest first).
fn refresh_logs(app: &AppWindow) {
    let lines: Vec<SharedString> = logbuf::snapshot()
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

    // Settings — requirements checklist
    s.set_req_title(loc.t("req-title").into());
    s.set_req_cli(loc.t("req-cli").into());
    s.set_req_login(loc.t("req-login").into());
    s.set_req_installed(loc.t("req-installed").into());
    s.set_req_shortcut(loc.t("req-shortcut").into());
    s.set_req_autostart(loc.t("req-autostart").into());
    s.set_btn_install_cli(loc.t("btn-install-cli").into());
    s.set_req_install_hint(loc.t("req-install-hint").into());

    // Settings
    s.set_settings_title(loc.t("settings-title").into());
    s.set_field_auto_start(loc.t("field-auto-start").into());
    s.set_field_probe_interval(loc.t("field-probe-interval").into());
    s.set_field_default_expiration(loc.t("field-default-expiration").into());
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

        // No placeholder yet — only one row, status derives to "idle".
        let real_row_status = derive_status(&st, "tid1", 9000, 0);
        assert_eq!(real_row_status, "idle");

        // Push a placeholder; its fields are what `rebuild_rows` turns into a row.
        let id = st.push_placeholder("new-group".into(), 4000, "tcp".into());
        assert_eq!(st.placeholders.len(), 1);
        assert_eq!(st.placeholders[0].port, 4000);
        assert_eq!(st.placeholders[0].group, "new-group");
        assert_eq!(st.placeholders[0].protocol, "tcp");

        // `rebuild_rows` assigns this id to every placeholder row, which the
        // theme/UI render as the "Provisioning…" badge.
        assert_eq!(PROVISIONING_STATUS, "provisioning");

        // After removal the placeholder list is empty again.
        st.remove_placeholder(id);
        assert!(st.placeholders.is_empty());
    }

    #[test]
    fn derive_host_state_session_hosting_wins_over_service_count() {
        let mut st = make_state();
        st.host.insert("t1".into(), "hosting".into());
        // Even with host_connections > 0, this-session state returns "hosting".
        assert_eq!(derive_host_state(&st, "t1", 3), "hosting");
    }

    #[test]
    fn derive_host_state_session_connecting_wins_over_service_count() {
        let mut st = make_state();
        st.host.insert("t1".into(), "host".into());
        assert_eq!(derive_host_state(&st, "t1", 1), "hosting");
    }

    #[test]
    fn derive_host_state_external_when_service_has_connections() {
        let st = make_state();
        // No entry in st.host (this session is not hosting), but service reports connections.
        assert_eq!(derive_host_state(&st, "t1", 2), "external");
    }

    #[test]
    fn derive_host_state_idle_when_no_connections() {
        let st = make_state();
        assert_eq!(derive_host_state(&st, "t1", 0), "");
    }

    #[test]
    fn derive_status_session_hosting_wins() {
        let mut st = make_state();
        st.host.insert("t1".into(), "hosting".into());
        assert_eq!(derive_status(&st, "t1", 3000, 0), "host");
    }

    #[test]
    fn derive_status_external_host_connections_gives_host_color() {
        let st = make_state();
        // service says hosted externally — dot should use "host" color
        assert_eq!(derive_status(&st, "t1", 3000, 1), "host");
    }

    #[test]
    fn derive_status_zero_connections_is_idle() {
        let st = make_state();
        assert_eq!(derive_status(&st, "t1", 3000, 0), "idle");
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
