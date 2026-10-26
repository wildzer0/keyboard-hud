//! egui application: overlay window, header with live indicators, keyboard
//! view, context menu.

pub mod diagnostics;
pub mod export;
pub mod frameless;
pub mod header;
pub mod history;
pub mod key_editor;
pub mod keyboard;
pub mod keycast;
pub mod marker;
pub mod report;
pub mod search_panel;
pub mod settings;
pub mod theme;
pub mod training;

use crate::autostart;
use crate::backdrop::ScreenRect;
use crate::backup;
use crate::changes;
use crate::config::{
    Config, ConfigWatcher, LanguageChoice, Reload, ShownIn, StatsConfig, ThemeChoice, ViewMode, WindowState,
};
use crate::config_edit::{self, Edit};
use crate::headsup::{self, HeadsUp};
use crate::hid::protocol::{caps, flags, HudInfo, HudState, OsKind};
use crate::hid::worker::{Event, KeyWrite, PauseReason, Status, Worker, WriteError};
use crate::i18n::{tr, Lang, Msg};
use crate::kle::{KeyRole, KleKey};
use crate::labels::Labels;
use crate::logging;
use crate::model::{classify_layers, highest_layer, Keymap};
use crate::paths;
use crate::screen;
use crate::stats::{Stats, Totals};
use crate::style::{self, Look, Style, StyleWatcher};
use crate::vial::definition::Definition;
use crate::vial::dynamic::Dynamic;
use crate::vial::keycode::Mods5;
use crate::vial::legend::LegendContext;
use egui::{Align2, Color32, FontId, Key, Rect, Sense, Vec2, ViewportCommand};
use report::{ReportInput, ReportWindow};
use search_panel::SearchPanel;
use settings::{SettingsInput, SettingsWindow};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use theme::{layer_table, Palette};

/// Window size at scale 1.0, per view mode.
pub fn base_size(mode: ViewMode) -> Vec2 {
    match mode {
        ViewMode::Full => Vec2::new(900.0, 400.0),
        ViewMode::Compact => Vec2::new(620.0, 230.0),
    }
}

/// The context menu lives in its own small window: inside the HUD it would
/// be clipped when the HUD is small (low scale, compact view).
const MENU_VIEWPORT: &str = "hud-menu";
/// Keycast window: id and size.
const KEYCAST_VIEWPORT: &str = "hud-keycast";
const KEYCAST_SIZE: Vec2 = Vec2::new(720.0, 56.0);

/// Mini indicator window: id and size (the panel inside fits its contents).
const INDICATOR_VIEWPORT: &str = "hud-indicator";
const INDICATOR_SIZE: Vec2 = Vec2::new(420.0, 32.0);

/// Menu size before the first measurement.
/// The menu window never changes size (resizing it while it draws flickers on
/// Windows): tabs keep it short.
const MENU_SIZE: Vec2 = Vec2::new(430.0, 470.0);
/// Width of the tab column in the menu.
const MENU_TABS_WIDTH: f32 = 96.0;

#[derive(Debug, Clone, Copy)]
struct MenuWindow {
    /// Click position on screen; the menu opens towards the inside of the HUD.
    /// `None` when the window system does not tell where the HUD is (Wayland):
    /// the compositor places the menu.
    anchor: Option<egui::Pos2>,
    left: bool,
    up: bool,
    focused_once: bool,
}

/// Tabs of the context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum MenuTab {
    #[default]
    Window,
    Style,
    Keys,
    Tools,
    General,
}

/// Commands for the HUD window, also when issued while drawing another window.
fn root_cmd(ctx: &egui::Context, cmd: ViewportCommand) {
    ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, cmd);
}

/// Position and size of the HUD window, also while drawing another window.
fn root_outer_rect(ctx: &egui::Context) -> Option<Rect> {
    ctx.input(|i| i.raw.viewports.get(&egui::ViewportId::ROOT).and_then(|v| v.outer_rect))
}

/// Work area of the monitor showing most of `window` (points): exact on
/// Windows, guessed from the monitor size elsewhere.
fn work_area(ctx: &egui::Context, window: Rect) -> Option<Rect> {
    let ppp = ctx.pixels_per_point();
    if let Some(a) = screen::monitor_work_area((window.center().to_vec2() * ppp).to_pos2()) {
        return Some(Rect::from_min_max((a.min.to_vec2() / ppp).to_pos2(), (a.max.to_vec2() / ppp).to_pos2()));
    }
    let monitor = ctx.input(|i| i.raw.viewports.get(&egui::ViewportId::ROOT).and_then(|v| v.monitor_size))?;
    screen::guess_monitor_area(window, monitor)
}

/// Grid of the quick positions in the menu, as (anchor, arrow).
const POSITIONS: [[&str; 3]; 3] = [["↖", "↑", "↗"], ["←", "•", "→"], ["↙", "↓", "↘"]];

/// Where a window drag started: cursor on screen (physical pixels) and window
/// position (points).
#[derive(Debug, Clone, Copy)]
struct WindowDrag {
    cursor: egui::Pos2,
    window: egui::Pos2,
}

/// Cursor position on screen, in physical pixels. Only on Windows, where
/// winit's `StartDrag` stays dead for good once Windows does not start its
/// move loop (button already released, or held still until egui calls it a
/// drag): there the HUD moves its window by itself.
#[cfg(windows)]
fn cursor_on_screen() -> Option<egui::Pos2> {
    let mut p = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
    // SAFETY: GetCursorPos only writes the POINT it is given
    let ok = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut p) } != 0;
    ok.then(|| egui::pos2(p.x as f32, p.y as f32))
}

#[cfg(not(windows))]
fn cursor_on_screen() -> Option<egui::Pos2> {
    None
}

/// The heads-up marker fades in this fast once the HUD is gone.
const MARKER_FADE: Duration = Duration::from_millis(250);
/// Fn1 and Fn2 in the marker when their layers have no colour (the icon's).
const MARKER_FN: [Color32; 2] = [Color32::from_rgb(0x00, 0xB9, 0xD7), Color32::from_rgb(0x3C, 0xC8, 0x5A)];

/// Statistics are counted from polled states: below this rate quick taps get lost.
const MIN_STATS_HZ: u32 = 30;
const HEAT_REFRESH: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeatMode {
    Off,
    /// Presses on the layer shown.
    Layer,
    /// Presses per physical key, all layers together.
    All,
}

fn resolve_language(choice: LanguageChoice) -> Lang {
    match choice {
        LanguageChoice::Auto => crate::i18n::system(),
        LanguageChoice::It => Lang::It,
        LanguageChoice::En => Lang::En,
    }
}

fn stats_path(cfg: &StatsConfig) -> PathBuf {
    if cfg.file.trim().is_empty() {
        paths::base_dir().join("keyboard-hud-stats.json")
    } else {
        PathBuf::from(cfg.file.trim())
    }
}

/// Matrix positions present in the geometry.
fn positions(keys: &[KleKey]) -> Vec<(u8, u8)> {
    let mut p: Vec<(u8, u8)> = keys
        .iter()
        .filter_map(|k| match k.role() {
            Some(KeyRole::Matrix { row, col }) => Some((row, col)),
            _ => None,
        })
        .collect();
    p.sort();
    p.dedup();
    p
}

/// Press animation: instant on, fade out over this time after release.
const FADE: Duration = Duration::from_millis(200);
/// Two HUD_TOG taps within this time are a double tap.
const DOUBLE_TAP: Duration = Duration::from_millis(300);
const NOTICE_TIME: Duration = Duration::from_secs(4);
/// How long a change stays in the mini indicator.
const FLASH_TIME: Duration = Duration::from_secs(2);
const SAVE_DELAY: Duration = Duration::from_secs(1);

/// DejaVu Sans as a fallback font: it has the compact key symbols
/// (⌫ ⏎ ⇥ ⇧ ⇪ ⌦ ␣ ⇄ ⟳) missing from egui's default fonts.
pub fn install_fonts(ctx: &egui::Context) {
    use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
    ctx.add_font(FontInsert::new(
        "dejavu-sans",
        egui::FontData::from_static(include_bytes!("../../data/fonts/DejaVuSans.ttf")),
        vec![InsertFontFamily { family: egui::FontFamily::Proportional, priority: FontPriority::Lowest }],
    ));
}

/// What is shown on screen: geometry + keymap, live or loaded from disk.
pub struct Layout {
    pub definition: Definition,
    pub keys: Vec<KleKey>,
    pub keymap: Keymap,
    pub dynamic: Dynamic,
}

impl Layout {
    pub fn new(definition: Definition, layout_options: u32, keymap: Keymap, dynamic: Dynamic) -> Self {
        let keys = definition.visible_keys(layout_options);
        Layout { definition, keys, keymap, dynamic }
    }

    pub fn legend_context<'a>(&'a self, layer_names: &'a [String]) -> LegendContext<'a> {
        LegendContext {
            layer_names,
            custom: &self.definition.custom,
            tap_dances: &self.dynamic.tap_dances,
            macros: &self.dynamic.macros,
        }
    }
}

pub enum Mode {
    /// Talking to a keyboard (real or simulated).
    Live { worker: Worker, mock: bool },
    /// Files from disk only.
    Offline,
}

/// Everything the app needs at startup besides the keyboard.
pub struct Startup {
    pub mode: Mode,
    pub offline: Option<Layout>,
    pub start_layer: u8,
    pub screenshot: Option<Screenshot>,
    pub config: Config,
    pub watcher: ConfigWatcher,
    pub config_error: Option<String>,
    pub window: WindowState,
    pub window_path: PathBuf,
    /// `--verbose` wins over the configured log level.
    pub verbose: bool,
    pub dev: DevOptions,
    /// Command line options repeated by the autostart entry.
    pub autostart_args: Vec<String>,
}

/// Development aids to reach a UI state without interaction (screenshots).
#[derive(Default)]
pub struct DevOptions {
    pub search: Option<String>,
    pub heatmap: Option<String>,
    pub stats_window: bool,
    pub diagnostics: bool,
    pub settings: bool,
    pub settings_page: Option<String>,
    pub menu: bool,
    /// Opens the key editor on `(layer, row, col)`, searching for the text.
    pub edit_key: Option<((u8, u8, u8), String)>,
    pub history: bool,
    /// Opens the training window and starts a round once the keyboard is ready.
    pub training: bool,
    /// Writes the keymap page to this file as soon as a layout is known, then quits.
    pub export: Option<PathBuf>,
}

/// Development aid: capture the window to a PPM file, then quit.
pub struct Screenshot {
    pub path: PathBuf,
    pub at: Instant,
    requested: bool,
}

impl Screenshot {
    pub fn new(path: PathBuf, delay: Duration) -> Self {
        Screenshot { path, at: Instant::now() + delay, requested: false }
    }
}

fn write_ppm(path: &std::path::Path, image: &egui::ColorImage) -> std::io::Result<()> {
    let [w, h] = image.size;
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    for p in &image.pixels {
        // composited on black: the window itself is transparent
        let [r, g, b, _] = p.to_array();
        out.extend_from_slice(&[r, g, b]);
    }
    std::fs::write(path, out)
}

/// Press highlight per matrix position.
#[derive(Default)]
struct PressFade {
    matrix: [u8; 8],
    released: [[Option<Instant>; 8]; 8],
}

impl PressFade {
    fn update(&mut self, matrix: &[u8; 8], now: Instant) {
        for ((old, new), times) in self.matrix.iter().zip(matrix).zip(self.released.iter_mut()) {
            let released = old & !new;
            for (c, t) in times.iter_mut().enumerate() {
                if released & (1 << c) != 0 {
                    *t = Some(now);
                }
            }
        }
        self.matrix = *matrix;
    }

    fn clear(&mut self) {
        *self = PressFade::default();
    }

    /// Keys pressed in `matrix` that were up in the previous state.
    fn new_presses(&self, matrix: &[u8; 8]) -> Vec<(u8, u8)> {
        let mut out = Vec::new();
        for (r, (old, new)) in self.matrix.iter().zip(matrix).enumerate() {
            let pressed = new & !old;
            for c in 0..8u8 {
                if pressed & (1 << c) != 0 {
                    out.push((r as u8, c));
                }
            }
        }
        out
    }

    fn level(&self, row: u8, col: u8, now: Instant) -> f32 {
        let (r, c) = (row as usize, col as usize);
        if r >= 8 || c >= 8 {
            return 0.0;
        }
        if self.matrix[r] & (1 << c) != 0 {
            return 1.0;
        }
        match self.released[r][c] {
            Some(t) => 1.0 - (now.duration_since(t).as_secs_f32() / FADE.as_secs_f32()).min(1.0),
            None => 0.0,
        }
    }

    fn animating(&self, now: Instant) -> bool {
        self.released.iter().flatten().flatten().any(|t| now.duration_since(*t) < FADE)
    }
}

/// HUD_TOG: one tap toggles the HUD, a double tap toggles click-through.
#[derive(Default)]
struct HudToggle {
    count: Option<u8>,
    pending: Option<Instant>,
}

enum TapAction {
    None,
    Single,
    Double,
}

impl HudToggle {
    fn on_count(&mut self, count: u8, now: Instant) -> TapAction {
        let Some(old) = self.count.replace(count) else { return TapAction::None };
        match count.wrapping_sub(old) {
            0 => TapAction::None,
            1 if self.pending.is_some_and(|t| now.duration_since(t) <= DOUBLE_TAP) => {
                self.pending = None;
                TapAction::Double
            }
            1 => {
                self.pending = Some(now);
                TapAction::None
            }
            _ => {
                // two taps between polls
                self.pending = None;
                TapAction::Double
            }
        }
    }

    /// A single tap is confirmed once the double-tap window has passed.
    fn poll(&mut self, now: Instant) -> TapAction {
        match self.pending {
            Some(t) if now.duration_since(t) > DOUBLE_TAP => {
                self.pending = None;
                TapAction::Single
            }
            _ => TapAction::None,
        }
    }

    fn reset(&mut self) {
        *self = HudToggle::default();
    }
}

/// Choices made from the menu (and Ctrl + wheel), saved in the config file.
#[derive(Debug, Clone, PartialEq)]
struct Prefs {
    opacity: f32,
    scale: f32,
    mode: ViewMode,
    theme: ThemeChoice,
    language: LanguageChoice,
    style: String,
    key_opacity: Option<f32>,
    key_radius: Option<f32>,
    headsup: bool,
    marker: bool,
    combos: bool,
    keycast: bool,
}

