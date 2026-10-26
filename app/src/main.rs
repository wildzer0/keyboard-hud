//! Keyboard HUD: portable on-screen display of a Vial/QMK keyboard state.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod autostart;
mod backdrop;
mod backup;
mod changes;
mod cli;
mod clock;
mod config;
mod config_edit;
mod foreground;
mod headsup;
mod hid;
mod i18n;
mod keymap_guard;
mod kle;
mod labels;
mod logging;
mod model;
mod os_layout;
mod paths;
mod screen;
mod search;
mod stats;
mod style;
mod training;
mod tray;
mod ui;
mod update;
mod vial;

use cli::Options;
use config::{ConfigWatcher, WindowState};
use hid::mock::{MOCK_VIAL_JSON, MOCK_VIL};
use hid::worker::{Source, Worker};
use std::path::Path;
use std::process::ExitCode;
use ui::{HudApp, Layout, Mode, Screenshot, Startup};
use vial::definition::Definition;
use vial::vil::Vil;

fn load_offline(vial_json: &Path, vil: &Path) -> Result<Layout, String> {
    let def_text = std::fs::read_to_string(vial_json).map_err(|e| format!("{}: {e}", vial_json.display()))?;
    let vil_text = std::fs::read_to_string(vil).map_err(|e| format!("{}: {e}", vil.display()))?;
    layout_from(&def_text, &vil_text)
}

fn layout_from(def_text: &str, vil_text: &str) -> Result<Layout, String> {
    let def = Definition::from_json_str(def_text).map_err(|e| format!("vial.json: {e}"))?;
    let vil = Vil::parse(vil_text, def.rows, def.cols).map_err(|e| e.to_string())?;
    if let Some(uid) = vil.uid {
        log::info!("offline: {} (uid {uid:016x})", def.name);
    }
    if !vil.unknown.is_empty() {
        log::warn!("unknown keycodes in the .vil: {:?}", vil.unknown);
    }
    Ok(Layout::new(def, vil.layout_options, vil.keymap, vil.dynamic))
}

/// `vial.json` and the first `*.vil` in the app folder, if present.
fn find_offline_files() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let dir = paths::base_dir();
    let json = dir.join("vial.json");
    let vil = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("vil")))
        .min()?;
    json.exists().then_some((json, vil))
}

fn main() -> ExitCode {
    let opts = match cli::parse(std::env::args().skip(1)) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}\n\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    if opts.help {
        println!("{}", cli::USAGE);
        return ExitCode::SUCCESS;
    }
    if opts.version {
        println!("keyboard-hud {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if opts.print_udev_rule {
        print!("{}", hid::transport::udev_instructions());
        return ExitCode::SUCCESS;
    }
    run(opts)
}

fn run(opts: Options) -> ExitCode {
    if opts.x11 {
        // Force XWayland: winit picks Wayland only when this variable is set.
        // Safe here: no other thread exists yet.
        std::env::remove_var("WAYLAND_DISPLAY");
    }

    let base = paths::base_dir();
    let (watcher, config, config_error) = ConfigWatcher::new(base.join(paths::CONFIG_FILE));
    let level = if opts.verbose { log::LevelFilter::Debug } else { logging::parse_level(&config.log.level) };
    logging::init(&base.join("keyboard-hud.log"), level);
    log::info!("keyboard-hud {} · folder {}", env!("CARGO_PKG_VERSION"), base.display());
    if let Some(e) = &config_error {
        log::error!("{}: {e}", watcher.path().display());
    }
    let window_path = base.join("keyboard-hud-state.toml");
    let window = WindowState::load(&window_path);

    // Offline layout: explicit paths, files next to the app, or (with
    // --offline / --mock) the bundled sample.
    let offline = match (&opts.vial_json, &opts.vil) {
        (Some(j), Some(v)) => match load_offline(j, v) {
            Ok(l) => Some(l),
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
        },
        (None, None) => find_offline_files()
            .and_then(|(j, v)| load_offline(&j, &v).map_err(|e| log::warn!("{e}")).ok())
            .or_else(|| (opts.offline || opts.mock).then(|| layout_from(MOCK_VIAL_JSON, MOCK_VIL).ok()).flatten()),
        _ => {
            eprintln!("--vial-json e --vil vanno indicati insieme");
            return ExitCode::from(2);
        }
    };

    let a = &config.appearance;
    let scale = window.scale.unwrap_or(a.scale);
    let size = ui::base_size(window.mode.unwrap_or(a.mode)) * scale;
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(ui::frameless::HUD_TITLE)
        .with_app_id("keyboard-hud")
        .with_inner_size(size)
        .with_min_inner_size([200.0, 100.0])
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_taskbar(false)
        .with_icon(egui::IconData {
            rgba: include_bytes!("../packaging/icon/icon-64.rgba").to_vec(),
            width: 64,
            height: 64,
        });
    if let (Some(x), Some(y)) = (window.x.or(a.x), window.y.or(a.y)) {
        viewport = viewport.with_position([x, y]);
    }
    let native = eframe::NativeOptions { viewport, ..Default::default() };
    let (vid, pid) = (opts.vid.or(config.vid()), opts.pid.or(config.pid()));
    let polling = config.polling.clone();

    let result = eframe::run_native(
        "Keyboard HUD",
        native,
        Box::new(move |cc| {
            let autostart_args = opts.autostart_args();
            ui::install_fonts(&cc.egui_ctx);
            let mode = if opts.offline {
                Mode::Offline
            } else {
                let ctx = cc.egui_ctx.clone();
                let source = if opts.mock { Source::Mock(opts.mock_at) } else { Source::Live { vid, pid } };
                let worker =
                    Worker::start(source, polling.visible_hz, polling.timeout_ms, move || ctx.request_repaint());
                Mode::Live { worker, mock: opts.mock }
            };
            let startup = Startup {
                mode,
                offline,
                start_layer: opts.layer,
                screenshot: opts.screenshot.map(|p| {
                    Screenshot::new(p, std::time::Duration::from_millis(opts.screenshot_after.unwrap_or(2000)))
                }),
                dev: ui::DevOptions {
                    search: opts.search,
                    heatmap: opts.heatmap,
                    stats_window: opts.stats_window,
                    diagnostics: opts.diagnostics,
                    settings: opts.settings,
                    settings_page: opts.settings_page,
                    menu: opts.menu,
                    edit_key: opts.edit_key,
                    history: opts.history,
                    training: opts.training,
                    export: opts.export,
                },
                config,
                watcher,
                config_error,
                window,
                window_path,
                verbose: opts.verbose,
                autostart_args,
            };
            Ok(Box::new(HudApp::new(&cc.egui_ctx, startup)))
        }),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log::error!("{e}");
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
