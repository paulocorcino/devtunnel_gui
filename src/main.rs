// Esconde o console no Windows em release (app de tray).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod devtunnel;
mod model;

slint::include_modules!();

use slint::{CloseRequestResponse, ComponentHandle, ModelRc, VecModel};
use std::time::Duration;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    Icon, TrayIconBuilder, TrayIconEvent,
};

fn main() -> anyhow::Result<()> {
    let app = AppWindow::new()?;

    // ---- Tray icon + menu ----
    let menu = Menu::new();
    let show_item = MenuItem::new("Abrir janela", true, None);
    let quit_item = MenuItem::new("Sair", true, None);
    menu.append(&show_item)?;
    menu.append(&quit_item)?;
    let show_id = show_item.id().clone();
    let quit_id = quit_item.id().clone();

    // Mantido vivo durante toda a execução (drop = some do tray).
    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("DevTunnel GUI")
        .with_icon(build_icon())
        .build()?;

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

    // ---- Callbacks de UI ----
    app.on_copy_url(|url| {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            let _ = cb.set_text(url.to_string());
        }
    });
    app.on_open_url(|url| {
        let _ = open::that(url.to_string());
    });
    {
        let weak = app.as_weak();
        app.on_refresh(move || load_async(weak.clone()));
    }

    // ---- Bombeia eventos do tray no loop do Slint via Timer ----
    let menu_rx = MenuEvent::receiver();
    let tray_rx = TrayIconEvent::receiver();
    let timer = slint::Timer::default();
    {
        let weak = app.as_weak();
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(150), move || {
            while let Ok(ev) = menu_rx.try_recv() {
                if ev.id == quit_id {
                    let _ = slint::quit_event_loop();
                } else if ev.id == show_id {
                    show_window(&weak);
                }
            }
            while let Ok(ev) = tray_rx.try_recv() {
                if let TrayIconEvent::Click { .. } = ev {
                    show_window(&weak);
                }
            }
        });
    }

    // ---- Carga inicial ----
    load_async(app.as_weak());

    app.run()?;
    Ok(())
}

fn show_window(weak: &slint::Weak<AppWindow>) {
    if let Some(a) = weak.upgrade() {
        let _ = a.show();
    }
}

/// Busca os dados numa thread de fundo e empurra para a UI.
fn load_async(weak: slint::Weak<AppWindow>) {
    if let Some(a) = weak.upgrade() {
        a.set_status("atualizando…".into());
    }
    std::thread::spawn(move || {
        let result = devtunnel::fetch_rows();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(app) = weak.upgrade() else { return };
            match result {
                Ok(rows) => {
                    let count = rows.len();
                    let model: Vec<PortRow> = rows
                        .into_iter()
                        .map(|r| PortRow {
                            group: r.group.into(),
                            port: r.port,
                            protocol: r.protocol.into(),
                            url: r.url.into(),
                            expiration: r.expiration.into(),
                        })
                        .collect();
                    app.set_rows(ModelRc::new(VecModel::from(model)));
                    app.set_status(format!("{count} porta(s)").into());
                }
                Err(e) => app.set_status(format!("erro: {e}").into()),
            }
        });
    });
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
