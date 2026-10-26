//! Icon in the system tray (notification area, menu bar) with a small menu.
//! On Linux it goes through D-Bus (StatusNotifierItem, `ksni`), without GTK:
//! KDE and most panels show it, GNOME needs the AppIndicator extension.

use crate::i18n::{tr, Lang, Msg};
use std::sync::{Mutex, Once};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    ToggleHud,
    TogglePause,
    ToggleAutostart,
    Backups,
    Stats,
    Settings,
    /// Check for a new version, or install the one found.
    Update,
    Quit,
}

enum Raw {
    Menu(MenuId),
    LeftClick,
}

/// Events arrive on other threads; the handlers can be set only once per
/// process, so they feed this queue and wake the UI.
static QUEUE: Mutex<Vec<Raw>> = Mutex::new(Vec::new());
static WAKE: Mutex<Option<egui::Context>> = Mutex::new(None);
static HANDLERS: Once = Once::new();

fn push(raw: Raw) {
    if let Ok(mut q) = QUEUE.lock() {
        q.push(raw);
    }
    if let Some(ctx) = WAKE.lock().ok().and_then(|c| c.clone()) {
        ctx.request_repaint();
    }
}

/// Warning dot (orange with a dark rim) in the bottom right corner of an RGBA icon.
fn with_alert_dot(mut rgba: Vec<u8>, size: usize) -> Vec<u8> {
    let c = size as f32 * 0.74;
    let r = size as f32 * 0.24;
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 + 0.5 - c).powi(2) + (y as f32 + 0.5 - c).powi(2)).sqrt();
            let px = &mut rgba[(y * size + x) * 4..][..4];
            if d <= r * 0.72 {
                px.copy_from_slice(&[255, 140, 0, 255]);
            } else if d <= r {
                px.copy_from_slice(&[30, 30, 30, 255]);
            }
        }
    }
    rgba
}

/// Visible, paused, autostart, new version, why the keyboard is missing.
type Shown = (bool, bool, bool, Option<String>, Option<String>);

pub struct Tray {
    _icon: TrayIcon,
    show: MenuItem,
    pause: CheckMenuItem,
    autostart: CheckMenuItem,
    backups: MenuItem,
    stats: MenuItem,
    settings: MenuItem,
    update: MenuItem,
    quit: MenuItem,
    /// Last state shown, to touch the menu only on changes.
    shown: Option<Shown>,
    icon: Icon,
    icon_alert: Icon,
    lang: Lang,
}

impl Tray {
    pub fn new(ctx: &egui::Context, lang: Lang) -> Result<Self, String> {
        if let Ok(mut w) = WAKE.lock() {
            *w = Some(ctx.clone());
        }
        HANDLERS.call_once(|| {
            MenuEvent::set_event_handler(Some(|e: MenuEvent| push(Raw::Menu(e.id))));
            TrayIconEvent::set_event_handler(Some(|e: TrayIconEvent| {
                if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                    push(Raw::LeftClick);
                }
            }));
        });
        let t = |m| tr(lang, m);
        let title = MenuItem::new(format!("Keyboard HUD {}", env!("CARGO_PKG_VERSION")), false, None);
        let show = MenuItem::new(t(Msg::TrayHide), true, None);
        let pause = CheckMenuItem::new(t(Msg::MenuPause), true, false, None);
        let autostart = CheckMenuItem::new(t(Msg::MenuAutostart), true, false, None);
        let backups = MenuItem::new(t(Msg::MenuBackups), true, None);
        let stats = MenuItem::new(t(Msg::MenuStats), true, None);
        let settings = MenuItem::new(format!("{}…", t(Msg::SettingsTitle)), true, None);
        let update = MenuItem::new(format!("{}…", t(Msg::MenuCheckUpdates)), true, None);
        let quit = MenuItem::new(t(Msg::MenuQuit), true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &title,
            &PredefinedMenuItem::separator(),
            &show,
            &pause,
            &PredefinedMenuItem::separator(),
            &backups,
            &stats,
            &settings,
            &autostart,
            &update,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .map_err(|e| e.to_string())?;
        let rgba = include_bytes!("../packaging/icon/icon-64.rgba").to_vec();
        let icon = Icon::from_rgba(rgba.clone(), 64, 64).map_err(|e| e.to_string())?;
        let icon_alert = Icon::from_rgba(with_alert_dot(rgba, 64), 64, 64).map_err(|e| e.to_string())?;
        let tray = TrayIconBuilder::new()
            .with_tooltip("Keyboard HUD")
            .with_icon(icon.clone())
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Tray { _icon: tray, show, pause, autostart, backups, stats, settings, update, quit, shown: None, icon, icon_alert, lang })
    }

    /// Actions requested since the last call.
    pub fn poll(&self) -> Vec<Action> {
        let raw = QUEUE.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        raw.into_iter()
            .filter_map(|r| match r {
                Raw::LeftClick => Some(Action::ToggleHud),
                Raw::Menu(id) => [
                    (self.show.id(), Action::ToggleHud),
                    (self.pause.id(), Action::TogglePause),
                    (self.autostart.id(), Action::ToggleAutostart),
                    (self.backups.id(), Action::Backups),
                    (self.stats.id(), Action::Stats),
                    (self.settings.id(), Action::Settings),
                    (self.update.id(), Action::Update),
                    (self.quit.id(), Action::Quit),
                ]
                .into_iter()
                .find(|(i, _)| **i == id)
                .map(|(_, a)| a),
            })
            .collect()
    }

    /// Mirrors the app state in the menu; `update` is a new version found.
    /// `alert` is why the keyboard is missing: the icon gets a warning dot.
    pub fn sync(&mut self, visible: bool, paused: bool, autostart: bool, update: Option<&str>, alert: Option<&str>) {
        let now = (visible, paused, autostart, update.map(str::to_owned), alert.map(str::to_owned));
        if self.shown.as_ref() == Some(&now) {
            return;
        }
        self.update.set_text(match update {
            Some(v) => crate::tr_format!("Aggiorna alla versione {v}", "Update to version {v}"),
            None => format!("{}…", tr(self.lang, Msg::MenuCheckUpdates)),
        });
        let _ = self._icon.set_icon(Some(if alert.is_some() { self.icon_alert.clone() } else { self.icon.clone() }));
        let _ = self._icon.set_tooltip(Some(match alert {
            Some(why) => format!("Keyboard HUD · {why}"),
            None => "Keyboard HUD".to_string(),
        }));
        self.shown = Some(now);
        self.show.set_text(tr(self.lang, if visible { Msg::TrayHide } else { Msg::TrayShow }));
        self.pause.set_checked(paused);
        self.autostart.set_checked(autostart);
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        if let Ok(mut w) = WAKE.lock() {
            *w = None;
        }
    }
}