impl Prefs {
    /// What the config file says.
    fn from_config(c: &Config) -> Self {
        let a = &c.appearance;
        Prefs {
            opacity: a.opacity,
            scale: a.scale,
            mode: a.mode,
            theme: a.theme,
            language: a.language,
            style: if a.style.is_empty() { style::DEFAULT_STYLE.to_string() } else { a.style.clone() },
            key_opacity: a.key_opacity,
            key_radius: a.key_radius,
            headsup: c.headsup.enabled,
            marker: c.headsup.marker,
            combos: a.combos,
            keycast: c.keycast.enabled,
        }
    }

    /// The keys to write to go from `old` to `self`.
    fn edits(&self, old: &Prefs) -> Vec<Edit> {
        fn value<T: serde::Serialize>(v: &T) -> Option<toml::Value> {
            toml::Value::try_from(v).ok()
        }
        let mut out = Vec::new();
        let mut put = |changed: bool, key: &'static str, v: Option<toml::Value>| {
            if changed {
                out.push(Edit::Key(key, v));
            }
        };
        put(self.opacity != old.opacity, "appearance.opacity", value(&self.opacity));
        put(self.scale != old.scale, "appearance.scale", value(&self.scale));
        put(self.mode != old.mode, "appearance.mode", value(&self.mode));
        put(self.theme != old.theme, "appearance.theme", value(&self.theme));
        put(self.language != old.language, "appearance.language", value(&self.language));
        put(self.style != old.style, "appearance.style", value(&self.style));
        put(self.key_opacity != old.key_opacity, "appearance.key_opacity", self.key_opacity.and_then(|v| value(&v)));
        put(self.key_radius != old.key_radius, "appearance.key_radius", self.key_radius.and_then(|v| value(&v)));
        put(self.combos != old.combos, "appearance.combos", value(&self.combos));
        put(self.headsup != old.headsup, "headsup.enabled", value(&self.headsup));
        put(self.marker != old.marker, "headsup.marker", value(&self.marker));
        put(self.keycast != old.keycast, "keycast.enabled", value(&self.keycast));
        out
    }
}

pub struct HudApp {
    lang: Lang,
    language: LanguageChoice,
    mode: Mode,
    status: Option<Status>,
    live: Option<Layout>,
    info: Option<HudInfo>,
    offline: Option<Layout>,
    state: Option<HudState>,
    browse_layer: u8,
    layer_names: Vec<String>,
    layer_colors: Vec<Option<Color32>>,

    config: Config,
    watcher: ConfigWatcher,
    /// The config file does not parse (the last config read stays in use).
    config_error: Option<String>,
    /// Grows whenever the config file is read or written.
    config_revision: u64,
    /// Menu choices as last written to (or read from) the config file.
    synced: Prefs,
    verbose: bool,
    window: WindowState,
    window_path: PathBuf,
    window_dirty: Option<Instant>,

    opacity: f32,
    scale: f32,
    /// Style in use, its file watched for changes.
    style_name: String,
    style: Style,
    style_watcher: StyleWatcher,
    /// Style names, read when the menu opens.
    style_names: Vec<String>,
    /// Unset = from the style.
    key_opacity: Option<f32>,
    key_radius: Option<f32>,
    view: ViewMode,
    theme: ThemeChoice,
    dark: Option<bool>,
    click_through: bool,
    visible: bool,

    fade: PressFade,
    hud_toggle: HudToggle,
    notice: Option<(String, Instant)>,
    /// Change shown for a moment in the mini indicator.
    flash: Option<(String, Instant)>,
    /// The other half of the split keyboard answered in this session: from
    /// then on a missing half is a warning (keyboards that are not split
    /// never set the flag).
    split_seen: bool,
    screenshot: Option<Screenshot>,
    started: Instant,

    labels: Labels,
    headsup: HeadsUp,
    headsup_on: bool,
    /// Heads-up marker shown while the HUD is faded out.
    marker_on: bool,
    /// Since when the marker is shown (it fades in).
    marker_since: Option<Instant>,
    /// Light or dark background behind the marker.
    backdrop: crate::backdrop::Backdrop,
    /// A full screen window is in the foreground and the HUD hides for it.
    fullscreen_hidden: bool,
    updater: crate::update::Updater,
    /// The update state already told to the user.
    update_seen: crate::update::State,
    /// Combos drawn over the keys.
    show_combos: bool,
    keycast_on: bool,
    keycast: keycast::Keycast,
    /// Foreground application, for per-application labels.
    foreground: crate::foreground::Foreground,
    tray: Option<crate::tray::Tray>,
    autostart_args: Vec<String>,
    /// Autostart entry, read when the menu opens (error text if unreadable).
    autostart: Result<autostart::State, String>,
    /// Heads-up mode: HUD_TOG pinned the HUD on screen.
    pinned: bool,
    /// Mouse passthrough currently requested from the window system.
    passthrough: bool,
    /// Window drag moved by the HUD itself (Windows).
    drag: Option<WindowDrag>,
    /// The remembered position was checked against the screens.
    checked_on_screen: bool,
    search: SearchPanel,
    search_stale: bool,
    stats: Option<Stats>,
    report: ReportWindow,
    diagnostics: diagnostics::DiagnosticsWindow,
    settings: SettingsWindow,
    /// Last backup written in this session, for the diagnostics.
    last_backup: Option<(PathBuf, Instant)>,
    heat: HeatMode,
    heat_cache: Option<(Instant, Totals)>,
    menu: Option<MenuWindow>,
    /// Tab shown in the menu, kept while the app runs.
    menu_tab: MenuTab,
    /// Key under the pointer when the menu was opened: `(layer, row, col)`.
    menu_key: Option<(u8, u8, u8)>,
    key_editor: key_editor::KeyEditor,
    history: history::HistoryWindow,
    training: training::TrainingWindow,
    /// Last key written from the HUD, with the keycode it replaced (for undo).
    last_write: Option<(KeyWrite, u16)>,
    /// Undo sent, answer not come yet.
    undoing: Option<KeyWrite>,
    /// `--menu`: open the menu as soon as the window position is known.
    dev_menu: bool,
    /// `--training`: start a round as soon as the keyboard is ready.
    dev_training: bool,
    /// `--export`: write the keymap page once a layout is known, then quit.
    dev_export: Option<PathBuf>,
    /// `--edit-key`: open the key editor once the keyboard is connected.
    dev_edit_key: Option<((u8, u8, u8), String)>,
}

impl HudApp {
    pub fn new(ctx: &egui::Context, s: Startup) -> Self {
        let a = &s.config.appearance;
        let (layer_names, layer_colors) = layer_table(&s.config.layers);
        let (labels, label_errors) = Labels::from_config(&s.config.labels);
        let stats = s.config.stats.enabled.then(|| Stats::load(stats_path(&s.config.stats)));
        let headsup_on = s.window.headsup.unwrap_or(s.config.headsup.enabled);
        let marker_on = s.config.headsup.marker;
        let show_combos = s.window.combos.unwrap_or(a.combos);
        let keycast_on = s.window.keycast.unwrap_or(s.config.keycast.enabled);
        let language = s.window.language.unwrap_or(s.config.appearance.language);
        let lang = resolve_language(language);
        crate::i18n::set_current(lang);
        let style_name = s
            .window
            .style
            .as_deref()
            .map(|n| style::current_name(n).to_string())
            .or_else(|| Some(a.style.clone()).filter(|n| !n.is_empty()))
            .unwrap_or_else(|| style::DEFAULT_STYLE.to_string());
        let (style_name, style, style_error) = match style::load(&style_name) {
            Ok(st) => (style_name, st, None),
            Err(e) => {
                (style::DEFAULT_STYLE.to_string(), style::load(style::DEFAULT_STYLE).unwrap_or_default(), Some(e))
            }
        };
        let mut app = HudApp {
            lang,
            language,
            mode: s.mode,
            status: None,
            live: None,
            info: None,
            offline: s.offline,
            state: None,
            browse_layer: s.start_layer,
            layer_names,
            layer_colors,
            opacity: s.window.opacity.unwrap_or(a.opacity),
            scale: s.window.scale.unwrap_or(a.scale),
            style_watcher: StyleWatcher::new(&style_name),
            style_name,
            style,
            style_names: Vec::new(),
            key_opacity: s.window.key_opacity.or(a.key_opacity),
            key_radius: s.window.key_radius.or(a.key_radius),
            view: s.window.mode.unwrap_or(a.mode),
            theme: s.window.theme.unwrap_or(a.theme),
            synced: Prefs::from_config(&s.config),
            config: s.config,
            watcher: s.watcher,
            config_error: None,
            config_revision: 0,
            verbose: s.verbose,
            window: s.window,
            window_path: s.window_path,
            window_dirty: None,
            dark: None,
            click_through: false,
            visible: true,
            fade: PressFade::default(),
            hud_toggle: HudToggle::default(),
            notice: None,
            flash: None,
            split_seen: false,
            screenshot: s.screenshot,
            started: Instant::now(),
            labels,
            headsup: HeadsUp::default(),
            headsup_on,
            marker_on,
            marker_since: None,
            backdrop: crate::backdrop::Backdrop::new(),
            fullscreen_hidden: false,
            updater: crate::update::Updater::new(),
            update_seen: crate::update::State::Idle,
            show_combos,
            keycast_on,
            keycast: keycast::Keycast::default(),
            foreground: crate::foreground::Foreground::new(),
            tray: None,
            autostart_args: s.autostart_args,
            autostart: Ok(autostart::State::Off),
            pinned: false,
            passthrough: false,
            drag: None,
            checked_on_screen: false,
            search: SearchPanel::default(),
            search_stale: true,
            stats,
            report: ReportWindow::default(),
            diagnostics: diagnostics::DiagnosticsWindow::default(),
            settings: SettingsWindow::default(),
            last_backup: None,
            heat: HeatMode::Off,
            heat_cache: None,
            menu: None,
            menu_tab: MenuTab::default(),
            menu_key: None,
            key_editor: key_editor::KeyEditor::default(),
            history: history::HistoryWindow::default(),
            training: training::TrainingWindow::default(),
            last_write: None,
            undoing: None,
            dev_menu: s.dev.menu,
            dev_edit_key: s.dev.edit_key.clone(),
            dev_export: s.dev.export.clone(),
            dev_training: s.dev.training,
        };
        // what the file says, except where an older version remembered a menu
        // choice in the state file: that is moved into the config at the first save
        let legacy = app.window.has_menu_choices();
        app.synced = if legacy { Prefs::from_config(&app.config) } else { app.prefs() };
        if legacy {
            app.mark_dirty();
        }
        app.report_label_errors(&label_errors);
        if let Some(q) = s.dev.search {
            app.search.open_with(q);
        }
        app.heat = match s.dev.heatmap.as_deref() {
            Some("layer") => HeatMode::Layer,
            Some("all") => HeatMode::All,
            _ => HeatMode::Off,
        };
        app.report.open = s.dev.stats_window;
        app.diagnostics.open = s.dev.diagnostics;
        if s.dev.history {
            app.history.open(&app.backup_dir());
        }
        app.training.open = s.dev.training;
        if s.dev.settings {
            app.open_settings();
            app.settings.open_page(s.dev.settings_page.as_deref());
        }
        if (s.dev.stats_window
            || s.dev.diagnostics
            || s.dev.settings
            || s.dev.menu
            || s.dev.edit_key.is_some()
            || s.dev.history)
            && app.screenshot.is_some()
        {
            // capture the other windows inside the HUD window
            ctx.set_embed_viewports(true);
        }
        if let Some(e) = s.config_error {
            app.notify(format!("{}: {e}", tr(app.lang, Msg::ConfigError)));
            app.config_error = Some(e);
        }
        if let Some(e) = style_error {
            app.notify(e);
        }
        app.apply_backup();
        app.refresh_autostart();
        // no tray icon for unattended screenshots
        if app.screenshot.is_none() {
            app.apply_tray(ctx);
        }
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        app.apply_poll_rate();
        app
    }

    fn report_label_errors(&mut self, errors: &[String]) {
        for e in errors {
            log::warn!("{e}");
        }
        if let Some(first) = errors.first() {
            self.notify(format!("{} ({}): {first}", tr(self.lang, Msg::LabelErrors), errors.len()));
        }
    }

    fn open_url(&mut self, url: &str) {
        if let Err(e) = backup::open_path(Path::new(url)) {
            self.notify(format!("{url}: {e}"));
        }
    }

    /// Automatic check, and what the updater has to say.
    fn handle_update(&mut self, ctx: &egui::Context) {
        use crate::update::State;
        self.updater.tick(ctx, self.config.update.auto_check);
        let state = self.updater.state();
        if state == self.update_seen {
            return;
        }
        let manual = self.updater.manual;
        match &state {
            State::Available(r) => {
                let text = crate::tr_format!(
                    "Nuova versione {}: menu → Generale",
                    "New version {}: menu → General",
                    r.version
                );
                self.flash(text.clone());
                self.notify(text);
            }
            State::UpToDate if manual => self.notify(tr(self.lang, Msg::UpdateUpToDate).to_string()),
            State::Failed(e) => {
                log::warn!("update: {e}");
                if manual {
                    self.notify(format!("{}: {e}", tr(self.lang, Msg::UpdateFailed)));
                }
            }
            State::Installed => match self.updater.relaunch() {
                Ok(()) => {
                    log::info!("updated: restarting");
                    root_cmd(ctx, ViewportCommand::Close);
                }
                Err(e) => self.notify(format!("{}: {e}", tr(self.lang, Msg::UpdateFailed))),
            },
            _ => {}
        }
        self.update_seen = state;
    }

    fn notify(&mut self, text: String) {
        log::info!("{text}");
        self.notice = Some((text, Instant::now()));
    }

    /// A change worth a glance while the HUD is hidden: it goes in the mini indicator.
    fn flash(&mut self, text: String) {
        self.flash = Some((text, Instant::now()));
    }

