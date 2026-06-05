// Hide the console window on Windows in release builds (tray app).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod devtunnel;
mod host;
mod locale;
mod model;

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

fn main() -> anyhow::Result<()> {
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
    let (tx, rx) = std::sync::mpsc::channel::<anyhow::Result<Vec<devtunnel::Row>>>();

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
                run_op_async(&weak, &tx, "status-creating-group", &loc, move |loc| {
                    devtunnel::create_group(&opts, loc).map(|_| ())
                });
            },
        );
    }

    // ---- Add port (creates the group inline when "+ New group…" was chosen) ----
    {
        let weak = app.as_weak();
        let tx = tx.clone();
        let loc = loc.clone();
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
                run_op_async(&weak, &tx, "status-adding-port", &loc, move |loc| {
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
                });
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
            run_op_async(&weak, &tx, "status-deleting", &loc, move |loc| match port {
                Some(pn) => devtunnel::delete_port(&tunnel_id, pn, loc),
                None => devtunnel::delete_group(&tunnel_id, loc),
            });
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
                while let Ok(result) = rx.try_recv() {
                    apply_rows(&weak, &tray, &actions, result, &loc);
                }
            },
        );
    }

    // ---- Initial load ----
    load_async(&app.as_weak(), &tx, &loc);

    // Start minimized to tray: never call `show()`, just run the event loop.
    // The window appears via tray icon click or the "Open window" menu item.
    slint::run_event_loop()?;
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
    tx: &Sender<anyhow::Result<Vec<devtunnel::Row>>>,
    loc: &Rc<Locale>,
) {
    if let Some(a) = weak.upgrade() {
        a.set_status(loc.t("status-refreshing").into());
    }
    let tx = tx.clone();
    let lang = locale::system_locale();
    std::thread::spawn(move || {
        let loc = Locale::load(&lang);
        let _ = tx.send(devtunnel::fetch_rows(&loc));
    });
}

/// Runs a mutating CLI operation on a background thread, then refreshes the list.
/// The op's success feeds straight into `fetch_rows`, so the same `apply_rows`
/// path reconciles the UI (and tray) from the service after every mutation.
fn run_op_async<F>(
    weak: &slint::Weak<AppWindow>,
    tx: &Sender<anyhow::Result<Vec<devtunnel::Row>>>,
    status_key: &str,
    loc: &Rc<Locale>,
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
        let _ = tx.send(result);
    });
}

/// Applies a load result: fills the list and rebuilds the tray menu.
/// Always runs on the UI thread (called by the timer).
fn apply_rows(
    weak: &slint::Weak<AppWindow>,
    tray: &tray_icon::TrayIcon,
    actions: &Rc<RefCell<HashMap<MenuId, Action>>>,
    result: anyhow::Result<Vec<devtunnel::Row>>,
    loc: &Rc<Locale>,
) {
    let Some(app) = weak.upgrade() else { return };
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

            let model: Vec<PortRow> = rows
                .into_iter()
                .map(|r| PortRow {
                    group: r.group.into(),
                    tunnel_id: r.tunnel_id.into(),
                    port: r.port,
                    protocol: r.protocol.into(),
                    url: r.url.into(),
                    expiration: r.expiration.into(),
                    // Real status comes in slice #4 (health probe). For now: idle.
                    status: "idle".into(),
                })
                .collect();

            // Rebuild the tray menu with per-port actions from the same data.
            let menu = build_tray_menu(&model, &mut actions.borrow_mut(), loc);
            tray.set_menu(Some(Box::new(menu)));

            app.set_rows(ModelRc::new(VecModel::from(model)));
            let mut args = FluentArgs::new();
            args.set("count", count as i64);
            app.set_status(loc.t_args("status-port-count", &args).into());
        }
        Err(e) => {
            let mut args = FluentArgs::new();
            args.set("message", e.to_string());
            app.set_status(loc.t_args("status-error", &args).into());
        }
    }
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
    s.set_no_url(loc.t("no-url").into());
    s.set_expires_label(loc.t("expires-label").into());
    s.set_btn_copy(loc.t("btn-copy").into());
    s.set_btn_open(loc.t("btn-open").into());
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
