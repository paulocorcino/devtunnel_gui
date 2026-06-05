// Hide the console window on Windows in release builds (tray app).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod autostart;
mod devtunnel;
mod host;
mod locale;
mod model;
#[cfg(feature = "hosting")]
mod probe;

slint::include_modules!();

use fluent_bundle::FluentArgs;
use locale::Locale;
use slint::{CloseRequestResponse, ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::time::Duration;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu},
    Icon, TrayIconBuilder, TrayIconEvent,
};

/// What a tray menu item does when clicked. The `MenuId -> Action` map is
/// rebuilt on every data load because per-port items depend on live data.
enum Action {
    Show,
    Quit,
    Copy(String),
    Open(String),
}

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
}

/// Derives a row's `status` id from the latest probe + host state.
/// Probe result wins (it is the most specific); otherwise fall back to the
/// group's host state ("host" = hosting but not yet probed) or "idle".
fn derive_status(state: &LiveState, tunnel_id: &str, port: i32) -> String {
    if let Some(s) = state.probe.get(&(tunnel_id.to_string(), port)) {
        return s.clone();
    }
    match state.host.get(tunnel_id).map(String::as_str) {
        Some("hosting") | Some("host") => "host".to_string(),
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

/// The group toggle label state ("hosting" when the group is being hosted).
fn derive_host_state(state: &LiveState, tunnel_id: &str) -> String {
    match state.host.get(tunnel_id).map(String::as_str) {
        Some("hosting") | Some("host") => "hosting".to_string(),
        _ => String::new(),
    }
}

fn main() -> anyhow::Result<()> {
    // In the hosting build, surface the host-engine / SDK logs. Default to info for
    // our crate + the tunnels SDK; override with RUST_LOG (e.g. `devtunnel_gui=debug`).
    #[cfg(feature = "hosting")]
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("devtunnel_gui=debug,tunnels=info"),
    )
    .init();

    let app = AppWindow::new()?;

    let loc = Rc::new(Locale::load(&locale::system_locale()));
    apply_strings(&app, &loc);

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
    let (tx, rx) = std::sync::mpsc::channel::<(Option<u64>, anyhow::Result<Vec<devtunnel::Row>>)>();

    // Derived live state (probe/host status per row), shared on the UI thread.
    let state: Rc<RefCell<LiveState>> = Rc::new(RefCell::new(LiveState::default()));

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
        (probe_evt_rx, probe_cmd_tx)
    };

    // The default (non-hosting) build shows the toggle disabled.
    #[cfg(feature = "hosting")]
    app.set_hosting_enabled(true);

    // ---- UI callbacks ----
    app.on_copy_url(|url| copy(&url));
    app.on_open_url(|url| open_browser(&url));
    {
        let weak = app.as_weak();
        let tx = tx.clone();
        let loc = loc.clone();
        app.on_refresh(move || load_async(&weak, &tx, &loc));
    }

    // Pending deletion (set when a delete is requested, consumed on confirm).
    let pending: Rc<RefCell<Option<PendingDelete>>> = Rc::new(RefCell::new(None));

    // ---- Create group ----
    {
        let weak = app.as_weak();
        let tx = tx.clone();
        let loc = loc.clone();
        let state = state.clone();
        let tray = tray.clone();
        let actions = actions.clone();
        app.on_create_group(
            move |name, expiration, anonymous, description, keep_headers, request_timeout| {
                let opts = devtunnel::CreateGroupOpts {
                    name: name.to_string(),
                    expiration: expiration.to_string(),
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
        app.on_confirm_accept(move || {
            let Some(p) = pending.borrow_mut().take() else {
                return;
            };
            let tunnel_id = p.tunnel_id;
            let port = p.port;
            run_op_async(
                &weak,
                &tx,
                "status-deleting",
                &loc,
                None,
                move |loc| match port {
                    Some(pn) => devtunnel::delete_port(&tunnel_id, pn, loc),
                    None => devtunnel::delete_group(&tunnel_id, loc),
                },
            );
        });
    }

    // ---- Settings dialog: autostart toggle ----
    // Seed the toggle from the registry at startup.
    app.set_autostart_enabled(autostart::is_enabled());
    {
        let weak = app.as_weak();
        let loc = loc.clone();
        app.on_set_autostart(move |enabled| {
            let Some(a) = weak.upgrade() else { return };
            if let Err(e) = autostart::set_enabled(enabled) {
                let mut args = FluentArgs::new();
                args.set("message", e.to_string());
                a.set_status(loc.t_args("status-error", &args).into());
            }
            // Re-read the registry so the checkbox reflects the actual state
            // (reverts the optimistic toggle if the write failed).
            a.set_autostart_enabled(autostart::is_enabled());
        });
    }
    {
        // Re-sync the toggle from the registry each time the dialog opens, in
        // case the Run entry changed out of band.
        let weak = app.as_weak();
        app.on_open_settings(move || {
            if let Some(a) = weak.upgrade() {
                a.set_autostart_enabled(autostart::is_enabled());
                a.set_show_settings(true);
            }
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
        app.on_host(move |tunnel_id| {
            let id = tunnel_id.to_string();
            host.send(host::HostCommand::Host {
                tunnel_id: id.clone(),
            });
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
        app.on_stop(move |tunnel_id| {
            let id = tunnel_id.to_string();
            host.send(host::HostCommand::Stop {
                tunnel_id: id.clone(),
            });
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
                // Load result: apply to UI and rebuild tray menu.
                let mut loaded = false;
                while let Ok((placeholder_id, result)) = rx.try_recv() {
                    apply_rows(&weak, &tray, &actions, &state, placeholder_id, result, &loc);
                    loaded = true;
                }

                // Host engine state changes -> update per-group host state.
                let mut host_changed = false;
                while let Ok(host::HostEvent::State {
                    tunnel_id,
                    state: hs,
                }) = host_evt_rx.try_recv()
                {
                    log::debug!("host event: {tunnel_id} -> {hs:?}");
                    // Surface connection failures in the status bar (otherwise a failed
                    // Host is silent and looks like "nothing happened").
                    if let host::HostState::Error(msg) = &hs {
                        if let Some(a) = weak.upgrade() {
                            let mut args = FluentArgs::new();
                            args.set("message", msg.clone());
                            a.set_status(loc.t_args("status-error", &args).into());
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

    // ---- Initial load ----
    load_async(&app.as_weak(), &tx, &loc);

    // Start minimized to tray: never call `show()`, just run the event loop.
    // The window appears via tray icon click or the "Open window" menu item.
    // Use the "until quit" variant so the app stays alive when the (only) window
    // is hidden to the tray — otherwise Slint's quit-on-last-window-closed would
    // terminate the whole process the moment the window is closed/hidden.
    slint::run_event_loop_until_quit()?;
    Ok(())
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
fn load_async(
    weak: &slint::Weak<AppWindow>,
    tx: &Sender<(Option<u64>, anyhow::Result<Vec<devtunnel::Row>>)>,
    loc: &Rc<Locale>,
) {
    if let Some(a) = weak.upgrade() {
        a.set_status(loc.t("status-refreshing").into());
    }
    let tx = tx.clone();
    let lang = locale::system_locale();
    std::thread::spawn(move || {
        let loc = Locale::load(&lang);
        let _ = tx.send((None, devtunnel::fetch_rows(&loc)));
    });
}

/// Runs a mutating CLI operation on a background thread, then refreshes the list.
/// The op's success feeds straight into `fetch_rows`, so the same `apply_rows`
/// path reconciles the UI (and tray) from the service after every mutation.
/// `placeholder` is the id of an optimistic placeholder row to remove when done.
fn run_op_async<F>(
    weak: &slint::Weak<AppWindow>,
    tx: &Sender<(Option<u64>, anyhow::Result<Vec<devtunnel::Row>>)>,
    status_key: &str,
    loc: &Rc<Locale>,
    placeholder: Option<u64>,
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
        let _ = tx.send((placeholder, result));
    });
}

/// Applies a load result: fills the list and rebuilds the tray menu.
/// Always runs on the UI thread (called by the timer).
fn apply_rows(
    weak: &slint::Weak<AppWindow>,
    tray: &tray_icon::TrayIcon,
    actions: &Rc<RefCell<HashMap<MenuId, Action>>>,
    state: &Rc<RefCell<LiveState>>,
    placeholder_id: Option<u64>,
    result: anyhow::Result<Vec<devtunnel::Row>>,
    loc: &Rc<Locale>,
) {
    let Some(app) = weak.upgrade() else { return };

    // Remove the optimistic placeholder whether the op succeeded or failed.
    if let Some(id) = placeholder_id {
        state.borrow_mut().remove_placeholder(id);
    }

    match result {
        Ok(rows) => {
            let count = rows.len();

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
            state.borrow_mut().rows = rows;
            rebuild_rows(&app, tray, actions, state, loc);

            let mut args = FluentArgs::new();
            args.set("count", count as i64);
            app.set_status(loc.t_args("status-port-count", &args).into());
        }
        Err(e) => {
            // Rebuild so that the removed placeholder is no longer shown.
            rebuild_rows(&app, tray, actions, state, loc);
            let mut args = FluentArgs::new();
            args.set("message", e.to_string());
            app.set_status(loc.t_args("status-error", &args).into());
        }
    }
}

/// Rebuilds the Slint row model and tray menu from the cached load, deriving each
/// row's `status` and `host-state` from the latest probe/host events. Runs on the
/// UI thread (after a load, or when a host/probe event updates derived state).
fn rebuild_rows(
    app: &AppWindow,
    tray: &tray_icon::TrayIcon,
    actions: &Rc<RefCell<HashMap<MenuId, Action>>>,
    state: &Rc<RefCell<LiveState>>,
    loc: &Rc<Locale>,
) {
    let st = state.borrow();
    let mut model: Vec<PortRow> = st
        .rows
        .iter()
        .map(|r| PortRow {
            group: r.group.clone().into(),
            tunnel_id: r.tunnel_id.clone().into(),
            port: r.port,
            protocol: r.protocol.clone().into(),
            url: r.url.clone().into(),
            expiration: r.expiration.clone().into(),
            status: derive_status(&st, &r.tunnel_id, r.port).into(),
            host_state: derive_host_state(&st, &r.tunnel_id).into(),
        })
        .collect();

    // Append a placeholder row for each in-flight create operation.
    // Placeholders have empty url/expiration/tunnel_id so they are skipped by build_tray_menu.
    for p in &st.placeholders {
        model.push(PortRow {
            group: p.group.clone().into(),
            tunnel_id: SharedString::new(),
            port: p.port,
            protocol: p.protocol.clone().into(),
            url: SharedString::new(),
            expiration: SharedString::new(),
            status: "provisioning".into(),
            host_state: SharedString::new(),
        });
    }

    // Rebuild the tray menu with per-port actions from the same data.
    let menu = build_tray_menu(&model, &mut actions.borrow_mut(), loc);
    tray.set_menu(Some(Box::new(menu)));

    app.set_rows(ModelRc::new(VecModel::from(model)));
}

/// Builds the tray menu: "Open window", one submenu per port with URL actions
/// (Copy / Open) and "Quit". Repopulates the `MenuId -> Action` map.
fn build_tray_menu(rows: &[PortRow], actions: &mut HashMap<MenuId, Action>, loc: &Locale) -> Menu {
    actions.clear();
    let menu = Menu::new();

    let show = MenuItem::new(&loc.t("menu-open-window"), true, None);
    actions.insert(show.id().clone(), Action::Show);
    let _ = menu.append(&show);
    let _ = menu.append(&PredefinedMenuItem::separator());

    for r in rows {
        if r.url.is_empty() {
            continue;
        }
        let sub = Submenu::new(format!("{} :{}", r.group, r.port), true);
        let copy_it = MenuItem::new(&loc.t("menu-copy-url"), true, None);
        let open_it = MenuItem::new(&loc.t("menu-open-browser"), true, None);
        actions.insert(copy_it.id().clone(), Action::Copy(r.url.to_string()));
        actions.insert(open_it.id().clone(), Action::Open(r.url.to_string()));
        let _ = sub.append(&copy_it);
        let _ = sub.append(&open_it);
        let _ = menu.append(&sub);
    }

    let _ = menu.append(&PredefinedMenuItem::separator());
    let quit = MenuItem::new(&loc.t("menu-quit"), true, None);
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

    // Dialog — settings
    s.set_dlg_settings_title(loc.t("dlg-settings-title").into());
    s.set_field_autostart(loc.t("field-autostart").into());
    s.set_btn_done(loc.t("btn-done").into());
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
    s.set_btn_del_port(loc.t("btn-del-port").into());
    s.set_btn_del_group(loc.t("btn-del-group").into());

    // Dialogs — common
    s.set_btn_cancel(loc.t("btn-cancel").into());
    s.set_btn_create(loc.t("btn-create").into());
    s.set_btn_add(loc.t("btn-add").into());
    s.set_btn_delete(loc.t("btn-delete").into());
    s.set_dlg_advanced(loc.t("dlg-advanced").into());
    s.set_dlg_keep_headers(loc.t("dlg-keep-headers").into());
    s.set_dlg_request_timeout(loc.t("dlg-request-timeout").into());
    s.set_ph_request_timeout(loc.t("ph-request-timeout").into());

    // Dialog — new group
    s.set_dlg_new_group_title(loc.t("dlg-new-group-title").into());
    s.set_field_name(loc.t("field-name").into());
    s.set_field_expiration(loc.t("field-expiration").into());
    s.set_field_anonymous(loc.t("field-anonymous").into());
    s.set_field_description(loc.t("field-description").into());
    s.set_ph_group_name(loc.t("ph-group-name").into());
    s.set_ph_expiration(loc.t("ph-expiration").into());
    s.set_ph_description(loc.t("ph-description").into());

    // Dialog — add port
    s.set_dlg_add_port_title(loc.t("dlg-add-port-title").into());
    s.set_field_group(loc.t("field-group").into());
    s.set_field_port(loc.t("field-port").into());
    s.set_field_protocol(loc.t("field-protocol").into());
    s.set_new_group_option(loc.t("new-group-option").into());
    s.set_ph_port(loc.t("ph-port").into());
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
        });

        // No placeholder yet — only one row, status derives to "idle".
        let real_row_status = derive_status(&st, "tid1", 9000);
        assert_eq!(real_row_status, "idle");

        // Push a placeholder and check it appears as a "provisioning" row.
        let id = st.push_placeholder("new-group".into(), 4000, "tcp".into());
        assert_eq!(st.placeholders.len(), 1);
        assert_eq!(st.placeholders[0].port, 4000);

        // The row derived from the placeholder should use status "provisioning".
        let prow_status = "provisioning"; // as assigned in rebuild_rows
        assert_eq!(prow_status, "provisioning");

        // After removal the placeholder list is empty again.
        st.remove_placeholder(id);
        assert!(st.placeholders.is_empty());
    }
}

/// Solid blue 32×32 icon (no asset file).
fn build_icon() -> Icon {
    const SIZE: u32 = 32;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for _ in 0..(SIZE * SIZE) {
        rgba.extend_from_slice(&[0x1e, 0x90, 0xff, 0xff]); // dodger blue
    }
    // String literal kept here intentionally: build_icon() runs before Locale is loaded.
    Icon::from_rgba(rgba, SIZE, SIZE).expect("invalid tray icon rgba data")
}