    /// Layout or OS changed from one state to the next.
    fn flash_changes(&mut self, old: &HudState, new: &HudState) {
        let lang = self.lang;
        if old.flag(flags::COLEMAK) != new.flag(flags::COLEMAK) {
            self.flash(tr(lang, if new.flag(flags::COLEMAK) { Msg::Colemak } else { Msg::Qwerty }).to_string());
        }
        if (old.os_mode, old.active_os) != (new.os_mode, new.active_os) && new.active_os != OsKind::Auto {
            let how = tr(lang, if new.os_mode == OsKind::Auto { Msg::OsAuto } else { Msg::OsForced });
            self.flash(format!("{} · {how}", header::os_name(new.active_os)));
        }
    }

    /// The other half of the split keyboard was there and is gone.
    fn split_lost(&self) -> bool {
        self.split_seen && self.state.is_some_and(|s| !s.flag(flags::SLAVE_OK))
    }

    /// No keyboard to follow (not found, no permission, error): the HUD stays
    /// out of the way and the mini indicator and the tray icon say so.
    fn keyboard_absent(&self) -> bool {
        matches!(self.mode, Mode::Live { mock: false, .. })
            && matches!(self.status, None | Some(Status::Searching | Status::PermissionDenied(_) | Status::Error(_)))
    }

