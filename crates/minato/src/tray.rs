//! Notification-area icon and its menu. Must be created on the GPUI main
//! thread, whose message loop services the icon's hidden window.

use crossbeam_channel::Sender;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::AppEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    OpenSettings,
    TogglePause,
    Retile,
    ToggleAutostart,
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
    pause: CheckMenuItem,
    autostart: CheckMenuItem,
}

impl Tray {
    pub fn create(tx: Sender<AppEvent>, autostart_enabled: bool) -> anyhow::Result<Self> {
        let open = MenuItem::new("設定を開く", true, None);
        let pause = CheckMenuItem::new("一時停止", true, false, None);
        let retile = MenuItem::new("再タイル", true, None);
        let autostart = CheckMenuItem::new("Windows 起動時に開始", true, autostart_enabled, None);
        let quit = MenuItem::new("終了", true, None);

        let menu = Menu::new();
        menu.append(&open)?;
        menu.append(&pause)?;
        menu.append(&retile)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&autostart)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&quit)?;

        let ids = [
            (open.id().clone(), TrayCommand::OpenSettings),
            (pause.id().clone(), TrayCommand::TogglePause),
            (retile.id().clone(), TrayCommand::Retile),
            (autostart.id().clone(), TrayCommand::ToggleAutostart),
            (quit.id().clone(), TrayCommand::Quit),
        ];
        let menu_tx = tx.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            if let Some((_, cmd)) = ids.iter().find(|(id, _)| *id == e.id) {
                let _ = menu_tx.send(AppEvent::Tray(*cmd));
            }
        }));
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            let open = matches!(
                e,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } | TrayIconEvent::DoubleClick { .. }
            );
            if open {
                let _ = tx.send(AppEvent::Tray(TrayCommand::OpenSettings));
            }
        }));

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip("MinatoWM")
            .with_icon(icon(false))
            .build()?;
        Ok(Self {
            icon,
            pause,
            autostart,
        })
    }

    pub fn set_paused(&self, paused: bool) {
        self.pause.set_checked(paused);
        let _ = self.icon.set_icon(Some(icon(paused)));
        let _ = self
            .icon
            .set_tooltip(Some(if paused { "MinatoWM (一時停止中)" } else { "MinatoWM" }));
    }

    pub fn set_autostart(&self, enabled: bool) {
        self.autostart.set_checked(enabled);
    }
}

/// A 32×32 tile glyph: one tall tile and two stacked ones. Grey when paused.
fn icon(paused: bool) -> Icon {
    const S: usize = 32;
    let color: [u8; 4] = if paused {
        [0x90, 0x90, 0x98, 0xff]
    } else {
        [0x7a, 0xa2, 0xf7, 0xff]
    };
    let tiles = [(2, 2, 14, 30), (18, 2, 30, 14), (18, 18, 30, 30)];
    let mut rgba = vec![0u8; S * S * 4];
    for y in 0..S {
        for x in 0..S {
            let inside = tiles
                .iter()
                .any(|&(l, t, r, b)| x >= l && x < r && y >= t && y < b);
            if inside {
                rgba[(y * S + x) * 4..][..4].copy_from_slice(&color);
            }
        }
    }
    Icon::from_rgba(rgba, S as u32, S as u32).expect("valid icon")
}
