// Esconde o console no Windows em release (app de tray).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod devtunnel;
mod model;

slint::include_modules!();

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

/// O que um item de menu do tray faz quando clicado. O mapa `MenuId -> Action`
/// é reconstruído a cada carga, já que os itens por porta dependem dos dados.
enum Action {
    Show,
    Quit,
    Copy(String),
    Open(String),
}

fn main() -> anyhow::Result<()> {
    let app = AppWindow::new()?;

    // Mapa de ações do menu do tray (reconstruído a cada refresh). Vive só na
    // thread da UI — por isso `Rc`/`RefCell` bastam (nada cruza thread).
    let actions: Rc<RefCell<HashMap<MenuId, Action>>> = Rc::new(RefCell::new(HashMap::new()));

    // Menu inicial (sem portas ainda) + tray. Mantido vivo: drop = some do tray.
    let menu = build_tray_menu(&[], &mut actions.borrow_mut());
    let tray = Rc::new(
        TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("DevTunnel GUI")
            .with_icon(build_icon())
            .build()?,
    );

    // ---- Fechar no X esconde para o tray (não encerra) ----
    {
        let weak = app.as_weak();
        app.window().on_close_requested(move || {
            if let Some(a) = weak.upgrade() {
                let _ = a.hide();
            }
            CloseRequestResponse::HideWindow
        });
    }

    // ---- Carga: thread de fundo busca os dados e os envia pela `Sender`.
    // A thread da UI (timer) é quem aplica na janela e reconstrói o menu do tray,
    // mantendo objetos não-`Send` (tray, app) fora da thread de fundo.
    let (tx, rx) = std::sync::mpsc::channel::<anyhow::Result<Vec<devtunnel::Row>>>();

    // ---- Callbacks de UI ----
    app.on_copy_url(|url| copy(&url));
    app.on_open_url(|url| open_browser(&url));
    {
        let weak = app.as_weak();
        let tx = tx.clone();
        app.on_refresh(move || load_async(&weak, &tx));
    }

    // ---- Bombeia eventos (tray + carga) no loop do Slint via Timer ----
    let menu_rx = MenuEvent::receiver();
    let tray_rx = TrayIconEvent::receiver();
    let timer = slint::Timer::default();
    {
        let weak = app.as_weak();
        let tray = tray.clone();
        let actions = actions.clone();
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(150), move || {
            // Cliques no menu do tray.
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
            // Clique no ícone do tray: alterna a janela.
            while let Ok(ev) = tray_rx.try_recv() {
                if let TrayIconEvent::Click { .. } = ev {
                    toggle_window(&weak);
                }
            }
            // Resultado de uma carga: aplica na UI e reconstrói o menu do tray.
            while let Ok(result) = rx.try_recv() {
                apply_rows(&weak, &tray, &actions, result);
            }
        });
    }

    // ---- Carga inicial ----
    load_async(&app.as_weak(), &tx);

    // Inicia minimizado no tray: nunca chamamos `show()`, só rodamos o loop.
    // A janela aparece pelo clique no ícone ou pelo menu "Abrir janela".
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

/// Dispara a busca numa thread de fundo; o resultado volta pela `Sender`.
fn load_async(weak: &slint::Weak<AppWindow>, tx: &Sender<anyhow::Result<Vec<devtunnel::Row>>>) {
    if let Some(a) = weak.upgrade() {
        a.set_status("atualizando…".into());
    }
    let tx = tx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(devtunnel::fetch_rows());
    });
}

/// Aplica o resultado de uma carga: preenche a lista e reconstrói o menu do tray.
/// Roda sempre na thread da UI (chamado pelo timer).
fn apply_rows(
    weak: &slint::Weak<AppWindow>,
    tray: &tray_icon::TrayIcon,
    actions: &Rc<RefCell<HashMap<MenuId, Action>>>,
    result: anyhow::Result<Vec<devtunnel::Row>>,
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
                    // Status real chega na #4 (sonda). Por ora: idle.
                    status: "idle".into(),
                })
                .collect();

            // Reconstrói o menu do tray com ações por porta a partir dos mesmos dados.
            let menu = build_tray_menu(&model, &mut actions.borrow_mut());
            tray.set_menu(Some(Box::new(menu)));

            app.set_rows(ModelRc::new(VecModel::from(model)));
            app.set_status(format!("{count} porta(s)").into());
        }
        Err(e) => app.set_status(format!("erro: {e}").into()),
    }
}

/// Monta o menu do tray: "Abrir janela", um submenu por porta com URL
/// (Copiar / Abrir) e "Sair". Repovoa o mapa `MenuId -> Action`.
fn build_tray_menu(rows: &[PortRow], actions: &mut HashMap<MenuId, Action>) -> Menu {
    actions.clear();
    let menu = Menu::new();

    let show = MenuItem::new("Abrir janela", true, None);
    actions.insert(show.id().clone(), Action::Show);
    let _ = menu.append(&show);
    let _ = menu.append(&PredefinedMenuItem::separator());

    for r in rows {
        if r.url.is_empty() {
            continue;
        }
        let sub = Submenu::new(format!("{} :{}", r.group, r.port), true);
        let copy_it = MenuItem::new("Copiar URL", true, None);
        let open_it = MenuItem::new("Abrir no navegador", true, None);
        actions.insert(copy_it.id().clone(), Action::Copy(r.url.to_string()));
        actions.insert(open_it.id().clone(), Action::Open(r.url.to_string()));
        let _ = sub.append(&copy_it);
        let _ = sub.append(&open_it);
        let _ = menu.append(&sub);
    }

    let _ = menu.append(&PredefinedMenuItem::separator());
    let quit = MenuItem::new("Sair", true, None);
    actions.insert(quit.id().clone(), Action::Quit);
    let _ = menu.append(&quit);

    menu
}

/// Ícone azul sólido 32x32 (sem arquivo de asset).
fn build_icon() -> Icon {
    const SIZE: u32 = 32;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for _ in 0..(SIZE * SIZE) {
        rgba.extend_from_slice(&[0x1e, 0x90, 0xff, 0xff]); // dodger blue
    }
    Icon::from_rgba(rgba, SIZE, SIZE).expect("ícone do tray inválido")
}