    /// Short reason for the missing live state, for the mini indicator.
    fn connection_problem(&self) -> Option<&'static str> {
        let msg = match &self.status {
            Some(Status::Connected { .. }) => return None,
            None | Some(Status::Searching) => Msg::BadgeNoKeyboard,
            Some(Status::PermissionDenied(_)) => Msg::BadgePermission,
            Some(Status::NoHudFirmware { .. }) => Msg::BadgeNoHud,
            Some(Status::Paused(PauseReason::Vial)) => Msg::BadgePausedVial,
            Some(Status::Paused(PauseReason::Foreign)) => Msg::BadgePausedForeign,
            Some(Status::Paused(PauseReason::Manual)) => Msg::PausedManual,
            Some(Status::Error(_)) => Msg::BadgeError,
        };
        Some(tr(self.lang, msg))
    }

    fn worker(&self) -> Option<&Worker> {
        match &self.mode {
            Mode::Live { worker, .. } => Some(worker),
            Mode::Offline => None,
        }
    }

    fn is_mock(&self) -> bool {
        matches!(self.mode, Mode::Live { mock: true, .. })
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        let Mode::Live { worker, .. } = &self.mode else { return };
        let events: Vec<Event> = worker.events.try_iter().collect();
        let now = Instant::now();
        for event in events {
            match event {
                Event::Status(s) => {
                    match &s {
                        Status::Connected { .. } | Status::NoHudFirmware { .. } => {}
                        // keep the last layout on screen while paused, but no stale state
                        Status::Paused(_) => self.forget_state(),
                        _ => {
                            self.live = None;
                            self.info = None;
                            self.split_seen = false;
                            self.forget_state();
                        }
                    }
                    self.status = Some(s);
                }
                Event::Layout { definition, layout_options, keymap, dynamic, info } => {
                    self.live = Some(Layout::new(definition, layout_options, keymap, dynamic));
                    self.info = info;
                    self.search_stale = true;
                }
                Event::Backup { path, current, previous, keyboard } => {
                    self.backup_saved(&path, &current, previous, &keyboard)
                }
                Event::Keymap(km) => {
                    if let Some(l) = &mut self.live {
                        l.keymap = km;
                    }
                    self.search_stale = true;
                }
                Event::KeyWritten { write, result } => self.key_written(write, result),
                Event::State(s) => {
                    let presses = self.fade.new_presses(&s.matrix);
                    let layer = highest_layer(s.active_mask());
                    if let Some(stats) = &mut self.stats {
                        // counters only: which key, on which layer, which day
                        let today = crate::clock::today();
                        for &(r, c) in &presses {
                            stats.record((layer, r, c), today);
                        }
                    }
                    if self.keycast_on {
                        if let Some(l) = &self.live {
                            let codes: Vec<((u8, u8), u16)> = presses
                                .iter()
                                .map(|&(r, c)| ((r, c), l.keymap.resolve(layer, s.active_mask(), r, c).0))
                                .collect();
                            let ctx = l.legend_context(&self.layer_names);
                            let held = s.mods | s.oneshot_mods | s.oneshot_locked_mods;
                            self.keycast.update(&codes, |r, c| s.is_pressed(r, c), held, s.active_os, &ctx, now);
                        }
                    }
                    if self.training.active() {
                        if let (Some(l), Some(session)) = (&self.live, &mut self.training.session) {
                            let mods = s.mods | s.oneshot_mods | s.oneshot_locked_mods;
                            let mut last = None;
                            for &(r, c) in &presses {
                                let code = l.keymap.resolve(layer, s.active_mask(), r, c).0;
                                last = Some(session.press(code, mods, now));
                            }
                            if let Some(o) = last {
                                self.training.feedback(o, now);
                            }
                        }
                    }
                    self.fade.update(&s.matrix, now);
                    if let TapAction::Double = self.hud_toggle.on_count(s.hud_toggle_count, now) {
                        self.set_click_through(ctx, !self.click_through);
                    }
                    if let Some(old) = self.state {
                        self.flash_changes(&old, &s);
                    }
                    let half = s.flag(flags::SLAVE_OK);
                    if self.split_seen && self.state.is_some_and(|o| o.flag(flags::SLAVE_OK) != half) {
                        log::warn!("{}", if half { "other half reconnected" } else { tr(self.lang, Msg::SplitLost) });
                    }
                    self.split_seen |= half;
                    self.state = Some(s);
                }
            }
        }
        // without a way to turn it off again, click-through must not stay on
        if self.click_through && !self.click_through_available() {
            self.set_click_through(ctx, false);
        }
    }

    fn forget_state(&mut self) {
        self.state = None;
        self.fade.clear();
        self.hud_toggle.reset();
    }

    fn click_through_available(&self) -> bool {
        matches!(self.status, Some(Status::Connected { .. }))
            && self.info.is_some_and(|i| i.capabilities & caps::HUD_TOG != 0)
    }

    fn set_click_through(&mut self, ctx: &egui::Context, on: bool) {
        if on == self.click_through || (on && !self.click_through_available()) {
            return;
        }
        self.click_through = on;
        root_cmd(ctx, ViewportCommand::MousePassthrough(on));
        self.notify(tr(self.lang, if on { Msg::ClickThroughOn } else { Msg::ClickThroughOff }).to_string());
    }

    fn backup_dir(&self) -> PathBuf {
        let dir = self.config.backup.dir.trim();
        if dir.is_empty() {
            paths::base_dir().join("backups")
        } else {
            paths::base_dir().join(dir) // an absolute path replaces the base
        }
    }

    /// Automatic backups, for a real keyboard only.
    fn apply_backup(&self) {
        if let Some(w) = self.worker() {
            let on = self.config.backup.enabled && !self.is_mock();
            w.set_backup(on.then(|| backup::Settings { dir: self.backup_dir(), keep: self.config.backup.keep }));
        }
    }

    /// Creates or removes the tray icon to match the configuration.
    fn apply_tray(&mut self, ctx: &egui::Context) {
        match (self.config.appearance.tray, self.tray.is_some()) {
            (true, false) => match crate::tray::Tray::new(ctx, self.lang) {
                Ok(t) => self.tray = Some(t),
                Err(e) => log::warn!("tray icon not available: {e}"),
            },
            (false, true) => self.tray = None,
            _ => {}
        }
    }

    /// Switches the interface language; the tray menu is built again.
    fn set_language(&mut self, ctx: &egui::Context, choice: LanguageChoice) {
        self.language = choice;
        let lang = resolve_language(choice);
        if lang == self.lang {
            self.mark_dirty();
            return;
        }
        self.lang = lang;
        crate::i18n::set_current(lang);
        self.search_stale = true;
        if self.tray.take().is_some() {
            self.apply_tray(ctx);
        }
        self.mark_dirty();
    }

    /// Is the HUD on screen (not hidden, not faded out by heads-up)?
    fn on_screen(&self) -> bool {
        self.visible && (!(self.headsup_on || self.keyboard_absent()) || self.pinned || self.headsup.level() > 0.01)
    }

    fn handle_tray(&mut self, ctx: &egui::Context) {
        let Some(tray) = &self.tray else { return };
        for action in tray.poll() {
            match action {
                crate::tray::Action::ToggleHud => {
                    if self.on_screen() {
                        self.pinned = false;
                        self.set_visible(ctx, false);
                    } else {
                        // with heads-up it would fade out at once: keep it on screen
                        self.pinned = self.headsup_on || self.keyboard_absent();
                        self.set_visible(ctx, true);
                    }
                }
                crate::tray::Action::TogglePause => {
                    if let Some(w) = self.worker() {
                        w.set_paused(!w.is_paused());
                    }
                }
                crate::tray::Action::ToggleAutostart => {
                    self.refresh_autostart();
                    let on = matches!(self.autostart, Ok(autostart::State::On));
                    self.set_autostart(!on);
                }
                crate::tray::Action::Backups => {
                    if let Err(e) = backup::open_folder(&self.backup_dir()) {
                        self.notify(format!("{}: {e}", self.backup_dir().display()));
                    }
                }
                crate::tray::Action::Stats => {
                    self.report.open = true;
                    ctx.request_repaint();
                }
                crate::tray::Action::Settings => {
                    self.open_settings();
                    ctx.request_repaint();
                }
                crate::tray::Action::Update => match self.updater.state() {
                    crate::update::State::Available(r) if r.installable() => self.updater.install(ctx),
                    crate::update::State::Available(r) => self.open_url(&r.page),
                    _ => self.updater.check(ctx, true),
                },
                crate::tray::Action::Quit => root_cmd(ctx, ViewportCommand::Close),
            }
        }
        let (visible, paused) = (self.on_screen(), self.worker().is_some_and(Worker::is_paused));
        let alert = self.keyboard_absent().then(|| self.connection_problem()).flatten();
        let autostart = matches!(self.autostart, Ok(autostart::State::On));
        let update = match self.updater.state() {
            crate::update::State::Available(r) => Some(r.version),
            _ => None,
        };
        if let Some(t) = &mut self.tray {
            t.sync(visible, paused, autostart, update.as_deref(), alert);
        }
    }

    /// Writes what changed into the history next to the backups.
    fn backup_saved(
        &mut self,
        path: &Path,
        current: &serde_json::Value,
        previous: Option<serde_json::Value>,
        keyboard: &str,
    ) {
        self.last_backup = Some((path.to_path_buf(), Instant::now()));
        let custom = self.live.as_ref().map(|l| l.definition.custom.as_slice()).unwrap_or(&[]);
        let lines = match &previous {
            Some(prev) => {
                let ctx = changes::Context { layer_names: &self.layer_names, custom };
                changes::describe(prev, current, &ctx)
            }
            None => vec![changes::summary(current)],
        };
        let dir = path.parent().unwrap_or(Path::new("."));
        if let Err(e) = backup::append_changelog(dir, keyboard, path, crate::clock::now_secs(), &lines) {
            log::warn!("{}: {e}", dir.join(backup::CHANGELOG).display());
        }
        for l in &lines {
            log::info!("change: {l}");
        }
        let what = match (&previous, lines.len()) {
            (None, _) => tr(self.lang, Msg::BackupFirst).to_string(),
            (Some(_), 1) => lines[0].clone(),
            (Some(_), n) => format!("{n} {}", tr(self.lang, Msg::BackupChanges)),
        };
        self.notify(format!("{}: {what}", tr(self.lang, Msg::BackupSaved)));
    }

    fn set_visible(&mut self, ctx: &egui::Context, visible: bool) {
        if visible == self.visible {
            return;
        }
        self.visible = visible;
        root_cmd(ctx, ViewportCommand::Visible(visible));
        self.apply_poll_rate();
        log::info!("{}", if visible { "HUD visible" } else { tr(self.lang, Msg::HudHidden) });
        if visible {
            self.flash = None;
        } else {
            self.flash(tr(self.lang, Msg::HudHidden).to_string());
        }
    }

    fn apply_poll_rate(&self) {
        if let Some(w) = self.worker() {
            let p = &self.config.polling;
            let hidden = if self.stats.is_some() { p.hidden_hz.max(MIN_STATS_HZ) } else { p.hidden_hz };
            w.set_poll_hz(if self.visible { p.visible_hz } else { hidden });
            w.set_timeout_ms(p.timeout_ms);
        }
    }

    /// Applies a new configuration; returns the problems found in the labels.
    fn apply_config(&mut self, ctx: &egui::Context, config: Config) -> Vec<String> {
        if config.device != self.config.device {
            log::warn!("the [device] settings apply from the next start");
        }
        let (names, colors) = layer_table(&config.layers);
        self.layer_names = names;
        self.layer_colors = colors;
        let (labels, errors) = Labels::from_config(&config.labels);
        self.labels = labels;
        let app = self.foreground.poll().map(str::to_owned);
        self.labels.set_app(app.as_deref());
        self.search_stale = true;
        if config.stats != self.config.stats {
            if let Some(s) = &mut self.stats {
                s.save();
            }
            self.stats = config.stats.enabled.then(|| Stats::load(stats_path(&config.stats)));
            self.heat_cache = None;
        }
        if config.headsup.enabled != self.config.headsup.enabled {
            self.headsup_on = config.headsup.enabled;
        }
        if config.headsup.marker != self.config.headsup.marker {
            self.marker_on = config.headsup.marker;
        }
        if config.appearance.combos != self.config.appearance.combos {
            self.show_combos = config.appearance.combos;
        }
        if config.keycast.enabled != self.config.keycast.enabled {
            self.keycast_on = config.keycast.enabled;
        }
        if config.appearance.language != self.config.appearance.language {
            self.set_language(ctx, config.appearance.language);
        }
        // only what changed in the file: a menu choice not saved yet stays
        let (old, new) = (&self.config.appearance, &config.appearance);
        if new.opacity != old.opacity {
            self.opacity = new.opacity;
        }
        if new.theme != old.theme {
            self.theme = new.theme;
        }
        if new.key_opacity != old.key_opacity {
            self.key_opacity = new.key_opacity;
        }
        if new.key_radius != old.key_radius {
            self.key_radius = new.key_radius;
        }
        let new_style = (new.style != old.style).then(|| {
            if new.style.is_empty() {
                style::DEFAULT_STYLE.to_string()
            } else {
                new.style.clone()
            }
        });
        let resize = new.scale != old.scale || new.mode != old.mode;
        if resize {
            self.scale = new.scale;
            self.view = new.mode;
        }
        if !self.verbose {
            logging::set_level(logging::parse_level(&config.log.level));
        }
        self.config = config;
        self.synced = Prefs::from_config(&self.config);
        if let Some(name) = new_style {
            if !self.set_style(&name, false) {
                // a style that does not load: the file is not "fixed" behind the user's back
                self.synced.style = self.style_name.clone();
            }
        }
        self.apply_poll_rate();
        self.apply_backup();
        if self.screenshot.is_none() {
            self.apply_tray(ctx);
        }
        if resize {
            self.resize(ctx);
        }
        errors
    }

    /// Menu choices as they are now.
    fn prefs(&self) -> Prefs {
        Prefs {
            opacity: self.opacity,
            scale: self.scale,
            mode: self.view,
            theme: self.theme,
            language: self.language,
            style: self.style_name.clone(),
            key_opacity: self.key_opacity,
            key_radius: self.key_radius,
            headsup: self.headsup_on,
            marker: self.marker_on,
            combos: self.show_combos,
            keycast: self.keycast_on,
        }
    }

    /// Writes keys into the config file; the watcher does not reload it.
    fn write_config(&mut self, edits: &[Edit]) -> Result<Config, String> {
        let config = config_edit::write(self.watcher.path(), edits)?;
        self.written();
        Ok(config)
    }

    /// The app wrote a valid config file.
    fn written(&mut self) {
        self.watcher.written();
        self.config_error = None;
        self.config_revision += 1;
    }

    /// Menu choices changed since the last save go into the config file.
    fn save_prefs(&mut self) {
        let prefs = self.prefs();
        let edits = prefs.edits(&self.synced);
        if edits.is_empty() {
            return;
        }
        log::debug!("menu choices in the configuration: {edits:?}");
        // also on failure: the choice stays for this session, without a notice every second
        self.synced = prefs;
        match self.write_config(&edits) {
            Ok(c) => self.config = c,
            Err(e) => {
                let path = self.watcher.path().display().to_string();
                self.notify(format!("{} ({path}): {e}", tr(self.lang, Msg::ConfigError)));
            }
        }
    }

    /// A change from the settings window: written, then applied like an edit by hand.
    /// Label problems are not notified: the window shows them next to the label.
    fn edit_config(&mut self, ctx: &egui::Context, edits: &[Edit]) {
        self.save_prefs();
        match self.write_config(edits) {
            Ok(c) => {
                self.apply_config(ctx, c);
            }
            Err(e) => self.notify(format!("{}: {e}", tr(self.lang, Msg::ConfigError))),
        }
    }

    /// The whole file, edited as text in the settings window.
    fn save_config_text(&mut self, ctx: &egui::Context, text: &str) {
        match config_edit::write_text(self.watcher.path(), text) {
            Ok(c) => {
                self.written();
                let errors = self.apply_config(ctx, c);
                self.notify(tr(self.lang, Msg::ConfigReloaded).to_string());
                self.report_label_errors(&errors);
            }
            Err(e) => self.notify(format!("{}: {e}", tr(self.lang, Msg::ConfigError))),
        }
    }

    /// Opens the config file in the system editor, created first if missing.
    fn open_config_file(&mut self) {
        let path = self.watcher.path().to_path_buf();
        let result = if path.exists() { Ok(()) } else { self.write_config(&[]).map(|_| ()) };
        if let Err(e) = result.and_then(|()| backup::open_path(&path).map_err(|e| e.to_string())) {
            self.notify(format!("{}: {e}", path.display()));
        }
    }

    fn resize(&mut self, ctx: &egui::Context) {
        let size = base_size(self.view) * self.scale;
        root_cmd(ctx, ViewportCommand::InnerSize(size));
        if let Some(r) = root_outer_rect(ctx) {
            self.keep_on_screen(ctx, Rect::from_min_size(r.min, size));
        }
        self.mark_dirty();
    }

    /// Brings the HUD back on screen when part of it is out (larger size, or
    /// remembered position on a monitor that is gone).
    fn keep_on_screen(&self, ctx: &egui::Context, window: Rect) {
        if let Some(area) = work_area(ctx, window) {
            let pos = screen::keep_inside(window, area, self.config.appearance.screen_margin);
            if pos != window.min {
                root_cmd(ctx, ViewportCommand::OuterPosition(pos));
            }
        }
    }

    /// Moves the HUD to one of the quick positions.
    fn move_to(&self, ctx: &egui::Context, anchor: screen::Anchor) {
        if let Some(r) = root_outer_rect(ctx) {
            if let Some(area) = work_area(ctx, r) {
                let pos = screen::anchored(anchor, r.size(), area, self.config.appearance.screen_margin);
                log::debug!("position {anchor:?}: window {r:?}, area {area:?} -> {pos:?}");
                root_cmd(ctx, ViewportCommand::OuterPosition(pos));
            }
        }
    }

    fn mark_dirty(&mut self) {
        self.window_dirty = Some(Instant::now());
    }

    /// Position into the state file, menu choices into the config file.
    fn save_window(&mut self) {
        self.window = WindowState { x: self.window.x, y: self.window.y, ..Default::default() };
        self.window.save(&self.window_path);
        self.window_dirty = None;
        self.save_prefs();
    }

    fn track_position(&mut self, ctx: &egui::Context) {
        if let Some(r) = ctx.input(|i| i.viewport().outer_rect) {
            let moved = |old: Option<f32>, new: f32| old.is_none_or(|o| (o - new).abs() > 0.5);
            if moved(self.window.x, r.min.x) || moved(self.window.y, r.min.y) {
                self.window.x = Some(r.min.x);
                self.window.y = Some(r.min.y);
                self.mark_dirty();
            }
        }
        if self.window_dirty.is_some_and(|t| t.elapsed() >= SAVE_DELAY) {
            self.save_window();
        }
    }

    /// Shape of keys and window: the values set, then the style.
    fn look(&self) -> Look {
        self.style.look_with(self.key_opacity, self.key_radius)
    }

    /// Switches style. From the menu the style is shown as it is: its window
    /// opacity applies and the key values set before are dropped. From the
    /// file the other keys there keep their values. False if it does not load.
    fn set_style(&mut self, name: &str, from_menu: bool) -> bool {
        match style::load(name) {
            Ok(st) => {
                if from_menu {
                    self.key_opacity = None;
                    self.key_radius = None;
                    if let Some(o) = st.window_opacity {
                        self.opacity = o;
                    }
                }
                self.style = st;
                self.style_name = name.to_string();
                self.style_watcher = StyleWatcher::new(name);
                self.mark_dirty();
                true
            }
            Err(e) => {
                self.notify(e);
                false
            }
        }
    }

    /// The style file in use changed: applied at once, errors keep the old one.
    fn poll_style(&mut self) {
        match self.style_watcher.poll() {
            Some(Ok(st)) => {
                self.style = st;
                self.notify(format!("{}: {}", tr(self.lang, Msg::ConfigReloaded), self.style_name));
            }
            Some(Err(e)) => self.notify(e),
            None => {}
        }
    }

    /// Writes the current look, every color included, as a new style and switches to it.
    fn save_style(&mut self) {
        let look = self.look();
        let colors = |dark: bool| Palette::new(dark, self.opacity, self.style.colors(dark), look).colors();
        let st = Style {
            key_opacity: Some(look.key_opacity),
            key_radius: Some(look.key_radius),
            border_width: Some(look.border_width),
            legend_scale: Some(look.legend_scale),
            window_radius: Some(look.window_radius),
            window_opacity: Some(self.opacity),
            dark: colors(true),
            light: colors(false),
        };
        match style::save_new("personal", &st) {
            Ok(name) => {
                self.set_style(&name, true);
                self.notify(format!(
                    "{}: {}",
                    tr(self.lang, Msg::StyleSaved),
                    style::dir().join(format!("{name}.toml")).display()
                ));
            }
            Err(e) => self.notify(format!("{}: {e}", style::dir().display())),
        }
    }

    /// Everything the diagnostics window shows.
    fn diagnostics_sections(&self) -> Vec<diagnostics::Section> {
        use diagnostics::{short_duration, Section};
        let t = |m| tr(self.lang, m);
        let ago = |at: Instant| format!("{} {}", short_duration(at.elapsed()), t(Msg::DiagAgo));
        let mut keyboard = Section::new(t(Msg::DiagKeyboard));
        keyboard.row(t(Msg::DiagStatus), self.status_line());
        if let Some(l) = self.live.as_ref().or(self.offline.as_ref()) {
            keyboard.row(t(Msg::DiagDefinition), l.definition.name.clone());
            keyboard.row(
                t(Msg::DiagMatrix),
                format!("{} × {}, {} layer", l.definition.rows, l.definition.cols, l.keymap.layers()),
            );
        }
        if let Some(i) = self.info {
            keyboard.row(t(Msg::DiagFirmware), format!("v{} · capabilities {:#06x}", i.protocol, i.capabilities));
        }
        let mut sections = vec![keyboard];

        if let Some(w) = self.worker() {
            let link = w.link();
            let mut s = Section::new(t(Msg::DiagLink));
            let target = if self.visible { self.config.polling.visible_hz } else { self.config.polling.hidden_hz };
            s.row(t(Msg::DiagPolls), format!("{} / {target}", link.polls_per_sec));
            let ms = |d: Duration| format!("{:.1} ms", d.as_secs_f64() * 1000.0);
            s.row(t(Msg::DiagLatency), format!("{} (max {})", ms(link.latency_avg), ms(link.latency_max)));
            let errors = match &link.last_error {
                Some((e, at)) => format!("{} ({} {}: {e})", link.errors, t(Msg::DiagLast), ago(*at)),
                None => link.errors.to_string(),
            };
            s.row(t(Msg::DiagErrors), errors);
            s.row(t(Msg::DiagConnections), link.sessions.to_string());
            if let Some(since) = link.connected_since {
                s.row(t(Msg::DiagConnectedFor), short_duration(since.elapsed()));
            }
            sections.push(s);
        }

        if let Some(st) = &self.state {
            let mut s = Section::new(t(Msg::DiagLive));
            s.row(t(Msg::DiagKeymapVersion), st.keymap_version.to_string());
            let how = t(if st.os_mode == OsKind::Auto { Msg::OsAuto } else { Msg::OsForced });
            s.row(t(Msg::DiagOs), format!("{} · {how} ({:?})", header::os_name(st.active_os), st.detected_os));
            let split = match (self.split_seen, st.flag(flags::SLAVE_OK)) {
                (_, true) => t(Msg::DiagSplitOk),
                (true, false) => t(Msg::SplitLost),
                (false, false) => t(Msg::DiagSplitUnknown),
            };
            s.row(t(Msg::DiagSplit), split);
            sections.push(s);
        }

        let mut files = Section::new(t(Msg::DiagFiles));
        files.row(t(Msg::DiagConfig), self.watcher.path().display().to_string());
        files.row(t(Msg::DiagLog), paths::base_dir().join("keyboard-hud.log").display().to_string());
        files.row(t(Msg::DiagStyles), style::dir().display().to_string());
        let stats = if self.config.stats.enabled {
            stats_path(&self.config.stats).display().to_string()
        } else {
            t(Msg::DiagOff).to_string()
        };
        files.row(t(Msg::DiagStats), stats);
        let backups = if self.config.backup.enabled {
            self.backup_dir().display().to_string()
        } else {
            t(Msg::DiagOff).to_string()
        };
        files.row(t(Msg::DiagBackups), backups);
        let last = match &self.last_backup {
            Some((p, at)) => format!("{} ({})", p.file_name().unwrap_or_default().to_string_lossy(), ago(*at)),
            None => t(Msg::DiagNone).to_string(),
        };
        files.row(t(Msg::DiagLastBackup), last);
        sections.push(files);

        let mut app = Section::new(t(Msg::DiagApp));
        app.row(t(Msg::DiagVersion), env!("CARGO_PKG_VERSION"));
        app.row(t(Msg::DiagSystem), format!("{} {}", std::env::consts::OS, std::env::consts::ARCH));
        sections.push(app);
        sections
    }

    fn apply_theme(&mut self, ctx: &egui::Context) -> bool {
        let dark = match self.theme {
            ThemeChoice::Dark => true,
            ThemeChoice::Light => false,
            ThemeChoice::System => ctx.system_theme().is_none_or(|t| t == egui::Theme::Dark),
        };
        if self.dark != Some(dark) {
            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
            self.dark = Some(dark);
        }
        dark
    }

    fn status_line(&self) -> String {
        let t = |m| tr(self.lang, m);
        match (&self.mode, &self.status) {
            (Mode::Offline, _) => t(Msg::Offline).to_string(),
            (_, Some(Status::Paused(PauseReason::Vial))) => t(Msg::PausedVial).to_string(),
            (_, Some(Status::Paused(PauseReason::Manual))) => t(Msg::PausedManual).to_string(),
            (_, Some(Status::Paused(PauseReason::Foreign))) => t(Msg::PausedForeign).to_string(),
            (Mode::Live { mock: true, .. }, _) => t(Msg::Mock).to_string(),
            (_, None | Some(Status::Searching)) => t(Msg::Searching).to_string(),
            (_, Some(Status::Connected { product })) => format!("{} · {product}", t(Msg::Connected)),
            (_, Some(Status::NoHudFirmware { product })) => format!("{product}: {}", t(Msg::NoHudFirmware)),
            (_, Some(Status::PermissionDenied(path))) => {
                format!("{} ({path}). {}", t(Msg::PermissionDenied), t(Msg::UdevHint))
            }
            (_, Some(Status::Error(e))) => format!("{}: {e}", t(Msg::Error)),
        }
    }

    fn handle_browse_input(&mut self, ctx: &egui::Context, layers: u8) {
        let max = layers.saturating_sub(1);
        ctx.input(|i| {
            for n in 0..=9u8 {
                if Key::from_name(&n.to_string()).is_some_and(|k| i.key_pressed(k)) && n <= max {
                    self.browse_layer = n;
                }
            }
            if i.modifiers.command {
                return; // Ctrl + wheel zooms
            }
            // one layer per wheel event, whatever its size
            let scroll: f32 = i
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::MouseWheel { delta, .. } => Some(delta.y.signum()),
                    _ => None,
                })
                .sum();
            if scroll < 0.0 && self.browse_layer < max {
                self.browse_layer += 1;
            } else if scroll > 0.0 && self.browse_layer > 0 {
                self.browse_layer -= 1;
            }
        });
    }

    /// Moves the window while the background is dragged: by itself where the
    /// cursor position on screen is known, otherwise through the window system.
    fn handle_drag(&mut self, ctx: &egui::Context, response: &egui::Response) {
        if response.drag_started() {
            let outer = ctx.input(|i| i.viewport().outer_rect);
            match (cursor_on_screen(), outer) {
                (Some(cursor), Some(outer)) => self.drag = Some(WindowDrag { cursor, window: outer.min }),
                _ => ctx.send_viewport_cmd(ViewportCommand::StartDrag),
            }
        }
        if !response.dragged() {
            self.drag = None;
            return;
        }
        // from the start position, not from the current one: no error piles up
        if let (Some(d), Some(cursor)) = (self.drag, cursor_on_screen()) {
            let outer = ctx.input(|i| i.viewport().outer_rect);
            let mut target = d.window + (cursor - d.cursor) / ctx.pixels_per_point();
            if let Some(r) = outer {
                let moved = Rect::from_min_size(target, r.size());
                if let Some(area) = work_area(ctx, moved) {
                    let a = &self.config.appearance;
                    target = screen::snap(moved, area, a.snap_distance, a.screen_margin);
                }
            }
            if outer.is_none_or(|r| r.min != target) {
                ctx.send_viewport_cmd(ViewportCommand::OuterPosition(target));
            }
        }
    }

    fn handle_zoom(&mut self, ctx: &egui::Context) {
        let zoom = ctx.input(|i| i.zoom_delta());
        if (zoom - 1.0).abs() > f32::EPSILON {
            let scale = (self.scale * zoom).clamp(0.5, 2.0);
            if (scale - self.scale).abs() > 0.001 {
                self.scale = scale;
                self.resize(ctx);
            }
        }
    }

    fn close_menu(&mut self) {
        self.menu = None;
    }

    /// Keys can be written: connected to a keyboard, not paused.
    fn can_write_keys(&self) -> bool {
        self.live.is_some()
            && self.worker().is_some_and(|w| !w.is_paused())
            && matches!(self.status, Some(Status::Connected { .. } | Status::NoHudFirmware { .. }))
    }

    fn layer_label(&self, layer: u8) -> String {
        self.layer_names.get(layer as usize).cloned().unwrap_or_else(|| format!("L{layer}"))
    }

    fn open_key_editor(&mut self, (layer, row, col): (u8, u8, u8)) {
        let Some(l) = &self.live else { return };
        let legends = l.legend_context(&self.layer_names);
        let catalog =
            key_editor::catalog(l.keymap.layers(), l.dynamic.tap_dances.len(), l.dynamic.macros.len(), &legends);
        let place = crate::tr_format!(
            "{} · riga {row}, colonna {col}",
            "{} · row {row}, column {col}",
            self.layer_label(layer)
        );
        let target = key_editor::Target { layer, row, col, shown: l.keymap.get(layer, row, col), place };
        self.key_editor.open(target, catalog);
    }

    fn show_key_editor(&mut self, ctx: &egui::Context) {
        if self.live.is_some() || self.offline.is_some() {
            if let Some(path) = self.dev_export.take() {
                match self.export_keymap(&path) {
                    Ok(()) => log::info!("keymap exported to {}", path.display()),
                    Err(e) => log::error!("export to {}: {e}", path.display()),
                }
                root_cmd(ctx, ViewportCommand::Close);
            }
        }
        if self.live.is_some() {
            if let Some((key, query)) = self.dev_edit_key.take() {
                self.open_key_editor(key);
                self.key_editor.search_and_pick(&query);
            }
        }
        if !self.key_editor.is_open() {
            return;
        }
        let Some(l) = &self.live else {
            self.key_editor.close();
            return;
        };
        // the keymap may have changed since the window opened
        if let Some(t) = &mut self.key_editor.target {
            t.shown = l.keymap.get(t.layer, t.row, t.col);
        }
        let present = positions(&l.keys);
        let default_layer = self.state.map_or(0, |s| (s.default_layer_state as u32).trailing_zeros().min(31) as u8);
        let hud_tog = self.info.filter(|i| i.capabilities & caps::HUD_TOG != 0).map(|i| i.hud_tog_keycode);
        let target = self.key_editor.target.clone();
        let check = |code: u16| {
            let t = target.as_ref().ok_or(crate::keymap_guard::Refusal::Unchanged)?;
            let change = crate::keymap_guard::Change { layer: t.layer, row: t.row, col: t.col, code };
            crate::keymap_guard::check(&l.keymap, &present, &l.dynamic, default_layer, hud_tog, change)
        };
        let names = self.layer_names.clone();
        let layer_name = |n: u8| names.get(n as usize).cloned().unwrap_or_else(|| format!("L{n}"));
        let legends = l.legend_context(&self.layer_names);
        let available = self.can_write_keys();
        let backups = self.config.backup.enabled && !self.is_mock();
        let action = self.key_editor.show(ctx, self.lang, &legends, available, backups, &check, &layer_name);
        if let key_editor::Action::Write(w) = action {
            match self.worker() {
                Some(worker) => worker.write_key(w),
                None => self.key_editor.writing = None,
            }
        }
    }

    fn show_training(&mut self, ctx: &egui::Context) {
        if !self.training.open {
            return;
        }
        let available = self.can_write_keys() && self.info.is_some() && self.state.is_some();
        let Some(l) = &self.live else {
            let legends = crate::vial::legend::LegendContext {
                layer_names: &self.layer_names,
                custom: &[],
                tap_dances: &[],
                macros: &[],
            };
            let input = training::TrainingInput { lang: self.lang, legends: &legends, hint: None, available: false };
            self.training.show(ctx, &input);
            return;
        };
        let legends = l.legend_context(&self.layer_names);
        let hint = self
            .training
            .session
            .as_ref()
            .and_then(|s| s.current())
            .map(|t| crate::search::sequence(&l.keymap, &legends, &self.labels, t.layer, t.row, t.col));
        let input = training::TrainingInput { lang: self.lang, legends: &legends, hint, available };
        let start = matches!(self.training.show(ctx, &input), training::Action::Start);
        if start || (available && std::mem::take(&mut self.dev_training)) {
            let present = positions(&l.keys);
            let default_layer = self.state.map_or(0, |s| (s.default_layer_state as u32).trailing_zeros().min(31) as u8);
            let pool = crate::training::pool(&l.keymap, &present, default_layer);
            // presses per keycode, from the statistics: rare keys come up more
            let mut usage = HashMap::new();
            if let Some(stats) = &self.stats {
                for (&(layer, r, c), &n) in &stats.totals(None) {
                    *usage.entry(l.keymap.get(layer, r, c)).or_insert(0) += n;
                }
            }
            let seed =
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64);
            self.training.session =
                Some(crate::training::Session::new(pool, usage, crate::training::ROUND, seed, Instant::now()));
        }
    }

    /// `export/<keyboard>-keymap_<time>.html` next to the configuration.
    fn export_path(&self) -> PathBuf {
        let name = self.live.as_ref().or(self.offline.as_ref()).map_or("keyboard", |l| l.definition.name.as_str());
        let file = format!("{}-keymap_{}.html", backup::slug(name), crate::clock::file_stamp(crate::clock::now_secs()));
        paths::base_dir().join("export").join(file)
    }

    /// Writes the printable keymap page (light theme) to `path`.
    fn export_keymap(&self, path: &Path) -> std::io::Result<()> {
        let layout = self.live.as_ref().or(self.offline.as_ref()).ok_or_else(|| std::io::Error::other("no layout"))?;
        let palette = Palette::new(false, 1.0, self.style.colors(false), self.look());
        let date = format!("{} UTC", crate::clock::timestamp());
        let input = export::ExportInput {
            keyboard: &layout.definition.name,
            date: &date,
            keys: &layout.keys,
            keymap: &layout.keymap,
            dynamic: &layout.dynamic,
            layer_names: &self.layer_names,
            layer_colors: &self.layer_colors,
            custom: &layout.definition.custom,
            labels: &self.labels,
            palette: &palette,
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, export::html(&input))
    }

    fn show_history(&mut self, ctx: &egui::Context, palette: &Palette) {
        if !self.history.open {
            return;
        }
        let dir = self.backup_dir();
        let layout = self.live.as_ref().or(self.offline.as_ref());
        let colors = self.layer_colors.clone();
        let layer_color = |l: u8| colors.get(l as usize).copied().flatten();
        let input = history::HistoryInput {
            lang: self.lang,
            dir: &dir,
            keys: layout.map(|l| l.keys.as_slice()),
            rows: layout.map_or(0, |l| l.definition.rows),
            cols: layout.map_or(0, |l| l.definition.cols),
            layer_names: &self.layer_names,
            layer_color: &layer_color,
            custom: layout.map_or(&[], |l| l.definition.custom.as_slice()),
            labels: &self.labels,
            palette,
            can_restore: self.can_write_keys(),
        };
        match self.history.show(ctx, &input) {
            history::Action::None => {}
            history::Action::OpenFolder => {
                if let Err(e) = backup::open_folder(&dir) {
                    self.notify(format!("{}: {e}", dir.display()));
                }
            }
            history::Action::Restore(k) => {
                self.open_key_editor((k.layer, k.row, k.col));
                self.key_editor.preselect(k.before);
            }
        }
    }

    fn write_error_text(&self, e: &WriteError) -> String {
        match e {
            WriteError::NotConnected => tr(self.lang, Msg::WriteNotConnected).to_string(),
            WriteError::Remapped => tr(self.lang, Msg::WriteRemapped).to_string(),
            WriteError::Backup => tr(self.lang, Msg::WriteBackup).to_string(),
            WriteError::Bootloader => tr(self.lang, Msg::EditKeyBootloader).to_string(),
            WriteError::Device(d) => d.clone(),
        }
    }

    /// "Esc → Ctrl+Z" with the legends of the layer's keys.
    fn change_text(&self, from: u16, to: u16) -> String {
        let name = |code: u16| match &self.live {
            Some(l) => crate::vial::legend::legend(code, &l.legend_context(&self.layer_names)).tap.text,
            None => crate::vial::keycode::qmk_name(code),
        };
        format!("{} → {}", name(from), name(to))
    }

    fn key_written(&mut self, write: KeyWrite, result: Result<u16, WriteError>) {
        let undo = self.undoing.take_if(|u| *u == write).is_some();
        let edit = self.key_editor.writing.take_if(|w| *w == write).is_some();
        match result {
            Ok(previous) => {
                let what = self.change_text(previous, write.code);
                if undo {
                    self.last_write = None;
                    self.notify(crate::tr_format!("Modifica annullata: {what}", "Change undone: {what}"));
                } else {
                    self.last_write = Some((write, previous));
                    if edit {
                        self.key_editor.close();
                    }
                    let place = self.layer_label(write.layer);
                    self.notify(crate::tr_format!("Tasto scritto ({place}): {what}", "Key written ({place}): {what}"));
                }
            }
            Err(e) => {
                let text = self.write_error_text(&e);
                log::warn!("writing the key failed: {text}");
                if edit {
                    self.key_editor.error = Some(text);
                } else {
                    self.notify(crate::tr_format!("Tasto non scritto: {text}", "Key not written: {text}"));
                }
            }
        }
    }

    fn undo_key(&mut self) {
        let Some((done, previous)) = self.last_write else { return };
        let w = KeyWrite { code: previous, shown: done.code, ..done };
        if let Some(worker) = self.worker() {
            worker.write_key(w);
            self.undoing = Some(w);
        }
    }

    fn open_settings(&mut self) {
        self.style_names = style::names();
        self.settings.show_window();
    }

    /// Opens the menu at the pointer, `hud` being the HUD area in the window.
    fn exe() -> std::io::Result<PathBuf> {
        std::env::current_exe()
    }

    fn refresh_autostart(&mut self) {
        self.autostart =
            Self::exe().and_then(|exe| autostart::state(&exe, &self.autostart_args)).map_err(|e| e.to_string());
    }

    fn set_autostart(&mut self, on: bool) {
        let result = Self::exe().and_then(|exe| autostart::set(on, &exe, &self.autostart_args));
        let msg = match result {
            Ok(()) => tr(self.lang, if on { Msg::AutostartOn } else { Msg::AutostartOff }).to_string(),
            Err(e) => format!("{}: {e}", tr(self.lang, Msg::AutostartError)),
        };
        log::debug!("start with the system: {}", autostart::location());
        self.notify(msg);
        self.refresh_autostart();
    }

    fn open_menu(&mut self, ctx: &egui::Context, hud: Rect, pointer: egui::Pos2) {
        self.menu_key = None;
        self.refresh_autostart();
        self.style_names = style::names();
        let anchor = ctx.input(|i| i.viewport().inner_rect).map(|inner| inner.min + pointer.to_vec2());
        log::debug!("menu opened at {anchor:?}");
        self.menu = Some(MenuWindow {
            anchor,
            left: pointer.x > hud.center().x,
            up: pointer.y > hud.center().y,
            focused_once: false,
        });
    }

    /// Keycast: a click-through window at the bottom of the screen, empty
    /// when there is nothing to show.
    fn show_keycast(&self, ctx: &egui::Context, palette: &Palette) {
        let now = Instant::now();
        let (col, row) = self.config.keycast.position.cell();
        let builder = egui::ViewportBuilder::default()
            .with_title(frameless::KEYCAST_TITLE)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_resizable(false)
            .with_taskbar(false)
            .with_mouse_passthrough(true)
            .with_inner_size(KEYCAST_SIZE);
        let place = root_outer_rect(ctx).and_then(|r| work_area(ctx, r)).map(|area| {
            screen::anchored(screen::Anchor { col, row }, KEYCAST_SIZE, area, self.config.appearance.screen_margin)
        });
        let builder = match place {
            Some(p) => builder.with_position(p),
            None => builder,
        };
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of(KEYCAST_VIEWPORT), builder, |ui, _class| {
            let rect = ui.max_rect().shrink(4.0);
            if self.keycast.draw(ui.painter(), rect, col, palette, now) {
                ui.ctx().request_repaint();
            }
        });
        if !self.keycast.is_empty(now) {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    /// Mini indicator: a small click-through window in a corner of the screen.
    fn show_indicator(&self, ctx: &egui::Context, view: Option<&header::IndicatorView>, palette: &Palette) {
        let size = Vec2::new(INDICATOR_SIZE.x, INDICATOR_SIZE.y);
        let (col, row) = self.config.indicator.position.cell();
        let builder = egui::ViewportBuilder::default()
            .with_title(frameless::INDICATOR_TITLE)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_resizable(false)
            .with_taskbar(false)
            .with_mouse_passthrough(true)
            .with_inner_size(size);
        let place = root_outer_rect(ctx).and_then(|r| work_area(ctx, r)).map(|area| {
            screen::anchored(screen::Anchor { col, row }, size, area, self.config.appearance.screen_margin)
        });
        let builder = match place {
            Some(p) => builder.with_position(p),
            None => builder,
        };
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of(INDICATOR_VIEWPORT), builder, |ui, _class| {
            let rect = ui.max_rect().shrink(2.0);
            if view.is_some_and(|v| header::draw_indicator(ui.painter(), rect, col, v, palette)) {
                ui.ctx().request_repaint();
            }
        });
    }

    fn show_menu(&mut self, ctx: &egui::Context) {
        let Some(m) = self.menu else { return };
        let margin = self.config.appearance.screen_margin;
        // the work area of the monitor where the click happened
        let area = m.anchor.and_then(|a| work_area(ctx, Rect::from_center_size(a, Vec2::splat(2.0))));
        // taller than the screen (small or scaled displays): it scrolls
        let max_height = area.map_or(f32::INFINITY, |a| a.height() - 2.0 * margin);
        let size = Vec2::new(MENU_SIZE.x, MENU_SIZE.y.min(max_height));
        let place = m.anchor.map(|a| match area {
            Some(area) => screen::popup_position(a, size, area, m.left, m.up, margin),
            None => a - Vec2::new(if m.left { size.x } else { 0.0 }, if m.up { size.y } else { 0.0 }),
        });
        let builder = egui::ViewportBuilder::default()
            .with_title("Keyboard HUD · Menu")
            .with_decorations(false)
            .with_always_on_top()
            .with_resizable(false)
            .with_taskbar(false)
            .with_inner_size(size);
        let builder = match place {
            Some(p) => builder.with_position(p),
            None => builder,
        };
        let mut close = false;
        let mut focused_once = m.focused_once;
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of(MENU_VIEWPORT), builder, |ui, _class| {
            let vctx = ui.ctx().clone();
            let (esc, focused, close_requested) =
                vctx.input(|i| (i.key_pressed(Key::Escape), i.viewport().focused, i.viewport().close_requested()));
            if !focused_once {
                vctx.send_viewport_cmd(ViewportCommand::Focus);
            }
            match focused {
                Some(true) => focused_once = true,
                // a click elsewhere closes the menu, like a native one
                Some(false) if focused_once => close = true,
                _ => {}
            }
            close |= esc || close_requested;
            self.context_menu(ui);
        });
        match &mut self.menu {
            Some(m) if !close => m.focused_once = focused_once,
            _ => self.menu = None,
        }
    }

    /// The menu: what is under the click and the main switches on top, tabs
    /// on the left, the chosen tab in the middle, Settings and Quit at the
    /// bottom. Panels share the fixed window, which never changes size.
    fn context_menu(&mut self, ui: &mut egui::Ui) {
        let lang = self.lang;
        let ctx = ui.ctx().clone();
        egui::Panel::top("menu-head").show(ui, |ui| self.menu_head(ui));
        egui::Panel::bottom("menu-foot").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button(tr(lang, Msg::MenuSettings)).clicked() {
                    self.open_settings();
                    self.close_menu();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(tr(lang, Msg::MenuQuit)).clicked() {
                        root_cmd(&ctx, ViewportCommand::Close);
                    }
                });
            });
            ui.add_space(2.0);
        });
        egui::Panel::left("menu-tabs").resizable(false).exact_size(MENU_TABS_WIDTH + 12.0).show(ui, |ui| {
            ui.add_space(6.0);
            for (tab, msg) in [
                (MenuTab::Window, Msg::GroupWindow),
                (MenuTab::Style, Msg::GroupStyle),
                (MenuTab::Keys, Msg::GroupOnKeys),
                (MenuTab::Tools, Msg::GroupTools),
                (MenuTab::General, Msg::GroupGeneral),
            ] {
                let r = ui
                    .add_sized([MENU_TABS_WIDTH, 26.0], egui::Button::selectable(self.menu_tab == tab, tr(lang, msg)));
                if r.clicked() {
                    self.menu_tab = tab;
                }
            }
        });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("menu-body").auto_shrink([false, false]).show(ui, |ui| {
                ui.add_space(4.0);
                match self.menu_tab {
                    MenuTab::Window => self.menu_window_tab(ui, &ctx),
                    MenuTab::Style => self.menu_style_tab(ui),
                    MenuTab::Keys => self.menu_keys_tab(ui),
                    MenuTab::Tools => self.menu_tools_tab(ui, &ctx),
                    MenuTab::General => self.menu_general_tab(ui, &ctx),
                }
            });
        });
    }

    fn menu_head(&mut self, ui: &mut egui::Ui) {
        let lang = self.lang;
        let ctx = ui.ctx().clone();
        ui.spacing_mut().item_spacing.y = 5.0;
        // some window systems never give focus to the menu window: always offer a way out
        ui.horizontal(|ui| {
            ui.strong("Keyboard HUD");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").on_hover_text(tr(lang, Msg::MenuClose)).clicked() {
                    self.close_menu();
                }
            });
        });
        ui.separator();

        // the key under the click
        let can_write = self.can_write_keys();
        // always there, so it can be found: without a key under the click it says how
        let key = self.menu_key.filter(|_| self.live.is_some());
        let why_not = if !can_write { tr(lang, Msg::EditKeyUnavailable) } else { tr(lang, Msg::EditKeyRightClick) };
        let r = ui
            .add_enabled(can_write && key.is_some(), egui::Button::new(tr(lang, Msg::MenuEditKey)))
            .on_hover_text(tr(lang, Msg::MenuEditKeyHint))
            .on_disabled_hover_text(why_not);
        if r.clicked() {
            if let Some(key) = key {
                self.open_key_editor(key);
                self.close_menu();
            }
        }
        match (key, &self.live) {
            (Some((layer, row, col)), Some(l)) => {
                let shown =
                    crate::vial::legend::legend(l.keymap.get(layer, row, col), &l.legend_context(&self.layer_names));
                let place = crate::tr_format!(
                    "{} · riga {row}, colonna {col} · {}",
                    "{} · row {row}, column {col} · {}",
                    self.layer_label(layer),
                    shown.tap.text
                );
                ui.label(egui::RichText::new(place).small().weak());
            }
            _ => {
                ui.label(egui::RichText::new(why_not).small().weak());
            }
        }
        if let Some((done, previous)) = self.last_write {
            let what = self.change_text(done.code, previous);
            let r = ui.add_enabled(can_write && self.undoing.is_none(), egui::Button::new(tr(lang, Msg::MenuUndoKey)));
            if r.on_hover_text(format!("{} · {what}", self.layer_label(done.layer))).clicked() {
                self.undo_key();
                self.close_menu();
            }
        }
        ui.separator();

        // the switches used most
        if ui
            .checkbox(&mut self.headsup_on, tr(lang, Msg::MenuHeadsUp))
            .on_hover_text(tr(lang, Msg::HeadsUpHint))
            .changed()
        {
            self.pinned = false;
            self.mark_dirty();
        }
        ui.indent("marker", |ui| {
            let r = ui
                .add_enabled(self.headsup_on, egui::Checkbox::new(&mut self.marker_on, tr(lang, Msg::MenuMarker)))
                .on_hover_text(tr(lang, Msg::MarkerHint));
            if r.changed() {
                self.mark_dirty();
            }
        });
        let available = self.click_through_available();
        let mut ct = self.click_through;
        let hint = tr(lang, if available { Msg::ClickThroughHint } else { Msg::ClickThroughUnavailable });
        let r = ui
            .add_enabled(available, egui::Checkbox::new(&mut ct, tr(lang, Msg::MenuClickThrough)))
            .on_hover_text(hint)
            .on_disabled_hover_text(hint);
        if r.changed() {
            self.set_click_through(&ctx, ct);
            self.close_menu();
        }
        if let Some(w) = self.worker() {
            let mut paused = w.is_paused();
            if ui.checkbox(&mut paused, tr(lang, Msg::MenuPause)).changed() {
                w.set_paused(paused);
            }
        }
        ui.add_space(4.0);
    }

    fn menu_window_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let lang = self.lang;
        egui::Grid::new("menu-window-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label(tr(lang, Msg::MenuOpacity));
            let mut pct = (self.opacity * 100.0).round();
            if ui.add(egui::Slider::new(&mut pct, 20.0..=100.0).suffix("%").fixed_decimals(0)).changed() {
                self.opacity = pct / 100.0;
                self.mark_dirty();
            }
            ui.end_row();
            ui.label(tr(lang, Msg::MenuScale));
            let mut pct = (self.scale * 100.0).round();
            let r = ui.add(egui::Slider::new(&mut pct, 50.0..=200.0).suffix("%").fixed_decimals(0));
            if r.changed() {
                self.scale = pct / 100.0;
            }
            // resizing while dragging would move the slider under the pointer
            if r.drag_stopped() || (r.changed() && !r.dragged()) {
                self.resize(ctx);
            }
            ui.end_row();
            ui.label(tr(lang, Msg::MenuView));
            ui.horizontal(|ui| {
                let before = self.view;
                ui.selectable_value(&mut self.view, ViewMode::Full, tr(lang, Msg::ViewFull));
                ui.selectable_value(&mut self.view, ViewMode::Compact, tr(lang, Msg::ViewCompact));
                if self.view != before {
                    self.resize(ctx);
                }
            });
            ui.end_row();
            ui.label(tr(lang, Msg::MenuPosition));
            let movable = root_outer_rect(ctx).is_some();
            ui.add_enabled_ui(movable, |ui| {
                egui::Grid::new("positions").spacing([2.0, 2.0]).show(ui, |ui| {
                    for (row, arrows) in POSITIONS.iter().enumerate() {
                        for (col, arrow) in arrows.iter().enumerate() {
                            if ui.add_sized([28.0, 24.0], egui::Button::new(*arrow)).clicked() {
                                self.move_to(ctx, screen::Anchor { col: col as u8, row: row as u8 });
                                self.close_menu();
                            }
                        }
                        ui.end_row();
                    }
                });
            })
            .response
            .on_disabled_hover_text(tr(lang, Msg::PositionUnavailable));
            ui.end_row();
        });
    }

    fn menu_style_tab(&mut self, ui: &mut egui::Ui) {
        let lang = self.lang;
        egui::Grid::new("menu-theme-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label(tr(lang, Msg::MenuTheme));
            ui.horizontal(|ui| {
                let before = self.theme;
                ui.selectable_value(&mut self.theme, ThemeChoice::System, tr(lang, Msg::ThemeSystem));
                ui.selectable_value(&mut self.theme, ThemeChoice::Dark, tr(lang, Msg::ThemeDark));
                ui.selectable_value(&mut self.theme, ThemeChoice::Light, tr(lang, Msg::ThemeLight));
                if self.theme != before {
                    self.mark_dirty();
                }
            });
            ui.end_row();
        });
        ui.add_space(2.0);
        ui.label(tr(lang, Msg::MenuStyle));
        // the styles in even columns
        let mut chosen = None;
        egui::Grid::new("menu-styles").num_columns(3).spacing([4.0, 4.0]).min_col_width(84.0).show(ui, |ui| {
            for (i, name) in self.style_names.iter().enumerate() {
                let r = ui.add_sized([84.0, 20.0], egui::Button::selectable(*name == self.style_name, name.as_str()));
                if r.clicked() && *name != self.style_name {
                    chosen = Some(name.clone());
                }
                if i % 3 == 2 {
                    ui.end_row();
                }
            }
        });
        if let Some(name) = chosen {
            self.set_style(&name, true);
        }
        ui.add_space(2.0);
        let look = self.look();
        egui::Grid::new("menu-look-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label(tr(lang, Msg::MenuKeyOpacity));
            let mut pct = (look.key_opacity * 100.0).round();
            if ui.add(egui::Slider::new(&mut pct, 20.0..=100.0).suffix("%").fixed_decimals(0)).changed() {
                self.key_opacity = Some(pct / 100.0);
                self.mark_dirty();
            }
            ui.end_row();
            ui.label(tr(lang, Msg::MenuKeyRadius));
            let mut pct = (look.key_radius * 100.0).round();
            if ui.add(egui::Slider::new(&mut pct, 0.0..=50.0).suffix("%").fixed_decimals(0)).changed() {
                self.key_radius = Some(pct / 100.0);
                self.mark_dirty();
            }
            ui.end_row();
        });
        ui.horizontal(|ui| {
            if ui.button(tr(lang, Msg::MenuSaveStyle)).on_hover_text(tr(lang, Msg::SaveStyleHint)).clicked() {
                self.save_style();
            }
            let dir = style::dir();
            if ui.button(tr(lang, Msg::MenuStylesFolder)).on_hover_text(dir.display().to_string()).clicked() {
                if let Err(e) = backup::open_folder(&dir) {
                    self.notify(format!("{}: {e}", dir.display()));
                }
                self.close_menu();
            }
        });
    }

    fn menu_keys_tab(&mut self, ui: &mut egui::Ui) {
        let lang = self.lang;
        let has_combos = self.live.as_ref().or(self.offline.as_ref()).is_some_and(|l| !l.dynamic.combos.is_empty());
        let hint = tr(lang, if has_combos { Msg::CombosHint } else { Msg::NoCombos });
        let r = ui
            .add_enabled(has_combos, egui::Checkbox::new(&mut self.show_combos, tr(lang, Msg::MenuCombos)))
            .on_hover_text(hint)
            .on_disabled_hover_text(hint);
        if r.changed() {
            self.mark_dirty();
        }
        if ui
            .checkbox(&mut self.keycast_on, tr(lang, Msg::MenuKeycast))
            .on_hover_text(tr(lang, Msg::KeycastHint))
            .changed()
        {
            self.keycast = keycast::Keycast::default();
            self.mark_dirty();
        }
        ui.horizontal(|ui| {
            ui.label(tr(lang, Msg::MenuHeatmap));
            ui.add_enabled_ui(self.stats.is_some(), |ui| {
                ui.selectable_value(&mut self.heat, HeatMode::Off, tr(lang, Msg::HeatOff));
                ui.selectable_value(&mut self.heat, HeatMode::Layer, tr(lang, Msg::HeatLayer));
                ui.selectable_value(&mut self.heat, HeatMode::All, tr(lang, Msg::HeatAll));
            });
        });
    }

    fn menu_tools_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let lang = self.lang;
        let wide = |ui: &mut egui::Ui, text: &str| ui.add(egui::Button::new(text).min_size(Vec2::new(220.0, 0.0)));
        if wide(ui, tr(lang, Msg::MenuSearch)).clicked() {
            self.search.open();
            root_cmd(ctx, ViewportCommand::Focus);
            self.close_menu();
        }
        if wide(ui, tr(lang, Msg::MenuTraining)).clicked() {
            self.training.open = true;
            self.close_menu();
        }
        if wide(ui, tr(lang, Msg::MenuStats)).clicked() {
            self.report.open = true;
            self.close_menu();
        }
        let has_layout = self.live.is_some() || self.offline.is_some();
        let r =
            ui.add_enabled(has_layout, egui::Button::new(tr(lang, Msg::MenuExport)).min_size(Vec2::new(220.0, 0.0)));
        if r.on_hover_text(tr(lang, Msg::ExportHint)).clicked() {
            let path = self.export_path();
            match self.export_keymap(&path) {
                Ok(()) => {
                    if let Err(e) = backup::open_path(&path) {
                        log::warn!("{}: {e}", path.display());
                    }
                    self.notify(format!("{}: {}", tr(lang, Msg::ExportDone), path.display()));
                }
                Err(e) => self.notify(format!("{}: {e}", tr(lang, Msg::ExportFailed))),
            }
            self.close_menu();
        }
        if wide(ui, tr(lang, Msg::MenuHistory)).clicked() {
            self.history.open(&self.backup_dir());
            self.close_menu();
        }
        let dir = self.backup_dir();
        if wide(ui, tr(lang, Msg::MenuBackups)).on_hover_text(dir.display().to_string()).clicked() {
            if let Err(e) = backup::open_folder(&dir) {
                self.notify(format!("{}: {e}", dir.display()));
            }
            self.close_menu();
        }
        if wide(ui, tr(lang, Msg::MenuDiagnostics)).clicked() {
            self.diagnostics.open = true;
            self.close_menu();
        }
    }

    fn menu_general_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let lang = self.lang;
        ui.horizontal(|ui| {
            ui.label(tr(lang, Msg::MenuLanguage));
            let mut choice = self.language;
            ui.selectable_value(&mut choice, LanguageChoice::Auto, tr(lang, Msg::LanguageAuto));
            ui.selectable_value(&mut choice, LanguageChoice::It, "Italiano");
            ui.selectable_value(&mut choice, LanguageChoice::En, "English");
            if choice != self.language {
                self.set_language(ctx, choice);
            }
        });
        let (mut on, hint) = match &self.autostart {
            Ok(autostart::State::On) => (true, tr(lang, Msg::AutostartHint).to_string()),
            Ok(autostart::State::Off) => (false, tr(lang, Msg::AutostartHint).to_string()),
            Ok(autostart::State::Stale) => (false, tr(lang, Msg::AutostartStale).to_string()),
            Err(e) => (false, format!("{}: {e}", tr(lang, Msg::AutostartError))),
        };
        let hint = format!("{hint}\n{}", autostart::location());
        if ui.checkbox(&mut on, tr(lang, Msg::MenuAutostart)).on_hover_text(hint).changed() {
            self.set_autostart(on);
        }

        // updates
        use crate::update::State;
        ui.separator();
        let state = self.updater.state();
        ui.horizontal(|ui| {
            ui.label(format!("Keyboard HUD {}", env!("CARGO_PKG_VERSION")));
            if ui.add_enabled(!self.updater.busy(), egui::Button::new(tr(lang, Msg::MenuCheckUpdates))).clicked() {
                self.updater.check(ctx, true);
            }
        });
        match &state {
            State::Checking | State::Installing(_) | State::Installed => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tr(
                        lang,
                        if state == State::Checking { Msg::UpdateChecking } else { Msg::UpdateInstalling },
                    ));
                });
            }
            State::UpToDate => {
                ui.label(tr(lang, Msg::UpdateUpToDate));
            }
            State::Available(r) => {
                ui.horizontal(|ui| {
                    ui.strong(crate::tr_format!("Versione {} disponibile", "Version {} available", r.version));
                    if ui.link(tr(lang, Msg::UpdateNotes)).clicked() {
                        self.open_url(&r.page);
                    }
                });
                if r.installable() {
                    let b = ui.button(tr(lang, Msg::UpdateInstall)).on_hover_text(tr(lang, Msg::UpdateInstallHint));
                    if b.clicked() {
                        self.updater.install(ctx);
                    }
                } else {
                    ui.label(tr(lang, Msg::UpdateManual));
                }
            }
            State::Failed(e) => {
                ui.colored_label(ui.visuals().warn_fg_color, format!("{}: {e}", tr(lang, Msg::UpdateFailed)));
            }
            State::Idle => {}
        }
        let mut auto = self.config.update.auto_check;
        if ui.checkbox(&mut auto, tr(lang, Msg::UpdateAuto)).on_hover_text(tr(lang, Msg::UpdateAutoHint)).changed() {
            let edit = Edit::Key("update.auto_check", Some(toml::Value::Boolean(auto)));
            self.edit_config(ctx, &[edit]);
        }
        if self.updater.busy() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

