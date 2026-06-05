// Hide the console window on Windows in release builds (tray app).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod devtunnel;
mod locale;
mod model;

slint::include_modules!();

use fluent_bundle::FluentArgs;
use locale::Locale;
use slint::{CloseRequestResponse, ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;
use std::collections::HashMap;
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

    // ---- Pump events (tray + load results) into the Slint loop via Timer ----
    let menu_rx = MenuEvent::receiver();
    let tray_rx = TrayIconEvent::receiver();
    let timer = slint::Timer::default();
    {
        let weak = app.as_weak();
        let tray = tray.clone();
        let actions = actions.clone();
        let loc = loc.clone();
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(150), move || {
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
        });
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
fn load_async(weak: &slint::Weak<AppWindow>, tx: &Sender<anyhow::Result<Vec<devtunnel::Row>>>, loc: &Rc<Locale>) {
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
    s.set_no_url(loc.t("no-url").into());
    s.set_expires_label(loc.t("expires-label").into());
    s.set_btn_copy(loc.t("btn-copy").into());
    s.set_btn_open(loc.t("btn-open").into());
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
