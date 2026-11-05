// No console window in release builds; debug builds keep it for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod instance;
mod logger;
mod tray;

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use gpui_kit::component::Theme;
use gpui_kit::{AnyWindowHandle, App, QuitMode};
use minato_config::{Action, Config, ConfigWatcher};
use minato_ui::UiContext;
use minato_wm::{UiRequest, WmMsg};

use crate::tray::{Tray, TrayCommand};

/// Messages for the GPUI main thread.
pub enum AppEvent {
    OpenSettings,
    /// The WM restored every window and stopped.
    WmStopped,
    Tray(TrayCommand),
}

fn main() {
    logger::init();
    if let Err(e) = run() {
        log::error!("fatal: {e:#}");
    }
}

fn run() -> anyhow::Result<()> {
    minato_wm::win32::set_dpi_awareness();
    if let instance::Instance::Secondary = instance::acquire() {
        log::info!("already running; asked the existing instance to open its settings");
        return Ok(());
    }
    log::info!("MinatoWM {} starting", env!("CARGO_PKG_VERSION"));

    let path = minato_config::config_path();
    // A broken config must not stop the WM, and must not be overwritten.
    let (cfg, load_error) = match minato_config::load_or_create(&path) {
        Ok(cfg) => (cfg, None),
        Err(e) => (Config::default(), Some(format!("{e:#}"))),
    };
    let latest = Arc::new(ArcSwap::from_pointee(cfg.clone()));

    let (app_tx, app_rx) = crossbeam_channel::unbounded::<AppEvent>();
    let ui_tx = app_tx.clone();
    let mut wm = minato_wm::start(cfg, move |req| {
        let _ = ui_tx.send(match req {
            UiRequest::OpenSettings => AppEvent::OpenSettings,
            UiRequest::Quit => AppEvent::WmStopped,
        });
    })?;
    if let Some(e) = load_error {
        log::error!("{e}");
        wm.send(WmMsg::ConfigError(e));
    }

    let wm_tx = wm.sender();
    let watched = latest.clone();
    let _watcher = ConfigWatcher::spawn(path.clone(), move |result| match result {
        Ok(cfg) => {
            log::info!("config.json changed");
            let cfg = Arc::new(cfg);
            watched.store(cfg.clone());
            let _ = wm_tx.send(WmMsg::Config(cfg));
        }
        Err(e) => {
            log::warn!("config.json is invalid: {e}");
            let _ = wm_tx.send(WmMsg::ConfigError(e));
        }
    })?;

    let open_tx = app_tx.clone();
    instance::listen(move || {
        let _ = open_tx.send(AppEvent::OpenSettings);
    });

    let ui = UiContext {
        config_path: path,
        latest_config: latest,
        snapshot: wm.snapshot.clone(),
        wm: wm.sender(),
    };
    let wm_tx = wm.sender();
    let snapshot = wm.snapshot.clone();

    gpui_kit::application()
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            Theme::sync_system_appearance(None, cx);

            let tray = match Tray::create(app_tx.clone(), autostart::is_enabled()) {
                Ok(t) => Some(t),
                Err(e) => {
                    log::error!("tray icon: {e:#}");
                    None
                }
            };
            let first_run = !minato_config::config_dir().join(".seen").exists();
            if first_run {
                let _ = std::fs::write(minato_config::config_dir().join(".seen"), "");
                let _ = app_tx.send(AppEvent::OpenSettings);
            }

            let mut settings: Option<AnyWindowHandle> = None;
            let mut bars = minato_ui::bar::Bars::default();
            let mut shown_paused = false;
            cx.spawn(async move |cx| loop {
                cx.background_executor().timer(Duration::from_millis(50)).await;
                while let Ok(ev) = app_rx.try_recv() {
                    let action = |a: Action| {
                        let _ = wm_tx.send(WmMsg::Action(a));
                    };
                    match ev {
                        AppEvent::OpenSettings | AppEvent::Tray(TrayCommand::OpenSettings) => {
                            cx.update(|cx| open_or_focus(&mut settings, &ui, cx));
                        }
                        AppEvent::Tray(TrayCommand::TogglePause) => action(Action::TogglePause),
                        AppEvent::Tray(TrayCommand::Retile) => action(Action::Retile),
                        AppEvent::Tray(TrayCommand::ToggleAutostart) => {
                            if let Err(e) = autostart::set_enabled(!autostart::is_enabled()) {
                                log::error!("autostart: {e:#}");
                            }
                            if let Some(t) = &tray {
                                t.set_autostart(autostart::is_enabled());
                            }
                        }
                        AppEvent::Tray(TrayCommand::Quit) => action(Action::Quit),
                        AppEvent::WmStopped => {
                            log::info!("window manager stopped; exiting");
                            cx.update(|cx| {
                                bars.close_all(cx);
                                cx.quit();
                            });
                            return;
                        }
                    }
                }
                cx.update(|cx| bars.sync(&ui, cx));
                let paused = snapshot.load().paused;
                if paused != shown_paused {
                    shown_paused = paused;
                    if let Some(t) = &tray {
                        t.set_paused(paused);
                    }
                }
            })
            .detach();
        });

    // Normally the WM already stopped (that is what quits the app); this
    // also covers the app exiting any other way.
    wm.stop();
    Ok(())
}

fn open_or_focus(settings: &mut Option<AnyWindowHandle>, ui: &UiContext, cx: &mut App) {
    if let Some(handle) = settings {
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            return;
        }
    }
    match minato_ui::open_settings(ui.clone(), cx) {
        Ok(handle) => {
            *settings = Some(handle);
        }
        Err(e) => log::error!("opening settings: {e:#}"),
    }
}