impl eframe::App for HudApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        frameless::remove_shadows();
        self.drain_events(ctx);
        let now = Instant::now();
        if let TapAction::Single = self.hud_toggle.poll(now) {
            if self.headsup_on && self.visible {
                // heads-up: a tap pins the HUD on screen, another one gives it back to heads-up
                self.pinned = !self.pinned;
                self.notify(tr(self.lang, if self.pinned { Msg::Pinned } else { Msg::Unpinned }).to_string());
            } else {
                self.set_visible(ctx, !self.visible);
            }
        }
        if let Some(s) = &mut self.stats {
            s.save_if_due();
        }
        let open_search = ctx.input(|i| {
            (i.modifiers.command && i.key_pressed(Key::F)) || (i.key_pressed(Key::Slash) && !i.modifiers.any())
        });
        if open_search && !self.click_through {
            self.search.open();
            root_cmd(ctx, ViewportCommand::Focus);
        }
        // Ctrl+, settings, Ctrl+Shift+, the file itself, as in VS Code
        let (settings, file) = ctx.input(|i| {
            let comma = i.modifiers.command && i.key_pressed(Key::Comma);
            (comma && !i.modifiers.shift, comma && i.modifiers.shift)
        });
        if !self.click_through {
            if settings {
                self.open_settings();
            }
            if file {
                self.open_config_file();
            }
        }
        self.handle_tray(ctx);
        self.handle_update(ctx);
        let fullscreen = self.config.appearance.hide_in_fullscreen && self.foreground.fullscreen();
        if fullscreen != self.fullscreen_hidden {
            self.fullscreen_hidden = fullscreen;
            log::info!("full screen in the foreground: {fullscreen}");
            ctx.request_repaint();
        }
        if self.config.appearance.hide_in_fullscreen {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        // per-application labels
        if self.labels.has_app_labels() {
            let app = self.foreground.poll().map(str::to_owned);
            if self.labels.set_app(app.as_deref()) {
                log::debug!("labels for {:?}", self.labels.active_app());
                self.search_stale = true;
                ctx.request_repaint();
            }
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        self.poll_style();
        match self.watcher.poll() {
            Reload::Unchanged => {}
            Reload::Changed(c) => {
                self.config_error = None;
                self.config_revision += 1;
                let errors = self.apply_config(ctx, *c);
                self.notify(tr(self.lang, Msg::ConfigReloaded).to_string());
                self.report_label_errors(&errors);
            }
            Reload::Error(e) => {
                let path = self.watcher.path().display().to_string();
                self.notify(format!("{} ({path}): {e}", tr(self.lang, Msg::ConfigError)));
                self.config_error = Some(e);
                self.config_revision += 1;
            }
        }
        if !self.checked_on_screen && self.started.elapsed() > Duration::from_millis(500) {
            self.checked_on_screen = true;
            if let Some(r) = root_outer_rect(ctx) {
                self.keep_on_screen(ctx, r);
            }
        }
        self.track_position(ctx);

        if let Some(shot) = &mut self.screenshot {
            if !shot.requested && now >= shot.at {
                shot.requested = true;
                let (path, ctx2) = (shot.path.clone(), ctx.clone());
                ctx.send_viewport_cmd(ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |image| {
                    if let Err(e) = write_ppm(&path, &image) {
                        log::error!("{}: {e}", path.display());
                    }
                    ctx2.send_viewport_cmd(ViewportCommand::Close);
                    ctx2.request_repaint();
                })));
            }
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        // a pending single tap must be confirmed even if no new state arrives
        if self.hud_toggle.pending.is_some() {
            ctx.request_repaint_after(DOUBLE_TAP);
        }
        // keep checking the config file
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_window();
        if let Some(s) = &mut self.stats {
            s.save();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = Instant::now();
        let dark = self.apply_theme(&ctx);
        let palette = Palette::new(dark, self.opacity, self.style.colors(dark), self.look());
        let compact = self.view == ViewMode::Compact;

        let full = ui.max_rect();
        let response = ui.interact(full, ui.scope_id().with("background"), Sense::click_and_drag());
        if !self.click_through {
            self.handle_drag(&ctx, &response);
            self.handle_zoom(&ctx);
        }
        // live data wins; otherwise the files loaded from disk
        let live_state = self.live.as_ref().and(self.state);

        let layers = match (&live_state, self.live.as_ref()) {
            (Some(s), Some(l)) => classify_layers(&l.keymap, s),
            _ => Vec::new(),
        };

        // heads-up: fade in/out; without live state the HUD stays visible
        let hu = &self.config.headsup;
        let level = match (&live_state, self.headsup_on) {
            (Some(s), true) => {
                // locked layers and one-shots may be left to the mini indicator
                let s =
                    if self.config.indicator.enabled { headsup::without_indicator_states(s, &layers, hu) } else { *s };
                let wanted = self.headsup.wants(&s, hu, now);
                let force = self.pinned
                    || self.search.open
                    || self.menu.is_some()
                    || ctx.any_popup_open()
                    || self.drag.is_some()
                    || self.training.active()
                    || (self.headsup.level() > 0.5 && response.hovered());
                self.headsup.update(wanted, force, hu, now)
            }
            // no keyboard: the HUD fades out, unless asked for (tray, menu, search)
            _ if self.keyboard_absent() => {
                let force = self.pinned
                    || self.search.open
                    || self.menu.is_some()
                    || ctx.any_popup_open()
                    || self.drag.is_some();
                self.headsup.update(false, force, hu, now)
            }
            _ => 1.0,
        };
        // a game, a video or a slide show in full screen: out of the way
        let level = if self.fullscreen_hidden { 0.0 } else { level };
        let faded = level <= 0.01;
        let passthrough = self.click_through || faded;
        if passthrough != self.passthrough {
            self.passthrough = passthrough;
            ctx.send_viewport_cmd(ViewportCommand::MousePassthrough(passthrough));
        }
        let hidden_opacity = if self.fullscreen_hidden { 0.0 } else { hu.hidden_opacity };
        ui.multiply_opacity(hidden_opacity + (1.0 - hidden_opacity) * level);

        let painter = ui.painter_at(full);
        painter.rect_filled(full, palette.look.window_radius, palette.background);
        let layout = self.live.as_ref().or(self.offline.as_ref());

        let search_hit = self.search.selected().map(|h| h.layer);
        let (layer, active_mask, browsing) = match (&live_state, layout) {
            // search preview: the layer of the selected result
            _ if search_hit.is_some() => {
                let l = search_hit.unwrap_or(0);
                (l, (1 << l) | 1, false)
            }
            (Some(s), _) => {
                let mask = s.active_mask();
                (highest_layer(mask), mask, false)
            }
            (None, Some(l)) => {
                let layer = self.browse_layer.min(l.keymap.layers().saturating_sub(1));
                // offline, or a firmware without the HUD protocol: no live state to follow
                (layer, (1 << layer) | 1, self.live.is_none() || self.info.is_none())
            }
            (None, None) => (0, 1, false),
        };
        let pad = if compact { 10.0 } else { 16.0 };
        let header_h = if compact { 22.0 } else { 28.0 };
        let footer_h = if compact { 14.0 } else { 18.0 };
        let header_rect =
            Rect::from_min_size(full.min + Vec2::new(pad, 8.0), Vec2::new(full.width() - 2.0 * pad, header_h));
        // closures over single fields, so that other fields stay mutable below
        let (layer_names, layer_colors) = (&self.layer_names, &self.layer_colors);
        let names = |l: u8| layer_names.get(l as usize).cloned().unwrap_or_else(|| format!("L{l}"));
        let colors = |l: u8| layer_colors.get(l as usize).copied().flatten();
        let animating_header = header::draw(
            &painter,
            header_rect,
            &header::HeaderView {
                lang: self.lang,
                layer,
                layers: &layers,
                layer_name: &names,
                layer_color: &colors,
                state: live_state.as_ref(),
                capabilities: self.info.map_or(0, |i| i.capabilities),
                split_lost: self.split_lost(),
                compact,
                time: self.started.elapsed().as_secs_f64(),
            },
            &palette,
        );

        let body = Rect::from_min_max(
            full.min + Vec2::new(pad - 2.0, 8.0 + header_h + 6.0),
            full.max - Vec2::new(pad - 2.0, footer_h + 4.0),
        );
        let mut browse_layers = None;
        let mut key_under_pointer = None;
        if self.heat != HeatMode::Off && self.heat_cache.as_ref().is_none_or(|(t, _)| t.elapsed() >= HEAT_REFRESH) {
            self.heat_cache = self.stats.as_ref().map(|s| {
                let from = crate::clock::today() - self.config.stats.unused_days as i64 + 1;
                (Instant::now(), s.totals(Some(from)))
            });
        }
        match layout {
            Some(l) => {
                let legends = l.legend_context(&self.layer_names);
                let pos = positions(&l.keys);
                self.search.refresh(&l.keymap, &pos, &legends, &self.labels, &l.dynamic, self.search_stale);
                self.search_stale = false;
                let search = &self.search;
                // training: after a while the key to press lights up
                let train_hint = self
                    .training
                    .session
                    .as_ref()
                    .filter(|t| self.training.active() && t.hint_due(now))
                    .and_then(|t| t.current());
                let mark = |r: u8, c: u8| {
                    let hinted = train_hint.is_some_and(|t| (t.layer, t.row, t.col) == (layer, r, c));
                    if hinted {
                        1.0
                    } else {
                        search.mark(layer, r, c)
                    }
                };
                // heatmap: presses on the layer shown, or on every layer
                let heat_counts: Option<HashMap<(u8, u8), u64>> = match (self.heat, &self.heat_cache) {
                    (HeatMode::Off, _) | (_, None) => None,
                    (mode, Some((_, totals))) => {
                        let mut m = HashMap::new();
                        for (&(l2, r, c), &n) in totals {
                            if mode == HeatMode::All || l2 == layer {
                                *m.entry((r, c)).or_insert(0) += n;
                            }
                        }
                        Some(m)
                    }
                };
                let heat_max = heat_counts.as_ref().and_then(|m| m.values().copied().max()).unwrap_or(0).max(1);
                let heat_fn = |r: u8, c: u8| {
                    let n = heat_counts.as_ref().and_then(|m| m.get(&(r, c)).copied()).unwrap_or(0);
                    ((n as f32 / heat_max as f32).sqrt(), n)
                };
                let fade = &self.fade;
                let highlight = |r: u8, c: u8| if live_state.is_some() { fade.level(r, c, now) } else { 0.0 };
                // key overrides firing with the modifiers held now
                let mods = live_state.map_or(0, |s| s.mods | s.oneshot_mods | s.oneshot_locked_mods);
                let key_override = |code: u16| {
                    let Some(o) = l.dynamic.override_for(code, mods, layer) else {
                        // Shift held: the symbol the key types with Shift
                        let shift = mods & 0x22 != 0 && mods & !0x22 == 0;
                        let kc = u8::try_from(code).ok().filter(|_| code < 0x100)?;
                        let ch = crate::vial::legend::shifted(kc).filter(|_| shift)?;
                        return Some((0x0200 | code, format!("Shift: {ch}")));
                    };
                    let trigger = crate::vial::legend::legend(o.trigger, &legends).tap.text;
                    Some((o.replacement, format!("Key override: {}+{trigger}", Mods5(o.mods5()).display())))
                };
                let combos = (self.show_combos && !l.dynamic.combos.is_empty()).then_some(l.dynamic.combos.as_slice());
                let view = keyboard::KeyboardView {
                    keys: &l.keys,
                    keymap: &l.keymap,
                    layer,
                    active_mask,
                    highlight: &highlight,
                    legends: &legends,
                    labels: &self.labels,
                    layer_color: &colors,
                    mark: &mark,
                    heat: heat_counts.is_some().then_some(&heat_fn as &dyn Fn(u8, u8) -> (f32, u64)),
                    key_override: &key_override,
                    combos,
                };
                let hover = if self.click_through { None } else { response.hover_pos() };
                key_under_pointer = hover.and_then(|p| keyboard::key_at(&l.keys, body, p)).map(|(r, c)| (layer, r, c));
                if let Some(tip) = keyboard::draw(&painter, body, &view, &palette, hover) {
                    response.clone().on_hover_text(tip);
                }
                browse_layers = browsing.then(|| l.keymap.layers());
            }
            None => {
                painter.text(
                    body.center(),
                    Align2::CENTER_CENTER,
                    tr(self.lang, Msg::NoLayout),
                    FontId::proportional(15.0),
                    palette.text_dim,
                );
            }
        }

        // footer: notice, otherwise connection status
        let notice = self.notice.as_ref().filter(|(_, t)| t.elapsed() < NOTICE_TIME).map(|(s, _)| s.clone());
        let show_status = !compact || !matches!(self.status, Some(Status::Connected { .. })) || self.is_mock();
        let mut footer = notice.clone().unwrap_or_else(|| if show_status { self.status_line() } else { String::new() });
        if notice.is_none() && browsing && layout.is_some() {
            footer = format!("{footer} · {}", tr(self.lang, Msg::BrowseHint));
        }
        if let (None, Some(app)) = (&notice, self.labels.active_app()) {
            footer = format!("{footer} · {} {app}", tr(self.lang, Msg::LabelsFor));
        }
        let color = if notice.is_some() { palette.accent } else { palette.text_dim };
        painter.text(
            full.left_bottom() + Vec2::new(pad, -(footer_h / 2.0 + 4.0)),
            Align2::LEFT_CENTER,
            footer,
            FontId::proportional(if compact { 10.5 } else { 12.0 }),
            color,
        );

        // heads-up marker: where the HUD shows up, while it is faded out
        if faded && self.headsup_on && self.marker_on && live_state.is_some() && !self.fullscreen_hidden {
            if self.marker_since.is_none() {
                self.marker_since = Some(now);
                self.backdrop.reset();
            }
            let since = self.marker_since.unwrap_or(now);
            // on the side of the HUD closest to the screen edges; on top when the screen is not known
            let anchor = root_outer_rect(&ctx)
                .and_then(|window| work_area(&ctx, window).map(|area| marker::anchor(window, area)))
                .unwrap_or([1, 0]);
            let at = marker::rect(full, anchor);
            let light_background = ctx.input(|i| i.viewport().inner_rect).and_then(|inner| {
                let ppp = ctx.pixels_per_point();
                let on_screen = |r: Rect| {
                    let (min, max) = ((inner.min + r.min.to_vec2()) * ppp, (inner.min + r.max.to_vec2()) * ppp);
                    let (x, y) = (min.x.round() as i32, min.y.round() as i32);
                    ScreenRect { x, y, w: max.x.round() as i32 - x, h: max.y.round() as i32 - y }
                };
                // around the keys, not the keys themselves
                self.backdrop.light(on_screen(at.expand(8.0)), on_screen(at.expand(2.0)), now)
            });
            let appear = (now.duration_since(since).as_secs_f32() / MARKER_FADE.as_secs_f32()).min(1.0);
            let mut p = ui.painter_at(full);
            p.set_opacity(self.config.headsup.marker_opacity * appear);
            let fn_keys = [colors(1).unwrap_or(MARKER_FN[0]), colors(2).unwrap_or(MARKER_FN[1])];
            marker::draw(&p, at, &marker::Colors { light_background, fn_keys });
            if appear < 1.0 {
                ctx.request_repaint();
            }
        } else {
            self.marker_since = None;
        }

        if let Some(n) = browse_layers {
            if !self.search.open {
                self.handle_browse_input(&ctx, n);
            }
        }
        if !passthrough && response.secondary_clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                self.open_menu(&ctx, full, p);
                self.menu_key = key_under_pointer;
            }
        } else if response.clicked() || passthrough {
            self.close_menu();
        }
        // not in the very first frames: the window system is not ready for a second window yet
        if self.dev_menu && self.started.elapsed() > Duration::from_secs(1) {
            self.dev_menu = false;
            self.open_menu(&ctx, full, full.center() + Vec2::new(-10.0, -10.0));
        }
        self.show_menu(&ctx);
        // the indicator window lives as long as the app looks for a keyboard, empty
        // when not needed: created at the right moment it could take the focus while
        // typing (and in the very first frames it would not be created at all)
        if matches!(self.mode, Mode::Live { .. })
            && self.config.indicator.enabled
            && self.started.elapsed() > Duration::from_secs(1)
        {
            let flash = self.flash.as_ref().filter(|(_, t)| t.elapsed() < FLASH_TIME);
            if let Some((_, t)) = flash {
                ctx.request_repaint_after(FLASH_TIME.saturating_sub(t.elapsed()));
            }
            let (layer_names, layer_colors) = (self.layer_names.clone(), self.layer_colors.clone());
            let names = |l: u8| layer_names.get(l as usize).cloned().unwrap_or_else(|| format!("L{l}"));
            let colors = |l: u8| layer_colors.get(l as usize).copied().flatten();
            let view = header::IndicatorView {
                lang: self.lang,
                layers: &layers,
                layer_name: &names,
                layer_color: &colors,
                state: live_state.as_ref(),
                connection: self.connection_problem(),
                flash: flash.map(|(s, _)| s.as_str()),
                held_mods: self.headsup_on
                    && self.config.headsup.modifiers == ShownIn::Indicator
                    && self.headsup.mods_held_long(),
                split_lost: self.split_lost(),
                time: self.started.elapsed().as_secs_f64(),
            };
            let shown = (!self.visible || faded) && !self.fullscreen_hidden && header::indicator_needed(&view);
            self.show_indicator(&ctx, shown.then_some(&view), &palette);
        }
        if self.keycast_on && self.live.is_some() && self.started.elapsed() > Duration::from_secs(1) {
            self.show_keycast(&ctx, &palette);
        }
        let names = self.layer_names.clone();
        let name_of = |l: u8| names.get(l as usize).cloned().unwrap_or_else(|| format!("L{l}"));
        self.search.show(&ctx, self.lang, &palette, &name_of);

        // statistics window
        let layout = self.live.as_ref().or(self.offline.as_ref());
        let pos = layout.map(|l| positions(&l.keys)).unwrap_or_default();
        let input = ReportInput {
            lang: self.lang,
            layout,
            positions: &pos,
            layer_names: &self.layer_names,
            labels: &self.labels,
            days: self.config.stats.unused_days,
            csv_path: paths::base_dir().join("keyboard-hud-stats.csv"),
        };
        self.report.show(&ctx, self.stats.as_mut(), &input);
        self.show_history(&ctx, &palette);
        self.show_training(&ctx);
        self.show_key_editor(&ctx);
        if self.diagnostics.open {
            let sections = self.diagnostics_sections();
            match self.diagnostics.show(&ctx, self.lang, &sections) {
                diagnostics::Action::Copied => self.notify(tr(self.lang, Msg::DiagCopied).to_string()),
                diagnostics::Action::OpenFolder => {
                    if let Err(e) = backup::open_folder(paths::base_dir()) {
                        self.notify(format!("{}: {e}", paths::base_dir().display()));
                    }
                }
                diagnostics::Action::None => {}
            }
        }
        if self.settings.open {
            let input = SettingsInput {
                lang: self.lang,
                config: &self.config,
                path: self.watcher.path(),
                error: self.config_error.as_deref(),
                styles: &self.style_names,
                style_look: self.style.look(),
                layer_names: &self.layer_names,
                revision: self.config_revision,
            };
            match self.settings.show(&ctx, &input) {
                settings::Action::Edits(edits) => self.edit_config(&ctx, &edits),
                settings::Action::SaveText(text) => self.save_config_text(&ctx, &text),
                settings::Action::OpenFile => self.open_config_file(),
                settings::Action::None => {}
            }
        }

        if self.headsup.animating() {
            ctx.request_repaint();
        } else if self.headsup.waiting() {
            // a delay may run out without a new state from the keyboard
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        if self.fade.animating(now) || animating_header {
            ctx.request_repaint();
        } else if notice.is_some() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Menu choices written into the config file read back the same.
    #[test]
    fn prefs_round_trip_through_the_config() {
        let old = Prefs::from_config(&Config::default());
        assert_eq!(old.style, style::DEFAULT_STYLE);
        assert!(old.edits(&old).is_empty());
        let new = Prefs {
            opacity: 0.6,
            scale: 1.3,
            mode: ViewMode::Compact,
            theme: ThemeChoice::Dark,
            style: "glass".into(),
            key_opacity: Some(0.4),
            combos: true,
            keycast: true,
            ..old.clone()
        };
        let text = config_edit::apply("# mio\n", &new.edits(&old)).unwrap();
        assert!(text.starts_with("# mio\n"), "{text}");
        assert_eq!(Prefs::from_config(&Config::parse(&text).unwrap()), new);
        // back to the style's value: the key goes away
        let unset = Prefs { key_opacity: None, ..new.clone() };
        let text = config_edit::apply(&text, &unset.edits(&new)).unwrap();
        assert!(!text.contains("key_opacity"));
    }

    #[test]
    fn press_fade() {
        let t0 = Instant::now();
        let mut f = PressFade::default();
        let mut m = [0u8; 8];
        m[1] = 0b100;
        f.update(&m, t0);
        assert_eq!(f.level(1, 2, t0), 1.0);
        f.update(&[0; 8], t0);
        assert!((f.level(1, 2, t0 + Duration::from_millis(100)) - 0.5).abs() < 0.01);
        assert_eq!(f.level(1, 2, t0 + Duration::from_millis(250)), 0.0);
        assert!(f.animating(t0 + Duration::from_millis(50)));
        assert!(!f.animating(t0 + Duration::from_millis(300)));
        assert_eq!(f.level(9, 9, t0), 0.0);
    }

    #[test]
    fn hud_toggle_taps() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut h = HudToggle::default();
        assert!(matches!(h.on_count(7, t0), TapAction::None)); // baseline
                                                               // single tap, confirmed after the double-tap window
        assert!(matches!(h.on_count(8, ms(0)), TapAction::None));
        assert!(matches!(h.poll(ms(100)), TapAction::None));
        assert!(matches!(h.poll(ms(400)), TapAction::Single));
        // double tap
        assert!(matches!(h.on_count(9, ms(1000)), TapAction::None));
        assert!(matches!(h.on_count(10, ms(1200)), TapAction::Double));
        assert!(matches!(h.poll(ms(1600)), TapAction::None));
        // two taps between polls, across the u8 wrap
        h.reset();
        h.on_count(254, ms(2000));
        assert!(matches!(h.on_count(0, ms(2016)), TapAction::Double));
    }
}
